//! Where an analog channel's samples sit, and the sample at or around a tick: web/src/source.ts's
//! index helpers, which the columns and the probe read. A tick here is an f64, as the web's is,
//! since a pixel's edge falls between ticks.

use escapement_tickwave_format::model::{Break, Ratio, run_sample_tick, sample_tick};

/// An analog channel's samples' places: its first tick, period, count and breaks.
#[derive(Clone, Copy, Debug)]
pub struct Runs<'a> {
    pub first: i64,
    pub period: Ratio,
    /// `u64::MAX` for a tile whose channel's count doesn't matter.
    pub count: u64,
    pub breaks: &'a [Break],
}

impl Runs<'_> {
    /// The tick of sample `i`, its breaks included.
    pub fn tick(&self, i: u64) -> i64 {
        run_sample_tick(self.first, self.period, self.breaks, i)
    }
}

/// The index of the first sample whose tick is at or after `tick`, clamped to `0..=count`, for
/// samples from `first` every `period`.
pub fn sample_index_at_or_after(first: i64, period: Ratio, count: u64, tick: f64) -> u64 {
    let estimate = (((tick - first as f64) * period.den() as f64) / period.num() as f64).ceil();
    // The estimate can land one off either way; settle it against the sample ticks themselves.
    let mut i = if estimate > 0.0 {
        if estimate >= count as f64 {
            count
        } else {
            estimate as u64
        }
    } else {
        0
    };
    while i > 0 && sample_tick(first, period, i - 1) as f64 >= tick {
        i -= 1;
    }
    while i < count && (sample_tick(first, period, i) as f64) < tick {
        i += 1;
    }
    i
}

/// The index of the first sample at or after `tick`, breaks included, clamped to `0..=count`.
pub fn index_at_or_after(runs: &Runs, tick: f64) -> u64 {
    let b = runs.breaks;
    // The run whose start is the last at or before `tick`; its samples end where the next run's
    // start.
    let mut k = b.len();
    while k > 0 && b[k - 1].tick as f64 > tick {
        k -= 1;
    }
    let from = if k == 0 { 0 } else { b[k - 1].index };
    let to = if k < b.len() { b[k].index } else { runs.count };
    let start = if k == 0 { runs.first } else { b[k - 1].tick };
    from + sample_index_at_or_after(start, runs.period, to - from, tick)
}

/// The index of the last sample at or before `tick`: the value a cursor there reads. None before
/// the first sample or at or past `end`, the end of the channel's data.
pub fn sample_at_or_before(runs: &Runs, end: i64, tick: f64) -> Option<u64> {
    if tick < runs.first as f64 || tick >= end as f64 || runs.count == 0 {
        return None;
    }
    if !runs.breaks.is_empty() {
        let i = index_at_or_after(runs, tick + 1.0).checked_sub(1)?;
        return Some(i.min(runs.count - 1));
    }
    let (first, period) = (runs.first, runs.period);
    let estimate = (((tick - first as f64) * period.den() as f64) / period.num() as f64).floor();
    let mut i = if estimate > 0.0 { estimate as u64 } else { 0 };
    // The division can land one off; settle it against the sample ticks themselves.
    while i > 0 && sample_tick(first, period, i) as f64 > tick {
        i -= 1;
    }
    while i + 1 < runs.count && sample_tick(first, period, i + 1) as f64 <= tick {
        i += 1;
    }
    Some(i.min(runs.count - 1))
}
