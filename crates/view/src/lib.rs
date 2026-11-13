//! tickwave's view logic, apart from the drawing: readouts with exact digits, times, the value
//! scale, the time axis, pixel columns and point probes. Each module ports one of the web
//! viewer's TypeScript modules with the same inputs and outputs, held to the same tables of
//! cases in `testdata/view/`.

pub mod axis;
pub mod columns;
mod js;
pub mod probe;
pub mod readouts;
pub mod runs;
pub mod scale;
pub mod time;
