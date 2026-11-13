//! Where the port reproduces JavaScript's number semantics, and the shared tables have no case
//! that tells the two apart. Each expected value is what the web's own function gives, printed
//! by Node.

use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_view::probe::format_duty;
use escapement_tickwave_view::readouts::{
    format_at, format_brief, format_interval, format_value, nice_step,
};
use escapement_tickwave_view::time::{parse_time, to_ticks};

const GS: Ratio = Ratio::new(1_000_000_000, 1).unwrap();

#[test]
fn a_duty_rounds_as_to_fixed_and_math_round_do() {
    // 1 of 400 is 0.25 %, a tie at its one decimal: toFixed takes the larger, where Rust's own
    // formatting takes the even one, 0.2. At a place of ten or more, Math.round of 1.25 is 1.
    let cases = [
        (1, 400, "0.3\u{a0}%"),
        (1, 8, "10\u{a0}%"),
        (3, 7, "40\u{a0}%"),
        (1, 1600, "0.06\u{a0}%"),
        (7, 2, "350\u{a0}%"),
        (0, 5, "0\u{a0}%"),
    ];
    for (high, period, want) in cases {
        assert_eq!(format_duty(high, period, 1), want, "{high} of {period}");
    }
}

#[test]
fn a_value_reads_as_the_decimal_it_prints_as() {
    // 0.1 + 0.2 prints as 0.30000000000000004; 2^60 prints as 1152921504606847000, so its
    // digits past the 17th are zeros, not the binary value's 976.
    let cases = [
        (0.1 + 0.2, None, "300.0\u{a0}mV"),
        (
            1152921504606846976.0,
            Some(1.0),
            "1152921504.606847000\u{a0}GV",
        ),
        (9007199254740994.0, Some(1.0), "9007199.254740994\u{a0}GV"),
        (-0.01, Some(0.001), "\u{2212}10\u{a0}mV"),
        (1e-15, None, "0.001000\u{a0}pV"),
        (123456789.0, None, "123.5\u{a0}MV"),
        // A negative step is no step, as the web reads it: four figures.
        (-0.01, Some(-0.001), "\u{2212}10.00\u{a0}mV"),
    ];
    for (value, resolution, want) in cases {
        assert_eq!(
            format_value(value, resolution, "V", None).unwrap(),
            want,
            "{value:e}"
        );
    }
    assert_eq!(format_value(f64::NAN, None, "V", None), None);
    assert_eq!(format_value(1.0, Some(f64::INFINITY), "V", None), None);
    assert_eq!(
        format_at(0.30000000000000004, -1, "V").unwrap(),
        "300\u{a0}mV"
    );
    assert_eq!(format_at(-2.5e-7, -7, "A").unwrap(), "\u{2212}300\u{a0}nA");
}

#[test]
fn intervals_below_zero() {
    assert_eq!(format_brief(-999_600_000, GS, 1), "\u{2212}1.00\u{a0}s");
    assert_eq!(
        format_interval(-1, Ratio::new(3, 1).unwrap(), 1),
        "\u{2212}300\u{a0}ms"
    );
}

#[test]
fn nice_steps_as_the_web_takes_them() {
    let cases = [
        (0.3, 0.5, -1),
        (1e-7, 1e-7, -7),
        (7.0, 10.0, 1),
        (999.0, 1000.0, 3),
        (0.58 / 10.0, 0.1, -1),
    ];
    for (x, step, place) in cases {
        let s = nice_step(x).unwrap();
        assert_eq!((s.step, s.place), (step, place), "{x}");
    }
    assert_eq!(nice_step(0.0), None);
    assert_eq!(nice_step(f64::NAN), None);
}

#[test]
fn times_as_the_web_parses_them() {
    // JavaScript's white space includes the no-break space and U+2028; its \d is ASCII only; a
    // tick count stops at 2^53 − 1; the unit is looked up as written, so MS and a capital mu are
    // no unit.
    let cases = [
        ("\u{a0} 12 ms \u{2028}", Some(12_000_000)),
        ("12\tus", Some(12_000)),
        ("1.5\u{3bc}s", Some(1500)),
        ("007", Some(7)),
        ("9007199254740991", Some(9_007_199_254_740_991)),
        ("9007199254740992", None),
        ("1. ms", None),
        (".5 ms", None),
        ("1.5 MS", None),
        ("12 \u{39c}s", None),
    ];
    for (text, want) in cases {
        let got = parse_time(text).and_then(|t| to_ticks(&t, GS));
        assert_eq!(got, want, "{text:?}");
    }
}
