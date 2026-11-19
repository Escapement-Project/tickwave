//! What `jtag_check` and `cjtag_check` share: a checker stacked on JTAG's completed items, and
//! the report.

#![allow(dead_code)]

pub mod rows;

use escapement_tickwave_decode::{
    AnalysisId, Decoder, Definition, Delivery, Exchange, Host, InputSpec, KindId, Mode, Op, Pause,
    Schema, Status, Tick, UpstreamDecl, Value, name,
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
    /// IR scans of the switch from cJTAG to JTAG, whose captures are no TAP's: the one just
    /// before two zero-bit scans, while nothing drives TDO yet, and the one closing the command
    /// window those open, which the adapter's 1-bit stage reads (TI's SWCU185, §6.2.2).
    pub ir_switch: u64,
    /// Every IR scan, counted by `settle` once the zero-bit scans are all in.
    pub irs: Vec<IrSeen>,
    /// Where each zero-bit scan passes Capture-DR, from `ZeroBits`, where `watch_switch` added
    /// it: on JTAG's four wires alone, since on cJTAG's two a command window carries no switch.
    pub zero_bit: Vec<Tick>,
    pub switch_watched: bool,
    /// Where a chain is set: each TAP's parts by kind and place, and the scans not cut, by why.
    pub parts: BTreeMap<(&'static str, u64), u64>,
    pub unsplit: BTreeMap<u16, u64>,
    pub settled: bool,
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

/// An IR scan as `Seen` keeps it until it is counted: its place, span and length, the bits it
/// captured where TDO gave them, and where the first DR scan after it starts.
#[derive(Debug)]
pub struct IrSeen {
    pub index: u64,
    pub start: Tick,
    pub end: Tick,
    pub length: u64,
    pub captured: Option<Vec<bool>>,
    pub first_dr: Option<Tick>,
}

const CAPTURE_DR: u8 = 3;
const EXIT1_DR: u8 = 5;

impl Seen {
    /// Counts the IR scans by their captures, the switch's apart. Two zero-bit scans open a
    /// command window and the next IR scan closes it; the IR scan before them opens the switch
    /// where no DR scan came between.
    pub fn settle(&mut self) {
        if std::mem::replace(&mut self.settled, true) {
            return;
        }
        self.zero_bit.sort_unstable();
        let irs = std::mem::take(&mut self.irs);
        let mut switch = vec![false; irs.len()];
        for (i, ir) in irs.iter().enumerate() {
            let next = irs.get(i + 1).map_or(Tick::MAX, |n| n.start);
            let from = self.zero_bit.partition_point(|&z| z < ir.end);
            let to = self.zero_bit.partition_point(|&z| z < next);
            if to - from < 2 {
                continue;
            }
            if let Some(closes) = switch.get_mut(i + 1) {
                *closes = true;
            }
            let second = self.zero_bit[from + 1];
            if ir.first_dr.is_none_or(|d| d > second) {
                switch[i] = true;
            }
        }
        for (ir, switch) in irs.into_iter().zip(switch) {
            self.count(ir, switch);
        }
    }

    fn count(&mut self, ir: IrSeen, switch: bool) {
        if switch {
            self.ir_switch += 1;
            return;
        }
        let Some(captured) = ir.captured else {
            self.ir_unread += 1;
            self.unread.push((ir.start, ir.end));
            return;
        };
        let ok = captured[..2] == [true, false];
        if !ok {
            self.ir_failed += 1;
            if self.failures.len() < LISTED {
                self.failures.push(Failure {
                    index: ir.index,
                    start: ir.start,
                    end: ir.end,
                    length: ir.length,
                    captured,
                });
            }
        }
        self.ir_spans.push((ir.start, ir.end, ok));
    }
}

/// Stacks the checker on JTAG analysis `j`, counting into `seen`.
pub fn stack(
    h: &mut Host,
    j: AnalysisId,
    seen: &Arc<Mutex<Seen>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let s = seen.clone();
    h.add(
        name("check"),
        Arc::new(
            Definition::new("check", 1, Schema::new(vec![])).with_inputs([UpstreamDecl::new(
                "items",
                Mode::Completed,
            )
            .with_kinds(["state", "path", "ir-scan", "dr-scan", "instruction", "data"])]),
        ),
        InputSpec::Upstream(j),
        Box::new(move |_, _| Box::new(Checker(s.clone()))),
        &[],
        vec![],
    )?;
    Ok(())
}

/// Stacks the watcher of zero-bit scans on JTAG analysis `j`, so `seen` counts the IR scans of
/// the switch from cJTAG to JTAG apart.
pub fn watch_switch(
    h: &mut Host,
    j: AnalysisId,
    seen: &Arc<Mutex<Seen>>,
) -> Result<(), Box<dyn std::error::Error>> {
    seen.lock().unwrap().switch_watched = true;
    let s = seen.clone();
    h.add(
        name("zero-bit"),
        Arc::new(
            Definition::new("zero-bit", 1, Schema::new(vec![])).with_inputs([UpstreamDecl::new(
                "paths",
                Mode::Pieces,
            )
            .with_kinds(["path"])
            .with_sequences(["via"])]),
        ),
        InputSpec::Upstream(j),
        Box::new(move |_, _| {
            Box::new(ZeroBits {
                seen: s.clone(),
                last: None,
                to: None,
            })
        }),
        &[],
        vec![],
    )?;
    Ok(())
}

/// Finds the zero-bit scans, Capture-DR straight to Exit1-DR, in JTAG's paths piece by piece: a
/// completed path carries no states. A path ends where its `to`, a held state or, for one cut at
/// its length, the next, takes over.
pub struct ZeroBits {
    seen: Arc<Mutex<Seen>>,
    last: Option<u8>,
    to: Option<u8>,
}

impl Decoder for ZeroBits {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        let mut s = self.seen.lock().unwrap();
        while let Some(d) = ex.receive() {
            match d {
                Delivery::Piece { piece, .. } => match piece.op() {
                    Op::Set { field, value, .. } if *field == jtag::TO => {
                        self.to = match value {
                            Value::Name(n) => u8::try_from(*n).ok(),
                            _ => None,
                        };
                    }
                    Op::Extend { elements, .. } => {
                        for e in elements {
                            if self.last == Some(CAPTURE_DR) && e.value == EXIT1_DR {
                                s.zero_bit.push(e.cell.start);
                            }
                            self.last = Some(e.value);
                        }
                    }
                    Op::Close { .. } => self.last = self.to.take(),
                    _ => {}
                },
                Delivery::Lost { span, .. } => {
                    self.last = None;
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
                    if let Some(Value::Unknown { reason, .. }) =
                        item.field(jtag::SPLIT).map(|f| &f.value)
                    {
                        *s.unsplit.entry(reason.0).or_default() += 1;
                    }
                    match item.id.kind {
                        jtag::INSTRUCTION | jtag::DATA => {
                            let tap = match item.field(jtag::TAP_INDEX).map(|f| &f.value) {
                                Some(Value::Word { value, .. }) => *value,
                                _ => u64::MAX,
                            };
                            let kind = if item.id.kind == jtag::DATA {
                                "DR"
                            } else {
                                "IR"
                            };
                            *s.parts.entry((kind, tap)).or_default() += 1;
                        }
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
                            let ir = IrSeen {
                                index: s.ir - 1,
                                start: item.start,
                                end: item.end,
                                length,
                                captured: tdo.filter(|&(_, n)| n >= 2).map(|(b, n)| unpacked(b, n)),
                                first_dr: None,
                            };
                            s.irs.push(ir);
                        }
                        jtag::DR_SCAN => {
                            s.dr += 1;
                            if let Some(ir) = s.irs.last_mut() {
                                ir.first_dr.get_or_insert(item.start);
                            }
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
    let mut s = seen.lock().unwrap();
    s.settle();
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
    let switch = match s.switch_watched {
        true => format!(", {} in the switch from cJTAG to JTAG", s.ir_switch),
        false => String::new(),
    };
    println!(
        "IR capture ending 01: {} of {} ({} failed, {} without TDO value{switch})",
        s.ir - s.ir_failed - s.ir_unread - s.ir_switch,
        s.ir,
        s.ir_failed,
        s.ir_unread,
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
    if !s.parts.is_empty() || !s.unsplit.is_empty() {
        let parts: Vec<String> = (s.parts.iter())
            .map(|((k, tap), n)| format!("TAP {tap} {k} x{n}"))
            .collect();
        let schema = &h.definition(j).schema;
        let why: Vec<String> = (s.unsplit.iter())
            .map(|(r, n)| {
                format!(
                    "{} x{n}",
                    schema
                        .reasons
                        .get(usize::from(*r))
                        .map_or("?", |p| p.as_ref())
                )
            })
            .collect();
        println!("chain parts: {}", parts.join(", "));
        println!(
            "not cut: {}",
            if why.is_empty() {
                "none".into()
            } else {
                why.join("; ")
            }
        );
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

/// Completed items counted by a key made of each, and the statuses they closed with.
pub type Keyed = Arc<Mutex<BTreeMap<String, u64>>>;

/// Counts each completed item of its input under the key `key` makes of it.
pub struct Tally {
    pub counts: Keyed,
    pub key: fn(&escapement_tickwave_decode::Completed) -> String,
}

impl Decoder for Tally {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        let mut c = self.counts.lock().unwrap();
        while let Some(d) = ex.receive() {
            match d {
                Delivery::Item { item, .. } => {
                    let status = match item.status {
                        Status::Complete => "",
                        _ => " (incomplete)",
                    };
                    *c.entry(format!("{}{status}", (self.key)(&item)))
                        .or_default() += 1;
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

/// Stacks a tally of `kinds` of analysis `a`, keyed by `key`.
pub fn tally(
    h: &mut Host,
    a: AnalysisId,
    label: &'static str,
    kinds: &[&'static str],
    key: fn(&escapement_tickwave_decode::Completed) -> String,
) -> Result<Keyed, Box<dyn std::error::Error>> {
    let counts: Keyed = Arc::default();
    let c = counts.clone();
    h.add(
        name(label),
        Arc::new(
            Definition::new(label, 1, Schema::new(vec![])).with_inputs([UpstreamDecl::new(
                "items",
                Mode::Completed,
            )
            .with_kinds(kinds.to_vec())]),
        ),
        InputSpec::Upstream(a),
        Box::new(move |_, _| {
            Box::new(Tally {
                counts: c.clone(),
                key,
            })
        }),
        &[],
        vec![],
    )?;
    Ok(counts)
}

/// A tally's lines, most first, `most` of them.
pub fn tally_lines(counts: &Keyed, most: usize) -> Vec<String> {
    let c = counts.lock().unwrap();
    let mut v: Vec<_> = c.iter().collect();
    v.sort_by_key(|(k, n)| (std::cmp::Reverse(**n), (*k).clone()));
    v.into_iter()
        .take(most)
        .map(|(k, n)| format!("{k} x{n}"))
        .collect()
}

/// The chain (`--chain 4,6`) and the DP's TAP in it (`--dp 0`) the command line gives.
pub fn chain_args() -> (Vec<u8>, Option<i64>) {
    let args: Vec<String> = std::env::args().collect();
    let after = |flag: &str| (args.iter().position(|a| a == flag)).and_then(|k| args.get(k + 1));
    let chain = after("--chain")
        .map(|c| c.split(',').filter_map(|l| l.parse().ok()).collect())
        .unwrap_or_default();
    (chain, after("--dp").and_then(|t| t.parse().ok()))
}

/// JTAG-DP on TAP `tap` of JTAG analysis `j`'s chain and ADIv5 on it, each tallied.
pub fn stack_dp(
    h: &mut Host,
    j: AnalysisId,
    tap: i64,
) -> Result<Vec<(&'static str, Keyed)>, Box<dyn std::error::Error>> {
    let mut tallies = vec![];
    use escapement_tickwave_decode::{Completed, Setting, Value};
    use escapement_tickwave_decoders::{adiv5, jtagdp, swd};
    let dp = h.add(
        name("jtag-dp"),
        jtagdp::definition(),
        InputSpec::Upstream(j),
        jtagdp::JtagDp::factory(),
        &[("tap", Setting::Int(tap))],
        vec![],
    )?;
    let adi = h.add(
        name("adiv5"),
        adiv5::adi_definition(),
        InputSpec::Upstream(dp),
        adiv5::Adi::factory(),
        &[],
        vec![],
    )?;
    fn n(i: &Completed, f: escapement_tickwave_decode::FieldId) -> Option<u64> {
        match i.field(f).map(|f| &f.value) {
            Some(Value::Word { value, .. }) => Some(*value),
            Some(Value::Name(n)) => Some(u64::from(*n)),
            _ => None,
        }
    }
    let transfer = |i: &Completed| -> String {
        if i.id.kind == jtagdp::IDCODE {
            return format!("IDCODE 0x{:08X}", n(i, jtagdp::IDCODE_VALUE).unwrap_or(0));
        }
        let pick = |v: Option<u64>, names: &[&str]| {
            v.and_then(|v| names.get(v as usize).copied())
                .unwrap_or("?")
                .to_owned()
        };
        format!(
            "{} {} 0x{:X} {}",
            pick(n(i, swd::DIR), &["read", "write"]),
            pick(n(i, swd::PORT), &["AP", "DP"]),
            n(i, swd::A).unwrap_or(0),
            pick(n(i, swd::ACK), &swd::ACKS)
        )
    };
    let access = |i: &Completed| -> String {
        match i.id.kind {
            adiv5::DP_READ | adiv5::DP_WRITE => {
                let reg = n(i, adiv5::DP_REG)
                    .and_then(|r| adiv5::DP_NAMES.get(r as usize).copied())
                    .unwrap_or("?");
                // DPIDR's and CTRL/STAT's reads with their values: they name the DP and its
                // power state, a check that each read has its own result.
                let value = match (reg, n(i, adiv5::DP_VALUE), i.id.kind) {
                    ("DPIDR" | "CTRL/STAT", Some(v), adiv5::DP_READ) => format!(" 0x{v:08X}"),
                    _ => String::new(),
                };
                let dir = if i.id.kind == adiv5::DP_READ {
                    "read"
                } else {
                    "write"
                };
                format!("DP {dir} {reg}{value}")
            }
            k => {
                let reg = n(i, adiv5::AP_REG).unwrap_or(0xFFFF);
                // IDR's value too: it names the AP's class.
                let value = match (reg, n(i, adiv5::AP_VALUE)) {
                    (0xFC, Some(v)) => format!(" 0x{v:08X}"),
                    _ => String::new(),
                };
                let class = n(i, adiv5::AP_CLASS)
                    .and_then(|c| adiv5::CLASS_NAMES.get(c as usize).copied())
                    .unwrap_or("unidentified");
                format!(
                    "AP {} {} 0x{reg:02X}{value} {class}",
                    n(i, adiv5::AP).map_or("?".into(), |a| a.to_string()),
                    if k == adiv5::AP_READ { "read" } else { "write" },
                )
            }
        }
    };
    tallies.push((
        "JTAG-DP",
        tally(h, dp, "dp-tally", &["transfer", "idcode"], transfer)?,
    ));
    tallies.push((
        "ADIv5",
        tally(
            h,
            adi,
            "adi-tally",
            &["dp-read", "dp-write", "ap-read", "ap-write"],
            access,
        )?,
    ));
    Ok(tallies)
}
