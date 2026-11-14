//! Where an analog channel's samples sit, and the sample at or around a tick: web/src/source.ts's
//! index helpers, which the columns and the probe read. A tick here is an f64, as the web's is,
//! since a pixel's edge falls between ticks. Every tick these take and give is an axis tick; the
//! fields of `Runs` are the channel's source's ticks, which `map` places on the axis.

use std::ops::Range;

use escapement_tickwave_format::mapping::Mapping;
use escapement_tickwave_format::model::{
    Break, Ratio, lost_span, run_sample_tick, sample_support, sample_tick,
};

/// An analog channel's samples' places: its first tick, period, count and breaks, in its source's
/// ticks, and where those lie on the axis.
#[derive(Clone, Copy, Debug)]
pub struct Runs<'a> {
    /// The first sample's source tick.
    pub first: i64,
    /// Source ticks a sample.
    pub period: Ratio,
    /// `u64::MAX` for a tile whose channel's count doesn't matter.
    pub count: u64,
    /// Each break's tick is a source tick.
    pub breaks: &'a [Break],
    pub map: Mapping,
}

impl Runs<'_> {
    /// Source tick `k` on the axis.
    #[inline]
    pub fn axis(&self, k: i64) -> i64 {
        self.map.axis_saturating(k)
    }

    /// The axis tick of sample `i`, its breaks included.
    pub fn tick(&self, i: u64) -> i64 {
        self.axis(self.source_tick(i))
    }

    /// The source tick of sample `i`, its breaks included.
    pub fn source_tick(&self, i: u64) -> i64 {
        run_sample_tick(self.first, self.period, self.breaks, i)
    }

    /// The first sample's axis tick.
    pub fn start(&self) -> i64 {
        self.axis(self.first)
    }

    /// Break `k`'s axis tick: where run `k + 1` starts.
    pub fn break_tick(&self, k: usize) -> i64 {
        self.axis(self.breaks[k].tick)
    }

    /// The ticks sample `i` covers on the axis (`model::sample_support`), none past `length`.
    pub fn support(&self, i: u64, length: i64) -> Option<Range<i64>> {
        let s = sample_support(
            self.first,
            self.period,
            self.breaks,
            self.count,
            i64::MAX,
            i,
        )?;
        Some(self.axis(s.start).min(length)..self.axis(s.end).min(length))
    }

    /// The least source tick whose axis tick is at or after axis tick `tick`. For the identity the
    /// tick itself, a fraction included.
    fn source_at_or_after(&self, tick: f64) -> f64 {
        if self.map.is_identity() || tick.is_nan() {
            return tick;
        }
        self.map.first_at(tick.ceil() as i64) as f64
    }

    /// The greatest source tick whose axis tick is at or before axis tick `tick`, for the
    /// identity the tick itself.
    fn source_at_or_before(&self, tick: f64) -> f64 {
        if self.map.is_identity() || tick.is_nan() {
            return tick;
        }
        let next = (tick.floor() as i64).saturating_add(1);
        self.map.first_at(next).saturating_sub(1) as f64
    }
}

/// The index of the first sample whose tick is at or after `tick`, clamped to `0..=count`, for
/// samples from `first` every `period`, in one tick's units.
pub fn sample_index_at_or_after(first: i64, period: Ratio, count: u64, tick: f64) -> u64 {
    let estimate = (((tick - first as f64) * period.den() as f64) / period.num() as f64).ceil();
    // The estimate can land one off either way; settle it against the sample ticks themselves.
    let mut i = if estimate > 0.0 {
        if estimate >= count as f64 {
            count
        } else {
            estimate as u64
        }
    } else {
        0
    };
    while i > 0 && sample_tick(first, period, i - 1) as f64 >= tick {
        i -= 1;
    }
    while i < count && (sample_tick(first, period, i) as f64) < tick {
        i += 1;
    }
    i
}

/// The index of the first sample at or after axis tick `tick`, breaks included, clamped to
/// `0..=count`.
pub fn index_at_or_after(runs: &Runs, tick: f64) -> u64 {
    index_at_or_after_source(runs, runs.source_at_or_after(tick))
}

/// `index_at_or_after` for a source tick.
fn index_at_or_after_source(runs: &Runs, tick: f64) -> u64 {
    let b = runs.breaks;
    // The run whose start is the last at or before `tick`; its samples end where the next run's
    // start. Break ticks never fall, so those at or before `tick` come first. A NaN tick is past
    // every break, as the web's walk back from the last leaves it.
    let k = if tick.is_nan() {
        b.len()
    } else {
        b.partition_point(|x| x.tick as f64 <= tick)
    };
    let from = if k == 0 { 0 } else { b[k - 1].index };
    let to = if k < b.len() { b[k].index } else { runs.count };
    let start = if k == 0 { runs.first } else { b[k - 1].tick };
    from + sample_index_at_or_after(start, runs.period, to - from, tick)
}

/// The index of the last sample at or before axis tick `tick`: the value a cursor there reads.
/// None before the first sample or at or past `end`, the end of the channel's data, an axis tick.
pub fn sample_at_or_before(runs: &Runs, end: i64, tick: f64) -> Option<u64> {
    if tick < runs.start() as f64 || tick >= end as f64 || runs.count == 0 {
        return None;
    }
    let tick = runs.source_at_or_before(tick);
    if !runs.breaks.is_empty() {
        let i = index_at_or_after_source(runs, tick + 1.0).checked_sub(1)?;
        return Some(i.min(runs.count - 1));
    }
    let (first, period) = (runs.first, runs.period);
    let estimate = (((tick - first as f64) * period.den() as f64) / period.num() as f64).floor();
    let mut i = if estimate > 0.0 { estimate as u64 } else { 0 };
    // The division can land one off; settle it against the sample ticks themselves.
    while i > 0 && sample_tick(first, period, i) as f64 > tick {
        i -= 1;
    }
    while i + 1 < runs.count && sample_tick(first, period, i + 1) as f64 <= tick {
        i += 1;
    }
    Some(i.min(runs.count - 1))
}

/// The axis tick of the place after sample `i` on its run's grid: where its time ends, unless the
/// next run or the length comes sooner (FORMAT.md, "Gaps").
pub fn place_after(runs: &Runs, i: u64) -> i64 {
    let place = match runs.breaks.partition_point(|b| b.index <= i) {
        0 => sample_tick(runs.first, runs.period, i + 1),
        k => {
            let b = runs.breaks[k - 1];
            sample_tick(b.tick, runs.period, i + 1 - b.index)
        }
    };
    runs.axis(place)
}

/// Where a break's lost samples would have sat, in axis ticks, and how many there were.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LostSpan {
    pub from: i64,
    pub to: i64,
    pub lost: u64,
}

/// The span each break's lost samples cover (FORMAT.md, "Gaps"): from place `index` to place
/// `index + lost` on the grid of the run before the break, ending at the break's tick if that
/// comes sooner. A pause has none.
pub fn lost_spans(runs: &Runs) -> Vec<LostSpan> {
    lost_spans_in(runs, i64::MIN..i64::MAX)
}

/// `lost_spans` that reach into `range`, axis ticks. A break's lost samples end by its tick and
/// start after the run before it starts, so only the breaks from the first past `range.start` to
/// the first whose run starts at `range.end` are read.
pub fn lost_spans_in(runs: &Runs, range: Range<i64>) -> Vec<LostSpan> {
    let first = runs
        .breaks
        .partition_point(|b| runs.axis(b.tick) <= range.start);
    let mut out = Vec::new();
    for k in first..runs.breaks.len() {
        let run_start = k.checked_sub(1).map_or(runs.first, |p| runs.breaks[p].tick);
        if runs.axis(run_start) >= range.end {
            break;
        }
        let Some(span) = lost_span(runs.first, runs.period, runs.breaks, k) else {
            continue;
        };
        let (from, to) = (runs.axis(span.start), runs.axis(span.end));
        if to > range.start && from < range.end {
            out.push(LostSpan {
                from,
                to,
                lost: runs.breaks[k].lost,
            });
        }
    }
    out
}
