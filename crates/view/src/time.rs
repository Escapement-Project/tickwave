//! Times as written in attributes and annotations: an integer tick count, or a decimal with a
//! unit (s, ms, us, ns, ps; also µs). Converted to ticks exactly, rounding to the nearest tick. A
//! port of web/src/time.ts.

use escapement_tickwave_format::model::Ratio;
use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive};

use crate::js;

/// A decimal number of seconds, mantissa × 10^exp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seconds {
    pub mantissa: BigInt,
    pub exp: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimeSpec {
    Ticks(i64),
    Seconds(Seconds),
}

/// The largest integer an f64 holds with every integer below it: JavaScript's
/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE: u64 = (1 << 53) - 1;

fn unit_exp(unit: &str) -> Option<i32> {
    Some(match unit {
        "s" => 0,
        "ms" => -3,
        "us" | "µs" | "μs" => -6,
        "ns" => -9,
        "ps" => -12,
        _ => return None,
    })
}

/// Parses "1234" (ticks) or "317.85882 ms". None when it is neither, or a tick count is past
/// 2^53 − 1.
pub fn parse_time(text: &str) -> Option<TimeSpec> {
    let t = text.trim_matches(js::is_space);
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if digits(t) {
        let ticks: BigInt = t.parse().ok()?;
        return ticks
            .to_u64()
            .filter(|&n| n <= MAX_SAFE)
            .map(|n| TimeSpec::Ticks(n as i64));
    }
    let (negative, rest) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t),
    };
    let number_end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    let (number, unit) = rest.split_at(number_end);
    let (int, frac) = number.split_once('.').unwrap_or((number, ""));
    if !digits(int) || (number.contains('.') && !digits(frac)) {
        return None;
    }
    let exp = unit_exp(unit.trim_start_matches(js::is_space))?;
    let mantissa: BigInt = format!("{int}{frac}").parse().ok()?;
    Some(TimeSpec::Seconds(Seconds {
        mantissa: if negative { -mantissa } else { mantissa },
        exp: exp - frac.len() as i32,
    }))
}

/// Seconds to ticks, rounded to the nearest tick, halves away from zero. None past an i64.
pub fn seconds_to_ticks(s: &Seconds, timebase: Ratio) -> Option<i64> {
    // mantissa × 10^exp × num / den
    let mut n = &s.mantissa * BigInt::from(timebase.num());
    let mut d = BigInt::from(timebase.den());
    let scale = BigInt::from(10u8).pow(s.exp.unsigned_abs());
    if s.exp >= 0 {
        n *= scale;
    } else {
        d *= scale;
    }
    let q = (BigInt::from(2u8) * n.abs() + &d) / (BigInt::from(2u8) * d);
    (if n.is_negative() { -q } else { q }).to_i64()
}

pub fn to_ticks(spec: &TimeSpec, timebase: Ratio) -> Option<i64> {
    match spec {
        TimeSpec::Ticks(t) => Some(*t),
        TimeSpec::Seconds(s) => seconds_to_ticks(s, timebase),
    }
}

/// Seconds as a float, for scales and paddings where a tick's rounding does not matter.
pub fn seconds_value(s: &Seconds) -> f64 {
    js::bigint_to_f64(&s.mantissa) * 10f64.powf(f64::from(s.exp))
}
