//! What the fuzz targets share. A recording's records carry checks and its frames checksums, which
//! a mutation of the file's bytes breaks before it reaches anything behind them. So a target that
//! reads recordings takes a structured input instead, and `file` builds the file from it with its
//! framing, checks and frames made right: the fuzzer's changes reach the reader's own rules.
//! `structure` turns a fixture into such an input, for the seeds.
//!
//! A structured input is a flags byte, then records, each `[kind u8][mode u8][len u32][body]`.
//! The body is the record's payload, except that with `COMPRESS` a block, an overview block, the
//! footer and the annotation bundle give their frames decoded, and `file` compresses them:
//!
//! - block: `[28-byte head][decoded]`. `file` sets the head's decoded length to the decoded
//!   bytes'.
//! - overview block: `[17-byte head][decoded]`.
//! - footer: `[header_len u32][header][index_len u32][top index][decoded strips]`. The strips lie
//!   back to back: `file` cuts them at the decoded lengths the top index gives each channel,
//!   compresses each, and writes each frame's length there. Bytes past the last channel's strip
//!   go in one more frame.
//! - annotation bundle and annotation part: `[decoded]`. `file` writes its length before the
//!   frame.
//!
//! With `FIX_INDEX`, `file` points the index at the records it wrote, counting each kind in the
//! order it wrote them. Row `i` of a channel's blocks, in an index page, takes the start, frame,
//! decoded length and word of the channel's `i`th block record. Row `i` of an overview level
//! takes the frame, decoded length and `n` of the level's `i`th overview block record. Row `i` of
//! the annotation parts table takes the frame and decoded length of the `i`th part record; its
//! span stays the input's. Entry `i` of a directory page at place `p` points at page `p + i` of
//! its table at the depth below, and entry `i` of a table in the top index at page `i` of the
//! table at the depth it states. The top index's bundle place points at the latest bundle
//! record. Every other field stays as the input gives it.
//!
//! The flags `REQUIRES`, `CODEC` and `OPERATION` make a declaration's and a footer's header name
//! what this version doesn't know (FORMAT.md, "Unavailable channels"): a feature channel 0
//! requires, a codec the last channel states, and the first derived channel's operation. With
//! `LIMITS`, `file` pads a compressed body's decoded bytes with zeros to one byte short of its
//! kind's limit, the limit, or one past it, as the body's first byte picks: a block's and an
//! annotation part's 4 MiB, a bundle's 64 MiB.

use std::collections::HashMap;
use std::io::Read;

use escapement_tickwave_format::header::{
    BlockRow, ChannelKind, Header, MAJOR, MINOR, OverviewRow, PREAMBLE_LEN, Preamble, TRAILER_LEN,
    Trailer,
};
use escapement_tickwave_format::index::{Entry, PAGE_HEAD, PageHead, Table};
use escapement_tickwave_format::record::{self, BlockHead, Kind, Next, OverviewHead};

/// Flags: a trailer after the last footer record, made for it.
pub const AUTO_TRAILER: u8 = 1;
/// Flags: the index pointed at the records written.
pub const FIX_INDEX: u8 = 2;
/// Flags: no preamble before the records.
pub const NO_PREAMBLE: u8 = 4;
/// Flags: channel 0 requires a feature this version doesn't know.
pub const REQUIRES: u8 = 8;
/// Flags: the last channel states a codec this version doesn't know.
pub const CODEC: u8 = 16;
/// Flags: the first derived channel's operation is one this version doesn't know.
pub const OPERATION: u8 = 32;
/// Flags: compressed bodies padded to their kinds' limits.
pub const LIMITS: u8 = 64;

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

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// A frame `file` wrote: its offset and length, and what it decodes to where `file` compressed an
/// overview block's.
#[derive(Clone, Copy)]
struct Placed {
    offset: u64,
    length: u32,
    decoded: Option<u32>,
}

/// An overview level: its channel and shift.
type Level = (u32, u8);

/// A table's pages at one depth: the channel, the table's code and shift, and the depth.
type Pages = (u32, u8, u8, u8);

/// What `file` has written that the index points at, each kind in the order it wrote them.
#[derive(Default)]
struct Written {
    /// Each channel's block records.
    blocks: HashMap<u32, Vec<(BlockHead, Placed)>>,
    /// Each overview level's block records.
    overview: HashMap<Level, Vec<(OverviewHead, Placed)>>,
    /// Each table's page records at each depth: offset and length.
    pages: HashMap<Pages, Vec<(u64, u32)>>,
    /// The latest annotation bundle record: offset and length.
    bundle: Option<(u64, u32)>,
    /// The annotation part records' frames, with their decoded lengths.
    parts: Vec<Placed>,
}

impl Written {
    /// The record of `kind` and `payload` just written at `offset`. `decoded` is what an overview
    /// block's frame decodes to, where `file` compressed it.
    fn note(&mut self, kind: u8, offset: u64, payload: &[u8], decoded: Option<u32>) {
        let frame_at = |head: usize| offset + (record::PAYLOAD_AT + head) as u64;
        match Kind::from_byte(kind) {
            Some(Kind::Block) => {
                if let Some((head, frame)) = BlockHead::parse(payload) {
                    let placed = Placed {
                        offset: frame_at(BlockHead::LEN),
                        length: frame.len() as u32,
                        decoded: None,
                    };
                    self.blocks
                        .entry(head.channel)
                        .or_default()
                        .push((head, placed));
                }
            }
            Some(Kind::OverviewBlock) => {
                if let Some((head, frame)) = OverviewHead::parse(payload) {
                    let placed = Placed {
                        offset: frame_at(OverviewHead::LEN),
                        length: frame.len() as u32,
                        decoded,
                    };
                    self.overview
                        .entry((head.channel, head.shift))
                        .or_default()
                        .push((head, placed));
                }
            }
            Some(Kind::IndexPage | Kind::ListPage) => {
                if let Some((h, _)) = PageHead::parse(payload) {
                    let len = (record::FRAMING + payload.len()) as u32;
                    self.pages
                        .entry((h.channel, h.table, h.shift, h.depth))
                        .or_default()
                        .push((offset, len));
                }
            }
            Some(Kind::Bundle) => {
                self.bundle = Some((offset, (record::FRAMING + payload.len()) as u32));
            }
            Some(Kind::Part) if payload.len() >= 4 => self.parts.push(Placed {
                offset: frame_at(4),
                length: (payload.len() - 4) as u32,
                decoded: u32_at(payload, 0),
            }),
            _ => {}
        }
    }

    /// An index or list page's payload pointed at the records written before it: its block or
    /// overview rows, or its directory entries.
    fn fix_page(&self, payload: &mut [u8]) {
        let Some((h, _)) = PageHead::parse(payload) else {
            return;
        };
        let first = usize::try_from(h.place).unwrap_or(usize::MAX);
        let body = &mut payload[PAGE_HEAD..];
        if h.depth > 0 {
            let pages = self.pages.get(&(h.channel, h.table, h.shift, h.depth - 1));
            for (i, e) in body
                .as_chunks_mut::<{ Entry::SIZE }>()
                .0
                .iter_mut()
                .enumerate()
            {
                if let Some(&(offset, length)) = pages.and_then(|p| p.get(first.saturating_add(i)))
                {
                    put_place(e, offset, length);
                }
            }
        } else if h.table == Table::Blocks.code() {
            let blocks = self.blocks.get(&h.channel);
            for (i, r) in body
                .as_chunks_mut::<{ BlockRow::SIZE }>()
                .0
                .iter_mut()
                .enumerate()
            {
                if let Some((head, p)) = blocks.and_then(|b| b.get(first.saturating_add(i))) {
                    r[..8].copy_from_slice(&head.start.to_le_bytes());
                    r[8..16].copy_from_slice(&p.offset.to_le_bytes());
                    r[16..20].copy_from_slice(&p.length.to_le_bytes());
                    r[20..24].copy_from_slice(&head.decoded_length.to_le_bytes());
                    r[24..28].copy_from_slice(&head.word.to_le_bytes());
                }
            }
        } else if h.table == Table::Level(h.shift).code() {
            let level = self.overview.get(&(h.channel, h.shift));
            for (i, r) in body
                .as_chunks_mut::<{ OverviewRow::SIZE }>()
                .0
                .iter_mut()
                .enumerate()
            {
                if let Some((head, p)) = level.and_then(|l| l.get(first.saturating_add(i))) {
                    r[..8].copy_from_slice(&p.offset.to_le_bytes());
                    r[8..12].copy_from_slice(&p.length.to_le_bytes());
                    if let Some(d) = p.decoded {
                        r[12..16].copy_from_slice(&d.to_le_bytes());
                    }
                    r[16..20].copy_from_slice(&head.n.to_le_bytes());
                }
            }
        } else if h.table == Table::Parts.code() {
            for (i, r) in body
                .as_chunks_mut::<{ escapement_tickwave_format::parts::PartRow::SIZE }>()
                .0
                .iter_mut()
                .enumerate()
            {
                if let Some(p) = self.parts.get(first.saturating_add(i)) {
                    r[..8].copy_from_slice(&p.offset.to_le_bytes());
                    r[8..12].copy_from_slice(&p.length.to_le_bytes());
                    if let Some(d) = p.decoded {
                        r[12..16].copy_from_slice(&d.to_le_bytes());
                    }
                }
            }
        }
    }

    /// The top index pointed at the records written: each table's entries at its pages, and the
    /// bundle's place at the bundle record.
    fn fix_top(&self, index: &mut [u8], tables: &[TopTable]) {
        if let (Some((at, len)), Some(place)) = (self.bundle, index.get_mut(..12)) {
            place[..8].copy_from_slice(&at.to_le_bytes());
            place[8..].copy_from_slice(&len.to_le_bytes());
        }
        for t in tables {
            let (Some(&depth), Some(&e)) = (index.get(t.at), index.get(t.at + 1)) else {
                continue;
            };
            let pages = self
                .pages
                .get(&(t.channel, t.table.code(), t.table.shift(), depth));
            for i in 0..usize::from(e) {
                let at = t.at + 2 + i * Entry::SIZE;
                if let (Some(&(offset, length)), Some(entry)) = (
                    pages.and_then(|p| p.get(i)),
                    index.get_mut(at..at + Entry::SIZE),
                ) {
                    put_place(entry, offset, length);
                }
            }
        }
    }
}

/// A page record's offset and length, into a directory or top-index entry.
fn put_place(entry: &mut [u8], offset: u64, length: u32) {
    entry[16..24].copy_from_slice(&offset.to_le_bytes());
    entry[24..28].copy_from_slice(&length.to_le_bytes());
}

/// One table's entries in the top index: where its depth byte lies, and whose table it is.
struct TopTable {
    at: usize,
    channel: u32,
    table: Table,
}

/// The top index `index` as the header `h` lays it out (FORMAT.md, "Footer"): where each
/// channel's strip lengths lie, and each table's entries, as far as the index holds them.
fn top_layout(h: &Header, index: &[u8]) -> (Vec<usize>, Vec<TopTable>) {
    let (mut strips, mut tables) = (Vec::new(), Vec::new());
    let mut at = 12;
    if h.annotation_parts.is_some() {
        let Some(&e) = index.get(at + 1) else {
            return (strips, tables);
        };
        tables.push(TopTable {
            at,
            channel: 0,
            table: Table::Parts,
        });
        at += 2 + usize::from(e) * Entry::SIZE;
    }
    for o in 0..h.channels.len() + h.derived.len() {
        if at + 8 > index.len() {
            break;
        }
        strips.push(at);
        at += 8;
        let mut own = Vec::new();
        if o < h.channels.len() {
            own.push(Table::Blocks);
        }
        own.extend(
            h.levels(o)
                .unwrap_or_default()
                .iter()
                .map(|l| Table::Level(l.shift)),
        );
        match h.channels.get(o).map(|c| &c.data) {
            Some(ChannelKind::Digital {
                gap_count: Some(_), ..
            }) => own.push(Table::Gaps),
            Some(ChannelKind::Analog {
                break_count: Some(_),
                ..
            }) => own.push(Table::Breaks),
            _ => {}
        }
        for table in own {
            let Some(&e) = index.get(at + 1) else {
                return (strips, tables);
            };
            tables.push(TopTable {
                at,
                channel: o as u32,
                table,
            });
            at += 2 + usize::from(e) * Entry::SIZE;
        }
    }
    (strips, tables)
}

/// The decoded strips `raw` as frames, cut at the decoded length the top index `index` gives at
/// each of `strips`, each frame's length written there, and its decoded length where `raw` runs
/// out first. Bytes past the last strip go in one more frame.
fn strip_frames(index: &mut [u8], strips: &[usize], mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for &at in strips {
        let n = (u32_at(index, at + 4).unwrap_or(0) as usize).min(raw.len());
        let (strip, rest) = raw.split_at(n);
        raw = rest;
        let frame = compress(strip);
        index[at..at + 4].copy_from_slice(&(frame.len() as u32).to_le_bytes());
        index[at + 4..at + 8].copy_from_slice(&(n as u32).to_le_bytes());
        out.extend_from_slice(&frame);
    }
    if !raw.is_empty() {
        out.extend_from_slice(&compress(raw));
    }
    out
}

/// A header's or declaration's JSON naming what `flags` asks of it that this version doesn't
/// know; the bytes as they are where they don't read as a JSON object.
fn unknown(json: &[u8], flags: u8) -> Vec<u8> {
    if flags & (REQUIRES | CODEC | OPERATION) == 0 {
        return json.to_vec();
    }
    let Ok(serde_json::Value::Object(mut v)) = serde_json::from_slice(json) else {
        return json.to_vec();
    };
    if let Some(serde_json::Value::Array(channels)) = v.get_mut("channels") {
        if flags & REQUIRES != 0
            && let Some(serde_json::Value::Object(c)) = channels.first_mut()
        {
            c.insert("requires".into(), serde_json::json!(["x-fuzz"]));
        }
        if flags & CODEC != 0
            && let Some(serde_json::Value::Object(c)) = channels.last_mut()
        {
            c.insert("codec".into(), "x-fuzz".into());
        }
    }
    if flags & OPERATION != 0
        && let Some(serde_json::Value::Array(derived)) = v.get_mut("derived")
        && let Some(serde_json::Value::Object(d)) = derived.first_mut()
    {
        d.insert("op".into(), "x-fuzz".into());
    }
    serde_json::to_vec(&v).unwrap_or_else(|_| json.to_vec())
}

/// `raw` padded with zeros to one byte short of `limit`, `limit`, or one past it, as its first
/// byte picks.
fn at_limit(raw: &[u8], limit: usize) -> Vec<u8> {
    let n = limit - 1 + usize::from(raw.first().map_or(0, |b| b % 3));
    let mut out = raw[..raw.len().min(n)].to_vec();
    out.resize(n, 0);
    out
}

/// A structured footer's body as the footer's payload: its strips compressed, and its top index
/// pointed at what `file` wrote where `written` is given.
fn footer_payload(body: &[u8], written: Option<&Written>, flags: u8) -> Vec<u8> {
    let header_len = u32_at(body, 0).unwrap_or(0) as usize;
    let header = body
        .get(4..)
        .map_or(&[][..], |b| &b[..header_len.min(b.len())]);
    let after = (4 + header.len()).min(body.len());
    let index_len = u32_at(body, after).unwrap_or(0) as usize;
    let index_at = (after + 4).min(body.len());
    let mut index = body[index_at..][..index_len.min(body.len() - index_at)].to_vec();
    let raw = &body[index_at + index.len()..];
    let header = &unknown(header, flags)[..];
    let (strips, tables) = serde_json::from_slice::<Header>(header)
        .map_or_else(|_| (Vec::new(), Vec::new()), |h| top_layout(&h, &index));
    let frames = strip_frames(&mut index, &strips, raw);
    if let Some(w) = written {
        w.fix_top(&mut index, &tables);
    }
    let mut payload = Vec::with_capacity(8 + header.len() + index.len() + frames.len());
    payload.extend_from_slice(&(header.len() as u32).to_le_bytes());
    payload.extend_from_slice(&(index.len() as u32).to_le_bytes());
    payload.extend_from_slice(header);
    payload.extend_from_slice(&index);
    payload.extend_from_slice(&frames);
    payload
}

/// The recording a structured input describes.
pub fn file(data: &[u8]) -> Vec<u8> {
    let Some((&flags, mut rest)) = data.split_first() else {
        return Vec::new();
    };
    let fix = flags & FIX_INDEX != 0;
    let mut out = Vec::new();
    if flags & NO_PREAMBLE == 0 {
        out.extend_from_slice(&Preamble::CURRENT.to_bytes());
    }
    let mut written = Written::default();
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
        let compressed = mode & COMPRESS != 0;
        let limits = flags & LIMITS != 0;
        let mut decoded = None;
        let mut payload = match kind {
            1 => unknown(body, flags),
            2 if compressed && body.len() >= BlockHead::LEN => {
                let (head, raw) = body.split_at(BlockHead::LEN);
                let padded;
                let raw = if limits {
                    padded = at_limit(raw, record::MAX_BLOCK);
                    &padded[..]
                } else {
                    raw
                };
                let mut payload = head.to_vec();
                payload[24..28].copy_from_slice(&(raw.len() as u32).to_le_bytes());
                payload.extend_from_slice(&compress(raw));
                payload
            }
            5 if compressed && body.len() >= OverviewHead::LEN => {
                let (head, raw) = body.split_at(OverviewHead::LEN);
                decoded = Some(raw.len() as u32);
                [head, &compress(raw)].concat()
            }
            7 if compressed => footer_payload(body, fix.then_some(&written), flags),
            12 | 13 if compressed => {
                let limit = if kind == 12 {
                    record::MAX_BUNDLE
                } else {
                    record::MAX_PART
                };
                let padded;
                let raw = if limits {
                    padded = at_limit(body, limit);
                    &padded[..]
                } else {
                    body
                };
                let mut payload = (raw.len() as u32).to_le_bytes().to_vec();
                payload.extend_from_slice(&compress(raw));
                payload
            }
            _ => body.to_vec(),
        };
        if fix && (kind == Kind::IndexPage as u8 || kind == Kind::ListPage as u8) {
            written.fix_page(&mut payload);
        }
        let start = out.len() as u64;
        frame(&mut out, kind, &payload);
        if mode & BAD_CHECK != 0
            && let Some(last) = out.last_mut()
        {
            *last ^= 1;
        }
        written.note(kind, start, &payload, decoded);
        if kind == Kind::Footer as u8 {
            footer = Some((start, (out.len() as u64 - start) as u32));
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

/// Every zstd frame in `frames`, decoded and joined.
fn decode_all(frames: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    zstd::stream::Decoder::new(frames)
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

/// A block's or overview block's payload as its structured body: its `head` bytes, then its frame
/// decoded.
fn head_and_decoded(payload: &[u8], head: usize) -> Option<Vec<u8>> {
    let (head, frame) = payload.split_at_checked(head)?;
    Some([head, &decode_all(frame)?].concat())
}

/// A footer's payload as a structured footer's body, its strips decoded and joined.
fn footer_body(payload: &[u8]) -> Option<Vec<u8>> {
    let header_len = u32_at(payload, 0)? as usize;
    let index_len = u32_at(payload, 4)? as usize;
    let header = payload.get(8..8usize.checked_add(header_len)?)?;
    let index = payload.get(8 + header_len..(8 + header_len).checked_add(index_len)?)?;
    let frames = &payload[8 + header_len + index_len..];
    let mut body = (header_len as u32).to_le_bytes().to_vec();
    body.extend_from_slice(header);
    body.extend_from_slice(&(index_len as u32).to_le_bytes());
    body.extend_from_slice(index);
    body.extend_from_slice(&decode_all(frames)?);
    Some(body)
}

/// A recording as a structured input that `file` builds into an equivalent file: its frames
/// decoded, its checks, index places and trailer left for `file` to make.
pub fn structure(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < PREAMBLE_LEN || Preamble::parse(bytes).is_none() {
        return None;
    }
    let mut out = vec![AUTO_TRAILER | FIX_INDEX];
    let mut pos = PREAMBLE_LEN;
    let mut finished = false;
    while let Next::Record { kind, payload, len } = record::next(&bytes[pos..], usize::MAX) {
        pos += len;
        let decoded = match Kind::from_byte(kind) {
            Some(Kind::Block) => head_and_decoded(payload, BlockHead::LEN),
            Some(Kind::OverviewBlock) => head_and_decoded(payload, OverviewHead::LEN),
            Some(Kind::Bundle | Kind::Part) => payload.get(4..).and_then(decode_all),
            Some(Kind::Footer) => footer_body(payload),
            _ => None,
        };
        match decoded {
            Some(body) => put(&mut out, kind, COMPRESS, &body),
            None => put(&mut out, kind, 0, payload),
        }
        if kind == Kind::Footer as u8 {
            finished = true;
            // The trailer follows; `file` makes it.
            pos = bytes.len().min(pos + TRAILER_LEN);
            break;
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
