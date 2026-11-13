//! Records, the file's body: a kind, the payload's length, the payload, then a check over all
//! three, so a reader taking them in order finds where a cut-off file stops being whole.

use serde::{Deserialize, Serialize};

use crate::header::{BlockRow, Header, OverviewRow, Preamble, Trailer};
use crate::model::{Break, Gap};

/// A record's bytes besides its payload: the kind, the length and the check.
pub const FRAMING: usize = 1 + 4 + CHECK_LEN;
const CHECK_LEN: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Declaration = 1,
    Block = 2,
    Gap = 3,
    Run = 4,
    OverviewBlock = 5,
    Annotation = 6,
    Footer = 7,
}

impl Kind {
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Declaration,
            2 => Self::Block,
            3 => Self::Gap,
            4 => Self::Run,
            5 => Self::OverviewBlock,
            6 => Self::Annotation,
            7 => Self::Footer,
            _ => return None,
        })
    }
}

/// A record kind a reader may skip: one a later minor version added.
pub fn skippable(kind: u8) -> bool {
    kind >= 128
}

fn check(kind: u8, parts: &[&[u8]]) -> [u8; CHECK_LEN] {
    let len: usize = parts.iter().map(|p| p.len()).sum();
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[kind]);
    hasher.update(&(len as u32).to_le_bytes());
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().as_bytes()[..CHECK_LEN]
        .try_into()
        .unwrap()
}

/// Appends a record whose payload is `parts` joined. The caller keeps the payload under 4 GiB.
pub fn write(out: &mut Vec<u8>, kind: Kind, parts: &[&[u8]]) {
    let len: usize = parts.iter().map(|p| p.len()).sum();
    out.reserve(FRAMING + len);
    out.push(kind as u8);
    out.extend_from_slice(&(len as u32).to_le_bytes());
    for part in parts {
        out.extend_from_slice(part);
    }
    out.extend_from_slice(&check(kind as u8, parts));
}

/// What the bytes at a record's start hold.
#[derive(Debug, PartialEq, Eq)]
pub enum Next<'a> {
    /// The record runs past the bytes: it needs this many from its start.
    Need(u64),
    /// Not a whole record: kind 0, or a check that does not match.
    Bad,
    /// A payload of this many bytes, past what the reader holds: whole or not, it cannot tell.
    TooLarge(u64),
    Record {
        kind: u8,
        payload: &'a [u8],
        /// The record's whole length, its framing included.
        len: usize,
    },
}

/// The record at the start of `bytes`, with a payload of at most `max` bytes.
pub fn next(bytes: &[u8], max: usize) -> Next<'_> {
    match bytes.first() {
        None => return Next::Need(5),
        Some(0) => return Next::Bad,
        Some(_) => {}
    }
    if bytes.len() < 5 {
        return Next::Need(5);
    }
    let n = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as u64;
    if n > max as u64 {
        return Next::TooLarge(n);
    }
    let len = FRAMING as u64 + n;
    if (bytes.len() as u64) < len {
        return Next::Need(len);
    }
    let (n, len) = (n as usize, len as usize);
    let payload = &bytes[5..5 + n];
    if bytes[5 + n..len] != check(bytes[0], &[payload]) {
        return Next::Bad;
    }
    Next::Record {
        kind: bytes[0],
        payload,
        len,
    }
}

/// Where a record's payload starts, from the record's start.
pub const PAYLOAD_AT: usize = 5;

/// A block record's payload up to its zstd frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockHead {
    pub start: i64,
    pub end: i64,
    pub decoded_length: u32,
    /// A word a channel, as the index has it.
    pub words: Vec<u32>,
}

impl BlockHead {
    pub fn len(channels: usize) -> usize {
        20 + 4 * channels
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::len(self.words.len()));
        out.extend_from_slice(&self.start.to_le_bytes());
        out.extend_from_slice(&self.end.to_le_bytes());
        out.extend_from_slice(&self.decoded_length.to_le_bytes());
        for word in &self.words {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out
    }

    /// The head and the frame after it; None if the payload is shorter than a head.
    pub fn parse(payload: &[u8], channels: usize) -> Option<(Self, &[u8])> {
        let len = Self::len(channels);
        let head = payload.get(..len)?;
        let u32_at = |at: usize| u32::from_le_bytes(head[at..at + 4].try_into().unwrap());
        Some((
            Self {
                start: i64::from_le_bytes(head[0..8].try_into().unwrap()),
                end: i64::from_le_bytes(head[8..16].try_into().unwrap()),
                decoded_length: u32_at(16),
                words: (0..channels).map(|c| u32_at(20 + 4 * c)).collect(),
            },
            &payload[len..],
        ))
    }
}

pub const GAP_LEN: usize = 21;

pub fn gap_bytes(channel: u32, gap: Gap) -> [u8; GAP_LEN] {
    let mut out = [0; GAP_LEN];
    out[..4].copy_from_slice(&channel.to_le_bytes());
    out[4..12].copy_from_slice(&gap.from.to_le_bytes());
    out[12..20].copy_from_slice(&gap.to.to_le_bytes());
    out[20] = u8::from(gap.level);
    out
}

/// None unless the payload is exactly a gap with a level of 0 or 1.
pub fn parse_gap(payload: &[u8]) -> Option<(u32, Gap)> {
    if payload.len() != GAP_LEN || payload[20] > 1 {
        return None;
    }
    Some((
        u32::from_le_bytes(payload[..4].try_into().unwrap()),
        Gap {
            from: i64::from_le_bytes(payload[4..12].try_into().unwrap()),
            to: i64::from_le_bytes(payload[12..20].try_into().unwrap()),
            level: payload[20] == 1,
        },
    ))
}

pub const RUN_LEN: usize = 28;

/// A run record's payload: sample `run.index` of `channel` starts a run at `run.tick`, after
/// `run.lost` samples the instrument lost.
pub fn run_bytes(channel: u32, run: Break) -> [u8; RUN_LEN] {
    let mut out = [0; RUN_LEN];
    out[..4].copy_from_slice(&channel.to_le_bytes());
    out[4..12].copy_from_slice(&run.index.to_le_bytes());
    out[12..20].copy_from_slice(&run.tick.to_le_bytes());
    out[20..28].copy_from_slice(&run.lost.to_le_bytes());
    out
}

/// None unless the payload is exactly a run: channel, index, tick, lost.
pub fn parse_run(payload: &[u8]) -> Option<(u32, Break)> {
    if payload.len() != RUN_LEN {
        return None;
    }
    Some((
        u32::from_le_bytes(payload[..4].try_into().unwrap()),
        Break {
            index: u64::from_le_bytes(payload[4..12].try_into().unwrap()),
            tick: i64::from_le_bytes(payload[12..20].try_into().unwrap()),
            lost: u64::from_le_bytes(payload[20..28].try_into().unwrap()),
        },
    ))
}

/// An overview block record's payload before its frame: the level's shift, then `k`.
pub const OVERVIEW_HEAD_LEN: usize = 9;

pub fn overview_head(shift: u8, k: u64) -> [u8; OVERVIEW_HEAD_LEN] {
    let mut out = [0; OVERVIEW_HEAD_LEN];
    out[0] = shift;
    out[1..].copy_from_slice(&k.to_le_bytes());
    out
}

/// The shift, `k` and the frame; None if the payload is shorter than the head.
pub fn parse_overview(payload: &[u8]) -> Option<(u8, u64, &[u8])> {
    let head = payload.get(..OVERVIEW_HEAD_LEN)?;
    Some((
        head[0],
        u64::from_le_bytes(head[1..].try_into().unwrap()),
        &payload[OVERVIEW_HEAD_LEN..],
    ))
}

/// An annotation record's payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    /// The generator's name, as its file's first line gives it.
    pub source: String,
    /// One line of the generator's file: one annotation, its times in ticks.
    pub annotation: serde_json::Map<String, serde_json::Value>,
}

/// The footer record's payload before the header: its length, then the index's.
pub const FOOTER_HEAD_LEN: usize = 8;

/// The footer record's payload: the header, the index, then the overview's frame. Err with the
/// size if it passes what a record's u32 length holds.
pub fn footer_bytes(
    header: &Header,
    rows: &[BlockRow],
    overview_rows: &[OverviewRow],
    overview_frame: &[u8],
) -> Result<Vec<u8>, usize> {
    let json = serde_json::to_vec(header).expect("a header always serialises");
    let index_len = rows.len() * BlockRow::size(header.channels.len())
        + overview_rows.len() * OverviewRow::SIZE;
    let total = FOOTER_HEAD_LEN + json.len() + index_len + overview_frame.len();
    if u32::try_from(total + FRAMING).is_err() {
        return Err(total);
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&(index_len as u32).to_le_bytes());
    out.extend_from_slice(&json);
    for row in rows {
        row.write(&mut out);
    }
    for row in overview_rows {
        row.write(&mut out);
    }
    out.extend_from_slice(overview_frame);
    Ok(out)
}

/// Appends the footer record and the trailer to `out`, whose first byte sits at `at` in the file.
pub fn write_footer(out: &mut Vec<u8>, at: u64, footer: &[u8]) {
    let footer_offset = at + out.len() as u64;
    write(out, Kind::Footer, &[footer]);
    out.extend_from_slice(
        &Trailer {
            footer_offset,
            footer_len: (footer.len() + FRAMING) as u32,
            major: Preamble::CURRENT.major,
            minor: Preamble::CURRENT.minor,
        }
        .to_bytes(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_reads_back_and_any_changed_byte_spoils_it() {
        let mut out = Vec::new();
        write(&mut out, Kind::Gap, &[b"ab", b"cde"]);
        assert_eq!(out.len(), FRAMING + 5);
        assert_eq!(
            next(&out, 1 << 20),
            Next::Record {
                kind: Kind::Gap as u8,
                payload: b"abcde",
                len: out.len()
            }
        );
        for i in 0..out.len() {
            let mut bad = out.clone();
            bad[i] ^= 0x10;
            assert!(
                !matches!(next(&bad, 1 << 20), Next::Record { .. }),
                "byte {i} changed"
            );
        }
    }

    #[test]
    fn a_record_cut_short_asks_for_the_rest() {
        let mut out = Vec::new();
        write(&mut out, Kind::Block, &[&[7; 40]]);
        for cut in 1..out.len() {
            let want = if cut < 5 { 5 } else { out.len() as u64 };
            assert_eq!(next(&out[..cut], 1 << 20), Next::Need(want), "cut at {cut}");
        }
        assert_eq!(next(&[], 1 << 20), Next::Need(5));
        assert_eq!(next(&[0, 0, 0], 1 << 20), Next::Bad);
        assert_eq!(next(&out, 39), Next::TooLarge(40));
    }
}
