//! Readouts: times, rates and values shown with the digits their source supports and no more.
//! A port of web/src/readouts.ts, with the same inputs and outputs.
//!
//! Arithmetic is exact (big-integer rationals), so a digit count never depends on how a float
//! happened to round. A number is shown by rounding it at a "place", a power of ten; the place is
//! chosen from the number's resolution, and the unit from the rounded number's magnitude. An f64
//! goes in as the decimal it prints as, its shortest round-trip digits, as the web's `String(x)`
//! gives them.

use std::borrow::Cow;
use std::cmp::Ordering;

use escapement_tickwave_format::model::{Exact, Ratio};
use num_bigint::BigInt;
use num_traits::{Signed, Zero};

use crate::js;

pub const NBSP: char = '\u{a0}';
pub const MINUS: char = '\u{2212}';

/// A unit and the power of ten it stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub exp: i32,
    pub name: Cow<'static, str>,
}

const fn unit(exp: i32, name: &'static str) -> Unit {
    Unit {
        exp,
        name: Cow::Borrowed(name),
    }
}

pub const TIME_UNITS: &[Unit] = &[
    unit(0, "s"),
    unit(-3, "ms"),
    unit(-6, "µs"),
    unit(-9, "ns"),
    unit(-12, "ps"),
];

pub const RATE_UNITS: &[Unit] = &[
    unit(9, "GHz"),
    unit(6, "MHz"),
    unit(3, "kHz"),
    unit(0, "Hz"),
];

/// SI prefixes for a value's unit, largest first.
pub fn prefixed_units(unit: &str) -> Vec<Unit> {
    [
        (9, "G"),
        (6, "M"),
        (3, "k"),
        (0, ""),
        (-3, "m"),
        (-6, "µ"),
        (-9, "n"),
        (-12, "p"),
    ]
    .into_iter()
    .map(|(exp, prefix)| Unit {
        exp,
        name: format!("{prefix}{unit}").into(),
    })
    .collect()
}

fn pow10(e: u32) -> BigInt {
    BigInt::from(10u8).pow(e)
}

/// Exactly the decimal a finite number prints as, as a fraction n / d with d > 0. None for a
/// number that isn't finite.
pub fn rational(x: f64) -> Option<(BigInt, BigInt)> {
    if !x.is_finite() {
        return None;
    }
    if x == 0.0 {
        return Some((BigInt::ZERO, BigInt::from(1u8)));
    }
    let (digits, n) = js::shortest(x);
    let mut num: BigInt = digits.parse().expect("decimal digits");
    let e = n - digits.len() as i32;
    let mut den = BigInt::from(1u8);
    if e >= 0 {
        num *= pow10(e.unsigned_abs());
    } else {
        den = pow10(e.unsigned_abs());
    }
    Some((if x < 0.0 { -num } else { num }, den))
}

/// n/d compared with 10^p. Requires n, d > 0.
fn cmp_pow10(n: &BigInt, d: &BigInt, p: i32) -> Ordering {
    if p >= 0 {
        n.cmp(&(d * pow10(p.unsigned_abs())))
    } else {
        (n * pow10(p.unsigned_abs())).cmp(d)
    }
}

/// The exponent of the largest power of ten not larger than n/d, for n/d > 0.
///
/// # Panics
///
/// If n or d is not positive.
pub fn place_of(n: &BigInt, d: &BigInt) -> i32 {
    assert!(
        n.is_positive() && d.is_positive(),
        "place_of needs a positive ratio"
    );
    // The difference in digit counts is the place or one above it, never below.
    let mut p = n.to_string().len() as i32 - d.to_string().len() as i32;
    if cmp_pow10(n, d, p) == Ordering::Less {
        p -= 1;
    }
    p
}

/// n / (d × 10^p), rounded to an integer, halves away from zero. Requires d > 0.
pub fn round_at(n: &BigInt, d: &BigInt, p: i32) -> BigInt {
    let (a, b) = if p >= 0 {
        (n.clone(), d * pow10(p.unsigned_abs()))
    } else {
        (n * pow10(p.unsigned_abs()), d.clone())
    };
    let q = (BigInt::from(2u8) * a.abs() + &b) / (BigInt::from(2u8) * b);
    if a.is_negative() { -q } else { q }
}

fn digits_at(scaled: &BigInt, p: i32, unit: &Unit) -> String {
    let mut s = scaled.magnitude().to_string();
    if p >= unit.exp {
        if s != "0" {
            s.push_str(&"0".repeat((p - unit.exp) as usize));
        }
    } else {
        let decimals = (unit.exp - p) as usize;
        if s.len() < decimals + 1 {
            s = format!("{}{s}", "0".repeat(decimals + 1 - s.len()));
        }
        s.insert(s.len() - decimals, '.');
    }
    let sign = if scaled.is_negative() {
        MINUS.to_string()
    } else {
        String::new()
    };
    format!("{sign}{s}{NBSP}{}", unit.name)
}

/// The unit for `scaled` × 10^p: the largest not above its magnitude.
pub fn unit_for<'a>(scaled: &BigInt, p: i32, units: &'a [Unit]) -> &'a Unit {
    let last = units.last().expect("at least one unit");
    if scaled.is_zero() {
        return units.iter().find(|u| u.exp <= p).unwrap_or(last);
    }
    let abs = scaled.magnitude();
    units
        .iter()
        .find(|u| {
            let step = u.exp - p;
            step <= 0 || *abs >= *pow10(step.unsigned_abs()).magnitude()
        })
        .unwrap_or(last)
}

/// `scaled` × 10^p, in `unit` or the unit its magnitude picks.
pub fn format_scaled(scaled: &BigInt, p: i32, units: &[Unit], unit: Option<&Unit>) -> String {
    digits_at(
        scaled,
        p,
        unit.unwrap_or_else(|| unit_for(scaled, p, units)),
    )
}

fn big(x: impl Into<BigInt>) -> BigInt {
    x.into()
}

/// The place of one tick: the largest power of ten not larger than a tick's length. An interval
/// is a whole number of ticks, so its last digit sits here. At 1 GS/s that is 1 ns; at 200 kS/s
/// (5 us) it is 1 us, and every interval is then exact at that place. `step` is the ticks one
/// tick of the reading's source spans, its `m` (FORMAT.md, "Times"): 1 for a recording of one
/// source, a ratio where a source's tick is not a whole number of the recording's.
pub fn tick_place(timebase: Ratio, step: Ratio) -> i32 {
    place_of(
        &(big(timebase.den()) * big(step.num())),
        &(big(timebase.num()) * big(step.den())),
    )
}

/// An interval of a whole number of ticks: 1234567 ticks at 1 GS/s is 1.234567 ms. `step` is
/// the ticks one tick of the reading's source spans, where a merged recording ticks finer than
/// its sources: the last digit sits at the source's tick.
pub fn format_interval(ticks: i64, timebase: Ratio, step: Ratio) -> String {
    let p = tick_place(timebase, step);
    let scaled = round_at(&(big(ticks) * big(timebase.den())), &big(timebase.num()), p);
    format_scaled(&scaled, p, TIME_UNITS, None)
}

/// An interval to three significant figures, for a summary line: 927888190 ticks at 1 GS/s is
/// 928 ms. Never finer than one tick.
pub fn format_brief(ticks: i64, timebase: Ratio, step: Ratio) -> String {
    let n = big(ticks.unsigned_abs()) * big(timebase.den());
    let d = big(timebase.num());
    let tick = tick_place(timebase, step);
    if n.is_zero() {
        return format_scaled(&n, tick, TIME_UNITS, None);
    }
    let mut p = (place_of(&n, &d) - 2).max(tick);
    let mut scaled = round_at(&n, &d, p);
    // Rounding up can add a digit (999.6 ms to 1000 ms): one place coarser then.
    if scaled >= big(1000) {
        p += 1;
        scaled = round_at(&n, &d, p);
    }
    let scaled = if ticks < 0 { -scaled } else { scaled };
    format_scaled(&scaled, p, TIME_UNITS, None)
}

/// The mean rate over `periods` periods that together span `span` ticks, in Hz with an SI
/// prefix. None unless all three counts are positive.
///
/// The digits: each edge is recorded at the first sample after it, so an edge's tick is late by
/// less than one tick, and a span between two edges is wrong by less than one tick. Over `runs`
/// separate runs of periods the total is wrong by less than `runs` ticks, so the rate
/// f = periods / span is uncertain by about f × runs / span. The last digit shown is at the
/// largest power of ten not larger than that uncertainty.
pub fn format_rate(
    periods: u64,
    span: i64,
    timebase: Ratio,
    runs: u64,
    step: Ratio,
) -> Option<String> {
    if periods == 0 || span <= 0 || runs == 0 {
        return None;
    }
    let n = big(periods) * big(timebase.num());
    let d = big(span) * big(timebase.den());
    // Each run's span is wrong by less than one tick of the source: `step` of these ticks.
    let p = place_of(
        &(&n * big(runs) * big(step.num())),
        &(&d * big(span) * big(step.den())),
    );
    Some(format_scaled(&round_at(&n, &d, p), p, RATE_UNITS, None))
}

/// The mean of `periods` periods that together span `span` ticks: span / periods ticks. By the
/// rule above the span is wrong by less than `runs` ticks, so the mean is uncertain by
/// runs / periods ticks, and may carry digits finer than one tick.
pub fn format_mean_period(
    periods: u64,
    span: i64,
    timebase: Ratio,
    runs: u64,
    step: Ratio,
) -> Option<String> {
    if periods == 0 || span <= 0 || runs == 0 {
        return None;
    }
    let n = big(span) * big(timebase.den());
    let d = big(periods) * big(timebase.num());
    let p = place_of(
        &(big(runs) * big(step.num()) * big(timebase.den())),
        &(&d * big(step.den())),
    );
    Some(format_scaled(&round_at(&n, &d, p), p, TIME_UNITS, None))
}

/// The value n/d to the place of one code step, step_n/step_d where there is one, and to
/// `significant` figures, whichever is coarser. Without a step it gets four figures unless it
/// states otherwise.
fn format_exact(
    n: &BigInt,
    d: &BigInt,
    step: Option<(BigInt, BigInt)>,
    unit: &str,
    significant: Option<u32>,
) -> String {
    let figures = significant.or(if step.is_some() { None } else { Some(4) });
    let mut p = step.map(|(sn, sd)| place_of(&sn, &sd));
    // Zero has no leading digit to count figures from: it keeps to the code step where there is
    // one.
    if let Some(figures) = figures
        && (!n.is_zero() || p.is_none())
    {
        let by_figures = if n.is_zero() {
            0
        } else {
            place_of(&n.abs(), d) - (figures as i32 - 1)
        };
        p = Some(p.map_or(by_figures, |p| p.max(by_figures)));
    }
    let p = p.expect("a step or a figure count sets the place");
    format_scaled(&round_at(n, d, p), p, &prefixed_units(unit), None)
}

/// A value of an analog channel, to the digit of one code step (`resolution`), and to
/// `significant` figures where the channel states them, whichever is coarser. A source that
/// stored floating-point values states no resolution, and gets four significant figures unless
/// it states otherwise. None for a value or a resolution that isn't finite.
pub fn format_value(
    value: f64,
    resolution: Option<f64>,
    unit: &str,
    significant: Option<u32>,
) -> Option<String> {
    let (n, d) = rational(value)?;
    let step = match resolution {
        Some(r) if r > 0.0 => Some(rational(r)?),
        _ => None,
    };
    Some(format_exact(&n, &d, step, unit, significant))
}

/// The value of `code` on an `int` channel, `code × scale + offset`, exactly, with no f64 on the
/// way: to the digit of one code step, |scale|, and to `significant` figures where the channel
/// states them, whichever is coarser. An integer product's code and scale read the same way.
pub fn format_code(
    code: i128,
    scale: Exact,
    offset: Exact,
    unit: &str,
    significant: Option<u32>,
) -> String {
    let (sn, sd) = (big(scale.num()), big(scale.den()));
    let (on, od) = (big(offset.num()), big(offset.den()));
    let n = big(code) * &sn * &od + &on * &sd;
    let d = &sd * &od;
    let step = (!sn.is_zero()).then(|| (sn.abs(), sd));
    format_exact(&n, &d, step, unit, significant)
}

/// `n` with its digits grouped in threes, as `Intl.NumberFormat("en")` writes an integer.
pub fn format_count(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The periods of a digital channel's running clock (ANNOTATIONS.md, "Measures").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeriodStats {
    /// Periods between consecutive rising edges.
    pub count: u64,
    /// In ticks. The median of an even count is the lower middle, so it is a period that
    /// occurred.
    pub median: i64,
    pub min: i64,
    pub max: i64,
    /// The running clock: the periods inside Tukey's fences. The fences keep a clock's jitter
    /// and quantisation, and drop pauses and stretched cycles that occur in fewer than a quarter
    /// of the periods. `used` periods, in `runs` runs of consecutive periods, summing to `span`
    /// ticks.
    pub used: u64,
    pub runs: u64,
    pub span: i64,
}

/// Periods measured rising edge to rising edge. `level` is the level before `ticks[0]`. None
/// when there are fewer than two rising edges.
pub fn period_stats(level: bool, ticks: &[i64]) -> Option<PeriodStats> {
    // Edge i is rising when the level before it is low; the level before edge i is
    // level ^ (i & 1).
    let first = usize::from(level);
    let count = ticks.len().saturating_sub(first + 1) / 2;
    if count == 0 {
        return None;
    }
    let periods: Vec<i64> = (0..count)
        .map(|k| ticks[first + 2 * k + 2] - ticks[first + 2 * k])
        .collect();
    let mut sorted = periods.clone();
    sorted.sort_unstable();
    let median = sorted[(count - 1) >> 1];
    // Tukey's fences: Q1 = p[⌊(n − 1)/4⌋], Q3 = p[⌈3(n − 1)/4⌉], IQR at least one tick.
    let q1 = sorted[(count - 1) / 4];
    let q3 = sorted[(3 * (count - 1)).div_ceil(4)];
    let iqr = (q3 - q1).max(1);
    let (lo, hi) = (q1 as f64 - 1.5 * iqr as f64, q3 as f64 + 1.5 * iqr as f64);
    let (mut used, mut runs, mut span, mut in_run) = (0, 0, 0, false);
    for &p in &periods {
        if p as f64 >= lo && p as f64 <= hi {
            used += 1;
            span += p;
            if !in_run {
                runs += 1;
            }
            in_run = true;
        } else {
            in_run = false;
        }
    }
    Some(PeriodStats {
        count: count as u64,
        median,
        min: sorted[0],
        max: sorted[count - 1],
        used,
        runs,
        span,
    })
}

/// The running clock's rate, with its digits. None where the periods it keeps span no time.
pub fn format_clock_rate(s: &PeriodStats, timebase: Ratio, step: Ratio) -> Option<String> {
    format_rate(s.used, s.span, timebase, s.runs, step)
}

/// The running clock's period, with its digits. None where the periods it keeps span no time.
pub fn format_clock_period(s: &PeriodStats, timebase: Ratio, step: Ratio) -> Option<String> {
    format_mean_period(s.used, s.span, timebase, s.runs, step)
}

/// "mean of 31 of 32 periods", "mean of 7 periods" when it kept them all, "one of 7 periods",
/// "one period".
pub fn clock_note(s: &PeriodStats) -> String {
    let count = |n: u64| format_count(n as i64);
    if s.used == 1 {
        return if s.count == 1 {
            "one period".into()
        } else {
            format!("one of {} periods", count(s.count))
        };
    }
    if s.used == s.count {
        format!("mean of {} periods", count(s.count))
    } else {
        format!("mean of {} of {} periods", count(s.used), count(s.count))
    }
}

/// Edges in [start, end) of an ascending tick array.
pub fn count_edges(ticks: &[i64], start: i64, end: i64) -> usize {
    let lower = |x: i64| ticks.partition_point(|&t| t < x);
    lower(end).saturating_sub(lower(start))
}

/// A value known to be a multiple of 10^place (an axis label), in `unit` with an SI prefix.
/// None for a value that isn't finite.
pub fn format_at(value: f64, place: i32, unit: &str) -> Option<String> {
    let (n, d) = rational(value)?;
    Some(format_scaled(
        &round_at(&n, &d, place),
        place,
        &prefixed_units(unit),
        None,
    ))
}

/// A value range's two marks, each a multiple of 10^place (an axis's labels), in the one unit the
/// larger magnitude picks, so both read alike: 0 to 2.0 mA is "2.0 mA" and "0.0 mA". None for a
/// mark that isn't finite.
pub fn format_marks(top: f64, bottom: f64, place: i32, unit: &str) -> Option<(String, String)> {
    let scaled = |v: f64| rational(v).map(|(n, d)| round_at(&n, &d, place));
    let (t, b) = (scaled(top)?, scaled(bottom)?);
    let units = prefixed_units(unit);
    let larger = if t.magnitude() >= b.magnitude() {
        &t
    } else {
        &b
    };
    let u = unit_for(larger, place, &units);
    Some((
        format_scaled(&t, place, &units, Some(u)),
        format_scaled(&b, place, &units, Some(u)),
    ))
}

/// 1, 2 or 5 times a power of ten, and that power's exponent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NiceStep {
    pub step: f64,
    pub place: i32,
}

/// The smallest of 1, 2 or 5 times a power of ten not below `x`. None unless `x` is finite and
/// positive.
pub fn nice_step(x: f64) -> Option<NiceStep> {
    if !(x.is_finite() && x > 0.0) {
        return None;
    }
    let mut place = x.log10().floor() as i32;
    loop {
        for m in [1, 2, 5] {
            let step: f64 = format!("{m}e{place}").parse().expect("a decimal parses");
            if step >= x * (1.0 - 1e-12) {
                return Some(NiceStep { step, place });
            }
        }
        place += 1;
    }
}

/// What a `measure` annotation reads (ANNOTATIONS.md, "Measures").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    Rate,
    Period,
    Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    pub value: String,
    pub note: String,
}

/// A `measure` reading over a span's edges: `ticks` are the edges at ticks in [start, end), and
/// `level` the level before the first. `Rate` and `Period` read the running clock; `Duration`
/// the time from the first edge to the last, of either polarity, to the tick of the channel's
/// source: `step` is that tick in axis ticks, its `m`. None when the span has too few edges to say
/// anything.
pub fn measure_reading(
    what: Measure,
    level: bool,
    ticks: &[i64],
    timebase: Ratio,
    step: Ratio,
) -> Option<Reading> {
    if what == Measure::Duration {
        if ticks.len() < 2 {
            return None;
        }
        return Some(Reading {
            value: format_interval(ticks[ticks.len() - 1] - ticks[0], timebase, step),
            note: "first edge to last".into(),
        });
    }
    let Some(stats) = period_stats(level, ticks) else {
        return Some(Reading {
            value: "\u{2013}".into(),
            note: "no full period".into(),
        });
    };
    let value = match what {
        Measure::Rate => format_clock_rate(&stats, timebase, step),
        _ => format_clock_period(&stats, timebase, step),
    };
    Some(Reading {
        value: value.unwrap_or_else(|| "\u{2013}".into()),
        note: clock_note(&stats),
    })
}
