//! What an interval of each analog channel holds: how many samples count in it, the time they
//! cover, their extremes, and their sum weighted by that time, exactly for integer codes.
//!
//! A sample counts, and its value is one of the extremes, when its time (FORMAT.md, "Gaps")
//! clipped to the interval is above zero: one starting before the interval that reaches into it
//! counts. A NaN sample counts in nothing.

use std::ops::Range;
use std::sync::atomic::AtomicBool;

use escapement_tickwave_format::header::Exact;
use escapement_tickwave_format::mapping::Mapping;
use escapement_tickwave_format::model::{Break, Ratio};
use num_bigint::BigInt;
use num_traits::ToPrimitive;

use crate::read::check;
use crate::recording::{Error, Place, Recording};
use crate::runs::Runs;

#[derive(Clone, Debug, PartialEq)]
pub struct Statistics {
    pub interval: Range<i64>,
    /// By channel index: None for a digital channel.
    pub channels: Vec<Option<ChannelStatistics>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelStatistics {
    pub samples: u64,
    /// The ticks the counted samples cover inside the interval: whole ticks, since every
    /// sample's time is.
    pub measured: i64,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Σ value × the ticks it covers inside the interval.
    pub sum: Sum,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sum {
    /// An `int` channel's or an integer product's: Σ code × ticks, exactly, and the scale and
    /// offset that make a code a value. The value sum is `scale × codes_ticks + offset × measured`.
    Exact {
        codes_ticks: i128,
        scale: Exact,
        offset: Exact,
    },
    /// A float channel's, summed in f64.
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
                let n =
                    sn * &od * BigInt::from(codes_ticks) + on * &sd * BigInt::from(self.measured);
                Some((n, sd * od))
            }
            Sum::Float(_) => None,
        }
    }

    /// The mean value over the time the samples cover. None with nothing measured.
    pub fn mean(&self) -> Option<f64> {
        if self.measured == 0 {
            return None;
        }
        Some(match self.sum {
            Sum::Float(s) => s / self.measured as f64,
            Sum::Exact { .. } => {
                let (n, d) = self.weighted().expect("exact");
                ratio(&n, &(d * BigInt::from(self.measured)))
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

    /// The ticks of `interval` inside `extent` that no sample covers.
    pub fn unmeasured(&self, interval: &Range<i64>, extent: &Range<i64>) -> i64 {
        let inside = interval.end.min(extent.end) - interval.start.max(extent.start);
        inside.max(0) - self.measured
    }
}

/// `n / d` in f64: each converted, then divided.
fn ratio(n: &BigInt, d: &BigInt) -> f64 {
    n.to_f64().unwrap_or(f64::NAN) / d.to_f64().unwrap_or(f64::NAN)
}

/// Each sample's time on the axis, as `Runs::support` gives it, for samples taken in index order:
/// the run a sample falls in is found once a run, and a sample's place is the one worked out for
/// the end of the sample before it. Places are worked in the source's ticks, then mapped.
struct Supports<'a> {
    first: i64,
    /// The period's terms.
    num: u128,
    den: u128,
    breaks: &'a [Break],
    map: Mapping,
    /// Whether `map` is the identity, which skips the mapping for every sample.
    identity: bool,
    /// The axis tick no support runs past.
    length: i64,
    /// The run of the last sample asked for.
    run: Run,
    /// The sample after the last one asked for, and its place on the run's grid, uncapped.
    ahead: Option<(u64, i128)>,
}

/// A run's samples `start..end`, from source tick `tick`, and where its last sample's time ends
/// when a run follows it: the next run's tick.
#[derive(Clone, Copy)]
struct Run {
    start: u64,
    end: u64,
    tick: i64,
    cut: Option<i64>,
}

impl<'a> Supports<'a> {
    fn new(runs: &Runs<'a>, length: i64) -> Self {
        Self {
            first: runs.first,
            num: u128::from(runs.period.num()),
            den: u128::from(runs.period.den()),
            breaks: runs.breaks,
            map: runs.map,
            identity: runs.map.is_identity(),
            length,
            run: Run {
                start: 0,
                end: 0,
                tick: 0,
                cut: None,
            },
            ahead: None,
        }
    }

    fn run_of(&self, i: u64) -> Run {
        let k = self.breaks.partition_point(|b| b.index <= i);
        let (start, tick) = match k.checked_sub(1).and_then(|p| self.breaks.get(p)) {
            None => (0, self.first),
            Some(b) => (b.index, b.tick),
        };
        let next = self.breaks.get(k);
        Run {
            start,
            end: next.map_or(u64::MAX, |b| b.index),
            tick,
            cut: next.map(|b| b.tick),
        }
    }

    /// Place `steps` samples into run `r`, uncapped.
    fn place(&self, r: &Run, steps: u64) -> i128 {
        let n = u128::from(steps) * self.num;
        let ticks = match self.den {
            1 => n,
            d => n / d,
        };
        i128::from(r.tick).saturating_add(i128::try_from(ticks).unwrap_or(i128::MAX))
    }

    /// The time of sample `i`, one the channel has.
    fn of(&mut self, i: u64) -> Range<i64> {
        if !(self.run.start..self.run.end).contains(&i) {
            self.run = self.run_of(i);
            self.ahead = None;
        }
        let r = self.run;
        // Breaks come in index order, a file's checked and a writer's as it makes them, which puts
        // the run's start at or before `i`.
        let steps = i.saturating_sub(r.start);
        let from = match self.ahead {
            Some((j, p)) if j == i => p,
            _ => self.place(&r, steps),
        };
        let to = self.place(&r, steps + 1);
        self.ahead = Some((i + 1, to));
        let to = match r.cut {
            Some(cut) if i + 1 == r.end => to.min(i128::from(cut)),
            _ => to,
        };
        let length = i128::from(self.length);
        if self.identity {
            return from.min(length) as i64..to.min(length) as i64;
        }
        let axis = |x: i128| {
            let x = x.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
            self.map.axis_saturating(x).min(self.length)
        };
        axis(from)..axis(to)
    }
}

/// Σ value × ticks so far: of the codes, exactly, for an `int` channel or an integer product, or
/// of the values in f64.
enum Weighted {
    Codes(i128),
    Float(f64),
}

/// What an interval's samples add up to so far.
struct Tally {
    samples: u64,
    measured: i64,
    min: f64,
    max: f64,
    sum: Weighted,
}

impl Tally {
    fn value(&mut self, v: f64) {
        self.min = self.min.min(v);
        self.max = self.max.max(v);
    }

    fn measure(&mut self, samples: u64, ticks: i64) -> Result<(), Error> {
        self.samples += samples;
        self.measured = self.measured.checked_add(ticks).ok_or(Error::Overflow)?;
        Ok(())
    }

    fn add_codes(&mut self, codes: i128, ticks: i64) -> Result<(), Error> {
        let Weighted::Codes(sum) = &mut self.sum else {
            unreachable!("only an exact channel's codes are summed");
        };
        *sum = codes
            .checked_mul(i128::from(ticks))
            .and_then(|x| sum.checked_add(x))
            .ok_or(Error::Overflow)?;
        Ok(())
    }

    /// One sample over `ticks` of the interval; its code where the channel is exact.
    fn sample(&mut self, v: f64, code: Option<i128>, ticks: i64) -> Result<(), Error> {
        if ticks <= 0 || v.is_nan() {
            return Ok(());
        }
        self.measure(1, ticks)?;
        self.value(v);
        match (&mut self.sum, code) {
            (Weighted::Codes(_), Some(code)) => self.add_codes(code, ticks),
            (Weighted::Codes(_), None) => unreachable!("an exact channel's samples have codes"),
            (Weighted::Float(sum), _) => {
                *sum += v * ticks as f64;
                Ok(())
            }
        }
    }

    /// `n` samples a bucket holds, a full period `p` each, with its extremes and its codes' sum.
    fn bucket(&mut self, n: u64, p: i64, min: f64, max: f64, codes: i128) -> Result<(), Error> {
        let ticks = i64::try_from(n)
            .ok()
            .and_then(|n| n.checked_mul(p))
            .ok_or(Error::Overflow)?;
        self.measure(n, ticks)?;
        self.value(min);
        self.value(max);
        self.add_codes(codes, p)
    }
}

impl Recording {
    /// Each analog and derived channel's statistics over `interval`. Cancel is checked between
    /// blocks.
    #[tracing::instrument(level = "debug", name = "statistics", skip_all)]
    pub fn statistics(
        &self,
        interval: Range<i64>,
        cancel: &AtomicBool,
    ) -> Result<Statistics, Error> {
        let channels = (0..self.places.len())
            .map(|c| match &self.places[c] {
                Place::Digital { .. } => Ok(None),
                Place::Analog { .. } => self.channel_statistics(c, &interval, cancel).map(Some),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Statistics { interval, channels })
    }

    fn channel_statistics(
        &self,
        c: usize,
        interval: &Range<i64>,
        cancel: &AtomicBool,
    ) -> Result<ChannelStatistics, Error> {
        let place = self.place(c)?;
        let Place::Analog {
            period,
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
        let mut t = Tally {
            samples: 0,
            measured: 0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            sum: match exact {
                Some(_) => Weighted::Codes(0),
                None => Weighted::Float(0.0),
            },
        };
        let (a, b) = (interval.start.max(0), interval.end.min(self.length));
        if b > a && *count > 0 {
            let mut supports = Supports::new(&runs, self.length);
            // The first sample whose time reaches past `a`, and the first at or past `b`.
            let j = self.index_at(c, a)?;
            let lo = if j > 0 && supports.of(j - 1).end > a {
                j - 1
            } else {
                j
            };
            let hi = self.index_at(c, b)?;
            // A whole number of source ticks a sample, each a whole number of axis ticks: every
            // sample a run holds lasts as long on the axis.
            let m = map.m();
            let p = (period.den() == 1 && m.den() == 1)
                .then(|| period.num().checked_mul(m.num()))
                .flatten()
                .and_then(|p| i64::try_from(p).ok());
            let whole = match (exact, p) {
                (Some(_), Some(p)) => self.whole_buckets(c, p, a, b, &mut t, cancel)?,
                _ => Vec::new(),
            };
            // Every sample outside the buckets taken, one by one: before the first, between
            // each two, and after the last.
            let mut one_by_one = |from: u64, to: u64, t: &mut Tally| {
                self.each_sample(c, from, to, cancel, |i, v, code| {
                    let s = supports.of(i);
                    t.sample(v, code, s.end.min(b) - s.start.max(a))
                })
            };
            let mut from = lo;
            for r in &whole {
                one_by_one(from, self.index_at(c, r.start)?.min(hi), &mut t)?;
                from = self.index_at(c, r.end)?.max(from);
            }
            one_by_one(from, hi, &mut t)?;
        }
        let some = |x: f64| (t.samples > 0).then_some(x);
        Ok(ChannelStatistics {
            samples: t.samples,
            measured: t.measured,
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
    fn whole_buckets(
        &self,
        c: usize,
        p: i64,
        a: i64,
        b: i64,
        t: &mut Tally,
        cancel: &AtomicBool,
    ) -> Result<Vec<Range<i64>>, Error> {
        let place = self.place(c)?;
        let (Some(runs), Place::Analog { count, .. }) = (place.runs(), place) else {
            return Err(Error::NoChannel(c));
        };
        // A sample in a bucket that starts at or after `a` and ends by b − p + 1 covers a full
        // period inside [a, b), unless it ends a run cut short.
        let region_end = b.saturating_sub(p - 1);
        if self.levels.is_empty() || region_end <= a {
            return Ok(Vec::new());
        }
        // Each run's last sample whose time the next run or the length cuts short of a full
        // period, from the run holding `a` on. They come in order of their ticks.
        let from = runs.breaks.partition_point(|x| runs.axis(x.tick) <= a);
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
        for l in (0..self.levels.len()).rev() {
            let shift = self.levels[l].shift;
            let size = 1i64.checked_shl(u32::from(shift)).unwrap_or(i64::MAX);
            let covered = self.levels[l].end();
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
                    check(cancel)?;
                    let buckets = self.analog_buckets(c, l, k as u64, end as u64, cancel)?;
                    let sums = buckets.sums.as_ref().expect("an exact channel's sums");
                    for (o, kk) in (k..end).enumerate() {
                        let (s, e) = (kk * size, (kk + 1) * size);
                        let n = self.index_at(c, e)? - self.index_at(c, s)?;
                        if n > 0 {
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
    use escapement_tickwave_format::model::{Break, Ratio, sample_support};

    use super::Supports;
    use crate::runs::Runs;

    fn brk(index: u64, tick: i64) -> Break {
        Break {
            index,
            tick,
            lost: 0,
        }
    }

    /// Every 2 ticks from 0, samples 0 to 4; the next run starts at tick 9, inside sample 4's
    /// period; samples 5 to 7 from there; the length cuts sample 7 at 14. Worked by hand.
    #[test]
    fn a_runs_last_sample_ends_at_the_next_run_and_the_last_at_the_length() {
        let breaks = [brk(5, 9)];
        let runs = Runs {
            first: 0,
            period: Ratio::integer(2).unwrap(),
            count: 8,
            breaks: &breaks,
            map: Mapping::IDENTITY,
        };
        let mut s = Supports::new(&runs, 14);
        let got: Vec<Range<i64>> = (0..8).map(|i| s.of(i)).collect();
        assert_eq!(got, [0..2, 2..4, 4..6, 6..8, 8..9, 9..11, 11..13, 13..14]);
    }

    /// Against `sample_support` for every sample of each channel: in order from each sample, in
    /// steps of three, and backwards.
    #[test]
    fn each_samples_time_is_sample_supports() {
        let cases = [
            // A whole-tick period, the length inside the last sample's.
            (0, Ratio::integer(960).unwrap(), 1000, vec![], 959_500),
            // tests/runs.rs's channel: two lost at a break.
            (
                0,
                Ratio::new(3, 2).unwrap(),
                10,
                vec![Break {
                    index: 5,
                    tick: 30,
                    lost: 2,
                }],
                40,
            ),
            // A run cut short by the next, a run of one sample cut short by the next, and a last
            // sample of no time at the length.
            (
                5,
                Ratio::new(7, 3).unwrap(),
                15,
                vec![brk(4, 13), brk(9, 40), brk(10, 41)],
                50,
            ),
            // A break on the grid's next place.
            (0, Ratio::integer(2).unwrap(), 6, vec![brk(3, 6)], 12),
            // Places past an i64, held to the length.
            (
                i64::MAX - 10,
                Ratio::integer(4).unwrap(),
                5,
                vec![],
                i64::MAX,
            ),
        ];
        for (first, period, count, breaks, length) in cases {
            let runs = Runs {
                first,
                period,
                count,
                breaks: &breaks,
                map: Mapping::IDENTITY,
            };
            let want = |i| sample_support(first, period, &breaks, count, length, i).unwrap();
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
}
