//! A recording's content, independent of how a file stores it. Readers produce it, the encoder
//! consumes it, and the decoder gives it back.
//!
//! Channel data are single-pass iterators so a reader can stream a capture larger than memory. A
//! reader must validate its source when it opens it: the iterators cannot fail. To read a source
//! twice (the converter verifies its output against the source), open it twice.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};

pub use crate::header::{
    Derived, Exact, IntCoding, Kept, Offset, RateOrigin, Shaping, SourceFormat,
};

/// An exact ratio of positive integers. It reads and writes as `[num, den]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "[NonZeroU64; 2]", into = "[NonZeroU64; 2]")]
pub struct Ratio {
    num: NonZeroU64,
    den: NonZeroU64,
}

impl Ratio {
    pub const ONE: Ratio = Ratio::whole(NonZeroU64::MIN);

    /// None when a term is zero.
    pub const fn new(num: u64, den: u64) -> Option<Self> {
        match (NonZeroU64::new(num), NonZeroU64::new(den)) {
            (Some(num), Some(den)) => Some(Self { num, den }),
            _ => None,
        }
    }

    /// `n`/1; None for zero.
    pub const fn integer(n: u64) -> Option<Self> {
        Self::new(n, 1)
    }

    /// `n`/1.
    pub const fn whole(n: NonZeroU64) -> Self {
        Self {
            num: n,
            den: NonZeroU64::MIN,
        }
    }

    pub const fn num(self) -> u64 {
        self.num.get()
    }

    pub const fn den(self) -> u64 {
        self.den.get()
    }
}

impl From<[NonZeroU64; 2]> for Ratio {
    fn from([num, den]: [NonZeroU64; 2]) -> Self {
        Self { num, den }
    }
}

impl From<Ratio> for [NonZeroU64; 2] {
    fn from(r: Ratio) -> Self {
        [r.num, r.den]
    }
}

/// A span of a digital channel where the level is not known: ticks `from..to`. From `to` on the
/// level is `level`. It reads and writes as `[from, to, level]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "(i64, i64, bool)", into = "(i64, i64, bool)")]
pub struct Gap {
    pub from: i64,
    pub to: i64,
    pub level: bool,
}

impl From<(i64, i64, bool)> for Gap {
    fn from((from, to, level): (i64, i64, bool)) -> Self {
        Self { from, to, level }
    }
}

impl From<Gap> for (i64, i64, bool) {
    fn from(g: Gap) -> Self {
        (g.from, g.to, g.level)
    }
}

/// Where an analog channel's samples start again after a stop: sample `index` sits at `tick`, and
/// the samples after it follow at the channel's period. The instrument lost `lost` samples
/// before it, 0 when it only paused. It reads and writes as `[index, tick, lost]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "(u64, i64, u64)", into = "(u64, i64, u64)")]
pub struct Break {
    pub index: u64,
    pub tick: i64,
    pub lost: u64,
}

impl From<(u64, i64, u64)> for Break {
    fn from((index, tick, lost): (u64, i64, u64)) -> Self {
        Self { index, tick, lost }
    }
}

impl From<Break> for (u64, i64, u64) {
    fn from(b: Break) -> Self {
        (b.index, b.tick, b.lost)
    }
}

#[derive(Debug)]
pub struct Recording {
    /// Ticks a second. One tick is the source's time resolution: every time in the recording is a
    /// whole number of ticks.
    pub timebase: Ratio,
    /// How many ticks the capture covers: ticks `0..length`, at least one.
    pub length: i64,
    pub channels: Vec<Channel>,
    /// Channels computed from the others, which a file does not store.
    pub derived: Vec<Derived>,
    /// Where the channels came from; each channel names one by its index.
    pub sources: Vec<Source>,
}

#[derive(Debug)]
pub struct Channel {
    /// The name shown and used by annotations.
    pub name: String,
    /// The source's own name for the channel: `digital_3_0`, a sigrok probe name, a VCD signal's
    /// full scope path.
    pub source_name: String,
    /// Its source's index in `sources`.
    pub source: u32,
    pub data: ChannelData,
    /// How the converter moved and cut the channel; nothing for a channel as the source has it.
    pub shaping: Shaping,
}

#[derive(Debug)]
pub enum ChannelData {
    Digital(Digital),
    Analog(Analog),
}

pub struct Digital {
    /// The level at tick 0, before any edge there.
    pub initial: bool,
    /// The level is known at ticks `0..end`, and `end` is at most the recording's length. A source
    /// can stop knowing one channel before another. At 0 the level is never known, and `initial`
    /// is false.
    pub end: i64,
    /// Ticks at which the level flips, in order, each in `0..end`. Two can share a tick: a pulse
    /// the source stamped with one time. The VCD reader collapses changes at one time to their net
    /// change, and the Saleae reader refuses edges out of order.
    pub edges: Edges,
    /// Spans where the level is not known, in order. No edge lies in a gap's `from..=to`, and a
    /// gap from tick 0 needs `initial` false.
    pub gaps: Vec<Gap>,
}

pub type Edges = Box<dyn Iterator<Item = i64> + Send>;

pub struct Analog {
    /// The unit of the values, as the source names it ("V", "mA"); empty when it names none.
    pub unit: String,
    /// The tick of the first sample.
    pub first: i64,
    /// Ticks from one sample to the next.
    pub period: Ratio,
    pub count: u64,
    /// Where the samples start again after a stop, in order, each index in `1..count`.
    pub breaks: Vec<Break>,
    /// At most this many significant figures in a reading, where the source says so.
    pub significant: Option<u8>,
    pub samples: Samples,
}

/// The tick of sample `index`: `first + floor(index × period)`.
pub fn sample_tick(first: i64, period: Ratio, index: u64) -> i64 {
    first + (u128::from(index) * u128::from(period.num()) / u128::from(period.den())) as i64
}

/// The tick of sample `index` of a channel with `breaks`: the run it falls in starts at its
/// break's tick, or at `first` before the first break.
pub fn run_sample_tick(first: i64, period: Ratio, breaks: &[Break], index: u64) -> i64 {
    match breaks.partition_point(|b| b.index <= index) {
        0 => sample_tick(first, period, index),
        n => {
            let b = breaks[n - 1];
            sample_tick(b.tick, period, index - b.index)
        }
    }
}

/// An analog channel's samples, in the type the source stored them.
pub enum Samples {
    /// The values exactly as the source stored them.
    F32(Box<dyn Iterator<Item = f32> + Send>),
    F64(Box<dyn Iterator<Item = f64> + Send>),
    /// Unsigned codes of an `int` coding, whose `signed` is false.
    U64(IntCoding, Box<dyn Iterator<Item = u64> + Send>),
    /// Signed codes of an `int` coding, whose `signed` is true.
    I64(IntCoding, Box<dyn Iterator<Item = i64> + Send>),
}

impl Samples {
    /// Each sample as a `Sample`.
    pub fn into_samples(self) -> Box<dyn Iterator<Item = Sample> + Send> {
        match self {
            Self::F32(s) => Box::new(s.map(Sample::F32)),
            Self::F64(s) => Box::new(s.map(Sample::F64)),
            Self::U64(_, s) => Box::new(s.map(Sample::U64)),
            Self::I64(_, s) => Box::new(s.map(Sample::I64)),
        }
    }

    /// The `int` coding the codes are in; None for floats.
    pub fn int_coding(&self) -> Option<IntCoding> {
        match self {
            Self::U64(c, _) | Self::I64(c, _) => Some(*c),
            Self::F32(_) | Self::F64(_) => None,
        }
    }

    /// The samples with `f` around each iterator, as a pass counts or cuts them.
    pub fn map<F: Wrap>(self, f: F) -> Self {
        match self {
            Self::F32(s) => Self::F32(f.wrap(s)),
            Self::F64(s) => Self::F64(f.wrap(s)),
            Self::U64(c, s) => Self::U64(c, f.wrap(s)),
            Self::I64(c, s) => Self::I64(c, f.wrap(s)),
        }
    }
}

/// Wraps a samples iterator whatever its item type, for `Samples::map`.
pub trait Wrap {
    fn wrap<T: Send + 'static>(
        self,
        samples: Box<dyn Iterator<Item = T> + Send>,
    ) -> Box<dyn Iterator<Item = T> + Send>;
}

/// One analog sample in the type its channel stores: an f32 for an `f32` or `codes` channel, an
/// f64 for an `f64` one, and a code for an `int` one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sample {
    F32(f32),
    F64(f64),
    U64(u64),
    I64(i64),
}

impl From<f32> for Sample {
    fn from(v: f32) -> Self {
        Self::F32(v)
    }
}

impl From<f64> for Sample {
    fn from(v: f64) -> Self {
        Self::F64(v)
    }
}

impl From<u64> for Sample {
    fn from(v: u64) -> Self {
        Self::U64(v)
    }
}

impl From<i64> for Sample {
    fn from(v: i64) -> Self {
        Self::I64(v)
    }
}

impl Sample {
    /// The type's name, for a message.
    pub fn type_name(self) -> &'static str {
        match self {
            Self::F32(_) => "f32",
            Self::F64(_) => "f64",
            Self::U64(_) => "u64",
            Self::I64(_) => "i64",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub format: SourceFormat,
    /// File names read, without directories.
    pub files: Vec<String>,
    /// The source's tick rate. The recording's timebase is a whole multiple of it.
    pub timebase: Ratio,
    /// The recording's tick where the source's tick 0 lies, before any channel's offset.
    pub origin: i64,
    /// Ticks of the recording the source was moved by to line it up with others.
    pub align: Option<i64>,
    pub instrument: Option<String>,
    pub software: Option<String>,
    /// The source's own time at its tick 0 in seconds, where it has one (Saleae's `begin`).
    /// Scripts that work from the source map their times to ticks with it.
    pub begin_s: Option<f64>,
    /// The trigger's time in the source's own seconds, as `begin_s` is.
    pub trigger_s: Option<f64>,
    /// How the sample rate was come by, where the source doesn't record it.
    pub rate: Option<RateOrigin>,
    /// Anything else the source states about the capture.
    pub extra: BTreeMap<String, String>,
    /// What converted it, where a recording read back says: an encoder states its own only for a
    /// source that has none.
    pub converter: Option<String>,
    /// The title of the recording a merge took the source from.
    pub title: Option<String>,
    /// The metadata of the recording a merge took the source from.
    pub meta: BTreeMap<String, String>,
}

impl Source {
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
            title: None,
            meta: BTreeMap::new(),
        }
    }
}

impl fmt::Debug for Digital {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Digital")
            .field("initial", &self.initial)
            .field("end", &self.end)
            .field("edges", &self.edges.size_hint())
            .field("gaps", &self.gaps)
            .finish()
    }
}

impl fmt::Debug for Analog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Analog")
            .field("unit", &self.unit)
            .field("first", &self.first)
            .field("period", &self.period)
            .field("count", &self.count)
            .field("breaks", &self.breaks)
            .field("significant", &self.significant)
            .finish_non_exhaustive()
    }
}
