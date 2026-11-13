//! Where a channel's data lies: web/src/source.ts's `knownSpans`.

use std::ops::Range;

use escapement_tickwave_format::model::Gap;

use crate::runs::{Runs, place_after};

/// What `known_spans` reads of a channel.
#[derive(Clone, Copy, Debug)]
pub enum ChannelPlace<'a> {
    /// The level is known from `start` to `end`, outside `gaps`. A gap from tick 0 gives `start`
    /// and is not one of `gaps`.
    Digital {
        start: i64,
        end: i64,
        gaps: &'a [Gap],
    },
    /// The samples' places, and where the last one's time ends.
    Analog { runs: Runs<'a>, end: i64 },
}

impl ChannelPlace<'_> {
    /// The first tick the channel can hold data at: a digital channel's start, an analog
    /// channel's first sample.
    pub fn start(&self) -> i64 {
        match self {
            Self::Digital { start, .. } => *start,
            Self::Analog { runs, .. } => runs.first,
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
            for k in 0..=b.len() {
                let from = if k == 0 { runs.first } else { b[k - 1].tick };
                let to = match b.get(k) {
                    Some(next) => next.tick.min(place_after(runs, next.index - 1)),
                    None => *end,
                };
                keep(from, to);
            }
        }
        ChannelPlace::Digital { gaps, .. } => {
            let mut at = i64::MIN;
            for g in *gaps {
                keep(at, g.from);
                at = g.to;
            }
            keep(at, i64::MAX);
        }
    }
    spans
}
