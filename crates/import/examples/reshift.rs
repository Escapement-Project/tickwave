//! A recording written again with a finest overview level of its caller's choosing for every
//! channel, its content unchanged: for a fixture whose finest level sits in overview blocks, which
//! the converter's density rule would not give it.
//!
//!     cargo run -p escapement-tickwave-import --example reshift -- IN OUT SHIFT

use std::sync::Arc;

use escapement_tickwave_format::encode::{Bundle, Options, encode};
use escapement_tickwave_import::recording::read_bundled;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output, shift] = args.as_slice() else {
        return Err("usage: reshift IN OUT SHIFT".into());
    };
    let (header, bundled, recording) = read_bundled(Arc::new(std::fs::read(input)?))?;
    let out = encode(recording, &{
        let mut o = Options::default();
        o.title = header.title.clone();
        o.meta = header.meta.clone();
        o.analog = header.channels.iter().map(|c| c.coding()).collect();
        o.overview_shift = Some(shift.parse()?);
        o.bundle = bundled.map(Bundle::Files);
        o
    })?;
    std::fs::write(output, out)?;
    Ok(())
}
