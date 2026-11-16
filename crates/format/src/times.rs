//! Times as a sidecar or an annotation part writes them (ANNOTATIONS.md, "Times"): a whole tick
//! count, or a decimal with a unit, converted to ticks exactly.

use serde::Deserialize;

use crate::model::Ratio;

/// A time as a file writes it: a tick count, or a decimal with a unit.
#[derive(Clone, Debug, PartialEq)]
pub enum Time {
    Ticks(i64),
    Text(String),
}

impl<'de> Deserialize<'de> for Time {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visit;
        impl serde::de::Visitor<'_> for Visit {
            type Value = Time;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(r#"a whole tick count or a decimal with a unit, as "1.5 us""#)
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Time, E> {
                Ok(Time::Ticks(v))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Time, E> {
                i64::try_from(v)
                    .map(Time::Ticks)
                    .map_err(|_| E::custom(format!("{v} ticks is out of range")))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Time, E> {
                Ok(Time::Text(v.to_owned()))
            }
        }
        d.deserialize_any(Visit)
    }
}

/// Converts a time to ticks, rounding to the nearest tick (halves away from zero). A text time is
/// a decimal and a unit (`s`, `ms`, `us`, `µs`, `ns`, `ps`), with or without a space between.
/// The arithmetic is exact: `0.1 ms` at 1 GS/s is 100,000 ticks, not 99,999.
pub fn ticks(time: &Time, timebase: Ratio) -> Result<i64, String> {
    let text = match time {
        Time::Ticks(t) => return Ok(*t),
        Time::Text(s) => s.trim(),
    };
    exact_ticks(text, timebase)?
        .rounded()
        .ok_or_else(|| format!("{text:?}: out of range"))
}

/// A text time in ticks, exactly: `numerator / denominator`, negated when `negative`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExactTicks {
    pub negative: bool,
    pub numerator: i128,
    /// Positive.
    pub denominator: i128,
}

impl ExactTicks {
    /// The nearest tick, halves away from zero; None where it would pass i64.
    pub fn rounded(&self) -> Option<i64> {
        // Halves away from zero, on the magnitude. Checked: 2 × numerator can pass i128 where the
        // quotient would not, and a wrapped sum is a wrong tick, not an error.
        let rounded = self
            .numerator
            .checked_mul(2)
            .and_then(|n| n.checked_add(self.denominator))
            .zip(self.denominator.checked_mul(2))
            .map(|(n, d)| n / d)?;
        let value = i64::try_from(rounded).ok()?;
        Some(if self.negative { -value } else { value })
    }
}

/// A text time, a decimal and a unit, in ticks, exactly; `ticks` rounds it.
pub fn exact_ticks(text: &str, timebase: Ratio) -> Result<ExactTicks, String> {
    let text = text.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .ok_or_else(|| format!("{text:?}: no unit"))?;
    let (number, unit) = (text[..split].trim(), text[split..].trim());
    let unit_exp: u32 = match unit {
        "s" => 0,
        "ms" => 3,
        "us" | "µs" => 6,
        "ns" => 9,
        "ps" => 12,
        _ => return Err(format!("{text:?}: unknown unit {unit:?}")),
    };
    let (negative, digits) = match number.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, number.strip_prefix('+').unwrap_or(number)),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole
            .chars()
            .chain(fraction.chars())
            .all(|c| c.is_ascii_digit())
    {
        return Err(format!("{text:?}: not a decimal"));
    }
    let too_big = || format!("{text:?}: out of range");
    let mantissa: i128 = format!("{whole}{fraction}")
        .parse()
        .map_err(|_| too_big())?;
    // seconds = mantissa / 10^(fraction digits + unit exponent); ticks = seconds × num / den.
    let scale_exp = fraction.len() as u32 + unit_exp;
    let denominator = 10i128
        .checked_pow(scale_exp)
        .and_then(|p| p.checked_mul(i128::from(timebase.den())))
        .ok_or_else(too_big)?;
    let numerator = mantissa
        .checked_mul(i128::from(timebase.num()))
        .ok_or_else(too_big)?;
    Ok(ExactTicks {
        negative,
        numerator,
        denominator,
    })
}
