//! What a file says about itself: the preamble, the trailer, the declaration that opens the
//! records, the header the footer holds, and the index's rows.
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

use crate::model::{Break, Gap, Ratio};

pub const MAGIC: [u8; 8] = *b"\x89TKW\r\n\x1a\n";
pub const TRAILER_MAGIC: [u8; 8] = *b"\x89TKWEND\n";
pub const MAJOR: u16 = 1;
pub const MINOR: u16 = 0;
pub const PREAMBLE_LEN: usize = 12;
pub const TRAILER_LEN: usize = 24;
pub const ROW_FIXED: usize = 24;
pub const LEVEL_BIT: u32 = 1 << 31;

/// Every integer in the header stays below this, so a JavaScript reader parses it exactly.
pub const JSON_SAFE: u64 = 1 << 53;

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
}

/// A channel computed from two analog channels, which blocks do not store: FORMAT.md's derived
/// channels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Derived {
    pub name: String,
    pub unit: String,
    /// The two analog channels it is the product of, by name.
    pub product: [String; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub significant: Option<u8>,
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

/// The ticks `from..to` where a channel holds what the source recorded, after its move. It reads
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
        /// The level at tick 0 before any edge there.
        initial: bool,
    },
    Analog {
        unit: String,
        period: Ratio,
        coding: AnalogCoding,
        /// At most this many significant figures in a reading.
        #[serde(skip_serializing_if = "Option::is_none")]
        significant: Option<u8>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Header {
    /// Ticks a second.
    pub timebase: Ratio,
    /// Ticks from tick 0 to the capture's end.
    pub length: i64,
    pub channels: Vec<ChannelHeader>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived: Vec<Derived>,
    pub sources: Vec<SourceHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, String>,
    pub content_id: ContentId,
    pub blocks: u32,
    /// The finest overview level's shift, as the declaration gave it.
    pub overview_shift: u8,
    /// The overview levels in the footer, as bucket widths of 2^shift ticks, finest first.
    pub overview: Vec<u8>,
    /// The finer overview levels kept in overview blocks, finest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overview_blocks: Vec<OverviewLevel>,
    /// The sidecar's files by name, the hand-written TOML file and what it includes; in a
    /// bundled file only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverviewLevel {
    pub shift: u8,
    pub blocks: u32,
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
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ChannelKind {
    Digital {
        /// The level at tick 0 before any edge there.
        initial: bool,
        end: i64,
        edges: u64,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        gaps: Vec<Gap>,
    },
    Analog {
        unit: String,
        first: i64,
        /// Ticks from one sample to the next.
        period: Ratio,
        count: u64,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        breaks: Vec<Break>,
        coding: AnalogCoding,
        #[serde(skip_serializing_if = "Option::is_none")]
        significant: Option<u8>,
    },
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

    /// `x × num / den` in f64, each step rounded once: multiplied by the numerator, then divided
    /// by the denominator.
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
    unit: Option<String>,
    first: Option<i64>,
    period: Option<Ratio>,
    count: Option<u64>,
    breaks: Option<Vec<Break>>,
    coding: Option<RawCoding>,
    significant: Option<u8>,
    offset: Option<i64>,
    offset_kind: Option<String>,
    kept: Option<Kept>,
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

impl TryFrom<RawChannel> for ChannelHeader {
    type Error = String;

    fn try_from(mut raw: RawChannel) -> Result<Self, String> {
        let shaping = raw.shaping()?;
        let data = match raw.kind {
            RawKind::Digital => ChannelKind::Digital {
                initial: need(raw.initial, "initial")?,
                end: need(raw.end, "end")?,
                edges: need(raw.edges, "edges")?,
                gaps: raw.gaps.unwrap_or_default(),
            },
            RawKind::Analog => ChannelKind::Analog {
                unit: need(raw.unit, "unit")?,
                first: need(raw.first, "first")?,
                period: need(raw.period, "period")?,
                count: need(raw.count, "count")?,
                breaks: raw.breaks.unwrap_or_default(),
                coding: need(raw.coding, "coding")?.coding()?,
                significant: raw.significant,
            },
        };
        Ok(Self {
            name: raw.name,
            source_name: raw.source_name,
            source: raw.source,
            data,
            shaping,
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
            },
        };
        Ok(Self {
            name: raw.name,
            source_name: raw.source_name,
            source: raw.source,
            data,
            shaping,
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
                    ..
                } => DeclaredKind::Analog {
                    unit: unit.clone(),
                    period: *period,
                    coding: *coding,
                    significant: *significant,
                },
            },
            shaping: self.shaping.clone(),
        }
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
}

impl Header {
    /// What the declaration said: the header's values without what only the end settles. A
    /// source's `extra` keeps the keys the end added, which the declaration may not have.
    pub fn declaration(&self) -> Declaration {
        Declaration {
            timebase: self.timebase,
            channels: self.channels.iter().map(ChannelHeader::declared).collect(),
            derived: self.derived.clone(),
            sources: self.sources.clone(),
            title: self.title.clone(),
            meta: self.meta.clone(),
            overview_shift: self.overview_shift,
        }
    }
}

/// Where a recording's channels came from: FORMAT.md's sources.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceHeader {
    pub format: SourceFormat,
    pub files: Vec<String>,
    /// The source's tick rate: the recording's timebase is a whole multiple of it.
    pub timebase: Ratio,
    /// The recording's tick where the source's tick 0 lies, before any channel's offset.
    pub origin: i64,
    /// Ticks of the recording the source was moved by against the others, positive later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<i64>,
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
}

impl SourceHeader {
    /// A source of `format` ticking at `timebase`, its tick 0 at the recording's, that states
    /// nothing else.
    pub fn new(format: SourceFormat, timebase: Ratio) -> Self {
        Self {
            format,
            files: Vec::new(),
            timebase,
            origin: 0,
            align: None,
            instrument: None,
            software: None,
            begin_s: None,
            trigger_s: None,
            rate: None,
            extra: BTreeMap::new(),
            converter: None,
        }
    }
}

/// How many of the recording's ticks one of a source's ticks is: `recording / source`, None
/// unless it is a whole number. A checked header's factors lie below 2^53.
pub fn factor(recording: Ratio, source: Ratio) -> Option<u128> {
    let num = u128::from(recording.num()) * u128::from(source.den());
    let den = u128::from(recording.den()) * u128::from(source.num());
    num.is_multiple_of(den).then(|| num / den)
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
        s.strip_prefix("blake3:")
            .and_then(|hex| blake3::Hash::from_hex(hex).ok())
            .map(Self)
            .ok_or_else(|| format!("{s:?} is not a content id: blake3: and 64 hex digits"))
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockRow {
    pub start: i64,
    /// Where the block's zstd frame starts in the file, inside its record.
    pub offset: u64,
    pub length: u32,
    pub decoded_length: u32,
    /// A word a channel: digital, the edge count with the level at the block's start in
    /// `LEVEL_BIT`; analog, the sample count.
    pub channels: Vec<u32>,
}

impl BlockRow {
    pub fn size(channels: usize) -> usize {
        ROW_FIXED + 4 * channels
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.start.to_le_bytes());
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.length.to_le_bytes());
        out.extend_from_slice(&self.decoded_length.to_le_bytes());
        for word in &self.channels {
            out.extend_from_slice(&word.to_le_bytes());
        }
    }

    /// `bytes` is exactly one row.
    pub fn parse(bytes: &[u8], channels: usize) -> Self {
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        Self {
            start: i64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            offset: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            length: u32_at(16),
            decoded_length: u32_at(20),
            channels: (0..channels).map(|c| u32_at(ROW_FIXED + 4 * c)).collect(),
        }
    }
}

/// An overview block's place in the file. The index holds these after the data blocks' rows, by
/// level in the header's order, then in time order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverviewRow {
    pub offset: u64,
    pub length: u32,
    pub decoded_length: u32,
}

impl OverviewRow {
    pub const SIZE: usize = 16;

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.length.to_le_bytes());
        out.extend_from_slice(&self.decoded_length.to_le_bytes());
    }

    /// `bytes` is exactly one row.
    pub fn parse(bytes: &[u8]) -> Self {
        Self {
            offset: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            length: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            decoded_length: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
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
            }],
            coding,
            significant: Some(4),
        };
        for data in [
            ChannelKind::Digital {
                initial: true,
                end: 7,
                edges: 3,
                gaps: Vec::new(),
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
            },
            shaping,
        };
        let [whole, .., both] = shapings();
        assert_eq!(
            serde_json::to_string(&channel(whole)).unwrap(),
            r#"{"name":"a","source_name":"b","source":0,"kind":"digital","initial":true,"end":7,"edges":3}"#
        );
        assert_eq!(
            serde_json::to_string(&channel(both).declared()).unwrap(),
            r#"{"name":"a","source_name":"b","source":0,"kind":"digital","initial":true,"offset":-40,"offset_kind":"delay","kept":[3,7]}"#
        );
    }

    #[test]
    fn an_offset_and_its_kind_come_together() {
        let named = r#""name":"SWCLK","source_name":"b","source":0,"kind":"digital","initial":true,"end":7,"edges":3"#;
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
        let named = r#""name":"a","source_name":"b","source":0"#;
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
                r#"{"name":"a","source_name":"b","kind":"digital","initial":true,"end":7,"edges":3}"#
                    .to_owned(),
                "missing field `source`",
            ),
        ] {
            let err = parse(&json).unwrap_err().to_string();
            assert!(err.contains(problem), "{json}: {err}");
        }
    }
}
