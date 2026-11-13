//! JavaScript's number semantics where the web viewer's output depends on them: `String(x)`,
//! `toFixed`, `Math.round`, and `Math.max` and `Math.min`, which give NaN for a NaN operand where
//! Rust's `f64::max` and `f64::min` give the other one.

use num_bigint::{BigInt, BigUint};
use num_traits::Zero;

/// The shortest decimal digits that read back as `|x|`, and the exponent `n` such that `|x|` is
/// `0.d1d2... × 10^n`, as `Number::toString` picks them. `x` is finite and not zero.
pub(crate) fn shortest(x: f64) -> (String, i32) {
    // Rust's `{:e}` prints the shortest round-trip digits, as `Number::toString` does.
    let s = format!("{:e}", x.abs());
    let (mantissa, exp) = s.split_once('e').expect("{:e} has an exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    (digits, exp.parse::<i32>().expect("an integer exponent") + 1)
}

/// `String(x)` (ECMA-262, Number::toString).
pub(crate) fn string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let sign = if x < 0.0 { "-" } else { "" };
    let (d, n) = shortest(x);
    let k = d.len() as i32;
    let body = if k <= n && n <= 21 {
        format!("{d}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &d[..n as usize], &d[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{d}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let e = if e < 0 {
            format!("-{}", -e)
        } else {
            format!("+{e}")
        };
        match d.len() {
            1 => format!("{d}e{e}"),
            _ => format!("{}.{}e{e}", &d[..1], &d[1..]),
        }
    };
    format!("{sign}{body}")
}

/// `x.toFixed(digits)` (ECMA-262, Number.prototype.toFixed): `x` rounded at `digits` decimals
/// from its exact binary value, a tie going to the larger magnitude.
pub(crate) fn to_fixed(x: f64, digits: u32) -> String {
    if !x.is_finite() || x.abs() >= 1e21 {
        return string(x);
    }
    let negative = x < 0.0;
    // |x| = m × 2^e exactly.
    let bits = x.abs().to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (m, e) = if exponent == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1 << 52), exponent - 1075)
    };
    let scaled = BigUint::from(m) * BigUint::from(10u8).pow(digits);
    let n = if e >= 0 {
        scaled << e as usize
    } else {
        let den = BigUint::from(1u8) << (-e) as usize;
        (scaled * 2u8 + &den) / (den * 2u8)
    };
    let mut s = n.to_string();
    if digits > 0 {
        let d = digits as usize;
        if s.len() <= d {
            s = format!("{}{s}", "0".repeat(d + 1 - s.len()));
        }
        s.insert(s.len() - d, '.');
    }
    if negative { format!("-{s}") } else { s }
}

/// `Math.round(x)`: the nearest integer, a tie going towards +∞.
pub(crate) fn round(x: f64) -> f64 {
    if !x.is_finite() || x.fract() == 0.0 {
        return x;
    }
    let floor = x.floor();
    let r = if x - floor >= 0.5 { floor + 1.0 } else { floor };
    if r == 0.0 && x < 0.0 { -0.0 } else { r }
}

/// `Math.max(a, b)`.
pub(crate) fn max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        // +0 is the larger of the two zeros.
        if a.is_sign_positive() { a } else { b }
    } else if a > b {
        a
    } else {
        b
    }
}

/// `Math.min(a, b)`.
pub(crate) fn min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == b {
        if a.is_sign_negative() { a } else { b }
    } else if a < b {
        a
    } else {
        b
    }
}

/// White space as `String.prototype.trim` and a RegExp's `\s` read it: WhiteSpace and
/// LineTerminator.
pub(crate) fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `Number(n)` of a BigInt: the nearest f64, a tie going to the even one.
pub(crate) fn bigint_to_f64(n: &BigInt) -> f64 {
    if n.is_zero() {
        return 0.0;
    }
    // Parsing the decimal rounds correctly, which a conversion that truncates its low bits
    // need not.
    n.to_string().parse().expect("an integer parses as a float")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_strings_as_javascript_prints_them() {
        // Each from `String(x)` in Node.
        let cases = [
            (0.1, "0.1"),
            (3305.0 * 0.001, "3.305"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123456789012345680000.0, "123456789012345680000"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (0.000001, "0.000001"),
            (-0.0, "0"),
            (-2.5, "-2.5"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (100.0, "100"),
        ];
        for (x, want) in cases {
            assert_eq!(string(x), want, "{x:e}");
        }
    }

    #[test]
    fn to_fixed_rounds_the_exact_binary_value_ties_up() {
        // Each from `x.toFixed(d)` in Node. 0.25 and 2.5 are ties; 1.005 sits below its tie.
        let cases = [
            (0.25, 1, "0.3"),
            (2.5, 0, "3"),
            (1.005, 2, "1.00"),
            (50.78, 2, "50.78"),
            (0.0, 2, "0.00"),
            (-0.0001, 2, "-0.00"),
            (123.456, 0, "123"),
            (1e-10, 12, "0.000000000100"),
        ];
        for (x, d, want) in cases {
            assert_eq!(to_fixed(x, d), want, "{x}.toFixed({d})");
        }
    }

    #[test]
    fn math_round_and_max_and_min() {
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(-2.5), -2.0);
        assert_eq!(round(0.49999999999999994), 0.0);
        assert!(round(-0.4).is_sign_negative());
        assert!(max(1.0, f64::NAN).is_nan());
        assert!(min(f64::NAN, 1.0).is_nan());
        assert!(max(-0.0, 0.0).is_sign_positive());
        assert!(min(0.0, -0.0).is_sign_negative());
    }
}
