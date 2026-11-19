//! cJTAG over a recording's TCKC and TMSC with JTAG stacked on it, JTAG checked as `jtag_check`
//! checks it, and cJTAG's counts printed to compare with another implementation's, with each Scan
//! Packet's TDO driver as the drive policy gives it, nothing assumed. Nothing is
//! kept, unless `--rows` asks to check that both analyses' items fit the rows and lanes their
//! schemas declare, which keeps each item's span.
//!
//! `cargo run --release -p escapement-tickwave-decoders --example cjtag_check -- REC [--rows]`

#[path = "check/mod.rs"]
mod check;

use check::Seen;
use escapement_tickwave_decode::{
    Decoder, Definition, Delivery, Exchange, Host, InputSpec, KindId, Mode, Pause, Schema, Status,
    Subscription, Tick, Value, name, wire::WireSource,
};
use escapement_tickwave_decoders::cjtag;
use escapement_tickwave_view::{Cache, Recording};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Items by kind, and those closed incomplete by cause, each by its schema index; with
/// `--rows`, each item's kind and span; every JScan stretch's span; the CR scans by register,
/// value and meaning; and the Scan Packets' TDO drivers.
type Tally = (
    BTreeMap<u16, u64>,
    BTreeMap<u16, u64>,
    Option<Vec<(KindId, Tick, Tick)>>,
    Vec<(Tick, Tick)>,
    BTreeMap<String, u64>,
    BTreeMap<String, u64>,
);

/// A Scan Packet's TDO driver, where it has a TDO element: the target, no one, or why it is not
/// known.
fn tdo_driver(item: &escapement_tickwave_decode::Completed, schema: &Schema) -> Option<String> {
    let tdo = item.field(cjtag::PACKET_TDO)?;
    if tdo.driver == Some(cjtag::TARGET) {
        return Some("target".into());
    }
    Some(match item.field(cjtag::DRIVER).map(|f| &f.value) {
        Some(Value::Name(2)) => "no one".into(),
        Some(Value::Unknown { reason, .. }) => {
            let why = schema.reasons.get(usize::from(reason.0));
            format!("unknown ({})", why.map_or("?", |r| r))
        }
        _ => "?".into(),
    })
}

/// A CR scan as the tally counts it: its register, its value in hex, and what it means.
fn cr_line(item: &escapement_tickwave_decode::Completed) -> String {
    let field = |f| item.field(f).map(|f| &f.value);
    let register = match field(cjtag::REGISTER) {
        Some(Value::Name(n)) => cjtag::REGISTERS
            .get(usize::from(*n))
            .copied()
            .unwrap_or("?"),
        _ => "?",
    };
    let value = match field(cjtag::CR_VALUE) {
        Some(Value::Bits { bytes, .. }) => {
            let hex: String = bytes.iter().rev().map(|b| format!("{b:02X}")).collect();
            format!("0x{hex}")
        }
        Some(Value::Unknown { .. }) => "unknown".into(),
        _ => "none".into(),
    };
    let meaning = match field(cjtag::CR_MEANING) {
        Some(Value::Text(t)) => format!(" ({t})"),
        _ => String::new(),
    };
    format!("{register} {value}{meaning}")
}

/// Counts cJTAG's completed items by kind, and those closed incomplete by cause.
struct Kinds(Arc<Mutex<Tally>>, Schema);

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
                    if let Some(spans) = &mut k.2 {
                        spans.push((item.id.kind, item.start, item.end));
                    }
                    if item.id.kind == cjtag::JSCAN {
                        k.3.push((item.start, item.end));
                    }
                    if item.id.kind == cjtag::CR_SCAN {
                        *k.4.entry(cr_line(&item)).or_default() += 1;
                    }
                    if item.id.kind == cjtag::PACKET
                        && let Some(d) = tdo_driver(&item, &self.1)
                    {
                        *k.5.entry(d).or_default() += 1;
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
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: cjtag_check REC [--rows]")?;
    let rows = std::env::args().any(|a| a == "--rows");
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
        cjtag::Cjtag::factory(1_000_000),
        &[cjtag::assume_qualified(false)],
        vec![],
    )?;
    let (chain, dp_tap) = check::chain_args();
    let jtag_chain = (
        "chain",
        escapement_tickwave_decode::Setting::List(chain.iter().map(|&l| i64::from(l)).collect()),
    );
    let j = h.add(
        name("jtag"),
        cjtag::jtag_definition(),
        InputSpec::Upstream(cjtag::jtag_subscription(c)),
        cjtag::JtagOnCjtag::factory(),
        &[jtag_chain],
        vec![],
    )?;
    let kinds = Arc::new(Mutex::new((
        BTreeMap::new(),
        BTreeMap::new(),
        rows.then(Vec::new),
        Vec::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    )));
    let k2 = kinds.clone();
    let schema = h.definition(c).schema.clone();
    let s2 = schema.clone();
    let all: Vec<String> = schema.kinds.iter().map(|k| k.name.to_string()).collect();
    h.add(
        name("kinds"),
        Arc::new(Definition::new("kinds", 1, Schema::new(vec![]))),
        InputSpec::Upstream(Subscription::new(c, Mode::Completed).with_kinds(all)),
        Box::new(move |_, _| Box::new(Kinds(k2.clone(), s2.clone()))),
        &[],
        vec![],
    )?;
    let seen = Arc::new(Mutex::new(Seen {
        spans: rows.then(Vec::new),
        ..Seen::default()
    }));
    check::stack(&mut h, j, &seen)?;
    let tallies = match dp_tap {
        Some(tap) => check::stack_dp(&mut h, j, tap)?,
        None => vec![],
    };
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
    if let Some(spans) = &k.2 {
        println!("cjtag {}", check::rows_line(&schema, spans));
    }
    let crs: Vec<String> = (k.4.iter()).map(|(l, n)| format!("{l} x{n}")).collect();
    println!(
        "cjtag CR scans: {}",
        if crs.is_empty() {
            "none".to_string()
        } else {
            crs.join(", ")
        }
    );
    let drivers: Vec<String> = (k.5.iter()).map(|(d, n)| format!("{d} x{n}")).collect();
    println!("cjtag TDO drivers: {}", drivers.join(", "));
    check::report(&h, j, &seen, &rec, None, "JTAG on cJTAG", seconds);
    for (what, t) in &tallies {
        println!("{what}: {}", check::tally_lines(t, 20).join(", "));
    }
    // Each IR scan with no TDO value, against the JScan stretches: where a session runs in the
    // Standard Protocol, before a command activates OScan, TDO is not on the wire.
    let s = seen.lock().unwrap();
    let inside = |&(a, b): &(Tick, Tick)| k.3.iter().any(|&(c, d)| c <= a && b <= d);
    let n = s.unread.iter().filter(|x| inside(x)).count();
    println!(
        "IR scans with no TDO value inside a JScan stretch: {n} of {}",
        s.unread.len()
    );
    Ok(())
}
