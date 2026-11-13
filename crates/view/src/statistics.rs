//! What an interval of each analog channel holds: how many samples count in it, the time they
//! cover, their extremes, and their sum weighted by that time, exactly for integer codes.
//!
//! A sample counts, and its value is one of the extremes, when its time (FORMAT.md, "Gaps")
//! clipped to the interval is above zero: one starting before the interval that reaches into it
//! counts. A NaN sample counts in nothing.

use std::ops::Range;
use std::sync::atomic::AtomicBool;

use escapement_tickwave_format::header::Exact;
use escapement_tickwave_format::model::{Ratio, sample_support};
use num_bigint::BigInt;
use num_traits::ToPrimitive;

use crate::read::check;
use crate::recording::{Error, Place, Recording};

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

/// What an interval's samples add up to so far.
struct Tally {
    samples: u64,
    measured: i64,
    min: f64,
    max: f64,
    codes: Option<i128>,
    float: f64,
}

impl Tally {
    fn value(&mut self, v: f64) {
        self.min = self.min.min(v);
        self.max = self.max.max(v);
    }

    /// One sample over `ticks` of the interval.
    fn sample(&mut self, v: f64, code: Option<i128>, ticks: i64) -> Result<(), Error> {
        if ticks <= 0 || v.is_nan() {
            return Ok(());
        }
        self.samples += 1;
        self.measured = self.measured.checked_add(ticks).ok_or(Error::Overflow)?;
        self.value(v);
        match (&mut self.codes, code) {
            (Some(sum), Some(code)) => {
                *sum = code
                    .checked_mul(i128::from(ticks))
                    .and_then(|x| sum.checked_add(x))
                    .ok_or(Error::Overflow)?;
            }
            _ => self.float += v * ticks as f64,
        }
        Ok(())
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
            codes: exact.map(|_| 0),
            float: 0.0,
        };
        let (a, b) = (interval.start.max(0), interval.end.min(self.length));
        if b > a && *count > 0 {
            let support = |i: u64| {
                sample_support(runs.first, runs.period, runs.breaks, *count, self.length, i)
                    .expect("a sample")
            };
            // The first sample whose time reaches past `a`, and the first at or past `b`.
            let j = self.index_at(c, a)?;
            let lo = if j > 0 && support(j - 1).end > a {
                j - 1
            } else {
                j
            };
            let hi = self.index_at(c, b)?;
            let whole = if exact.is_some() && period.den() == 1 {
                self.whole_buckets(c, period.num() as i64, a, b, &mut t, cancel)?
            } else {
                Vec::new()
            };
            // Every sample outside the buckets taken, one by one.
            let mut from = lo;
            for r in whole.iter().chain(std::iter::once(&(i64::MAX..i64::MAX))) {
                let to = if r.start == i64::MAX {
                    hi
                } else {
                    self.index_at(c, r.start)?.min(hi)
                };
                if to > from {
                    let mut failed = None;
                    self.each_sample(c, from, to, cancel, |i, v, code| {
                        let s = support(i);
                        if failed.is_none()
                            && let Err(e) = t.sample(v, code, s.end.min(b) - s.start.max(a))
                        {
                            failed = Some(e);
                        }
                    })?;
                    if let Some(e) = failed {
                        return Err(e);
                    }
                }
                if r.start != i64::MAX {
                    from = self.index_at(c, r.end)?.max(from);
                }
            }
        }
        let some = |x: f64| (t.samples > 0).then_some(x);
        Ok(ChannelStatistics {
            samples: t.samples,
            measured: t.measured,
            min: some(t.min),
            max: some(t.max),
            sum: match exact {
                Some((scale, offset)) => Sum::Exact {
                    codes_ticks: t.codes.expect("exact"),
                    scale,
                    offset,
                },
                None => Sum::Float(t.float),
            },
        })
    }

    /// Adds to `t` the overview's whole buckets of [a, b) for a whole-tick period of `p` ticks:
    /// those whose every sample's time lies inside, a full period each, the coarsest first.
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
        let mut short: Vec<i64> = runs
            .breaks
            .iter()
            .map(|x| x.index - 1)
            .chain(std::iter::once(count - 1))
            .filter_map(|i| {
                let s =
                    sample_support(runs.first, runs.period, runs.breaks, *count, self.length, i)?;
                (s.end - s.start != p).then_some(s.start)
            })
            .collect();
        short.sort_unstable();
        let clean = |r: &Range<i64>| {
            let k = short.partition_point(|&x| x < r.start);
            short.get(k).is_none_or(|&x| x >= r.end)
        };
        let mut left: Vec<Range<i64>> = std::iter::once(a..region_end).collect();
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
                        if n == 0 {
                            continue;
                        }
                        t.samples += n;
                        t.measured = (n as i64)
                            .checked_mul(p)
                            .and_then(|x| t.measured.checked_add(x))
                            .ok_or(Error::Overflow)?;
                        t.value(buckets.min[o]);
                        t.value(buckets.max[o]);
                        let codes = t.codes.as_mut().expect("exact");
                        *codes = sums[o]
                            .checked_mul(i128::from(p))
                            .and_then(|x| codes.checked_add(x))
                            .ok_or(Error::Overflow)?;
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
