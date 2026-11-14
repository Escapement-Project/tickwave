//! What the fuzz targets share. A recording's records carry checks and its frames checksums, which
//! a mutation of the file's bytes breaks before it reaches anything behind them. So a target that
//! reads recordings takes a structured input instead, and `file` builds the file from it with its
//! framing, checks and frames made right: the fuzzer's changes reach the reader's own rules.
//! `structure` turns a fixture into such an input, for the seeds.
//!
//! A structured input is a flags byte, then records, each `[kind u8][mode u8][len u32][body]`.
//! The body is the record's payload, except that a block, an overview block and the footer give
//! their frames decoded, and `file` compresses them when the mode says so:
//!
//! - block: `[head_len u16][head][decoded]`
//! - overview block: `[9-byte head][decoded]`
//! - footer: `[header_len u32][header][index_len u32][index][decoded overview]`

use std::io::Read;

use escapement_tickwave_format::header::{MAJOR, MINOR, PREAMBLE_LEN, Preamble, Trailer};
use escapement_tickwave_format::record::{self, Kind, Next};

/// Flags: a trailer after the last footer record, made for it.
pub const AUTO_TRAILER: u8 = 1;
/// Flags: the footer's index rows rewritten to point at the frames written.
pub const FIX_INDEX: u8 = 2;
/// Flags: no preamble before the records.
pub const NO_PREAMBLE: u8 = 4;

/// Mode: the frame is compressed from the decoded bytes the body gives.
pub const COMPRESS: u8 = 1;
/// Mode: the record's check is spoilt.
pub const BAD_CHECK: u8 = 2;
/// Mode: the body is written as it is, with no framing.
pub const RAW: u8 = 4;

/// One zstd frame with a content checksum, at a fast level.
pub fn compress(raw: &[u8]) -> Vec<u8> {
    let mut c = zstd::bulk::Compressor::new(1).expect("a compressor");
    c.set_parameter(zstd::stream::raw::CParameter::ChecksumFlag(true))
        .expect("the checksum flag");
    c.compress(raw).expect("zstd compresses any bytes")
}

struct Frame {
    /// The block's head, or the overview block's shift and `k`.
    head: Vec<u8>,
    offset: u64,
    length: u32,
    decoded: u32,
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The footer's index with its rows pointed at the frames written: a block row at the block in
/// its place, an overview row at the overview blocks in the footer's order, by shift then `k`.
fn fix_index(header: &[u8], index: &mut [u8], blocks: &[Frame], overview: &mut [Frame]) {
    let Ok(h) = serde_json::from_slice::<serde_json::Value>(header) else {
        return;
    };
    let channels = h["channels"].as_array().map_or(0, Vec::len);
    let row = 24 + 4 * channels;
    let n = usize::try_from(h["blocks"].as_u64().unwrap_or(0)).unwrap_or(usize::MAX);
    for (i, b) in blocks.iter().enumerate().take(n) {
        let Some(r) = index.get_mut(i * row..(i + 1) * row) else {
            return;
        };
        if b.head.len() < 20 + 4 * channels {
            continue;
        }
        r[..8].copy_from_slice(&b.head[..8]);
        r[8..16].copy_from_slice(&b.offset.to_le_bytes());
        r[16..20].copy_from_slice(&b.length.to_le_bytes());
        r[20..24].copy_from_slice(&b.head[16..20]);
        r[24..].copy_from_slice(&b.head[20..20 + 4 * channels]);
    }
    overview.sort_by_key(|o| {
        (
            o.head[0],
            u64::from_le_bytes(o.head[1..9].try_into().unwrap()),
        )
    });
    let base = n.saturating_mul(row);
    for (j, o) in overview.iter().enumerate() {
        let Some(r) = index.get_mut(base.saturating_add(16 * j)..base.saturating_add(16 * (j + 1)))
        else {
            return;
        };
        r[..8].copy_from_slice(&o.offset.to_le_bytes());
        r[8..12].copy_from_slice(&o.length.to_le_bytes());
        r[12..16].copy_from_slice(&o.decoded.to_le_bytes());
    }
}

/// The recording a structured input describes.
pub fn file(data: &[u8]) -> Vec<u8> {
    let Some((&flags, mut rest)) = data.split_first() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if flags & NO_PREAMBLE == 0 {
        out.extend_from_slice(&Preamble::CURRENT.to_bytes());
    }
    let mut blocks = Vec::new();
    let mut overview = Vec::new();
    let mut footer: Option<(u64, u32)> = None;
    while rest.len() >= 6 {
        let (kind, mode) = (rest[0], rest[1]);
        let len = (u32_at(rest, 2).unwrap() as usize).min(rest.len() - 6);
        let body = &rest[6..6 + len];
        rest = &rest[6 + len..];
        if mode & RAW != 0 {
            out.extend_from_slice(body);
            continue;
        }
        let at = out.len() as u64;
        let compressed = mode & COMPRESS != 0;
        let mut payload = Vec::with_capacity(body.len());
        match kind {
            2 if compressed && body.len() >= 2 => {
                let head_len =
                    (u16::from_le_bytes([body[0], body[1]]) as usize).min(body.len() - 2);
                let (head, raw) = body[2..].split_at(head_len);
                let frame = compress(raw);
                payload.extend_from_slice(head);
                payload.extend_from_slice(&frame);
                blocks.push(Frame {
                    head: head.to_vec(),
                    offset: at + (record::PAYLOAD_AT + head_len) as u64,
                    length: frame.len() as u32,
                    decoded: raw.len() as u32,
                });
            }
            5 if compressed && body.len() >= 9 => {
                let (head, raw) = body.split_at(9);
                let frame = compress(raw);
                payload.extend_from_slice(head);
                payload.extend_from_slice(&frame);
                overview.push(Frame {
                    head: head.to_vec(),
                    offset: at + (record::PAYLOAD_AT + 9) as u64,
                    length: frame.len() as u32,
                    decoded: raw.len() as u32,
                });
            }
            7 if compressed => {
                let header_len = u32_at(body, 0).unwrap_or(0) as usize;
                let header = body
                    .get(4..)
                    .map_or(&[][..], |b| &b[..header_len.min(b.len())]);
                let after = (4 + header.len()).min(body.len());
                let index_len = u32_at(body, after).unwrap_or(0) as usize;
                let index_at = (after + 4).min(body.len());
                let mut index = body[index_at..][..index_len.min(body.len() - index_at)].to_vec();
                let raw = &body[index_at + index.len()..];
                if flags & FIX_INDEX != 0 {
                    fix_index(header, &mut index, &blocks, &mut overview);
                }
                payload.extend_from_slice(&(header.len() as u32).to_le_bytes());
                payload.extend_from_slice(&(index.len() as u32).to_le_bytes());
                payload.extend_from_slice(header);
                payload.extend_from_slice(&index);
                payload.extend_from_slice(&compress(raw));
            }
            _ => payload.extend_from_slice(body),
        }
        let start = out.len();
        frame(&mut out, kind, &payload);
        if mode & BAD_CHECK != 0
            && let Some(last) = out.last_mut()
        {
            *last ^= 1;
        }
        if kind == Kind::Footer as u8 {
            footer = Some((start as u64, (out.len() - start) as u32));
        }
    }
    if flags & AUTO_TRAILER != 0
        && let Some((footer_offset, footer_len)) = footer
    {
        out.extend_from_slice(
            &Trailer {
                footer_offset,
                footer_len,
                major: MAJOR,
                minor: MINOR,
            }
            .to_bytes(),
        );
    }
    out.extend_from_slice(rest);
    out
}

/// A record of any kind byte: its kind, payload length, payload and check (FORMAT.md, "Records").
fn frame(out: &mut Vec<u8>, kind: u8, payload: &[u8]) {
    let len = (payload.len() as u32).to_le_bytes();
    let mut h = blake3::Hasher::new();
    h.update(&[kind]);
    h.update(&len);
    h.update(payload);
    out.push(kind);
    out.extend_from_slice(&len);
    out.extend_from_slice(payload);
    out.extend_from_slice(&h.finalize().as_bytes()[..8]);
}

fn decode_all(frame: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    zstd::stream::Decoder::new(frame)
        .ok()?
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}

fn put(out: &mut Vec<u8>, kind: u8, mode: u8, body: &[u8]) {
    out.push(kind);
    out.push(mode);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
}

/// A footer's payload as a structured footer's body, its overview decoded.
fn footer_body(payload: &[u8]) -> Option<Vec<u8>> {
    let header_len = u32_at(payload, 0)? as usize;
    let index_len = u32_at(payload, 4)? as usize;
    let header = payload.get(8..8usize.checked_add(header_len)?)?;
    let index = payload.get(8 + header_len..(8 + header_len).checked_add(index_len)?)?;
    let frame = &payload[8 + header_len + index_len..];
    let mut body = (header_len as u32).to_le_bytes().to_vec();
    body.extend_from_slice(header);
    body.extend_from_slice(&(index_len as u32).to_le_bytes());
    body.extend_from_slice(index);
    body.extend_from_slice(&decode_all(frame)?);
    Some(body)
}

/// A recording as a structured input that `file` builds into an equivalent file: its frames
/// decoded, its checks, index offsets and trailer left for `file` to make.
pub fn structure(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < PREAMBLE_LEN || Preamble::parse(bytes).is_none() {
        return None;
    }
    let mut out = vec![AUTO_TRAILER | FIX_INDEX];
    let mut pos = PREAMBLE_LEN;
    let mut channels = 0;
    let mut finished = false;
    while let Next::Record { kind, payload, len } = record::next(&bytes[pos..], usize::MAX) {
        pos += len;
        match Kind::from_byte(kind) {
            Some(Kind::Declaration) => {
                let d: Option<serde_json::Value> = serde_json::from_slice(payload).ok();
                channels = d
                    .as_ref()
                    .and_then(|d| d["channels"].as_array())
                    .map_or(0, Vec::len);
                put(&mut out, kind, 0, payload);
            }
            Some(Kind::Block) => {
                let head_len = 20 + 4 * channels;
                let (head, frame) = payload.split_at(head_len.min(payload.len()));
                match decode_all(frame) {
                    Some(raw) => {
                        let mut body = (head.len() as u16).to_le_bytes().to_vec();
                        body.extend_from_slice(head);
                        body.extend_from_slice(&raw);
                        put(&mut out, kind, COMPRESS, &body);
                    }
                    None => put(&mut out, kind, 0, payload),
                }
            }
            Some(Kind::OverviewBlock) => {
                let (head, frame) = payload.split_at(9.min(payload.len()));
                match decode_all(frame) {
                    Some(raw) => put(&mut out, kind, COMPRESS, &[head, &raw].concat()),
                    None => put(&mut out, kind, 0, payload),
                }
            }
            Some(Kind::Footer) => {
                match footer_body(payload) {
                    Some(body) => put(&mut out, kind, COMPRESS, &body),
                    None => put(&mut out, kind, 0, payload),
                }
                finished = true;
                // The trailer follows; `file` makes it.
                pos = bytes
                    .len()
                    .min(pos + escapement_tickwave_format::header::TRAILER_LEN);
                break;
            }
            _ => put(&mut out, kind, 0, payload),
        }
    }
    if !finished || pos < bytes.len() {
        put(&mut out, 0, RAW, &bytes[pos..]);
    }
    Some(out)
}

/// Chunk lengths from `seed`, each 1 to 4096 bytes, to feed a reader in pieces.
pub fn chunks(data: &[u8], seed: u64) -> impl Iterator<Item = &[u8]> {
    let mut state = seed | 1;
    let mut rest = data;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let n = (state % 4096 + 1) as usize;
        let (a, b) = rest.split_at(n.min(rest.len()));
        rest = b;
        Some(a)
    })
}

/// Reads every item a recording's iterators give: what a conversion's encoder would take.
pub fn drain(recording: escapement_tickwave_format::model::Recording) {
    use escapement_tickwave_format::model::{ChannelData, Samples};
    for c in recording.channels {
        match c.data {
            ChannelData::Digital(d) => {
                d.edges.for_each(drop);
            }
            ChannelData::Analog(a) => match a.samples {
                Samples::F32(s) => s.for_each(drop),
                Samples::F64(s) => s.for_each(drop),
                Samples::U64(_, s) => s.for_each(drop),
                Samples::I64(_, s) => s.for_each(drop),
            },
        }
    }
}

/// A sigrok target's input as a session file's bytes: after a 0, the bytes as they are; after a
/// 1, a zip of members, each `[name_len u8][name][flags u8][len u32][content]`, with CRCs the zip
/// crate makes right. Flag 1 deflates the member. Flags 2 and 4 are followed by a u32 that the
/// member's local header and directory entry then state as its size, and its compressed size,
/// whatever it holds.
pub fn session(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    match data.split_first() {
        Some((1, rest)) => {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            let mut parts = Parts(rest);
            let n = parts.byte().unwrap_or(0);
            let mut lies = Vec::new();
            for _ in 0..n {
                let (Some(name), Some(flags), Some(content)) =
                    (parts.short(), parts.byte(), parts.long())
                else {
                    break;
                };
                if flags & 6 != 0 {
                    let Some(stated) = parts.u32() else {
                        break;
                    };
                    lies.push((name, flags, stated));
                }
                let method = if flags & 1 == 1 {
                    zip::CompressionMethod::Deflated
                } else {
                    zip::CompressionMethod::Stored
                };
                let options = zip::write::SimpleFileOptions::default().compression_method(method);
                let name = String::from_utf8_lossy(name);
                if zip.start_file(name, options).is_err() || zip.write_all(content).is_err() {
                    break;
                }
            }
            let mut out = zip.finish().map(|c| c.into_inner()).unwrap_or_default();
            for (name, flags, stated) in lies {
                state_sizes(&mut out, name, flags, stated);
            }
            out
        }
        Some((_, rest)) => rest.to_vec(),
        None => Vec::new(),
    }
}

/// Writes `stated` over member `name`'s size (flag 2) and compressed size (flag 4), in its local
/// header and its directory entry. A zip the zip crate wrote: no zip64 fields, every offset in
/// place; one that isn't is left as it is.
fn state_sizes(zip: &mut [u8], name: &[u8], flags: u8, stated: u32) {
    let u16_at = |z: &[u8], at: usize| {
        Some(u16::from_le_bytes(z.get(at..at + 2)?.try_into().ok()?) as usize)
    };
    let u32_at = |z: &[u8], at: usize| {
        Some(u32::from_le_bytes(z.get(at..at + 4)?.try_into().ok()?) as usize)
    };
    let Some(end) = (0..zip.len().saturating_sub(21))
        .rev()
        .find(|&i| zip[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
    else {
        return;
    };
    let (Some(entries), Some(mut at)) = (u16_at(zip, end + 10), u32_at(zip, end + 16)) else {
        return;
    };
    let put = |z: &mut [u8], at: usize| {
        if let Some(field) = z.get_mut(at..at + 4) {
            field.copy_from_slice(&stated.to_le_bytes());
        }
    };
    for _ in 0..entries {
        let (Some(n), Some(extra), Some(comment), Some(local)) = (
            u16_at(zip, at + 28),
            u16_at(zip, at + 30),
            u16_at(zip, at + 32),
            u32_at(zip, at + 42),
        ) else {
            return;
        };
        if zip.get(at + 46..at + 46 + n) == Some(name) {
            if flags & 2 != 0 {
                put(zip, at + 24);
                put(zip, local + 22);
            }
            if flags & 4 != 0 {
                put(zip, at + 20);
                put(zip, local + 18);
            }
        }
        at += 46 + n + extra + comment;
    }
}

/// Length-prefixed parts of an input.
pub struct Parts<'a>(pub &'a [u8]);

impl<'a> Parts<'a> {
    pub fn byte(&mut self) -> Option<u8> {
        let (&b, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(b)
    }

    fn take(&mut self, n: usize) -> &'a [u8] {
        let (a, b) = self.0.split_at(n.min(self.0.len()));
        self.0 = b;
        a
    }

    pub fn u32(&mut self) -> Option<u32> {
        let n = u32::from_le_bytes(self.0.get(..4)?.try_into().ok()?);
        self.0 = &self.0[4..];
        Some(n)
    }

    /// A part after a one-byte length.
    pub fn short(&mut self) -> Option<&'a [u8]> {
        let n = self.byte()? as usize;
        Some(self.take(n))
    }

    /// A part after a four-byte length.
    pub fn long(&mut self) -> Option<&'a [u8]> {
        let n = u32::from_le_bytes(self.0.get(..4)?.try_into().ok()?) as usize;
        self.0 = &self.0[4..];
        Some(self.take(n))
    }
}

/// A Saleae export's files from `[n u8]`, then each `[name_len u8][name][len u32][bytes]`, and the
/// channels a reader takes: each `.bin` file's stem, named as itself.
pub fn export(
    data: &[u8],
) -> (
    Vec<escapement_tickwave_import::saleae::ExportFile>,
    Vec<(String, String)>,
) {
    let mut parts = Parts(data);
    let mut files = Vec::new();
    let mut channels = Vec::new();
    for _ in 0..parts.byte().unwrap_or(0) {
        let (Some(name), Some(bytes)) = (parts.short(), parts.long()) else {
            break;
        };
        let name = String::from_utf8_lossy(name).into_owned();
        if let Some(stem) = name.strip_suffix(".bin")
            && !channels.iter().any(|(s, _): &(String, String)| s == stem)
        {
            channels.push((stem.to_owned(), stem.to_owned()));
        }
        let input: Arc<[u8]> = bytes.into();
        files.push((
            std::path::PathBuf::from(name),
            Arc::new(input) as Arc<dyn escapement_tickwave_import::input::Input>,
        ));
    }
    (files, channels)
}

/// A rate byte as a Saleae reader's given rate: none, or one of a few the captures use.
pub fn rate(b: u8) -> Option<escapement_tickwave_import::saleae::SampleRate> {
    let hz = [
        0,
        1_000_000,
        6_250_000,
        25_000_000,
        100_000_000,
        500_000_000,
        1_000_000_000,
        3,
    ];
    escapement_tickwave_import::saleae::SampleRate::new(hz[usize::from(b % 8)])
}

use std::sync::Arc;
