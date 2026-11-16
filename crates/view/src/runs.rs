//! Where an analog channel's samples sit, and the sample at or around a tick: web/src/source.ts's
//! index helpers, which the columns and the probe read. A tick here is an f64, as the web's is,
//! since a pixel's edge falls between ticks. Every tick these take and give is an axis tick: a
//! sample's place, a whole number of its grid's `1/d` source ticks, lies on the axis at the tick
//! it maps to (FORMAT.md, "Header"). The fields of `Runs` are the channel's source's.

use std::ops::Range;

use escapement_tickwave_format::mapping::Mapping;
use escapement_tickwave_format::model::{Break, Grid};

/// An analog channel's samples' places: its first run's tick, its grid, count and breaks, in its
/// source's ticks, and where those lie on the axis.
#[derive(Clone, Copy, Debug)]
pub struct Runs<'a> {
    /// The first run's whole source tick; its first sample lies the grid's phase past it.
    pub first: i64,
    /// The period and the declared phase, places counted in `1/d` source ticks.
    pub grid: Grid,
    /// `u64::MAX` for a tile whose channel's count doesn't matter.
    pub count: u64,
    /// Each break's tick is a source tick, and its phase in the grid's `1/d` ticks.
    pub breaks: &'a [Break],
    pub map: Mapping,
}

impl Runs<'_> {
    /// A place of `units / d` source ticks on the axis, saturating where it passes an i64.
    #[inline]
    pub fn at(&self, units: i128) -> i64 {
        self.map
            .axis_place(units, self.grid.d())
            .unwrap_or(if units < 0 { i64::MIN } else { i64::MAX })
    }

    /// The axis tick of sample `i`, its breaks included.
    pub fn tick(&self, i: u64) -> i64 {
        self.at(self.grid.sample_place(self.first, self.breaks, i))
    }

    /// The whole source tick at or before sample `i`'s place, its breaks included.
    pub fn source_tick(&self, i: u64) -> i64 {
        self.grid.sample_tick(self.first, self.breaks, i)
    }

    /// The first sample's axis tick.
    pub fn start(&self) -> i64 {
        self.at(self.grid.start(self.grid.first_run(self.first, 0)))
    }

    /// Break `k`'s axis tick: where run `k + 1` starts.
    pub fn break_tick(&self, k: usize) -> i64 {
        self.at(self.grid.start(self.breaks[k]))
    }

    /// The ticks sample `i` covers on the axis (`Grid::sample_time`), none past `length`.
    pub fn support(&self, i: u64, length: i64) -> Option<Range<i64>> {
        let s = self
            .grid
            .sample_time(self.first, self.breaks, self.count, i, None)?;
        Some(self.at(s.start).min(length)..self.at(s.end).min(length))
    }
}

/// The least index of `0..count` at which `past` holds, `count` where it never does: `past` must
/// hold from some index on.
fn first_index(count: u64, past: impl Fn(u64) -> bool) -> u64 {
    let (mut lo, mut hi) = (0u64, count);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if past(mid) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

/// The index of the first sample at or after axis tick `tick`, breaks included, clamped to
/// `0..=count`. A NaN tick is past every sample.
pub fn index_at_or_after(runs: &Runs, tick: f64) -> u64 {
    if tick.is_nan() {
        return runs.count;
    }
    // A sample's axis tick never falls as its index grows: each run starts after the one before
    // it ends.
    first_index(runs.count, |i| runs.tick(i) as f64 >= tick)
}

/// The index of the last sample at or before axis tick `tick`: the value a cursor there reads.
/// None before the first sample or at or past `end`, the end of the channel's data, an axis tick.
pub fn sample_at_or_before(runs: &Runs, end: i64, tick: f64) -> Option<u64> {
    if tick < runs.start() as f64 || tick >= end as f64 || runs.count == 0 {
        return None;
    }
    let after = first_index(runs.count, |i| runs.tick(i) as f64 > tick);
    Some(after.checked_sub(1)?.min(runs.count - 1))
}

/// The axis tick of the place after sample `i` on its run's grid: where its time ends, unless the
/// next run or the length comes sooner (FORMAT.md, "Gaps").
pub fn place_after(runs: &Runs, i: u64) -> i64 {
    let run = runs.grid.run_of(runs.first, runs.breaks, i);
    runs.at(runs.grid.place(run, i.saturating_add(1)))
}

/// Where a break's lost samples would have sat, in axis ticks, and how many there were.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LostSpan {
    pub from: i64,
    pub to: i64,
    pub lost: u64,
}

/// The span each break's lost samples cover (FORMAT.md, "Gaps"): from place `index` to place
/// `index + lost` on the grid of the run before the break, ending at the break's start if that
/// comes sooner. A pause has none.
pub fn lost_spans(runs: &Runs) -> Vec<LostSpan> {
    lost_spans_in(runs, i64::MIN..i64::MAX)
}

/// `lost_spans` that reach into `range`, axis ticks. A break's lost samples end by its start and
/// start after the run before it starts, so only the breaks from the first past `range.start` to
/// the first whose run starts at `range.end` are read.
pub fn lost_spans_in(runs: &Runs, range: Range<i64>) -> Vec<LostSpan> {
    let first = runs
        .breaks
        .partition_point(|b| runs.at(runs.grid.start(*b)) <= range.start);
    let mut out = Vec::new();
    for k in first..runs.breaks.len() {
        let run_start = match k.checked_sub(1) {
            Some(p) => runs.break_tick(p),
            None => runs.start(),
        };
        if run_start >= range.end {
            break;
        }
        let Some(span) = runs.grid.lost_time(runs.first, runs.breaks, k) else {
            continue;
        };
        let (from, to) = (runs.at(span.start), runs.at(span.end));
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
