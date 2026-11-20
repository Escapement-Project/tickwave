//! An exact axis time, for ordering observations from sources with different mappings.

use std::cmp::Ordering;

/// An exact axis time, `num / den` ticks. A digital edge's denominator is its mapping's, below
/// 2^53; an analog sample's is that times its grid's, below 2^69. Times of one source compare
/// their numerators; others compare cross products, in 256 bits where they pass 128.
#[derive(Clone, Copy, Debug)]
pub struct Exact {
    num: i128,
    /// Positive, and no more than `i128::MAX`.
    den: u128,
}

impl Exact {
    /// `num / den`; None for a denominator of 0 or past `i128::MAX`.
    pub fn new(num: i128, den: u128) -> Option<Self> {
        (den != 0 && i128::try_from(den).is_ok()).then_some(Self { num, den })
    }

    pub fn whole(t: i64) -> Self {
        Self {
            num: i128::from(t),
            den: 1,
        }
    }

    pub fn num(self) -> i128 {
        self.num
    }

    pub fn den(self) -> u128 {
        self.den
    }

    /// The axis tick it lies in, saturating at the ends of i64.
    pub fn floor(self) -> i64 {
        // `den` fits an i128 and is positive.
        let t = self.num.div_euclid(self.den as i128);
        t.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
    }
}

/// `a × b` as 256 bits, high half first.
fn mul_wide(a: u128, b: u128) -> (u128, u128) {
    let (a1, a0) = (a >> 64, a & u128::from(u64::MAX));
    let (b1, b0) = (b >> 64, b & u128::from(u64::MAX));
    let low = a0 * b0;
    let (mid, carry) = (a0 * b1).overflowing_add(a1 * b0);
    let (lo, c1) = low.overflowing_add(mid << 64);
    let hi = a1 * b1 + (mid >> 64) + (u128::from(carry) << 64) + u128::from(c1);
    (hi, lo)
}

impl Ord for Exact {
    fn cmp(&self, o: &Self) -> Ordering {
        if self.den == o.den {
            return self.num.cmp(&o.num);
        }
        // Both denominators fit an i128, so only the products can overflow.
        let (da, db) = (self.den as i128, o.den as i128);
        if let (Some(a), Some(b)) = (self.num.checked_mul(db), o.num.checked_mul(da)) {
            return a.cmp(&b);
        }
        let (sa, sb) = (self.num.signum(), o.num.signum());
        if sa != sb {
            return sa.cmp(&sb);
        }
        let a = mul_wide(self.num.unsigned_abs(), o.den);
        let b = mul_wide(o.num.unsigned_abs(), self.den);
        if sa < 0 { b.cmp(&a) } else { a.cmp(&b) }
    }
}

impl PartialOrd for Exact {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl PartialEq for Exact {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}

impl Eq for Exact {}
