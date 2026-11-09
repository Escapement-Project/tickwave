//! The benchmark's native side: the baseline hashes every browser decoder is checked against, the
//! corrupted inputs every decoder must refuse, and native decode timings for reference.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tickwave_format::decode::{Block, BlockChannel, Head, OverviewBlock};
use tickwave_format::header::{PREAMBLE_LEN, Preamble};
use tickwave_format::overview::ChannelOverview;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Hash every block's decoded output, as `src/canonical.ts` does in the browser.
    Hashes {
        #[arg(long)]
        out: PathBuf,
        files: Vec<PathBuf>,
    },
    /// Write corrupted copies of one block (default: the middle one) that every decoder must refuse.
    Corrupt {
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        block: Option<usize>,
        file: PathBuf,
    },
    /// Time `Head::decode_block` over every block, `runs` times.
    Time {
        #[arg(long, default_value_t = 20)]
        runs: usize,
        #[arg(long)]
        out: PathBuf,
        files: Vec<PathBuf>,
    },
}

/// The bytes `src/canonical.ts` hashes; keep the two in step.
fn canonical(b: &Block) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&b.start.to_le_bytes());
    for c in &b.channels {
        match c {
            BlockChannel::Digital { initial, edges } => {
                out.push(0);
                out.push(u8::from(*initial));
                out.extend_from_slice(&(edges.len() as u64).to_le_bytes());
                for e in edges {
                    out.extend_from_slice(&e.to_le_bytes());
                }
            }
            BlockChannel::Analog {
                first_index,
                values,
            } => {
                out.push(1);
                out.extend_from_slice(&first_index.to_le_bytes());
                out.extend_from_slice(&(values.len() as u64).to_le_bytes());
                for v in values {
                    out.extend_from_slice(&v.to_bits().to_le_bytes());
                }
            }
        }
    }
    out
}

/// The bytes `canonicalOverview` in `src/canonical.ts` hashes; keep the two in step.
fn canonical_overview(o: &OverviewBlock) -> Vec<u8> {
    let mut out = vec![o.level.shift];
    out.extend_from_slice(&o.first.to_le_bytes());
    out.extend_from_slice(&o.level.buckets.to_le_bytes());
    for c in &o.level.channels {
        match c {
            ChannelOverview::Digital { level, change } => {
                out.push(0);
                out.extend_from_slice(level);
                out.extend_from_slice(change);
            }
            ChannelOverview::Analog { min, max } => {
                out.push(1);
                for v in min.iter().chain(max) {
                    out.extend_from_slice(&v.to_bits().to_le_bytes());
                }
            }
        }
    }
    out
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn items(b: &Block) -> (u64, u64) {
    b.channels.iter().fold((0, 0), |(e, s), c| match c {
        BlockChannel::Digital { edges, .. } => (e + edges.len() as u64, s),
        BlockChannel::Analog { values, .. } => (e, s + values.len() as u64),
    })
}

fn open(path: &Path) -> Result<(Vec<u8>, Head)> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let head = Head::parse(&bytes).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    Ok((bytes, head))
}

fn frame<'a>(bytes: &'a [u8], head: &Head, i: usize) -> &'a [u8] {
    let (offset, len) = head.block_location(i);
    &bytes[offset as usize..offset as usize + len as usize]
}

fn stem(path: &Path) -> String {
    let name = path.file_name().unwrap().to_string_lossy();
    name.strip_suffix(".tickwave").unwrap_or(&name).to_owned()
}

fn bar(m: &MultiProgress, len: u64, what: String) -> ProgressBar {
    let b = m.add(ProgressBar::new(len));
    b.set_style(
        ProgressStyle::with_template("{msg:32} {bar:40} {pos}/{len} {per_sec} eta {eta}").unwrap(),
    );
    b.set_message(what);
    b
}

#[derive(Serialize)]
struct BlockHash {
    hash: String,
    edges: u64,
    samples: u64,
}

#[derive(Serialize)]
struct Hashes {
    file: String,
    file_sha256: String,
    blocks: Vec<BlockHash>,
    /// Every overview block's hash, by level in the header's order, then in time order.
    overview: Vec<String>,
}

fn hashes(out: &Path, files: &[PathBuf]) -> Result<()> {
    fs::create_dir_all(out)?;
    let m = MultiProgress::new();
    let all = bar(&m, files.len() as u64, "recordings".into());
    for path in files {
        let (bytes, head) = open(path)?;
        let b = bar(&m, head.rows.len() as u64, stem(path));
        let mut blocks = Vec::with_capacity(head.rows.len());
        for i in 0..head.rows.len() {
            let block = head
                .decode_block(i, frame(&bytes, &head, i))
                .map_err(|e| anyhow::anyhow!("{} block {i}: {e}", path.display()))?;
            let (edges, samples) = items(&block);
            blocks.push(BlockHash {
                hash: hex(&Sha256::digest(canonical(&block))),
                edges,
                samples,
            });
            b.inc(1);
        }
        b.finish_and_clear();
        let mut overview = Vec::new();
        for (level, l) in head.header.overview_blocks.iter().enumerate() {
            for k in 0..l.blocks as usize {
                let (offset, len) = head.overview_block_location(level, k);
                let frame = &bytes[offset as usize..offset as usize + len as usize];
                let o = head.decode_overview_block(level, k, frame).map_err(|e| {
                    anyhow::anyhow!(
                        "{} overview block {k} of level {level}: {e}",
                        path.display()
                    )
                })?;
                overview.push(hex(&Sha256::digest(canonical_overview(&o))));
            }
        }
        let h = Hashes {
            file: stem(path),
            file_sha256: hex(&Sha256::digest(&bytes)),
            blocks,
            overview,
        };
        fs::write(
            out.join(format!("{}.json", stem(path))),
            serde_json::to_string(&h)?,
        )?;
        m.suspend(|| {
            eprintln!(
                "{}: {} blocks and {} overview blocks hashed",
                stem(path),
                h.blocks.len(),
                h.overview.len()
            )
        });
        all.inc(1);
    }
    all.finish();
    Ok(())
}

#[derive(Serialize)]
struct Corruption {
    kind: &'static str,
    /// What was done to the bytes.
    what: String,
    /// The bytes to hand a decoder: a frame for block `block` (the file's own head applies), or a
    /// whole recording.
    path: String,
    whole_file: bool,
    /// The block a decoder is asked to decode.
    block: usize,
    /// Where a decoder must refuse: opening the `head`, or decoding the `block`.
    stage: &'static str,
    /// The native decoder's refusal, or "accepted".
    native: String,
}

#[derive(Serialize)]
struct CorruptManifest {
    file: String,
    block: usize,
    corruptions: Vec<Corruption>,
}

/// A frame of the same length with no content checksum: the 4-byte frame content size field
/// widened to 8 bytes takes the place of the dropped checksum.
fn without_checksum(f: &[u8]) -> Result<Vec<u8>> {
    let fhd = f[4];
    ensure!(fhd & 0x04 != 0, "the frame has no checksum to drop");
    ensure!(
        fhd >> 6 == 2,
        "needs a 4-byte frame content size field, has code {}",
        fhd >> 6
    );
    ensure!(fhd & 0x03 == 0, "a dictionary id is not handled");
    let single_segment = fhd & 0x20 != 0;
    let fcs_at = if single_segment { 5 } else { 6 };
    let mut out = Vec::with_capacity(f.len());
    out.extend_from_slice(&f[..4]);
    out.push((fhd & !0x04) | 0xc0);
    out.extend_from_slice(&f[5..fcs_at]);
    let fcs = u32::from_le_bytes(f[fcs_at..fcs_at + 4].try_into().unwrap());
    out.extend_from_slice(&u64::from(fcs).to_le_bytes());
    out.extend_from_slice(&f[fcs_at + 4..f.len() - 4]);
    ensure!(out.len() == f.len());
    Ok(out)
}

/// The block's payload with one single-byte interval after a channel's first set to zero, so two
/// edges share a tick; the section lengths and edge counts are unchanged.
fn zero_interval(head: &Head, payload: &mut [u8]) -> Result<String> {
    let n = head.header.channels.len();
    let lengths: Vec<usize> = (0..n)
        .map(|c| u32::from_le_bytes(payload[4 * c..4 * c + 4].try_into().unwrap()) as usize)
        .collect();
    let mut at = 4 * n;
    for (c, len) in lengths.iter().enumerate() {
        let section = at..at + len;
        if matches!(
            head.header.channels[c].data,
            tickwave_format::header::ChannelKind::Digital { .. }
        ) && *len > 2
        {
            // Walk to the second interval; replace the first single-byte one after it.
            let mut p = section.start;
            let mut k = 0;
            while p < section.end {
                let one_byte = payload[p] < 0x80;
                if k >= 1 && one_byte && payload[p] != 0 {
                    let was = payload[p];
                    payload[p] = 0;
                    return Ok(format!(
                        "channel {c} interval {k} at payload byte {p}: {was} set to 0"
                    ));
                }
                while payload[p] >= 0x80 {
                    p += 1;
                }
                p += 1;
                k += 1;
            }
        }
        at += len;
    }
    bail!("no digital channel has a one-byte interval to zero")
}

/// For a recording without edges: 4 bytes moved from one section's length to the next, so the
/// sections still fill the payload but neither holds its count.
fn section_shift(head: &Head, payload: &mut [u8]) -> Result<String> {
    let n = head.header.channels.len();
    ensure!(n >= 2, "needs two channels");
    let len = |p: &[u8], c: usize| u32::from_le_bytes(p[4 * c..4 * c + 4].try_into().unwrap());
    let (a, b) = (len(payload, 0), len(payload, 1));
    ensure!(a >= 4, "channel 0's section is too short to shift");
    payload[0..4].copy_from_slice(&(a - 4).to_le_bytes());
    payload[4..8].copy_from_slice(&(b + 4).to_le_bytes());
    Ok(format!(
        "section lengths {a}, {b} changed to {}, {}",
        a - 4,
        b + 4
    ))
}

fn corrupt(out: &Path, block: Option<usize>, path: &Path) -> Result<()> {
    fs::create_dir_all(out)?;
    let (bytes, head) = open(path)?;
    let i = block.unwrap_or(head.rows.len() / 2);
    let f = frame(&bytes, &head, i);
    let len = head.rows[i].decoded_length as usize;
    head.decode_block(i, f)
        .map_err(|e| anyhow::anyhow!("block {i} does not decode as it is: {e}"))?;
    let payload = zstd::bulk::decompress(f, len)?;
    let name = stem(path);
    let mut corruptions = Vec::new();

    let mut frames: Vec<(&'static str, String, Vec<u8>)> = Vec::new();
    let mut g = f.to_vec();
    let mid = g.len() / 2;
    g[mid] ^= 0x01;
    frames.push((
        "flip-data",
        format!("bit 0 of frame byte {mid} of {} flipped", g.len()),
        g,
    ));
    let mut g = f.to_vec();
    let last = g.len() - 1;
    g[last] ^= 0x01;
    frames.push((
        "flip-checksum",
        "bit 0 of the stored checksum's last byte flipped".into(),
        g,
    ));
    let g = without_checksum(f)?;
    // Still a well-formed frame: libzstd decodes it to the same payload.
    ensure!(
        zstd::bulk::decompress(&g, len)? == payload,
        "the frame without its checksum does not decode to the same payload"
    );
    frames.push((
        "no-checksum",
        "checksum flag cleared and the checksum dropped; frame content size widened to 8 bytes to keep the length".into(),
        g,
    ));
    for (kind, what, g) in frames {
        let native = match head.decode_block(i, &g) {
            Ok(_) => bail!("{kind}: the native decoder accepted it"),
            Err(e) => e.to_string(),
        };
        let p = out.join(format!("{name}.b{i}.{kind}.bin"));
        fs::write(&p, &g)?;
        corruptions.push(Corruption {
            kind,
            what,
            path: p.file_name().unwrap().to_string_lossy().into_owned(),
            whole_file: false,
            block: i,
            stage: "block",
            native,
        });
    }

    // Whole-file corruptions: block i's frame or index row changed in a copy of the file, with the
    // index and preamble moved to fit, so only the one block is wrong.
    let mut whole: Vec<(&'static str, String, Vec<u8>, u32)> = Vec::new();
    let mut bad = payload.clone();
    let (kind, what) = match zero_interval(&head, &mut bad) {
        Ok(what) => ("zero-interval", what),
        Err(_) => {
            bad.copy_from_slice(&payload);
            ("section-shift", section_shift(&head, &mut bad)?)
        }
    };
    let mut c = zstd::bulk::Compressor::new(19)?;
    c.include_checksum(true)?;
    c.include_contentsize(true)?;
    whole.push((
        kind,
        format!("{what}; recompressed with a checksum, so only the block parser can tell"),
        c.compress(&bad)?,
        len as u32,
    ));
    let mut g = f.to_vec();
    // An empty skippable frame: magic 0x184D2A50, then a zero u32 length.
    g.extend_from_slice(&[0x50, 0x2a, 0x4d, 0x18, 0, 0, 0, 0]);
    whole.push((
        "trailing-frame",
        "an empty skippable frame after the block's frame; the index length covers both".into(),
        g,
        len as u32,
    ));
    let mut g = f.to_vec();
    g.extend_from_slice(&c.compress(&[])?);
    whole.push((
        "trailing-empty-frame",
        "an empty zstd frame, with its checksum, after the block's frame; the index length covers both".into(),
        g,
        len as u32,
    ));
    let mut g = f.to_vec();
    g[f.len() / 2] ^= 0x01;
    g.extend_from_slice(&c.compress(&[])?);
    whole.push((
        "flip-data-then-empty-frame",
        "flip-data's frame, then an empty zstd frame with its checksum; the index length covers both".into(),
        g,
        len as u32,
    ));
    let mut g = f.to_vec();
    // A skippable frame holding the block's own checksum, so the file still ends in it.
    g.extend_from_slice(&[0x50, 0x2a, 0x4d, 0x18, 4, 0, 0, 0]);
    g.extend_from_slice(&f[f.len() - 4..]);
    whole.push((
        "trailing-frame-checksum",
        "a skippable frame holding the block's checksum after the block's frame, so the last 4 bytes still match".into(),
        g,
        len as u32,
    ));
    whole.push((
        "decoded-longer",
        format!("the index's decoded length {len} raised by 1"),
        f.to_vec(),
        len as u32 + 1,
    ));
    whole.push((
        "decoded-shorter",
        format!("the index's decoded length {len} lowered by 1"),
        f.to_vec(),
        len as u32 - 1,
    ));
    for (kind, what, g, decoded) in whole {
        let file = splice(&bytes, &head, i, &g, decoded)?;
        let spliced =
            Head::parse(&file).map_err(|e| anyhow::anyhow!("{kind}: spliced head: {e}"))?;
        for j in [0, spliced.rows.len() - 1] {
            if j != i {
                spliced
                    .decode_block(j, frame(&file, &spliced, j))
                    .map_err(|e| anyhow::anyhow!("{kind}: spliced file, block {j}: {e}"))?;
            }
        }
        for (level, rows) in spliced.overview_rows.iter().enumerate() {
            for k in [0, rows.len() - 1] {
                let (offset, len) = spliced.overview_block_location(level, k);
                let g = &file[offset as usize..offset as usize + len as usize];
                spliced.decode_overview_block(level, k, g).map_err(|e| {
                    anyhow::anyhow!(
                        "{kind}: spliced file, overview block {k} of level {level}: {e}"
                    )
                })?;
            }
        }
        let native = match spliced.decode_block(i, frame(&file, &spliced, i)) {
            Ok(_) => "accepted".to_owned(),
            Err(e) => e.to_string(),
        };
        let fp = out.join(format!("{name}.b{i}.{kind}.tickwave"));
        fs::write(&fp, &file)?;
        corruptions.push(Corruption {
            kind,
            what,
            path: fp.file_name().unwrap().to_string_lossy().into_owned(),
            whole_file: true,
            block: i,
            stage: "block",
            native,
        });
    }

    corruptions.extend(settled_rules(out, &name, &bytes, &head, i)?);
    for c in &corruptions {
        eprintln!("{:16} native: {}", c.kind, c.native);
    }
    let manifest = CorruptManifest {
        file: name.clone(),
        block: i,
        corruptions,
    };
    fs::write(
        out.join(format!("{name}.corrupt.json")),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    Ok(())
}

/// `bytes` with the first `find` after `after` in the header replaced by `replace`, of the same
/// length.
fn header_edit(
    bytes: &[u8],
    head: &Head,
    after: &str,
    find: &str,
    replace: &str,
) -> Result<Vec<u8>> {
    ensure!(
        find.len() == replace.len(),
        "a header edit must keep the length"
    );
    let end = PREAMBLE_LEN + head.preamble.header_len as usize;
    let text = std::str::from_utf8(&bytes[PREAMBLE_LEN..end])?;
    let from = text
        .find(after)
        .with_context(|| format!("no {after} in the header"))?;
    let at = from
        + text[from..]
            .find(find)
            .with_context(|| format!("no {find} after {after}"))?;
    let mut file = bytes.to_vec();
    file[PREAMBLE_LEN + at..PREAMBLE_LEN + at + find.len()].copy_from_slice(replace.as_bytes());
    Ok(file)
}

/// The JSON number after `key` in the header, as text, for an edit that keeps its length.
fn header_number(bytes: &[u8], head: &Head, after: &str, key: &str) -> Result<String> {
    let end = PREAMBLE_LEN + head.preamble.header_len as usize;
    let text = std::str::from_utf8(&bytes[PREAMBLE_LEN..end])?;
    let from = text
        .find(after)
        .with_context(|| format!("no {after} in the header"))?;
    let at = from
        + text[from..]
            .find(key)
            .with_context(|| format!("no {key}"))?
        + key.len();
    Ok(text[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect())
}

/// A corruption before it is checked and written: kind, what was done, the bytes (a frame for
/// `block`, or a whole file), the block a decoder is asked for, where it must refuse, whole file.
type Pending = (&'static str, String, Vec<u8>, usize, &'static str, bool);

/// One corruption for each rule FORMAT.md settled on 2026-11-09, each breaking that rule and
/// nothing else, with every length kept. The native decoder's verdict is recorded, not required.
fn settled_rules(
    out: &Path,
    name: &str,
    bytes: &[u8],
    head: &Head,
    i: usize,
) -> Result<Vec<Corruption>> {
    use tickwave_format::header::{BlockRow, ChannelKind};
    let mut list: Vec<Pending> = Vec::new();
    let nch = head.header.channels.len();
    let index_at = PREAMBLE_LEN + head.preamble.header_len as usize;
    let mut c19 = zstd::bulk::Compressor::new(19)?;
    c19.include_checksum(true)?;
    c19.include_contentsize(true)?;
    let section = |payload: &[u8], c: usize| {
        let len =
            |k: usize| u32::from_le_bytes(payload[4 * k..4 * k + 4].try_into().unwrap()) as usize;
        let at = 4 * nch + (0..c).map(len).sum::<usize>();
        (at, len(c))
    };

    // A well-formed frame naming dictionary 42: the flag for a 1-byte id, and the id where the
    // header holds it, after the window descriptor.
    let f = frame(bytes, head, i);
    ensure!(
        f[4] & 0x03 == 0,
        "block {i}'s frame already names a dictionary"
    );
    let at = if f[4] & 0x20 != 0 { 5 } else { 6 };
    let mut g = Vec::with_capacity(f.len() + 1);
    g.extend_from_slice(&f[..at]);
    g.push(42);
    g.extend_from_slice(&f[at..]);
    g[4] |= 0x01;
    let file = splice(bytes, head, i, &g, head.rows[i].decoded_length)?;
    list.push((
        "dictionary",
        "the frame made to name dictionary 42 (flag and id byte), otherwise unchanged".into(),
        file,
        i,
        "block",
        true,
    ));

    let open = "\"overview\":[";
    let end = PREAMBLE_LEN + head.preamble.header_len as usize;
    let text = std::str::from_utf8(&bytes[PREAMBLE_LEN..end])?;
    let at = text.find(open).context("no overview in the header")? + open.len();
    let close = at + text[at..].find(']').context("overview is not closed")?;
    let mut file = bytes.to_vec();
    file[PREAMBLE_LEN + at..PREAMBLE_LEN + close].fill(b' ');
    list.push((
        "no-overview",
        format!(
            "the header's overview list {} emptied with spaces",
            &text[at - 1..=close]
        ),
        file,
        i,
        "head",
        true,
    ));

    let digital = head
        .header
        .channels
        .iter()
        .position(|c| matches!(c.data, ChannelKind::Digital { edges, .. } if edges > 0));
    if let Some(c) = digital {
        let mut file = bytes.to_vec();
        let at = index_at + i * BlockRow::size(nch) + 24 + 4 * c + 3;
        file[at] ^= 0x80;
        list.push((
            "level-bit",
            format!("block {i}'s level bit for channel {c} flipped in the index"),
            file,
            i,
            "head",
            true,
        ));

        let length = header_number(bytes, head, "", "\"length\":")?;
        ensure!(
            length.as_bytes()[0] > b'1',
            "length {length} does not start above 1"
        );
        let shorter = format!("1{}", &length[1..]);
        let file = header_edit(
            bytes,
            head,
            "",
            &format!("\"length\":{length}"),
            &format!("\"length\":{shorter}"),
        )?;
        list.push((
            "length-before-end",
            format!("the header's length {length} made {shorter}, before the channels' ends"),
            file,
            i,
            "head",
            true,
        ));

        // The last block's start plus one: every earlier block's edges stay at or before it, and
        // the last block's later edges pass it.
        let ch = &head.header.channels[c];
        let key = format!("\"name\":\"{}\"", ch.name);
        let end = header_number(bytes, head, &key, "\"end\":")?;
        let last = head.rows.len() - 1;
        let earlier = (head.rows[last].start + 1).to_string();
        ensure!(
            earlier.len() == end.len(),
            "end {end} and {earlier} differ in length"
        );
        let file = header_edit(
            bytes,
            head,
            &key,
            &format!("\"end\":{end}"),
            &format!("\"end\":{earlier}"),
        )?;
        list.push((
            "edge-after-end",
            format!(
                "channel {c}'s end {end} made {earlier}, one tick into the last block, block {last}"
            ),
            file,
            last,
            "block",
            true,
        ));

        let f0 = frame(bytes, head, 0);
        let mut payload = zstd::bulk::decompress(f0, head.rows[0].decoded_length as usize)?;
        let (at, _) = section(&payload, c);
        let mut n = 1;
        while payload[at + n - 1] & 0x80 != 0 {
            n += 1;
        }
        for b in &mut payload[at..at + n - 1] {
            *b = 0x80;
        }
        payload[at + n - 1] = 0;
        let g = c19.compress(&payload)?;
        let file = splice(bytes, head, 0, &g, head.rows[0].decoded_length)?;
        list.push(("edge-at-tick-0", format!("block 0's first interval on channel {c} replaced by a zero of the same {n} bytes, so its first edge sits at tick 0; recompressed"), file, 0, "block", true));
    }

    let analog = head
        .header
        .channels
        .iter()
        .position(|c| matches!(c.data, ChannelKind::Analog { .. }));
    if let Some(a) = analog {
        let ch = &head.header.channels[a];
        let key = format!("\"name\":\"{}\"", ch.name);
        let bits = header_number(bytes, head, &key, "\"bits\":")?;
        ensure!(bits.len() == 2, "bits {bits} is not two digits");
        let file = header_edit(
            bytes,
            head,
            &key,
            &format!("\"bits\":{bits}"),
            "\"bits\":17",
        )?;
        list.push((
            "bits-17",
            format!("channel {a}'s bits {bits} made 17"),
            file,
            i,
            "head",
            true,
        ));

        let file = header_edit(bytes, head, &key, "\"period\":[1,", "\"period\":[0,")?;
        list.push((
            "period-zero",
            format!("channel {a}'s period numerator 1 made 0"),
            file,
            i,
            "head",
            true,
        ));

        let f = frame(bytes, head, i);
        let mut payload = zstd::bulk::decompress(f, head.rows[i].decoded_length as usize)?;
        let (at, len) = section(&payload, a);
        payload[at + len / 2] = 0xff;
        let g = c19.compress(&payload)?;
        let file = splice(bytes, head, i, &g, head.rows[i].decoded_length)?;
        list.push((
            "code-too-wide",
            format!(
                "block {i}'s first code on channel {a} given a high byte of 0xff; recompressed"
            ),
            file,
            i,
            "block",
            true,
        ));
    }

    let mut out_list = Vec::new();
    for (kind, what, g, block, stage, whole_file) in list {
        let native = if whole_file {
            match Head::parse(&g) {
                Err(e) => format!("head: {e}"),
                Ok(h) => match h.decode_block(block, frame(&g, &h, block)) {
                    Err(e) => e.to_string(),
                    Ok(_) => "accepted".to_owned(),
                },
            }
        } else {
            match head.decode_block(block, &g) {
                Err(e) => e.to_string(),
                Ok(_) => "accepted".to_owned(),
            }
        };
        let path = out.join(format!(
            "{name}.b{block}.{kind}.{}",
            if whole_file { "tickwave" } else { "bin" }
        ));
        fs::write(&path, &g)?;
        out_list.push(Corruption {
            kind,
            what,
            path: path.file_name().unwrap().to_string_lossy().into_owned(),
            whole_file,
            block,
            stage,
            native,
        });
    }
    Ok(out_list)
}

/// A copy of the file with block `i`'s frame and decoded length replaced, and every later
/// offset, and `file_len`, moved by the change in the frame's length.
fn splice(bytes: &[u8], head: &Head, i: usize, g: &[u8], decoded: u32) -> Result<Vec<u8>> {
    let mut rows = head.rows.clone();
    let delta = g.len() as i64 - i64::from(rows[i].length);
    rows[i].length = g.len() as u32;
    rows[i].decoded_length = decoded;
    for r in rows.iter_mut().skip(i + 1) {
        r.offset = (r.offset as i64 + delta) as u64;
    }
    let p = head.preamble;
    let moved = Preamble {
        file_len: (p.file_len as i64 + delta) as u64,
        ..p
    };
    let index_at = PREAMBLE_LEN + p.header_len as usize;
    let mut file = Vec::with_capacity(bytes.len());
    file.extend_from_slice(&moved.to_bytes());
    file.extend_from_slice(&bytes[PREAMBLE_LEN..index_at]);
    for r in &rows {
        r.write(&mut file);
    }
    // The overview blocks follow the data blocks, so they move by the same amount.
    for level in &head.overview_rows {
        for r in level {
            let mut r = *r;
            r.offset = (r.offset as i64 + delta) as u64;
            r.write(&mut file);
        }
    }
    file.extend_from_slice(&bytes[index_at + p.index_len as usize..rows[i].offset as usize]);
    file.extend_from_slice(g);
    let after = head.rows[i].offset as usize + head.rows[i].length as usize;
    file.extend_from_slice(&bytes[after..]);
    ensure!(file.len() as u64 == moved.file_len);
    Ok(file)
}

#[derive(Serialize)]
struct Timing {
    file: String,
    /// `ns[run][block]`
    ns: Vec<Vec<u64>>,
    edges: Vec<u64>,
    samples: Vec<u64>,
    whole_ns: Vec<u64>,
}

fn time(runs: usize, out: &Path, files: &[PathBuf]) -> Result<()> {
    fs::create_dir_all(out)?;
    let m = MultiProgress::new();
    for path in files {
        let (bytes, head) = open(path)?;
        let n = head.rows.len();
        let mut edges = vec![0; n];
        let mut samples = vec![0; n];
        for i in 0..n {
            let b = head.decode_block(i, frame(&bytes, &head, i)).unwrap();
            (edges[i], samples[i]) = items(&b);
        }
        let all = bar(&m, runs as u64, format!("{} runs", stem(path)));
        let mut ns = Vec::with_capacity(runs);
        let mut whole_ns = Vec::with_capacity(runs);
        for _ in 0..runs {
            let b = bar(&m, n as u64, "blocks".into());
            let mut row = Vec::with_capacity(n);
            let whole = Instant::now();
            for i in 0..n {
                let t = Instant::now();
                let block = head.decode_block(i, frame(&bytes, &head, i)).unwrap();
                row.push(t.elapsed().as_nanos() as u64);
                drop(std::hint::black_box(block));
                b.inc(1);
            }
            whole_ns.push(whole.elapsed().as_nanos() as u64);
            b.finish_and_clear();
            ns.push(row);
            all.inc(1);
        }
        all.finish_and_clear();
        let t = Timing {
            file: stem(path),
            ns,
            edges,
            samples,
            whole_ns,
        };
        fs::write(
            out.join(format!("{}.json", stem(path))),
            serde_json::to_string(&t)?,
        )?;
        m.suspend(|| eprintln!("{}: {runs} runs of {n} blocks", stem(path)));
    }
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Hashes { out, files } => hashes(&out, &files),
        Command::Corrupt { out, block, file } => corrupt(&out, block, &file),
        Command::Time { runs, out, files } => time(runs, &out, &files),
    }
}
