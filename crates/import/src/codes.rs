//! Recovering the integer codes behind analog values stored as f32.
//!
//! An instrument's ADC produces integer codes, and its software stores each as an f32 computed
//! from the code. When every stored value is `(offset + code × scale) as f32` for one offset and
//! scale, the codes can be stored instead and the values rebuilt bit for bit.
//!
//! # Algorithm
//!
//! 1. Collect the distinct values, at most 2^16 of them, all finite, and sort them.
//! 2. Assign codes, the smallest value 0. The smallest gap between neighbours is taken as m steps
//!    for m = 1, 2, ...; each gap must then be a whole number of steps to within a quarter step,
//!    the step re-estimated from the span covered so far. The first m that passes and yields an
//!    exact coding wins, so the coding uses the largest step that works.
//! 3. Each value is reproduced exactly by any real `offset + code × scale` inside the interval
//!    that rounds to it as f32. Those intervals are strips in the (scale, offset) plane, and the
//!    width of the offsets left at a given scale is a concave function of the scale. Ternary search
//!    finds its maximum; a negative maximum means no exact coding with these codes.
//! 4. Within the feasible region, try scales and offsets with the fewest significant digits
//!    first, then its middle. Every distinct value is checked bit for bit with the f64 arithmetic
//!    a decoder uses, and the first candidate that reproduces all of them wins.
//!
//! # Limits
//!
//! - Step 2 is sure to pass only when a step spans at least four f32 ulps of the values it
//!   separates. With finer steps an exact coding can be missed.
//! - The coding uses the largest step that works: if only every other code of an ADC occurs, the
//!   codes come back halved and the scale doubled.
//! - Code 0 is the smallest value, not the instrument's own code 0.
//! - A channel holding both +0.0 and -0.0 has no coding.

use std::collections::HashSet;

/// The largest number of bits a code may need.
pub const MAX_BITS: u8 = 16;

/// An exact integer coding of an analog channel's values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Codes {
    /// Every code is below `2^bits`.
    pub bits: u8,
    pub scale: f64,
    pub offset: f64,
}

impl Codes {
    /// The value a code stands for. A reader must compute exactly this: an f64 multiply, an f64
    /// add, then rounding to the nearest f32 with ties to even. A fused multiply-add rounds once
    /// instead of twice and can give a different f32.
    pub fn decode(&self, code: u16) -> f32 {
        (self.offset + f64::from(code) * self.scale) as f32
    }

    /// The code for a value, if one decodes to exactly the value.
    pub fn encode(&self, value: f32) -> Option<u16> {
        let code = ((f64::from(value) - self.offset) / self.scale).round();
        if !(0.0..f64::from(1u32 << self.bits)).contains(&code) {
            return None;
        }
        let code = code as u16;
        (self.decode(code).to_bits() == value.to_bits()).then_some(code)
    }
}

/// Finds `bits <= 16`, a scale and an offset such that every value is
/// `(offset + code as f64 * scale) as f32` bit for bit, for an integer code in `0..2^bits`. `None`
/// when no such coding is found, or there are no values.
pub fn recover_codes(values: impl IntoIterator<Item = f32>) -> Option<Codes> {
    let limit = 1usize << MAX_BITS;
    let mut distinct = HashSet::new();
    for value in values {
        if !value.is_finite() {
            return None;
        }
        if distinct.insert(value.to_bits()) && distinct.len() > limit {
            return None;
        }
    }
    let mut values: Vec<f32> = distinct.into_iter().map(f32::from_bits).collect();
    values.sort_by(f32::total_cmp);
    if values.windows(2).any(|w| w[0] == w[1]) {
        // +0.0 and -0.0.
        return None;
    }
    match values.len() {
        0 => return None,
        1 => {
            let (lo, hi) = rounding_interval(values[0]);
            return decimals(lo, hi).into_iter().find_map(|offset| {
                verified(
                    &values,
                    &[0.0],
                    Codes {
                        bits: 0,
                        scale: 1.0,
                        offset,
                    },
                )
            });
        }
        _ => {}
    }

    let v: Vec<f64> = values.iter().map(|&x| f64::from(x)).collect();
    let (lo, hi): (Vec<f64>, Vec<f64>) = values.iter().map(|&x| rounding_interval(x)).unzip();
    let span = v[v.len() - 1] - v[0];
    let gap = v
        .windows(2)
        .map(|w| w[1] - w[0])
        .fold(f64::INFINITY, f64::min);
    let max_code = (limit - 1) as f64;
    for m in 1u32.. {
        let step = gap / f64::from(m);
        if span / step > max_code + 0.5 {
            return None;
        }
        let Some(codes) = assign(&v, step) else {
            continue;
        };
        if codes[codes.len() - 1] > max_code {
            return None;
        }
        if let Some(found) = solve(&values, &codes, &lo, &hi) {
            return Some(found);
        }
    }
    unreachable!("the loop returns once the step is small enough")
}

/// The real numbers that round to `x` as f32 (ties to even), as a closed interval. Its ends are
/// midpoints between neighbouring f32s, which an f64 holds exactly.
fn rounding_interval(x: f32) -> (f64, f64) {
    let v = f64::from(x);
    let lo = if x == 0.0 && x.is_sign_positive() {
        0.0
    } else {
        (v + f64::from(x.next_down())) / 2.0
    };
    let hi = if x == 0.0 && x.is_sign_negative() {
        0.0
    } else {
        (v + f64::from(x.next_up())) / 2.0
    };
    (lo, hi)
}

/// Codes for sorted values, 0 for the first, each gap a whole number of steps.
fn assign(v: &[f64], step: f64) -> Option<Vec<f64>> {
    let mut codes = Vec::with_capacity(v.len());
    codes.push(0.0);
    let mut step = step;
    for i in 1..v.len() {
        let steps = (v[i] - v[i - 1]) / step;
        let whole = steps.round();
        if whole < 1.0 || (steps - whole).abs() > 0.25 {
            return None;
        }
        let code = codes[i - 1] + whole;
        codes.push(code);
        step = (v[i] - v[0]) / code;
    }
    Some(codes)
}

/// The range of offsets that put every value inside its interval at this scale, as (low, high);
/// empty when low > high.
fn offsets(scale: f64, codes: &[f64], lo: &[f64], hi: &[f64]) -> (f64, f64) {
    let mut low = f64::NEG_INFINITY;
    let mut high = f64::INFINITY;
    for i in 0..codes.len() {
        low = low.max(lo[i] - codes[i] * scale);
        high = high.min(hi[i] - codes[i] * scale);
    }
    (low, high)
}

fn solve(values: &[f32], codes: &[f64], lo: &[f64], hi: &[f64]) -> Option<Codes> {
    let n = codes.len() - 1;
    let top = codes[n];
    let width = |s: f64| {
        let (low, high) = offsets(s, codes, lo, hi);
        high - low
    };
    // Any feasible scale keeps the first and last values in their intervals.
    let (mut a, mut b) = ((lo[n] - hi[0]) / top, (hi[n] - lo[0]) / top);
    for _ in 0..400 {
        let m1 = a + (b - a) / 3.0;
        let m2 = b - (b - a) / 3.0;
        if !(a < m1 && m1 < m2 && m2 < b) {
            break;
        }
        if width(m1) < width(m2) {
            a = m1;
        } else {
            b = m2;
        }
    }
    let best = a + (b - a) / 2.0;
    if width(best) < 0.0 {
        return None;
    }

    // The feasible scales form an interval around `best`. Try the shortest decimal scales in it
    // first, each with the shortest offsets it allows.
    let bits = bits_for(top);
    let below = edge(best, best * (1.0 - 1e-12), &width);
    let above = edge(best, best * (1.0 + 1e-12), &width);
    for scale in decimals(below, above).into_iter().chain([best]) {
        let (low, high) = offsets(scale, codes, lo, hi);
        if low > high {
            continue;
        }
        for offset in decimals(low, high) {
            let found = verified(
                values,
                codes,
                Codes {
                    bits,
                    scale,
                    offset,
                },
            );
            if found.is_some() {
                return found;
            }
        }
    }
    None
}

/// The last scale from `inside` towards `outside` at which the width is still non-negative.
fn edge(inside: f64, outside: f64, width: &impl Fn(f64) -> f64) -> f64 {
    let (mut inside, mut outside) = (inside, outside);
    for _ in 0..64 {
        if width(outside) < 0.0 {
            break;
        }
        outside = inside + (outside - inside) * 2.0;
    }
    for _ in 0..200 {
        let mid = inside + (outside - inside) / 2.0;
        if mid == inside || mid == outside {
            break;
        }
        if width(mid) >= 0.0 {
            inside = mid;
        } else {
            outside = mid;
        }
    }
    inside
}

/// Numbers in `[low, high]`, fewest significant decimal digits first: zero if it holds zero, then
/// its middle rounded to 1, 2, ... 17 digits, then the middle itself.
fn decimals(low: f64, high: f64) -> Vec<f64> {
    let middle = low + (high - low) / 2.0;
    let mut found = Vec::new();
    if low <= 0.0 && 0.0 <= high {
        found.push(0.0);
    }
    for digits in 1..=17 {
        let candidate: f64 = format!("{:.*e}", digits - 1, middle)
            .parse()
            .expect("formatted float parses");
        if low <= candidate && candidate <= high && !found.contains(&candidate) {
            found.push(candidate);
        }
    }
    if !found.contains(&middle) {
        found.push(middle);
    }
    found
}

fn bits_for(top: f64) -> u8 {
    let top = top as u32;
    (u32::BITS - top.leading_zeros()) as u8
}

/// `codes` if every value decodes from its code bit for bit.
fn verified(values: &[f32], codes: &[f64], candidate: Codes) -> Option<Codes> {
    if !candidate.scale.is_finite() || !candidate.offset.is_finite() {
        return None;
    }
    values
        .iter()
        .zip(codes)
        .all(|(&value, &code)| candidate.decode(code as u16).to_bits() == value.to_bits())
        .then_some(candidate)
}
