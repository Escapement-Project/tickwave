//! Where a channel's source ticks lie on the recording's axis (FORMAT.md, "Times"):
//! `T = origin + floor((k + offset) × m)`, `m = stretch × R / rate`, exact.

use crate::header::JSON_SAFE;
use crate::model::Ratio;

pub(crate) fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A source's `m`, `stretch × recording / rate`, in lowest terms: each of the three terms on top
/// cancelled against each of the three below before multiplying. None unless both terms lie
/// below 2^53.
pub fn axis_ratio(stretch: Ratio, recording: Ratio, rate: Ratio) -> Option<Ratio> {
    let mut top = [stretch.num(), recording.num(), rate.den()];
    let mut below = [stretch.den(), recording.den(), rate.num()];
    for t in &mut top {
        for b in &mut below {
            let g = gcd(*t, *b);
            *t /= g;
            *b /= g;
        }
    }
    let product = |terms: [u64; 3]| {
        terms
            .iter()
            .try_fold(1u64, |p, &t| p.checked_mul(t))
            .filter(|&p| p < JSON_SAFE)
    };
    Ratio::new(product(top)?, product(below)?)
}

/// A channel's mapping from its source's ticks to the axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    origin: i64,
    offset: i64,
    /// `m` in lowest terms.
    num: u64,
    den: u64,
}

impl Mapping {
    /// Source tick `k` at axis tick `k`.
    pub const IDENTITY: Mapping = Mapping {
        origin: 0,
        offset: 0,
        num: 1,
        den: 1,
    };

    /// `m` must be in lowest terms, as `axis_ratio` gives it.
    pub fn new(origin: i64, offset: i64, m: Ratio) -> Self {
        Self {
            origin,
            offset,
            num: m.num(),
            den: m.den(),
        }
    }

    #[inline]
    pub fn origin(&self) -> i64 {
        self.origin
    }

    #[inline]
    pub fn offset(&self) -> i64 {
        self.offset
    }

    /// How many axis ticks one source tick lasts, in lowest terms.
    #[inline]
    pub fn m(&self) -> Ratio {
        Ratio::new(self.num, self.den).expect("positive terms")
    }

    #[inline]
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// The axis tick source tick `k` maps to, exactly; None where it does not fit an i64. Any `k`
    /// is safe: `|k + offset| < 2^64` and `m`'s numerator is below 2^53, so the product fits
    /// 128 bits.
    #[inline]
    pub fn axis(&self, k: i64) -> Option<i64> {
        let x = i128::from(k) + i128::from(self.offset);
        i64::try_from(floor_scaled(x, self.num, self.den) + i128::from(self.origin)).ok()
    }

    /// `axis`, saturating at the ends of i64 where the tick does not fit one.
    #[inline]
    pub fn axis_saturating(&self, k: i64) -> i64 {
        let x = i128::from(k) + i128::from(self.offset);
        let t = floor_scaled(x, self.num, self.den) + i128::from(self.origin);
        t.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }

    /// The axis tick a place of `units / d` source ticks maps to, exactly: `origin + floor((units
    /// + offset × d) × m / d)` (FORMAT.md, "Times"). None where it does not fit an i64, or the
    /// product passes 128 bits, which no place a header allows does: with `|units| < 2^70` and
    /// `m`'s terms below 2^53 it stays below 2^123.
    #[inline]
    pub fn axis_place(&self, units: i128, d: u64) -> Option<i64> {
        if d == 1 {
            let k = i64::try_from(units).ok()?;
            return self.axis(k);
        }
        let x = units.checked_add(i128::from(self.offset).checked_mul(i128::from(d))?)?;
        let p = x.checked_mul(i128::from(self.num))?;
        let q = i128::from(self.den).checked_mul(i128::from(d))?;
        i64::try_from(p.div_euclid(q).checked_add(i128::from(self.origin))?).ok()
    }

    /// The axis time a place of `units / d` source ticks maps to, exactly, unrounded, counted in
    /// `1 / (d × den)` axis ticks, `den` being `m`'s: `origin × d × den + (units + offset × d) ×
    /// num`. `axis_place` is its floor in whole ticks. None where it passes 128 bits.
    #[inline]
    pub fn axis_time(&self, units: i128, d: u64) -> Option<i128> {
        let per = i128::from(d).checked_mul(i128::from(self.den))?;
        let at = units.checked_add(i128::from(self.offset).checked_mul(i128::from(d))?)?;
        i128::from(self.origin)
            .checked_mul(per)?
            .checked_add(at.checked_mul(i128::from(self.num))?)
    }

    /// The source time axis tick `t` maps back to, exactly: `(t − origin) / m − offset`, as a
    /// numerator and a positive denominator, `m`'s numerator (FORMAT.md, "Gaps").
    pub fn source_time(&self, t: i64) -> (i128, i128) {
        let num = (i128::from(t) - i128::from(self.origin)) * i128::from(self.den)
            - i128::from(self.offset) * i128::from(self.num);
        (num, i128::from(self.num))
    }

    /// The least source tick that maps to axis tick `t` or later, saturating at the ends of i64.
    pub fn first_at(&self, t: i64) -> i64 {
        let y = i128::from(t) - i128::from(self.origin);
        // ceil(y × den / num) = −floor(−y × den / num).
        let k = -floor_scaled(-y, self.den, self.num) - i128::from(self.offset);
        k.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }
}

/// `floor(x × num / den)` for `|x| < 2^65` and terms below 2^53. A product that fits 64 bits
/// divides in 64 bits, which costs far less than 128, and a whole `m` divides not at all.
#[inline]
fn floor_scaled(x: i128, num: u64, den: u64) -> i128 {
    let p = x * i128::from(num);
    if den == 1 {
        return p;
    }
    match i64::try_from(p) {
        // `den` is below 2^53, so it fits an i64.
        Ok(p) => i128::from(p.div_euclid(den as i64)),
        Err(_) => p.div_euclid(i128::from(den)),
    }
}

/// `a × b` as 256 bits, high half first.
fn mul_wide(a: u128, b: u128) -> (u128, u128) {
    let (a1, a0) = (a >> 64, a & u128::from(u64::MAX));
    let (b1, b0) = (b >> 64, b & u128::from(u64::MAX));
    let low = a0 * b0;
    let mid1 = a0 * b1;
    let mid2 = a1 * b0;
    let high = a1 * b1;
    let (mid, carry) = mid1.overflowing_add(mid2);
    let (lo, c1) = low.overflowing_add(mid << 64);
    let hi = high + (mid >> 64) + (u128::from(carry) << 64) + u128::from(c1);
    (hi, lo)
}

/// A 256-bit number shifted left by `s` bits, or None if bits fall off the top.
fn shl_wide((hi, lo): (u128, u128), s: u32) -> Option<(u128, u128)> {
    if s == 0 {
        return Some((hi, lo));
    }
    if s >= 256 {
        return (hi == 0 && lo == 0).then_some((0, 0));
    }
    if s >= 128 {
        let s = s - 128;
        return (hi == 0 && (s == 0 || lo >> (128 - s) == 0)).then_some((lo << s, 0));
    }
    if hi >> (128 - s) != 0 {
        return None;
    }
    Some(((hi << s) | (lo >> (128 - s)), lo << s))
}

fn bits(x: u128) -> i32 {
    128 - x.leading_zeros() as i32
}

/// An interval between items in axis ticks, exact: `num / den`, both positive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spacing {
    num: u128,
    den: u128,
}

impl Spacing {
    pub fn new(num: u128, den: u128) -> Option<Self> {
        (num > 0 && den > 0).then_some(Self { num, den })
    }

    /// An analog channel's: its period, in its source's ticks, times its source's `m`.
    pub fn of_samples(period: Ratio, m: Ratio) -> Self {
        Self {
            num: u128::from(period.num()) * u128::from(m.num()),
            den: u128::from(period.den()) * u128::from(m.den()),
        }
    }

    /// A digital channel's: `span` axis ticks over `edges` edges; None for no edge, or no span.
    pub fn of_edges(span: u64, edges: u64) -> Option<Self> {
        Self::new(u128::from(span), u128::from(edges))
    }

    /// Whether this spacing is closer than `other`: `a/b < c/d` as `a × d < c × b`.
    pub fn closer_than(self, other: Spacing) -> bool {
        mul_wide(self.num, other.den) < mul_wide(other.num, self.den)
    }

    /// The closer of two spacings.
    pub fn closest(spacings: impl IntoIterator<Item = Spacing>) -> Option<Spacing> {
        spacings
            .into_iter()
            .reduce(|a, b| if b.closer_than(a) { b } else { a })
    }
}

/// The items a bucket of a channel's finest overview level aims to hold.
pub const ITEMS_A_BUCKET: u128 = 64;

/// A channel's finest overview level's shift for its spacing (FORMAT.md, "Overview"): the whole
/// number nearest `log2(64 × t)`, held to `0..=62`. That is the `s` for which `(64 t)²` lies
/// between `2^(2s − 1)` and `2^(2s + 1)`; no ratio falls on either bound. With no item at all, 62.
pub fn finest_shift(spacing: Option<Spacing>) -> u8 {
    const MAX: i32 = crate::overview::MAX_SHIFT as i32;
    let Some(t) = spacing else {
        return MAX as u8;
    };
    // x = a / b with a = 64 × num, b = den; num and den are below 2^106 for every spacing the
    // format gives, and a ratio this far out still rounds right as long as a fits.
    let Some(a) = t.num.checked_mul(ITEMS_A_BUCKET) else {
        return MAX as u8;
    };
    let b = t.den;
    // e = floor(log2(a / b)): 2^e ≤ a / b < 2^(e + 1). With e the difference in bit lengths,
    // a / b lies within a factor of 2 of 2^e, and the shifted side has a's or b's bit length.
    let mut e = bits(a) - bits(b);
    let at_least = if e >= 0 { a >= b << e } else { a << -e >= b };
    if !at_least {
        e -= 1;
    }
    // Round up when x ≥ 2^(e + 1/2): a² ≥ 2^(2e + 1) × b².
    let (a2, b2) = (mul_wide(a, a), mul_wide(b, b));
    let up = if e >= 0 {
        shl_wide(b2, (2 * e + 1) as u32).is_some_and(|rhs| a2 >= rhs)
    } else {
        shl_wide(a2, (-2 * e - 1) as u32).is_none_or(|lhs| lhs >= b2)
    };
    (e + i32::from(up)).clamp(0, MAX) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(n: u64, d: u64) -> Ratio {
        Ratio::new(n, d).unwrap()
    }

    #[test]
    fn m_is_in_lowest_terms_and_held_below_2_53() {
        // 96 MHz and 1 GHz at 12 GHz: 125 and 12.
        let axis = r(12_000_000_000, 1);
        assert_eq!(
            axis_ratio(Ratio::ONE, axis, r(96_000_000, 1)),
            Some(r(125, 1))
        );
        assert_eq!(
            axis_ratio(Ratio::ONE, axis, r(1_000_000_000, 1)),
            Some(r(12, 1))
        );
        // 1,234 ppm slow on the 96 MHz source.
        assert_eq!(
            axis_ratio(r(1_001_234, 1_000_000), axis, r(96_000_000, 1)),
            Some(r(500_617, 4_000))
        );
        // Stated unreduced, the same m.
        assert_eq!(
            axis_ratio(r(2, 2), r(24_000_000_000, 2), r(192_000_000, 2)),
            Some(r(125, 1))
        );
        // Rates 2^30 and 2^-30 at 2^30: the slow one's m is 2^60.
        assert_eq!(axis_ratio(Ratio::ONE, r(1 << 30, 1), r(1, 1 << 30)), None);
    }

    #[test]
    fn the_mapping_floors_toward_minus_infinity() {
        // m = 1/2, a source at axis tick 5, a channel moved one tick earlier: its tick 0 maps to
        // 5 + floor(-1/2) = 4, where truncation would give 5.
        let map = Mapping::new(5, -1, r(1, 2));
        assert_eq!(map.axis(0), Some(4));
        assert_eq!(map.axis(1), Some(5));
        assert_eq!(map.axis(2), Some(5));
        assert_eq!(map.axis(3), Some(6));
        let map = Mapping::new(-1000, 0, r(125, 12));
        // 1000 × 12 / 125 = 96: source tick 96 is axis tick 0.
        assert_eq!(map.axis(96), Some(0));
        assert_eq!(map.axis(97), Some(10));
        assert_eq!(Mapping::IDENTITY.axis(i64::MAX), Some(i64::MAX));
        assert_eq!(Mapping::new(i64::MAX, 0, r(2, 1)).axis(1), None);
    }

    #[test]
    fn the_floor_is_the_same_where_a_product_fits_64_bits_and_where_it_does_not() {
        // Each x either side of where x × num leaves an i64, and far inside and past it.
        let reference = |x: i128, n: u64, d: u64| (x * i128::from(n)).div_euclid(i128::from(d));
        let mut checked = 0;
        for (n, d) in [
            (500_617, 4_000),
            (125, 24),
            (7, 3),
            (1, 2),
            ((1 << 52) + 1, (1 << 52) - 1),
        ] {
            let edge = i128::from(i64::MAX) / i128::from(n);
            for x in [
                0,
                1,
                -1,
                7,
                -7,
                edge,
                edge + 1,
                -edge,
                -edge - 1,
                edge - 13,
                -edge + 13,
                i128::from(i64::MAX),
                i128::from(i64::MIN),
                1 << 64,
                -(1 << 64),
            ] {
                assert_eq!(floor_scaled(x, n, d), reference(x, n, d), "{x} × {n} / {d}");
                checked += 1;
            }
        }
        assert_eq!(checked, 75);
        // A whole m takes no division: 1,000 ticks of 125 each.
        assert_eq!(floor_scaled(-1000, 125, 1), -125_000);
    }

    #[test]
    fn first_at_is_the_least_source_tick_at_or_past_an_axis_tick() {
        for (origin, offset, m) in [
            (0, 0, r(1, 1)),
            (5, -1, r(1, 2)),
            (-1000, 3, r(125, 12)),
            (17, 0, r(999, 1000)),
            (-3, -7, r(7, 3)),
        ] {
            let map = Mapping::new(origin, offset, m);
            for t in -50..200 {
                let k = map.first_at(t);
                assert!(map.axis(k).unwrap() >= t, "{map:?} {t}: {k}");
                assert!(map.axis(k - 1).unwrap() < t, "{map:?} {t}: {k}");
            }
        }
    }

    /// The whole number nearest log2(64 t), from f64, for spacings far from a rounding bound.
    fn by_float(t: Spacing) -> u8 {
        let x = 64.0 * t.num as f64 / t.den as f64;
        x.log2().round().clamp(0.0, 62.0) as u8
    }

    #[test]
    fn the_finest_shift_puts_about_64_items_in_a_bucket() {
        let s = |n, d| finest_shift(Spacing::new(n, d));
        // verge's pair at 100 kHz on 150 MHz: 1,500 ticks, log2(96,000) = 16.55.
        assert_eq!(s(1500, 1), 17);
        // verge's pair: 96,000 ticks at 96 MHz and 1 kHz: log2(6,144,000) = 22.55.
        assert_eq!(s(96_000, 1), 23);
        // A sample a tick: 2^6 exactly.
        assert_eq!(s(1, 1), 6);
        // The bound between 6 and 7 is 2^6.5 = 90.50...: 90/64 rounds down, 91/64 up.
        assert_eq!(s(90, 64), 6);
        assert_eq!(s(91, 64), 7);
        // Below a 64th of a tick a bucket of one tick holds more.
        assert_eq!(s(1, 256), 0);
        assert_eq!(finest_shift(None), 62);
        // The same data at a timebase 8 times finer: three more.
        assert_eq!(s(8 * 96_000, 1), 26);
        // Near the top of the terms the spacing can have.
        let big = (1u128 << 105) + 12345;
        assert_eq!(s(big, 1), 62);
        assert_eq!(s(1, big), 0);
        for (n, d) in [
            (10_000u128, 1u128),
            (125, 12),
            (3, 7),
            (1 << 40, 3),
            (1 << 100, 1 << 99),
        ] {
            let t = Spacing::new(n, d).unwrap();
            assert_eq!(finest_shift(Some(t)), by_float(t), "{n}/{d}");
        }
    }

    #[test]
    fn the_densest_channel_is_the_one_with_the_closest_spacing() {
        let a = Spacing::of_samples(r(960, 1), r(1, 1));
        let b = Spacing::of_edges(1_000_000, 2000).unwrap();
        let c = Spacing::new(1u128 << 104, (1u128 << 104) + 1).unwrap();
        assert_eq!(Spacing::closest([a, b]), Some(b));
        assert_eq!(Spacing::closest([a, b, c]), Some(c));
        assert_eq!(Spacing::closest([]), None);
        assert!(!a.closer_than(a));
    }
}
