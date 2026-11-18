//! What `jtag_check` and `cjtag_check` share: a checker stacked on JTAG's completed items, and
//! the report.

#![allow(dead_code)]

pub mod rows;

use escapement_tickwave_decode::{
    AnalysisId, Decoder, Delivery, Exchange, Host, KindId, Pause, Schema, Status, Tick, Value,
};
use escapement_tickwave_decoders::jtag;
use escapement_tickwave_view::Recording;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Default, Debug)]
pub struct Seen {
    pub states: u64,
    pub paths: u64,
    pub ir: u64,
    pub ir_failed: u64,
    pub ir_unread: u64,
    pub dr: u64,
    pub incomplete: u64,
    pub lengths: BTreeMap<(bool, u64), u64>,
    pub idcodes: BTreeMap<u32, u64>,
    pub bypass: u64,
    pub after_reset: bool,
    /// The TDO shifted out by 32-bit DR scans, which an IDCODE instruction selects.
    pub dr32: BTreeMap<u32, u64>,
    /// Each item's kind and span, kept only where `--rows` asks for the rows check.
    pub spans: Option<Vec<(KindId, Tick, Tick)>>,
}

/// The rows check's line: for each row of `schema`, top first, the most of `spans` at once
/// against the row's lanes.
pub fn rows_line(schema: &Schema, spans: &[(KindId, Tick, Tick)]) -> String {
    let rows = rows::most_at_once(schema, spans.iter().copied());
    let each: Vec<String> = (rows.iter())
        .map(|(most, lanes)| format!("{most} of {lanes}"))
        .collect();
    let fits = rows.iter().all(|(most, lanes)| most <= lanes);
    format!(
        "rows, most at once of their lanes: {} ({})",
        each.join(", "),
        if fits { "fits" } else { "DOES NOT FIT" }
    )
}

pub struct Checker(pub Arc<Mutex<Seen>>);

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
                    if let Some(spans) = &mut s.spans {
                        spans.push((item.id.kind, item.start, item.end));
                    }
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

pub fn peak_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

/// Prints the JTAG analysis `j`'s counts and checks, and the decode's time and memory.
pub fn report(
    h: &Host,
    j: AnalysisId,
    seen: &Mutex<Seen>,
    rec: &Recording,
    label: &str,
    seconds: f64,
) {
    let tb = rec.timebase();
    let capture = rec.length() as f64 * tb.den() as f64 / tb.num() as f64;
    let s = seen.lock().unwrap();
    let c = h.store(j).counts;
    println!(
        "{label}; states {} paths {} ir {} dr {} incomplete {}; lost {}; ops {} elements {} peak_open {}",
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
    if let Some(spans) = &s.spans {
        println!("JTAG {}", rows_line(&h.definition(j).schema, spans));
    }
    println!(
        "decoded in {seconds:.2} s for {capture:.2} s of capture ({:.2}x real time); peak RSS {} MiB",
        capture / seconds,
        peak_rss_kb() / 1024
    );
    println!("largest batch: {} operations", h.stats().peak_batch);
}
