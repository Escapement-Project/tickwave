//! How soon a window late in a cJTAG recording shows its decode after one forward pass: with the
//! host keeping checkpoints, and from the start without them. cJTAG with JTAG over it. The
//! forward pass keeps no items, as a view's would where nothing is shown yet; each seek keeps the
//! window's.
//!
//! `cargo run --release -p escapement-tickwave-decoders --example cjtag_seek -- REC [AT_S]
//! [WINDOW_US] [EVERY_MS] [BUDGET_MIB] [WITHOUT] [EVENTS]`; WITHOUT 0 skips the run from the
//! start, and EVENTS spaces checkpoints by that many observations too.

#[path = "check/mod.rs"]
mod check;

use escapement_tickwave_decode::{
    Host, InputSpec, Keeping, Seek, Source, Span, Tick, name, wire::WireSource,
};
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
        .ok_or("usage: cjtag_seek REC [AT_S] [WINDOW_US] [EVERY_MS] [BUDGET_MIB]")?;
    let (at_s, window_us, every_ms, budget_mib) =
        (arg(2, 25.0), arg(3, 1000.0), arg(4, 20.0), arg(5, 64.0));
    let file = std::fs::File::open(&path)?;
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(64 << 20))?);
    let tb = rec.timebase();
    let per_s = tb.num() as f64 / tb.den() as f64;
    let ticks = |s: f64| (s * per_s) as Tick;
    let capture = rec.length() as f64 / per_s;
    let window = Span {
        start: ticks(at_s),
        end: ticks(at_s + window_us * 1e-6),
    };
    let every = ticks(every_ms * 1e-3);
    let budget = (budget_mib * 1048576.0) as usize;
    let without = arg(6, 1.0) != 0.0;
    let events = arg(7, 0.0) as u64;

    let lines = vec![
        rec.channel("TCKC").ok_or("no TCKC")?,
        rec.channel("TMSC").ok_or("no TMSC")?,
    ];
    let mut h = Host::default();
    let c = h.add(
        name("cjtag"),
        cjtag::definition(),
        InputSpec::Wire {
            source: Box::new(WireSource::new(rec.clone(), lines.clone())?),
            local: None,
        },
        cjtag::Cjtag::factory(1_000_000),
        &[cjtag::assume_qualified(true)],
        vec![],
    )?;
    let j = h.add(
        name("jtag"),
        cjtag::jtag_definition(),
        InputSpec::Upstream(cjtag::jtag_subscription(c)),
        cjtag::JtagOnCjtag::factory(),
        &[],
        vec![],
    )?;
    let keeping = Keeping::new(every, budget);
    h.keep_checkpoints(
        c,
        if events > 0 {
            keeping.with_events(events)
        } else {
            keeping
        },
    )?;

    let start = Instant::now();
    h.run_until(|_| false)?;
    let forward = start.elapsed().as_secs_f64();
    let kept = h.kept(c).ok_or("no keeping")?;
    println!(
        "forward pass: {forward:.2} s for {capture:.2} s of capture ({:.2}x real time), {} observations; peak RSS {} MiB",
        capture / forward,
        h.observed(c),
        check::peak_rss_kb() / 1024
    );
    println!(
        "kept every {every_ms} ms of input or {events} observations within {budget_mib} MiB: {} checkpoints, {:.2} MiB, {:.0} KiB each, widest gaps {:.2} ms and {} observations, first {:.3} s, latest {:.3} s",
        kept.count,
        kept.bytes as f64 / 1048576.0,
        kept.bytes as f64 / kept.count.max(1) as f64 / 1024.0,
        kept.widest as f64 / per_s * 1e3,
        kept.widest_observed,
        kept.first.unwrap_or(0) as f64 / per_s,
        kept.latest.unwrap_or(0) as f64 / per_s,
    );
    h.set_keep(c, vec![window]);
    h.set_keep(j, vec![window]);
    let mut first = None;
    for keep in [true, false] {
        if !keep && !without {
            break;
        }
        if keep {
            let mut times = vec![];
            let mut settles = vec![];
            let mut at = 0.5;
            while at + window_us * 1e-6 < capture {
                let w = Span {
                    start: ticks(at),
                    end: ticks(at + window_us * 1e-6),
                };
                h.set_keep(c, vec![w]);
                h.set_keep(j, vec![w]);
                let asked = Instant::now();
                let from = match h.seek(c, w.start)? {
                    Seek::Restored(t) => t,
                    _ => 0,
                };
                let restore = asked.elapsed().as_secs_f64() * 1e3;
                h.run_until(|h| h.consumed(c) >= w.start)?;
                let replay = asked.elapsed().as_secs_f64() * 1e3;
                h.run_until(|h| h.consumed(c) >= w.end && h.quiet_below(c))?;
                let drawn = asked.elapsed().as_secs_f64() * 1e3;
                h.run_until(|h| h.settled(j) >= w.end)?;
                let settled = asked.elapsed().as_secs_f64() * 1e3;
                let mut cold = WireSource::new(rec.clone(), lines.clone())?;
                let read = Instant::now();
                let stop = std::sync::atomic::AtomicBool::new(false);
                let n = cold
                    .window(w.start, w.start + ticks(1e-3), usize::MAX, &stop)?
                    .map_or(0, |w| w.events.len());
                let read = read.elapsed().as_secs_f64() * 1e3;
                println!(
                    "  window {at:5.1} s: restored {:.2} ms of input before it in {restore:.2} ms; replayed to its start by {replay:.1} ms; drawn by {drawn:.1} ms; settled by {settled:.1} ms; a new source reads 1 ms there ({n} events) in {read:.2} ms",
                    (w.start - from) as f64 / per_s * 1e3
                );
                times.push((drawn, at));
                settles.push((settled, at));
                at += 0.5;
            }
            for (what, times) in [("drawn", &mut times), ("settled", &mut settles)] {
                times.sort_by(|a, b| a.0.total_cmp(&b.0));
                let q = |f: f64| times[((times.len() - 1) as f64 * f) as usize];
                let worst = times[times.len() - 1];
                println!(
                    "with checkpoints, a window every 0.5 s ({}), {what}: median {:.1} ms, 90th percentile {:.1} ms, worst {:.1} ms at {:.1} s",
                    times.len(),
                    q(0.5).0,
                    q(0.9).0,
                    worst.0,
                    worst.1
                );
            }
            h.set_keep(c, vec![window]);
            h.set_keep(j, vec![window]);
        } else {
            h.keep_checkpoints(c, Keeping::new(every, 0))?;
        }
        let asked = Instant::now();
        let seek = h.seek(c, window.start)?;
        let restored = asked.elapsed().as_secs_f64();
        h.run_until(|h| h.settled(j) >= window.end)?;
        let landed = asked.elapsed().as_secs_f64();
        let ids = |a| -> Vec<_> {
            (h.store(a).items.iter())
                .map(|(id, i)| (*id, i.end))
                .collect()
        };
        let got = (ids(c), ids(j));
        let from = match seek {
            Seek::Restored(t) => format!("restored at {:.4} s", t as f64 / per_s),
            _ => "from the start".to_string(),
        };
        let same = match &first {
            None => "",
            Some(f) if *f == got => ", the same items as with checkpoints",
            Some(_) => ", DIFFERENT items from with checkpoints",
        };
        println!(
            "{}: {from}; restore {:.2} ms, window settled after {:.1} ms; items kept: cJTAG {}, JTAG {}{same}",
            if keep { "with checkpoints" } else { "without" },
            restored * 1e3,
            landed * 1e3,
            got.0.len(),
            got.1.len(),
        );
        first.get_or_insert(got);
    }
    Ok(())
}
