//! JTAG over a recording's TCK, TMS, TDI and TDO, checked against what IEEE 1149.1 guarantees
//! where TDO is recorded: every IR scan captures a value whose two lowest bits are 01, and a DR
//! scan after Test-Logic-Reset, before any IR scan, shifts out IDCODE (bit 0 high) or BYPASS (one
//! low bit). A checker stacked on JTAG reads its completed items; nothing is kept, so memory
//! follows the decoders' state, not the capture.
//!
//! `cargo run --release -p escapement-tickwave-decoders --example jtag_check -- REC [--tdo-at-fall]`

use escapement_tickwave_decode::{
    Decoder, Definition, Delivery, Exchange, Host, InputSpec, Mode, Pause, Schema, Status,
    Subscription, Value, name, wire::WireSource,
};
use escapement_tickwave_decoders::jtag;
use escapement_tickwave_view::{Cache, Recording};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Default, Debug)]
struct Seen {
    states: u64,
    paths: u64,
    ir: u64,
    ir_failed: u64,
    ir_unread: u64,
    dr: u64,
    incomplete: u64,
    lengths: BTreeMap<(bool, u64), u64>,
    idcodes: BTreeMap<u32, u64>,
    bypass: u64,
    after_reset: bool,
    /// The TDO shifted out by 32-bit DR scans, which an IDCODE instruction selects.
    dr32: BTreeMap<u32, u64>,
}

struct Checker(Arc<Mutex<Seen>>);

fn bits(v: &Value) -> Option<(&[u8], u32)> {
    match v {
        Value::Bits { bytes, bits } => Some((bytes, *bits)),
        _ => None,
    }
}

impl Decoder for Checker {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        let mut s = self.0.lock().unwrap();
        while let Some(d) = ex.receive() {
            match d {
                Delivery::Item { item, .. } => {
                    let length = match item.field(jtag::LENGTH).map(|f| &f.value) {
                        Some(Value::Word { value, .. }) => *value,
                        _ => 0,
                    };
                    let tdo = item.field(jtag::TDO_VALUE).and_then(|f| bits(&f.value));
                    if item.status != Status::Complete && item.id.kind != jtag::STATE {
                        s.incomplete += 1;
                    }
                    match item.id.kind {
                        jtag::STATE => {
                            s.states += 1;
                            let v = item.field(jtag::TAP_STATE).map(|f| &f.value);
                            if v == Some(&Value::Name(0)) {
                                s.after_reset = true;
                            }
                        }
                        jtag::PATH => {
                            s.paths += 1;
                            if item.field(jtag::TO).map(|f| &f.value) == Some(&Value::Name(0)) {
                                s.after_reset = true;
                            }
                        }
                        jtag::IR_SCAN => {
                            s.ir += 1;
                            *s.lengths.entry((true, length)).or_default() += 1;
                            s.after_reset = false;
                            match tdo {
                                Some((b, n)) if n >= 2 => {
                                    if b.first().is_none_or(|x| x & 3 != 1) {
                                        s.ir_failed += 1;
                                    }
                                }
                                _ => s.ir_unread += 1,
                            }
                        }
                        jtag::DR_SCAN => {
                            s.dr += 1;
                            *s.lengths.entry((false, length)).or_default() += 1;
                            if let (32, Some((b, 32))) = (length, tdo) {
                                let mut w = [0u8; 4];
                                w.copy_from_slice(&b[..4]);
                                *s.dr32.entry(u32::from_le_bytes(w)).or_default() += 1;
                            }
                            if std::mem::take(&mut s.after_reset)
                                && let Some((b, n)) = tdo
                            {
                                match (b.first().map(|x| x & 1), n) {
                                    (Some(1), 32..) => {
                                        let mut w = [0u8; 4];
                                        w.copy_from_slice(&b[..4]);
                                        *s.idcodes.entry(u32::from_le_bytes(w)).or_default() += 1;
                                    }
                                    (Some(0), _) => s.bypass += 1,
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Delivery::Lost { span, .. } => {
                    s.after_reset = false;
                    ex.lost(span.start, span.end);
                }
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

fn peak_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

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
    let tb = rec.timebase();
    let capture = rec.length() as f64 * tb.den() as f64 / tb.num() as f64;
    let s = seen.lock().unwrap();
    let c = h.store(j).counts;
    println!(
        "TDO {}{}; states {} paths {} ir {} dr {} incomplete {}; lost {}; ops {} elements {} peak_open {}",
        tdo.is_some(),
        if at_fall {
            " at TCK's fall"
        } else {
            " at TCK's rise"
        },
        s.states,
        s.paths,
        s.ir,
        s.dr,
        s.incomplete,
        h.lost(j).len(),
        c.ops,
        c.elements,
        c.peak_open
    );
    println!(
        "IR capture ending 01: {} of {} ({} failed, {} without TDO value)",
        s.ir - s.ir_failed - s.ir_unread,
        s.ir,
        s.ir_failed,
        s.ir_unread
    );
    let ids: Vec<String> = (s.idcodes.iter())
        .map(|(k, n)| format!("0x{k:08X} x{n}"))
        .collect();
    println!(
        "after reset: IDCODE {} ; BYPASS {}",
        if ids.is_empty() {
            "none".into()
        } else {
            ids.join(", ")
        },
        s.bypass
    );
    let mut dr32: Vec<_> = s.dr32.iter().collect();
    dr32.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    let top: Vec<String> = (dr32.iter().take(3))
        .map(|(v, n)| format!("0x{v:08X} x{n}"))
        .collect();
    println!("32-bit DR scans' TDO, most often: {}", top.join(", "));
    let mut lengths: Vec<_> = s.lengths.iter().collect();
    lengths.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    let top: Vec<String> = (lengths.iter().take(8))
        .map(|((ir, l), n)| format!("{}{l} x{n}", if *ir { "IR " } else { "DR " }))
        .collect();
    println!("scan lengths: {}", top.join(", "));
    println!(
        "decoded in {seconds:.2} s for {capture:.2} s of capture ({:.2}x real time); peak RSS {} MiB",
        capture / seconds,
        peak_rss_kb() / 1024
    );
    Ok(())
}
