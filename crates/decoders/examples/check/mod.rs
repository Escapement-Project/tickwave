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
    /// The TDO shifted out by 32-bit DR scans; and by the instruction the IR scan before each
    /// shifted in, its TDI, since a 32-bit data register need not be an IDCODE.
    pub dr32: BTreeMap<u32, u64>,
    pub dr32_by_ir: BTreeMap<(Option<u64>, u32), u64>,
    pub last_ir: Option<u64>,
    /// The first IR scans whose capture does not end in 01; every IR scan with a TDO value, its
    /// span and whether its capture did; and those with none.
    pub failures: Vec<Failure>,
    pub ir_spans: Vec<(Tick, Tick, bool)>,
    pub unread: Vec<(Tick, Tick)>,
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

/// An IR scan whose capture does not end in 01: its place among the IR scans, its span and
/// length, and the bits it captured, first shifted first.
#[derive(Debug)]
pub struct Failure {
    pub index: u64,
    pub start: Tick,
    pub end: Tick,
    pub length: u64,
    pub captured: Vec<bool>,
}

/// The most failures `Seen` lists.
const LISTED: usize = 64;

pub struct Checker(pub Arc<Mutex<Seen>>);

/// The first `n` bits of `bytes`, first shifted first.
fn unpacked(bytes: &[u8], n: u32) -> Vec<bool> {
    (0..n as usize)
        .map(|k| bytes.get(k / 8).is_some_and(|b| b >> (k % 8) & 1 == 1))
        .collect()
}

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
                            let tdi = item.field(jtag::TDI_VALUE).and_then(|f| bits(&f.value));
                            s.last_ir = tdi.filter(|&(_, n)| n <= 64).map(|(b, n)| {
                                (unpacked(b, n).iter().enumerate())
                                    .fold(0u64, |v, (k, &x)| v | u64::from(x) << k)
                            });
                            let end = item.end;
                            match tdo {
                                Some((b, n)) if n >= 2 => {
                                    let ok = b.first().is_some_and(|x| x & 3 == 1);
                                    if !ok {
                                        s.ir_failed += 1;
                                        if s.failures.len() < LISTED {
                                            let f = Failure {
                                                index: s.ir - 1,
                                                start: item.start,
                                                end,
                                                length,
                                                captured: unpacked(b, n),
                                            };
                                            s.failures.push(f);
                                        }
                                    }
                                    s.ir_spans.push((item.start, end, ok));
                                }
                                _ => {
                                    s.ir_unread += 1;
                                    s.unread.push((item.start, end));
                                }
                            }
                        }
                        jtag::DR_SCAN => {
                            s.dr += 1;
                            *s.lengths.entry((false, length)).or_default() += 1;
                            if let (32, Some((b, 32))) = (length, tdo) {
                                let mut w = [0u8; 4];
                                w.copy_from_slice(&b[..4]);
                                let w = u32::from_le_bytes(w);
                                *s.dr32.entry(w).or_default() += 1;
                                let ir = s.last_ir;
                                *s.dr32_by_ir.entry((ir, w)).or_default() += 1;
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

/// The median spacing of channel `tck`'s rises in `start..end`, in seconds: the clock's period
/// within a burst, which a mean would stretch over the pauses between bursts. None with fewer
/// than two rises.
fn period(rec: &Recording, tck: usize, start: Tick, end: Tick) -> Option<f64> {
    let never = std::sync::atomic::AtomicBool::new(false);
    let (level, edges) = rec.edges(tck, start, end.max(start + 1), &never).ok()?;
    // Edges alternate from `level`, so every other one is a rise.
    let rises: Vec<Tick> = (edges.iter().skip(usize::from(level)).step_by(2))
        .copied()
        .collect();
    let mut gaps: Vec<Tick> = rises.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort_unstable();
    let tb = rec.timebase();
    let median = *gaps.get(gaps.len() / 2)?;
    Some(median as f64 * tb.den() as f64 / tb.num() as f64)
}

/// IR scans' TCK periods in nanoseconds, rounded, and how many had each.
fn periods(rec: &Recording, tck: usize, spans: impl Iterator<Item = (Tick, Tick)>) -> String {
    let mut by: BTreeMap<i64, u64> = BTreeMap::new();
    for (a, b) in spans {
        if let Some(p) = period(rec, tck, a, b) {
            *by.entry((p * 1e9).round() as i64).or_default() += 1;
        }
    }
    let each: Vec<String> = by.iter().map(|(ns, n)| format!("{ns} ns x{n}")).collect();
    each.join(", ")
}

/// Prints the JTAG analysis `j`'s counts and checks, and the decode's time and memory. With
/// `tck`, TCK's channel, the IR scans that fail the capture check are listed with their TCK
/// period, and every IR scan's period is tallied.
pub fn report(
    h: &Host,
    j: AnalysisId,
    seen: &Mutex<Seen>,
    rec: &Recording,
    tck: Option<usize>,
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
    let mut by_ir: BTreeMap<Option<u64>, Vec<(u32, u64)>> = BTreeMap::new();
    for (&(ir, w), &n) in &s.dr32_by_ir {
        by_ir.entry(ir).or_default().push((w, n));
    }
    for (ir, mut tdo) in by_ir {
        tdo.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
        let total: u64 = tdo.iter().map(|(_, n)| n).sum();
        let top: Vec<String> = (tdo.iter().take(3))
            .map(|(v, n)| format!("0x{v:08X} x{n}"))
            .collect();
        let ir = ir.map_or("unknown".into(), |v| format!("0x{v:X}"));
        println!(
            "  after IR {ir}: {total} 32-bit DR scans, TDO most often {}",
            top.join(", ")
        );
    }
    let tb = rec.timebase();
    let secs = |t: Tick| t as f64 * tb.den() as f64 / tb.num() as f64;
    for f in &s.failures {
        let captured: String = f
            .captured
            .iter()
            .rev()
            .map(|&b| if b { '1' } else { '0' })
            .collect();
        let p = tck.and_then(|c| period(rec, c, f.start, f.end));
        println!(
            "  IR scan #{} failing at {:.6} s: {} bits, captured 0b{captured}{}",
            f.index,
            secs(f.start),
            f.length,
            p.map_or(String::new(), |p| format!(", TCK period {:.1} ns", p * 1e9))
        );
    }
    if let Some(c) = tck {
        let ok = s.ir_spans.iter().filter(|x| x.2).map(|x| (x.0, x.1));
        let failed = s.ir_spans.iter().filter(|x| !x.2).map(|x| (x.0, x.1));
        println!("IR scans' TCK period, passing: {}", periods(rec, c, ok));
        println!("IR scans' TCK period, failing: {}", periods(rec, c, failed));
    }
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
