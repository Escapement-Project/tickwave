//! How long one host step takes, which bounds how long a decode view's UI can wait for the host,
//! and the two ways a view could run the forward pass, each against a UI that draws at 60 Hz and
//! holds the host for DRAW_MS of each frame: stepped on the UI thread for SLICE_MS of each frame,
//! or on a worker that the UI locks for its draw.
//!
//! `cargo run --release -p escapement-tickwave-decoders --example step_times -- REC STACK
//! [WINDOW] [KEEP] [SLICE_MS] [DRAW_MS]`
//!
//! STACK is `swd` (SWD, ADIv5 and MEM-AP), `jtag` or `cjtag` (cJTAG with JTAG on it). WINDOW is
//! the wire decoder's window in ticks. KEEP 1 keeps every item, as a view showing all of a
//! recording would; 0 keeps none, as a forward pass with nothing on screen.

use escapement_tickwave_decode::{Host, InputSpec, Span, Tick, name, wire::WireSource};
use escapement_tickwave_decoders::{adiv5, cjtag, jtag, swd};
use escapement_tickwave_view::{Cache, Recording};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Error = Box<dyn std::error::Error + Send + Sync>;

fn stack(rec: &Arc<Recording>, kind: &str, window: Tick, keep: bool) -> Result<Host, Error> {
    let keep = || {
        if keep {
            vec![Span {
                start: Tick::MIN,
                end: Tick::MAX,
            }]
        } else {
            vec![]
        }
    };
    let channel = |n: &str| rec.channel(n).ok_or_else(|| format!("no {n}"));
    let wire = |channels: Vec<Option<usize>>| -> Result<InputSpec, Error> {
        Ok(InputSpec::Wire {
            source: Box::new(WireSource::with_ports(rec.clone(), channels)?),
            local: None,
        })
    };
    let mut h = Host::default();
    match kind {
        "swd" => {
            let ports = vec![Some(channel("SWCLK")?), Some(channel("SWDIO")?)];
            let s = h.add(
                name("swd"),
                swd::definition(),
                wire(ports)?,
                swd::Swd::factory(window),
                &[],
                keep(),
            )?;
            let a = h.add(
                name("adiv5"),
                adiv5::adi_definition(),
                InputSpec::Upstream(s),
                adiv5::Adi::factory(),
                &[],
                keep(),
            )?;
            h.add(
                name("mem-ap"),
                adiv5::mem_definition(),
                InputSpec::Upstream(a),
                adiv5::Mem::factory(),
                &[],
                keep(),
            )?;
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
                keep(),
            )?;
        }
        "cjtag" => {
            let ports = vec![Some(channel("TCKC")?), Some(channel("TMSC")?)];
            let c = h.add(
                name("cjtag"),
                cjtag::definition(),
                wire(ports)?,
                cjtag::Cjtag::factory(window),
                &[cjtag::assume_qualified(true)],
                keep(),
            )?;
            h.add(
                name("jtag"),
                jtag::definition(),
                InputSpec::Upstream(c),
                jtag::Jtag::factory(window),
                &[],
                keep(),
            )?;
        }
        _ => return Err(format!("no stack {kind}").into()),
    }
    Ok(h)
}

/// `v`'s quantiles, sorted in place: p50, p90, p99, p99.9 and the most, in milliseconds.
fn quantiles(v: &mut [Duration]) -> String {
    if v.is_empty() {
        return "none".into();
    }
    v.sort_unstable();
    let at = |q: f64| v[((v.len() - 1) as f64 * q) as usize].as_secs_f64() * 1e3;
    format!(
        "n {}  p50 {:.3}  p90 {:.3}  p99 {:.3}  p99.9 {:.3}  max {:.3} ms",
        v.len(),
        at(0.5),
        at(0.9),
        at(0.99),
        at(0.999),
        at(1.0)
    )
}

/// Spends `d` on the CPU, as a draw would.
fn busy(d: Duration) {
    let t = Instant::now();
    while t.elapsed() < d {
        std::hint::spin_loop();
    }
}

const FRAME: Duration = Duration::from_nanos(16_666_667);

/// Sleeps to the next frame after `next`, or starts the next one now when the frame ran over.
fn next_frame(next: &mut Instant) {
    *next += FRAME;
    match next.checked_duration_since(Instant::now()) {
        Some(w) => std::thread::sleep(w),
        None => *next = Instant::now(),
    }
}

fn main() -> Result<(), Error> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).ok_or("give a recording")?;
    let kind = args.get(2).map_or("swd", String::as_str);
    let arg = |k: usize, d: f64| args.get(k).and_then(|a| a.parse().ok()).unwrap_or(d);
    let window = arg(3, 100_000.0) as Tick;
    let keep = arg(4, 1.0) == 1.0;
    let slice = Duration::from_secs_f64(arg(5, 8.0) / 1e3);
    let draw = Duration::from_secs_f64(arg(6, 1.0) / 1e3);
    let file = std::fs::File::open(path)?;
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(64 << 20))?);
    println!("{path}: {kind}, window {window} ticks, keep {keep}, slice {slice:?}, draw {draw:?}");

    // Each step alone, back to back.
    let mut h = stack(&rec, kind, window, keep)?;
    let mut steps = vec![];
    let all = Instant::now();
    loop {
        let t = Instant::now();
        let ran = h.step()?;
        steps.push(t.elapsed());
        if !ran {
            break;
        }
    }
    let alone = all.elapsed();
    drop(h);
    let over = |v: &[Duration], ms: u64| v.iter().filter(|d| d.as_millis() >= ms.into()).count();
    println!(
        "steps: {}; whole pass {:.3} s; steps of 4 ms or more {}, of 16 ms or more {}",
        quantiles(&mut steps),
        alone.as_secs_f64(),
        over(&steps, 4),
        over(&steps, 16)
    );

    // On the UI thread: each frame steps until its slice is spent, then draws.
    let mut h = stack(&rec, kind, window, keep)?;
    let (mut work, mut next) = (vec![], Instant::now());
    let all = Instant::now();
    loop {
        let f = Instant::now();
        let mut done = false;
        while f.elapsed() < slice {
            if !h.step()? {
                done = true;
                break;
            }
        }
        busy(draw);
        work.push(f.elapsed());
        if done {
            break;
        }
        next_frame(&mut next);
    }
    let ui = all.elapsed();
    drop(h);
    println!(
        "ui thread: frame work {}; pass {:.3} s ({:.2}x alone)",
        quantiles(&mut work),
        ui.as_secs_f64(),
        ui.as_secs_f64() / alone.as_secs_f64()
    );

    // On a worker: the worker steps while the UI does not want the host; each frame the UI
    // locks the host, waits for the step under way, and draws holding it.
    let host = Arc::new(Mutex::new(stack(&rec, kind, window, keep)?));
    let wants = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let worker = {
        let (host, wants, done) = (host.clone(), wants.clone(), done.clone());
        std::thread::spawn(move || -> Result<(), Error> {
            loop {
                while wants.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                let ran = host.lock().map_err(|_| "poisoned")?.step()?;
                if !ran {
                    done.store(true, Ordering::Release);
                    return Ok(());
                }
            }
        })
    };
    let (mut waits, mut work, mut next) = (vec![], vec![], Instant::now());
    let all = Instant::now();
    while !done.load(Ordering::Acquire) {
        let f = Instant::now();
        wants.store(true, Ordering::Release);
        let held = host.lock().map_err(|_| "poisoned")?;
        waits.push(f.elapsed());
        wants.store(false, Ordering::Release);
        busy(draw);
        drop(held);
        work.push(f.elapsed());
        next_frame(&mut next);
    }
    worker.join().map_err(|_| "the worker panicked")??;
    let worked = all.elapsed();
    println!("worker: the UI's wait {}", quantiles(&mut waits));
    println!(
        "worker: frame work {}; pass {:.3} s ({:.2}x alone)",
        quantiles(&mut work),
        worked.as_secs_f64(),
        worked.as_secs_f64() / alone.as_secs_f64()
    );
    Ok(())
}
