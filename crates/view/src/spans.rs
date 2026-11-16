//! Where a channel's data lies: web/src/source.ts's `knownSpans`.

use std::ops::Range;

use escapement_tickwave_format::model::Gap;

use crate::runs::{Runs, place_after};

/// What `known_spans` reads of a channel.
#[derive(Clone, Copy, Debug)]
pub enum ChannelPlace<'a> {
    /// The level is known from `start` to `end`, outside `gaps`, all axis ticks. A gap from the
    /// channel's start gives `start` and is not one of `gaps`.
    Digital {
        start: i64,
        end: i64,
        gaps: &'a [Gap],
    },
    /// The samples' places, and where the last one's time ends on the axis.
    Analog { runs: Runs<'a>, end: i64 },
}

impl ChannelPlace<'_> {
    /// The first tick the channel can hold data at: a digital channel's start, an analog
    /// channel's first sample.
    pub fn start(&self) -> i64 {
        match self {
            Self::Digital { start, .. } => *start,
            Self::Analog { runs, .. } => runs.start(),
        }
    }

    pub fn end(&self) -> i64 {
        match self {
            Self::Digital { end, .. } | Self::Analog { end, .. } => *end,
        }
    }
}

/// The ticks of a channel's data, clipped to [start, end): a digital channel's split around its
/// gaps, an analog channel's into its runs, each from its first sample to the place after its
/// last on its grid, or to the next run's start if that comes sooner.
pub fn known_spans(c: &ChannelPlace, start: i64, end: i64) -> Vec<Range<i64>> {
    let mut spans = Vec::new();
    let stop = end.min(c.end());
    let mut keep = |a: i64, b: i64| {
        let (s, e) = (a.max(start).max(c.start()), b.min(stop));
        if e > s {
            spans.push(s..e);
        }
    };
    match c {
        ChannelPlace::Analog { runs, end } => {
            if runs.count == 0 {
                return spans;
            }
            let b = runs.breaks;
            // Run k starts at break k − 1's tick: the runs before the one holding `start` end
            // before it, and those from `stop` on start after the window.
            for k in b.partition_point(|x| runs.at(runs.grid.start(*x)) <= start)..=b.len() {
                let from = if k == 0 {
                    runs.start()
                } else {
                    runs.break_tick(k - 1)
                };
                if from >= stop {
                    break;
                }
                let to = match b.get(k) {
                    Some(next) => runs.break_tick(k).min(place_after(runs, next.index - 1)),
                    None => *end,
                };
                keep(from, to);
            }
        }
        ChannelPlace::Digital { gaps, .. } => {
            // The gaps that end before `start` bound no span that reaches it.
            let k = gaps.partition_point(|g| g.to < start);
            let mut at = k.checked_sub(1).map_or(i64::MIN, |p| gaps[p].to);
            for g in &gaps[k..] {
                if at >= stop {
                    break;
                }
                keep(at, g.from);
                at = g.to;
            }
            keep(at, i64::MAX);
        }
    }
    spans
}
