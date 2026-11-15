//! tickwave's native renderer: a recording drawn with egui through the view core.
//!
//! A `Viewer` opens a recording, finished or still being written, and answers queries on a thread
//! of its own. A `Graph` draws one analog trace with digital lanes under it, and a `Strip` the
//! whole recording small; both turn the reader's gestures into changes of the host's
//! `ViewState`. Every colour and font comes from the host's `Palette`.
//!
//! A frame's work is in `tracing` spans at debug level: `graph`, and in it `layout`,
//! `build query`, `axes`, `trace`, `lane`, `markers` and `tessellate`, and `strip`. The query
//! thread's are `sketch job`, `query job` and `snapshot`, around the view core's own.

mod draw;
mod graph;
mod palette;
mod state;
mod strip;
mod viewer;

#[cfg(test)]
mod tests;

pub use escapement_tickwave_view as view;
pub use graph::{Extras, Graph};
pub use palette::Palette;
pub use state::{ViewState, Window, Y};
pub use strip::Strip;
pub use viewer::Viewer;
