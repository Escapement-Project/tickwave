//! JTAG over a recording's TCK, TMS, TDI and TDO (TDO where it has one), every item kept,
//! printed as `swd_canon` prints SWD.
//!
//! `cargo run --release -p escapement-tickwave-decoders --example jtag_canon -- REC`

#[path = "canon/mod.rs"]
mod canon;

use escapement_tickwave_decode::{Host, InputSpec, Span, Tick, name, wire::WireSource};
use escapement_tickwave_decoders::jtag;
use escapement_tickwave_view::{Cache, Recording};
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: jtag_canon REC")?;
    let file = std::fs::File::open(&path)?;
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(64 << 20))?);
    let mut lines = vec![];
    for n in ["TCK", "TMS", "TDI"] {
        lines.push(rec.channel(n).ok_or(format!("no {n}"))?);
    }
    let tdo = rec.channel("TDO");
    lines.extend(tdo);
    let source = WireSource::new(rec.clone(), lines)?;
    let all = vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }];
    let mut h = Host::default();
    let j = h.add(
        name("jtag"),
        jtag::definition(),
        InputSpec::Wire {
            source: Box::new(source),
            local: None,
        },
        jtag::Jtag::factory(tdo.is_some(), 50_000),
        all,
    )?;
    let start = std::time::Instant::now();
    h.run_until(|_| false)?;
    let seconds = start.elapsed().as_secs_f64();
    canon::print(&h, &[(j, None)]);
    println!("seconds {seconds:.2}");
    Ok(())
}
