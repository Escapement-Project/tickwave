//! A decode view's forward pass on a worker, run as a view would run it: keeping no items, taking
//! checkpoints, and deriving a window behind the pass every half second, as a zoom asks for one,
//! while a UI draws at FPS and holds the host for DRAW_MS of each frame. Prints the UI's wait for
//! the host and its frame work, the time from asking for a window to its being drawn, the pass's
//! time against the pass alone, and how long stopping the worker blocks the thread that stops it.
//!
//! `cargo run --release -p escapement-tickwave-decoders --example view_times -- REC STACK
//! [WINDOW] [FPS] [DRAW_MS]`; STACK as `step_times` takes it. Run under `taskset -c 0` for one
//! core. With `VIEW_TIMES_TRACE` set, the worker prints its progress to stderr each second.

use escapement_tickwave_decode::{
    AnalysisId, Host, InputSpec, Keeping, Span, Tick, WindowId, name, wire::WireSource,
};
use escapement_tickwave_decoders::{adiv5, cjtag, jtag, swd};
use escapement_tickwave_view::{Cache, Recording};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// The stack, keeping nothing, and its wire analysis.
fn stack(rec: &Arc<Recording>, kind: &str, window: Tick) -> Result<(Host, AnalysisId), Error> {
    let channel = |n: &str| rec.channel(n).ok_or_else(|| format!("no {n}"));
    let wire = |channels: Vec<Option<usize>>| -> Result<InputSpec, Error> {
        Ok(InputSpec::Wire {
            source: Box::new(WireSource::with_ports(rec.clone(), channels)?),
            local: None,
        })
    };
    let mut h = Host::default();
    let root = match kind {
        "swd" => {
            let ports = vec![Some(channel("SWCLK")?), Some(channel("SWDIO")?)];
            let s = h.add(
                name("swd"),
                swd::definition(),
                wire(ports)?,
                swd::Swd::factory(window),
                &[],
                vec![],
            )?;
            let a = h.add(
                name("adiv5"),
                adiv5::adi_definition(),
                InputSpec::Upstream(s),
                adiv5::Adi::factory(),
                &[],
                vec![],
            )?;
            h.add(
                name("mem-ap"),
                adiv5::mem_definition(),
                InputSpec::Upstream(a),
                adiv5::Mem::factory(),
                &[],
                vec![],
            )?;
            s
        }
        "jtag" => {
            let mut ports: Vec<_> = ["TCK", "TMS", "TDI"]
                .iter()
                .map(|n| channel(n).map(Some))
                .collect::<Result<_, _>>()?;
            ports.push(rec.channel("TDO"));
            h.add(
                name("jtag"),
                jtag::definition(),
                wire(ports)?,
                jtag::Jtag::factory(window),
                &[],
                vec![],
            )?
        }
        "cjtag" => {
            let ports = vec![Some(channel("TCKC")?), Some(channel("TMSC")?)];
            let c = h.add(
                name("cjtag"),
                cjtag::definition(),
                wire(ports)?,
                cjtag::Cjtag::factory(window),
                &[cjtag::assume_qualified(true)],
                vec![],
            )?;
            h.add(
                name("jtag"),
                jtag::definition(),
                InputSpec::Upstream(c),
                jtag::Jtag::factory(window),
                &[],
                vec![],
            )?;
            c
        }
        _ => return Err(format!("no stack {kind}").into()),
    };
    Ok((h, root))
}

fn quantiles(v: &mut [Duration]) -> String {
    if v.is_empty() {
        return "none".into();
    }
    v.sort_unstable();
    let at = |q: f64| v[((v.len() - 1) as f64 * q) as usize].as_secs_f64() * 1e3;
    format!(
        "n {}  p50 {:.3}  p90 {:.3}  p99 {:.3}  max {:.3} ms",
        v.len(),
        at(0.5),
        at(0.9),
        at(0.99),
        at(1.0)
    )
}

fn busy(d: Duration) {
    let t = Instant::now();
    while t.elapsed() < d {
        std::hint::spin_loop();
    }
}

/// What the UI asks of the worker, and what the worker reports back.
#[derive(Default)]
struct Asks {
    /// A window to derive, and when it was asked for.
    window: Option<(Span, Instant)>,
    /// The time from each ask to its window being drawn.
    drawn: Vec<Duration>,
}

/// Steps `h` until `stop` or the end, deriving what `asks` asks for and timing it to drawn.
fn work(
    host: &Mutex<Host>,
    root: AnalysisId,
    wants: &AtomicBool,
    stop: &AtomicBool,
    asks: &Mutex<Asks>,
) -> Result<(), Error> {
    let mut live: Option<(WindowId, Span, Instant)> = None;
    let mut shown: Option<WindowId> = None;
    let (began, mut told, mut steps) = (Instant::now(), Instant::now(), 0u64);
    loop {
        while wants.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut h = host.lock().map_err(|_| "poisoned")?;
        if let Some((w, asked)) = asks.lock().map_err(|_| "poisoned")?.window.take() {
            let d = h.derive(root, w)?;
            if let Some((old, ..)) = live.replace((d.window, w, asked)) {
                h.retire(old);
            }
        }
        let ran = h.step()?;
        steps += 1;
        if std::env::var_os("VIEW_TIMES_TRACE").is_some()
            && told.elapsed() >= Duration::from_secs(1)
        {
            told = Instant::now();
            let chain = |w: Option<WindowId>| {
                w.and_then(|w| h.window_run(w, root))
                    .map(|r| format!("consumed {} quiet {}", r.consumed(), r.quiet_below()))
            };
            eprintln!(
                "{:.1} s: {steps} steps, ran {ran}; forward consumed {} finished {:?}; live {:?} {:?}; shown {:?}",
                began.elapsed().as_secs_f64(),
                h.consumed(root),
                h.finished(root),
                live.map(|l| l.1),
                chain(live.map(|l| l.0)),
                chain(shown),
            );
        }
        if let Some((id, w, asked)) = live {
            let drawn = h
                .window_run(id, root)
                .is_some_and(|r| r.consumed() >= w.end && r.quiet_below());
            if drawn {
                asks.lock()
                    .map_err(|_| "poisoned")?
                    .drawn
                    .push(asked.elapsed());
                if let Some(old) = shown.replace(id) {
                    h.retire(old);
                }
                live = None;
            }
        }
        if !ran {
            return Ok(());
        }
    }
}

fn main() -> Result<(), Error> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).ok_or("give a recording")?;
    let kind = args.get(2).map_or("swd", String::as_str);
    let arg = |k: usize, d: f64| args.get(k).and_then(|a| a.parse().ok()).unwrap_or(d);
    let window = arg(3, 100_000.0) as Tick;
    let fps = arg(4, 60.0);
    let draw = Duration::from_secs_f64(arg(5, 1.0) / 1e3);
    let frame = Duration::from_secs_f64(1.0 / fps);
    let file = std::fs::File::open(path)?;
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(64 << 20))?);
    let tb = rec.timebase();
    let per_s = tb.num() as f64 / tb.den() as f64;
    let ticks = |s: f64| (s * per_s) as Tick;
    println!("{path}: {kind}, window {window} ticks, {fps} frames a second, draw {draw:?}");

    // The pass alone, keeping nothing, as the baseline.
    let (mut h, _) = stack(&rec, kind, window)?;
    let all = Instant::now();
    h.run_until(|_| false)?;
    let alone = all.elapsed();
    drop(h);
    println!("pass alone {:.3} s", alone.as_secs_f64());

    let (mut h, root) = stack(&rec, kind, window)?;
    h.keep_checkpoints(
        root,
        Keeping::new(ticks(1.0), 64 << 20).with_events(100_000),
    )?;
    let host = Arc::new(Mutex::new(h));
    let (wants, stop) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let asks = Arc::new(Mutex::new(Asks::default()));
    let worker = {
        let (host, wants, stop, asks) = (host.clone(), wants.clone(), stop.clone(), asks.clone());
        std::thread::spawn(move || work(&host, root, &wants, &stop, &asks))
    };
    let (mut waits, mut frames) = (vec![], vec![]);
    let (all, mut next, mut asked) = (Instant::now(), Instant::now(), Instant::now());
    while !worker.is_finished() {
        let f = Instant::now();
        wants.store(true, Ordering::Release);
        let held = host.lock().map_err(|_| "poisoned")?;
        waits.push(f.elapsed());
        wants.store(false, Ordering::Release);
        let consumed = held.consumed(root);
        busy(draw);
        drop(held);
        // Every half second, a millisecond's window halfway behind the pass, as a zoom asks.
        if asked.elapsed() >= Duration::from_millis(500) && consumed > ticks(0.01) {
            let at = consumed / 2;
            let w = Span {
                start: at,
                end: at + ticks(1e-3),
            };
            asks.lock().map_err(|_| "poisoned")?.window = Some((w, Instant::now()));
            asked = Instant::now();
        }
        frames.push(f.elapsed());
        next += frame;
        match next.checked_duration_since(Instant::now()) {
            Some(w) => std::thread::sleep(w),
            None => next = Instant::now(),
        }
    }
    worker.join().map_err(|_| "the worker panicked")??;
    let pass = all.elapsed();
    let mut drawn = std::mem::take(&mut asks.lock().map_err(|_| "poisoned")?.drawn);
    println!("the UI's wait {}", quantiles(&mut waits));
    println!("frame work {}", quantiles(&mut frames));
    println!("asked to drawn {}", quantiles(&mut drawn));
    println!(
        "pass {:.3} s ({:.2}x alone)",
        pass.as_secs_f64(),
        pass.as_secs_f64() / alone.as_secs_f64()
    );

    // Stopping: a worker stopped at a point in the pass, timed from the stop to its join.
    let mut stops = vec![];
    for k in 0..10u64 {
        let (h, root) = stack(&rec, kind, window)?;
        let host = Arc::new(Mutex::new(h));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let (host, stop) = (host.clone(), stop.clone());
            let (wants, asks) = (AtomicBool::new(false), Mutex::new(Asks::default()));
            std::thread::spawn(move || work(&host, root, &wants, &stop, &asks))
        };
        std::thread::sleep(Duration::from_millis(100 + 37 * k));
        let t = Instant::now();
        stop.store(true, Ordering::Release);
        worker.join().map_err(|_| "the worker panicked")??;
        stops.push(t.elapsed());
    }
    println!("stop to joined {}", quantiles(&mut stops));
    Ok(())
}
