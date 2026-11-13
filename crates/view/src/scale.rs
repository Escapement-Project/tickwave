//! An analog row's value range, as the reader zooms and moves it: the arithmetic, apart from the
//! pointer and the canvas. A port of web/src/render/scale.ts.

use crate::js;
use crate::readouts::nice_step;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValueRange {
    pub lo: f64,
    pub hi: f64,
}

/// How far a channel's range may zoom and where it may sit. `whole` is its range unzoomed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScaleLimits {
    pub whole: ValueRange,
    pub min_span: f64,
    pub max_span: f64,
}

/// In to four steps of the instrument's resolution, where it has one, and out to four times the
/// whole.
pub fn scale_limits(whole: ValueRange, resolution: Option<f64>) -> ScaleLimits {
    let span = whole.hi - whole.lo;
    let min_span = match resolution {
        Some(r) if r > 0.0 => js::max(4.0 * r, span * 1e-6),
        _ => span * 1e-4,
    };
    ScaleLimits {
        whole,
        min_span: js::min(min_span, span),
        max_span: 4.0 * span,
    }
}

/// `r` with its span within the limits, about its middle, and its middle within the whole range.
pub fn clamp_range(r: ValueRange, lim: &ScaleLimits) -> ValueRange {
    let span = js::min(lim.max_span, js::max(lim.min_span, r.hi - r.lo));
    let mid = js::min(lim.whole.hi, js::max(lim.whole.lo, (r.lo + r.hi) / 2.0));
    ValueRange {
        lo: mid - span / 2.0,
        hi: mid + span / 2.0,
    }
}

/// `r` zoomed by `factor` (above 1 zooms out) about the value `at`, which keeps its place.
pub fn zoom_range(r: ValueRange, at: f64, factor: f64, lim: &ScaleLimits) -> ValueRange {
    let span = js::min(lim.max_span, js::max(lim.min_span, (r.hi - r.lo) * factor));
    let f = (at - r.lo) / (r.hi - r.lo);
    clamp_range(
        ValueRange {
            lo: at - f * span,
            hi: at - f * span + span,
        },
        lim,
    )
}

/// `r` moved by `by` in value.
pub fn move_range(r: ValueRange, by: f64, lim: &ScaleLimits) -> ValueRange {
    clamp_range(
        ValueRange {
            lo: r.lo + by,
            hi: r.hi + by,
        },
        lim,
    )
}

/// The range that just holds the values from `min` to `max`, or None for none.
pub fn fit_range(min: f64, max: f64, lim: &ScaleLimits) -> Option<ValueRange> {
    // Not `min > max`: a NaN fits nothing.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    if !(min <= max) {
        return None;
    }
    Some(clamp_range(ValueRange { lo: min, hi: max }, lim))
}

/// Where an analog row's two scale labels go, and the decimal place they carry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Marks {
    pub top: f64,
    pub bottom: f64,
    pub place: i32,
}

/// The highest and lowest multiples of a round step inside `r`, and the decimal place they
/// carry. The step is at most a quarter of the range, so the labels stay near its edges, clear of
/// the channel's name between them. None for a range with no width.
pub fn range_marks(r: ValueRange) -> Option<Marks> {
    let step = nice_step((r.hi - r.lo) / 10.0)?;
    // 3.6 / 0.2 is 17.999999999999996 in binary: a value on a step counts as on it.
    Some(Marks {
        top: (r.hi / step.step + 1e-9).floor() * step.step,
        bottom: (r.lo / step.step - 1e-9).ceil() * step.step,
        place: step.place,
    })
}
