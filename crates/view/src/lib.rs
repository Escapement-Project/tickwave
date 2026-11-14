//! tickwave's view logic, apart from the drawing: a recording opened finished or read live, the
//! columns, samples, statistics and markers a view asks of it, and the readouts, times, value
//! scale, time axis and probes the web viewer has. Each module that ports one of the web's
//! TypeScript modules takes the same inputs and gives the same outputs, held to the same tables of
//! cases in `testdata/view/`.

pub mod axis;
mod bytes;
mod cache;
pub mod columns;
mod js;
mod markers;
pub mod probe;
mod query;
mod read;
pub mod readouts;
mod recording;
pub mod runs;
pub mod scale;
pub mod spans;
mod statistics;
pub mod time;

pub use bytes::Bytes;
pub use cache::Cache;
pub use escapement_tickwave_format as format;
pub use markers::{Marker, MarkerCursor, Page};
pub use query::{
    AnalogColumns, Answer, Drawn, MarkerAnswer, MarkerQuery, Point, Query, StripAnswer,
};
pub use recording::{ChannelInfo, ChannelKind, Error, Recording, Resolution, Revision};
pub use statistics::{ChannelStatistics, Statistics, Sum};
