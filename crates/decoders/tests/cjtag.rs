//! cJTAG on synthetic wires, with JTAG stacked on it, against a direct pipeline that feeds JTAG
//! from cJTAG's own clocks.

mod common;

use common::*;
use escapement_tickwave_decode::{
    AnalysisId, Counts, Decoder, Exchange, Host, HostOptions, InputSpec, Item, KindId, Observation,
    Observed, Out, Pause, Request, Span, Status, Tick, Value, Window, name,
};
use escapement_tickwave_decoders::{
    cjtag::{self, Change, Cjtag, Feed, JtagOnCjtag, Lines, Oscan},
    jtag::{self, Tap},
};
use std::sync::atomic::Ordering::Relaxed;

fn all() -> Vec<Span> {
    vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]
}

// ---- TAP.7 sequences ----------------------------------------------------------------------------

/// TMS bits that make a zero-bit DR scan from Run-Test/Idle back to it.
const ZBS: [u8; 5] = [1, 0, 1, 1, 0];

/// A DR scan from Run-Test/Idle that shifts `n` bits, back to Run-Test/Idle.
fn dr_scan(n: u32) -> Vec<u8> {
    if n == 0 {
        return ZBS.to_vec();
    }
    let mut v = vec![1, 0, 0];
    v.extend(std::iter::repeat_n(0, n as usize - 1));
    v.extend([1, 1, 0]);
    v
}

/// A command from Run-Test/Idle at control level two: CP1 and CP2.
fn command(opcode: u8, operand: u8) -> Vec<u8> {
    let mut v = dr_scan(u32::from(opcode));
    v.extend(dr_scan(u32::from(operand)));
    v
}

/// TMS bits that lock control level two from Run-Test/Idle: two zero-bit scans, a shifting scan.
fn enter_command_level() -> Vec<u8> {
    let mut v = ZBS.repeat(2);
    v.extend(dr_scan(1));
    v
}

// ---- A synthetic OScan1 session -----------------------------------------------------------------

/// Builds TCKC and TMSC at 20 MHz with 1 ns ticks. TMSC changes with TCKC low; an escape toggles it
/// with TCKC high.
struct Gen {
    t: Tick,
    line: bool,
    edges: [Vec<Tick>; 2],
}

/// The commands the activation sends in OScan1: STMC 4 (SGC = 0), MSC 0, ECL.
const ACTIVATION_OSCAN1: [(u8, u8); 3] = [(0, 4), (7, 0), (0, 1)];

impl Gen {
    fn line_to(&mut self, v: bool) {
        if self.line != v {
            self.edges[1].push(self.t);
            self.line = v;
        }
    }

    fn bit(&mut self, value: bool, toggles: usize) {
        self.t += 10;
        self.line_to(value);
        self.t += 15;
        self.edges[0].push(self.t);
        for _ in 0..toggles {
            self.t += 2;
            let v = !self.line;
            self.line_to(v);
        }
        self.t += 25 - 2 * toggles as Tick;
        let fall = self.t.max(self.edges[0].last().copied().unwrap_or(0) + 1);
        self.edges[0].push(fall);
    }

    /// The activation the project's probe sends: a reset escape; in JScan, control level two,
    /// SREDGE = 1 and STFMT 9, then a check packet; in OScan1, STMC 4, MSC 0 and ECL, each followed
    /// by a scan packet that does not advance the TAP and a check packet.
    fn activate_oscan1(&mut self) {
        self.bit(true, 10);
        let mut jscan = vec![1, 1, 1, 1, 1, 0];
        jscan.extend(enter_command_level());
        jscan.extend(command(1, 1));
        jscan.extend(command(3, 9));
        for b in jscan {
            self.bit(b == 1, 0);
        }
        self.check();
        for (opcode, operand) in ACTIVATION_OSCAN1 {
            for b in command(opcode, operand) {
                self.clock_oscan1(b == 1, false, true);
            }
            self.clock_oscan1(false, false, true);
            self.check();
        }
    }

    /// A check packet that continues.
    fn check(&mut self) {
        for _ in 0..4 {
            self.bit(false, 0);
        }
    }

    /// One JTAG clock as an OScan1 scan packet: nTDI, TMS, TDO.
    fn clock_oscan1(&mut self, tms: bool, tdi: bool, tdo: bool) {
        self.bit(!tdi, 0);
        self.bit(tms, 0);
        self.bit(tdo, 0);
    }
}

const IDCODE: u32 = 0x4BA0_0477;
const DR_TEXT: &[u8; 32] = b"tickwave over cJTAG, OScan1 sel.";

/// The JTAG clocks of one session: reset, IDCODE through IR and DR, a 256-bit DR scan of text, and
/// idle clocks.
fn session_clocks() -> Vec<(bool, bool, bool)> {
    let mut clocks = vec![];
    let tms = |c: &mut Vec<(bool, bool, bool)>, seq: &[u8]| {
        c.extend(seq.iter().map(|&b| (b == 1, true, true)))
    };
    let shift = |c: &mut Vec<(bool, bool, bool)>, tdi: &[bool], tdo: &[bool]| {
        for k in 0..tdi.len() {
            c.push((k == tdi.len() - 1, tdi[k], tdo[k]));
        }
    };
    let bits = |v: u64, n: usize| -> Vec<bool> { (0..n).map(|k| v >> k & 1 == 1).collect() };
    let text: Vec<bool> = (DR_TEXT.iter())
        .flat_map(|b| (0..8).map(move |k| b >> k & 1 == 1))
        .collect();
    tms(&mut clocks, &[1, 1, 1, 1, 1, 0, 0]);
    tms(&mut clocks, &[1, 1, 0, 0]);
    shift(&mut clocks, &bits(0b1110, 4), &bits(0b0001, 4));
    tms(&mut clocks, &[1, 0, 1, 0, 0]);
    shift(&mut clocks, &bits(0, 32), &bits(IDCODE.into(), 32));
    tms(&mut clocks, &[1, 0, 1, 1, 0, 0]);
    shift(&mut clocks, &bits(0b1010, 4), &bits(0b0001, 4));
    tms(&mut clocks, &[1, 0, 1, 0, 0]);
    let inverted: Vec<bool> = text.iter().map(|b| !b).collect();
    shift(&mut clocks, &text, &inverted);
    tms(&mut clocks, &[1, 0, 0, 0, 0]);
    clocks
}

/// The session `sessions` times over, each from its own activation, with each `(line, from, to)`
/// a gap in TCKC (0) or TMSC (1).
fn synth_with(gaps: &[(usize, Tick, Tick)], sessions: usize) -> (Wires, Gen) {
    let mut g = Gen {
        t: 1_000,
        line: false,
        edges: Default::default(),
    };
    let clocks = session_clocks();
    for k in 0..sessions {
        if k > 0 {
            g.t += 1_000;
        }
        g.activate_oscan1();
        for &(m, i, o) in &clocks {
            g.clock_oscan1(m, i, o);
        }
    }
    let mut lines = vec![
        Line {
            edges: g.edges[0].clone(),
            ..Line::default()
        },
        Line {
            edges: g.edges[1].clone(),
            ..Line::default()
        },
    ];
    for &(k, from, to) in gaps {
        lines[k].gaps.push(Span {
            start: from,
            end: to,
        });
    }
    let w = Wires {
        lines,
        end: g.t + 100,
        reversed: false,
    };
    (w, g)
}

fn synth() -> Wires {
    synth_with(&[], 1).0
}

// ---- Running it ---------------------------------------------------------------------------------

fn host(w: Wires, window: Tick, local: Option<Tick>, qualified: bool) -> (Host, [AnalysisId; 2]) {
    host_with(HostOptions::default(), w, window, local, qualified)
}

fn host_with(
    options: HostOptions,
    w: Wires,
    window: Tick,
    local: Option<Tick>,
    qualified: bool,
) -> (Host, [AnalysisId; 2]) {
    let mut h = Host::new(options);
    let c = h
        .add(
            name("cjtag"),
            cjtag::definition(),
            InputSpec::Wire {
                source: Box::new(w),
                local,
            },
            Cjtag::factory(window),
            &[cjtag::assume_qualified(qualified)],
            all(),
        )
        .unwrap();
    let j = h
        .add(
            name("jtag"),
            cjtag::jtag_definition(),
            InputSpec::Upstream(cjtag::jtag_subscription(c)),
            JtagOnCjtag::factory(),
            &[],
            all(),
        )
        .unwrap();
    (h, [c, j])
}

fn run(w: Wires, window: Tick) -> (Host, [AnalysisId; 2]) {
    run_with(HostOptions::default(), w, window)
}

fn run_with(options: HostOptions, w: Wires, window: Tick) -> (Host, [AnalysisId; 2]) {
    let (mut h, a) = host_with(options, w, window, None, true);
    h.run_until(|_| false).unwrap();
    assert!(a.iter().all(|&x| h.finished(x).is_some()));
    (h, a)
}

/// Each analysis's counts and lost stretches.
fn everything(h: &Host, a: [AnalysisId; 2]) -> Vec<(Counts, Vec<Span>)> {
    a.iter()
        .map(|&x| (h.store(x).counts, h.lost(x).to_vec()))
        .collect()
}

/// cJTAG and JTAG in one wire decoder, JTAG fed from cJTAG's own clocks: only JTAG's ops are
/// emitted.
#[derive(Clone)]
struct Direct {
    lines: Option<Lines>,
    oscan: Option<Oscan>,
    tap: Tap,
    started: bool,
    next: Tick,
    window: Tick,
    first: [Option<bool>; 2],
}

impl Direct {
    fn feed(&mut self, c: Change, out: &mut Out) {
        let mut scratch = Out::default();
        match (c, &mut self.oscan) {
            (Change::Gap { from, to, .. }, Some(o)) if from < to => o.change(&c, &mut scratch),
            (Change::Gap { .. }, Some(_)) => {}
            (
                Change::Gap {
                    from,
                    to,
                    clock,
                    line,
                },
                None,
            ) => {
                self.oscan = Some(Oscan::new(to, clock, line, true));
                if from < to {
                    self.tap.gap(from, to, out);
                }
            }
            (_, Some(o)) => o.change(&c, &mut scratch),
            (_, None) => {}
        }
        if let Some(o) = &mut self.oscan {
            for f in o.clocks.drain(..) {
                match f {
                    Feed::Clock(c) => self.tap.clock(&c, out),
                    Feed::Gap { from, to } => self.tap.gap(from, to, out),
                }
            }
        }
    }

    fn observe(&mut self, w: &Window, out: &mut Out) {
        let mut changes = vec![];
        for e in &w.events {
            match *e {
                Observed::Start { port, level } => {
                    if self.lines.is_none() {
                        self.first[usize::from(port)] = level;
                    }
                }
                Observed::Unknown { at, .. } if w.end == Some(at) => {}
                e => {
                    let first = self.first;
                    if self.lines.is_none()
                        && let [Some(clock), Some(line)] = first
                    {
                        self.oscan = Some(Oscan::new(w.from, clock, line, true));
                    }
                    let l = self.lines.get_or_insert_with(|| Lines::new(w.from, first));
                    l.observe(&e, &mut changes);
                    for c in changes.drain(..) {
                        self.feed(c, out);
                    }
                }
            }
        }
        if let Some(l) = &mut self.lines {
            l.flush(&mut changes);
        }
        for c in changes.drain(..) {
            self.feed(c, out);
        }
    }
}

impl Decoder for Direct {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        if !std::mem::replace(&mut self.started, true) {
            self.tap.begin(&mut ex.out);
        }
        let Some(w) = ex.reply() else {
            ex.request(Request::Window {
                from: self.next,
                upto: self.next + self.window,
            });
            return Pause::Input;
        };
        self.observe(&w, &mut ex.out);
        if let Some(end) = w.end {
            if let (Some(from), Some(l)) = (self.lines.as_ref().and_then(Lines::gap), &self.lines)
                && from < end
            {
                let [clock, line] = l.levels();
                let c = Change::Gap {
                    from,
                    to: end,
                    clock,
                    line,
                };
                self.feed(c, &mut ex.out);
            }
            self.tap.finish(end, &mut ex.out);
            ex.finish(end);
            return Pause::Finished;
        }
        self.next = w.upto;
        ex.request(Request::Window {
            from: self.next,
            upto: self.next + self.window,
        });
        Pause::Input
    }

    fn save(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(self.clone()))
    }
}

/// JTAG's output from the direct pipeline.
fn direct(w: Wires) -> Counts {
    let mut h = Host::default();
    let d = h
        .add(
            name("direct"),
            std::sync::Arc::new(
                (*cjtag::jtag_definition())
                    .clone()
                    .with_ports(["TCKC", "TMSC"]),
            ),
            InputSpec::Wire {
                source: Box::new(w),
                local: None,
            },
            Box::new(|_, _| {
                Box::new(Direct {
                    lines: None,
                    oscan: None,
                    tap: Tap::new(0, true),
                    started: false,
                    next: 0,
                    window: 1 << 20,
                    first: [None; 2],
                })
            }),
            &[],
            all(),
        )
        .unwrap();
    h.run_until(|_| false).unwrap();
    assert!(h.finished(d).is_some());
    h.store(d).counts
}

fn of(h: &Host, a: AnalysisId, kinds: &[KindId]) -> Vec<Item> {
    let mut items: Vec<Item> = (h.store(a).items.values())
        .filter(|i| kinds.contains(&i.id.kind))
        .cloned()
        .collect();
    items.sort_by_key(|i| i.start);
    items
}

fn value(i: &Item, f: escapement_tickwave_decode::FieldId) -> Option<Value> {
    i.field(f).map(|f| f.value.clone())
}

fn bytes_of(els: &[escapement_tickwave_decode::Element]) -> Vec<u8> {
    els.chunks(8)
        .map(|c| (c.iter().enumerate()).fold(0, |b, (k, e)| b | e.value << k))
        .collect()
}

fn length(i: &Item) -> Option<u64> {
    match value(i, jtag::LENGTH)? {
        Value::Word { value, .. } => Some(value),
        _ => None,
    }
}

/// The 256-bit DR scan with TDI, the first at or after `after`.
fn long_scan(h: &Host, j: AnalysisId, after: Tick) -> Item {
    of(h, j, &[jtag::DR_SCAN])
        .into_iter()
        .find(|i| length(i) == Some(256) && i.start >= after)
        .unwrap()
}

// ---- Tests --------------------------------------------------------------------------------------

/// Host options that bound each read to `ticks` ticks of observations.
fn reads_of(ticks: usize) -> HostOptions {
    let mut o = HostOptions::default();
    o.window_ticks = ticks;
    o
}

#[test]
fn reads_bounded_to_a_few_ticks_do_not_change_the_output() {
    for w in [synth(), gapped().0] {
        let (h, a) = run(w.clone(), 1 << 40);
        let want = everything(&h, a);
        for ticks in [1, 2, 5, 64] {
            let (h, a) = run_with(reads_of(ticks), w.clone(), 1 << 40);
            assert_eq!(everything(&h, a), want, "reads of {ticks} ticks");
        }
    }
}

#[test]
fn the_activation_and_the_packets_decode() {
    let (h, [c, j]) = run(synth(), 1000);
    fits_its_rows(&h, c);
    fits_its_rows(&h, j);
    let esc = of(&h, c, &[cjtag::ESCAPE]);
    assert_eq!(esc.len(), 1);
    assert_eq!(
        value(&esc[0], cjtag::MEANING),
        Some(Value::Name(0)),
        "reset"
    );
    assert_eq!(esc[0].seq(cjtag::TOGGLE_BITS).len(), 10);
    let commands: Vec<_> = (of(&h, c, &[cjtag::COMMAND]).iter())
        .map(|i| (value(i, cjtag::EFFECT), i.parent.is_some()))
        .collect();
    let text = |s: &str| Some(Value::Text(s.into()));
    assert_eq!(
        commands,
        [
            (text("SREDGE = 1"), true),
            (text("SCNFMT = OScan1"), true),
            (text("Scan Group Candidate SGC = 0"), true),
            (text("Scan Group Candidate: controller 0"), true),
            (text("exit the control level (ECL)"), true),
        ]
    );
    let checks = of(&h, c, &[cjtag::CHECK]);
    assert_eq!(checks.len(), 4);
    assert!(
        (checks.iter()).all(|i| value(i, cjtag::RESULT) == Some(Value::Name(1))),
        "each continues"
    );
    let formats = of(&h, c, &[cjtag::FORMAT]);
    assert_eq!(value(&formats[0], cjtag::SCAN_FORMAT), text("OScan1"));
    let in_oscan1: usize = (ACTIVATION_OSCAN1.iter())
        .map(|&(o, p)| command(o, p).len() + 1)
        .sum();
    let packets = of(&h, c, &[cjtag::PACKET]);
    assert_eq!(packets.len(), session_clocks().len() + in_oscan1);
    let held = (packets.iter())
        .filter(|p| value(p, cjtag::ADVANCE) == Some(Value::Bool(false)))
        .count();
    assert_eq!(held, ACTIVATION_OSCAN1.len());
    // A DR scan in JScan carries no TDI on two wires, and says so.
    let in_jscan = &of(&h, j, &[jtag::DR_SCAN])[0];
    assert_eq!(
        value(in_jscan, jtag::TDI_VALUE),
        Some(Value::Unknown {
            reason: jtag::TWO_WIRE,
            needs: None
        })
    );
    let idcode = (of(&h, j, &[jtag::DR_SCAN]).into_iter())
        .find(|i| {
            length(i) == Some(32)
                && value(i, jtag::TDI_VALUE).is_some_and(|v| {
                    v != Value::Unknown {
                        reason: jtag::TWO_WIRE,
                        needs: None,
                    }
                })
        })
        .unwrap();
    assert_eq!(
        value(&idcode, jtag::TDO_VALUE),
        Some(Value::Bits {
            bytes: IDCODE.to_le_bytes().to_vec(),
            bits: 32
        })
    );
    let long = long_scan(&h, j, 0);
    assert_eq!(bytes_of(long.seq(jtag::TDI_BITS)), DR_TEXT);
    assert_eq!(h.store(c).counts.incomplete, 0);
    assert!(h.lost(c).is_empty() && h.lost(j).is_empty());
}

#[test]
fn a_jtag_bit_names_the_cjtag_element_it_came_from() {
    let (h, [c, j]) = run(synth(), 1000);
    let scan = long_scan(&h, j, 0);
    for (k, e) in scan.seq(jtag::TDI_BITS).iter().enumerate() {
        let Observation::Upstream { id, seq, index } = e.at else {
            panic!("bit {k} names no upstream element");
        };
        assert_eq!((id.kind, seq, index), (cjtag::PACKET, cjtag::NTDI_BITS, 0));
        let packet = &h.store(c).items[&id];
        let ntdi = packet.seq(cjtag::NTDI_BITS)[0];
        assert_eq!(ntdi.value, 1 - e.value, "nTDI is TDI inverted");
        assert!(matches!(ntdi.at, Observation::Wire(_)));
        assert_eq!(e.cell.start, packet.start);
    }
    let tdo = scan.seq(jtag::TDO_BITS)[0].at;
    assert!(matches!(
        tdo,
        Observation::Upstream {
            seq: cjtag::TDO_BITS,
            ..
        }
    ));
}

#[test]
fn tdo_has_a_driver_only_where_qualification_is_assumed() {
    let driver = |qualified| {
        let (mut h, [c, _]) = host(synth(), 1000, None, qualified);
        h.run_until(|_| false).unwrap();
        let p = &of(&h, c, &[cjtag::PACKET])[0];
        let f = p.field(cjtag::PACKET_TDO).unwrap();
        (f.driver, value(p, cjtag::DRIVER))
    };
    assert_eq!(driver(true), (Some(cjtag::TARGET), None));
    assert_eq!(
        driver(false),
        (
            None,
            Some(Value::Unknown {
                reason: cjtag::UNQUALIFIED,
                needs: None
            })
        )
    );
}

#[test]
fn hosted_cjtag_feeds_jtag_as_the_direct_run_does() {
    let want = direct(synth());
    let (h, [c, _]) = run(synth(), 1 << 40);
    let cjtag = h.store(c).counts;
    for window in [1, 7, 1000, 1 << 40] {
        let (h, [c, j]) = run(synth(), window);
        assert_eq!(h.store(j).counts, want, "window {window}");
        assert_eq!(h.store(c).counts, cjtag, "window {window}");
    }
}

/// The session twice, with TCKC lost for 400 ticks in the middle of the first one's 256-bit DR
/// scan, and TMSC from 300 ticks later for 400: one stretch of 700 where either line is lost.
/// Nothing is framed after it until the second session's reset escape. Also where the cut scan
/// starts.
fn gapped() -> (Wires, Span, Tick) {
    let (h, [_, j]) = run(synth_with(&[], 2).0, 1000);
    let scan = long_scan(&h, j, 0);
    let mid = scan.start + (scan.end.unwrap() - scan.start) / 2;
    let (w, _) = synth_with(&[(0, mid, mid + 400), (1, mid + 300, mid + 700)], 2);
    let lost = Span {
        start: mid,
        end: mid + 700,
    };
    (w, lost, scan.start)
}

#[test]
fn lost_input_reaches_jtag_and_is_declared_again() {
    let (w, lost, start) = gapped();
    let want = direct(w.clone());
    for window in [1, 13, 1000, 1 << 40] {
        let (h, [c, j]) = run(w.clone(), window);
        assert_eq!(h.store(j).counts, want, "window {window}");
        assert_eq!((h.lost(c), h.lost(j)), (&[lost][..], &[lost][..]));
        fits_its_rows(&h, c);
        fits_its_rows(&h, j);
    }
    let (h, [c, j]) = run(w.clone(), 1000);
    let cut = (of(&h, j, &[jtag::DR_SCAN]).into_iter())
        .find(|i| i.start == start)
        .unwrap();
    assert_eq!(
        (cut.end, cut.status),
        (Some(lost.start), Some(Status::Incomplete(jtag::GAP_CUT)))
    );
    assert!(
        (of(&h, c, &[cjtag::FORMAT]).iter())
            .any(|i| i.end == Some(lost.start)
                && i.status == Some(Status::Incomplete(cjtag::GAP_CUT))),
        "the gap cuts the format"
    );
    // After the gap the TAP state is unknown, for that reason.
    let after = (of(&h, j, &[jtag::STATE]).into_iter())
        .find(|i| i.start == lost.end)
        .unwrap();
    assert_eq!(
        value(&after, jtag::TAP_STATE),
        Some(Value::Unknown {
            reason: jtag::LOST_STATE,
            needs: None
        })
    );
    // The second session decodes whole.
    let second = long_scan(&h, j, lost.end);
    assert_eq!(second.status, Some(Status::Complete));
    assert_eq!(bytes_of(second.seq(jtag::TDI_BITS)), DR_TEXT);
    // A run that starts inside the gap knows nothing until it ends, and says so.
    let inside = lost.start + 350;
    let (mut h, [c, j]) = host(w, 1000, Some(inside), true);
    h.run_until(|_| false).unwrap();
    let rest = Span {
        start: inside,
        end: lost.end,
    };
    assert_eq!((h.lost(c), h.lost(j)), (&[rest][..], &[rest][..]));
}

#[test]
fn a_restored_chain_resumes_inside_a_gap_a_command_and_a_scan() {
    let (w, lost, start) = gapped();
    let (h, a) = run(w.clone(), 50);
    let want = everything(&h, a);
    let command = &of(&h, a[0], &[cjtag::COMMAND])[0];
    let later = long_scan(&h, a[1], lost.end);
    let cuts = [
        ("a gap", lost.start + 350),
        (
            "a command",
            command.start + (command.end.unwrap() - command.start) / 2,
        ),
        ("a scan", start + (lost.start - start) / 2),
        (
            "a scan after the loss",
            later.start + (later.end.unwrap() - later.start) / 2,
        ),
    ];
    for (inside, at) in cuts {
        let (mut h, a) = host(w.clone(), 50, None, true);
        h.run_until(|h| h.consumed(a[0]) >= at && h.quiet_below(a[0]))
            .unwrap();
        let cut = h.checkpoint(a[0]).unwrap();
        h.run_until(|_| false).unwrap();
        assert_eq!(everything(&h, a), want, "forward past {inside}");
        assert_eq!(h.restore(&cut).unwrap().len(), 2);
        h.run_until(|_| false).unwrap();
        assert_eq!(everything(&h, a), want, "restored inside {inside}");
    }
}

/// Runs `h` turn by turn until `last` finishes, raising each analysis's interrupt before its turn
/// where `interrupt`. Returns the turns taken.
fn turn_by_turn(h: &mut Host, a: [AnalysisId; 2], interrupt: bool) -> u64 {
    let mut turns = 0;
    while h.finished(a[1]).is_none() {
        while let Some(r) = h.take_io().unwrap() {
            h.reply(r).unwrap();
        }
        let x = *a.iter().rev().find(|&&x| h.runnable(x)).unwrap();
        h.interrupt(x).store(interrupt, Relaxed);
        h.turn(x).unwrap();
        turns += 1;
        assert!(turns < 1_000_000, "no progress");
    }
    turns
}

#[test]
fn an_interrupted_turn_carries_on_to_the_same_output() {
    let (w, _, _) = gapped();
    let (mut h, a) = host(w.clone(), 1000, None, true);
    let plain = turn_by_turn(&mut h, a, false);
    let want = everything(&h, a);
    let (mut h, a) = host(w, 1000, None, true);
    let interrupted = turn_by_turn(&mut h, a, true);
    assert_eq!(everything(&h, a), want);
    assert!(
        interrupted > 20 * plain,
        "{interrupted} turns against {plain}"
    );
}

/// The session with the TMSC edge before bit `k`'s rise moved back onto the TCKC fall before it.
fn moved_to_fall(w: &Wires, g: &Gen, k: usize) -> Wires {
    let mut moved = w.clone();
    let fall = g.edges[0][2 * k - 1];
    let e = (moved.lines[1].edges.iter())
        .position(|&x| x == fall + 10)
        .unwrap();
    moved.lines[1].edges[e] = fall;
    moved
}

#[test]
fn a_tmsc_change_at_a_tckc_fall_counts_after_it_in_either_order() {
    let (w, g) = synth_with(&[], 1);
    let want = run(w.clone(), 1000);
    let want = everything(&want.0, want.1);
    // A TMSC change in the middle of the text scan's packets.
    let k = (g.edges[0].len() / 2 - 40..)
        .find(|&k| g.edges[1].contains(&(g.edges[0][2 * k - 1] + 10)))
        .unwrap();
    let mut moved = moved_to_fall(&w, &g, k);
    let (h, a) = run(moved.clone(), 1000);
    assert_eq!(everything(&h, a), want);
    moved.reversed = true;
    let (h, a) = run(moved, 1000);
    assert_eq!(
        everything(&h, a),
        want,
        "the order of coincident changes does not matter"
    );
}

#[test]
fn every_open_item_stays_within_the_declared_bound() {
    let (w, _, _) = gapped();
    let (h, [c, j]) = run(w, 1000);
    assert!(h.store(c).counts.peak_open <= 3);
    assert!(h.store(j).counts.peak_open <= 2);
}

#[test]
fn after_sredge_oscan_bits_are_sampled_at_the_rise() {
    let (w, g) = synth_with(&[], 1);
    let (h, [c, _]) = run(w, 1000);
    let formats = of(&h, c, &[cjtag::FORMAT]);
    assert_eq!(
        value(&formats[0], cjtag::SAMPLING),
        Some(Value::Name(0)),
        "TCKC rise"
    );
    let rises: std::collections::HashSet<Tick> = g.edges[0].iter().step_by(2).copied().collect();
    for p in of(&h, c, &[cjtag::PACKET]) {
        for s in [cjtag::NTDI_BITS, cjtag::TMS_BITS, cjtag::TDO_BITS] {
            for e in p.seq(s) {
                let Observation::Wire(t) = e.at else {
                    panic!("a packet element names no wire tick");
                };
                assert!(rises.contains(&t), "{t} is not a TCKC rise");
            }
        }
    }
}

// ---- Selection, deselection and CR scans --------------------------------------------------------

impl Gen {
    fn new() -> Self {
        Self {
            t: 1_000,
            line: false,
            edges: Default::default(),
        }
    }

    fn wires(&self) -> Wires {
        let line = |k: usize| Line {
            edges: self.edges[k].clone(),
            ..Line::default()
        };
        Wires {
            lines: vec![line(0), line(1)],
            end: self.t + 100,
            reversed: false,
        }
    }

    fn bits(&mut self, v: u32, n: usize) {
        for k in 0..n {
            self.bit(v >> k & 1 == 1, 0);
        }
    }

    /// Standard Protocol TMS bits.
    fn jscan(&mut self, tms: &[u8]) {
        for &b in tms {
            self.bit(b == 1, 0);
        }
    }

    fn oscan1(&mut self, tms: &[u8]) {
        for &b in tms {
            self.clock_oscan1(b == 1, false, true);
        }
    }

    /// A reset escape, then the Standard Protocol to Run-Test/Idle.
    fn reset_to_idle(&mut self) {
        self.bit(true, 10);
        self.jscan(&[1, 1, 1, 1, 1, 0]);
    }

    /// A selection escape on a 0 bit, then a Selection Sequence: OAC, EC, a long form's Global
    /// Register State, and a Check Packet.
    fn select(&mut self, oac: u32, ec: u32, registers: Option<u32>) {
        self.bit(false, 6);
        self.bits(oac, 4);
        self.bits(ec, 4);
        if let Some(r) = registers {
            self.bits(r, 24);
        }
        self.check();
    }

    /// An OScan1 command at control level two, with the Scan Packet that does not advance and the
    /// Check Packet a register store brings.
    fn oscan1_command(&mut self, opcode: u8, operand: u8) {
        self.oscan1(&command(opcode, operand));
        self.clock_oscan1(false, false, true);
        self.check();
    }

    /// An OScan1 CR scan of `n` bits from Run-Test/Idle.
    fn oscan1_cr(&mut self, tdi: u64, tdo: u64, n: usize) {
        self.oscan1(&[1, 0, 0]);
        for k in 0..n {
            self.clock_oscan1(k == n - 1, tdi >> k & 1 == 1, tdo >> k & 1 == 1);
        }
        self.oscan1(&[1, 0]);
    }
}

fn decode(g: &Gen, qualified: bool) -> (Host, [AnalysisId; 2]) {
    let (mut h, a) = host(g.wires(), 1000, None, qualified);
    h.run_until(|_| false).unwrap();
    for &x in &a {
        fits_its_rows(&h, x);
    }
    (h, a)
}

fn name_of(i: &Item, f: escapement_tickwave_decode::FieldId) -> Option<u16> {
    match value(i, f)? {
        Value::Name(n) => Some(n),
        _ => None,
    }
}

fn idcode_read(h: &Host, j: AnalysisId) -> bool {
    let want = Value::Bits {
        bytes: IDCODE.to_le_bytes().to_vec(),
        bits: 32,
    };
    (of(h, j, &[jtag::DR_SCAN]).iter()).any(|i| value(i, jtag::TDO_VALUE) == Some(want.clone()))
}

#[test]
fn a_deselection_parks_the_controller_offline_and_a_selection_brings_oscan1_back() {
    let mut g = Gen::new();
    g.reset_to_idle();
    // A qualified deselection on a TMS 0 bit: Run-Test/Idle is its parking state (table 11-5).
    g.bit(false, 4);
    // Another controller's traffic, then a deselection while Offline: both ignored.
    g.bits(0b1011_0110, 8);
    g.bit(false, 4);
    // Star-2, parked in Run-Test/Idle, the short form: OScan1 (table 11-11).
    g.select(0b1100, 0b1000, None);
    for &(m, i, o) in &session_clocks() {
        g.clock_oscan1(m, i, o);
    }
    let (h, [c, j]) = decode(&g, true);
    let escapes: Vec<_> = (of(&h, c, &[cjtag::ESCAPE]).iter())
        .map(|e| {
            let tms: Vec<u8> = e.seq(cjtag::ESCAPE_TMS).iter().map(|b| b.value).collect();
            (name_of(e, cjtag::MEANING), name_of(e, cjtag::RESPONSE), tms)
        })
        .collect();
    assert_eq!(
        escapes,
        [
            (Some(0), None, vec![]),
            (Some(2), Some(0), vec![0]),
            (Some(2), Some(2), vec![]),
            (Some(1), Some(0), vec![]),
        ],
        "reset; a deselection taken, its bit a TMS 0; one ignored offline; a selection taken"
    );
    let offline = of(&h, c, &[cjtag::OFFLINE]);
    assert_eq!(offline.len(), 1);
    assert_eq!(name_of(&offline[0], cjtag::OFFLINE_BY), Some(0));
    let selection_escape = &of(&h, c, &[cjtag::ESCAPE])[3];
    assert_eq!(
        offline[0].end,
        Some(selection_escape.start),
        "Offline until the selection, through the deselection it ignores"
    );
    let sel = &of(&h, c, &[cjtag::SELECTION])[0];
    let fields =
        [cjtag::BRANCH, cjtag::PARKING, cjtag::FORM, cjtag::OUTCOME].map(|f| name_of(sel, f));
    assert_eq!(fields, [Some(3), Some(0), Some(1), Some(0)]);
    assert_eq!(value(sel, cjtag::PROTECT), Some(Value::Bool(false)));
    assert_eq!(
        value(sel, cjtag::SELECTED_FORMAT),
        Some(Value::Text("OScan1".into()))
    );
    assert_eq!(sel.seq(cjtag::SELECTION_BITS).len(), 8);
    let formats = of(&h, c, &[cjtag::FORMAT]);
    assert_eq!(
        value(&formats[0], cjtag::SCAN_FORMAT),
        Some(Value::Text("OScan1".into()))
    );
    assert_eq!(of(&h, c, &[cjtag::PACKET]).len(), session_clocks().len());
    assert!(idcode_read(&h, j), "JTAG follows the parked TAP through it");
    assert_eq!(h.store(c).counts.incomplete, 0);
}

#[test]
fn a_long_selection_loads_the_scan_format_and_protection_withholds_the_target_drive() {
    let mut g = Gen::new();
    g.reset_to_idle();
    // Online in the Standard Protocol: a qualified selection escape, its bit a TMS 0. Long form,
    // PROTECT, SCNFMT OScan1.
    g.select(0b1100, 0b0100, Some(9 << 19));
    for &(m, i, o) in &session_clocks() {
        g.clock_oscan1(m, i, o);
    }
    let (h, [c, j]) = decode(&g, true);
    let sel = &of(&h, c, &[cjtag::SELECTION])[0];
    assert_eq!(name_of(sel, cjtag::FORM), Some(0), "long");
    assert_eq!(value(sel, cjtag::PROTECT), Some(Value::Bool(true)));
    assert_eq!(sel.seq(cjtag::SELECTION_BITS).len(), 32);
    for f in [cjtag::READY, cjtag::DELAY, cjtag::TRANSPORT_STATES] {
        assert!(matches!(value(sel, f), Some(Value::Word { value: 0, .. })));
    }
    assert!(of(&h, c, &[cjtag::OFFLINE]).is_empty());
    let p = &of(&h, c, &[cjtag::PACKET])[0];
    assert_eq!(p.field(cjtag::PACKET_TDO).unwrap().driver, None);
    assert_eq!(
        value(p, cjtag::DRIVER),
        Some(Value::Unknown {
            reason: cjtag::PROTECTED,
            needs: None
        })
    );
    assert!(idcode_read(&h, j));

    // Ready, delay or transport elements in the loaded state: OScan1 is no longer framed.
    for registers in [9 << 19 | 1 << 17, 9 << 19 | 1 << 10] {
        let mut g = Gen::new();
        g.reset_to_idle();
        g.select(0b1100, 0, Some(registers));
        for &(m, i, o) in &session_clocks() {
            g.clock_oscan1(m, i, o);
        }
        let (h, [c, _]) = decode(&g, true);
        assert!(of(&h, c, &[cjtag::PACKET]).is_empty());
    }
}

#[test]
fn a_short_selection_of_every_topology_brings_the_standard_protocol() {
    let mut g = Gen::new();
    g.reset_to_idle();
    g.bit(false, 4);
    g.select(0b0000, 0b1000, None);
    g.jscan(&[1, 1, 0]);
    let (h, [c, _]) = decode(&g, true);
    let formats = of(&h, c, &[cjtag::FORMAT]);
    assert_eq!(
        value(&formats[0], cjtag::SCAN_FORMAT),
        Some(Value::Text("JScan2".into()))
    );
    let after = formats[0].start;
    let tms: usize = (of(&h, c, &[cjtag::JSCAN]).iter())
        .filter(|i| i.start >= after)
        .map(|i| i.seq(cjtag::JSCAN_TMS).len())
        .sum();
    assert_eq!(tms, 3);
}

#[test]
fn a_selection_cut_short_by_an_escape_is_incomplete() {
    let mut g = Gen::new();
    g.reset_to_idle();
    g.bit(false, 6);
    g.bits(0b1100, 3);
    g.bit(true, 10);
    let (h, [c, _]) = decode(&g, true);
    let sel = &of(&h, c, &[cjtag::SELECTION])[0];
    assert_eq!(sel.status, Some(Status::Incomplete(cjtag::INTERRUPTED)));
}

#[test]
fn a_selection_for_another_technology_or_parking_state_leaves_it_offline() {
    for (oac, ec, outcome) in [(0b1010, 0b1000, 1), (0b1100, 0b1011, 2)] {
        let mut g = Gen::new();
        g.reset_to_idle();
        g.bit(false, 4);
        g.select(oac, ec, None);
        g.oscan1(&[1, 1, 0, 1]);
        let (h, [c, _]) = decode(&g, true);
        let sel = &of(&h, c, &[cjtag::SELECTION])[0];
        assert_eq!(name_of(sel, cjtag::OUTCOME), Some(outcome));
        let branch = if outcome == 1 { 7 } else { 3 };
        assert_eq!(name_of(sel, cjtag::BRANCH), Some(branch), "Arm, or Star-2");
        let bits = if outcome == 1 { 4 } else { 8 };
        assert_eq!(sel.seq(cjtag::SELECTION_BITS).len(), bits);
        let offline = of(&h, c, &[cjtag::OFFLINE]);
        let by: Vec<_> = offline
            .iter()
            .map(|o| name_of(o, cjtag::OFFLINE_BY))
            .collect();
        assert_eq!(by, [Some(0), Some(outcome)]);
        assert!(of(&h, c, &[cjtag::PACKET, cjtag::CHECK]).is_empty());
    }
}

#[test]
fn an_unqualified_escape_is_data() {
    // The Standard Protocol inside a zero-bit scan count: shown as ignored, its bit the TMS 1 that
    // starts the second zero-bit scan, and the count goes on to lock control level two.
    let mut g = Gen::new();
    g.reset_to_idle();
    g.jscan(&ZBS);
    g.bit(true, 4);
    g.jscan(&ZBS[1..]);
    g.jscan(&dr_scan(1));
    let (h, [c, j]) = decode(&g, true);
    let esc = &of(&h, c, &[cjtag::ESCAPE])[1];
    assert_eq!(name_of(esc, cjtag::RESPONSE), Some(1));
    assert_eq!(esc.seq(cjtag::ESCAPE_TMS).len(), 1);
    assert!(of(&h, c, &[cjtag::OFFLINE]).is_empty());
    assert_eq!(of(&h, c, &[cjtag::LEVEL]).len(), 1);
    // JTAG takes the escape's TMS bit too: without it the last scan would be an IR scan.
    let scans = of(&h, j, &[jtag::DR_SCAN, jtag::IR_SCAN]);
    assert_eq!(scans.len(), 1);
    assert_eq!(
        (scans[0].id.kind, length(&scans[0])),
        (jtag::DR_SCAN, Some(1))
    );

    // The Advanced Protocol: off a Scan Packet's first bit it is the packet's own bit; on it, the
    // packet is dropped and the controller goes Offline.
    let mut g = Gen::new();
    g.activate_oscan1();
    g.bit(true, 0);
    g.bit(false, 4);
    g.bit(true, 0);
    g.bit(true, 4);
    g.oscan1(&[0, 0, 0]);
    let (h, [c, _]) = decode(&g, true);
    let escapes = of(&h, c, &[cjtag::ESCAPE]);
    assert_eq!(escapes.len(), 2, "the reset and the qualified deselection");
    assert_eq!(name_of(&escapes[1], cjtag::RESPONSE), Some(0));
    let in_oscan1: usize = (ACTIVATION_OSCAN1.iter())
        .map(|&(o, p)| command(o, p).len() + 1)
        .sum();
    assert_eq!(of(&h, c, &[cjtag::PACKET]).len(), in_oscan1 + 1);
    assert_eq!(of(&h, c, &[cjtag::OFFLINE]).len(), 1);
}

#[test]
fn a_cr_scan_is_named_by_its_command_and_carries_its_register() {
    let cnfg0 = 0x03C0_0714;
    let mut g = Gen::new();
    g.activate_oscan1();
    g.oscan1(&enter_command_level());
    g.oscan1_command(9, 2);
    g.oscan1_cr(0, cnfg0, 32);
    g.oscan1_command(8, 1);
    g.oscan1_cr(1, 0, 1);
    // CIDD is a two-part command: the ECL after it is a command, not a CR scan.
    g.oscan1_command(11, 0x10);
    g.oscan1_command(0, 1);
    let (h, [c, _]) = decode(&g, true);
    let crs = of(&h, c, &[cjtag::CR_SCAN]);
    assert_eq!(crs.len(), 2);
    let bits = |v: u64, n: u32| Value::Bits {
        bytes: v.to_le_bytes()[..n.div_ceil(8) as usize].to_vec(),
        bits: n,
    };
    assert_eq!(name_of(&crs[0], cjtag::REGISTER), Some(12), "CNFG0");
    assert_eq!(
        value(&crs[0], cjtag::CR_LENGTH),
        Some(Value::Word {
            value: 32,
            bits: 32
        })
    );
    assert_eq!(value(&crs[0], cjtag::CR_VALUE), Some(bits(cnfg0, 32)));
    assert_eq!(
        value(&crs[0], cjtag::CR_MEANING),
        Some(Value::Text("class T4, revision 1".into()))
    );
    assert_eq!(name_of(&crs[1], cjtag::REGISTER), Some(1), "CGM");
    assert_eq!(value(&crs[1], cjtag::CR_VALUE), Some(bits(1, 1)), "written");
    assert!(crs.iter().all(|i| i.parent.is_some()));
    let effects: Vec<_> = (of(&h, c, &[cjtag::COMMAND]).iter().rev().take(2))
        .map(|i| value(i, cjtag::EFFECT))
        .collect();
    let text = |s: &str| Some(Value::Text(s.into()));
    assert_eq!(
        effects,
        [
            text("exit the control level (ECL)"),
            text("de-allocate every controller ID")
        ]
    );
    assert_eq!(h.store(c).counts.incomplete, 0);
}

#[test]
fn a_cr_scan_in_the_standard_protocol_has_no_data_on_tmsc() {
    let mut g = Gen::new();
    g.reset_to_idle();
    g.jscan(&enter_command_level());
    g.jscan(&command(9, 2));
    g.jscan(&dr_scan(32));
    let (h, [c, _]) = decode(&g, true);
    let cr = &of(&h, c, &[cjtag::CR_SCAN])[0];
    let why = Some(Value::Unknown {
        reason: cjtag::NOT_ON_TMSC,
        needs: None,
    });
    assert_eq!(
        value(cr, cjtag::CR_LENGTH),
        Some(Value::Word {
            value: 32,
            bits: 32
        })
    );
    for f in [cjtag::CR_TDI, cjtag::CR_TDO, cjtag::CR_VALUE] {
        assert_eq!(value(cr, f), why);
    }
}
