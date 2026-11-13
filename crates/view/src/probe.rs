//! What the view measures at one point: a digital channel's pulse around a tick, with its width,
//! period, frequency and duty, and an analog channel's value or range at a pixel. web/src/probe.ts's
//! pure functions; finding the edges past what is in hand needs the data layer.

use escapement_tickwave_format::model::Ratio;

use crate::columns::{AnalogOverview, Edges, Samples};
use crate::js;
use crate::readouts::{format_interval, format_rate};
use crate::runs::{Runs, sample_at_or_before};

/// The ticks where a channel's level is known, `start..end`: every edge lies after `start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Known {
    pub start: i64,
    pub end: i64,
}

/// Where a pulse's edge is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeAt {
    Tick(i64),
    /// There is none: the pulse runs from where the channel's level is first known, or to its
    /// end.
    Open,
    /// The edges in hand don't reach it.
    Unread,
}

impl EdgeAt {
    /// The edge's tick, None where it is open or unread.
    pub fn tick(self) -> Option<i64> {
        match self {
            Self::Tick(t) => Some(t),
            Self::Open | Self::Unread => None,
        }
    }
}

/// The pulse a tick lies in: the level over it and the edges around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pulse {
    pub level: bool,
    /// The edge the pulse starts at.
    pub start: EdgeAt,
    /// The edge it ends at.
    pub end: EdgeAt,
    /// The edge after `end`, which closes the period that starts at `start`.
    pub next: EdgeAt,
}

/// The pulse at `tick` from edges read over [from, to), which hold `tick`. An edge at a tick is
/// where the new level starts.
pub fn pulse_from_edges(e: &Edges, from: i64, to: i64, known: Known, tick: i64) -> Pulse {
    let t = e.ticks;
    // The edges at or before the tick.
    let i = t.partition_point(|&x| x <= tick);
    let level = e.level ^ (i % 2 == 1);
    let start = match i {
        0 if from <= known.start + 1 => EdgeAt::Open,
        0 => EdgeAt::Unread,
        _ => EdgeAt::Tick(t[i - 1]),
    };
    let after = |k: usize| match t.get(k) {
        Some(&x) => EdgeAt::Tick(x),
        None if to >= known.end => EdgeAt::Open,
        None => EdgeAt::Unread,
    };
    let end = after(i);
    let next = if end == EdgeAt::Open {
        EdgeAt::Open
    } else {
        after(i + 1)
    };
    Pulse {
        level,
        start,
        end,
        next,
    }
}

/// Whether every edge the pulse's figures need is in hand.
pub fn complete(p: &Pulse) -> bool {
    p.start != EdgeAt::Unread && p.end != EdgeAt::Unread && p.next != EdgeAt::Unread
}

/// A pulse's figures, as text: what a label or a readout line shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PulseFigures {
    pub level: &'static str,
    /// The pulse's width; "at least ..." where it runs from the capture's start or to its end.
    pub width: String,
    /// The width's reciprocal as a rate, where both edges are known: one UART bit gives the baud
    /// rate.
    pub inverse_width: Option<String>,
    pub period: Option<String>,
    pub frequency: Option<String>,
    /// The share of the period spent high.
    pub duty: Option<String>,
}

/// A pulse's figures, an unread edge read as open. Widths are whole ticks; a rate and a duty
/// carry the digits the ticks support. `step`, at least 1, is the ticks one tick of the
/// channel's source spans.
pub fn pulse_figures(p: &Pulse, timebase: Ratio, known: Known, step: u64) -> PulseFigures {
    let level = if p.level { "high" } else { "low" };
    let (s, e, n) = (p.start.tick(), p.end.tick(), p.next.tick());
    let interval = |ticks| format_interval(ticks, timebase, step);
    let width = match (s, e) {
        (Some(s), Some(e)) => interval(e - s),
        // A channel known from the capture's start spans it; one moved or trimmed spans what it
        // records.
        (None, None) => format!(
            "{}, {}",
            if known.start == 0 {
                "the whole capture"
            } else {
                "all it records"
            },
            interval(known.end - known.start)
        ),
        (Some(s), None) => format!("at least {}", interval(known.end - s)),
        (None, Some(e)) => format!("at least {}", interval(e - known.start)),
    };
    let inverse_width = match (s, e) {
        (Some(s), Some(e)) => format_rate(1, e - s, timebase, 1, step),
        _ => None,
    };
    let (Some(s), Some(e), Some(n)) = (s, e, n) else {
        return PulseFigures {
            level,
            width,
            inverse_width,
            period: None,
            frequency: None,
            duty: None,
        };
    };
    let period = n - s;
    let high = if p.level { e - s } else { n - e };
    PulseFigures {
        level,
        width,
        inverse_width,
        period: Some(interval(period)),
        frequency: format_rate(1, period, timebase, 1, step),
        duty: Some(format_duty(high, period, step)),
    }
}

/// `high` of `period` ticks, as a percentage. Each span is off by less than a tick of the
/// source, `step` ticks, so the share is uncertain by about 100 step / period percent; the last
/// digit shown is at the largest power of ten not larger than that, as a rate's is.
pub fn format_duty(high: i64, period: i64, step: u64) -> String {
    let place = ((100.0 * step as f64) / period as f64).log10().floor();
    let share = (100.0 * high as f64) / period as f64;
    // A no-break space before the unit, as every readout writes it.
    if place < 0.0 {
        return format!("{}\u{a0}%", js::to_fixed(share, (-place) as u32));
    }
    // `powf`, not `powi`: it rounds as the web's `10 ** place` does.
    let unit = 10f64.powf(place);
    format!("{}\u{a0}%", js::string(js::round(share / unit) * unit))
}

/// An analog channel at one pixel of the plot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnalogAt {
    Value(f64),
    Range { min: f64, max: f64 },
    None,
}

fn reading(min: f64, max: f64) -> AnalogAt {
    if min > max {
        AnalogAt::None
    } else if min == max {
        AnalogAt::Value(min)
    } else {
        AnalogAt::Range { min, max }
    }
}

/// An analog channel over the ticks [t0, t1) a pixel covers, from samples read over the span:
/// the one sample at or before `tick` where the pixel holds at most one, else the smallest and
/// largest in it, as the trace draws them. `end` is the end of the channel's data.
pub fn analog_from_samples<T: Copy + Into<f64>>(
    c: &Runs,
    end: i64,
    s: &Samples<T>,
    t0: f64,
    t1: f64,
    tick: f64,
) -> AnalogAt {
    let first = sample_at_or_before(c, end, js::max(c.first as f64, t0.ceil()));
    let at = sample_at_or_before(c, end, tick.floor());
    let (Some(first), Some(at)) = (first, at) else {
        return AnalogAt::None;
    };
    let mut last = first;
    while last + 1 < c.count && (c.tick(last + 1) as f64) < t1 {
        last += 1;
    }
    let value = |i: u64| {
        i.checked_sub(s.first)
            .and_then(|k| s.values.get(k as usize))
            .map_or(f64::NAN, |&v| v.into())
    };
    if last <= first || c.tick(first + 1) as f64 >= t1 {
        let v = value(at);
        return if v.is_finite() {
            AnalogAt::Value(v)
        } else {
            AnalogAt::None
        };
    }
    let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
    for v in (first..=last).map(value).filter(|v| v.is_finite()) {
        if v < min {
            min = v;
        }
        if v > max {
            max = v;
        }
    }
    reading(min, max)
}

/// An analog channel over [t0, t1) from an overview: the smallest and largest of the buckets it
/// meets.
pub fn analog_from_overview<T: Copy + Into<f64>>(
    o: &AnalogOverview<T>,
    t0: f64,
    t1: f64,
) -> AnalogAt {
    let (start, bucket) = (o.start as f64, o.bucket as f64);
    let k0 = js::max(0.0, ((t0 - start) / bucket).floor()) as usize;
    let k1 = js::min(o.min.len() as f64, ((t1 - start) / bucket).ceil()) as usize;
    let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
    for k in k0..k1 {
        // A bucket past the maxima reads as no maximum, as the web's read past the end does.
        let (a, b) = (
            o.min[k].into(),
            o.max.get(k).map_or(f64::NEG_INFINITY, |&b| b.into()),
        );
        if a < min {
            min = a;
        }
        if b > max {
            max = b;
        }
    }
    reading(min, max)
}
