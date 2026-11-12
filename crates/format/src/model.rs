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

pub use crate::header::{RateOrigin, SourceFormat};

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
/// the samples after it follow at the channel's period. It reads and writes as `[index, tick]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "(u64, i64)", into = "(u64, i64)")]
pub struct Break {
    pub index: u64,
    pub tick: i64,
}

impl From<(u64, i64)> for Break {
    fn from((index, tick): (u64, i64)) -> Self {
        Self { index, tick }
    }
}

impl From<Break> for (u64, i64) {
    fn from(b: Break) -> Self {
        (b.index, b.tick)
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
    pub source: Source,
}

#[derive(Debug)]
pub struct Channel {
    /// The name shown and used by annotations.
    pub name: String,
    /// The source's own name for the channel: `digital_3_0`, a sigrok probe name, a VCD signal's
    /// full scope path.
    pub source_name: String,
    pub data: ChannelData,
}

#[derive(Debug)]
pub enum ChannelData {
    Digital(Digital),
    Analog(Analog),
}

pub struct Digital {
    /// The level at tick 0.
    pub initial: bool,
    /// The level is known at ticks `0..end`, and `end` is at most the recording's length. A source
    /// can stop knowing one channel before another. At 0 the level is never known, and `initial`
    /// is false.
    pub end: i64,
    /// Ticks at which the level flips, in order, each in `1..end`. Two can share a tick: a pulse
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

pub enum Samples {
    /// The values exactly as the source stored them.
    F32(Box<dyn Iterator<Item = f32> + Send>),
}

#[derive(Clone, Debug)]
pub struct Source {
    pub format: SourceFormat,
    /// File names read, without directories.
    pub files: Vec<String>,
    pub instrument: Option<String>,
    pub software: Option<String>,
    /// The source's own time of tick 0 in seconds, where it has one (Saleae's `begin`). Scripts
    /// that work from the source map their times to ticks with it.
    pub begin_s: Option<f64>,
    /// The trigger's time in the source's own seconds, as `begin_s` is.
    pub trigger_s: Option<f64>,
    /// How the sample rate was come by, where the source doesn't record it.
    pub rate: Option<RateOrigin>,
    /// Anything else the source states about the capture.
    pub extra: BTreeMap<String, String>,
}

impl Source {
    /// A source of `format` that states nothing else.
    pub fn new(format: SourceFormat) -> Self {
        Self {
            format,
            files: Vec::new(),
            instrument: None,
            software: None,
            begin_s: None,
            trigger_s: None,
            rate: None,
            extra: BTreeMap::new(),
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
            .finish_non_exhaustive()
    }
}
