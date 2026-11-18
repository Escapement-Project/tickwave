//! JTAG over a recording's TCK, TMS, TDI and TDO, checked against what IEEE 1149.1 guarantees
//! where TDO is recorded: every IR scan captures a value whose two lowest bits are 01, and a DR
//! scan after Test-Logic-Reset, before any IR scan, shifts out IDCODE (bit 0 high) or BYPASS (one
//! low bit). A checker stacked on JTAG reads its completed items; nothing is kept, so memory
//! follows the decoders' state, not the capture.
//!
//! `cargo run --release -p escapement-tickwave-decoders --example jtag_check -- REC [--tdo-at-fall]`

#[path = "check/mod.rs"]
mod check;

use check::{Checker, Seen};
use escapement_tickwave_decode::{
    Definition, Host, InputSpec, Mode, Schema, Subscription, name, wire::WireSource,
};
use escapement_tickwave_decoders::jtag;
use escapement_tickwave_view::{Cache, Recording};
use std::sync::{Arc, Mutex};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: jtag_check REC [--tdo-at-fall]")?;
    let at_fall = std::env::args().any(|a| a == "--tdo-at-fall");
    let file = std::fs::File::open(&path)?;
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(64 << 20))?);
    let mut lines = vec![];
    for n in ["TCK", "TMS", "TDI"] {
        lines.push(rec.channel(n).ok_or(format!("no {n}"))?);
    }
    let tdo = rec.channel("TDO");
    lines.extend(tdo);
    let source = WireSource::new(rec.clone(), lines)?;
    let mut h = Host::default();
    let j = h.add(
        name("jtag"),
        jtag::definition(),
        InputSpec::Wire {
            source: Box::new(source),
            local: None,
        },
        jtag::Jtag::factory_with(tdo.is_some(), 50_000, at_fall),
        vec![],
    )?;
    let seen = Arc::new(Mutex::new(Seen::default()));
    let s2 = seen.clone();
    h.add(
        name("check"),
        Arc::new(Definition::new("check", 1, Schema::new(vec![]))),
        InputSpec::Upstream(
            Subscription::new(j, Mode::Completed)
                .with_kinds(["state", "path", "ir-scan", "dr-scan"]),
        ),
        Box::new(move |_| Box::new(Checker(s2.clone()))),
        vec![],
    )?;
    let start = std::time::Instant::now();
    h.run_until(|_| false)?;
    let seconds = start.elapsed().as_secs_f64();
    let label = format!(
        "TDO {}{}",
        tdo.is_some(),
        if at_fall {
            " at TCK's fall"
        } else {
            " at TCK's rise"
        }
    );
    check::report(&h, j, &seen, &rec, &label, seconds);
    Ok(())
}
