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

#[derive(Debug)]
pub struct Recording {
    /// Ticks a second. One tick is the source's time resolution: every time in the recording is a
    /// whole number of ticks.
    pub timebase: Ratio,
    /// Ticks from the capture's start, tick 0, to its end.
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
    /// The tick up to which the level is known, at most the recording's length. A source can stop
    /// knowing one channel before another.
    pub end: i64,
    /// Ticks at which the level flips, strictly increasing, each in `1..=end`. Two changes within
    /// one tick cannot be represented: a source reader collapses them to their net change (VCD) or
    /// refuses the source (Saleae).
    pub edges: Edges,
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
    pub samples: Samples,
}

/// The tick of sample `index`: `first + floor(index × period)`.
pub fn sample_tick(first: i64, period: Ratio, index: u64) -> i64 {
    first + (u128::from(index) * u128::from(period.num()) / u128::from(period.den())) as i64
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
            .finish_non_exhaustive()
    }
}
