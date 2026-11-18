//! cJTAG over a recording's TCKC and TMSC with JTAG stacked on it, JTAG checked as `jtag_check`
//! checks it, and cJTAG's counts printed to compare with another implementation's. Nothing is
//! kept.
//!
//! `cargo run --release -p escapement-tickwave-decoders --example cjtag_check -- REC`

#[path = "check/mod.rs"]
mod check;

use check::{Checker, Seen};
use escapement_tickwave_decode::{
    Decoder, Definition, Delivery, Exchange, Host, InputSpec, Mode, Pause, Schema, Status,
    Subscription, name, wire::WireSource,
};
use escapement_tickwave_decoders::cjtag;
use escapement_tickwave_view::{Cache, Recording};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Items by kind, and those closed incomplete by cause, each by its schema index.
type Tally = (BTreeMap<u16, u64>, BTreeMap<u16, u64>);

/// Counts cJTAG's completed items by kind, and those closed incomplete by cause.
struct Kinds(Arc<Mutex<Tally>>);

impl Decoder for Kinds {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        let mut k = self.0.lock().unwrap();
        while let Some(d) = ex.receive() {
            match d {
                Delivery::Item { item, .. } => {
                    *k.0.entry(item.id.kind.0).or_default() += 1;
                    if let Status::Incomplete(c) = item.status {
                        *k.1.entry(c.0).or_default() += 1;
                    }
                }
                Delivery::Lost { span, .. } => ex.lost(span.start, span.end),
                Delivery::Frontier { start, .. } => ex.frontier(start),
                Delivery::End { end, .. } => {
                    ex.finish(end);
                    return Pause::Finished;
                }
                _ => {}
            }
        }
        if ex.more() { Pause::Work } else { Pause::Input }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: cjtag_check REC")?;
    let file = std::fs::File::open(&path)?;
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(64 << 20))?);
    let lines = vec![
        rec.channel("TCKC").ok_or("no TCKC")?,
        rec.channel("TMSC").ok_or("no TMSC")?,
    ];
    let source = WireSource::new(rec.clone(), lines)?;
    let mut h = Host::default();
    let c = h.add(
        name("cjtag"),
        cjtag::definition(),
        InputSpec::Wire {
            source: Box::new(source),
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
    let kinds = Arc::new(Mutex::new((BTreeMap::new(), BTreeMap::new())));
    let k2 = kinds.clone();
    let schema = h.definition(c).schema.clone();
    let all: Vec<String> = schema.kinds.iter().map(|k| k.name.to_string()).collect();
    h.add(
        name("kinds"),
        Arc::new(Definition::new("kinds", 1, Schema::new(vec![]))),
        InputSpec::Upstream(Subscription::new(c, Mode::Completed).with_kinds(all)),
        Box::new(move |_| Box::new(Kinds(k2.clone()))),
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
    for (x, name) in [(c, "cjtag"), (j, "jtag")] {
        let k = h.store(x).counts;
        println!(
            "{name}: items {} ops {} elements {} incomplete {} peak_open {} lost {}",
            k.opens,
            k.ops,
            k.elements,
            k.incomplete,
            k.peak_open,
            h.lost(x).len()
        );
    }
    let k = kinds.lock().unwrap();
    let by_kind: Vec<String> = (k.0.iter())
        .map(|(id, n)| {
            let kind = schema.kinds.get(usize::from(*id)).map_or("?", |d| &d.name);
            format!("{kind} {n}")
        })
        .collect();
    println!("cjtag by kind: {}", by_kind.join(", "));
    let causes: Vec<String> = (k.1.iter())
        .map(|(id, n)| {
            let why = schema.causes.get(usize::from(*id)).map_or("?", |d| d);
            format!("{why}: {n}")
        })
        .collect();
    println!(
        "cjtag incomplete: {}",
        if causes.is_empty() {
            "none".to_string()
        } else {
            causes.join("; ")
        }
    );
    check::report(&h, j, &seen, &rec, "JTAG on cJTAG", seconds);
    Ok(())
}
