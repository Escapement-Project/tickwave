//! JTAG over a recording's TCK, TMS, TDI and TDO, checked against what IEEE 1149.1 guarantees
//! where TDO is recorded: every IR scan captures a value whose two lowest bits are 01, and a DR
//! scan after Test-Logic-Reset, before any IR scan, shifts out IDCODE (bit 0 high) or BYPASS (one
//! low bit). A checker stacked on JTAG reads its completed items; nothing is kept, so memory
//! follows the decoders' state, not the capture. `--rows` keeps each item's span, to check that
//! the items fit the rows and lanes JTAG's schema declares.
//!
//! `cargo run --release -p escapement-tickwave-decoders --example jtag_check -- REC [--tdo-at-rise | --tdo-at-fall] [--rows]`
//!
//! TDO is read where the IR captures read unless a flag fixes the edge.

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
        .ok_or("usage: jtag_check REC [--tdo-at-rise | --tdo-at-fall] [--rows]")?;
    let rows = std::env::args().any(|a| a == "--rows");
    let flag = |f: &str| std::env::args().any(|a| a == f);
    let edge = match (flag("--tdo-at-rise"), flag("--tdo-at-fall")) {
        (true, _) => jtag::TdoEdge::Rise,
        (_, true) => jtag::TdoEdge::Fall,
        _ => jtag::TdoEdge::IrCapture,
    };
    let file = std::fs::File::open(&path)?;
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(64 << 20))?);
    let mut lines = vec![];
    for n in ["TCK", "TMS", "TDI"] {
        lines.push(Some(rec.channel(n).ok_or(format!("no {n}"))?));
    }
    let tdo = rec.channel("TDO");
    lines.push(tdo);
    let source = WireSource::with_ports(rec.clone(), lines)?;
    let mut h = Host::default();
    let j = h.add(
        name("jtag"),
        jtag::definition(),
        InputSpec::Wire {
            source: Box::new(source),
            local: None,
        },
        jtag::Jtag::factory(50_000),
        &[jtag::tdo_edge(edge)],
        vec![],
    )?;
    let seen = Arc::new(Mutex::new(Seen {
        spans: rows.then(Vec::new),
        ..Seen::default()
    }));
    let s2 = seen.clone();
    h.add(
        name("check"),
        Arc::new(Definition::new("check", 1, Schema::new(vec![]))),
        InputSpec::Upstream(
            Subscription::new(j, Mode::Completed)
                .with_kinds(["state", "path", "ir-scan", "dr-scan"]),
        ),
        Box::new(move |_, _| Box::new(Checker(s2.clone()))),
        &[],
        vec![],
    )?;
    let start = std::time::Instant::now();
    h.run_until(|_| false)?;
    let seconds = start.elapsed().as_secs_f64();
    let label = format!(
        "TDO {}{}",
        tdo.is_some(),
        match edge {
            jtag::TdoEdge::Rise => " at TCK's rise",
            jtag::TdoEdge::Fall => " at TCK's fall",
            _ => " where the IR captures read",
        }
    );
    let tck = rec.channel("TCK");
    check::report(&h, j, &seen, &rec, tck, &label, seconds);
    Ok(())
}
