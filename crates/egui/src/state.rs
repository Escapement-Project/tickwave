//! What the host keeps between frames: the window on screen, the value range, the selection.

use std::ops::Range;

/// The time on screen, in ticks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Window {
    /// The last `span` ticks of the recording, moving with its end: from `span` before the end
    /// of the extent, or from its start where the extent is shorter.
    Follow {
        span: i64,
    },
    At(Range<i64>),
}

/// The trace's value range, in its unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Y {
    /// What the window shows, with zero where the graph includes it, a margin, and room for the
    /// marker labels.
    Fit,
    Fixed {
        low: f64,
        high: f64,
    },
}

/// The view a host keeps for a `Graph`. The graph's gestures change it, and so may the host.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewState {
    pub window: Window,
    pub y: Y,
    pub selection: Option<Range<i64>>,
    /// A drag selects rather than pans.
    pub select_mode: bool,
}

impl Default for ViewState {
    /// The whole recording, following its end.
    fn default() -> Self {
        Self {
            window: Window::Follow { span: i64::MAX },
            y: Y::Fit,
            selection: None,
            select_mode: false,
        }
    }
}

impl Window {
    /// The ticks on screen given the recording's extent, at least one wide. Before any data a
    /// followed window starts at tick 0.
    pub fn resolve(&self, extent: Option<&Range<i64>>) -> Range<i64> {
        match self {
            Self::At(r) => r.start..r.end.max(r.start.saturating_add(1)),
            Self::Follow { span } => match extent {
                Some(e) => e.end.saturating_sub(*span).max(e.start)..e.end.max(e.start + 1),
                None => 0..(*span).max(1),
            },
        }
    }
}
