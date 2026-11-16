//! A recording's content, independent of how a file stores it. Readers produce it, the encoder
//! consumes it, and the decoder gives it back.
//!
//! Channel data are single-pass iterators so a reader can stream a capture larger than memory. A
//! reader must validate its source when it opens it: the iterators cannot fail. To read a source
//! twice (the converter verifies its output against the source), open it twice.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;
use std::ops::Range;

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

    /// Whether it is in lowest terms, as the header holds every ratio.
    pub fn is_lowest(self) -> bool {
        gcd(self.num(), self.den()) == 1
    }

    /// The same ratio in lowest terms.
    pub fn lowest(self) -> Self {
        let g = gcd(self.num(), self.den());
        Self::new(self.num() / g, self.den() / g).expect("positive terms")
    }

    /// Whether it is less than `other`, exactly.
    pub fn less_than(self, other: Ratio) -> bool {
        u128::from(self.num()) * u128::from(other.den())
            < u128::from(other.num()) * u128::from(self.den())
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

/// Where an analog channel's samples start again after a stop: sample `index` starts a run at
/// source tick `tick` plus `phase / D`, `D` the channel's grid (`Grid`), and the samples after it
/// follow at the channel's period. The instrument lost `lost` samples before it, 0 when it only
/// paused. It reads and writes as `[index, tick, lost]`, or `[index, tick, lost, phase]` where
/// the phase is not 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Break {
    pub index: u64,
    pub tick: i64,
    pub lost: u64,
    pub phase: u32,
}

impl Break {
    /// A run at a whole tick: phase 0.
    pub const fn new(index: u64, tick: i64, lost: u64) -> Self {
        Self {
            index,
            tick,
            lost,
            phase: 0,
        }
    }
}

impl From<(u64, i64, u64)> for Break {
    fn from((index, tick, lost): (u64, i64, u64)) -> Self {
        Self::new(index, tick, lost)
    }
}

impl Serialize for Break {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeTuple;
        let n = if self.phase == 0 { 3 } else { 4 };
        let mut t = s.serialize_tuple(n)?;
        t.serialize_element(&self.index)?;
        t.serialize_element(&self.tick)?;
        t.serialize_element(&self.lost)?;
        if self.phase != 0 {
            t.serialize_element(&self.phase)?;
        }
        t.end()
    }
}

impl<'de> Deserialize<'de> for Break {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visit;
        impl<'de> serde::de::Visitor<'de> for Visit {
            type Value = Break;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("[index, tick, lost] or [index, tick, lost, phase]")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut s: A) -> Result<Break, A::Error> {
                let short = || serde::de::Error::custom("a break is [index, tick, lost, phase?]");
                let index = s.next_element()?.ok_or_else(short)?;
                let tick = s.next_element()?.ok_or_else(short)?;
                let lost = s.next_element()?.ok_or_else(short)?;
                let phase = s.next_element()?.unwrap_or(0);
                if s.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(short());
                }
                Ok(Break {
                    index,
                    tick,
                    lost,
                    phase,
                })
            }
        }
        d.deserialize_seq(Visit)
    }
}

/// The most a channel's grid can be: `D` is below 2^16 (FORMAT.md, "Header").
pub const GRID_LIMIT: u64 = 1 << 16;

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// An analog channel's grid (FORMAT.md, "Header"): every place of its samples is a whole number
/// of `1/d` source ticks, where `d` is the least common multiple of its phase's and its period's
/// denominators in lowest terms. Places are counted in those units, exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    d: u64,
    /// The period, in `1/d` ticks.
    step: u128,
    /// The declared phase, the first run's, in `1/d` ticks.
    phase: u32,
}

impl Grid {
    /// The grid of a channel of `period` and declared `phase` (None for 0). None where `d`
    /// reaches 2^16, or the phase is not below one tick.
    pub fn new(period: Ratio, phase: Option<Ratio>) -> Option<Self> {
        let (pn, pd) = {
            let g = gcd(period.num(), period.den());
            (period.num() / g, period.den() / g)
        };
        let (fnum, fden) = match phase {
            None => (0, 1),
            Some(f) => {
                let g = gcd(f.num(), f.den());
                (f.num() / g, f.den() / g)
            }
        };
        if fnum >= fden {
            return None;
        }
        let d = (pd / gcd(pd, fden)).checked_mul(fden)?;
        if d >= GRID_LIMIT {
            return None;
        }
        Some(Self {
            d,
            step: u128::from(pn) * u128::from(d / pd),
            phase: (fnum * (d / fden)) as u32,
        })
    }

    /// A whole grid: places at whole ticks, a period of one tick.
    pub const WHOLE: Grid = Grid {
        d: 1,
        step: 1,
        phase: 0,
    };

    pub fn d(self) -> u64 {
        self.d
    }

    /// The period in `1/d` ticks.
    pub fn step(self) -> u128 {
        self.step
    }

    /// The declared phase, in `1/d` ticks.
    pub fn phase(self) -> u32 {
        self.phase
    }

    /// The channel's first run, the one at sample 0, starting at `first`.
    pub fn first_run(self, first: i64, lost_before: u64) -> Break {
        Break {
            index: 0,
            tick: first,
            lost: lost_before,
            phase: self.phase,
        }
    }

    /// Where `run` starts, in `1/d` ticks.
    pub fn start(self, run: Break) -> i128 {
        i128::from(run.tick) * i128::from(self.d) + i128::from(run.phase)
    }

    /// The place of sample `index` on the grid of `run`, in `1/d` ticks, exactly; saturating for
    /// an index no header allows. An index before the run's counts back from it.
    pub fn place(self, run: Break, index: u64) -> i128 {
        let steps =
            |n: u64| i128::try_from(u128::from(n).saturating_mul(self.step)).unwrap_or(i128::MAX);
        let start = self.start(run);
        if index >= run.index {
            start.saturating_add(steps(index - run.index))
        } else {
            start.saturating_sub(steps(run.index - index))
        }
    }

    /// The whole source tick at or before a place of `units / d` ticks, saturating.
    pub fn floor(self, units: i128) -> i64 {
        let t = units.div_euclid(i128::from(self.d));
        t.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    /// A place of `units / d` ticks as a whole tick and a phase below `d`.
    pub fn split(self, units: i128) -> (i64, u32) {
        (
            self.floor(units),
            units.rem_euclid(i128::from(self.d)) as u32,
        )
    }

    /// The run that holds sample `index` of a channel whose first run starts at `first` and
    /// whose later runs are `breaks`.
    pub fn run_of(self, first: i64, breaks: &[Break], index: u64) -> Break {
        match breaks.partition_point(|b| b.index <= index) {
            0 => self.first_run(first, 0),
            n => breaks[n - 1],
        }
    }

    /// The place of sample `index` of such a channel, in `1/d` ticks.
    pub fn sample_place(self, first: i64, breaks: &[Break], index: u64) -> i128 {
        self.place(self.run_of(first, breaks, index), index)
    }

    /// The time sample `index` of a channel of `count` samples covers (FORMAT.md, "Gaps"), in
    /// `1/d` ticks: from its place to the next place on its run's grid, or the next run's start
    /// if that comes sooner, and `cap` if that comes sooner still (a kept span's `to`, in `1/d`
    /// ticks). The source time the length maps back to, which needn't be a whole number of
    /// `1/d` ticks, is the caller's to cut at. None past the last sample.
    pub fn sample_time(
        self,
        first: i64,
        breaks: &[Break],
        count: u64,
        index: u64,
        cap: Option<i128>,
    ) -> Option<std::ops::Range<i128>> {
        if index >= count {
            return None;
        }
        let run = self.run_of(first, breaks, index);
        let from = self.place(run, index);
        let mut to = self.place(run, index + 1);
        if let Some(next) = breaks.get(breaks.partition_point(|b| b.index <= index))
            && next.index == index + 1
        {
            to = to.min(self.start(*next));
        }
        if let Some(cap) = cap {
            to = to.min(cap);
        }
        Some(from..to.max(from))
    }

    /// The time the samples lost before `breaks[k]` would have covered, in `1/d` ticks: from
    /// place `index` to place `index + lost` on the grid of the run before the break, ending at
    /// the break's start if that comes sooner. None for a pause, and for no break `k`.
    pub fn lost_time(
        self,
        first: i64,
        breaks: &[Break],
        k: usize,
    ) -> Option<std::ops::Range<i128>> {
        let b = *breaks.get(k)?;
        if b.lost == 0 {
            return None;
        }
        let run = match k {
            0 => self.first_run(first, 0),
            _ => breaks[k - 1],
        };
        let end = self.start(b);
        let from = self.place(run, b.index).min(end);
        Some(from..self.place(run, b.index.saturating_add(b.lost)).min(end))
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
    /// The level at the channel's start, before any edge there: its kept span's `from`, or the
    /// source tick at the axis's tick 0.
    pub initial: bool,
    /// A source tick: the level is known from the channel's start to `end`, which maps to the
    /// recording's length or before it. A source can stop knowing one channel before another. At
    /// the start the level is never known, and `initial` is the declaration's, which says nothing
    /// of the level.
    pub end: i64,
    /// Source ticks at which the level flips, in order, each in the channel's start to `end`. Two
    /// can share a tick: a pulse the source stamped with one time. The VCD reader collapses
    /// changes at one time to their net change, and the Saleae reader refuses edges out of order.
    pub edges: Edges,
    /// How many edges `edges` gives: the encoder takes the channel's density from it before it
    /// reads any, and refuses a channel that gives another number.
    pub count: u64,
    /// Spans where the level is not known, in source ticks and in order. No edge lies in a gap's
    /// `from..=to`, and a gap from the channel's start needs `initial` false.
    pub gaps: Vec<Gap>,
}

pub type Edges = Box<dyn Iterator<Item = i64> + Send>;

pub struct Analog {
    /// The unit of the values, as the source names it ("V", "mA"); empty when it names none.
    pub unit: String,
    /// The tick the first run starts at: its first sample sits at `first + phase`.
    pub first: i64,
    /// The first run's place past `first`, a fraction of a tick; None for 0 (FORMAT.md,
    /// "Header").
    pub phase: Option<Ratio>,
    /// Ticks from one sample to the next, at least 1.
    pub period: Ratio,
    pub count: u64,
    /// Where the samples start again after a stop, in order, each index in `1..count`.
    pub breaks: Vec<Break>,
    /// Samples the instrument lost before the first and after the last, 0 for none
    /// (FORMAT.md, "Gaps").
    pub lost_before: u64,
    pub lost_after: u64,
    /// At most this many significant figures in a reading, where the source says so.
    pub significant: Option<u8>,
    pub samples: Samples,
}

/// The source tick of sample `index`: `first + floor(index × period)`, saturating for an index
/// past any sample a header allows.
pub fn sample_tick(first: i64, period: Ratio, index: u64) -> i64 {
    let steps = u128::from(index) * u128::from(period.num()) / u128::from(period.den());
    first.saturating_add(i64::try_from(steps).unwrap_or(i64::MAX))
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

/// Place `index` on the grid of the run that starts at sample `start`, at `tick`, or `cap` if
/// that comes sooner. The grid goes on past the run's last sample.
fn place_until(tick: i64, start: u64, period: Ratio, index: u64, cap: i64) -> i64 {
    let steps = u128::from(index - start) * u128::from(period.num()) / u128::from(period.den());
    let steps = i128::try_from(steps).unwrap_or(i128::MAX);
    i128::from(tick).saturating_add(steps).min(i128::from(cap)) as i64
}

/// The run that holds sample `index`: its first sample's index and its tick.
fn run_of(first: i64, breaks: &[Break], index: u64) -> (u64, i64) {
    match breaks.partition_point(|b| b.index <= index) {
        0 => (0, first),
        n => (breaks[n - 1].index, breaks[n - 1].tick),
    }
}

/// The ticks sample `index` of a channel of `count` samples covers (FORMAT.md, "Gaps"): from its
/// place on its run's grid to the next place, which inside a run is the next sample's tick. A
/// run's last sample ends sooner at the next run's tick, and no support runs past `length`. None
/// for an index past the last sample.
pub fn sample_support(
    first: i64,
    period: Ratio,
    breaks: &[Break],
    count: u64,
    length: i64,
    index: u64,
) -> Option<Range<i64>> {
    if index >= count {
        return None;
    }
    let (start, tick) = run_of(first, breaks, index);
    let next_run = breaks
        .get(breaks.partition_point(|b| b.index <= index))
        .filter(|b| b.index == index + 1);
    let cap = next_run.map_or(length, |b| b.tick.min(length));
    let from = place_until(tick, start, period, index, length);
    Some(from..place_until(tick, start, period, index + 1, cap))
}

/// The ticks the samples lost before `breaks[k]` would have covered (FORMAT.md, "Gaps"): from
/// place `index` to place `index + lost` on the grid of the run before the break, ending at the
/// break's tick if that comes sooner. None for a pause, which loses nothing, and for no break `k`.
pub fn lost_span(first: i64, period: Ratio, breaks: &[Break], k: usize) -> Option<Range<i64>> {
    let b = *breaks.get(k)?;
    if b.lost == 0 {
        return None;
    }
    let (start, tick) = match k {
        0 => (0, first),
        _ => (breaks[k - 1].index, breaks[k - 1].tick),
    };
    let place = |index| place_until(tick, start, period, index, b.tick);
    Some(place(b.index)..place(b.index.saturating_add(b.lost)))
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
    /// The source's tick rate, any exact ratio. One of its ticks lasts `m` axis ticks.
    pub timebase: Ratio,
    /// The axis tick where the source's tick 0 lies, before any channel's offset.
    pub origin: i64,
    /// Axis ticks the source was moved by to line it up with others.
    pub align: Option<i64>,
    /// How much the source's ticks were lengthened to line its clock up with others'; None is 1.
    pub stretch: Option<Ratio>,
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

impl fmt::Debug for Digital {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Digital")
            .field("initial", &self.initial)
            .field("end", &self.end)
            .field("count", &self.count)
            .field("gaps", &self.gaps)
            .finish()
    }
}

impl fmt::Debug for Analog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Analog")
            .field("unit", &self.unit)
            .field("first", &self.first)
            .field("phase", &self.phase)
            .field("period", &self.period)
            .field("count", &self.count)
            .field("breaks", &self.breaks)
            .field("lost_before", &self.lost_before)
            .field("lost_after", &self.lost_after)
            .field("significant", &self.significant)
            .finish_non_exhaustive()
    }
}
