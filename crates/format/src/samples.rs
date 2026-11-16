//! Analog samples as blocks store them, and derived channels' products: the rules every writer
//! and reader here share, so a value, a code's bytes and a product come out the same everywhere.

use crate::header::{AnalogCoding, IntCoding, Op, Stored, code_value};
use crate::model::Sample;

/// A block's samples of one analog channel, in the type it stores: f32 values for an `f32` or a
/// `codes` channel, f64 values, or an `int` channel's codes.
#[derive(Clone, Debug, PartialEq)]
pub enum Values {
    F32(Vec<f32>),
    F64(Vec<f64>),
    U64(Vec<u64>),
    I64(Vec<i64>),
}

impl Values {
    pub fn len(&self) -> usize {
        match self {
            Self::F32(v) => v.len(),
            Self::F64(v) => v.len(),
            Self::U64(v) => v.len(),
            Self::I64(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, i: usize) -> Option<Sample> {
        Some(match self {
            Self::F32(v) => Sample::F32(*v.get(i)?),
            Self::F64(v) => Sample::F64(*v.get(i)?),
            Self::U64(v) => Sample::U64(*v.get(i)?),
            Self::I64(v) => Sample::I64(*v.get(i)?),
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = Sample> + '_ {
        (0..self.len()).map(|i| self.get(i).expect("inside"))
    }

    /// The f32 values of an `f32` or `codes` channel; None for any other.
    pub fn as_f32(&self) -> Option<&[f32]> {
        match self {
            Self::F32(v) => Some(v),
            _ => None,
        }
    }
}

/// A sample's value in f64 under `coding`: the f32 or f64 itself, or an `int` code through
/// `IntCoding::value`.
#[inline]
pub fn value_f64(sample: Sample, coding: AnalogCoding) -> f64 {
    match (sample, coding) {
        (Sample::F32(v), _) => f64::from(v),
        (Sample::F64(v), _) => v,
        (Sample::U64(c), AnalogCoding::Int(int)) => int.value(i128::from(c)),
        (Sample::I64(c), AnalogCoding::Int(int)) => int.value(i128::from(c)),
        (Sample::U64(c), _) => c as f64,
        (Sample::I64(c), _) => c as f64,
    }
}

/// Why a sample doesn't fit its channel's coding.
#[inline]
pub fn misfit(sample: Sample, coding: AnalogCoding) -> Option<String> {
    match (sample, coding) {
        (Sample::F32(_), AnalogCoding::F32 | AnalogCoding::Codes { .. })
        | (Sample::F64(_), AnalogCoding::F64) => None,
        (Sample::U64(c), AnalogCoding::Int(int)) if !int.signed => {
            (!int.holds(i128::from(c))).then(|| format!("code {c} is wider than {} bits", int.bits))
        }
        (Sample::I64(c), AnalogCoding::Int(int)) if int.signed => (!int.holds(i128::from(c)))
            .then(|| format!("code {c} lies outside {} signed bits", int.bits)),
        (s, coding) => Some(format!(
            "a{} {} sample for a channel stored as {}",
            if matches!(s, Sample::I64(_)) { "n" } else { "" },
            s.type_name(),
            coding_name(coding)
        )),
    }
}

/// A coding's name, for a message.
pub fn coding_name(coding: AnalogCoding) -> String {
    match coding {
        AnalogCoding::F32 => "f32".into(),
        AnalogCoding::F64 => "f64".into(),
        AnalogCoding::Int(int) => format!(
            "{} {}-bit int",
            if int.signed { "signed" } else { "unsigned" },
            int.bits
        ),
        AnalogCoding::Codes { bits, .. } => format!("{bits}-bit codes"),
    }
}

/// Appends a sample's bytes to a section being built: an f32 or f64 whole, a code as its
/// `ceil(bits / 8)` low bytes, little-endian, in two's complement. `to_planes` then splits
/// the codes into byte planes. `code` is the `codes` channel's code for an f32 sample.
#[inline]
pub fn put(section: &mut Vec<u8>, sample: Sample, coding: AnalogCoding, code: Option<u16>) {
    match (sample, coding) {
        (Sample::F32(_), AnalogCoding::Codes { .. }) => {
            let code = code.expect("a codes channel's sample comes with its code");
            section.extend_from_slice(&code.to_le_bytes()[..coding.sample_len()]);
        }
        (Sample::F32(v), _) => section.extend_from_slice(&v.to_le_bytes()),
        (Sample::F64(v), _) => section.extend_from_slice(&v.to_le_bytes()),
        (Sample::U64(c), _) => section.extend_from_slice(&c.to_le_bytes()[..coding.sample_len()]),
        (Sample::I64(c), _) => section.extend_from_slice(&c.to_le_bytes()[..coding.sample_len()]),
    }
}

/// A section of codes, each `width` bytes together, as byte planes: every code's lowest byte,
/// then every code's next, and so on.
pub fn to_planes(section: &[u8], width: usize, out: &mut Vec<u8>) {
    for plane in 0..width {
        out.extend(section.iter().skip(plane).step_by(width));
    }
}

/// An empty list with room for `n` items, or an error starting with `decode::CANT_HOLD` where
/// the reader can't allocate it.
fn room<T>(n: usize) -> Result<Vec<T>, String> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| format!("{} {n} samples", crate::decode::CANT_HOLD))?;
    Ok(v)
}

/// The codes in `count` codes' byte planes of `width` bytes each, as u64s, each code's bytes in
/// place and the bytes above them zero.
fn from_planes(section: &[u8], count: usize, width: usize) -> Result<Vec<u64>, String> {
    let mut codes = room(count)?;
    codes.resize(count, 0u64);
    for (plane, bytes) in section.chunks_exact(count.max(1)).take(width).enumerate() {
        for (code, &b) in codes.iter_mut().zip(bytes) {
            *code |= u64::from(b) << (8 * plane);
        }
    }
    Ok(codes)
}

/// An analog section's `count` samples in `coding`. A code outside its coding's range is
/// refused. Where the reader can't allocate the samples, the error starts with
/// `decode::CANT_HOLD`.
pub fn decode(section: &[u8], count: usize, coding: AnalogCoding) -> Result<Values, String> {
    if count.checked_mul(coding.sample_len()) != Some(section.len()) {
        return Err("the section does not hold its samples".into());
    }
    Ok(match coding {
        AnalogCoding::F32 => {
            let mut v = room(count)?;
            v.extend(
                section
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&b| f32::from_le_bytes(b)),
            );
            Values::F32(v)
        }
        AnalogCoding::F64 => {
            let mut v = room(count)?;
            v.extend(
                section
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|&b| f64::from_le_bytes(b)),
            );
            Values::F64(v)
        }
        AnalogCoding::Codes {
            bits,
            scale,
            offset,
        } => {
            let codes = from_planes(section, count, coding.sample_len())?;
            // One OR over every code, apart from the conversion, so both loops stay vectorised.
            let all = codes.iter().fold(0, |a, &c| a | c);
            if all >> bits != 0 {
                let wide = codes.iter().find(|&&c| c >> bits != 0).unwrap_or(&0);
                return Err(format!("code {wide} is wider than {bits} bits"));
            }
            Values::F32(
                codes
                    .iter()
                    .map(|&c| code_value(c as u16, scale, offset))
                    .collect(),
            )
        }
        AnalogCoding::Int(int) => {
            let width = int.planes();
            let codes = from_planes(section, count, width)?;
            if int.signed {
                // Sign-extended from the top stored bit, then held to the coding's bits.
                let spare = 64 - 8 * width as u32;
                let codes: Vec<i64> = codes
                    .into_iter()
                    .map(|c| ((c << spare) as i64) >> spare)
                    .collect();
                if let Some(c) = codes.iter().find(|&&c| !int.holds(i128::from(c))) {
                    return Err(format!("code {c} lies outside {} signed bits", int.bits));
                }
                Values::I64(codes)
            } else {
                if let Some(c) = codes.iter().find(|&&c| !int.holds(i128::from(c))) {
                    return Err(format!("code {c} is wider than {} bits", int.bits));
                }
                Values::U64(codes)
            }
        }
    })
}

/// How a derived channel's samples follow from its operands' (FORMAT.md, "Derived channels"):
/// a product or a difference, as integer codes where the rule allows and as f64 otherwise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Product {
    /// The integer product of two `int` channels' codes, neither with an offset, their bits
    /// summing to at most 64, and their scales' numerators' product and denominators' product
    /// each of a magnitude below 2^53: signed if either is. Its scale is the numerators' product
    /// over the denominators', each exact in f64. Its codes are `bits` wide, the operands'
    /// summed.
    Int {
        signed: bool,
        bits: u8,
        num: f64,
        den: f64,
    },
    /// `f64(a) × f64(b)`, rounded once.
    F64 { a: AnalogCoding, b: AnalogCoding },
    /// The difference of two `int` channels as a signed code, `ka × a − kb × b`, `bits` wide,
    /// whose value is `code × num / den + onum / oden`: `num / den` the greatest common divisor
    /// of the operands' scales, and `onum / oden` their offsets' difference, each term of a
    /// magnitude below 2^53.
    Difference {
        ka: i128,
        kb: i128,
        bits: u8,
        num: f64,
        den: f64,
        onum: f64,
        oden: f64,
    },
    /// `f64(a) − f64(b)`, rounded once.
    F64Difference { a: AnalogCoding, b: AnalogCoding },
}

fn gcd128(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The least and greatest code an `int` coding allows.
fn code_range(c: IntCoding) -> (i128, i128) {
    let bits = u32::from(c.bits);
    if c.signed {
        let half = 1i128 << (bits - 1);
        (-half, half - 1)
    } else {
        (0, (1i128 << bits) - 1)
    }
}

/// The least and greatest of `k × c` for `c` in `lo..=hi`; None where it passes 128 bits.
fn scaled_range(k: i128, (lo, hi): (i128, i128)) -> Option<(i128, i128)> {
    let (a, b) = (k.checked_mul(lo)?, k.checked_mul(hi)?);
    Some((a.min(b), a.max(b)))
}

/// The fewest bits of a signed code that hold every one of `lo..=hi`.
fn signed_bits(lo: i128, hi: i128) -> u32 {
    (1..=128)
        .find(|&b| {
            let half = 1i128.checked_shl(b - 1).unwrap_or(i128::MAX);
            b == 128 || (lo >= -half && hi < half)
        })
        .unwrap_or(128)
}

impl Product {
    /// The rule for operation `op` on operands coded `a` and `b`; None for an operation this
    /// version doesn't define.
    pub fn rule(op: &Op, a: AnalogCoding, b: AnalogCoding) -> Option<Self> {
        match op {
            Op::Multiply => Some(Self::of(a, b)),
            Op::Subtract => Some(Self::difference(a, b)),
            Op::Other(_) => None,
        }
    }

    /// The difference rule for operands coded `a` and `b` (FORMAT.md, "Derived channels").
    pub fn difference(a: AnalogCoding, b: AnalogCoding) -> Self {
        let f64_rule = Self::F64Difference { a, b };
        let (AnalogCoding::Int(x), AnalogCoding::Int(y)) = (a, b) else {
            return f64_rule;
        };
        let (sx, sy) = (x.scale(), y.scale());
        let (p, q) = (i128::from(sx.num()), i128::from(sx.den()));
        let (u, v) = (i128::from(sy.num()), i128::from(sy.den()));
        // s = gcd(|p| v, |u| q) / (q v): every term is below 2^53, so each product is below 2^106.
        let g = gcd128(p.unsigned_abs() * v as u128, u.unsigned_abs() * q as u128);
        let qv = (q * v) as u128;
        let g2 = gcd128(g, qv);
        let (snum, sden) = (g / g2, qv / g2);
        // scale_a / s and scale_b / s, whole numbers.
        let ka = p * v / g as i128;
        let kb = u * q / g as i128;
        let range = scaled_range(ka, code_range(x)).and_then(|(alo, ahi)| {
            let (blo, bhi) = scaled_range(kb, code_range(y))?;
            Some((alo.checked_sub(bhi)?, ahi.checked_sub(blo)?))
        });
        let Some((lo, hi)) = range else {
            return f64_rule;
        };
        let bits = signed_bits(lo, hi);
        let (oa, ob) = (x.offset(), y.offset());
        let (oan, oad) = (i128::from(oa.num()), i128::from(oa.den()));
        let (obn, obd) = (i128::from(ob.num()), i128::from(ob.den()));
        let on = oan * obd - obn * oad;
        let od = oad * obd;
        let og = gcd128(on.unsigned_abs(), od as u128).max(1) as i128;
        let (onum, oden) = (on / og, od / og);
        let safe = |t: i128| t.unsigned_abs() < 1 << 53;
        if bits > 64 || !safe(snum as i128) || !safe(sden as i128) || !safe(onum) || !safe(oden) {
            return f64_rule;
        }
        Self::Difference {
            ka,
            kb,
            bits: bits as u8,
            num: snum as f64,
            den: sden as f64,
            onum: onum as f64,
            oden: oden as f64,
        }
    }

    /// The product rule for operands coded `a` and `b`.
    pub fn of(a: AnalogCoding, b: AnalogCoding) -> Self {
        match (a, b) {
            (AnalogCoding::Int(x), AnalogCoding::Int(y))
                if x.offset.is_none() && y.offset.is_none() && x.bits + y.bits <= 64 =>
            {
                let (sx, sy) = (x.scale(), y.scale());
                let num = i128::from(sx.num()) * i128::from(sy.num());
                let den = i128::from(sx.den()) * i128::from(sy.den());
                let exact = |v: i128| v.unsigned_abs() < 1 << 53;
                if !exact(num) || !exact(den) {
                    return Self::F64 { a, b };
                }
                Self::Int {
                    signed: x.signed || y.signed,
                    bits: x.bits + y.bits,
                    num: num as f64,
                    den: den as f64,
                }
            }
            _ => Self::F64 { a, b },
        }
    }

    pub fn stored(&self) -> Stored {
        match self {
            Self::Int { signed: false, .. } => Stored::U64,
            Self::Int { signed: true, .. } | Self::Difference { .. } => Stored::I64,
            Self::F64 { .. } | Self::F64Difference { .. } => Stored::F64,
        }
    }

    /// An integer result's code width in bits; None for an f64 result.
    pub fn bits(&self) -> Option<u8> {
        match self {
            Self::Int { bits, .. } | Self::Difference { bits, .. } => Some(*bits),
            Self::F64 { .. } | Self::F64Difference { .. } => None,
        }
    }

    /// Sample i of the derived channel from its operands' sample i.
    pub fn sample(&self, a: Sample, b: Sample) -> Sample {
        match (self, a, b) {
            (Self::Int { signed: false, .. }, Sample::U64(x), Sample::U64(y)) => Sample::U64(x * y),
            (Self::Int { signed: true, .. }, x, y) => {
                let int = |s| match s {
                    Sample::U64(c) => i128::from(c),
                    Sample::I64(c) => i128::from(c),
                    _ => unreachable!("an integer product's operands are int channels"),
                };
                // The bits sum to at most 64, so the product fits an i64.
                Sample::I64((int(x) * int(y)) as i64)
            }
            (Self::F64 { a: ca, b: cb }, x, y) => {
                Sample::F64(value_f64(x, *ca) * value_f64(y, *cb))
            }
            (Self::Difference { ka, kb, .. }, x, y) => {
                // `bits` is at most 64, so the code fits an i64.
                Sample::I64((ka * int_of(x) - kb * int_of(y)) as i64)
            }
            (Self::F64Difference { a: ca, b: cb }, x, y) => {
                Sample::F64(value_f64(x, *ca) - value_f64(y, *cb))
            }
            _ => unreachable!("an integer product's operands are int channels"),
        }
    }

    /// A derived sample's value in f64: an integer result's code by the rule for an `int`
    /// channel, `fl(fl(code × num) / den)` and its offset added; an f64 result as it is.
    #[inline]
    pub fn value(&self, sample: Sample) -> f64 {
        match (self, sample) {
            (Self::Int { .. } | Self::Difference { .. }, Sample::U64(c)) => self.scaled(c as f64),
            (Self::Int { .. } | Self::Difference { .. }, Sample::I64(c)) => self.scaled(c as f64),
            (_, s) => value_f64(s, AnalogCoding::F64),
        }
    }

    /// `x` through an integer result's scale and offset, as `value` takes a code: times the
    /// numerator, then over the denominator, then the offset added, each in f64. An f64 result's
    /// `x` as it is.
    #[inline]
    pub fn scaled(&self, x: f64) -> f64 {
        match self {
            Self::Int { num, den, .. } => (x * num) / den,
            Self::Difference {
                num,
                den,
                onum,
                oden,
                ..
            } => (x * num) / den + onum / oden,
            Self::F64 { .. } | Self::F64Difference { .. } => x,
        }
    }
}

/// An `int` sample's code.
fn int_of(s: Sample) -> i128 {
    match s {
        Sample::U64(c) => i128::from(c),
        Sample::I64(c) => i128::from(c),
        _ => unreachable!("an integer result's operands are int channels"),
    }
}

/// A derived channel's samples in a block, from its operands' values there, which share their
/// first index and their count.
pub fn product_values(product: Product, a: &Values, b: &Values) -> Result<Values, String> {
    if a.len() != b.len() {
        return Err(format!(
            "its operands hold {} and {} samples in the block",
            a.len(),
            b.len()
        ));
    }
    // Each loop below is `Product::sample` over one pair of stored types, chosen once a block.
    // The bits sum to at most 64, so a signed product fits an i64.
    fn signed<A: Copy + Into<i128>, B: Copy + Into<i128>>(a: &[A], b: &[B]) -> Vec<i64> {
        a.iter()
            .zip(b)
            .map(|(&x, &y)| (x.into() * y.into()) as i64)
            .collect()
    }
    Ok(match (product, a, b) {
        (Product::Int { signed: false, .. }, Values::U64(x), Values::U64(y)) => {
            Values::U64(x.iter().zip(y).map(|(&x, &y)| x * y).collect())
        }
        (Product::Int { signed: true, .. }, a, b) => Values::I64(match (a, b) {
            (Values::U64(x), Values::U64(y)) => signed(x, y),
            (Values::U64(x), Values::I64(y)) => signed(x, y),
            (Values::I64(x), Values::U64(y)) => signed(x, y),
            (Values::I64(x), Values::I64(y)) => signed(x, y),
            _ => return Err(NOT_INT.into()),
        }),
        (Product::F64 { a: ca, b: cb }, a, b) => {
            let mut out = Vec::with_capacity(a.len());
            each_f64(a, ca, |x| out.push(x));
            let mut slots = out.iter_mut();
            each_f64(b, cb, |y| *slots.next().expect("as many as a's") *= y);
            Values::F64(out)
        }
        (Product::F64Difference { a: ca, b: cb }, a, b) => {
            let mut out = Vec::with_capacity(a.len());
            each_f64(a, ca, |x| out.push(x));
            let mut slots = out.iter_mut();
            each_f64(b, cb, |y| *slots.next().expect("as many as a's") -= y);
            Values::F64(out)
        }
        (Product::Difference { ka, kb, .. }, a, b) => {
            fn diff<A: Copy + Into<i128>, B: Copy + Into<i128>>(
                a: &[A],
                b: &[B],
                ka: i128,
                kb: i128,
            ) -> Vec<i64> {
                a.iter()
                    .zip(b)
                    .map(|(&x, &y)| (ka * x.into() - kb * y.into()) as i64)
                    .collect()
            }
            Values::I64(match (a, b) {
                (Values::U64(x), Values::U64(y)) => diff(x, y, ka, kb),
                (Values::U64(x), Values::I64(y)) => diff(x, y, ka, kb),
                (Values::I64(x), Values::U64(y)) => diff(x, y, ka, kb),
                (Values::I64(x), Values::I64(y)) => diff(x, y, ka, kb),
                _ => return Err(NOT_INT.into()),
            })
        }
        _ => return Err(NOT_INT.into()),
    })
}

const NOT_INT: &str = "an integer product's operands don't hold integer codes";

/// Calls `f` with each of `values`' samples as `value_f64` gives it under `coding`, the match on
/// the samples' type made once rather than for each sample.
fn each_f64(values: &Values, coding: AnalogCoding, mut f: impl FnMut(f64)) {
    match values {
        Values::F32(v) => v.iter().for_each(|&x| f(f64::from(x))),
        Values::F64(v) => v.iter().for_each(|&x| f(x)),
        Values::U64(v) => v.iter().for_each(|&c| f(value_f64(Sample::U64(c), coding))),
        Values::I64(v) => v.iter().for_each(|&c| f(value_f64(Sample::I64(c), coding))),
    }
}

#[cfg(all(test, feature = "encode"))]
mod tests {
    use super::*;
    use crate::header::{Exact, IntCoding};

    fn int(bits: u8, signed: bool) -> AnalogCoding {
        AnalogCoding::Int(IntCoding::new(bits, signed))
    }

    /// The bytes the writer stores for `samples`: each one's bytes, codes then split into
    /// planes.
    fn stored(samples: &[Sample], coding: AnalogCoding) -> Vec<u8> {
        let mut section = Vec::new();
        for &s in samples {
            assert_eq!(misfit(s, coding), None, "{s:?}");
            put(&mut section, s, coding, None);
        }
        crate::writer::analog_raw(&section, coding)
    }

    #[test]
    fn codes_are_stored_in_byte_planes_lowest_first() {
        // Worked by hand. 12 bits: two planes, every code's low byte, then every high byte.
        let codes = [0x0abc, 0x0123, 0x0fff].map(Sample::U64);
        let bytes = [0xbc, 0x23, 0xff, 0x0a, 0x01, 0x0f];
        assert_eq!(stored(&codes, int(12, false)), bytes);
        assert_eq!(
            decode(&bytes, 3, int(12, false)).unwrap(),
            Values::U64(vec![0x0abc, 0x0123, 0x0fff])
        );
        // 29 bits: four planes.
        let codes = [0x1abc_def0, 1, 0x1fff_ffff].map(Sample::U64);
        let bytes = [
            0xf0, 0x01, 0xff, 0xde, 0x00, 0xff, 0xbc, 0x00, 0xff, 0x1a, 0x00, 0x1f,
        ];
        assert_eq!(stored(&codes, int(29, false)), bytes);
        assert_eq!(
            decode(&bytes, 3, int(29, false)).unwrap(),
            Values::U64(vec![0x1abc_def0, 1, 0x1fff_ffff])
        );
        // A signed 12-bit code in two's complement over its two bytes: -1 is ffff, -2048 f800.
        let codes = [-1, -2048, 2047].map(Sample::I64);
        let bytes = [0xff, 0x00, 0xff, 0xff, 0xf8, 0x07];
        assert_eq!(stored(&codes, int(12, true)), bytes);
        assert_eq!(
            decode(&bytes, 3, int(12, true)).unwrap(),
            Values::I64(vec![-1, -2048, 2047])
        );
        // A codes coding of 8 bits or fewer takes one plane.
        let coding = AnalogCoding::Codes {
            bits: 8,
            scale: 0.5,
            offset: 1.0,
        };
        let mut section = Vec::new();
        for code in [3u16, 200] {
            put(
                &mut section,
                Sample::F32(code_value(code, 0.5, 1.0)),
                coding,
                Some(code),
            );
        }
        assert_eq!(section, [3, 200]);
        assert_eq!(
            decode(&section, 2, coding).unwrap(),
            Values::F32(vec![2.5, 101.0])
        );
    }

    #[test]
    fn a_code_outside_its_coding_is_refused() {
        for (bytes, coding, why) in [
            (
                vec![0x00, 0x10],
                int(12, false),
                "code 4096 is wider than 12 bits",
            ),
            (
                vec![0x00, 0x08],
                int(12, true),
                "code 2048 lies outside 12 signed bits",
            ),
            (
                vec![0xff, 0xf7],
                int(12, true),
                "code -2049 lies outside 12 signed bits",
            ),
            (vec![0x02], int(1, false), "code 2 is wider than 1 bits"),
            (
                vec![0x01],
                int(1, true),
                "code 1 lies outside 1 signed bits",
            ),
        ] {
            assert_eq!(decode(&bytes, 1, coding), Err(why.to_owned()), "{bytes:?}");
        }
        assert_eq!(
            misfit(Sample::U64(1 << 12), int(12, false)).as_deref(),
            Some("code 4096 is wider than 12 bits")
        );
        assert_eq!(
            misfit(Sample::U64(1), int(12, true)).as_deref(),
            Some("a u64 sample for a channel stored as signed 12-bit int")
        );
        assert_eq!(
            misfit(Sample::F32(1.0), AnalogCoding::F64).as_deref(),
            Some("a f32 sample for a channel stored as f64")
        );
    }

    #[test]
    fn the_extremes_round_trip() {
        for (samples, coding) in [
            (vec![Sample::U64(u64::MAX), Sample::U64(0)], int(64, false)),
            (
                vec![Sample::I64(i64::MIN), Sample::I64(i64::MAX)],
                int(64, true),
            ),
            (vec![Sample::U64(0), Sample::U64(1)], int(1, false)),
            (vec![Sample::I64(-1), Sample::I64(0)], int(1, true)),
            (
                vec![
                    Sample::F64(f64::NAN),
                    Sample::F64(f64::INFINITY),
                    Sample::F64(-0.0),
                ],
                AnalogCoding::F64,
            ),
        ] {
            let bytes = stored(&samples, coding);
            let got: Vec<Sample> = decode(&bytes, samples.len(), coding)
                .unwrap()
                .iter()
                .collect();
            let bits = |s: &Sample| match *s {
                Sample::F64(v) => v.to_bits(),
                Sample::U64(c) => c,
                Sample::I64(c) => c as u64,
                Sample::F32(v) => u64::from(v.to_bits()),
            };
            assert_eq!(
                got.iter().map(bits).collect::<Vec<_>>(),
                samples.iter().map(bits).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn products_follow_the_format_rule() {
        let v = AnalogCoding::Int(IntCoding {
            scale: Exact::new(1, 1000),
            ..IntCoding::new(32, false)
        });
        let i = AnalogCoding::Int(IntCoding {
            scale: Exact::new(1, 1_000_000_000),
            ..IntCoding::new(32, false)
        });
        // 32 + 32 bits, no offset: the integer product, scale 1/10^12.
        let p = Product::of(i, v);
        assert_eq!(
            p,
            Product::Int {
                signed: false,
                bits: 64,
                num: 1.0,
                den: 1e12
            }
        );
        // Scales 3/7 and -5/2 make -15/14.
        let a = AnalogCoding::Int(IntCoding {
            scale: Exact::new(3, 7),
            ..IntCoding::new(16, false)
        });
        let b = AnalogCoding::Int(IntCoding {
            scale: Exact::new(-5, 2),
            ..IntCoding::new(16, true)
        });
        assert_eq!(
            Product::of(a, b),
            Product::Int {
                signed: true,
                bits: 32,
                num: -15.0,
                den: 14.0
            }
        );
        let max = u64::from(u32::MAX);
        assert_eq!(
            p.sample(Sample::U64(max), Sample::U64(max)),
            Sample::U64(max * max)
        );
        // Signed if either is: -3 x 5.
        let s = Product::of(int(8, true), int(8, false));
        assert_eq!(s.sample(Sample::I64(-3), Sample::U64(5)), Sample::I64(-15));
        // Two scales of 1/10^9: their denominators multiply to 10^18, past 2^53, so f64 even at
        // 32 + 32 bits. 10^9 at either is then 1 as a value, and their product 1.
        let nano = AnalogCoding::Int(IntCoding {
            scale: Exact::new(1, 1_000_000_000),
            ..IntCoding::new(32, false)
        });
        let n = Product::of(nano, nano);
        assert!(matches!(n, Product::F64 { .. }), "{n:?}");
        assert_eq!(
            n.sample(Sample::U64(1_000_000_000), Sample::U64(1_000_000_000)),
            Sample::F64(1.0)
        );
        // Numerators past 2^53 in magnitude, the same: 2^30 x -2^30 = -2^60.
        let big = |num| {
            AnalogCoding::Int(IntCoding {
                scale: Exact::new(num, 1),
                ..IntCoding::new(16, true)
            })
        };
        assert!(matches!(
            Product::of(big(1 << 30), big(-(1 << 30))),
            Product::F64 { .. }
        ));
        // At 2^26 x 2^26 = 2^52 it is still exact: an integer product.
        assert!(matches!(
            Product::of(big(1 << 26), big(1 << 26)),
            Product::Int { .. }
        ));
        // 33 + 32 bits: past 64, so f64, rounded once: 2^32 x 3.
        let wide = Product::of(int(33, false), int(32, false));
        assert!(matches!(wide, Product::F64 { .. }));
        assert_eq!(
            wide.sample(Sample::U64(1 << 32), Sample::U64(3)),
            Sample::F64(12_884_901_888.0)
        );
        // An offset turns it to f64: (1/2 + 3 x 1/4) x 2 = 2.5.
        let offset = AnalogCoding::Int(IntCoding {
            bits: 8,
            signed: false,
            scale: Exact::new(1, 4),
            offset: Exact::new(1, 2),
        });
        let f = Product::of(offset, int(8, false));
        assert_eq!(f.sample(Sample::U64(3), Sample::U64(2)), Sample::F64(2.5));
        // An f32 times an f64: one rounding of the exact product. 0.1f32 is
        // 0.100000001490116119384765625; times 3 in f64 it rounds once.
        let g = Product::of(AnalogCoding::F32, AnalogCoding::F64);
        assert_eq!(
            g.sample(Sample::F32(0.1), Sample::F64(3.0)),
            Sample::F64(f64::from(0.1f32) * 3.0)
        );
        assert_ne!(f64::from(0.1f32) * 3.0, f64::from(0.1f32 * 3.0));
    }

    #[test]
    fn a_blocks_products_are_each_samples_product() {
        let unsigned = Values::U64(vec![0, 1, 255, 7, u64::from(u32::MAX)]);
        let signed = Values::I64(vec![-128, -1, 0, 127, 5]);
        let offset = AnalogCoding::Int(IntCoding {
            bits: 8,
            signed: false,
            scale: Exact::new(1, 4),
            offset: Exact::new(1, 2),
        });
        let cases = [
            (int(32, false), int(32, false), &unsigned, &unsigned),
            (int(8, true), int(8, false), &signed, &unsigned),
            (int(8, false), int(8, true), &unsigned, &signed),
            (int(8, true), int(8, true), &signed, &signed),
            (int(33, false), int(32, false), &unsigned, &unsigned),
            (offset, int(8, true), &unsigned, &signed),
            (int(8, true), offset, &signed, &unsigned),
            (
                AnalogCoding::F32,
                AnalogCoding::F64,
                &Values::F32(vec![0.1, -2.5, 3.0, 0.0, 1e30]),
                &Values::F64(vec![3.0, 0.1, -1e300, 7.0, 1e30]),
            ),
        ];
        for (ca, cb, a, b) in cases {
            let p = Product::of(ca, cb);
            let each: Vec<Sample> = a
                .iter()
                .zip(b.iter())
                .map(|(x, y)| p.sample(x, y))
                .collect();
            let block = product_values(p, a, b).unwrap();
            assert_eq!(block.iter().collect::<Vec<_>>(), each, "{p:?}");
        }
    }

    #[test]
    fn an_integer_product_of_anything_but_codes_is_refused() {
        let empty = Values::F64(Vec::new());
        for p in [
            Product::of(int(32, false), int(32, false)),
            Product::of(int(8, true), int(8, false)),
        ] {
            assert_eq!(
                product_values(p, &empty, &empty),
                Err(NOT_INT.to_owned()),
                "{p:?}"
            );
            let one = Values::F32(vec![1.0]);
            assert_eq!(
                product_values(p, &one, &Values::U64(vec![1])),
                Err(NOT_INT.to_owned())
            );
        }
    }
}
