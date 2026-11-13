//! `format_code`, an `int` channel's value from its code with no f64 on the way. It has no web
//! counterpart, so its cases are here. Expected values are worked by hand.

use escapement_tickwave_format::model::Exact;
use escapement_tickwave_view::readouts::{format_code, format_value};

fn exact(num: i64, den: i64) -> Exact {
    Exact::new(num, den).unwrap()
}

#[test]
fn a_code_reads_to_one_step_and_its_figures() {
    let none = Exact::ZERO;
    // verge's V in mV and I in nA, as the web's float rows read them.
    assert_eq!(
        format_code(3305, exact(1, 1000), none, "V", None),
        "3.305\u{a0}V"
    );
    assert_eq!(
        format_code(32_898_465, exact(1, 1_000_000_000), none, "A", None),
        "32.898465\u{a0}mA"
    );
    assert_eq!(
        format_code(350_123_456, exact(1, 1_000_000_000), none, "A", Some(4)),
        "350.1\u{a0}mA"
    );
    assert_eq!(
        format_code(0, exact(1, 1_000_000_000), none, "A", Some(4)),
        "0\u{a0}nA"
    );
    // P, the integer product of those two codes, in W at 1/10^12: 108,729,426,825 pW.
    let p = 32_898_465i128 * 3305;
    let pw = exact(1, 1_000_000_000_000);
    assert_eq!(format_code(p, pw, none, "W", None), "108.729426825\u{a0}mW");
    assert_eq!(format_code(p, pw, none, "W", Some(4)), "108.7\u{a0}mW");
}

#[test]
fn an_offset_adds_exactly_and_the_step_is_the_scales_size() {
    // 100 × 1/2 − 3/4 is 49.25; a step of 0.5 reads to 0.1, so 492.5 tenths, half away: 49.3.
    assert_eq!(
        format_code(100, exact(1, 2), exact(-3, 4), "V", None),
        "49.3\u{a0}V"
    );
    // A negative scale's step is its size: −10 mV to 1 mV, where the web's float route would
    // read a negative resolution as none and give four figures.
    assert_eq!(
        format_code(10, exact(-1, 1000), Exact::ZERO, "V", None),
        "\u{2212}10\u{a0}mV"
    );
}

#[test]
fn a_code_past_an_f64_keeps_every_digit() {
    // 2^60 + 1 at 1 V: an f64 holds 2^60, which prints as 1152921504606847000. The exact route
    // and the float route must disagree here.
    let code = (1i128 << 60) + 1;
    assert_eq!(
        format_code(code, Exact::ONE, Exact::ZERO, "V", None),
        "1152921504.606846977\u{a0}GV"
    );
    assert_eq!(
        format_value(code as f64, Some(1.0), "V", None).unwrap(),
        "1152921504.606847000\u{a0}GV"
    );
    // An unsigned 64-bit code's largest at 1 pW: 18,446,744.07... W, in MW.
    assert_eq!(
        format_code(
            i128::from(u64::MAX),
            exact(1, 1_000_000_000_000),
            Exact::ZERO,
            "W",
            None
        ),
        "18.446744073709551615\u{a0}MW"
    );
}
