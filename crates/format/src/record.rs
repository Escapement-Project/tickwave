//! Records, the file's body: a kind, the payload's length, the payload, then a check over all
//! three, so a reader taking them in order finds where a cut-off file stops being whole.

use crate::header::{Preamble, Trailer};
use crate::model::{Break, Gap};

/// A record's bytes besides its payload: the kind, the length and the check.
pub const FRAMING: usize = 1 + 4 + CHECK_LEN;
/// The bytes of a record's check, after its payload.
pub const CHECK_LEN: usize = 8;

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
    KnownThrough = 8,
    IndexPage = 10,
    ListPage = 11,
    Bundle = 12,
    Part = 13,
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
            8 => Self::KnownThrough,
            10 => Self::IndexPage,
            11 => Self::ListPage,
            12 => Self::Bundle,
            13 => Self::Part,
            _ => return None,
        })
    }
}

/// A record kind a reader may skip: one a later minor version added.
pub fn skippable(kind: u8) -> bool {
    kind >= 128
}

/// Kinds 128 to 191: optional records, which a reader skips and a tool that writes a recording
/// again refuses, as it would drop them.
pub fn optional(kind: u8) -> bool {
    (128..192).contains(&kind)
}

/// Kinds 192 to 255: auxiliary records, which hold only what the rest of the file gives again. A
/// reader skips them, and a tool that writes a recording again drops them.
pub fn auxiliary(kind: u8) -> bool {
    kind >= 192
}

/// Why a tool that writes a recording again, `doing` it ("re-cutting"), refuses one it reads: it
/// would drop what the recording holds.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Rewrite {
    #[error(
        "the file is format 1.{minor}, newer than this tool's 1.{}: {doing} would drop what it adds",
        crate::header::MINOR
    )]
    Newer { minor: u16, doing: &'static str },
    #[error(
        "the file holds an optional record of kind {kind}, which this version doesn't know: \
         {doing} would drop it"
    )]
    Optional { kind: u8, doing: &'static str },
    #[error(
        "channel {channel} is of kind {kind}, which this version doesn't know: {doing} would drop \
         what its entry holds"
    )]
    Kind {
        channel: String,
        kind: String,
        doing: &'static str,
    },
}

impl Rewrite {
    /// The refusal for a recording of minor version `minor`, where it is later than this one's.
    pub fn of_minor(minor: u16, doing: &'static str) -> Option<Self> {
        (minor > crate::header::MINOR).then_some(Self::Newer { minor, doing })
    }

    /// The refusal for a record of `kind`, where it is an optional one: this version knows none.
    pub fn of_kind(kind: u8, doing: &'static str) -> Option<Self> {
        optional(kind).then_some(Self::Optional { kind, doing })
    }
}

/// The most bytes a block decodes to (FORMAT.md, "Limits").
pub const MAX_BLOCK: usize = 1 << 22;
/// The most an overview block decodes to: 4,096 buckets of at most 32 bytes.
pub const MAX_OVERVIEW_BLOCK: usize = 4096 * 32;
/// The most a strip decodes to: 1,024 buckets of at most 32 bytes.
pub const MAX_STRIP: usize = 1024 * 32;
/// The most an annotation bundle decodes to.
pub const MAX_BUNDLE: usize = 64 << 20;
/// The most an annotation part decodes to, and an annotation record's payload.
pub const MAX_PART: usize = 4 << 20;
/// The most a declaration's payload holds, and a footer's header.
pub const MAX_DECLARATION: usize = 64 << 20;
/// The most a footer record holds, its framing included.
pub const MAX_FOOTER: usize = 256 << 20;
/// The most channels and derived channels a recording has, together.
pub const MAX_CHANNELS: usize = 4096;
/// The most generators a recording's annotations have.
pub const MAX_GENERATORS: usize = 256;

/// zstd's `ZSTD_COMPRESSBOUND`: the most bytes a frame of `n` decoded bytes needs.
pub const fn compress_bound(n: usize) -> usize {
    let margin = if n < 128 << 10 {
        ((128 << 10) - n) >> 11
    } else {
        0
    };
    n + (n >> 8) + margin
}

/// The most a page record's payload holds: its head and 4,096 entries of the largest size, a
/// part row's 32 bytes. A page's exact length follows from its table's shape.
const MAX_PAGE: usize = crate::index::PAGE_HEAD + 4096 * 32;

/// The payload's length a record of `kind` may have: an exact length where the kind has one, and
/// otherwise the most it holds. A kind from 128 up can be any length; one this version doesn't
/// define from 1 to 127 has none.
pub fn payload_limit(kind: u8) -> Option<Limit> {
    let most = |n: usize| Some(Limit::Most(n as u64));
    match Kind::from_byte(kind) {
        Some(Kind::Declaration) => most(MAX_DECLARATION),
        Some(Kind::Block) => most(BlockHead::LEN + compress_bound(MAX_BLOCK)),
        Some(Kind::Gap) => Some(Limit::Exactly(GAP_LEN as u64)),
        Some(Kind::Run) => Some(Limit::Exactly(RUN_LEN as u64)),
        Some(Kind::OverviewBlock) => most(OverviewHead::LEN + compress_bound(MAX_OVERVIEW_BLOCK)),
        Some(Kind::Annotation) => most(MAX_PART),
        Some(Kind::Footer) => most(MAX_FOOTER - FRAMING),
        Some(Kind::KnownThrough) => Some(Limit::Exactly(8)),
        Some(Kind::IndexPage | Kind::ListPage) => most(MAX_PAGE),
        Some(Kind::Bundle) => most(4 + compress_bound(MAX_BUNDLE)),
        Some(Kind::Part) => most(4 + compress_bound(MAX_PART)),
        None if skippable(kind) => most(u32::MAX as usize),
        None => None,
    }
}

/// A record kind's payload length: exactly this, or at most this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    Exactly(u64),
    Most(u64),
}

impl Limit {
    pub fn allows(self, n: u64) -> bool {
        match self {
            Limit::Exactly(e) => n == e,
            Limit::Most(m) => n <= m,
        }
    }
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
    /// A payload of `n` bytes, which no record of `kind` holds (`payload_limit`): not a whole
    /// record, as `Bad`.
    Oversized { kind: u8, n: u64 },
    Record {
        kind: u8,
        payload: &'a [u8],
        /// The record's whole length, its framing included.
        len: usize,
    },
}

/// The record at the start of `bytes`, with a payload of at most `max` bytes: `Oversized`,
/// before the payload is read, where its kind holds no payload of its length (`payload_limit`).
/// A reader stops there as at a record that doesn't check: what follows a crash can be any
/// bytes. A kind this version doesn't define below 128 has no limit here; the reader refuses it
/// once its record checks.
pub fn next_checked(bytes: &[u8], max: usize) -> Next<'_> {
    if let (Some(&kind), Some(n)) = (bytes.first(), bytes.get(1..5))
        && kind != 0
    {
        let n = u32::from_le_bytes(n.try_into().unwrap()) as u64;
        if payload_limit(kind).is_some_and(|limit| !limit.allows(n)) {
            return Next::Oversized { kind, n };
        }
    }
    next(bytes, max)
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockHead {
    pub channel: u32,
    pub start: i64,
    /// The channel's next block's start, or the length for its last.
    pub end: i64,
    /// The channel's word, as the index has it.
    pub word: u32,
    pub decoded_length: u32,
}

impl BlockHead {
    pub const LEN: usize = 28;

    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut out = [0; Self::LEN];
        out[..4].copy_from_slice(&self.channel.to_le_bytes());
        out[4..12].copy_from_slice(&self.start.to_le_bytes());
        out[12..20].copy_from_slice(&self.end.to_le_bytes());
        out[20..24].copy_from_slice(&self.word.to_le_bytes());
        out[24..28].copy_from_slice(&self.decoded_length.to_le_bytes());
        out
    }

    /// The head and the frame after it; None if the payload is shorter than a head.
    pub fn parse(payload: &[u8]) -> Option<(Self, &[u8])> {
        let head = payload.get(..Self::LEN)?;
        let u32_at = |at: usize| u32::from_le_bytes(head[at..at + 4].try_into().unwrap());
        Some((
            Self {
                channel: u32_at(0),
                start: i64::from_le_bytes(head[4..12].try_into().unwrap()),
                end: i64::from_le_bytes(head[12..20].try_into().unwrap()),
                word: u32_at(20),
                decoded_length: u32_at(24),
            },
            &payload[Self::LEN..],
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

pub const RUN_LEN: usize = 32;

/// A run record's payload: sample `run.index` of `channel` starts a run at `run.tick` plus
/// `run.phase / D`, after `run.lost` samples the instrument lost.
pub fn run_bytes(channel: u32, run: Break) -> [u8; RUN_LEN] {
    let mut out = [0; RUN_LEN];
    out[..4].copy_from_slice(&channel.to_le_bytes());
    out[4..12].copy_from_slice(&run.index.to_le_bytes());
    out[12..20].copy_from_slice(&run.tick.to_le_bytes());
    out[20..28].copy_from_slice(&run.lost.to_le_bytes());
    out[28..32].copy_from_slice(&run.phase.to_le_bytes());
    out
}

/// None unless the payload is exactly a run: channel, index, tick, lost, phase.
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
            phase: u32::from_le_bytes(payload[28..32].try_into().unwrap()),
        },
    ))
}

/// An overview block record's payload before its frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverviewHead {
    /// The overview channel: a derived channel numbered after the channels.
    pub channel: u32,
    pub shift: u8,
    /// Its first block is block `k` of the level.
    pub k: u64,
    /// The blocks it stands for, each its frame's bytes.
    pub n: u32,
}

impl OverviewHead {
    pub const LEN: usize = 17;

    pub fn to_bytes(&self) -> [u8; Self::LEN] {
        let mut out = [0; Self::LEN];
        out[..4].copy_from_slice(&self.channel.to_le_bytes());
        out[4] = self.shift;
        out[5..13].copy_from_slice(&self.k.to_le_bytes());
        out[13..17].copy_from_slice(&self.n.to_le_bytes());
        out
    }

    /// The head and the frame after it; None if the payload is shorter than a head.
    pub fn parse(payload: &[u8]) -> Option<(Self, &[u8])> {
        let head = payload.get(..Self::LEN)?;
        Some((
            Self {
                channel: u32::from_le_bytes(head[..4].try_into().unwrap()),
                shift: head[4],
                k: u64::from_le_bytes(head[5..13].try_into().unwrap()),
                n: u32::from_le_bytes(head[13..17].try_into().unwrap()),
            },
            &payload[Self::LEN..],
        ))
    }
}

/// A known-through record's payload: its tick.
pub fn known_through_bytes(tick: i64) -> [u8; 8] {
    tick.to_le_bytes()
}

/// None unless the payload is exactly a tick.
pub fn parse_known_through(payload: &[u8]) -> Option<i64> {
    Some(i64::from_le_bytes(payload.try_into().ok()?))
}

/// An annotation record's payload (FORMAT.md, "Annotations"): a generator's name and one line of
/// its file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    /// The generator's name, as its file's first line gives it.
    pub source: String,
    /// One line of the generator's file, without its newline: a JSON object, one annotation.
    pub line: String,
}

impl Annotation {
    /// The line's fields; None where it is not a JSON object.
    pub fn fields(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        serde_json::from_str(&self.line).ok()
    }

    /// The line's `start` where it is a whole tick count.
    pub fn start(&self) -> Option<i64> {
        self.fields()?.get("start")?.as_i64()
    }

    /// The record's payload: the name's length as a u16, the name, then the line and its newline.
    pub fn to_payload(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 + self.source.len() + self.line.len() + 1);
        out.extend_from_slice(&(self.source.len() as u16).to_le_bytes());
        out.extend_from_slice(self.source.as_bytes());
        out.extend_from_slice(self.line.as_bytes());
        out.push(b'\n');
        out
    }

    /// A record's payload, or why a reader refuses it: a generator name ANNOTATIONS.md doesn't
    /// allow, or a line that holds a newline before its end, lacks its newline, is not a JSON
    /// object, or names a key twice in an object.
    pub fn parse(payload: &[u8]) -> Result<Self, String> {
        let (source, line) = named(payload)?;
        let line = line
            .strip_suffix(b"\n")
            .ok_or("its line does not end with a newline")?;
        if line.contains(&b'\n') {
            return Err("its line holds a newline before its end".into());
        }
        let line = std::str::from_utf8(line).map_err(|_| "its line is not UTF-8".to_owned())?;
        match crate::parts::line_object(line) {
            Err(_) => return Err("its line is not a JSON object".into()),
            Ok(Some(key)) => return Err(format!("its line names key {key:?} twice")),
            Ok(None) => {}
        }
        Ok(Self {
            source,
            line: line.to_owned(),
        })
    }
}

/// A generator's name, as an annotation record or a part's lines start with it: a u16 length,
/// then the name, which must be one a generator may take. The name and the bytes after it.
pub(crate) fn named(bytes: &[u8]) -> Result<(String, &[u8]), String> {
    let (len, rest) = bytes
        .split_first_chunk::<2>()
        .ok_or("too short for its generator's name")?;
    let len = usize::from(u16::from_le_bytes(*len));
    let name = rest
        .get(..len)
        .ok_or("too short for its generator's name")?;
    let name = std::str::from_utf8(name).map_err(|_| "its generator's name is not UTF-8")?;
    if let Some(problem) = check_source(name) {
        return Err(problem);
    }
    Ok((name.to_owned(), &rest[len..]))
}

/// Why `source` can't name a generator, if it can't. Its file is `<stem>.<source>.jsonl`,
/// beside the recording, so the name holds no path separator.
pub(crate) fn check_source(source: &str) -> Option<String> {
    if source.is_empty() {
        return Some("an empty generator name".into());
    }
    if source.chars().any(char::is_whitespace) {
        return Some(format!("generator name {source:?} holds whitespace"));
    }
    if source.contains(['/', '\\']) {
        return Some(format!(
            "generator name {source:?} holds a '/' or '\\', and its file is named on its own"
        ));
    }
    None
}

/// The footer record's payload before the header: its length, then the top index's.
pub const FOOTER_HEAD_LEN: usize = 8;

/// The footer record's payload: the header's JSON, the top index, then the strips. Err with the
/// size if it passes what a record's u32 length holds.
pub fn footer_bytes(json: &[u8], top: &[u8], strips: &[u8]) -> Result<Vec<u8>, usize> {
    let total = FOOTER_HEAD_LEN + json.len() + top.len() + strips.len();
    if u32::try_from(total + FRAMING).is_err() {
        return Err(total);
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&(top.len() as u32).to_le_bytes());
    out.extend_from_slice(json);
    out.extend_from_slice(top);
    out.extend_from_slice(strips);
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
