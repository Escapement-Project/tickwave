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

/// The JTAG clocks of one session from Run-Test/Idle, where the activation leaves the TAPC: IDCODE
/// through IR and DR, a 256-bit DR scan of text, and idle clocks. No Test-Logic-Reset: in the
/// Advanced Protocol it brings back the Standard Protocol (table 11-8).
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
    tms(&mut clocks, &[0, 0]);
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
    let mut items: Vec<Item> = (h.store(a).items().values())
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
        let packet = &h.store(c).items()[&id];
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

/// Each Scan Packet's TDO driver: its role, and the driver field where one is set.
fn drivers(
    h: &Host,
    c: AnalysisId,
) -> Vec<(Option<escapement_tickwave_decode::Role>, Option<Value>)> {
    (of(h, c, &[cjtag::PACKET]).iter())
        .filter_map(|p| {
            let f = p.field(cjtag::PACKET_TDO)?;
            Some((f.driver, value(p, cjtag::DRIVER)))
        })
        .collect()
}

#[test]
fn after_a_reset_the_counts_say_who_drives_tdo_whatever_the_setting() {
    // The activation: SGC 0, then MSC for controller zero selects one, this one, so in the Shift
    // states it drives alone, and elsewhere every controller drives a 1.
    for qualified in [true, false] {
        let (mut h, [c, _]) = host(synth(), 1000, None, qualified);
        h.run_until(|_| false).unwrap();
        let after = of(&h, c, &[cjtag::PACKET]).len() - session_clocks().len();
        let session = &drivers(&h, c)[after..];
        assert!(
            session.iter().all(|d| *d == (Some(cjtag::TARGET), None)),
            "{qualified}"
        );
    }
}

#[test]
fn a_control_path_dr_scan_s_tdo_is_driven_by_no_one_but_a_cr_scan_s_is() {
    let mut g = Gen::new();
    g.activate_oscan1();
    g.oscan1(&enter_command_level());
    g.oscan1_command(6, 0);
    let before = {
        let (h, [c, _]) = decode(&g, false);
        of(&h, c, &[cjtag::PACKET]).len()
    };
    g.oscan1_command(9, 2);
    g.oscan1_cr(0, 0x03C0_0714, 32);
    let (h, [c, _]) = decode(&g, false);
    let packets = of(&h, c, &[cjtag::PACKET]);
    let no_one = Some(Value::Name(2));
    // SCNS's command parts: each DR scan's Shift-DR packets, none driven.
    let command = &packets[before..before + command(9, 2).len()];
    assert!(command.iter().any(|p| value(p, cjtag::DRIVER) == no_one));
    // Its CR scan: every packet driven by the target.
    let cr = of(&h, c, &[cjtag::CR_SCAN]);
    let in_cr: Vec<_> = (packets.iter())
        .filter(|p| p.start >= cr[0].start && p.end <= cr[0].end)
        .collect();
    assert!(in_cr.len() >= 32);
    assert!(
        in_cr
            .iter()
            .all(|p| p.field(cjtag::PACKET_TDO).unwrap().driver == Some(cjtag::TARGET))
    );
}

#[test]
fn a_controller_not_the_one_candidate_does_not_drive_tdo_in_a_shift_state() {
    // SGC 0, then MSC for controller 5, which after a reset does nothing to this one but still
    // counts one candidate: in the Shift states this controller is inhibited. MSC adding a
    // candidate makes many, and every controller drives a 1.
    // Without MSC, SGC 0 alone counts no candidate, and every controller drives a 1.
    for (msc, adds, want) in [
        (
            true,
            false,
            Some(Value::Unknown {
                reason: cjtag::INHIBITED,
                needs: None,
            }),
        ),
        (true, true, None),
        (false, false, None),
    ] {
        let mut g = Gen::new();
        g.activate_oscan1();
        g.oscan1(&enter_command_level());
        g.oscan1_command(0, 0b00100);
        if msc {
            g.oscan1_command(7, 5);
        }
        if adds {
            g.oscan1_command(7, 0x10 | 6);
        }
        g.oscan1_command(0, 1);
        g.oscan1(&[0, 1, 0, 0]);
        for k in 0..8 {
            g.clock_oscan1(k == 7, false, true);
        }
        g.oscan1(&[1, 0]);
        let (h, [c, _]) = decode(&g, true);
        let shift = &drivers(&h, c);
        let last: Vec<_> = shift[shift.len() - 10..shift.len() - 2].to_vec();
        assert!(
            last.iter().all(|d| d.1 == want),
            "msc {msc} adds {adds}: {last:?}"
        );
    }

    // A reset counts many candidates, until MSC selects one: with this controller's SGC its
    // default, not known, whether it is that one is not known either.
    for (msc, want) in [
        (false, None),
        (
            true,
            Some(Value::Unknown {
                reason: cjtag::COUNTS_UNKNOWN,
                needs: None,
            }),
        ),
    ] {
        let mut g = Gen::new();
        g.bit(true, 10);
        let mut jscan = vec![1, 1, 1, 1, 1, 0];
        jscan.extend(enter_command_level());
        jscan.extend(command(3, 9));
        g.jscan(&jscan);
        g.check();
        if msc {
            g.oscan1_command(7, 5);
        }
        g.oscan1_command(0, 1);
        g.oscan1(&[0, 1, 0, 0]);
        for k in 0..8 {
            g.clock_oscan1(k == 7, false, true);
        }
        g.oscan1(&[1, 0]);
        let (h, [c, _]) = decode(&g, false);
        let shift = &drivers(&h, c);
        let last: Vec<_> = shift[shift.len() - 10..shift.len() - 2].to_vec();
        assert!(last.iter().all(|d| d.1 == want), "msc {msc}: {last:?}");
    }
}

#[test]
fn without_a_reset_the_counts_are_unknown_and_the_setting_decides() {
    for (qualified, want) in [
        (true, (Some(cjtag::TARGET), None)),
        (
            false,
            (
                None,
                Some(Value::Unknown {
                    reason: cjtag::COUNTS_UNKNOWN,
                    needs: None,
                }),
            ),
        ),
    ] {
        let mut g = Gen::new();
        g.select(0b1100, 0b1011, None);
        g.oscan1(&[1, 1, 0]);
        for &(m, i, o) in &session_clocks() {
            g.clock_oscan1(m, i, o);
        }
        let (h, [c, _]) = decode(&g, qualified);
        let all = drivers(&h, c);
        // In the Shift states; elsewhere every controller drives a 1, counts or not.
        assert!(all.contains(&want), "{qualified}");
        let unknown = all.iter().filter(|d| d.1.is_some()).count();
        assert_eq!(unknown > 0, !qualified, "{qualified}");
    }
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
fn a_long_selection_loads_the_scan_format_and_protection_makes_every_count_many() {
    let mut g = Gen::new();
    g.reset_to_idle();
    // Online in the Standard Protocol: a qualified selection escape, its bit a TMS 0. Long form,
    // PROTECT, SCNFMT OScan1.
    g.select(0b1100, 0b0100, Some(9 << 19));
    for &(m, i, o) in &session_clocks() {
        g.clock_oscan1(m, i, o);
    }
    let (h, [c, j]) = decode(&g, false);
    let sel = &of(&h, c, &[cjtag::SELECTION])[0];
    assert_eq!(name_of(sel, cjtag::FORM), Some(0), "long");
    assert_eq!(value(sel, cjtag::PROTECT), Some(Value::Bool(true)));
    assert_eq!(sel.seq(cjtag::SELECTION_BITS).len(), 32);
    for f in [cjtag::READY, cjtag::DELAY, cjtag::TRANSPORT_STATES] {
        assert!(matches!(value(sel, f), Some(Value::Word { value: 0, .. })));
    }
    assert!(of(&h, c, &[cjtag::OFFLINE]).is_empty());
    // Drive conflict protection makes every count many: every controller drives a 1, in the
    // Shift states too.
    assert!(
        drivers(&h, c)
            .iter()
            .all(|d| *d == (Some(cjtag::TARGET), None))
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
    // MCM for controller zero, as the project's probe sends it: CGM 1 opens the String Path.
    g.oscan1_command(6, 0);
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
    g.jscan(&command(6, 0));
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

// ---- The remaining limits: CGM, SUSPEND, alerts, start-up, Test-Logic-Reset -------------------

impl Gen {
    /// A Selection Alert: `ones` 1s, its 128 bits from the start bit, four padding bits (10.5.1).
    fn selection_alert(&mut self, ones: usize, wrong_bit: Option<u32>) {
        self.bits(0xFF, ones);
        let alert: u128 = 0x19BC_0EA2_E3DD_AFE9_8685_2D95_6209_F392;
        for k in 0..128 {
            self.bit((alert >> k & 1 == 1) != (wrong_bit == Some(k)), 0);
        }
        self.bits(0, 4);
    }

    /// A Selection Sequence's short form with no escape before it, and its Check Packet.
    fn sequence(&mut self, oac: u32, ec: u32) {
        self.bits(oac, 4);
        self.bits(ec, 4);
        self.check();
    }
}

/// Decodes with cJTAG's `start_up` setting offline.
fn decode_offline_start(g: &Gen) -> (Host, [AnalysisId; 2]) {
    let mut h = Host::new(HostOptions::default());
    let c = h
        .add(
            name("cjtag"),
            cjtag::definition(),
            InputSpec::Wire {
                source: Box::new(g.wires()),
                local: None,
            },
            Cjtag::factory(1000),
            &[cjtag::assume_qualified(true), cjtag::start_up(true)],
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
    h.run_until(|_| false).unwrap();
    for &x in &[c, j] {
        fits_its_rows(&h, x);
    }
    (h, [c, j])
}

/// Each CR scan's register, and whether it carries a value.
fn registers(h: &Host, c: AnalysisId) -> Vec<(Option<Value>, bool)> {
    (of(h, c, &[cjtag::CR_SCAN]).iter())
        .map(|i| {
            (
                value(i, cjtag::REGISTER),
                value(i, cjtag::CR_VALUE).is_some(),
            )
        })
        .collect()
}

fn reg(n: u16) -> Option<Value> {
    Some(Value::Name(n))
}

#[test]
fn an_scns_or_private_scnb_scan_reaches_the_bypass_bit_until_cgm_is_set() {
    let cnfg0 = 0x03C0_0714;
    let mut g = Gen::new();
    g.activate_oscan1();
    g.oscan1(&enter_command_level());
    // After a reset CGM is 0 (9.4.1.7): SCNS and a private SCNB reach the bypass bit, and a public
    // SCNB bit needs no CGM (table 9-10).
    g.oscan1_command(9, 2);
    g.oscan1_cr(0, 0, 32);
    g.oscan1_command(8, 16);
    g.oscan1_cr(1, 0, 1);
    g.oscan1_command(8, 2);
    g.oscan1_cr(0, 1, 1);
    // Until a CIDD, MCM for another controller does nothing; for controller zero it sets CGM.
    g.oscan1_command(6, 5);
    g.oscan1_command(9, 2);
    g.oscan1_cr(0, 0, 32);
    g.oscan1_command(6, 0);
    g.oscan1_command(9, 2);
    g.oscan1_cr(0, cnfg0, 32);
    let (h, [c, _]) = decode(&g, true);
    assert_eq!(
        registers(&h, c),
        [
            (reg(19), false),
            (reg(19), false),
            (reg(2), true),
            (reg(19), false),
            (reg(12), true),
        ]
    );
}

#[test]
fn cgm_follows_the_controller_id_and_is_unknown_where_the_id_is() {
    let mut g = Gen::new();
    g.activate_oscan1();
    g.oscan1(&enter_command_level());
    // STMC sets CGM; every ID de-allocated, MCM clears it.
    g.oscan1_command(0, 0b00111);
    g.oscan1_command(9, 2);
    g.oscan1_cr(0, 0, 32);
    g.oscan1_command(11, 0x10);
    g.oscan1_command(6, 0);
    g.oscan1_command(9, 2);
    g.oscan1_cr(0, 0, 32);
    // After a CIDA nothing on TMSC says which controller took the ID, so an MCM that leaves
    // others alone leaves CGM unknown, and the scan's register with it.
    g.oscan1_command(10, 0x13);
    g.oscan1_cr(0, 0, 36);
    g.oscan1_command(6, 0x13);
    g.oscan1_command(9, 2);
    g.oscan1_cr(0, 0, 32);
    // SCNB's public CGM bit stores it again.
    g.oscan1_command(8, 1);
    g.oscan1_cr(1, 0, 1);
    g.oscan1_command(9, 2);
    g.oscan1_cr(0, 0, 32);
    let (h, [c, _]) = decode(&g, true);
    let unknown = Some(Value::Unknown {
        reason: cjtag::CGM_UNKNOWN,
        needs: None,
    });
    let regs = registers(&h, c);
    assert_eq!(
        regs,
        [
            (reg(12), true),
            (reg(19), false),
            (reg(18), true),
            (unknown, true),
            (reg(1), true),
            (reg(12), true),
        ]
    );
    let cr = &of(&h, c, &[cjtag::CR_SCAN])[3];
    assert_eq!(
        value(cr, cjtag::CR_VALUE),
        Some(Value::Unknown {
            reason: cjtag::CGM_UNKNOWN,
            needs: None
        })
    );
}

#[test]
fn suspend_stops_control_levels_and_qualifies_escapes_until_eight_zero_bit_scans() {
    // SUSPEND ends the level, and no level locks while it holds; the eighth zero-bit scan ends it.
    let mut g = Gen::new();
    g.reset_to_idle();
    g.jscan(&enter_command_level());
    g.jscan(&command(0, 2));
    g.jscan(&enter_command_level());
    g.jscan(&[1, 1, 1, 1, 1, 0]);
    g.jscan(&enter_command_level());
    g.jscan(&command(0, 2));
    g.jscan(&ZBS.repeat(8));
    g.jscan(&enter_command_level());
    let (h, [c, _]) = decode(&g, true);
    let levels = of(&h, c, &[cjtag::LEVEL]);
    assert_eq!(
        levels.len(),
        3,
        "the first, after Test-Logic-Reset, after eight scans"
    );
    let suspend = &of(&h, c, &[cjtag::COMMAND])[0];
    assert_eq!(levels[0].end, suspend.end, "SUSPEND ends its level");

    // Suspended, the System Path is in use whatever the zero-bit scans: a deselection is taken.
    let mut g = Gen::new();
    g.reset_to_idle();
    g.jscan(&enter_command_level());
    g.jscan(&command(0, 2));
    g.jscan(&ZBS.repeat(2));
    g.bit(false, 4);
    let (h, [c, _]) = decode(&g, true);
    let deselection = &of(&h, c, &[cjtag::ESCAPE])[1];
    assert_eq!(name_of(deselection, cjtag::RESPONSE), Some(0));
    assert_eq!(of(&h, c, &[cjtag::OFFLINE]).len(), 1);
}

#[test]
fn a_selection_alert_while_offline_starts_a_selection_sequence() {
    // Seven 1s are too few to start the detector; one bit wrong is no alert.
    for (ones, wrong) in [(8, None), (8, Some(37)), (7, None)] {
        let mut g = Gen::new();
        g.reset_to_idle();
        g.bit(false, 4);
        g.selection_alert(ones, wrong);
        g.sequence(0b1100, 0b1000);
        for &(m, i, o) in &session_clocks() {
            g.clock_oscan1(m, i, o);
        }
        let (h, [c, j]) = decode(&g, true);
        let alerts = of(&h, c, &[cjtag::ALERT]);
        if (ones, wrong) != (8, None) {
            assert!(alerts.is_empty(), "{ones} 1s, bit {wrong:?} wrong");
            assert!(of(&h, c, &[cjtag::SELECTION]).is_empty());
            continue;
        }
        assert_eq!(alerts.len(), 1);
        assert_eq!(name_of(&alerts[0], cjtag::ALERT_IS), Some(0));
        let sel = &of(&h, c, &[cjtag::SELECTION])[0];
        assert_eq!(
            alerts[0].end,
            Some(sel.start),
            "the alert and its padding, then the sequence"
        );
        assert_eq!(name_of(sel, cjtag::OUTCOME), Some(0));
        let offline = &of(&h, c, &[cjtag::OFFLINE])[0];
        assert_eq!(offline.end, Some(alerts[0].start));
        assert!(idcode_read(&h, j));
    }
}

#[test]
fn control_level_six_then_run_test_idle_is_a_deselection_alert() {
    // One period in Run-Test/Idle parks the TAPC in Select-DR-Scan; more, in Run-Test/Idle (10.6.1).
    for (after, ec, outcome) in [(0, 0b1000, 0), (1, 0b1001, 0), (1, 0b1000, 2)] {
        let mut g = Gen::new();
        g.activate_oscan1();
        let mut level6 = ZBS.repeat(6);
        level6.extend(dr_scan(1));
        g.oscan1(&level6);
        g.oscan1(&[after]);
        g.oscan1(&[0, 0]);
        g.select(0b1100, ec, None);
        let (h, [c, _]) = decode(&g, true);
        let alerts = of(&h, c, &[cjtag::ALERT]);
        assert_eq!(alerts.len(), 1);
        assert_eq!(name_of(&alerts[0], cjtag::ALERT_IS), Some(1));
        let by: Vec<_> = (of(&h, c, &[cjtag::OFFLINE]).iter())
            .map(|o| name_of(o, cjtag::OFFLINE_BY))
            .collect();
        let expected = if outcome == 0 {
            vec![Some(3)]
        } else {
            vec![Some(3), Some(2)]
        };
        assert_eq!(by, expected);
        let sel = &of(&h, c, &[cjtag::SELECTION])[0];
        assert_eq!(
            name_of(sel, cjtag::OUTCOME),
            Some(outcome),
            "parked after {after}"
        );
        let level = &of(&h, c, &[cjtag::LEVEL])[1];
        assert_eq!(
            level.end,
            Some(alerts[0].start),
            "the level ends where the alert begins"
        );
    }

    // A CNFG0 read whose DSEL_ALERTS is 0, and SEL_ALERTS 1: level six then Run-Test/Idle leaves it
    // Online.
    let mut g = Gen::new();
    g.activate_oscan1();
    g.oscan1(&enter_command_level());
    g.oscan1_command(6, 0);
    g.oscan1_command(9, 2);
    g.oscan1_cr(0, 0x43C0_0714, 32);
    g.oscan1_command(0, 1);
    let mut level6 = ZBS.repeat(6);
    level6.extend(dr_scan(1));
    g.oscan1(&level6);
    g.oscan1(&[1, 0, 0]);
    let (h, [c, _]) = decode(&g, true);
    assert!(of(&h, c, &[cjtag::ALERT, cjtag::OFFLINE]).is_empty());
}

#[test]
fn offline_at_start_up_a_selection_needs_its_preamble() {
    // 30 alternating bits qualify the selection; 20 do not, nor do 30 with an escape among them.
    for (alternating, escape_at) in [(30, None), (20, None), (30, Some(12))] {
        let mut g = Gen::new();
        g.bit(true, 10);
        g.bits(0, 10);
        g.bit(false, 6);
        for k in 0..alternating {
            g.bit(k % 2 == 0, if escape_at == Some(k) { 2 } else { 0 });
        }
        g.select(0b1100, 0b1000, None);
        for &(m, i, o) in &session_clocks() {
            g.clock_oscan1(m, i, o);
        }
        let (h, [c, j]) = decode_offline_start(&g);
        let responses: Vec<_> = (of(&h, c, &[cjtag::ESCAPE]).iter())
            .map(|e| name_of(e, cjtag::RESPONSE))
            .collect();
        let offline = of(&h, c, &[cjtag::OFFLINE]);
        assert_eq!(name_of(&offline[0], cjtag::OFFLINE_BY), Some(4));
        if (alternating, escape_at) != (30, None) {
            assert_eq!(
                responses,
                [None, Some(1), Some(1)],
                "no preamble, no selection"
            );
            assert!(of(&h, c, &[cjtag::SELECTION]).is_empty());
            continue;
        }
        assert_eq!(responses, [None, Some(1), Some(0)]);
        let sel = &of(&h, c, &[cjtag::SELECTION])[0];
        assert_eq!(
            name_of(sel, cjtag::OUTCOME),
            Some(0),
            "the TAPC is in Run-Test/Idle"
        );
        // JTAG learns the TAP state from TMS bits, and the controller moved its TAPC with none.
        assert_eq!(of(&h, c, &[cjtag::PACKET]).len(), session_clocks().len());
        assert!(!idcode_read(&h, j));
    }

    // A CNFG0 read whose DAS bit is 1 says the controller starts Offline, setting or not.
    let mut g = Gen::new();
    g.activate_oscan1();
    g.oscan1(&enter_command_level());
    g.oscan1_command(6, 0);
    g.oscan1_command(9, 2);
    g.oscan1_cr(0, 0x03C8_0714, 32);
    g.bit(true, 10);
    g.bits(0, 10);
    let (h, [c, _]) = decode(&g, true);
    let by: Vec<_> = (of(&h, c, &[cjtag::OFFLINE]).iter())
        .map(|o| name_of(o, cjtag::OFFLINE_BY))
        .collect();
    assert_eq!(by, [Some(4)]);

    // A Selection Alert works at start-up too.
    let mut g = Gen::new();
    g.bit(true, 10);
    g.selection_alert(8, None);
    g.sequence(0b1100, 0b1000);
    let (h, [c, _]) = decode_offline_start(&g);
    assert_eq!(of(&h, c, &[cjtag::ALERT]).len(), 1);
    assert_eq!(
        name_of(&of(&h, c, &[cjtag::SELECTION])[0], cjtag::OUTCOME),
        Some(0)
    );
}

#[test]
fn test_logic_reset_brings_back_the_standard_protocol() {
    let mut g = Gen::new();
    g.activate_oscan1();
    g.oscan1(&[1, 1, 1]);
    g.jscan(&[0, 1, 0]);
    let (h, [c, _]) = decode(&g, true);
    let format = &of(&h, c, &[cjtag::FORMAT])[0];
    let packets = of(&h, c, &[cjtag::PACKET]);
    assert_eq!(
        format.end,
        packets.last().unwrap().end,
        "OScan1 to the packet entering it"
    );
    let tms: usize = (of(&h, c, &[cjtag::JSCAN]).iter())
        .filter(|i| i.start >= format.end.unwrap())
        .map(|i| i.seq(cjtag::JSCAN_TMS).len())
        .sum();
    assert_eq!(tms, 3);
}

#[test]
fn a_format_from_a_selection_without_a_reset_does_not_know_its_sampling_edge() {
    let mut g = Gen::new();
    g.select(0b1100, 0b1000, None);
    g.oscan1(&[0, 0]);
    let (h, [c, _]) = decode(&g, true);
    let format = &of(&h, c, &[cjtag::FORMAT])[0];
    assert_eq!(
        value(format, cjtag::SAMPLING),
        Some(Value::Unknown {
            reason: cjtag::SREDGE_UNKNOWN,
            needs: None
        })
    );
}

// ---- Scan Selection Directives ------------------------------------------------------------------

/// An SSD's directive and payload, first bit first (table 20-14).
fn ssd_bits(directive: [u8; 3], payload: u64, n: usize) -> Vec<u8> {
    let mut v = directive.to_vec();
    v.extend((0..n).map(|k| (payload >> k & 1) as u8));
    v
}
const DA: [u8; 3] = [0, 0, 0];
const SA: [u8; 3] = [0, 0, 1];
const SOC: [u8; 3] = [0, 1, 0];
const SOT: [u8; 3] = [0, 1, 1];

impl Gen {
    /// OScan1 activation, then SSDE = 1 at control level two, back in Run-Test/Idle.
    fn ssde_on(&mut self) {
        self.activate_oscan1();
        self.oscan1(&enter_command_level());
        self.oscan1_command(0, 0b10001);
        self.oscan1_command(0, 1);
    }

    /// SSD bits as TDI with TMS 0, then the bit after them, TDI 1 and TMS `exit`.
    fn ssd(&mut self, bits: &[u8], exit: bool) {
        for &b in bits {
            self.clock_oscan1(false, b == 1, true);
        }
        self.clock_oscan1(exit, true, true);
    }
}

/// Each SSD's directive, outcome and SSD state after it.
fn ssds(h: &Host, c: AnalysisId) -> Vec<(Option<u16>, Option<u16>, Option<Value>)> {
    (of(h, c, &[cjtag::SSD]).iter())
        .map(|i| {
            (
                name_of(i, cjtag::DIRECTIVE),
                name_of(i, cjtag::SSD_OUTCOME),
                value(i, cjtag::SSD_STATE),
            )
        })
        .collect()
}

fn ssd_state(n: u16) -> Option<Value> {
    Some(Value::Name(n))
}

#[test]
fn ssds_in_run_test_idle_and_pause_dr_set_the_ssd_state() {
    // SSDE 0, as after a reset: TDI 0 bits in Run-Test/Idle are no SSD.
    let mut g = Gen::new();
    g.activate_oscan1();
    g.ssd(&DA, false);
    let (h, [c, _]) = decode(&g, true);
    assert!(of(&h, c, &[cjtag::SSD]).is_empty());

    // In Run-Test/Idle any SSD leaves Select All; in Pause-DR, Deselect All leaves SSD_DA_DR and
    // Select All brings SSD_SA back. An SSD's item spans its own bits.
    let mut g = Gen::new();
    g.ssde_on();
    g.ssd(&DA, false);
    g.ssd(&SA, false);
    g.oscan1(&[1, 0, 1, 0]);
    g.ssd(&DA, false);
    g.ssd(&SA, false);
    let (h, [c, _]) = decode(&g, true);
    assert_eq!(
        ssds(&h, c),
        [
            (Some(0), Some(0), ssd_state(0)),
            (Some(1), Some(0), ssd_state(0)),
            (Some(0), Some(0), ssd_state(2)),
            (Some(1), Some(0), ssd_state(0)),
        ]
    );
    let items = of(&h, c, &[cjtag::SSD]);
    let packets = of(&h, c, &[cjtag::PACKET]);
    let first = packets
        .iter()
        .position(|p| p.start == items[0].start)
        .unwrap();
    assert_eq!(items[0].end, packets[first + 2].end, "three bits");
}

#[test]
fn an_ssd_in_pause_dr_decides_whether_an_escape_counts() {
    // Deselect All in Pause-DR: a deselection escape once the Delayed SSD State has it, a TCK
    // after, is not qualified, so it is the packet's bit; after Select All it is taken.
    for (then, offline) in [(DA, false), (SA, true)] {
        let mut g = Gen::new();
        g.ssde_on();
        g.oscan1(&[1, 0, 1, 0]);
        g.ssd(&then, false);
        // TDI 1: a TDI 0 here would start another SSD.
        g.clock_oscan1(false, true, true);
        g.bit(false, 4);
        g.bit(false, 0);
        g.bit(true, 0);
        let (h, [c, _]) = decode(&g, true);
        assert_eq!(of(&h, c, &[cjtag::OFFLINE]).len(), usize::from(offline));
    }
}

#[test]
fn a_select_one_ssd_compares_the_controller_id() {
    // After a reset the controller ID is zero: a CID of 0 selects it, 5 does not; a TCA is never
    // known to match, so the SSD state after it is unknown, and an escape it decides ends the
    // framing.
    for (bits, state) in [
        (ssd_bits(SOC, 0, 4), ssd_state(5)),
        (ssd_bits(SOC, 5, 4), ssd_state(3)),
        (
            ssd_bits(SOT, 0x1_2345_6789, 35),
            Some(Value::Unknown {
                reason: cjtag::SSD_STATE_UNKNOWN,
                needs: None,
            }),
        ),
    ] {
        let mut g = Gen::new();
        g.ssde_on();
        g.oscan1(&[1, 1, 0, 1, 0]);
        g.ssd(&bits, false);
        g.clock_oscan1(false, true, true);
        g.bit(false, 4);
        g.oscan1(&[0, 0]);
        let (h, [c, _]) = decode(&g, true);
        let got = ssds(&h, c);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].2, state);
        let ssd = &of(&h, c, &[cjtag::SSD])[0];
        let payload = if bits[1] == 1 && bits[2] == 1 {
            value(ssd, cjtag::SSD_TCA)
        } else {
            value(ssd, cjtag::SSD_CID)
        };
        let want = (bits[3..].iter().enumerate()).fold(0u64, |v, (k, &b)| v | u64::from(b) << k);
        assert!(matches!(payload, Some(Value::Word { value, .. }) if value == want));
        if state
            .as_ref()
            .is_some_and(|s| matches!(s, Value::Unknown { .. }))
        {
            let escape = &of(&h, c, &[cjtag::ESCAPE])[1];
            assert_eq!(name_of(escape, cjtag::RESPONSE), Some(3));
            let cut = of(&h, c, &[cjtag::FORMAT]);
            assert_eq!(
                cut.last().unwrap().status,
                Some(Status::Incomplete(cjtag::SSD_UNKNOWN))
            );
        }
    }
}

#[test]
fn an_ssd_is_aborted_by_tms_1_and_permitted_by_the_delayed_state() {
    // TMS 1 in its bits, or in the one after them: aborted, no effect.
    let mut g = Gen::new();
    g.ssde_on();
    g.oscan1(&[1, 0, 1, 0]);
    g.clock_oscan1(false, false, true);
    g.clock_oscan1(true, false, true);
    g.oscan1(&[1, 0]);
    g.ssd(&DA, true);
    let (h, [c, _]) = decode(&g, true);
    let got: Vec<_> = ssds(&h, c).iter().map(|s| s.1).collect();
    assert_eq!(got, [Some(1), Some(1)]);

    // After Deselect All in Pause-DR, an SSD in Run-Test/Idle is not processed (table 20-16).
    let mut g = Gen::new();
    g.ssde_on();
    g.oscan1(&[1, 0, 1, 0]);
    g.ssd(&DA, false);
    g.oscan1(&[1, 1, 0]);
    g.ssd(&SA, false);
    let (h, [c, _]) = decode(&g, true);
    assert_eq!(of(&h, c, &[cjtag::SSD]).len(), 1);
}

#[test]
fn an_executed_ssd_zeroes_the_zero_bit_count() {
    // One zero-bit scan, an SSD, one more, then a shifting scan: control level one, not two.
    for (with_ssd, level) in [(true, 1), (false, 2)] {
        let mut g = Gen::new();
        g.ssde_on();
        g.oscan1(&ZBS);
        if with_ssd {
            g.ssd(&SA, false);
        }
        g.oscan1(&ZBS);
        g.oscan1(&dr_scan(1));
        let (h, [c, _]) = decode(&g, true);
        let levels = of(&h, c, &[cjtag::LEVEL]);
        let last = levels.last().unwrap();
        assert!(matches!(
            value(last, cjtag::CONTROL_LEVEL),
            Some(Value::Word { value, .. }) if value == level
        ));
    }
}

#[test]
fn in_jscan3_with_ssde_set_an_escape_s_qualification_is_unknown() {
    let mut g = Gen::new();
    g.reset_to_idle();
    g.jscan(&enter_command_level());
    g.jscan(&command(3, 3));
    g.jscan(&command(0, 0b10001));
    g.jscan(&command(0, 1));
    g.jscan(&[0, 0]);
    g.bit(false, 4);
    g.jscan(&[0, 1, 1]);
    let (h, [c, _]) = decode(&g, true);
    let escape = &of(&h, c, &[cjtag::ESCAPE])[1];
    assert_eq!(name_of(escape, cjtag::RESPONSE), Some(3));
    assert!(of(&h, c, &[cjtag::OFFLINE]).is_empty());

    // In JScan2 SSDs are not detected (table 23-10): the same escape is taken.
    let mut g = Gen::new();
    g.reset_to_idle();
    g.jscan(&enter_command_level());
    g.jscan(&command(3, 2));
    g.jscan(&command(0, 0b10001));
    g.jscan(&command(0, 1));
    g.jscan(&[0, 0]);
    g.bit(false, 4);
    let (h, [c, _]) = decode(&g, true);
    assert_eq!(of(&h, c, &[cjtag::OFFLINE]).len(), 1);
}

#[test]
fn framing_lost_leaves_cgm_unknown_until_a_reset() {
    // CGM set, then OScan3, which is not framed: commands may pass unseen, so after the selection
    // that frames it again, an SCNS scan's register is unknown.
    let mut g = Gen::new();
    g.activate_oscan1();
    g.oscan1(&enter_command_level());
    g.oscan1_command(6, 0);
    g.oscan1_command(3, 11);
    g.oscan1(&[0, 0, 0]);
    // Parked in Pause-DR, a single state, then to Run-Test/Idle.
    g.select(0b1100, 0b1011, None);
    g.oscan1(&[1, 1, 0]);
    g.oscan1(&enter_command_level());
    g.oscan1_command(9, 2);
    g.oscan1_cr(0, 0, 32);
    // Nor is the controller ID known, so an MCM for ID zero leaves CGM unknown too.
    g.oscan1_command(6, 0);
    g.oscan1_command(9, 2);
    g.oscan1_cr(0, 0, 32);
    let (h, [c, _]) = decode(&g, true);
    let unknown = Some(Value::Unknown {
        reason: cjtag::CGM_UNKNOWN,
        needs: None,
    });
    let crs = of(&h, c, &[cjtag::CR_SCAN]);
    assert_eq!(crs.len(), 2);
    assert!(crs.iter().all(|cr| value(cr, cjtag::REGISTER) == unknown));
}

#[test]
fn drive_protection_makes_the_counts_many_before_run_test_idle_takes_them_again() {
    // One candidate counted and this controller idle: in Shift-DR it would not drive. A protected
    // selection parked in Pause-DR, then straight to Shift-DR: many potential members, so every
    // controller drives a 1.
    let mut g = Gen::new();
    g.activate_oscan1();
    g.oscan1(&enter_command_level());
    g.oscan1_command(0, 0b00100);
    g.oscan1_command(7, 5);
    g.oscan1_command(0, 1);
    for _ in 0..2 {
        g.clock_oscan1(false, true, true);
    }
    g.oscan1(&[1, 0, 1, 0]);
    g.select(0b1100, 0b1111, None);
    g.oscan1(&[1, 0]);
    for k in 0..8 {
        g.clock_oscan1(k == 7, false, true);
    }
    g.oscan1(&[1, 0]);
    let (h, [c, _]) = decode(&g, false);
    let sel = &of(&h, c, &[cjtag::SELECTION])[0];
    assert_eq!(name_of(sel, cjtag::OUTCOME), Some(0));
    let shift = &drivers(&h, c);
    let last: Vec<_> = shift[shift.len() - 10..shift.len() - 2].to_vec();
    assert!(
        last.iter().all(|d| *d == (Some(cjtag::TARGET), None)),
        "{last:?}"
    );
}
