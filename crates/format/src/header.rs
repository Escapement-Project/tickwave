//! What a file says about itself: the preamble, the trailer, the declaration that opens the
//! records, the header the footer holds, and the rows its index pages hold.
//!
//! Layout, little-endian:
//!
//! ```text
//! preamble  12 bytes   magic, u16 major, u16 minor
//! records   ...        u8 kind, u32 payload length, payload, 8-byte check (see `record`)
//! trailer   24 bytes   u64 footer offset, u32 footer length, u16 major, u16 minor, end magic
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::mapping::Mapping;
use crate::model::{Break, Gap, Ratio};

pub const MAGIC: [u8; 8] = *b"\x89TKW\r\n\x1a\n";
pub const TRAILER_MAGIC: [u8; 8] = *b"\x89TKWEND\n";
pub const MAJOR: u16 = 1;
pub const MINOR: u16 = 0;
pub const PREAMBLE_LEN: usize = 12;
pub const TRAILER_LEN: usize = 24;
pub const LEVEL_BIT: u32 = 1 << 31;

/// Every integer in the header stays below this, so a JavaScript reader parses it exactly.
pub const JSON_SAFE: u64 = 1 << 53;

/// The most significant figures a channel states: the digits an f64 holds.
pub const MAX_SIGNIFICANT: u8 = 17;

/// A `significant` read as an integer of any size a u64 holds, so a value past
/// `MAX_SIGNIFICANT` is refused for what it is rather than for its type.
fn significant<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u8>, D::Error> {
    match Option::<u64>::deserialize(d)? {
        Some(n) if n > u64::from(MAX_SIGNIFICANT) => Err(serde::de::Error::custom(format!(
            "{n} significant figures; a channel states at most {MAX_SIGNIFICANT}"
        ))),
        n => Ok(n.map(|n| n as u8)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preamble {
    pub major: u16,
    pub minor: u16,
}

impl Preamble {
    pub const CURRENT: Preamble = Preamble {
        major: MAJOR,
        minor: MINOR,
    };

    pub fn to_bytes(self) -> [u8; PREAMBLE_LEN] {
        let mut out = [0; PREAMBLE_LEN];
        out[..8].copy_from_slice(&MAGIC);
        out[8..10].copy_from_slice(&self.major.to_le_bytes());
        out[10..12].copy_from_slice(&self.minor.to_le_bytes());
        out
    }

    /// None unless `bytes` starts with the magic and holds a whole preamble.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < PREAMBLE_LEN || bytes[..8] != MAGIC {
            return None;
        }
        Some(Self {
            major: u16::from_le_bytes([bytes[8], bytes[9]]),
            minor: u16::from_le_bytes([bytes[10], bytes[11]]),
        })
    }
}

/// The file's last bytes: where the footer record is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trailer {
    /// Where the footer record starts.
    pub footer_offset: u64,
    /// The footer record's length, its framing included. It ends where the trailer starts.
    pub footer_len: u32,
    pub major: u16,
    pub minor: u16,
}

impl Trailer {
    pub fn to_bytes(self) -> [u8; TRAILER_LEN] {
        let mut out = [0; TRAILER_LEN];
        out[..8].copy_from_slice(&self.footer_offset.to_le_bytes());
        out[8..12].copy_from_slice(&self.footer_len.to_le_bytes());
        out[12..14].copy_from_slice(&self.major.to_le_bytes());
        out[14..16].copy_from_slice(&self.minor.to_le_bytes());
        out[16..].copy_from_slice(&TRAILER_MAGIC);
        out
    }

    /// None unless `bytes` is exactly a trailer, its magic last.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != TRAILER_LEN || bytes[16..] != TRAILER_MAGIC {
            return None;
        }
        Some(Self {
            footer_offset: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            footer_len: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            major: u16::from_le_bytes([bytes[12], bytes[13]]),
            minor: u16::from_le_bytes([bytes[14], bytes[15]]),
        })
    }
}

/// The first record: what a writer knows about a recording before any data. The footer's header
/// holds the same values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Declaration {
    pub timebase: Ratio,
    pub channels: Vec<DeclaredChannel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived: Vec<Derived>,
    pub sources: Vec<SourceHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, String>,
    /// The finest overview level's shift: the writer picks it before it knows the length.
    pub overview_shift: u8,
    /// Features a reader must know to read the recording at all. This version defines none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawChannel")]
pub struct DeclaredChannel {
    pub name: String,
    pub source_name: String,
    /// Its source's index in `sources`.
    pub source: u32,
    #[serde(flatten)]
    pub data: DeclaredKind,
    #[serde(flatten)]
    pub shaping: Shaping,
    /// Its finest overview level's shift; None takes the recording's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview_shift: Option<u8>,
    /// Features a reader must know to read this channel; one it doesn't know makes the channel
    /// unavailable to it. This version defines none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// How its blocks are stored, where not as FORMAT.md's "Blocks" says. This version defines
    /// none, so a channel that states one is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
}

/// A derived channel's operation. It reads and writes as its name, and keeps a name this
/// version doesn't know, which makes the channel unavailable.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Op {
    /// `a × b`.
    Multiply,
    /// `a − b`.
    Subtract,
    Other(String),
}

impl Op {
    pub fn as_str(&self) -> &str {
        match self {
            Op::Multiply => "multiply",
            Op::Subtract => "subtract",
            Op::Other(name) => name,
        }
    }
}

impl From<String> for Op {
    fn from(name: String) -> Self {
        match name.as_str() {
            "multiply" => Op::Multiply,
            "subtract" => Op::Subtract,
            _ => Op::Other(name),
        }
    }
}

impl From<Op> for String {
    fn from(op: Op) -> Self {
        match op {
            Op::Other(name) => name,
            known => known.as_str().to_owned(),
        }
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A channel computed from analog channels, which blocks do not store: FORMAT.md's derived
/// channels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Derived {
    pub name: String,
    #[serde(default)]
    pub unit: String,
    pub op: Op,
    /// The channels it is computed from, by name, in order: two for the operations this version
    /// defines.
    pub operands: Vec<String>,
    /// An operation's further terms; none for the operations this version defines.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    #[serde(
        default,
        deserialize_with = "significant",
        skip_serializing_if = "Option::is_none"
    )]
    pub significant: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

impl Derived {
    /// `a × b`, as power is current times voltage.
    pub fn multiply(name: &str, unit: &str, a: &str, b: &str) -> Self {
        Self::of(Op::Multiply, name, unit, a, b)
    }

    /// `a − b`, as a differential probe's two inputs give one signal.
    pub fn subtract(name: &str, unit: &str, a: &str, b: &str) -> Self {
        Self::of(Op::Subtract, name, unit, a, b)
    }

    fn of(op: Op, name: &str, unit: &str, a: &str, b: &str) -> Self {
        Self {
            name: name.into(),
            unit: unit.into(),
            op,
            operands: vec![a.into(), b.into()],
            params: BTreeMap::new(),
            significant: None,
            requires: Vec::new(),
        }
    }

    /// Whether this version knows how to compute it, from its own entry: an operation it
    /// defines, given no `params`, and no `requires`. Its operands must be available too.
    pub fn known(&self) -> bool {
        matches!(self.op, Op::Multiply | Op::Subtract)
            && self.params.is_empty()
            && self.requires.is_empty()
    }

    /// Its two operands, for an operation this version defines.
    pub fn pair(&self) -> Option<[&str; 2]> {
        match self.operands.as_slice() {
            [a, b] => Some([a, b]),
            _ => None,
        }
    }
}

/// A derived channel as the footer's header states it: the declaration's entry, and the levels
/// of its overview that go in overview blocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedHeader {
    pub name: String,
    #[serde(default)]
    pub unit: String,
    pub op: Op,
    pub operands: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    #[serde(
        default,
        deserialize_with = "significant",
        skip_serializing_if = "Option::is_none"
    )]
    pub significant: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overview_blocks: Vec<OverviewLevel>,
}

impl DerivedHeader {
    pub fn declared(&self) -> Derived {
        Derived {
            name: self.name.clone(),
            unit: self.unit.clone(),
            op: self.op.clone(),
            operands: self.operands.clone(),
            params: self.params.clone(),
            significant: self.significant,
            requires: self.requires.clone(),
        }
    }
}

/// What a converter did to a channel that the instrument did not: FORMAT.md's moved and trimmed
/// channels. A channel as the source recorded it states neither, and its entry has no key for
/// them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Shaping {
    #[serde(flatten)]
    pub offset: Option<Offset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept: Option<Kept>,
}

impl Shaping {
    /// Whether the channel states neither: it is as the source recorded it.
    pub fn is_whole(&self) -> bool {
        self.offset.is_none() && self.kept.is_none()
    }
}

/// The ticks a channel was moved by, positive later, and why: `offset` and `offset_kind`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Offset {
    #[serde(rename = "offset")]
    pub ticks: i64,
    #[serde(rename = "offset_kind")]
    pub kind: String,
}

impl Offset {
    /// A known delay in the channel's path.
    pub const DELAY: &str = "delay";
}

/// The source ticks `from..to` where a channel holds what the source recorded, before its move:
/// the ticks its items are stored in. It reads
/// and writes as `[from, to]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "(i64, i64)", into = "(i64, i64)")]
pub struct Kept {
    pub from: i64,
    pub to: i64,
}

impl From<(i64, i64)> for Kept {
    fn from((from, to): (i64, i64)) -> Self {
        Self { from, to }
    }
}

impl From<Kept> for (i64, i64) {
    fn from(k: Kept) -> Self {
        (k.from, k.to)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DeclaredKind {
    Digital {
        /// The level at the channel's start before any edge there.
        initial: bool,
    },
    Analog {
        unit: String,
        /// Source ticks from one sample to the next, at least 1.
        period: Ratio,
        coding: AnalogCoding,
        /// At most this many significant figures in a reading.
        #[serde(skip_serializing_if = "Option::is_none")]
        significant: Option<u8>,
        /// The first run's place past its whole tick, a fraction of a tick; None for 0.
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<Ratio>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Header {
    /// Ticks a second.
    pub timebase: Ratio,
    /// Axis ticks from tick 0 to the capture's end.
    pub length: i64,
    pub channels: Vec<ChannelHeader>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived: Vec<DerivedHeader>,
    pub sources: Vec<SourceHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, String>,
    pub content_id: ContentId,
    /// The finest overview level's shift of a channel that states none.
    pub overview_shift: u8,
    /// Features a reader must know to read the recording at all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// The number of annotation parts, at least 1; None where there are none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotation_parts: Option<u64>,
}

/// One level of a channel's overview that goes in overview blocks: its shift, its blocks of 4,096
/// buckets, and the rows of its index, a run of alike blocks a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverviewLevel {
    pub shift: u8,
    pub blocks: u64,
    pub rows: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawChannel")]
pub struct ChannelHeader {
    pub name: String,
    pub source_name: String,
    /// Its source's index in `sources`.
    pub source: u32,
    #[serde(flatten)]
    pub data: ChannelKind,
    #[serde(flatten)]
    pub shaping: Shaping,
    /// Its finest overview level's shift, where it states one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview_shift: Option<u8>,
    /// Its blocks, which tile `0..length`.
    pub blocks: u64,
    /// The levels of its overview in overview blocks, finest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub overview_blocks: Vec<OverviewLevel>,
    /// As `DeclaredChannel::requires`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// As `DeclaredChannel::codec`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ChannelKind {
    Digital {
        /// The level at the channel's start before any edge there.
        initial: bool,
        /// A source tick: the level is known from the channel's start to here.
        end: i64,
        edges: u64,
        /// The gaps, when there are at most `INLINE_LIST` of them.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        gaps: Vec<Gap>,
        /// How many gaps the list pages hold, when there are more; `gaps` is then empty.
        #[serde(skip_serializing_if = "Option::is_none")]
        gap_count: Option<u64>,
    },
    Analog {
        unit: String,
        first: i64,
        /// Ticks from one sample to the next.
        period: Ratio,
        count: u64,
        /// The breaks, when there are at most `INLINE_LIST` of them.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        breaks: Vec<Break>,
        /// How many breaks the list pages hold, when there are more; `breaks` is then empty.
        #[serde(skip_serializing_if = "Option::is_none")]
        break_count: Option<u64>,
        coding: AnalogCoding,
        #[serde(skip_serializing_if = "Option::is_none")]
        significant: Option<u8>,
        /// The first run's place past `first`, a fraction of a tick; None for 0.
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<Ratio>,
        /// Samples the instrument lost before the first, 0 for none.
        #[serde(skip_serializing_if = "is_zero")]
        lost_before: u64,
        /// Samples the instrument lost after the last, 0 for none.
        #[serde(skip_serializing_if = "is_zero")]
        lost_after: u64,
    },
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// The most entries a gap or break list holds in the header; a longer one goes in list pages.
pub const INLINE_LIST: usize = 64;

impl ChannelKind {
    /// How many gaps or breaks the channel has, inline or paged.
    pub fn list_len(&self) -> u64 {
        match self {
            Self::Digital {
                gaps, gap_count, ..
            } => gap_count.unwrap_or(gaps.len() as u64),
            Self::Analog {
                breaks,
                break_count,
                ..
            } => break_count.unwrap_or(breaks.len() as u64),
        }
    }

    /// Whether its gaps or breaks are in list pages.
    pub fn paged(&self) -> bool {
        match self {
            Self::Digital { gap_count, .. } => gap_count.is_some(),
            Self::Analog { break_count, .. } => break_count.is_some(),
        }
    }
}

/// An exact ratio of signed integers with a positive denominator: an `int` coding's scale and
/// offset. It reads and writes as `[num, den]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "[i64; 2]", into = "[i64; 2]")]
pub struct Exact {
    num: i64,
    den: i64,
}

impl Exact {
    pub const ONE: Exact = Exact { num: 1, den: 1 };
    pub const ZERO: Exact = Exact { num: 0, den: 1 };

    /// None unless `den` is positive.
    pub const fn new(num: i64, den: i64) -> Option<Self> {
        if den > 0 {
            Some(Self { num, den })
        } else {
            None
        }
    }

    pub const fn num(self) -> i64 {
        self.num
    }

    /// Positive.
    pub const fn den(self) -> i64 {
        self.den
    }

    /// Whether it is in lowest terms, as the header holds every ratio: 0 only as `[0, 1]`.
    pub fn is_lowest(self) -> bool {
        crate::mapping::gcd(self.num.unsigned_abs(), self.den as u64) == 1
    }

    /// `x × num / den` in f64, each step rounded once: multiplied by the numerator, then divided
    /// by the denominator.
    #[inline]
    pub fn times(self, x: f64) -> f64 {
        (x * self.num as f64) / self.den as f64
    }
}

impl TryFrom<[i64; 2]> for Exact {
    type Error = String;

    fn try_from([num, den]: [i64; 2]) -> Result<Self, String> {
        Self::new(num, den)
            .ok_or_else(|| format!("[{num}, {den}]: the denominator is not positive"))
    }
}

impl From<Exact> for [i64; 2] {
    fn from(e: Exact) -> Self {
        [e.num, e.den]
    }
}

/// An `int` coding: integer samples, each a value exactly, `offset + code × scale`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct IntCoding {
    /// 1 to 64.
    pub bits: u8,
    pub signed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<Exact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<Exact>,
}

impl IntCoding {
    /// `bits` wide, with no scale or offset.
    pub const fn new(bits: u8, signed: bool) -> Self {
        Self {
            bits,
            signed,
            scale: None,
            offset: None,
        }
    }

    #[inline]
    pub fn scale(&self) -> Exact {
        self.scale.unwrap_or(Exact::ONE)
    }

    pub fn offset(&self) -> Exact {
        self.offset.unwrap_or(Exact::ZERO)
    }

    /// Whether `code` lies in the coding's range: `0..2^bits`, or `−2^(bits−1)..2^(bits−1)`.
    pub fn holds(&self, code: i128) -> bool {
        let bits = u32::from(self.bits);
        if self.signed {
            let half = 1i128 << (bits - 1);
            (-half..half).contains(&code)
        } else {
            (0..1i128 << bits).contains(&code)
        }
    }

    /// The bytes each code is stored in.
    pub fn planes(&self) -> usize {
        usize::from(self.bits).div_ceil(8)
    }

    /// A code's value in f64 (FORMAT.md, "Codings"): the code as an f64, times the scale's
    /// numerator, over its denominator, plus the offset's numerator over its denominator where
    /// there is one, each step rounded once. For an ADC's counts this is the f64 nearest the
    /// value: 3300 at a scale of 1/1000 is 3.3. The browser draws with these, and the f64 product
    /// of a derived channel multiplies them.
    #[inline]
    pub fn value(&self, code: i128) -> f64 {
        let scaled = self.scale().times(code as f64);
        match self.offset {
            Some(o) => scaled + o.num as f64 / o.den as f64,
            None => scaled,
        }
    }
}

/// How a block stores an analog channel's samples.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AnalogCoding {
    /// The source's f32 values, little-endian.
    F32,
    /// The source's f64 values, little-endian.
    F64,
    /// Integer codes in byte planes, each a value exactly.
    Int(IntCoding),
    /// Codes in byte planes, each one an f32 value: `(offset + code × scale) as f32`, which
    /// reproduces the source's value exactly. The converter only picks this after checking every
    /// sample.
    Codes { bits: u8, scale: f64, offset: f64 },
}

/// The value a code stands for: an f64 multiply, an f64 add, then rounding to the nearest f32. A
/// reader must compute exactly this: a fused multiply-add rounds once instead of twice and can give
/// another f32. Every reader and writer here goes through this one.
#[inline]
pub fn code_value(code: u16, scale: f64, offset: f64) -> f32 {
    (offset + f64::from(code) * scale) as f32
}

/// The number type an analog channel's samples are stored and summarised in: an `f32` or `codes`
/// channel's f32 values, an `f64` channel's values, an `int` channel's codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stored {
    F32,
    F64,
    U64,
    I64,
}

impl AnalogCoding {
    /// The bytes one sample takes in a block.
    pub fn sample_len(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F64 => 8,
            Self::Int(int) => int.planes(),
            Self::Codes { bits, .. } => usize::from(bits).div_ceil(8),
        }
    }

    /// Whether the samples are stored as codes in byte planes.
    pub fn planes(self) -> bool {
        matches!(self, Self::Int(_) | Self::Codes { .. })
    }

    pub fn stored(self) -> Stored {
        match self {
            Self::F32 | Self::Codes { .. } => Stored::F32,
            Self::F64 => Stored::F64,
            Self::Int(IntCoding { signed: false, .. }) => Stored::U64,
            Self::Int(IntCoding { signed: true, .. }) => Stored::I64,
        }
    }
}

/// A channel as the JSON holds it, in the header or the declaration, every kind's fields inline
/// and optional. Deriving
/// `Deserialize` for the flattened, internally tagged form instead makes serde buffer each channel
/// before parsing it, which costs the browser's decoder a large share of its size.
#[derive(Deserialize)]
struct RawChannel {
    name: String,
    source_name: String,
    source: u32,
    kind: RawKind,
    initial: Option<bool>,
    end: Option<i64>,
    edges: Option<u64>,
    gaps: Option<Vec<Gap>>,
    gap_count: Option<u64>,
    unit: Option<String>,
    first: Option<i64>,
    period: Option<Ratio>,
    count: Option<u64>,
    breaks: Option<Vec<Break>>,
    break_count: Option<u64>,
    coding: Option<RawCoding>,
    #[serde(default, deserialize_with = "significant")]
    significant: Option<u8>,
    lost_before: Option<u64>,
    lost_after: Option<u64>,
    offset: Option<i64>,
    offset_kind: Option<String>,
    kept: Option<Kept>,
    overview_shift: Option<u8>,
    blocks: Option<u64>,
    overview_blocks: Option<Vec<OverviewLevel>>,
    phase: Option<Ratio>,
    requires: Option<Vec<String>>,
    codec: Option<String>,
}

impl RawChannel {
    /// The channel's move and cut. An offset comes with its kind, which the content id pins with
    /// it, so a reader refuses either without the other.
    fn shaping(&mut self) -> Result<Shaping, String> {
        let offset = match (self.offset.take(), self.offset_kind.take()) {
            (Some(ticks), Some(kind)) => Some(Offset { ticks, kind }),
            (None, None) => None,
            (Some(_), None) => {
                return Err(format!(
                    "channel {} states an offset without an offset_kind",
                    self.name
                ));
            }
            (None, Some(_)) => {
                return Err(format!(
                    "channel {} states an offset_kind without an offset",
                    self.name
                ));
            }
        };
        Ok(Shaping {
            offset,
            kept: self.kept,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawKind {
    Digital,
    Analog,
}

#[derive(Deserialize)]
struct RawCoding {
    #[serde(rename = "type")]
    kind: RawCodingKind,
    bits: Option<u8>,
    signed: Option<bool>,
    scale: Option<RawNumber>,
    offset: Option<RawNumber>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawCodingKind {
    F32,
    F64,
    Int,
    Codes,
}

/// A coding's `scale` or `offset`: a number for `codes`, `[num, den]` for `int`. Read by hand:
/// an untagged enum would make serde buffer it, as `RawChannel` explains.
enum RawNumber {
    Float(f64),
    Ratio(i64, i64),
}

impl<'de> Deserialize<'de> for RawNumber {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visit;
        impl<'de> serde::de::Visitor<'de> for Visit {
            type Value = RawNumber;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a number, or [num, den]")
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<RawNumber, E> {
                Ok(RawNumber::Float(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<RawNumber, E> {
                Ok(RawNumber::Float(v as f64))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<RawNumber, E> {
                Ok(RawNumber::Float(v as f64))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut s: A,
            ) -> Result<RawNumber, A::Error> {
                let short = || serde::de::Error::custom("a ratio is [num, den]");
                let num = s.next_element()?.ok_or_else(short)?;
                let den = s.next_element()?.ok_or_else(short)?;
                if s.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(short());
                }
                Ok(RawNumber::Ratio(num, den))
            }
        }
        d.deserialize_any(Visit)
    }
}

impl RawNumber {
    fn float(self, field: &str) -> Result<f64, String> {
        match self {
            Self::Float(v) => Ok(v),
            Self::Ratio(..) => Err(format!("a codes coding's `{field}` is a number")),
        }
    }

    fn exact(self, field: &str) -> Result<Exact, String> {
        match self {
            Self::Ratio(num, den) => {
                Exact::try_from([num, den]).map_err(|e| format!("an int coding's `{field}` {e}"))
            }
            Self::Float(_) => Err(format!("an int coding's `{field}` is [num, den]")),
        }
    }
}

fn need<T>(value: Option<T>, field: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("missing field `{field}`"))
}

impl RawCoding {
    fn coding(self) -> Result<AnalogCoding, String> {
        Ok(match self.kind {
            RawCodingKind::F32 => AnalogCoding::F32,
            RawCodingKind::F64 => AnalogCoding::F64,
            RawCodingKind::Int => AnalogCoding::Int(IntCoding {
                bits: need(self.bits, "bits")?,
                signed: need(self.signed, "signed")?,
                scale: self.scale.map(|s| s.exact("scale")).transpose()?,
                offset: self.offset.map(|o| o.exact("offset")).transpose()?,
            }),
            RawCodingKind::Codes => AnalogCoding::Codes {
                bits: need(self.bits, "bits")?,
                scale: need(self.scale, "scale")?.float("scale")?,
                offset: need(self.offset, "offset")?.float("offset")?,
            },
        })
    }
}

/// A list stated inline or as a count of paged entries, never both: an inline list holds at most
/// `INLINE_LIST` entries, a paged one more, and a list of none states neither.
fn list<T>(
    name: &str,
    inline: Option<Vec<T>>,
    paged: Option<u64>,
    what: &str,
) -> Result<(Vec<T>, Option<u64>), String> {
    match (inline, paged) {
        (Some(_), Some(_)) => Err(format!(
            "channel {name} states its {what} both inline and as a count"
        )),
        (Some(v), None) if v.len() > INLINE_LIST => Err(format!(
            "channel {name} states {} {what} inline, more than the {INLINE_LIST} a header holds",
            v.len()
        )),
        (None, Some(n)) if n <= INLINE_LIST as u64 => Err(format!(
            "channel {name} states {n} {what} in list pages, which a header holds inline"
        )),
        (inline, paged) => Ok((inline.unwrap_or_default(), paged)),
    }
}

/// A loss at either end, which is at least 1 where it is stated.
fn loss(name: &str, value: Option<u64>, what: &str) -> Result<u64, String> {
    match value {
        Some(0) => Err(format!(
            "channel {name} states a {what} of 0; it states none"
        )),
        v => Ok(v.unwrap_or(0)),
    }
}

impl TryFrom<RawChannel> for ChannelHeader {
    type Error = String;

    fn try_from(mut raw: RawChannel) -> Result<Self, String> {
        let shaping = raw.shaping()?;
        let data = match raw.kind {
            RawKind::Digital => {
                let (gaps, gap_count) = list(&raw.name, raw.gaps, raw.gap_count, "gaps")?;
                ChannelKind::Digital {
                    initial: need(raw.initial, "initial")?,
                    end: need(raw.end, "end")?,
                    edges: need(raw.edges, "edges")?,
                    gaps,
                    gap_count,
                }
            }
            RawKind::Analog => {
                let (breaks, break_count) = list(&raw.name, raw.breaks, raw.break_count, "breaks")?;
                ChannelKind::Analog {
                    unit: need(raw.unit, "unit")?,
                    first: need(raw.first, "first")?,
                    period: need(raw.period, "period")?,
                    count: need(raw.count, "count")?,
                    breaks,
                    break_count,
                    coding: need(raw.coding, "coding")?.coding()?,
                    significant: raw.significant,
                    phase: raw.phase,
                    lost_before: loss(&raw.name, raw.lost_before, "lost_before")?,
                    lost_after: loss(&raw.name, raw.lost_after, "lost_after")?,
                }
            }
        };
        Ok(Self {
            name: raw.name,
            source_name: raw.source_name,
            source: raw.source,
            data,
            shaping,
            overview_shift: raw.overview_shift,
            blocks: need(raw.blocks, "blocks")?,
            overview_blocks: raw.overview_blocks.unwrap_or_default(),
            requires: raw.requires.unwrap_or_default(),
            codec: raw.codec,
        })
    }
}

impl TryFrom<RawChannel> for DeclaredChannel {
    type Error = String;

    fn try_from(mut raw: RawChannel) -> Result<Self, String> {
        let shaping = raw.shaping()?;
        let data = match raw.kind {
            RawKind::Digital => DeclaredKind::Digital {
                initial: need(raw.initial, "initial")?,
            },
            RawKind::Analog => DeclaredKind::Analog {
                unit: need(raw.unit, "unit")?,
                period: need(raw.period, "period")?,
                coding: need(raw.coding, "coding")?.coding()?,
                significant: raw.significant,
                phase: raw.phase,
            },
        };
        Ok(Self {
            name: raw.name,
            source_name: raw.source_name,
            source: raw.source,
            data,
            shaping,
            overview_shift: raw.overview_shift,
            requires: raw.requires.unwrap_or_default(),
            codec: raw.codec,
        })
    }
}

impl ChannelHeader {
    /// What the declaration said of this channel.
    pub fn declared(&self) -> DeclaredChannel {
        DeclaredChannel {
            name: self.name.clone(),
            source_name: self.source_name.clone(),
            source: self.source,
            data: match &self.data {
                ChannelKind::Digital { initial, .. } => DeclaredKind::Digital { initial: *initial },
                ChannelKind::Analog {
                    unit,
                    period,
                    coding,
                    significant,
                    phase,
                    ..
                } => DeclaredKind::Analog {
                    unit: unit.clone(),
                    period: *period,
                    coding: *coding,
                    significant: *significant,
                    phase: *phase,
                },
            },
            shaping: self.shaping.clone(),
            overview_shift: self.overview_shift,
            requires: self.requires.clone(),
            codec: self.codec.clone(),
        }
    }

    /// Whether this version can read the channel: it states no `requires` and no `codec`.
    pub fn available(&self) -> bool {
        self.requires.is_empty() && self.codec.is_none()
    }

    /// The analog channel's coding; None for a digital one.
    pub fn coding(&self) -> Option<AnalogCoding> {
        match &self.data {
            ChannelKind::Analog { coding, .. } => Some(*coding),
            ChannelKind::Digital { .. } => None,
        }
    }
}

impl DeclaredChannel {
    /// The analog channel's coding; None for a digital one.
    pub fn coding(&self) -> Option<AnalogCoding> {
        match &self.data {
            DeclaredKind::Analog { coding, .. } => Some(*coding),
            DeclaredKind::Digital { .. } => None,
        }
    }

    /// The analog channel's grid; None for a digital one, or one whose grid passes 2^16.
    pub fn grid(&self) -> Option<crate::model::Grid> {
        match &self.data {
            DeclaredKind::Analog { period, phase, .. } => crate::model::Grid::new(*period, *phase),
            DeclaredKind::Digital { .. } => None,
        }
    }

    /// Whether this version can read the channel: it states no `requires` and no `codec`.
    pub fn available(&self) -> bool {
        self.requires.is_empty() && self.codec.is_none()
    }
}

/// Source `s`'s `m` on an axis of `timebase`: how many axis ticks one of its ticks lasts, in
/// lowest terms. None for no such source, or terms past 2^53.
fn source_m(timebase: Ratio, sources: &[SourceHeader], s: usize) -> Option<Ratio> {
    let source = sources.get(s)?;
    crate::mapping::axis_ratio(
        source.stretch.unwrap_or(Ratio::ONE),
        timebase,
        source.timebase,
    )
}

fn channel_mapping(
    timebase: Ratio,
    sources: &[SourceHeader],
    source: u32,
    shaping: &Shaping,
) -> Option<Mapping> {
    let s = source as usize;
    Some(Mapping::new(
        sources.get(s)?.origin,
        shaping.offset.as_ref().map_or(0, |o| o.ticks),
        source_m(timebase, sources, s)?,
    ))
}

/// FORMAT.md, "Header": a channel's kept span's `from`, or the source tick at the axis's tick 0,
/// `-origin`, where it states none.
fn channel_start(sources: &[SourceHeader], source: u32, shaping: &Shaping) -> Option<i64> {
    match shaping.kept {
        Some(kept) => Some(kept.from),
        None => sources.get(source as usize)?.origin.checked_neg(),
    }
}

impl Declaration {
    /// Source `s`'s `m`: how many axis ticks one of its ticks lasts, in lowest terms. None for
    /// no such source, or terms past 2^53.
    pub fn m(&self, s: usize) -> Option<Ratio> {
        source_m(self.timebase, &self.sources, s)
    }

    /// Channel `c`'s mapping from its source's ticks to the axis. None for no such channel, or
    /// a source whose `m` has terms past 2^53; a checked declaration has neither.
    pub fn mapping(&self, c: usize) -> Option<Mapping> {
        let channel = self.channels.get(c)?;
        channel_mapping(
            self.timebase,
            &self.sources,
            channel.source,
            &channel.shaping,
        )
    }

    /// Every channel's mapping, in channel order; None as `mapping` gives it.
    pub fn mappings(&self) -> Option<Vec<Mapping>> {
        (0..self.channels.len()).map(|c| self.mapping(c)).collect()
    }

    /// Channel `c`'s start, in its source's ticks (FORMAT.md, "Header"): its kept span's `from`,
    /// or the source tick at the axis's tick 0, `-origin`, where it states none.
    pub fn start(&self, c: usize) -> Option<i64> {
        let channel = self.channels.get(c)?;
        channel_start(&self.sources, channel.source, &channel.shaping)
    }

    /// Overview channel `o`'s finest level's shift: a channel's own or the recording's, and a
    /// derived channel's operands' coarser (FORMAT.md, "Overview"). None past the last.
    pub fn shift(&self, o: usize) -> Option<u8> {
        let names = self.derived.iter().map(|d| d.operands.as_slice());
        shift_of(
            self.overview_shift,
            &self
                .channels
                .iter()
                .map(|c| (c.name.as_str(), c.overview_shift))
                .collect::<Vec<_>>(),
            names,
            o,
        )
    }

    /// Whether this version can read overview channel `o`: a channel that states no `requires`
    /// or `codec`, or a derived channel whose operation it knows over channels it can read.
    pub fn available(&self, o: usize) -> bool {
        available_of(
            &self
                .channels
                .iter()
                .map(|c| (c.name.as_str(), c.available()))
                .collect::<Vec<_>>(),
            self.derived.get(o.wrapping_sub(self.channels.len())),
            o,
        )
    }
}

/// Whether overview channel `o` is available, from each channel's name and availability and,
/// for a derived channel, its entry.
fn available_of(channels: &[(&str, bool)], derived: Option<&Derived>, o: usize) -> bool {
    if let Some(&(_, available)) = channels.get(o) {
        return available;
    }
    derived.is_some_and(|d| {
        d.known()
            && d.operands
                .iter()
                .all(|name| channels.iter().find(|c| c.0 == name).is_some_and(|c| c.1))
    })
}

/// Overview channel `o`'s finest shift, from each channel's name and stated shift and each
/// derived channel's operands by name: a derived channel takes the coarsest of its operands',
/// whatever its operation. None for no such channel, or a derived channel with no operand or
/// one that names no channel.
fn shift_of<'a>(
    recording: u8,
    channels: &[(&str, Option<u8>)],
    mut derived: impl Iterator<Item = &'a [String]>,
    o: usize,
) -> Option<u8> {
    if let Some(&(_, own)) = channels.get(o) {
        return Some(own.unwrap_or(recording));
    }
    let operands = derived.nth(o - channels.len())?;
    let of = |name: &String| {
        channels
            .iter()
            .find(|c| c.0 == name)
            .map(|c| c.1.unwrap_or(recording))
    };
    let mut shift: Option<u8> = None;
    for name in operands {
        let s = of(name)?;
        shift = Some(shift.map_or(s, |a| a.max(s)));
    }
    shift
}

impl Header {
    /// Source `s`'s `m`, as `Declaration::m`.
    pub fn m(&self, s: usize) -> Option<Ratio> {
        source_m(self.timebase, &self.sources, s)
    }

    /// Channel `c`'s mapping, as `Declaration::mapping`.
    pub fn mapping(&self, c: usize) -> Option<Mapping> {
        let channel = self.channels.get(c)?;
        channel_mapping(
            self.timebase,
            &self.sources,
            channel.source,
            &channel.shaping,
        )
    }

    /// Every channel's mapping, as `Declaration::mappings`.
    pub fn mappings(&self) -> Option<Vec<Mapping>> {
        (0..self.channels.len()).map(|c| self.mapping(c)).collect()
    }

    /// Channel `c`'s start, as `Declaration::start`.
    pub fn start(&self, c: usize) -> Option<i64> {
        let channel = self.channels.get(c)?;
        channel_start(&self.sources, channel.source, &channel.shaping)
    }

    /// Overview channel `o`'s finest shift, as `Declaration::shift`.
    pub fn shift(&self, o: usize) -> Option<u8> {
        shift_of(
            self.overview_shift,
            &self
                .channels
                .iter()
                .map(|c| (c.name.as_str(), c.overview_shift))
                .collect::<Vec<_>>(),
            self.derived.iter().map(|d| d.operands.as_slice()),
            o,
        )
    }

    /// Whether this version can read overview channel `o`, as `Declaration::available`.
    pub fn available(&self, o: usize) -> bool {
        let derived = self.derived.get(o.wrapping_sub(self.channels.len()));
        available_of(
            &self
                .channels
                .iter()
                .map(|c| (c.name.as_str(), c.available()))
                .collect::<Vec<_>>(),
            derived.map(DerivedHeader::declared).as_ref(),
            o,
        )
    }

    /// Overview channel `o`'s levels in overview blocks, as the header states them.
    pub fn levels(&self, o: usize) -> Option<&[OverviewLevel]> {
        match self.channels.get(o) {
            Some(c) => Some(&c.overview_blocks),
            None => Some(&self.derived.get(o - self.channels.len())?.overview_blocks),
        }
    }

    /// What the declaration said: the header's values without what only the end settles. A
    /// source's `extra` keeps the keys the end added, which the declaration may not have.
    pub fn declaration(&self) -> Declaration {
        Declaration {
            timebase: self.timebase,
            channels: self.channels.iter().map(ChannelHeader::declared).collect(),
            derived: self.derived.iter().map(DerivedHeader::declared).collect(),
            sources: self.sources.clone(),
            title: self.title.clone(),
            meta: self.meta.clone(),
            overview_shift: self.overview_shift,
            requires: self.requires.clone(),
        }
    }
}

/// Where a recording's channels came from: FORMAT.md's sources.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceHeader {
    pub format: SourceFormat,
    pub files: Vec<String>,
    /// The source's tick rate, any exact ratio. One of its ticks lasts `m` axis ticks.
    pub timebase: Ratio,
    /// The axis tick where the source's tick 0 lies, before any channel's offset.
    pub origin: i64,
    /// Axis ticks the source was moved by against the others, positive later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<i64>,
    /// How much the source's ticks were lengthened against its timebase, to line its clock up
    /// with the others': a claim, as `align` is. None is 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stretch: Option<Ratio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub software: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub begin_s: Option<f64>,
    /// The trigger's time in the source's own seconds, as `begin_s` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<RateOrigin>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
    /// The converter's name, version and arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub converter: Option<String>,
    /// The title of the recording a merge took the source from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The metadata of the recording a merge took the source from.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, String>,
}

impl SourceHeader {
    /// A source of `format` ticking at `timebase`, its tick 0 at the axis's, that states
    /// nothing else.
    pub fn new(format: SourceFormat, timebase: Ratio) -> Self {
        Self {
            format,
            files: Vec::new(),
            timebase,
            origin: 0,
            align: None,
            stretch: None,
            instrument: None,
            software: None,
            begin_s: None,
            trigger_s: None,
            rate: None,
            extra: BTreeMap::new(),
            converter: None,
            title: None,
            meta: BTreeMap::new(),
        }
    }
}

/// The kind of capture a recording was converted from. It reads and writes as its name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum SourceFormat {
    /// `saleae-binary-0`: Saleae Logic 2's binary export, version 0.
    SaleaeBinary0,
    /// `saleae-binary-1`: the same, version 1.
    SaleaeBinary1,
    /// `sigrok-sr`: a sigrok session file.
    SigrokSr,
    /// `vcd`: a value change dump.
    Vcd,
    /// A name this version doesn't know, as a newer converter may write.
    Other(String),
}

impl SourceFormat {
    pub fn as_str(&self) -> &str {
        match self {
            SourceFormat::SaleaeBinary0 => "saleae-binary-0",
            SourceFormat::SaleaeBinary1 => "saleae-binary-1",
            SourceFormat::SigrokSr => "sigrok-sr",
            SourceFormat::Vcd => "vcd",
            SourceFormat::Other(name) => name,
        }
    }
}

impl From<String> for SourceFormat {
    fn from(name: String) -> Self {
        match name.as_str() {
            "saleae-binary-0" => SourceFormat::SaleaeBinary0,
            "saleae-binary-1" => SourceFormat::SaleaeBinary1,
            "sigrok-sr" => SourceFormat::SigrokSr,
            "vcd" => SourceFormat::Vcd,
            _ => SourceFormat::Other(name),
        }
    }
}

impl From<SourceFormat> for String {
    fn from(format: SourceFormat) -> Self {
        match format {
            SourceFormat::Other(name) => name,
            known => known.as_str().to_owned(),
        }
    }
}

impl fmt::Display for SourceFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A recording's content id: `blake3:` and the hex BLAKE3 digest of its decoded content (see
/// `hash`). It reads and writes in that form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentId(blake3::Hash);

impl ContentId {
    pub fn new(digest: blake3::Hash) -> Self {
        Self(digest)
    }
}

impl FromStr for ContentId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        // One spelling: `from_hex` takes upper case too.
        s.strip_prefix("blake3:")
            .filter(|hex| hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .and_then(|hex| blake3::Hash::from_hex(hex).ok())
            .map(Self)
            .ok_or_else(|| {
                format!("{s:?} is not a content id: blake3: and 64 lower-case hex digits")
            })
    }
}

impl TryFrom<String> for ContentId {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        s.parse()
    }
}

impl From<ContentId> for String {
    fn from(id: ContentId) -> Self {
        id.to_string()
    }
}

impl fmt::Display for ContentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "blake3:{}", self.0.to_hex())
    }
}

/// How the converter came by the sample rate, where the source doesn't record it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RateOrigin {
    /// The lowest rate on whose grid all the source's edges sit.
    Inferred,
    /// Given to the converter.
    Given,
}

/// A block's row in its channel's index pages, 28 bytes: where it starts, where its frame is,
/// and the channel's word, as its block record has them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockRow {
    pub start: i64,
    /// Where the block's zstd frame starts in the file, inside its record.
    pub offset: u64,
    pub length: u32,
    pub decoded_length: u32,
    /// A digital channel's edge count with the level at the block's start in `LEVEL_BIT`, or an
    /// analog channel's sample count.
    pub word: u32,
}

impl BlockRow {
    pub const SIZE: usize = 28;

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.start.to_le_bytes());
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.length.to_le_bytes());
        out.extend_from_slice(&self.decoded_length.to_le_bytes());
        out.extend_from_slice(&self.word.to_le_bytes());
    }

    /// `bytes` is exactly one row.
    pub fn parse(bytes: &[u8]) -> Self {
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        Self {
            start: i64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            offset: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            length: u32_at(16),
            decoded_length: u32_at(20),
            word: u32_at(24),
        }
    }

    /// The items the block holds: a digital word keeps the level in its top bit.
    pub fn items(&self, digital: bool) -> u32 {
        if digital {
            self.word & !LEVEL_BIT
        } else {
            self.word
        }
    }
}

/// A row of one overview level's index, 20 bytes: an overview block record's frame, and `n`, the
/// blocks it stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverviewRow {
    pub offset: u64,
    pub length: u32,
    pub decoded_length: u32,
    pub n: u32,
}

impl OverviewRow {
    pub const SIZE: usize = 20;

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.length.to_le_bytes());
        out.extend_from_slice(&self.decoded_length.to_le_bytes());
        out.extend_from_slice(&self.n.to_le_bytes());
    }

    /// `bytes` is exactly one row.
    pub fn parse(bytes: &[u8]) -> Self {
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        Self {
            offset: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            length: u32_at(8),
            decoded_length: u32_at(12),
            n: u32_at(16),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<ChannelHeader, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn a_ratio_reads_and_writes_as_two_positive_integers() {
        let r = Ratio::new(3, 2).unwrap();
        assert_eq!(serde_json::to_string(&r).unwrap(), "[3,2]");
        assert_eq!(serde_json::from_str::<Ratio>("[3,2]").unwrap(), r);
        for bad in ["[0,1]", "[1,0]", "[1]", "[1,2,3]"] {
            assert!(serde_json::from_str::<Ratio>(bad).is_err(), "{bad}");
        }
        assert_eq!(Ratio::new(1, 0), None);
    }

    #[test]
    fn a_source_format_reads_and_writes_as_its_name_and_keeps_one_it_does_not_know() {
        for name in [
            "saleae-binary-0",
            "saleae-binary-1",
            "sigrok-sr",
            "vcd",
            "logic-99",
        ] {
            let json = format!("\"{name}\"");
            let format: SourceFormat = serde_json::from_str(&json).unwrap();
            assert_eq!(serde_json::to_string(&format).unwrap(), json);
        }
        let read = |json| serde_json::from_str::<SourceFormat>(json).unwrap();
        assert_eq!(read("\"vcd\""), SourceFormat::Vcd);
        assert_eq!(read("\"logic-99\""), SourceFormat::Other("logic-99".into()));
    }

    #[test]
    fn a_content_id_is_blake3_and_64_hex_digits() {
        let id = format!("blake3:{}", "0f".repeat(32));
        let parsed: ContentId = id.parse().unwrap();
        assert_eq!(parsed.to_string(), id);
        assert_eq!(serde_json::to_string(&parsed).unwrap(), format!("\"{id}\""));
        let hex = "0f".repeat(32);
        for bad in [
            "blake3:00ff".to_owned(),
            hex.clone(),
            format!("sha256:{hex}"),
            format!("blake3:{}", "zz".repeat(32)),
            format!("blake3:{}", "0F".repeat(32)),
        ] {
            assert!(bad.parse::<ContentId>().is_err(), "{bad}");
        }
    }

    #[test]
    fn channels_parse_as_the_encoder_writes_them() {
        let analog = |coding| ChannelKind::Analog {
            unit: "V".into(),
            first: 2,
            period: Ratio::new(3, 2).unwrap(),
            count: 9,
            breaks: vec![Break {
                index: 4,
                tick: 20,
                lost: 3,
                phase: 0,
            }],
            break_count: None,
            coding,
            significant: Some(4),
            lost_before: 2,
            lost_after: 0,
            phase: None,
        };
        for data in [
            ChannelKind::Digital {
                initial: true,
                end: 7,
                edges: 3,
                gaps: Vec::new(),
                gap_count: None,
            },
            ChannelKind::Digital {
                initial: false,
                end: 9,
                edges: 1,
                gaps: vec![Gap {
                    from: 0,
                    to: 4,
                    level: true,
                }],
                gap_count: None,
            },
            ChannelKind::Digital {
                initial: false,
                end: 9,
                edges: 1,
                gaps: Vec::new(),
                gap_count: Some(65),
            },
            analog(AnalogCoding::F32),
            analog(AnalogCoding::F64),
            analog(AnalogCoding::Codes {
                bits: 12,
                scale: 0.001,
                offset: -1.5,
            }),
            analog(AnalogCoding::Int(IntCoding::new(64, true))),
            analog(AnalogCoding::Int(IntCoding {
                bits: 29,
                signed: false,
                scale: Exact::new(-11, 13650),
                offset: Exact::new(1, 3),
            })),
        ] {
            for shaping in shapings() {
                let channel = ChannelHeader {
                    name: "a".into(),
                    source_name: "b".into(),
                    source: 2,
                    data: data.clone(),
                    shaping,
                    overview_shift: Some(9),
                    blocks: 3,
                    overview_blocks: vec![OverviewLevel {
                        shift: 9,
                        blocks: 2,
                        rows: 2,
                    }],
                    codec: None,
                    requires: Vec::new(),
                };
                let json = serde_json::to_string(&channel).unwrap();
                assert_eq!(parse(&json).unwrap(), channel, "{json}");
                let declared = channel.declared();
                let json = serde_json::to_string(&declared).unwrap();
                assert_eq!(
                    serde_json::from_str::<DeclaredChannel>(&json).unwrap(),
                    declared,
                    "{json}"
                );
            }
        }
    }

    /// None, each field alone, and both.
    fn shapings() -> [Shaping; 4] {
        let offset = Some(Offset {
            ticks: -40,
            kind: Offset::DELAY.into(),
        });
        let kept = Some(Kept { from: 3, to: 7 });
        [
            Shaping::default(),
            Shaping {
                offset: offset.clone(),
                kept: None,
            },
            Shaping { offset: None, kept },
            Shaping { offset, kept },
        ]
    }

    #[test]
    fn a_whole_channel_has_no_key_for_a_move_or_a_cut_and_a_shaped_one_has_each() {
        let channel = |shaping| ChannelHeader {
            name: "a".into(),
            source_name: "b".into(),
            source: 0,
            data: ChannelKind::Digital {
                initial: true,
                end: 7,
                edges: 3,
                gaps: Vec::new(),
                gap_count: None,
            },
            shaping,
            overview_shift: None,
            blocks: 1,
            overview_blocks: Vec::new(),
            codec: None,
            requires: Vec::new(),
        };
        let [whole, .., both] = shapings();
        assert_eq!(
            serde_json::to_string(&channel(whole)).unwrap(),
            r#"{"name":"a","source_name":"b","source":0,"kind":"digital","initial":true,"end":7,"edges":3,"blocks":1}"#
        );
        assert_eq!(
            serde_json::to_string(&channel(both).declared()).unwrap(),
            r#"{"name":"a","source_name":"b","source":0,"kind":"digital","initial":true,"offset":-40,"offset_kind":"delay","kept":[3,7]}"#
        );
    }

    #[test]
    fn an_offset_and_its_kind_come_together() {
        let named = r#""name":"SWCLK","source_name":"b","source":0,"kind":"digital","initial":true,"end":7,"edges":3,"blocks":1"#;
        for (fields, problem) in [
            (
                r#""offset":4"#,
                "channel SWCLK states an offset without an offset_kind",
            ),
            (
                r#""offset_kind":"delay""#,
                "channel SWCLK states an offset_kind without an offset",
            ),
        ] {
            let json = format!("{{{named},{fields}}}");
            let err = parse(&json).unwrap_err().to_string();
            assert!(err.contains(problem), "{json}: {err}");
            let err = serde_json::from_str::<DeclaredChannel>(&json)
                .unwrap_err()
                .to_string();
            assert!(err.contains(problem), "{json}: {err}");
        }
        // A kind this version doesn't know is kept.
        let json = format!(r#"{{{named},"offset":4,"offset_kind":"align"}}"#);
        assert_eq!(parse(&json).unwrap().shaping.offset.unwrap().kind, "align");
    }

    #[test]
    fn a_channel_missing_a_field_of_its_kind_is_refused() {
        let named = r#""name":"a","source_name":"b","source":0,"blocks":1"#;
        let analog = r#""kind":"analog","unit":"V","first":0,"period":[1,1],"count":1"#;
        for (json, problem) in [
            (
                format!(r#"{{{named},"kind":"digital","initial":true,"end":7}}"#),
                "missing field `edges`",
            ),
            (format!("{{{named},{analog}}}"), "missing field `coding`"),
            (
                format!(
                    r#"{{{named},{analog},"coding":{{"type":"codes","bits":12,"scale":1.0}}}}"#
                ),
                "missing field `offset`",
            ),
            (
                format!(r#"{{{named},"kind":"logic"}}"#),
                "unknown variant `logic`",
            ),
            (
                format!(r#"{{{named},{analog},"coding":{{"type":"int","bits":12}}}}"#),
                "missing field `signed`",
            ),
            (
                format!(
                    r#"{{{named},{analog},"coding":{{"type":"int","bits":12,"signed":false,"scale":0.5}}}}"#
                ),
                "an int coding's `scale` is [num, den]",
            ),
            (
                format!(
                    r#"{{{named},{analog},"coding":{{"type":"int","bits":12,"signed":false,"scale":[1,0]}}}}"#
                ),
                "the denominator is not positive",
            ),
            (
                format!(
                    r#"{{{named},{analog},"coding":{{"type":"codes","bits":12,"scale":[1,2],"offset":0}}}}"#
                ),
                "a codes coding's `scale` is a number",
            ),
            (
                r#"{"name":"a","source_name":"b","kind":"digital","initial":true,"end":7,"edges":3,"blocks":1}"#
                    .to_owned(),
                "missing field `source`",
            ),
            (
                format!(r#"{{{named},"kind":"digital","initial":true,"end":7,"edges":3,"gap_count":64}}"#),
                "states 64 gaps in list pages, which a header holds inline",
            ),
            (
                format!(
                    r#"{{{named},"kind":"digital","initial":true,"end":7,"edges":3,"gaps":[],"gap_count":65}}"#
                ),
                "states its gaps both inline and as a count",
            ),
            (
                format!(r#"{{{named},{analog},"coding":{{"type":"f32"}},"lost_after":0}}"#),
                "states a lost_after of 0",
            ),
            (
                r#"{"name":"a","source_name":"b","source":0,"kind":"digital","initial":true,"end":7,"edges":3}"#
                    .to_owned(),
                "missing field `blocks`",
            ),
        ] {
            let err = parse(&json).unwrap_err().to_string();
            assert!(err.contains(problem), "{json}: {err}");
        }
    }
}
