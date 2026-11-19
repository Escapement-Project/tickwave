//! Each refusal rule FORMAT.md states, one case a rule: a small valid recording with one thing
//! changed so that it breaks exactly that rule, read the way the rule says a reader reads it.
//! The cases are also the format's invalid-file vectors, which another implementation can check
//! its refusals against. They aren't kept in the repository: `write_the_invalid_vectors` writes
//! them, as testdata/format/README.md says.

use std::collections::BTreeMap;
use std::path::PathBuf;

use escapement_tickwave_format::decode::{DecodeError, Footer, Index, from_memory};
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::{
    AnalogCoding, Exact, IntCoding, PREAMBLE_LEN, TRAILER_LEN,
};
use escapement_tickwave_format::model::{
    Analog, Break, Channel, ChannelData, Digital, Gap, Ratio, Recording, Samples, Source,
    SourceFormat,
};
use escapement_tickwave_format::reader::{Ending, Reader};
use escapement_tickwave_format::record::{self, Kind};
use escapement_tickwave_format::verify::verify;
use serde_json::{Value, json};

mod common;

const LENGTH: i64 = 20_000;
/// A's edges, 8 ticks apart from tick 10, all before its first gap: enough for several blocks.
fn edges_a() -> Vec<i64> {
    (0..100).map(|k| 10 + 8 * k).collect()
}
/// G's gaps: more than a header holds, so they go in a list page.
const G_GAPS: i64 = 70;
/// V's and I's samples: a run of 500 from tick 100 + 1/2, then a break at 5,200 after 3 lost.
const COUNT: u64 = 1_500;

fn ratio(n: u64, d: u64) -> Ratio {
    Ratio::new(n, d).unwrap()
}

fn coding() -> IntCoding {
    IntCoding {
        scale: Exact::new(1, 1000),
        ..IntCoding::new(12, true)
    }
}

fn analog(name: &str) -> Channel {
    Channel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: ChannelData::Analog(Analog {
            significant: None,
            unit: if name == "V" { "V".into() } else { "A".into() },
            first: 100,
            phase: Some(ratio(1, 2)),
            period: Ratio::integer(10).unwrap(),
            count: COUNT,
            breaks: vec![Break {
                index: 500,
                tick: 5_200,
                lost: 3,
                phase: 1,
            }],
            lost_before: 2,
            lost_after: 2,
            samples: Samples::I64(
                coding(),
                Box::new((0..COUNT).map(|i| (i % 4000) as i64 - 2000)),
            ),
        }),
        shaping: Default::default(),
    }
}

fn digital(name: &str, edges: Vec<i64>, gaps: Vec<Gap>) -> Channel {
    Channel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: ChannelData::Digital(Digital {
            initial: false,
            end: LENGTH,
            count: edges.len() as u64,
            edges: Box::new(edges.into_iter()),
            gaps,
        }),
        shaping: Default::default(),
    }
}

/// The base: A, digital with two gaps in its header; G, digital with 70 gaps in a list page; V
/// and I, 12-bit analog on a half-tick phase with a break after lost samples and losses at both
/// ends; P = V × I; and three generated annotations.
fn base_recording() -> Recording {
    let g_gaps = (1..=G_GAPS)
        .map(|k| Gap {
            from: k * 200,
            to: k * 200 + 50,
            level: k % 2 == 0,
        })
        .collect();
    Recording {
        timebase: Ratio::integer(1_000_000).unwrap(),
        length: LENGTH,
        channels: vec![
            digital(
                "A",
                edges_a(),
                vec![
                    Gap {
                        from: 1_000,
                        to: 1_100,
                        level: true,
                    },
                    Gap {
                        from: 5_000,
                        to: 5_200,
                        level: false,
                    },
                ],
            ),
            digital("G", Vec::new(), g_gaps),
            analog("V"),
            analog("I"),
        ],
        derived: vec![escapement_tickwave_format::header::Derived {
            name: "P".into(),
            unit: "W".into(),
            op: escapement_tickwave_format::header::Op::Multiply,
            operands: vec!["V".into(), "I".into()],
            params: BTreeMap::new(),
            significant: None,
            requires: Vec::new(),
        }],
        sources: vec![Source::new(
            SourceFormat::Other("rules".into()),
            Ratio::integer(1_000_000).unwrap(),
        )],
    }
}

fn base() -> Vec<u8> {
    let mut o = Options::default();
    o.level = 3;
    o.block_bytes = 64;
    o.overview_shift = Some(4);
    o.analog = vec![
        None,
        None,
        Some(AnalogCoding::Int(coding())),
        Some(AnalogCoding::Int(coding())),
    ];
    for t in [150, 4_000, 12_000] {
        let Value::Object(m) = json!({"id": format!("mark/{t}"), "start": t}) else {
            unreachable!()
        };
        o.annotations.push(("gen".into(), m));
    }
    encode(base_recording(), &o).unwrap()
}

// --- How a rule is read -------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    /// A reader with random access before it draws: trailer, footer, header, top index, every
    /// page, every strip.
    Footer,
    /// A reader with random access that decodes every block and overview block the index places,
    /// and reads the annotations through their parts and the bundle.
    Random,
    /// A reader in order, from the first record to the trailer.
    InOrder,
    /// A full check against the recording the file was written from.
    Verify,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Footer => "footer",
            Mode::Random => "random",
            Mode::InOrder => "in-order",
            Mode::Verify => "verify",
        }
    }
}

fn footer_reads(file: &[u8]) -> Result<Index, DecodeError> {
    let index = Index::of_file(file)?;
    for o in 0..index.footer.channels.len() {
        if index.footer.available(o) {
            index.footer.strip(o)?;
        }
    }
    Ok(index)
}

fn random_reads(file: &[u8]) -> Result<(), DecodeError> {
    let index = footer_reads(file)?;
    for c in 0..index.header().channels.len() {
        if index.footer.available(c) {
            index.channel_blocks(file, c)?;
        }
    }
    for o in 0..index.footer.channels.len() {
        if index.footer.available(o) {
            for li in 0..index.levels[o].len() {
                index.level_in(file, o, li)?;
            }
        }
    }
    index.footer.read_sidecar(from_memory(file))?;
    Ok(())
}

/// A finished file read in order: one that ends cut off is not a whole recording.
fn in_order_reads(file: &[u8]) -> Result<(), DecodeError> {
    let mut r = Reader::default();
    r.feed(file);
    while r.next_event()?.is_some() {}
    match r.end()? {
        Ending::Complete => Ok(()),
        Ending::CutOff { at } => Err(DecodeError::Corrupt(format!("cut off at {at}"))),
    }
}

/// Why `mode` refuses `file`, or None where it takes it.
fn refusal(mode: Mode, file: &[u8]) -> Option<String> {
    let result = match mode {
        Mode::Footer => footer_reads(file).map(|_| ()).map_err(|e| e.to_string()),
        Mode::Random => random_reads(file).map_err(|e| e.to_string()),
        Mode::InOrder => in_order_reads(file).map_err(|e| e.to_string()),
        Mode::Verify => verify(file, base_recording())
            .map(|_| ())
            .map_err(|e| e.to_string()),
    };
    result.err()
}

// --- Editing a file -----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Rec {
    at: usize,
    kind: u8,
    len: usize,
}

impl Rec {
    fn payload(self) -> std::ops::Range<usize> {
        self.at + record::PAYLOAD_AT..self.at + self.len - record::CHECK_LEN
    }
}

/// Every record from the declaration to the footer, in order.
fn records(file: &[u8]) -> Vec<Rec> {
    let mut out = Vec::new();
    let mut at = PREAMBLE_LEN;
    while at < file.len() - TRAILER_LEN {
        match record::next(&file[at..], usize::MAX) {
            record::Next::Record { kind, len, .. } => {
                out.push(Rec { at, kind, len });
                at += len;
            }
            other => panic!("no record at {at}: {other:?}"),
        }
    }
    out
}

/// Channel `c`'s block record `n`.
fn block_of(file: &[u8], c: u32, n: usize) -> Rec {
    records(file)
        .into_iter()
        .filter(|r| r.kind == Kind::Block as u8)
        .filter(|r| file[r.payload()][0..4] == c.to_le_bytes())
        .nth(n)
        .unwrap_or_else(|| panic!("no block {n} of channel {c}"))
}

/// `file` with every run record at sample `index` edited, so both of a derived channel's
/// operands keep the same runs.
fn runs_edited(file: &[u8], index: u64, edit: impl Fn(&mut Vec<u8>)) -> Vec<u8> {
    let mut f = file.to_vec();
    let mut n = 0;
    while let Some(r) = records(&f)
        .into_iter()
        .filter(|r| r.kind == Kind::Run as u8)
        .filter(|r| f[r.payload()][4..12] == index.to_le_bytes())
        .nth(n)
    {
        f = repayloaded(&f, r, &edit);
        n += 1;
    }
    assert!(n >= 2, "runs at {index}: {n}");
    f
}

fn nth(file: &[u8], kind: Kind, n: usize) -> Rec {
    records(file)
        .into_iter()
        .filter(|r| r.kind == kind as u8)
        .nth(n)
        .unwrap_or_else(|| panic!("no record {n} of kind {kind:?}"))
}

/// A record of kind byte `kind` holding `payload`, its check right.
fn framed(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![kind];
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    let check = blake3::Hasher::new()
        .update(&[kind])
        .update(&(payload.len() as u32).to_le_bytes())
        .update(payload)
        .finalize();
    out.extend_from_slice(&check.as_bytes()[..8]);
    out
}

/// `file` with `r` replaced by `with`. Records after it move where the lengths differ, so the
/// footer's places past it no longer hold: for readers in order.
fn spliced(file: &[u8], r: Rec, with: &[u8]) -> Vec<u8> {
    let mut out = file[..r.at].to_vec();
    out.extend_from_slice(with);
    out.extend_from_slice(&file[r.at + r.len..]);
    let delta = with.len() as i64 - r.len as i64;
    relaid(&out, r.at + r.len..usize::MAX, delta)
}

fn inserted(file: &[u8], at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut out = file[..at].to_vec();
    out.extend_from_slice(bytes);
    out.extend_from_slice(&file[at..]);
    relaid(&out, at..usize::MAX, bytes.len() as i64)
}

/// `file`, whose bytes that stood at `moved` (in the file before the edit) now lie `delta`
/// further on, with every offset the file states into them moved too: block, overview and part
/// rows, directory entries in pages and the top index, the bundle's slot and the trailer. Each
/// block row then takes its start, frame length, decoded length and word from its block record,
/// so the one record an edit changed is the only thing out of step: a reader that lacks the
/// case's rule reads the rest as whole.
fn relaid(file: &[u8], moved: std::ops::Range<usize>, delta: i64) -> Vec<u8> {
    let shift = |p: &mut [u8]| {
        let o = u64::from_le_bytes(p[..8].try_into().unwrap()) as usize;
        if o != 0 && moved.contains(&o) {
            let n = (o as i64 + delta) as u64;
            p[..8].copy_from_slice(&n.to_le_bytes());
        }
    };
    let mut out = file.to_vec();
    // The block records as they now lie: frame offset to start, frame length, decoded length
    // and word.
    let mut blocks = std::collections::HashMap::new();
    for r in records(&out) {
        if r.kind == Kind::Block as u8 {
            let p = &out[r.payload()];
            blocks.insert(
                (r.at + record::PAYLOAD_AT + HEAD) as u64,
                (
                    p[4..12].to_vec(),
                    (r.len - record::FRAMING - HEAD) as u32,
                    p[24..28].to_vec(),
                    p[20..24].to_vec(),
                ),
            );
        }
    }
    for r in records(&out) {
        let mut p = out[r.payload()].to_vec();
        if r.kind == Kind::IndexPage as u8 {
            let (table, depth) = (p[4], p[6]);
            let entries = &mut p[PAGE_HEAD..];
            if depth > 0 {
                for e in entries.chunks_mut(28) {
                    shift(&mut e[16..24]);
                }
            } else if table == 0 {
                for row in entries.chunks_mut(28) {
                    shift(&mut row[8..16]);
                    let at = u64::from_le_bytes(row[8..16].try_into().unwrap());
                    if let Some((start, len, decoded, word)) = blocks.get(&at) {
                        row[0..8].copy_from_slice(start);
                        row[16..20].copy_from_slice(&len.to_le_bytes());
                        row[20..24].copy_from_slice(decoded);
                        row[24..28].copy_from_slice(word);
                    }
                }
            } else if table == 1 {
                for row in entries.chunks_mut(20) {
                    shift(&mut row[0..8]);
                }
            } else if table == 4 {
                for row in entries.chunks_mut(32) {
                    shift(&mut row[0..8]);
                }
            }
        } else if r.kind == Kind::Footer as u8 && r.at + r.len == out.len() - TRAILER_LEN {
            let h = u32::from_le_bytes(p[0..4].try_into().unwrap()) as usize;
            let header: Value = serde_json::from_slice(&p[8..8 + h]).unwrap();
            let top = &mut p[8 + h..];
            shift(&mut top[0..8]);
            let mut at = 12;
            let tables = |top: &mut [u8], at: &mut usize, n: usize| {
                for _ in 0..n {
                    let e = usize::from(top[*at + 1]);
                    *at += 2;
                    for _ in 0..e {
                        shift(&mut top[*at + 16..*at + 24]);
                        *at += 28;
                    }
                }
            };
            if header.get("annotation_parts").is_some() {
                tables(top, &mut at, 1);
            }
            let levels = |c: &Value| c["overview_blocks"].as_array().map_or(0, Vec::len);
            for c in header["channels"].as_array().unwrap() {
                at += 8;
                let list = c.get("gap_count").is_some() || c.get("break_count").is_some();
                tables(top, &mut at, 1 + levels(c) + usize::from(list));
            }
            for d in header["derived"].as_array().into_iter().flatten() {
                at += 8;
                tables(top, &mut at, levels(d));
            }
        } else {
            continue;
        }
        let rec = framed(r.kind, &p);
        out[r.at..r.at + r.len].copy_from_slice(&rec);
    }
    let t = out.len() - TRAILER_LEN;
    shift(&mut out[t..t + 8]);
    out
}

/// `file` with record `r`'s payload edited, framed again in place.
fn repayloaded(file: &[u8], r: Rec, edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut p = file[r.payload()].to_vec();
    edit(&mut p);
    spliced(file, r, &framed(r.kind, &p))
}

/// The footer record, and its payload split into header, top index and strips.
struct Foot {
    rec: Rec,
    header: Value,
    top: Vec<u8>,
    strips: Vec<u8>,
}

fn foot(file: &[u8]) -> Foot {
    let rec = *records(file).last().unwrap();
    assert_eq!(rec.kind, Kind::Footer as u8);
    let p = &file[rec.payload()];
    let h = u32::from_le_bytes(p[0..4].try_into().unwrap()) as usize;
    let i = u32::from_le_bytes(p[4..8].try_into().unwrap()) as usize;
    Foot {
        rec,
        header: serde_json::from_slice(&p[8..8 + h]).unwrap(),
        top: p[8 + h..8 + h + i].to_vec(),
        strips: p[8 + h + i..].to_vec(),
    }
}

/// `file` with its footer's payload made from `header_len`, `index_len` and the parts given,
/// the footer framed again and the trailer pointed at it.
fn refooted_raw(file: &[u8], header_len: u32, index_len: u32, rest: &[u8]) -> Vec<u8> {
    let f = foot(file);
    let mut p = header_len.to_le_bytes().to_vec();
    p.extend_from_slice(&index_len.to_le_bytes());
    p.extend_from_slice(rest);
    let rec = framed(Kind::Footer as u8, &p);
    let mut out = file[..f.rec.at].to_vec();
    out.extend_from_slice(&rec);
    let mut trailer = file[file.len() - TRAILER_LEN..].to_vec();
    trailer[0..8].copy_from_slice(&(f.rec.at as u64).to_le_bytes());
    trailer[8..12].copy_from_slice(&(rec.len() as u32).to_le_bytes());
    out.extend_from_slice(&trailer);
    out
}

fn refooted(file: &[u8], edit: impl FnOnce(&mut Foot)) -> Vec<u8> {
    let mut f = foot(file);
    edit(&mut f);
    let h = serde_json::to_vec(&f.header).unwrap();
    let mut rest = h.clone();
    rest.extend_from_slice(&f.top);
    rest.extend_from_slice(&f.strips);
    refooted_raw(file, h.len() as u32, f.top.len() as u32, &rest)
}

/// `file` with its footer's header JSON edited.
fn reheadered(file: &[u8], edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    refooted(file, |f| edit(&mut f.header))
}

/// `file` with its footer's header edited as text, which a `Value` can't hold: a key named twice.
fn header_text_edited(file: &[u8], edit: impl FnOnce(&str) -> String) -> Vec<u8> {
    let f = foot(file);
    let h = edit(&serde_json::to_string(&f.header).unwrap());
    let mut rest = h.as_bytes().to_vec();
    rest.extend_from_slice(&f.top);
    rest.extend_from_slice(&f.strips);
    refooted_raw(file, h.len() as u32, f.top.len() as u32, &rest)
}

/// `file` with its declaration's JSON edited: for readers in order, which refuse it there.
fn redeclared(file: &[u8], edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let r = nth(file, Kind::Declaration, 0);
    let mut json: Value = serde_json::from_slice(&file[r.payload()]).unwrap();
    edit(&mut json);
    spliced(
        file,
        r,
        &framed(r.kind, &serde_json::to_vec(&json).unwrap()),
    )
}

fn channel<'a>(h: &'a mut Value, name: &str) -> &'a mut Value {
    h["channels"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|c| c["name"] == name)
        .unwrap()
}

/// Channel `name`'s entry, whose kind's fields sit beside its name.
fn data<'a>(h: &'a mut Value, name: &str) -> &'a mut Value {
    channel(h, name)
}

fn trailer_edited(file: &[u8], edit: impl FnOnce(&mut [u8])) -> Vec<u8> {
    let mut out = file.to_vec();
    let t = out.len() - TRAILER_LEN;
    edit(&mut out[t..]);
    out
}

/// Where a top index entry for channel `c`'s blocks starts, and the place of its first directory
/// entry's length field.
fn page_entry(file: &[u8], c: usize) -> escapement_tickwave_format::index::Entry {
    let footer = Footer::parse_file(file).unwrap();
    footer.channels[c].blocks.as_ref().unwrap().levels[0][0]
}

/// `file` with the page record at `entry` given an edited payload of the same length.
fn page_edited(
    file: &[u8],
    entry: escapement_tickwave_format::index::Entry,
    edit: impl FnOnce(&mut [u8]),
) -> Vec<u8> {
    let at = entry.offset as usize;
    let r = Rec {
        at,
        kind: file[at],
        len: entry.length as usize,
    };
    let mut p = file[r.payload()].to_vec();
    edit(&mut p);
    let rec = framed(r.kind, &p);
    assert_eq!(rec.len(), r.len);
    let mut out = file.to_vec();
    out[at..at + rec.len()].copy_from_slice(&rec);
    out
}

fn list_entry(file: &[u8], c: usize) -> escapement_tickwave_format::index::Entry {
    let footer = Footer::parse_file(file).unwrap();
    footer.channels[c].list.as_ref().unwrap().levels[0][0]
}

// --- A second base: a live writer's file, with known-through records -----------------------------

/// A recorder's file: digital D with edges, a flush at 100 and 200, a gap learned after the
/// second flush that starts before it, and a flush at 400, finished at 500.
/// The live bases' one digital channel, D, at 1 MHz.
fn live_declaration() -> escapement_tickwave_format::header::Declaration {
    use escapement_tickwave_format::header::{
        Declaration, DeclaredChannel, DeclaredKind, SourceHeader,
    };
    let timebase = Ratio::integer(1_000_000).unwrap();
    Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "D".into(),
            source_name: "d".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
            overview_shift: None,
            codec: None,
            requires: Vec::new(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("live".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
        requires: Vec::new(),
    }
}

fn live_writer() -> escapement_tickwave_format::writer::Writer {
    use escapement_tickwave_format::writer::{WriteOptions, Writer};
    let options = WriteOptions {
        level: 3,
        ..WriteOptions::default()
    };
    Writer::new(live_declaration(), options).unwrap()
}

fn live_base() -> Vec<u8> {
    use escapement_tickwave_format::writer::Finish;
    let mut w = live_writer();
    for t in [10, 20, 30] {
        w.edge(0, t).unwrap();
    }
    w.flush(100).unwrap();
    w.edge(0, 150).unwrap();
    w.flush(200).unwrap();
    w.gap(
        0,
        Gap {
            from: 180,
            to: 250,
            level: true,
        },
    )
    .unwrap();
    w.edge(0, 300).unwrap();
    w.flush(400).unwrap();
    let mut file = w.take_output();
    file.extend(w.finish(500, &[500], Finish::default()).unwrap().tail);
    file
}

/// A live recording stopped inside a loss: D's last gap runs past the length, so the header
/// leaves it out and D ends at its `from`.
fn left_out_base() -> Vec<u8> {
    use escapement_tickwave_format::writer::Finish;
    let mut w = live_writer();
    for t in [10, 20, 30] {
        w.edge(0, t).unwrap();
    }
    w.flush(100).unwrap();
    w.gap(
        0,
        Gap {
            from: 150,
            to: 600,
            level: true,
        },
    )
    .unwrap();
    let mut file = w.take_output();
    file.extend(w.finish(500, &[500], Finish::default()).unwrap().tail);
    file
}

fn known_through_edited(file: &[u8], n: usize, tick: i64) -> Vec<u8> {
    repayloaded(file, nth(file, Kind::KnownThrough, n), |p| {
        p.copy_from_slice(&tick.to_le_bytes())
    })
}

// --- Block contents ------------------------------------------------------------------------------

const HEAD: usize = 28;

/// `file` with block record `r`'s decoded section edited, compressed again with a checksum and
/// its decoded length restated: for readers in order.
fn section_edited(file: &[u8], r: Rec, edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    repayloaded(file, r, |p| {
        let mut section = zstd::bulk::decompress(&p[HEAD..], 1 << 22).unwrap();
        edit(&mut section);
        let frame = escapement_tickwave_format::compress::compress(&section, 3).unwrap();
        p.truncate(HEAD);
        p[24..28].copy_from_slice(&(section.len() as u32).to_le_bytes());
        p.extend_from_slice(&frame);
    })
}

/// `file` with block record `r`'s frame replaced by `frame` as it is.
fn frame_replaced(file: &[u8], r: Rec, frame: impl FnOnce(&[u8]) -> Vec<u8>) -> Vec<u8> {
    repayloaded(file, r, |p| {
        let f = frame(&p[HEAD..]);
        p.truncate(HEAD);
        p.extend_from_slice(&f);
    })
}

fn leb_read(mut bytes: &[u8]) -> Vec<u64> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = bytes[0];
            bytes = &bytes[1..];
            v |= u64::from(b & 0x7f) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                break;
            }
        }
        out.push(v);
    }
    out
}

fn leb_write(values: &[u64]) -> Vec<u8> {
    let mut out = Vec::new();
    for &v in values {
        let mut v = v;
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
    }
    out
}

fn lebs_edited(file: &[u8], r: Rec, edit: impl FnOnce(&mut Vec<u64>)) -> Vec<u8> {
    section_edited(file, r, |s| {
        let mut v = leb_read(s);
        edit(&mut v);
        *s = leb_write(&v);
    })
}

/// Channel `c`'s records of `kind` whose payload starts with its number.
fn of_channel(file: &[u8], kind: Kind, c: u32) -> Vec<Rec> {
    records(file)
        .into_iter()
        .filter(|r| r.kind == kind as u8 && file[r.payload()][0..4] == c.to_le_bytes())
        .collect()
}

/// `file` with the payloads of records `x` and `y`, of one length, swapped.
fn swapped(file: &[u8], x: Rec, y: Rec) -> Vec<u8> {
    assert_eq!(x.len, y.len);
    let mut out = file.to_vec();
    let a = file[x.at..x.at + x.len].to_vec();
    let b = file[y.at..y.at + y.len].to_vec();
    out[x.at..x.at + x.len].copy_from_slice(&b);
    out[y.at..y.at + y.len].copy_from_slice(&a);
    out
}

/// `file` with record `r` moved to just after record `after`, which comes later.
fn moved_after(file: &[u8], r: Rec, after: Rec) -> Vec<u8> {
    assert!(after.at > r.at);
    let bytes = file[r.at..r.at + r.len].to_vec();
    let mut out = file[..r.at].to_vec();
    out.extend_from_slice(&file[r.at + r.len..after.at + after.len]);
    out.extend_from_slice(&bytes);
    out.extend_from_slice(&file[after.at + after.len..]);
    relaid(&out, r.at + r.len..after.at + after.len, -(r.len as i64))
}

fn block_span(file: &[u8], r: Rec) -> (i64, i64) {
    let p = &file[r.payload()];
    (
        i64::from_le_bytes(p[4..12].try_into().unwrap()),
        i64::from_le_bytes(p[12..20].try_into().unwrap()),
    )
}

/// An annotation record of generator `name` holding `line`, its newline added.
fn annotation(name: &str, line: &str) -> Vec<u8> {
    let mut p = (name.len() as u16).to_le_bytes().to_vec();
    p.extend_from_slice(name.as_bytes());
    p.extend_from_slice(line.as_bytes());
    p.push(b'\n');
    framed(Kind::Annotation as u8, &p)
}

/// `file` with annotation record `n` holding `name` and `line`.
fn annotation_record(file: &[u8], n: usize, name: &str, line: &str) -> Vec<u8> {
    spliced(
        file,
        nth(file, Kind::Annotation, n),
        &annotation(name, line),
    )
}

/// `file` with its bundle's JSON edited as text, which a map can't hold: a file named twice.
fn bundle_text_edited(file: &[u8], edit: impl FnOnce(&str) -> String) -> Vec<u8> {
    let mut len = 0;
    let out = repayloaded(file, nth(file, Kind::Bundle, 0), |p| {
        let raw = zstd::bulk::decompress(&p[4..], 1 << 22).unwrap();
        let raw = edit(std::str::from_utf8(&raw).unwrap()).into_bytes();
        p.clear();
        p.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        p.extend_from_slice(&escapement_tickwave_format::compress::compress(&raw, 3).unwrap());
        len = p.len() + record::FRAMING;
    });
    refooted(&out, |f| {
        f.top[8..12].copy_from_slice(&(len as u32).to_le_bytes())
    })
}

/// `file` with its bundle's files edited, the bundle record framed again and the footer's slot
/// for it restated.
fn bundle_edited(file: &[u8], edit: impl FnOnce(&mut BTreeMap<String, String>)) -> Vec<u8> {
    let mut len = 0;
    let out = repayloaded(file, nth(file, Kind::Bundle, 0), |p| {
        let raw = zstd::bulk::decompress(&p[4..], 1 << 22).unwrap();
        let mut files: BTreeMap<String, String> = serde_json::from_slice(&raw).unwrap();
        edit(&mut files);
        let raw = serde_json::to_vec(&files).unwrap();
        p.clear();
        p.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        p.extend_from_slice(&escapement_tickwave_format::compress::compress(&raw, 3).unwrap());
        len = p.len() + record::FRAMING;
    });
    refooted(&out, |f| {
        f.top[8..12].copy_from_slice(&(len as u32).to_le_bytes())
    })
}

fn level_entry(file: &[u8], o: usize) -> escapement_tickwave_format::index::Entry {
    let footer = Footer::parse_file(file).unwrap();
    footer.channels[o].levels[0].1.levels[0][0]
}

// --- The cases ----------------------------------------------------------------------------------

struct Case {
    id: &'static str,
    rule: &'static str,
    mode: Mode,
    /// A word of the refusal that says the rule refused it, not some other.
    because: &'static str,
    file: Vec<u8>,
    /// Whether it goes in the vectors: not where the file would be large for what it shows.
    vector: bool,
}

fn case(
    id: &'static str,
    mode: Mode,
    because: &'static str,
    rule: &'static str,
    file: Vec<u8>,
) -> Case {
    Case {
        id,
        rule,
        mode,
        because,
        file,
        vector: true,
    }
}

/// A case kept out of the vectors.
fn unlisted(mut c: Case) -> Case {
    c.vector = false;
    c
}

const PAGE_HEAD: usize = escapement_tickwave_format::index::PAGE_HEAD;

fn cases(b: &[u8]) -> Vec<Case> {
    use Mode::*;
    let mut out = Vec::new();
    let skip = |kind: u8| framed(kind, b"later");

    // Framing and versions.
    out.push(case(
        "FR01-first-record-not-declaration",
        InOrder,
        "first record",
        "Records: a file whose first record is not the declaration is refused",
        inserted(
            b,
            PREAMBLE_LEN,
            &framed(Kind::KnownThrough as u8, &0i64.to_le_bytes()),
        ),
    ));
    out.push(case(
        "FR02-unknown-kind-9",
        InOrder,
        "unknown kind 9",
        "Records: a reader refuses a record of a kind it does not know from 1 to 127, kind 9 included",
        inserted(b, nth(b, Kind::Block, 0).at, &skip(9)),
    ));
    out.push(case(
        "FR02-unknown-kind-127",
        InOrder,
        "unknown kind 127",
        "Records: a reader refuses a record of a kind it does not know from 1 to 127",
        inserted(b, nth(b, Kind::Block, 0).at, &skip(127)),
    ));
    out.push(case(
        "FR03-extension-before-declaration",
        InOrder,
        "first record",
        "Records: kinds from 128 are skipped anywhere after the declaration; a first record that is not the declaration is refused",
        inserted(b, PREAMBLE_LEN, &skip(200)),
    ));
    let extra = framed(200, b"after");
    out.push(case(
        "FR04-footer-not-before-trailer",
        Footer,
        "trailer",
        "Trailer: the footer record ends where the trailer starts, and a reader refuses a trailer that says otherwise",
        {
            let t = b.len() - TRAILER_LEN;
            inserted(b, t, &extra)
        },
    ));
    out.push(case(
        "FR05-trailer-major-2",
        Footer,
        "version",
        "Trailer: a reader refuses a major version it does not know",
        trailer_edited(b, |t| t[12..14].copy_from_slice(&2u16.to_le_bytes())),
    ));
    out.push(case(
        "FR06-preamble-major-2",
        InOrder,
        "version",
        "Preamble: a reader refuses a major version it does not know",
        {
            let mut f = b.to_vec();
            f[8..10].copy_from_slice(&2u16.to_le_bytes());
            f
        },
    ));
    out.push(case(
        "FR07-trailer-minor-differs",
        InOrder,
        "differ",
        "Trailer: a reader that reads the preamble too refuses a trailer whose versions differ from it",
        trailer_edited(b, |t| t[14..16].copy_from_slice(&1u16.to_le_bytes())),
    ));
    out.push(case(
        "FR08-byte-after-trailer",
        InOrder,
        "trailer",
        "In order: after the footer record come exactly the trailer's 24 bytes",
        {
            let mut f = b.to_vec();
            f.push(0);
            f
        },
    ));
    out.push(case(
        "FR08-byte-after-trailer-end",
        Footer,
        "trailer",
        "Trailer: the file ends with the trailer, its magic last",
        {
            let mut f = b.to_vec();
            f.push(0);
            f
        },
    ));
    out.push(case(
        "FR09-unknown-requires",
        Footer,
        "requires",
        "Header: a reader refuses a recording whose `requires` names one it doesn't know",
        reheadered(b, |h| h["requires"] = json!(["later-feature"])),
    ));
    out.push(case(
        "FR09-unknown-requires-declared",
        InOrder,
        "requires",
        "Header: a reader refuses a recording whose `requires` names one it doesn't know",
        redeclared(b, |h| h["requires"] = json!(["later-feature"])),
    ));
    out.push(case(
        "FR10-gap-record-oversized",
        InOrder,
        "cut off",
        "Limits: a gap record's payload is 21 bytes exactly; a reader stops at a record whose `n` passes that before it reads the payload, so the file is cut off there",
        repayloaded(b, nth(b, Kind::Gap, 0), |p| p.push(0)),
    ));

    // Header.
    out.push(case(
        "HD03-end-past-length",
        Footer,
        "",
        "Header: a digital channel's `end` maps into 0..=length",
        reheadered(b, |h| data(h, "A")["end"] = json!(LENGTH + 1)),
    ));
    out.push(case(
        "HD04-origin-above-0",
        Footer,
        "",
        "Header: in a recording that states no kept span, the source's `origin` is at most 0",
        reheadered(b, |h| h["sources"][0]["origin"] = json!(5)),
    ));
    out.push(case(
        "HD05-length-0",
        Footer,
        "",
        "Header: `length` is at least 1",
        reheadered(b, |h| h["length"] = json!(0)),
    ));
    out.push(case(
        "HD06-length-2-53",
        Footer,
        "",
        "Header: `length` is below 2^53",
        reheadered(b, |h| h["length"] = json!(1u64 << 53)),
    ));
    out.push(case(
        "HD06-edges-2-53",
        Footer,
        "",
        "Header: `edges` is below 2^53",
        reheadered(b, |h| data(h, "A")["edges"] = json!(1u64 << 53)),
    ));
    out.push(case(
        "HD07-origin-magnitude",
        Footer,
        "",
        "Header: every `origin` has a magnitude below 2^53",
        reheadered(b, |h| h["sources"][0]["origin"] = json!(-(1i64 << 53))),
    ));
    out.push(case(
        "HD08-timebase-zero-term",
        Footer,
        "",
        "Header: no term of a `timebase` is zero",
        reheadered(b, |h| h["timebase"] = json!([0, 1])),
    ));
    out.push(case(
        "HD09-period-below-1",
        Footer,
        "",
        "Header: a `period` is at least 1",
        reheadered(b, |h| data(h, "V")["period"] = json!([1, 2])),
    ));
    out.push(case(
        "HD10-stretch-past-2",
        Footer,
        "",
        "Header: a `stretch` lies in 1/2..=2",
        reheadered(b, |h| h["sources"][0]["stretch"] = json!([3, 1])),
    ));
    out.push(case(
        "HD11-grid-2-16",
        Footer,
        "2^16",
        "Header: a channel's grid `D` is below 2^16",
        reheadered(b, |h| {
            data(h, "V")["period"] = json!([655_371, 65_537]);
            data(h, "I")["period"] = json!([655_371, 65_537]);
        }),
    ));
    out.push(case(
        "HD12-timebase-not-lowest",
        Footer,
        "",
        "Header: every `[num, den]` is in lowest terms with a positive `den`",
        reheadered(b, |h| h["timebase"] = json!([2_000_000, 2])),
    ));
    out.push(case(
        "HD12-scale-not-lowest",
        Footer,
        "",
        "Header: every `[num, den]` in the header, a coding's `scale` included, is in lowest terms",
        reheadered(b, |h| data(h, "V")["coding"]["scale"] = json!([2, 2000])),
    ));
    out.push(case(
        "HD13-phase-num-past-den",
        Footer,
        "",
        "Header: an analog `phase` has `num` from 1 to `den - 1`",
        reheadered(b, |h| data(h, "V")["phase"] = json!([3, 2])),
    ));
    out.push(case(
        "HD14-never-known-with-edges-blocks",
        Footer,
        "ends at its start",
        "Header: a channel whose `end` is at its start has no edges; its blocks' edges then lie outside the channel",
        reheadered(b, |h| {
            let a = data(h, "A");
            a["end"] = json!(0);
            a.as_object_mut().unwrap().remove("gaps");
        }),
    ));
    out.push(case(
        "HD15-leading-gap-initial-true",
        Footer,
        "",
        "Gaps: a channel with a gap from its start has an `initial` of false",
        reheadered(b, |h| {
            let a = data(h, "A");
            a["initial"] = json!(true);
            a["gaps"][0] = json!([0, 5, true]);
        }),
    ));
    out.push(case(
        "HD18-one-source-timebase-differs",
        Footer,
        "",
        "Times: a reader refuses a recording of one source whose `timebase` has another value than the source's",
        reheadered(b, |h| h["sources"][0]["timebase"] = json!([2_000_000, 1])),
    ));
    out.push(case(
        "HD19-names-shared",
        Footer,
        "",
        "Derived channels: no two channels or derived channels share a name",
        reheadered(b, |h| channel(h, "G")["name"] = json!("A")),
    ));
    out.push(case(
        "HD19-derived-name-shared",
        Footer,
        "",
        "Derived channels: no two channels or derived channels share a name",
        reheadered(b, |h| h["derived"][0]["name"] = json!("V")),
    ));
    out.push(case(
        "HD20-operand-digital",
        Footer,
        "",
        "Derived channels: a reader refuses operands that differ in what pairing needs",
        reheadered(b, |h| h["derived"][0]["operands"] = json!(["V", "A"])),
    ));
    out.push(case(
        "HD20-operand-count-differs",
        Footer,
        "",
        "Derived channels: a reader refuses operands that differ in any of these",
        reheadered(b, |h| data(h, "I")["count"] = json!(COUNT - 1)),
    ));
    out.push(case(
        "HD21-offset-without-kind",
        Footer,
        "",
        "Moved and trimmed channels: a reader refuses an `offset` without an `offset_kind`",
        reheadered(b, |h| channel(h, "A")["offset"] = json!(5)),
    ));
    out.push(case(
        "HD21-kind-without-offset",
        Footer,
        "",
        "Moved and trimmed channels: a reader refuses an `offset_kind` without an `offset`",
        reheadered(b, |h| channel(h, "A")["offset_kind"] = json!("aligned")),
    ));
    out.push(case(
        "HD22-kept-on-some",
        Footer,
        "",
        "Moved and trimmed channels: a reader refuses a recording where only some channels state `kept`",
        reheadered(b, |h| channel(h, "A")["kept"] = json!([0, LENGTH])),
    ));
    out.push(case(
        "HD24-lost-before-below-0",
        Footer,
        "",
        "Gaps: a reader refuses a loss whose start lies below source tick 0",
        reheadered(b, |h| {
            data(h, "V")["lost_before"] = json!(1000);
            data(h, "I")["lost_before"] = json!(1000);
        }),
    ));
    out.push(case(
        "HD25-lost-after-past-length",
        Footer,
        "",
        "Gaps: a reader refuses a loss whose end maps past the length",
        reheadered(b, |h| {
            data(h, "V")["lost_after"] = json!(1_000_000);
            data(h, "I")["lost_after"] = json!(1_000_000);
        }),
    ));
    out.push(case(
        "HD26-gap-count-64",
        Footer,
        "",
        "Gaps: a reader refuses a `gap_count` of 64 or fewer",
        reheadered(b, |h| data(h, "G")["gap_count"] = json!(64)),
    ));
    out.push(case(
        "HD26-header-list-65",
        Footer,
        "",
        "Gaps: a reader refuses a list in the header of more than 64 entries",
        reheadered(b, |h| {
            data(h, "A")["gaps"] = Value::Array(
                (0..65)
                    .map(|k| json!([2_000 + k * 20, 2_005 + k * 20, false]))
                    .collect(),
            )
        }),
    ));
    out.push(case(
        "HD27-overview-level-not-given",
        Footer,
        "",
        "Overview: a reader refuses a header that lists other levels in `overview_blocks`",
        reheadered(b, |h| {
            channel(h, "A")["overview_blocks"][0]["shift"] = json!(1)
        }),
    ));
    out.push(case(
        "HD29-gaps-out-of-order",
        Footer,
        "",
        "Gaps: the gaps go in time order, each gap's `from` past the `to` before it",
        reheadered(b, |h| {
            let g = data(h, "A")["gaps"].as_array_mut().unwrap();
            g.swap(0, 1);
        }),
    ));
    out.push(case(
        "HD29-gap-from-not-below-to",
        Footer,
        "",
        "Gaps: `from < to`",
        reheadered(b, |h| data(h, "A")["gaps"][0] = json!([1_100, 1_100, true])),
    ));
    out.push(case(
        "HD29-gap-to-at-end",
        Footer,
        "",
        "Gaps: `to < end`",
        reheadered(b, |h| {
            data(h, "A")["gaps"][1] = json!([5_000, LENGTH, false])
        }),
    ));
    out.push(case(
        "HD30-break-index-0",
        Footer,
        "",
        "Gaps: each break's `index` lies in 1..count",
        reheadered(b, |h| {
            data(h, "V")["breaks"][0][0] = json!(0);
            data(h, "I")["breaks"][0][0] = json!(0);
        }),
    ));
    out.push(case(
        "HD30-break-before-sample",
        Footer,
        "",
        "Gaps: each run's start is at least the place of the sample before it",
        reheadered(b, |h| {
            data(h, "V")["breaks"][0][1] = json!(4_000);
            data(h, "I")["breaks"][0][1] = json!(4_000);
        }),
    ));
    out.push(case(
        "HD30-break-lost-not-past-place",
        Footer,
        "",
        "Gaps: where a run states lost samples, it starts past place `index` on the run before it",
        reheadered(b, |h| {
            data(h, "V")["breaks"][0][1] = json!(5_100);
            data(h, "I")["breaks"][0][1] = json!(5_100);
        }),
    ));
    out.push(case(
        "HD31-content-id-upper-case",
        Footer,
        "lower-case",
        "Header: `content_id` is `blake3:` and the 64 lower-case hex digits of the content's digest",
        reheadered(b, |h| {
            let id = h["content_id"].as_str().unwrap().to_owned();
            h["content_id"] = json!(format!("blake3:{}", id["blake3:".len()..].to_uppercase()));
        }),
    ));
    out.push(unlisted(case(
        "HD28-channels-past-4096",
        Footer,
        "",
        "Limits: a recording has at most 4,096 channels and derived channels together",
        reheadered(b, |h| {
            let one = channel(h, "G").clone();
            let chans = h["channels"].as_array_mut().unwrap();
            for k in 0..4_100 {
                let mut c = one.clone();
                c["name"] = json!(format!("c{k}"));
                chans.push(c);
            }
        }),
    )));

    // Index and footer.
    out.push(case(
        "IX04-top-index-longer",
        Footer,
        "top index",
        "Footer: a reader refuses a top index that is not exactly `index_len` bytes",
        refooted(b, |f| f.top.push(0)),
    ));
    out.push(case(
        "IX05-strips-past-footer",
        Footer,
        "strips",
        "Footer: a reader refuses strips whose frames don't end where the footer does",
        refooted(b, |f| f.strips.push(0)),
    ));
    out.push(case(
        "IX02-directory-entry-length",
        Footer,
        "bytes",
        "Index: a reader refuses a directory entry whose length differs from what the table's shape gives, before it fetches the page",
        refooted(b, |f| {
            // The first channel's entry after the bundle (12 bytes) and the parts table: strip
            // lengths (8), then its blocks table's depth and count (2), then the entry.
            let parts = 2 + 28 * usize::from(f.top[13]);
            let at = 12 + parts + 8 + 2 + 24;
            let n = u32::from_le_bytes(f.top[at..at + 4].try_into().unwrap()) + 1;
            f.top[at..at + 4].copy_from_slice(&n.to_le_bytes());
        }),
    ));
    let e = page_entry(b, 0);
    out.push(case(
        "IX03-page-channel",
        Footer,
        "channel",
        "Index: a reader refuses a page whose channel differs from the entry that points at it",
        page_edited(b, e, |p| p[0..4].copy_from_slice(&1u32.to_le_bytes())),
    ));
    out.push(case(
        "IX03-page-table",
        Footer,
        "",
        "Index: a reader refuses a page whose table differs from the entry that points at it",
        page_edited(b, e, |p| p[4] = 2),
    ));
    out.push(case(
        "IX03-page-depth",
        Footer,
        "",
        "Index: a reader refuses a page whose depth differs from the entry that points at it",
        page_edited(b, e, |p| p[6] = 1),
    ));
    out.push(case(
        "IX03-page-first-place",
        Footer,
        "",
        "Index: a reader refuses a page whose first place differs from what the shape gives",
        page_edited(b, e, |p| p[7..15].copy_from_slice(&1u64.to_le_bytes())),
    ));
    out.push(case(
        "IX03-page-rows-out-of-order",
        Footer,
        "order",
        "Index: a reader refuses a page whose entries are out of order",
        page_edited(b, e, |p| {
            // Rows 1 and 2, so the directory entry still names row 0's start.
            let (a, c) = (PAGE_HEAD + 28, PAGE_HEAD + 56);
            let first = p[a..a + 28].to_vec();
            let second = p[c..c + 28].to_vec();
            p[a..a + 28].copy_from_slice(&second);
            p[c..c + 28].copy_from_slice(&first);
        }),
    ));
    let l = list_entry(b, 1);
    out.push(case(
        "IX03-list-gaps-out-of-order",
        Footer,
        "order",
        "Index: a reader refuses a page whose entries are out of order",
        page_edited(b, l, |p| {
            let (a, c) = (PAGE_HEAD + 17, PAGE_HEAD + 34);
            let first = p[a..a + 17].to_vec();
            let second = p[c..c + 17].to_vec();
            p[a..a + 17].copy_from_slice(&second);
            p[c..c + 17].copy_from_slice(&first);
        }),
    ));
    out.push(case(
        "IX03-list-gap-level-2",
        Footer,
        "level",
        "Index: a gap entry's `level` is 0 or 1",
        page_edited(b, l, |p| p[PAGE_HEAD + 16] = 2),
    ));
    out.push(case(
        "IX06-footer-past-256-mib",
        Footer,
        "",
        "Limits: the footer record is at most 256 MiB",
        trailer_edited(b, |t| {
            t[8..12].copy_from_slice(&(300u32 << 20).to_le_bytes())
        }),
    ));

    // Blocks.
    out.push(case(
        "BL02-frame-past-compress-bound",
        Random,
        "",
        "Limits: a reader refuses a row whose frame length passes ZSTD_COMPRESSBOUND before fetching the frame",
        common::rows_edited(b, 0, |rows| {
            rows[0].length = record::compress_bound(rows[0].decoded_length as usize) as u32 + 1
        }),
    ));
    out.push(case(
        "BL03-decoded-past-4-mib",
        Random,
        "",
        "Limits: a block decodes to at most 4 MiB",
        common::rows_edited(b, 0, |rows| rows[0].decoded_length = 5 << 20),
    ));
    out.push(case(
        "BL08-block-level-bit",
        InOrder,
        "level",
        "Blocks: a digital block's level bit is the one the edges and gap ends before it give",
        repayloaded(b, block_of(b, 0, 1), |p| {
            let w = u32::from_le_bytes(p[20..24].try_into().unwrap()) ^ (1 << 31);
            p[20..24].copy_from_slice(&w.to_le_bytes());
        }),
    ));
    out.push(case(
        "BL09-blocks-not-tiling",
        InOrder,
        "",
        "Blocks: a channel's blocks tile the axis: each starts at the end of the one before",
        repayloaded(b, block_of(b, 0, 1), |p| {
            let s = i64::from_le_bytes(p[4..12].try_into().unwrap()) + 1;
            p[4..12].copy_from_slice(&s.to_le_bytes());
        }),
    ));

    // Records in order.
    out.push(case(
        "RO05-gap-records-out-of-order",
        InOrder,
        "",
        "Gap and run records: a reader refuses a gap record out of order",
        {
            let (x, y) = (nth(b, Kind::Gap, 2), nth(b, Kind::Gap, 3));
            let px = b[x.payload()].to_vec();
            let py = b[y.payload()].to_vec();
            let f = spliced(b, x, &framed(x.kind, &py));
            let y = nth(&f, Kind::Gap, 3);
            spliced(&f, y, &framed(y.kind, &px))
        },
    ));
    out.push(case(
        "RO09-run-phase-past-grid",
        InOrder,
        "",
        "Gap and run records: a reader refuses a run record with a `phase` of `D` or more",
        runs_edited(b, 500, |p| p[28..32].copy_from_slice(&2u32.to_le_bytes())),
    ));
    out.push(case(
        "RO10-first-run-phase",
        InOrder,
        "",
        "Gap and run records: a reader refuses a run record at index 0 whose phase isn't the declared one",
        runs_edited(b, 0, |p| p[28..32].copy_from_slice(&0u32.to_le_bytes())),
    ));
    out.push(case(
        "RO11-run-before-sample",
        InOrder,
        "",
        "Gap and run records: a reader refuses a run record that starts where Gaps says a run may not",
        runs_edited(b, 500, |p| p[12..20].copy_from_slice(&4_000i64.to_le_bytes())),
    ));

    // Annotations.
    out.push(case(
        "AN04-parts-without-bundle",
        Footer,
        "bundle",
        "Annotations: a recording with annotation parts has a bundle record; a reader refuses one that doesn't",
        refooted(b, |f| f.top[0..12].fill(0)),
    ));

    // Header, more.
    out.push(case(
        "HD02-break-past-length",
        Footer,
        "",
        "Header: every tick a break names maps into 0..length",
        reheadered(b, |h| {
            data(h, "V")["breaks"][0][1] = json!(LENGTH + 5);
            data(h, "I")["breaks"][0][1] = json!(LENGTH + 5);
        }),
    ));
    let two_sources = |stretch: Value, timebase: Value| {
        reheadered(b, move |h| {
            let mut s = h["sources"][0].clone();
            s["timebase"] = timebase;
            if !stretch.is_null() {
                s["stretch"] = stretch;
            }
            h["sources"].as_array_mut().unwrap().push(s);
        })
    };
    out.push(case(
        "HD16-m-below-half",
        Footer,
        "",
        "Times: a reader refuses a source whose `m_s` is below 1/2",
        two_sources(Value::Null, json!([4_000_000, 1])),
    ));
    out.push(case(
        "HD17-m-terms-past-2-53",
        Footer,
        "",
        "Times: a reader refuses a source whose `a` or `b` reaches 2^53",
        two_sources(json!([(1u64 << 52) + 1, 1u64 << 52]), json!([3, 1])),
    ));
    out.push(case(
        "HD23-kept-contradicted",
        Random,
        "",
        "Moved and trimmed channels: a reader refuses a `kept` that its channel's data contradicts",
        reheadered(b, |h| {
            for n in ["A", "G", "V", "I"] {
                channel(h, n)["kept"] = json!([0, LENGTH]);
            }
            channel(h, "A")["kept"] = json!([500, LENGTH]);
        }),
    ));

    // Index and footer, more.
    out.push(case(
        "IX05-strip-decoded-length",
        Footer,
        "",
        "Footer: a reader refuses a strip whose decoded length is not that level's size",
        refooted(b, |f| {
            let parts = 2 + 28 * usize::from(f.top[13]);
            let at = 12 + parts + 4;
            let n = u32::from_le_bytes(f.top[at..at + 4].try_into().unwrap()) + 1;
            f.top[at..at + 4].copy_from_slice(&n.to_le_bytes());
        }),
    ));
    out.push(case(
        "IX06-header-past-64-mib",
        Footer,
        "",
        "Limits: the footer's header is at most 64 MiB (here its length passes the footer too, as a whole 64 MiB header is too large for a vector)",
        {
            let f = foot(b);
            let h = serde_json::to_vec(&f.header).unwrap();
            let mut rest = h.clone();
            rest.extend_from_slice(&f.top);
            rest.extend_from_slice(&f.strips);
            refooted_raw(b, 65 << 20, f.top.len() as u32, &rest)
        },
    ));
    let lv = level_entry(b, 2);
    let row_n = |p: &mut [u8], i: usize, edit: &dyn Fn(u32) -> u32| {
        let at = PAGE_HEAD + 20 * i + 16;
        let n = edit(u32::from_le_bytes(p[at..at + 4].try_into().unwrap()));
        p[at..at + 4].copy_from_slice(&n.to_le_bytes());
    };
    out.push(case(
        "IX01-overview-row-zero-n",
        Footer,
        "",
        "Index: a reader refuses an overview row with a zero `n`",
        page_edited(b, lv, |p| row_n(p, 0, &|_| 0)),
    ));
    out.push(case(
        "IX01-overview-rows-sum",
        Footer,
        "",
        "Index: a reader refuses overview rows whose `n` sum to another count",
        page_edited(b, lv, |p| row_n(p, 0, &|n| n + 1)),
    ));

    // Blocks, more.
    let a0 = block_of(b, 0, 0);
    out.push(case(
        "BL01-frame-without-checksum",
        InOrder,
        "checksum",
        "Blocks: a reader refuses a frame without a checksum",
        frame_replaced(b, a0, |f| {
            let raw = zstd::bulk::decompress(f, 1 << 22).unwrap();
            let mut c = zstd::bulk::Compressor::new(3).unwrap();
            c.include_checksum(false).unwrap();
            c.compress(&raw).unwrap()
        }),
    ));
    out.push(case(
        "BL01-frame-wrong-checksum",
        InOrder,
        "",
        "Blocks: a reader refuses a frame whose checksum does not match",
        frame_replaced(b, a0, |f| {
            let mut f = f.to_vec();
            let n = f.len();
            f[n - 1] ^= 1;
            f
        }),
    ));
    out.push(case(
        "BL01-frame-then-skippable",
        InOrder,
        "",
        "Blocks: a reader refuses a frame with anything else in its bytes",
        frame_replaced(b, a0, |f| {
            let mut f = f.to_vec();
            f.extend_from_slice(&[0x50, 0x2a, 0x4d, 0x18, 0, 0, 0, 0]);
            f
        }),
    ));
    out.push(case(
        "BL04-step-0",
        InOrder,
        "",
        "Blocks: a reader refuses a `step` of 0",
        lebs_edited(b, a0, |v| v[0] = 0),
    ));
    out.push(case(
        "BL05-edge-past-block",
        InOrder,
        "",
        "Blocks: a reader refuses an edge whose axis tick lies outside its block's span",
        lebs_edited(b, a0, |v| v[1] = 15_000),
    ));
    out.push(case(
        "BL06-bytes-after-last-edge",
        InOrder,
        "",
        "Blocks: a reader refuses bytes left after the last edge",
        lebs_edited(b, a0, |v| v.push(1)),
    ));
    out.push(case(
        "BL07-code-past-coding",
        InOrder,
        "",
        "Codings: a reader refuses a code outside its coding's range (12-bit signed: -2048..2048)",
        section_edited(b, block_of(b, 2, 0), |s| {
            let n = s.len() / 2;
            s[n] = 0x40;
        }),
    ));
    out.push(case(
        "BL10-index-moves-samples",
        Random,
        "",
        "Index: a reader refuses an index that places a sample outside its block",
        common::rows_edited(b, 2, |rows| {
            rows[0].word += 1;
            rows[1].word -= 1;
        }),
    ));

    // Records in order, more.
    let a_gap = of_channel(b, Kind::Gap, 0)[0];
    out.push(case(
        "RO06-gap-over-edges",
        InOrder,
        "",
        "Gap and run records: a reader refuses a gap record that an edge contradicts",
        repayloaded(b, a_gap, |p| {
            p[4..12].copy_from_slice(&500i64.to_le_bytes())
        }),
    ));
    out.push(case(
        "RO07-gap-after-its-block",
        InOrder,
        "",
        "Gap and run records: a reader refuses a gap record whose `to` maps before the end of the channel's last block record before it",
        {
            // A's last gap, so that moving it keeps the gaps in order.
            let last = of_channel(b, Kind::Gap, 0)[1];
            let holder = of_channel(b, Kind::Block, 0)
                .into_iter()
                .find(|r| {
                    let (s, e) = block_span(b, *r);
                    s <= 5_200 && 5_200 < e
                })
                .unwrap();
            moved_after(b, last, holder)
        },
    ));
    out.push(case(
        "RO08-runs-out-of-order",
        InOrder,
        "",
        "Gap and run records: a reader refuses a run record out of order",
        {
            let v = of_channel(b, Kind::Run, 2);
            let f = swapped(b, v[0], v[1]);
            let i = of_channel(&f, Kind::Run, 3);
            swapped(&f, i[0], i[1])
        },
    ));
    out.push(case(
        "RO13-run-record-oversized",
        InOrder,
        "cut off",
        "Limits: a run record's payload is 32 bytes exactly; a reader stops at a record whose `n` passes that before it reads the payload, so the file is cut off there",
        repayloaded(b, nth(b, Kind::Run, 0), |p| p.push(0)),
    ));
    out.push(case(
        "RO14-list-page-differs",
        InOrder,
        "",
        "In order: a page that differs by a byte from what the records before it give is refused (tickwave's reader checks; others may skip pages)",
        page_edited(b, l, |p| {
            let at = PAGE_HEAD + 17 * 5 + 8;
            let to = i64::from_le_bytes(p[at..at + 8].try_into().unwrap()) - 1;
            p[at..at + 8].copy_from_slice(&to.to_le_bytes());
        }),
    ));

    // Annotations, more.
    out.push(case(
        "AN01-generator-name",
        InOrder,
        "",
        "Annotations: a reader refuses a record whose `s` is not a name ANNOTATIONS.md allows a generator",
        annotation_record(b, 0, "g/n", r#"{"id":"mark/150","start":150}"#),
    ));
    out.push(case(
        "AN02-line-not-object",
        InOrder,
        "",
        "Annotations: a reader refuses a record whose `a` is not a JSON object",
        annotation_record(b, 0, "gen", "[150]"),
    ));
    out.push(case(
        "AN02-newline-inside",
        InOrder,
        "",
        "Annotations: a reader refuses a record whose `a` holds a newline before its end",
        annotation_record(b, 0, "gen", "{\"id\":\"mark/150\",\n\"start\":150}"),
    ));
    out.push(case(
        "AN02-key-twice",
        InOrder,
        "twice",
        "Annotations: a reader refuses a record whose `a` states a key twice",
        annotation_record(
            b,
            0,
            "gen",
            r#"{"id":"mark/150","start":150,"id":"mark/151"}"#,
        ),
    ));
    out.push(case(
        "AN05-record-without-start",
        InOrder,
        "can't hold",
        "Annotations: a reader refuses a record that the row of the part covering it couldn't state: one whose line has no `start`",
        annotation_record(b, 0, "gen", r#"{"id":"mark/150"}"#),
    ));
    out.push(case(
        "AN05-record-time-with-unit",
        InOrder,
        "whole tick",
        "Annotations: a record's times are axis ticks, JSON integers; a reader refuses a `start` or `end` that is not one",
        annotation_record(b, 0, "gen", r#"{"id":"mark/150","start":"150 us"}"#),
    ));
    out.push(case(
        "AN05-record-end-before-start",
        InOrder,
        "can't hold",
        "Annotations: a reader refuses a record that the row of the part covering it couldn't state: one with an `end` before its `start`",
        annotation_record(b, 0, "gen", r#"{"id":"mark/150","start":150,"end":149}"#),
    ));
    out.push(case(
        "AN06-record-no-part-covers",
        InOrder,
        "no part covers",
        "Annotations: a reader in order refuses a footer that comes after an annotation record no part covers",
        inserted(b, nth(b, Kind::Bundle, 0).at, &annotation("gen", r#"{"id":"mark/160","start":160}"#)),
    ));
    out.push(case(
        "AN07-bundled-generator-file-two-lines",
        Random,
        "more than its generator's first line",
        "Annotations: in the bundle, a generator file's text is its first line alone, with its newline",
        bundle_edited(b, |files| {
            let (_, text) = files.iter_mut().find(|(name, _)| name.ends_with(".jsonl")).unwrap();
            text.push_str("{\"id\":\"mark/160\",\"start\":160}\n");
        }),
    ));
    out.push(case(
        "AN07-bundle-names-a-file-twice",
        Random,
        "names file",
        "Annotations: the bundle's object names each file once; a reader refuses one naming a file twice",
        bundle_text_edited(b, |raw| {
            let files: BTreeMap<String, String> = serde_json::from_str(raw).unwrap();
            let (name, text) = files.iter().find(|(name, _)| name.ends_with(".jsonl")).unwrap();
            let pair = format!("{}:{}", json!(name), json!(text));
            format!("{{{pair},{}", &raw[1..])
        }),
    ));
    out.push(case(
        "AN07-bundled-generator-file-without-newline",
        Random,
        "lacks its newline",
        "Annotations: in the bundle, a generator file's text is its first line alone, with its newline",
        bundle_edited(b, |files| {
            let (_, text) = files.iter_mut().find(|(name, _)| name.ends_with(".jsonl")).unwrap();
            text.pop();
        }),
    ));
    out.push(case(
        "HD33-header-number-past-f64",
        Footer,
        "parse whole",
        "Header: every number in the header fits an f64",
        header_text_edited(b, |h| h.replacen('{', r#"{"zz":1e400,"#, 1)),
    ));
    out.push(case(
        "HD32-header-key-twice",
        Footer,
        "twice",
        "Header: no object in the header names a key twice, at any depth",
        header_text_edited(b, |h| {
            h.replacen(r#""name":"A""#, r#""name":"A","name":"A""#, 1)
        }),
    ));
    out.push(case(
        "DC02-declaration-key-twice",
        InOrder,
        "twice",
        "Header: no object in the declaration names a key twice, at any depth",
        repayloaded(b, nth(b, Kind::Declaration, 0), |p| {
            let text = String::from_utf8(p.clone()).unwrap();
            assert!(text.contains(r#""name":"A""#));
            *p = text
                .replacen(r#""name":"A""#, r#""name":"A","name":"A""#, 1)
                .into_bytes();
        }),
    ));

    // Where records go, and which channel they name.
    out.push(case(
        "FR11-second-declaration",
        InOrder,
        "",
        "Records: the declaration is the first record, and only the first",
        {
            let d = nth(b, Kind::Declaration, 0);
            inserted(b, nth(b, Kind::Block, 0).at, &b[d.at..d.at + d.len])
        },
    ));
    out.push(case(
        "FR12-footer-not-last",
        InOrder,
        "",
        "Records: the footer is the last record, and the trailer follows it",
        {
            let f = foot(b).rec;
            inserted(b, nth(b, Kind::Block, 0).at, &b[f.at..f.at + f.len])
        },
    ));
    out.push(case(
        "DC01-footer-header-differs",
        InOrder,
        "",
        "Declaration: the footer's header holds the same values as the declaration",
        reheadered(b, |h| h["title"] = json!("another")),
    ));
    out.push(case(
        "RO15-gap-on-analog",
        InOrder,
        "",
        "Gap and run records: a gap record names a digital channel",
        repayloaded(b, a_gap, |p| p[0..4].copy_from_slice(&2u32.to_le_bytes())),
    ));
    out.push(case(
        "RO16-run-on-digital",
        InOrder,
        "",
        "Gap and run records: a run record names an analog channel",
        repayloaded(b, nth(b, Kind::Run, 0), |p| {
            p[0..4].copy_from_slice(&0u32.to_le_bytes())
        }),
    ));
    out.push(case(
        "RO17-block-past-last-channel",
        InOrder,
        "",
        "Blocks: a block record names one of the recording's channels",
        repayloaded(b, a0, |p| p[0..4].copy_from_slice(&9u32.to_le_bytes())),
    ));
    out.push(case(
        "IX03-page-shift",
        Footer,
        "",
        "Index: a reader refuses a page whose shift differs from the entry that points at it",
        page_edited(b, level_entry(b, 2), |p| p[5] += 4),
    ));

    out
}

/// The cases on the live writer's file.
fn live_cases(l: &[u8]) -> Vec<Case> {
    use Mode::*;
    vec![
        case(
            "RO01-known-through-before-block-end",
            InOrder,
            "",
            "Known-through records: a reader refuses a known-through record that a block record before it ends after",
            known_through_edited(l, 0, 50),
        ),
        case(
            "RO02-block-item-before-known-through",
            InOrder,
            "",
            "Known-through records: a reader refuses a block record after a known-through record that holds an item before its tick",
            known_through_edited(l, 0, 160),
        ),
        case(
            "RO03-gap-ends-before-known-through",
            InOrder,
            "",
            "Known-through records: a reader refuses a gap record after a known-through record whose `to` maps before its tick",
            repayloaded(l, nth(l, Kind::Gap, 0), |p| {
                p[12..20].copy_from_slice(&190i64.to_le_bytes())
            }),
        ),
        case(
            "RO04-known-through-not-increasing",
            InOrder,
            "",
            "Known-through records: each one's tick is later than the one before it",
            known_through_edited(l, 1, 100),
        ),
        case(
            "RO04-known-through-past-length",
            InOrder,
            "",
            "Known-through records: a known-through tick is no later than the length",
            {
                let last = records(l)
                    .into_iter()
                    .filter(|r| r.kind == Kind::KnownThrough as u8)
                    .count()
                    - 1;
                known_through_edited(l, last, 600)
            },
        ),
    ]
}

// --- What a later version may write, which this one reads -----------------------------------------

/// The live recording's D, its edges and no gap: a channel of a kind this version doesn't know
/// holds no gap or run record.
fn plain_live() -> Vec<u8> {
    use escapement_tickwave_format::writer::Finish;
    let mut w = live_writer();
    for t in [10, 20, 30] {
        w.edge(0, t).unwrap();
    }
    w.flush(100).unwrap();
    w.edge(0, 150).unwrap();
    w.flush(200).unwrap();
    let mut file = w.take_output();
    file.extend(w.finish(500, &[500], Finish::default()).unwrap().tail);
    file
}

/// The live recording's D beside a second channel E, both kept from tick 0 to 500, as a merged or
/// cut recording's channels are.
fn kept_live() -> Vec<u8> {
    use escapement_tickwave_format::header::Kept;
    use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
    let mut d = live_declaration();
    let mut e = d.channels[0].clone();
    e.name = "E".into();
    e.source_name = "e".into();
    d.channels.push(e);
    for ch in &mut d.channels {
        ch.shaping.kept = Some(Kept { from: 0, to: 500 });
    }
    let options = WriteOptions {
        level: 3,
        ..WriteOptions::default()
    };
    let mut w = Writer::new(d, options).unwrap();
    for t in [10, 20, 30] {
        w.edge(0, t).unwrap();
        w.edge(1, t + 5).unwrap();
    }
    w.flush(100).unwrap();
    let mut file = w.take_output();
    file.extend(w.finish(500, &[500, 500], Finish::default()).unwrap().tail);
    file
}

/// A file a later minor version may write, which every reader of this one takes: the rule that
/// says so, and the channels it reads as unavailable.
struct Later {
    id: &'static str,
    rule: &'static str,
    file: Vec<u8>,
    unavailable: Vec<usize>,
}

fn later_files(p: &[u8]) -> Vec<Later> {
    let footer_at = foot(p).rec.at;
    let events = |c: &mut Value| {
        let c = c.as_object_mut().unwrap();
        c.insert("kind".into(), "events".into());
        for key in ["initial", "end", "edges"] {
            c.remove(key);
        }
        c.insert("frames".into(), json!([{"at": 12}]));
    };
    vec![
        Later {
            id: "LV01-later-minor-version",
            rule: "Preamble: a reader takes a later minor version of its major version, reading what its own version defines",
            file: {
                let mut f = p.to_vec();
                f[10..12].copy_from_slice(&1u16.to_le_bytes());
                trailer_edited(&f, |t| t[14..16].copy_from_slice(&1u16.to_le_bytes()))
            },
            unavailable: vec![],
        },
        Later {
            id: "LV02-auxiliary-record-and-object",
            rule: "Records: a reader skips an auxiliary record, of a kind from 192; Header: it ignores the keys of `auxiliary`",
            file: reheadered(&inserted(p, footer_at, &framed(200, b"cache")), |h| {
                h["auxiliary"] = json!({"cache": {"at": 12}});
            }),
            unavailable: vec![],
        },
        Later {
            id: "LV03-optional-record",
            rule: "Records: a reader skips an optional record, of a kind from 128 to 191",
            file: inserted(p, footer_at, &framed(130, b"later")),
            unavailable: vec![],
        },
        Later {
            id: "LV05-unknown-kind-among-kept-spans",
            rule: "Unavailable channels: a channel of a `kind` this version doesn't know states its `offset`, `offset_kind` and `kept` as every channel does, so the kept rules hold with it among the channels",
            file: {
                let k = kept_live();
                reheadered(&redeclared(&k, |d| events(channel(d, "E"))), |h| {
                    events(channel(h, "E"))
                })
            },
            unavailable: vec![1],
        },
        Later {
            id: "LV04-unknown-channel-kind",
            rule: "Unavailable channels: a channel of a `kind` this version doesn't know reads as unavailable, its entry read for what every kind has",
            file: reheadered(&redeclared(p, |d| events(channel(d, "D"))), |h| {
                events(channel(h, "D"))
            }),
            unavailable: vec![0],
        },
    ]
}

fn left_out_cases(g: &[u8]) -> Vec<Case> {
    vec![case(
        "RO18-gap-left-out-inside-length",
        Mode::InOrder,
        "inside the length",
        "Gap and run records: a reader refuses a gap record whose `to` is at or past its channel's `end` and maps before the length",
        repayloaded(g, nth(g, Kind::Gap, 0), |p| {
            p[12..20].copy_from_slice(&450i64.to_le_bytes())
        }),
    )]
}

fn all_cases(b: &[u8]) -> Vec<Case> {
    let mut out = cases(b);
    out.extend(live_cases(&live_base()));
    out.extend(left_out_cases(&left_out_base()));
    out
}

#[test]
fn the_bases_are_taken_every_way() {
    let b = base();
    for mode in [Mode::Footer, Mode::Random, Mode::InOrder, Mode::Verify] {
        assert_eq!(refusal(mode, &b), None, "{}", mode.name());
    }
    for (name, file) in [("live", live_base()), ("left-out", left_out_base())] {
        for mode in [Mode::Footer, Mode::Random, Mode::InOrder] {
            assert_eq!(refusal(mode, &file), None, "{name}, {}", mode.name());
        }
    }
}

#[test]
fn what_a_later_version_may_write_is_taken_every_way() {
    let p = plain_live();
    for mode in [Mode::Footer, Mode::Random, Mode::InOrder] {
        assert_eq!(refusal(mode, &p), None, "plain live, {}", mode.name());
    }
    for l in later_files(&p) {
        for mode in [Mode::Footer, Mode::Random, Mode::InOrder] {
            assert_eq!(refusal(mode, &l.file), None, "{}, {}", l.id, mode.name());
        }
        let footer = Footer::parse_file(&l.file).unwrap();
        let unavailable: Vec<usize> = (0..footer.channels.len())
            .filter(|&o| !footer.available(o))
            .collect();
        assert_eq!(unavailable, l.unavailable, "{}", l.id);
    }
}

#[test]
fn each_rule_is_refused_where_the_spec_puts_it() {
    let b = base();
    let mut failed = Vec::new();
    let mut report = String::new();
    for c in all_cases(&b) {
        match refusal(c.mode, &c.file) {
            Some(why) if why.contains(c.because) => {
                report.push_str(&format!("ok   {:42} {:8} {why}\n", c.id, c.mode.name()));
            }
            Some(why) => {
                report.push_str(&format!("WHY  {:42} {:8} {why}\n", c.id, c.mode.name()));
                failed.push(c.id);
            }
            None => {
                report.push_str(&format!("TOOK {:42} {:8}\n", c.id, c.mode.name()));
                failed.push(c.id);
            }
        }
    }
    eprintln!("{report}");
    assert!(
        failed.is_empty(),
        "not refused as the rule says: {failed:?}"
    );
}

/// Writes the bases, the cases kept as vectors and their listing into the empty directory
/// `TICKWAVE_INVALID_VECTORS` names.
#[test]
#[ignore = "writes files, run by hand"]
fn write_the_invalid_vectors() {
    let dir = PathBuf::from(
        std::env::var_os("TICKWAVE_INVALID_VECTORS")
            .expect("TICKWAVE_INVALID_VECTORS names the directory to write the vectors into"),
    );
    std::fs::create_dir_all(&dir).unwrap();
    assert!(
        std::fs::read_dir(&dir).unwrap().next().is_none(),
        "{} isn't empty",
        dir.display()
    );
    let b = base();
    std::fs::write(dir.join("base.tickwave"), &b).unwrap();
    std::fs::write(dir.join("live.tickwave"), live_base()).unwrap();
    std::fs::write(dir.join("left-out.tickwave"), left_out_base()).unwrap();
    let p = plain_live();
    std::fs::write(dir.join("plain-live.tickwave"), &p).unwrap();
    let mut taken = Vec::new();
    for l in later_files(&p) {
        std::fs::write(dir.join(format!("{}.tickwave", l.id)), &l.file).unwrap();
        taken.push(json!({"file": format!("{}.tickwave", l.id), "rule": l.rule, "unavailable": l.unavailable}));
    }
    let mut listing = Vec::new();
    for c in all_cases(&b).iter().filter(|c| c.vector) {
        std::fs::write(dir.join(format!("{}.tickwave", c.id)), &c.file).unwrap();
        listing.push(
            json!({"file": format!("{}.tickwave", c.id), "mode": c.mode.name(), "rule": c.rule}),
        );
    }
    let text = serde_json::to_string_pretty(&json!({
        "bases": ["base.tickwave", "live.tickwave", "left-out.tickwave", "plain-live.tickwave"],
        "cases": listing,
        "taken": taken,
    }))
    .unwrap();
    std::fs::write(dir.join("invalid.json"), text + "\n").unwrap();
}

/// Each case read every other way as well, for a look at what else in it a reader meets.
#[test]
#[ignore = "a report, run by hand"]
fn every_case_every_way() {
    let b = base();
    for c in all_cases(&b) {
        let row: Vec<String> = [Mode::Footer, Mode::Random, Mode::InOrder]
            .into_iter()
            .map(|m| match refusal(m, &c.file) {
                None => format!("{}=ok", m.name()),
                Some(why) => format!(
                    "{}=R({})",
                    m.name(),
                    why.chars().take(70).collect::<String>()
                ),
            })
            .collect();
        eprintln!("{:42} {:8} {}", c.id, c.mode.name(), row.join(" | "));
    }
}
