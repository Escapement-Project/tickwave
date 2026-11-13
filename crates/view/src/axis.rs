//! The time axis: labelled ticks at 1, 2 or 5 times a power of ten seconds, never closer than one
//! tick of the recording, spaced so no two labels touch. Every label shares one unit. A port of
//! web/src/render/axis.ts.

use escapement_tickwave_format::model::Ratio;
use num_bigint::BigInt;

use crate::readouts::{TIME_UNITS, Unit, format_scaled, unit_for};

#[derive(Clone, Debug, PartialEq)]
pub struct AxisTick {
    /// The recording's tick, which a step finer than a whole tick count can leave between ticks.
    pub tick: f64,
    /// From the left of the plot, in the units `width` is in.
    pub x: f64,
    pub label: String,
    /// What `measure` gave for the label.
    pub width: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    pub ticks: Vec<AxisTick>,
    pub unit: Option<Unit>,
}

const MULTIPLES: [f64; 3] = [1.0, 2.0, 5.0];

/// Ticks for the view [start, end) over `width` pixels, with labels `measure` gives the width
/// of, and at least `gap` between them (the web's is 16). A label is kept only where it fits
/// inside the plot whole.
pub fn time_axis(
    start: f64,
    end: f64,
    width: f64,
    timebase: Ratio,
    measure: impl Fn(&str) -> f32,
    gap: f64,
) -> Axis {
    let none = Axis {
        ticks: Vec::new(),
        unit: None,
    };
    let span = end - start;
    if !(span > 0.0 && width > 0.0) {
        return none;
    }
    let per_px = span / width;
    let tick_seconds = timebase.den() as f64 / timebase.num() as f64;
    // Start a decade below the smaller of one tick and 24 pixels' worth of time.
    let mut e = ((crate::js::min(tick_seconds, (24.0 * per_px) * tick_seconds))
        .log10()
        .floor() as i32)
        .saturating_sub(1);
    for _ in 0..40 {
        for s in MULTIPLES {
            // `powf`, not `powi`: it rounds as the web's `10 ** e` does.
            let step_seconds = s * 10f64.powf(f64::from(e));
            if step_seconds < tick_seconds * (1.0 - 1e-12) {
                continue;
            }
            let step_ticks = step_seconds / tick_seconds;
            let step_px = step_ticks / per_px;
            if step_px < 24.0 {
                continue;
            }
            let m0 = (start / step_ticks - 1e-9).ceil() as i64;
            let m1 = (end / step_ticks + 1e-9).floor() as i64;
            if m1.saturating_sub(m0) > 400 {
                continue;
            }
            let s = s as i64;
            let biggest = BigInt::from(m0.unsigned_abs().max(m1.unsigned_abs())) * s;
            let unit = unit_for(&biggest, e, TIME_UNITS);
            let mut ticks = Vec::new();
            let mut widest = 0.0f64;
            for m in m0..=m1 {
                let label = format_scaled(&(BigInt::from(m) * s), e, TIME_UNITS, Some(unit));
                let w = measure(&label);
                widest = crate::js::max(widest, f64::from(w));
                let tick = m as f64 * step_ticks;
                ticks.push(AxisTick {
                    tick,
                    x: (tick - start) / per_px,
                    label,
                    width: w,
                });
            }
            if step_px < widest + gap {
                continue;
            }
            ticks.retain(|t| {
                let half = f64::from(t.width) / 2.0;
                t.x - half >= 0.0 && t.x + half <= width
            });
            return Axis {
                ticks,
                unit: Some(unit.clone()),
            };
        }
        e += 1;
    }
    none
}
