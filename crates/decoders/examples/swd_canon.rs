//! SWD, ADIv5 and MEM-AP over a recording's SWCLK and SWDIO, every item kept, printed as counts
//! and a digest of each item rendered by name: a form that does not depend on how operations are
//! encoded, so another implementation's output can be compared with it.
//!
//! `cargo run --release -p escapement-tickwave-decoders --example swd_canon -- REC`

#[path = "canon/mod.rs"]
mod canon;

use escapement_tickwave_decode::{Host, InputSpec, Span, Status, Tick, name, wire::WireSource};
use escapement_tickwave_decoders::{adiv5, swd};
use escapement_tickwave_view::{Cache, Recording};
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: swd_canon REC")?;
    let file = std::fs::File::open(&path)?;
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(64 << 20))?);
    let clock = rec.channel("SWCLK").ok_or("no SWCLK")?;
    let data = rec.channel("SWDIO").ok_or("no SWDIO")?;
    let source = WireSource::new(rec.clone(), vec![clock, data])?;
    let all = vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }];
    let mut h = Host::default();
    let s = h.add(
        name("swd"),
        swd::definition(),
        InputSpec::Wire {
            source: Box::new(source),
            local: None,
        },
        swd::Swd::factory(100_000),
        &[],
        all.clone(),
    )?;
    let a = h.add(
        name("adi"),
        adiv5::adi_definition(),
        InputSpec::Upstream(adiv5::adi_subscription(s)),
        adiv5::Adi::factory(),
        &[],
        all.clone(),
    )?;
    let m = h.add(
        name("mem"),
        adiv5::mem_definition(),
        InputSpec::Upstream(adiv5::mem_subscription(a)),
        adiv5::Mem::factory(),
        &[],
        all,
    )?;
    let start = std::time::Instant::now();
    h.run_until(|_| false)?;
    let seconds = start.elapsed().as_secs_f64();
    canon::print(&h, &[(s, None), (a, Some(s)), (m, Some(a))]);
    let dropped = (h.store(s).items.values())
        .filter(|i| i.status == Some(Status::Incomplete(swd::DROPPED)))
        .count();
    println!("transfers whose rises SWD no longer held: {dropped}");
    if dropped > 0 {
        return Err("SWD dropped transfers".into());
    }
    println!("seconds {seconds:.2}");
    println!("largest batch: {} operations", h.stats().peak_batch);
    Ok(())
}
