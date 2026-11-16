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
    first_past(runs, tick, false)
}

/// The index of the last sample at or before axis tick `tick`: the value a cursor there reads.
/// None before the first sample or at or past `end`, the end of the channel's data, an axis tick.
pub fn sample_at_or_before(runs: &Runs, end: i64, tick: f64) -> Option<u64> {
    if tick < runs.start() as f64 || tick >= end as f64 || runs.count == 0 {
        return None;
    }
    let after = first_past(runs, tick, true);
    Some(after.checked_sub(1)?.min(runs.count - 1))
}

/// The first sample whose axis tick reaches `tick` (passes it, where `strict`), `count` where none
/// does. A sample's axis tick never falls as its index grows, since each run starts after the one
/// before it ends: so it lies in the last run whose first sample doesn't reach `tick`, or is the
/// first sample of the run after it.
fn first_past(runs: &Runs, tick: f64, strict: bool) -> u64 {
    let reaches = |t: i64| {
        if strict {
            t as f64 > tick
        } else {
            t as f64 >= tick
        }
    };
    let b = runs.breaks;
    let k = b.partition_point(|x| !reaches(runs.at(runs.grid.start(*x))));
    let (run, from) = match k.checked_sub(1) {
        None => (runs.grid.first_run(runs.first, 0), 0),
        Some(p) => (b[p], b[p].index),
    };
    let to = b.get(k).map_or(runs.count, |x| x.index);
    if from >= to {
        return from;
    }
    let past = |i: u64| reaches(runs.at(runs.grid.place(run, i)));
    // An estimate from the mapping's inverse, settled against the samples' own ticks: off by a
    // sample or two where an f64 resolves the ticks, and finished by a bisection where it
    // doesn't.
    let m = runs.map.m();
    let source = (tick - runs.map.origin() as f64) * m.den() as f64 / m.num() as f64
        - runs.map.offset() as f64;
    let units = source * runs.grid.d() as f64 - runs.grid.start(run) as f64;
    let estimate = run.index as f64 + units / runs.grid.step() as f64;
    let mut i = if estimate.is_nan() || estimate <= from as f64 {
        from
    } else if estimate >= to as f64 {
        to
    } else {
        estimate.ceil() as u64
    };
    for _ in 0..8 {
        if i > from && past(i - 1) {
            i -= 1;
        } else if i < to && !past(i) {
            i += 1;
        } else {
            return i;
        }
    }
    if i > from && past(i - 1) {
        from + first_index(i - from, |j| past(from + j))
    } else {
        i + first_index(to - i, |j| past(i + j))
    }
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

#[cfg(test)]
mod tests {
    use escapement_tickwave_format::mapping::Mapping;
    use escapement_tickwave_format::model::{Break, Grid, Ratio};

    use super::{Runs, first_index, index_at_or_after, sample_at_or_before};

    /// Against a bisection over every sample's tick, at every tick and half tick across each
    /// channel: whole and fractional periods, a phase, breaks with phases of their own, a
    /// stretch either way, a source slower than the axis, an origin and an offset, and ticks past
    /// what an f64 resolves, where the estimate misses by more than the steps that settle it.
    #[test]
    fn the_estimated_search_finds_what_a_bisection_finds() {
        let r = |n, d| Ratio::new(n, d).unwrap();
        let brk = |index, tick, phase| Break {
            phase,
            ..Break::new(index, tick, 0)
        };
        let far = 1 << 60;
        struct Case {
            first: i64,
            count: u64,
            grid: Grid,
            breaks: Vec<Break>,
            map: Mapping,
        }
        let case = |first, count, period, phase, breaks, map| Case {
            first,
            count,
            grid: Grid::new(period, phase).unwrap(),
            breaks,
            map,
        };
        let cases = [
            case(5, 100, r(1, 1), None, vec![], Mapping::IDENTITY),
            case(
                5,
                100,
                r(960, 1),
                None,
                vec![brk(40, 50_000, 0)],
                Mapping::new(-7, 3, r(1, 1)),
            ),
            case(
                5,
                100,
                r(7, 3),
                Some(r(1, 3)),
                vec![brk(30, 200, 2), brk(31, 205, 0)],
                Mapping::IDENTITY,
            ),
            case(
                5,
                100,
                r(3, 2),
                Some(r(1, 4)),
                vec![],
                Mapping::new(100, -12, r(1001, 1000)),
            ),
            case(
                5,
                100,
                r(1, 1),
                None,
                vec![brk(50, 70, 0)],
                Mapping::new(0, 0, r(999, 1000)),
            ),
            case(5, 100, r(5, 4), None, vec![], Mapping::new(10, 0, r(3, 1))),
            case(5, 100, r(1, 1), None, vec![], Mapping::new(0, 0, r(1, 2))),
            case(
                far,
                1000,
                r(1, 1),
                None,
                vec![brk(600, far + 700, 0)],
                Mapping::IDENTITY,
            ),
        ];
        for c in &cases {
            let runs = Runs {
                first: c.first,
                grid: c.grid,
                count: c.count,
                breaks: &c.breaks,
                map: c.map,
            };
            let (lo, hi) = (runs.tick(0) - 3, runs.tick(runs.count - 1) + 3);
            for t2 in 2 * lo..2 * hi {
                let tick = t2 as f64 / 2.0;
                let want = first_index(runs.count, |i| runs.tick(i) as f64 >= tick);
                let at = format!("{:?} {:?} first {}, tick {tick}", c.grid, c.map, c.first);
                assert_eq!(index_at_or_after(&runs, tick), want, "{at}");
                let after = first_index(runs.count, |i| runs.tick(i) as f64 > tick);
                let end = hi;
                let before = (tick >= runs.start() as f64 && tick < end as f64)
                    .then(|| after.checked_sub(1))
                    .flatten();
                assert_eq!(sample_at_or_before(&runs, end, tick), before, "{at}");
            }
        }
    }
}
