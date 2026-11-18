//! How soon a window shows its decode while the forward pass is still running: cJTAG with JTAG
//! over it, the forward pass keeping checkpoints and no items, stopped at a point, then windows
//! behind it re-derived beside it in chains of their own, each timed to drawn and settled and
//! retired. The forward pass then runs to the end, and its counts are printed to compare with a
//! run that derived nothing.
//!
//! `cargo run --release -p escapement-tickwave-decoders --example cjtag_window -- REC [PAUSE_S]
//! [EVENTS]`

#[path = "check/mod.rs"]
mod check;

use escapement_tickwave_decode::{Host, InputSpec, Keeping, Span, Tick, name, wire::WireSource};
use escapement_tickwave_decoders::cjtag;
use escapement_tickwave_view::{Cache, Recording};
use std::sync::Arc;
use std::time::Instant;

fn arg(n: usize, default: f64) -> f64 {
    std::env::args()
        .nth(n)
        .and_then(|a| a.parse().ok())
        .unwrap_or(default)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: cjtag_window REC [PAUSE_S] [EVENTS]")?;
    let (pause_s, events) = (arg(2, 28.0), arg(3, 50_000.0) as u64);
    let file = std::fs::File::open(&path)?;
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(64 << 20))?);
    let tb = rec.timebase();
    let per_s = tb.num() as f64 / tb.den() as f64;
    let ticks = |s: f64| (s * per_s) as Tick;
    let lines = vec![
        rec.channel("TCKC").ok_or("no TCKC")?,
        rec.channel("TMSC").ok_or("no TMSC")?,
    ];
    let mut h = Host::default();
    let c = h.add(
        name("cjtag"),
        cjtag::definition(),
        InputSpec::Wire {
            source: Box::new(WireSource::new(rec.clone(), lines)?),
            local: None,
        },
        cjtag::Cjtag::factory(true, 1_000_000),
        vec![],
    )?;
    let j = h.add(
        name("jtag"),
        cjtag::jtag_definition(),
        InputSpec::Upstream(cjtag::jtag_subscription(c)),
        cjtag::JtagOnCjtag::factory(),
        vec![],
    )?;
    h.keep_checkpoints(c, Keeping::new(ticks(1.0), 64 << 20).with_events(events))?;
    let pause = ticks(pause_s);
    let start = Instant::now();
    h.run_until(|h| h.consumed(c) >= pause)?;
    println!(
        "forward pass paused at {:.3} s of input after {:.2} s",
        h.consumed(c) as f64 / per_s,
        start.elapsed().as_secs_f64()
    );
    let mut drawn_all = vec![];
    for at_s in [1.0, 5.0, 10.0, 15.0, 20.0, 22.5, 25.0, 27.0, pause_s - 0.5] {
        let w = Span {
            start: ticks(at_s),
            end: ticks(at_s + 1e-3),
        };
        let asked = Instant::now();
        let d = h.derive(c, w)?;
        let (wc, wj) = (d.chain[0].1, d.chain[1].1);
        h.run_until(|h| h.consumed(wc) >= w.end && h.quiet_below(wc))?;
        let drawn = asked.elapsed().as_secs_f64() * 1e3;
        h.run_until(|h| h.settled(wj) >= w.end)?;
        let settled = asked.elapsed().as_secs_f64() * 1e3;
        let items = (h.store(wc).items.len(), h.store(wj).items.len());
        println!(
            "  window {at_s:5.1} s: restored {:.2} ms of input before it; drawn by {drawn:.1} ms, settled by {settled:.1} ms; items cJTAG {}, JTAG {}; the forward pass at {:.3} s",
            d.restored.map_or(f64::NAN, |t| (w.start - t) as f64 / per_s * 1e3),
            items.0,
            items.1,
            h.consumed(c) as f64 / per_s,
        );
        h.retire(wc);
        drawn_all.push(drawn);
    }
    drawn_all.sort_by(f64::total_cmp);
    println!(
        "drawn: median {:.1} ms, worst {:.1} ms",
        drawn_all[drawn_all.len() / 2],
        drawn_all[drawn_all.len() - 1]
    );
    h.run_until(|_| false)?;
    for (x, n) in [(c, "cjtag"), (j, "jtag")] {
        let k = h.store(x).counts;
        println!(
            "{n} at the end: items {} ops {} elements {} digest {:016x}",
            k.opens, k.ops, k.elements, k.digest
        );
    }
    println!(
        "kept {} checkpoints; peak RSS {} MiB",
        h.kept(c).map_or(0, |k| k.count),
        check::peak_rss_kb() / 1024
    );
    Ok(())
}
