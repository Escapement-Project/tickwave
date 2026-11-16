//! What an interval of each analog channel holds: how many samples count in it, the time they
//! cover, their extremes, and their sum weighted by that time, exactly for integer codes.
//!
//! A sample counts, and its value is one of the extremes, when its time (FORMAT.md, "Gaps")
//! clipped to the interval is above zero: one starting before the interval that reaches into it
//! counts. A NaN sample counts in nothing. A sample's time runs from its place to the next, which a
//! phase, a fractional period or a source's `m` puts between axis ticks: it is measured exactly, in
//! `1 / per` axis ticks, `per` being the channel's grid `d` times its `m`'s denominator.

use std::num::NonZeroU128;
use std::ops::Range;
use std::sync::atomic::AtomicBool;

use escapement_tickwave_format::header::Exact;
use escapement_tickwave_format::mapping::Mapping;
use escapement_tickwave_format::model::{Break, Grid, Ratio};
use num_bigint::BigInt;
use num_traits::ToPrimitive;

use crate::cache::Reading;
use crate::read::{check, index_in};
use crate::recording::{Error, Place, Recording};
use crate::runs::Runs;

#[derive(Clone, Debug, PartialEq)]
pub struct Statistics {
    pub interval: Range<i64>,
    /// By channel index: None for a digital channel and one this version can't read.
    pub channels: Vec<Option<ChannelStatistics>>,
}

/// A span of axis time, exactly: `units / per` ticks.
#[derive(Clone, Copy, Debug)]
pub struct Ticks {
    pub units: i128,
    /// A channel's grid `d` times its `m`'s denominator: below 2^69.
    pub per: NonZeroU128,
}

impl Ticks {
    /// `ticks` whole ticks.
    pub const fn whole(ticks: i64) -> Self {
        Self {
            units: ticks as i128,
            per: NonZeroU128::MIN,
        }
    }

    /// In ticks, for presenting.
    pub fn as_f64(self) -> f64 {
        self.units as f64 / self.per.get() as f64
    }
}

/// Equal as numbers, whatever their units.
impl PartialEq for Ticks {
    fn eq(&self, other: &Self) -> bool {
        BigInt::from(self.units) * BigInt::from(other.per.get())
            == BigInt::from(other.units) * BigInt::from(self.per.get())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelStatistics {
    pub samples: u64,
    /// The time the counted samples cover inside the interval.
    pub measured: Ticks,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Σ value × the time it covers inside the interval.
    pub sum: Sum,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sum {
    /// An `int` channel's or an integer product's: Σ code × time, exactly, in `1 / per` ticks,
    /// `measured`'s units, and the scale and offset that make a code a value. The value sum, in
    /// ticks, is `(scale × codes_ticks + offset × measured.units) / per`.
    Exact {
        codes_ticks: i128,
        scale: Exact,
        offset: Exact,
    },
    /// A float channel's, in value × ticks, summed in f64.
    Float(f64),
}

fn exact(e: Exact) -> (BigInt, BigInt) {
    (BigInt::from(e.num()), BigInt::from(e.den()))
}

impl ChannelStatistics {
    /// Σ value × ticks as `n / d`, exactly where the sum is.
    fn weighted(&self) -> Option<(BigInt, BigInt)> {
        match self.sum {
            Sum::Exact {
                codes_ticks,
                scale,
                offset,
            } => {
                let ((sn, sd), (on, od)) = (exact(scale), exact(offset));
                let n = sn * &od * BigInt::from(codes_ticks)
                    + on * &sd * BigInt::from(self.measured.units);
                Some((n, sd * od * BigInt::from(self.measured.per.get())))
            }
            Sum::Float(_) => None,
        }
    }

    /// The mean value over the time the samples cover. None with nothing measured.
    pub fn mean(&self) -> Option<f64> {
        if self.measured.units == 0 {
            return None;
        }
        Some(match self.sum {
            Sum::Float(s) => s / self.measured.as_f64(),
            Sum::Exact { .. } => {
                let (n, d) = self.weighted().expect("exact");
                let m = &self.measured;
                ratio(
                    &(n * BigInt::from(m.per.get())),
                    &(d * BigInt::from(m.units)),
                )
            }
        })
    }

    /// Σ value × time in the unit times seconds: charge from a current, energy from a power.
    pub fn integral(&self, timebase: Ratio) -> f64 {
        match self.sum {
            Sum::Float(s) => s * timebase.den() as f64 / timebase.num() as f64,
            Sum::Exact { .. } => {
                let (n, d) = self.weighted().expect("exact");
                ratio(
                    &(n * BigInt::from(timebase.den())),
                    &(d * BigInt::from(timebase.num())),
                )
            }
        }
    }

    /// The time of `interval` inside `extent` that no sample covers.
    pub fn unmeasured(&self, interval: &Range<i64>, extent: &Range<i64>) -> Ticks {
        let inside = i128::from(interval.end.min(extent.end))
            .saturating_sub(i128::from(interval.start.max(extent.start)))
            .max(0);
        // An extent of axis ticks below 2^53 times `per` below 2^69 stays inside an i128; past
        // that it saturates.
        let per = i128::try_from(self.measured.per.get()).unwrap_or(i128::MAX);
        Ticks {
            units: inside
                .saturating_mul(per)
                .saturating_sub(self.measured.units),
            per: self.measured.per,
        }
    }
}

/// `n / d` in f64: each converted, then divided.
fn ratio(n: &BigInt, d: &BigInt) -> f64 {
    n.to_f64().unwrap_or(f64::NAN) / d.to_f64().unwrap_or(f64::NAN)
}

/// Each sample's time on the axis, exactly, in `1 / per` ticks: from its place to the next on its
/// run's grid, or the next run's start if that comes sooner, and no later than the length.
struct Supports<'a> {
    first: i64,
    grid: Grid,
    breaks: &'a [Break],
    map: Mapping,
    /// `d` times `m`'s denominator, the units of a tick: below 2^69, `d` below 2^16 and the
    /// denominator below 2^53.
    per: NonZeroU128,
    /// The length, in those units.
    length: i128,
    /// The run of the last sample asked for, as the samples it holds and its place among the
    /// breaks. Samples are asked for in order, so the next is most often in it.
    run: Option<(Range<u64>, usize)>,
    /// The sample after the last one asked for, where it is in the same run, and its place on the
    /// axis, not yet held to the length.
    ahead: Option<(u64, i128)>,
    /// Whether `map` is the identity, under which a place's axis time is the place.
    identity: bool,
    /// How long one sample lasts on a run's grid, in those units: the step times `m`'s
    /// numerator. None where `origin × per` or it is too large for the bound `of` needs.
    stride: Option<i128>,
}

impl<'a> Supports<'a> {
    fn new(runs: &Runs<'a>, length: i64) -> Self {
        // Both terms are at least 1, and their product fits a u128.
        let per = NonZeroU128::new(u128::from(runs.grid.d()) * u128::from(runs.map.m().den()))
            .unwrap_or(NonZeroU128::MIN);
        Self {
            first: runs.first,
            grid: runs.grid,
            breaks: runs.breaks,
            map: runs.map,
            per,
            // Below 2^122 for a length below 2^53; saturating past it.
            length: i128::from(length).saturating_mul(units(per)),
            run: None,
            ahead: None,
            identity: runs.map.is_identity(),
            stride: i128::try_from(runs.grid.step())
                .ok()
                .and_then(|s| s.checked_mul(i128::from(runs.map.m().num())))
                .filter(|&s| {
                    s < 1 << 90
                        && i128::from(runs.map.origin())
                            .checked_mul(units(per))
                            .is_some_and(|o| o.unsigned_abs() < 1 << 125)
                }),
        }
    }

    /// A place of `units / d` source ticks on the axis, in `1 / per` ticks, saturating.
    fn at(&self, units: i128) -> i128 {
        if self.identity {
            return units;
        }
        self.map
            .axis_time(units, self.grid.d())
            .unwrap_or(if units < 0 { i128::MIN } else { i128::MAX })
    }

    /// The time of sample `i`, one the channel has.
    fn of(&mut self, i: u64) -> Range<i128> {
        let k = match &self.run {
            Some((held, k)) if held.contains(&i) => *k,
            _ => {
                let k = self.breaks.partition_point(|b| b.index <= i);
                let from = k.checked_sub(1).map_or(0, |p| self.breaks[p].index);
                let to = self.breaks.get(k).map_or(u64::MAX, |b| b.index);
                self.run = Some((from..to, k));
                k
            }
        };
        let run = match k.checked_sub(1) {
            None => self.grid.first_run(self.first, 0),
            Some(p) => self.breaks[p],
        };
        let from = match self.ahead {
            Some((j, t)) if j == i => t,
            _ => self.at(self.grid.place(run, i)),
        };
        let next = self.breaks.get(k).filter(|b| b.index == i + 1);
        let to = match (next, self.stride) {
            // The time is linear in the place, and with `from` below 2^100 no sum `at` makes for
            // the next place passes 128 bits, so it is this one's plus the stride exactly.
            (None, Some(stride)) if from.unsigned_abs() < 1 << 100 => from + stride,
            (None, _) => self.at(self.grid.place(run, i.saturating_add(1))),
            (Some(next), _) => {
                let after = self.grid.place(run, i.saturating_add(1));
                self.at(after.min(self.grid.start(*next)))
            }
        };
        self.ahead = next.is_none().then_some((i + 1, to));
        from.min(self.length)..to.min(self.length)
    }
}

/// A sum, or the overflow it ran into. `ok_or(Error::Overflow)` builds the error, and drops it,
/// on every call: this runs for every sample.
#[inline]
fn held(x: Option<i128>) -> Result<i128, Error> {
    match x {
        Some(x) => Ok(x),
        None => Err(Error::Overflow),
    }
}

/// `per` as the i128 a tick's units multiply by: it is below 2^69.
fn units(per: NonZeroU128) -> i128 {
    i128::try_from(per.get()).unwrap_or(i128::MAX)
}

/// Σ value × time so far: of the codes, exactly, in code × `1 / per` ticks, for an `int` channel
/// or an integer product, or of the values in f64, in value × ticks.
enum Weighted {
    Codes(i128),
    Float(f64),
}

/// What an interval's samples add up to so far.
struct Tally {
    samples: u64,
    /// In `1 / per` ticks.
    measured: i128,
    per: NonZeroU128,
    min: f64,
    max: f64,
    sum: Weighted,
}

impl Tally {
    fn value(&mut self, v: f64) {
        self.min = self.min.min(v);
        self.max = self.max.max(v);
    }

    fn measure(&mut self, samples: u64, units: i128) -> Result<(), Error> {
        self.samples += samples;
        self.measured = held(self.measured.checked_add(units))?;
        Ok(())
    }

    fn add_codes(&mut self, codes: i128, units: i128) -> Result<(), Error> {
        let Weighted::Codes(sum) = &mut self.sum else {
            unreachable!("only an exact channel's codes are summed");
        };
        *sum = held(codes.checked_mul(units).and_then(|x| sum.checked_add(x)))?;
        Ok(())
    }

    /// One sample over `units` of the interval, in `1 / per` ticks; its code where the channel is
    /// exact.
    #[inline]
    fn sample(&mut self, v: f64, code: Option<i128>, units: i128) -> Result<(), Error> {
        if units <= 0 || v.is_nan() {
            return Ok(());
        }
        self.measure(1, units)?;
        self.value(v);
        match (&mut self.sum, code) {
            (Weighted::Codes(_), Some(code)) => self.add_codes(code, units),
            (Weighted::Codes(_), None) => unreachable!("an exact channel's samples have codes"),
            (Weighted::Float(sum), _) => {
                *sum += v * (units as f64 / self.per.get() as f64);
                Ok(())
            }
        }
    }

    /// `n` samples a bucket holds, a full period of `p` units each, with its extremes and its
    /// codes' sum.
    fn bucket(&mut self, n: u64, p: i128, min: f64, max: f64, codes: i128) -> Result<(), Error> {
        let units = held(i128::from(n).checked_mul(p))?;
        self.measure(n, units)?;
        self.value(min);
        self.value(max);
        self.add_codes(codes, p)
    }
}

impl Recording {
    /// Each analog and derived channel's statistics over `interval`. Cancel is checked between
    /// blocks.
    pub fn statistics(
        &self,
        interval: Range<i64>,
        cancel: &AtomicBool,
    ) -> Result<Statistics, Error> {
        self.statistics_in(&Reading::new(cancel), interval)
    }

    #[tracing::instrument(level = "debug", name = "statistics", skip_all)]
    pub(crate) fn statistics_in(
        &self,
        r: &Reading,
        interval: Range<i64>,
    ) -> Result<Statistics, Error> {
        // Each channel's place over the interval alone: a list in pages reads only the pages it
        // needs.
        let span = interval.start.max(0)..interval.end.min(self.length).max(interval.start.max(0));
        let channels = (0..self.places.len())
            .map(|c| match self.places.get(c) {
                Some(Place::Analog { .. }) => {
                    let place = self.place_over(r, c, span.clone())?;
                    self.channel_statistics(r, c, &place, &interval).map(Some)
                }
                _ => Ok(None),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Statistics { interval, channels })
    }

    fn channel_statistics(
        &self,
        r: &Reading,
        c: usize,
        place: &Place,
        interval: &Range<i64>,
    ) -> Result<ChannelStatistics, Error> {
        let Place::Analog {
            grid,
            count,
            rule,
            map,
            ..
        } = place
        else {
            return Err(Error::NoChannel(c));
        };
        let runs = place.runs().expect("analog");
        let exact = rule.exact();
        let mut supports = Supports::new(&runs, self.length);
        let per = supports.per;
        let mut t = Tally {
            samples: 0,
            measured: 0,
            per,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            sum: match exact {
                Some(_) => Weighted::Codes(0),
                None => Weighted::Float(0.0),
            },
        };
        let (a, b) = (interval.start.max(0), interval.end.min(self.length));
        // Used only where both lie in 0..=length, below length × per; saturating otherwise.
        let (au, bu) = (
            i128::from(a).saturating_mul(units(per)),
            i128::from(b).saturating_mul(units(per)),
        );
        if b > a && *count > 0 {
            // The first sample whose time reaches past `a`, and the first at or past `b`.
            let j = index_in(c, place, a)?;
            let lo = if j > 0 && supports.of(j - 1).end > au {
                j - 1
            } else {
                j
            };
            let hi = index_in(c, place, b)?;
            // Places on whole source ticks, each a whole number of axis ticks: every sample a run
            // holds lasts as long on the axis, and lies wholly in the bucket of its tick.
            let m = map.m();
            let p = (grid.d() == 1 && m.den() == 1)
                .then(|| u64::try_from(grid.step()).ok()?.checked_mul(m.num()))
                .flatten()
                .and_then(|p| i64::try_from(p).ok());
            let whole = match (exact, p) {
                (Some(_), Some(p)) => self.whole_buckets(r, c, place, p, a, b, &mut t)?,
                _ => Vec::new(),
            };
            // Every sample outside the buckets taken, one by one: before the first, between
            // each two, and after the last.
            let mut one_by_one = |from: u64, to: u64, t: &mut Tally| {
                self.each_sample_in(r, c, place, from, to, |i, v, code| {
                    let s = supports.of(i);
                    t.sample(v, code, s.end.min(bu) - s.start.max(au))
                })
            };
            let mut from = lo;
            for r in &whole {
                one_by_one(from, index_in(c, place, r.start)?.min(hi), &mut t)?;
                from = index_in(c, place, r.end)?.max(from);
            }
            one_by_one(from, hi, &mut t)?;
        }
        let some = |x: f64| (t.samples > 0).then_some(x);
        Ok(ChannelStatistics {
            samples: t.samples,
            measured: Ticks {
                units: t.measured,
                per,
            },
            min: some(t.min),
            max: some(t.max),
            sum: match (t.sum, exact) {
                (Weighted::Codes(codes_ticks), Some((scale, offset))) => Sum::Exact {
                    codes_ticks,
                    scale,
                    offset,
                },
                (Weighted::Float(s), _) => Sum::Float(s),
                (Weighted::Codes(_), None) => unreachable!("codes are summed for an exact channel"),
            },
        })
    }

    /// Adds to `t` the overview's whole buckets of [a, b) for a whole-tick period of `p` axis
    /// ticks: those whose every sample's time lies inside, a full period each, the coarsest first.
    /// Leaves out each bucket that holds a run's last sample whose time a run or the length cuts
    /// short. The tick ranges taken, in order.
    #[expect(
        clippy::too_many_arguments,
        reason = "a channel's place, its period, the interval and the tally"
    )]
    fn whole_buckets(
        &self,
        reading: &Reading,
        c: usize,
        place: &Place,
        p: i64,
        a: i64,
        b: i64,
        t: &mut Tally,
    ) -> Result<Vec<Range<i64>>, Error> {
        let (Some(runs), Place::Analog { count, .. }) = (place.runs(), place) else {
            return Err(Error::NoChannel(c));
        };
        // A sample in a bucket that starts at or after `a` and ends by b − p + 1 covers a full
        // period inside [a, b), unless it ends a run cut short.
        let region_end = b.saturating_sub(p - 1);
        if self.levels[c].is_empty() || region_end <= a {
            return Ok(Vec::new());
        }
        // Each run's last sample whose time the next run or the length cuts short of a full
        // period, from the run holding `a` on. They come in order of their ticks.
        let from = runs
            .breaks
            .partition_point(|x| runs.at(runs.grid.start(*x)) <= a);
        let mut short = Vec::new();
        for i in runs.breaks[from..]
            .iter()
            .map(|x| x.index - 1)
            .chain([count - 1])
        {
            let s = runs.support(i, self.length).expect("a run's last sample");
            if s.start >= region_end {
                break;
            }
            if s.end - s.start != p {
                short.push(s.start);
            }
        }
        let clean = |r: &Range<i64>| {
            let k = short.partition_point(|&x| x < r.start);
            short.get(k).is_none_or(|&x| x >= r.end)
        };
        // The tick ranges still to cover, coarsest level first: the region, then what each level
        // leaves of it.
        #[expect(
            clippy::single_range_in_vec_init,
            reason = "a list of ranges that starts as one, not the range's ticks"
        )]
        let mut left = vec![a..region_end];
        let mut taken = Vec::new();
        for l in (0..self.levels[c].len()).rev() {
            let shift = self.levels[c][l].shift;
            let size = 1i64.checked_shl(u32::from(shift)).unwrap_or(i64::MAX);
            let covered = self.levels[c][l].end();
            let mut next = Vec::new();
            for r in left {
                let first = r.start.div_euclid(size) + i64::from(r.start.rem_euclid(size) != 0);
                let last = r.end.min(covered).div_euclid(size);
                let mut at = r.start;
                let mut k = first;
                while k < last {
                    let bucket = k * size..(k + 1) * size;
                    if !clean(&bucket) {
                        k += 1;
                        continue;
                    }
                    // A run of clean buckets, read together.
                    let mut end = k + 1;
                    while end < last && clean(&(end * size..(end + 1) * size)) {
                        end += 1;
                    }
                    check(reading.cancel)?;
                    let buckets = self.analog_buckets(reading, c, l, k as u64, end as u64)?;
                    let sums = buckets.sums.as_ref().expect("an exact channel's sums");
                    for (o, kk) in (k..end).enumerate() {
                        let (s, e) = (kk * size, (kk + 1) * size);
                        let n = index_in(c, place, e)? - index_in(c, place, s)?;
                        if n > 0 {
                            let p = i128::from(p);
                            t.bucket(n, p, buckets.min[o], buckets.max[o], sums[o])?;
                        }
                    }
                    if k * size > at {
                        next.push(at..k * size);
                    }
                    taken.push(k * size..end * size);
                    at = end * size;
                    k = end;
                }
                if r.end > at {
                    next.push(at..r.end);
                }
            }
            left = next;
        }
        taken.sort_by_key(|r| r.start);
        Ok(taken)
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use escapement_tickwave_format::mapping::Mapping;
    use escapement_tickwave_format::model::{Break, Grid, Ratio};

    use super::{Supports, Tally, Weighted};
    use crate::recording::Error;
    use crate::runs::Runs;

    /// A sum that passes 128 bits, of the time measured or of code × time, is an error, not a
    /// figure wrapped or held at the bound.
    #[test]
    fn a_sum_past_what_128_bits_hold_is_an_error() {
        let tally = |measured| Tally {
            samples: 0,
            measured,
            per: std::num::NonZeroU128::MIN,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            sum: Weighted::Codes(0),
        };
        let overflow = |r: Result<(), Error>| matches!(r, Err(Error::Overflow));
        assert!(overflow(tally(0).sample(1.0, Some(i128::MAX / 2), 3)));
        assert!(overflow(tally(i128::MAX).sample(1.0, Some(1), 1)));
        assert!(overflow(tally(0).bucket(3, i128::MAX / 2, 1.0, 1.0, 1)));
        assert!(tally(0).sample(1.0, Some(i128::MAX / 3), 3).is_ok());
    }

    fn brk(index: u64, tick: i64) -> Break {
        Break {
            index,
            tick,
            lost: 0,
            phase: 0,
        }
    }

    /// Every 2 ticks from 0, samples 0 to 4; the next run starts at tick 9, inside sample 4's
    /// period; samples 5 to 7 from there; the length cuts sample 7 at 14. Worked by hand.
    #[test]
    fn a_runs_last_sample_ends_at_the_next_run_and_the_last_at_the_length() {
        let breaks = [brk(5, 9)];
        let runs = Runs {
            first: 0,
            grid: Grid::new(Ratio::integer(2).unwrap(), None).unwrap(),
            count: 8,
            breaks: &breaks,
            map: Mapping::IDENTITY,
        };
        let mut s = Supports::new(&runs, 14);
        let got: Vec<Range<i128>> = (0..8).map(|i| s.of(i)).collect();
        assert_eq!(got, [0..2, 2..4, 4..6, 6..8, 8..9, 9..11, 11..13, 13..14]);
    }

    /// A phase of a quarter tick and a period of 3/2: places at 1/4, 7/4, 13/4 and so on, in
    /// quarters of a tick; on a source of `m` 2, eighths of an axis tick, from twice the place.
    /// Worked by hand.
    #[test]
    fn a_samples_time_runs_from_its_exact_place_to_the_next() {
        let grid = Grid::new(Ratio::new(3, 2).unwrap(), Some(Ratio::new(1, 4).unwrap())).unwrap();
        assert_eq!(grid.d(), 4);
        let runs = Runs {
            first: 0,
            grid,
            count: 4,
            breaks: &[],
            map: Mapping::IDENTITY,
        };
        let mut s = Supports::new(&runs, 100);
        assert_eq!(s.per.get(), 4);
        let got: Vec<Range<i128>> = (0..4).map(|i| s.of(i)).collect();
        assert_eq!(got, [1..7, 7..13, 13..19, 19..25]);
        let m2 = Mapping::new(0, 0, Ratio::integer(2).unwrap());
        let runs = Runs { map: m2, ..runs };
        let mut s = Supports::new(&runs, 100);
        assert_eq!(s.per.get(), 4);
        let got: Vec<Range<i128>> = (0..2).map(|i| s.of(i)).collect();
        assert_eq!(got, [2..14, 14..26]);
        // A length of 3 axis ticks cuts the second at 12 quarters.
        let mut s = Supports::new(&runs, 3);
        assert_eq!(s.of(1), 12..12);
    }

    /// Against `Grid::sample_time` for every sample of each channel, held to the length: in order
    /// from each sample, in steps of three, and backwards.
    #[test]
    fn each_samples_time_is_the_grids() {
        let cases = [
            // A whole-tick period, the length inside the last sample's time.
            (0, Ratio::integer(960).unwrap(), None, 1000, vec![], 959_500),
            // tests/runs.rs's channel: two lost at a break.
            (
                0,
                Ratio::new(3, 2).unwrap(),
                None,
                10,
                vec![Break {
                    index: 5,
                    tick: 30,
                    lost: 2,
                    phase: 0,
                }],
                40,
            ),
            // A phase of a third and a run at a phase of its own cut short by the next, a run of
            // one sample cut short by the next, and a last sample of no time at the length.
            (
                5,
                Ratio::new(7, 3).unwrap(),
                Some(Ratio::new(1, 3).unwrap()),
                15,
                vec![
                    Break {
                        index: 4,
                        tick: 13,
                        lost: 0,
                        phase: 2,
                    },
                    brk(9, 40),
                    brk(10, 41),
                ],
                50,
            ),
            // A break on the grid's next place.
            (0, Ratio::integer(2).unwrap(), None, 6, vec![brk(3, 6)], 12),
            // Places past an i64, held to the length: places are i128, so they don't wrap.
            (
                i64::MAX - 10,
                Ratio::integer(4).unwrap(),
                None,
                5,
                vec![],
                i64::MAX,
            ),
        ];
        for (first, period, phase, count, breaks, length) in cases {
            let grid = Grid::new(period, phase).unwrap();
            let runs = Runs {
                first,
                grid,
                count,
                breaks: &breaks,
                map: Mapping::IDENTITY,
            };
            let cap = i128::from(length) * i128::from(grid.d());
            let want = |i| {
                let t = grid.sample_time(first, &breaks, count, i, None).unwrap();
                t.start.min(cap)..t.end.min(cap)
            };
            let at = format!("{first} every {period:?}, {breaks:?}");
            for from in 0..count {
                let mut s = Supports::new(&runs, length);
                for i in from..count {
                    assert_eq!(s.of(i), want(i), "{at}: sample {i} from {from}");
                }
            }
            let mut s = Supports::new(&runs, length);
            for i in (0..count).step_by(3).chain((0..count).rev()) {
                assert_eq!(s.of(i), want(i), "{at}: sample {i}, stepping");
            }
        }
    }

    /// A place whose axis time passes what an i128 holds saturates, and so is held to the length:
    /// tick 2^62 on a grid of 2^15ths, 2^77 units, on a source of `m` (2^52 − 1)/1, past 2^129.
    #[test]
    fn a_place_past_what_an_i128_maps_is_held_to_the_length() {
        let grid = Grid::new(Ratio::ONE, Some(Ratio::new(1, 1 << 15).unwrap())).unwrap();
        assert_eq!(grid.d(), 1 << 15);
        let map = Mapping::new(0, 0, Ratio::integer((1 << 52) - 1).unwrap());
        let runs = Runs {
            first: 1 << 62,
            grid,
            count: 3,
            breaks: &[],
            map,
        };
        let mut s = Supports::new(&runs, 1000);
        let end = 1000 << 15;
        for i in 0..3 {
            assert_eq!(s.of(i), end..end, "sample {i}");
        }
    }
}
