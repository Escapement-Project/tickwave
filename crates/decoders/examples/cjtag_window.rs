//! How soon a window shows its decode while the forward pass is still running: cJTAG with JTAG
//! over it, the forward pass keeping checkpoints and no items, stopped at a point, then windows
//! behind it re-derived beside it in chains of their own, each timed to drawn and settled and
//! retired. The forward pass then runs to the end, and its counts are printed to compare with a
//! run that derived nothing.
//!
//! With RUNNING set, the forward pass is not stopped: after every second of input it takes, a
//! window three seconds behind it is asked for and derived while the forward pass carries on,
//! sharing turns as `HostOptions::window_turns` says.
//!
//! `cargo run --release -p escapement-tickwave-decoders --example cjtag_window -- REC [PAUSE_S]
//! [EVENTS] [RUNNING] [WINDOW_TURNS]`; RUNNING 2 instead asks for a window at 25 s while the
//! forward pass is at 5 s, ahead of it.

#[path = "check/mod.rs"]
mod check;

use escapement_tickwave_decode::{
    AnalysisId, Host, HostOptions, InputSpec, Keeping, RunView, Span, Tick, WindowId, name,
    wire::WireSource,
};
use escapement_tickwave_decoders::cjtag;
use escapement_tickwave_view::{Cache, Recording};
use std::sync::Arc;
use std::time::Instant;

/// Analysis `a`'s run in window chain `w`, which is live.
fn win(h: &Host, w: WindowId, a: AnalysisId) -> RunView<'_> {
    h.window_run(w, a).expect("the chain is live")
}

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
    let running = arg(4, 0.0) == 1.0;
    let ahead = arg(4, 0.0) == 2.0;
    let mut options = HostOptions::default();
    options.window_turns = arg(5, 4.0) as usize;
    let file = std::fs::File::open(&path)?;
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(64 << 20))?);
    let tb = rec.timebase();
    let per_s = tb.num() as f64 / tb.den() as f64;
    let ticks = |s: f64| (s * per_s) as Tick;
    let lines = vec![
        rec.channel("TCKC").ok_or("no TCKC")?,
        rec.channel("TMSC").ok_or("no TMSC")?,
    ];
    let mut h = Host::new(options);
    let c = h.add(
        name("cjtag"),
        cjtag::definition(),
        InputSpec::Wire {
            source: Box::new(WireSource::new(rec.clone(), lines)?),
            local: None,
        },
        cjtag::Cjtag::factory(1_000_000),
        &[cjtag::assume_qualified(true)],
        vec![],
    )?;
    let j = h.add(
        name("jtag"),
        cjtag::jtag_definition(),
        InputSpec::Upstream(c),
        cjtag::JtagOnCjtag::factory(),
        &[],
        vec![],
    )?;
    h.keep_checkpoints(c, Keeping::new(ticks(1.0), 64 << 20).with_events(events))?;
    let pause = ticks(pause_s);
    let start = Instant::now();
    if ahead {
        // A window at 25 s asked for while the forward pass is at 5 s.
        h.run_until(|h| h.consumed(c) >= ticks(5.0))?;
        let w = Span {
            start: ticks(25.0),
            end: ticks(25.0 + 1e-3),
        };
        let asked = Instant::now();
        let d = h.derive(c, w)?;
        let id = d.window;
        h.run_until(|h| win(h, id, c).consumed() >= w.end && win(h, id, c).quiet_below())?;
        let drawn = asked.elapsed().as_secs_f64() * 1e3;
        h.run_until(|h| win(h, id, j).settled() >= w.end)?;
        let settled = asked.elapsed().as_secs_f64() * 1e3;
        let unknown = |a| {
            let items = &win(&h, id, a).store().items();
            let n = (items.values())
                .filter(|i| {
                    i.status
                        .is_some_and(|s| s != escapement_tickwave_decode::Status::Complete)
                        || (i.fields.iter()).any(|f| {
                            matches!(f.value, escapement_tickwave_decode::Value::Unknown { .. })
                        })
                })
                .count();
            (items.len(), n)
        };
        let (cn, cu) = unknown(c);
        let (jn, ju) = unknown(j);
        println!(
            "window at 25 s, the forward pass at {:.3} s: provisional {}; drawn by {drawn:.1} ms, settled by {settled:.1} ms; cJTAG {cn} items, {cu} incomplete or with an unknown field; JTAG {jn} items, {ju}",
            h.consumed(c) as f64 / per_s,
            d.provisional,
        );
        return Ok(());
    }
    if running {
        let capture = rec.length() as f64 / per_s;
        let mut times = vec![];
        let mut next = ticks(4.0);
        while h.finished(c).is_none() {
            h.run_until(|h| h.consumed(c) >= next || h.finished(c).is_some())?;
            if h.finished(c).is_some() {
                break;
            }
            let at = h.consumed(c) - ticks(3.0);
            let w = Span {
                start: at,
                end: at + ticks(1e-3),
            };
            let from = h.consumed(c);
            let asked = Instant::now();
            let d = h.derive(c, w)?;
            let id = d.window;
            h.run_until(|h| win(h, id, c).consumed() >= w.end && win(h, id, c).quiet_below())?;
            let drawn = asked.elapsed().as_secs_f64() * 1e3;
            h.run_until(|h| win(h, id, j).settled() >= w.end)?;
            let settled = asked.elapsed().as_secs_f64() * 1e3;
            println!(
                "  window {:6.3} s: drawn by {drawn:.1} ms, settled by {settled:.1} ms; the forward pass moved {:.2} ms of input meanwhile",
                at as f64 / per_s,
                (h.consumed(c) - from) as f64 / per_s * 1e3,
            );
            times.push((drawn, settled));
            h.retire(id);
            next = h.consumed(c) + ticks(1.0);
        }
        h.run_until(|_| false)?;
        let total = start.elapsed().as_secs_f64();
        let q = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            (v[v.len() / 2], v[v.len() * 9 / 10], v[v.len() - 1])
        };
        let mut drawn: Vec<f64> = times.iter().map(|t| t.0).collect();
        let mut settled: Vec<f64> = times.iter().map(|t| t.1).collect();
        println!(
            "{} windows: drawn median {:.1} / 90th {:.1} / worst {:.1} ms; settled {:.1} / {:.1} / {:.1} ms",
            times.len(),
            q(&mut drawn).0,
            q(&mut drawn).1,
            q(&mut drawn).2,
            q(&mut settled).0,
            q(&mut settled).1,
            q(&mut settled).2,
        );
        println!(
            "forward pass with the windows: {total:.2} s for {capture:.2} s of capture ({:.2}x real time), window_turns {}",
            capture / total,
            options.window_turns
        );
    }
    h.run_until(|h| h.consumed(c) >= pause || h.finished(c).is_some())?;
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
        let id = d.window;
        h.run_until(|h| win(h, id, c).consumed() >= w.end && win(h, id, c).quiet_below())?;
        let drawn = asked.elapsed().as_secs_f64() * 1e3;
        h.run_until(|h| win(h, id, j).settled() >= w.end)?;
        let settled = asked.elapsed().as_secs_f64() * 1e3;
        let items = (
            win(&h, id, c).store().items().len(),
            win(&h, id, j).store().items().len(),
        );
        println!(
            "  window {at_s:5.1} s: restored {:.2} ms of input before it; drawn by {drawn:.1} ms, settled by {settled:.1} ms; items cJTAG {}, JTAG {}; the forward pass at {:.3} s",
            d.restored
                .map_or(f64::NAN, |t| (w.start - t) as f64 / per_s * 1e3),
            items.0,
            items.1,
            h.consumed(c) as f64 / per_s,
        );
        h.retire(id);
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
