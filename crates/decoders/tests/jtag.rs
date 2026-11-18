//! JTAG on synthetic wires.

mod common;

use common::*;
use escapement_tickwave_decode::{
    AnalysisId, CheckpointError, Counts, Element, Host, InputSpec, Item, KindId, Span, Status,
    Tick, Value, name,
};
use escapement_tickwave_decoders::jtag;
use std::sync::atomic::Ordering::Relaxed;

fn all() -> Vec<Span> {
    vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]
}

/// A JTAG session as clocks, each with the tick it starts at and its TMS, TDI and TDO. They change
/// at a clock's start; TCK rises half a period later and falls at its end.
#[derive(Clone, Debug)]
struct Session {
    t: Tick,
    period: Tick,
    clocks: Vec<(Tick, [bool; 3])>,
    /// Applied to every line.
    gaps: Vec<Span>,
    end: Tick,
}

impl Session {
    fn new(t: Tick, period: Tick) -> Self {
        Self {
            t,
            period,
            clocks: vec![],
            gaps: vec![],
            end: 0,
        }
    }

    fn clock(&mut self, tms: bool, tdi: bool, tdo: bool) {
        self.clocks.push((self.t, [tms, tdi, tdo]));
        self.t += self.period;
    }

    fn tms(&mut self, seq: &[u8]) {
        for &b in seq {
            self.clock(b == 1, true, true);
        }
    }

    /// Shifts `tdi` and `tdo`, leaving Shift on the last bit. `pause` leaves after that bit for
    /// Pause, waits there and comes back. `stop` ends without leaving Shift.
    fn shift(&mut self, tdi: &[bool], tdo: &[bool], pause: Option<usize>, stop: Option<usize>) {
        let n = stop.unwrap_or(tdi.len());
        for k in 0..n {
            let last = stop.is_none() && k == tdi.len() - 1;
            let p = pause == Some(k);
            self.clock(last || p, tdi[k], tdo[k]);
            if p {
                for _ in 0..20 {
                    self.clock(false, true, true);
                }
                self.clock(true, true, true);
                self.clock(false, true, true);
            }
        }
    }

    fn rise(&self, k: usize) -> Tick {
        self.clocks[k].0 + self.period / 2
    }

    /// TCK, TMS, TDI and TDO, all low before the first clock.
    fn wires(&self) -> Wires {
        let mut lines = vec![
            Line::default(),
            Line::default(),
            Line::default(),
            Line::default(),
        ];
        let mut level = [false; 4];
        let mut put = |ch: usize, v: bool, at: Tick| {
            if level[ch] != v {
                lines[ch].edges.push(at);
                level[ch] = v;
            }
        };
        for &(t, v) in &self.clocks {
            for (k, &b) in v.iter().enumerate() {
                put(k + 1, b, t);
            }
            put(0, true, t + self.period / 2);
            put(0, false, t + self.period);
        }
        for l in &mut lines {
            l.gaps = self.gaps.clone();
        }
        Wires {
            lines,
            end: self.end,
            reversed: false,
        }
    }
}

fn bits(v: u64, n: usize) -> Vec<bool> {
    (0..n).map(|k| v >> k & 1 == 1).collect()
}

fn byte_bits(bytes: &[u8]) -> Vec<bool> {
    (bytes.iter())
        .flat_map(|b| (0..8).map(move |k| b >> k & 1 == 1))
        .collect()
}

const IDCODE: u32 = 0x4BA0_0477;

/// What a synthetic session holds.
struct Info {
    payload_tdi: Vec<u8>,
    payload_tdo: Vec<u8>,
    gap: Span,
    /// The clock that starts the long scan's payload.
    long_from: usize,
}

/// The sizes a session is built with: the long scan's bytes, the gap-cut scan's bits and where the
/// gap cuts it, the bits before the end cuts the last scan, and the idle stretches between scans.
#[derive(Clone, Copy)]
struct Sizes {
    long_bytes: usize,
    cut_bits: usize,
    gap_after: usize,
    last_bits: usize,
    idle: Tick,
}

/// The full-size session: a 16,384-bit DR scan paused halfway, a gap after 1,500 bits of a
/// 4,096-bit scan, and the end of the recording 3,000 bits into the last.
const FULL: Sizes = Sizes {
    long_bytes: 2048,
    cut_bits: 4096,
    gap_after: 1500,
    last_bits: 3000,
    idle: 20_000,
};
/// The same session, small enough to read a tick at a time.
const SMALL: Sizes = Sizes {
    long_bytes: 40,
    cut_bits: 512,
    gap_after: 150,
    last_bits: 300,
    idle: 2_000,
};

fn long_text(n: usize) -> Vec<u8> {
    let mut s = String::new();
    let mut line = 0;
    while s.len() < n {
        s.push_str(&format!(
            "{line:04} tickwave: a long DR scan, read in chunks.\n"
        ));
        line += 1;
    }
    s.into_bytes()[..n].to_vec()
}

/// At 10 MHz with 1 ns ticks: mid-session, then a reset, IDCODE through IR and DR, a long DR scan
/// paused halfway, a DR scan a gap cuts, a reset, and a DR scan the end of the recording cuts.
fn session(z: Sizes) -> (Session, Info) {
    let mut s = Session::new(5_000, 100);
    // Mid-session: the TAP state is not known yet.
    s.tms(&[0, 0, 1, 0, 1, 1, 0, 0, 0, 1, 0]);
    s.tms(&[1, 1, 1, 1, 1, 0, 0, 0]);
    s.tms(&[1, 1, 0, 0]);
    s.shift(&bits(0b1110, 4), &bits(0b0001, 4), None, None);
    s.tms(&[1, 0, 1, 0, 0]);
    s.shift(&bits(0, 32), &bits(IDCODE.into(), 32), None, None);
    s.tms(&[1, 0, 0, 0]);
    s.t += z.idle;
    let payload_tdi = long_text(z.long_bytes);
    let payload_tdo: Vec<u8> = (0..z.long_bytes as u32)
        .map(|i| (i * 37 + 11) as u8)
        .collect();
    s.tms(&[1, 1, 0, 0]);
    s.shift(&bits(0b1010, 4), &bits(0b0001, 4), None, None);
    s.tms(&[1, 0, 1, 0, 0]);
    let long_from = s.clocks.len();
    let half = z.long_bytes * 4 - 1;
    s.shift(
        &byte_bits(&payload_tdi),
        &byte_bits(&payload_tdo),
        Some(half),
        None,
    );
    s.tms(&[1, 0, 0, 0]);
    s.t += z.idle;
    s.tms(&[1, 0, 0]);
    let g0 = s.t + z.gap_after as Tick * s.period + 30;
    let gap = Span {
        start: g0,
        end: g0 + z.idle,
    };
    s.gaps.push(gap);
    let zeros = vec![false; z.cut_bits];
    s.shift(&zeros, &vec![true; z.cut_bits], None, None);
    s.tms(&[1, 0, 0, 0]);
    s.t += z.idle;
    s.tms(&[1, 1, 1, 1, 1, 0]);
    s.tms(&[1, 1, 0, 0]);
    s.shift(&bits(0b1010, 4), &bits(0b0001, 4), None, None);
    s.tms(&[1, 0, 1, 0, 0]);
    let last = byte_bits(&payload_tdo);
    s.shift(&last, &last, None, Some(z.last_bits));
    s.end = s.t + 20;
    (
        s,
        Info {
            payload_tdi,
            payload_tdo,
            gap,
            long_from,
        },
    )
}

fn host(w: Wires, window: Tick, local: Option<Tick>, has_tdo: bool) -> (Host, AnalysisId) {
    let mut h = Host::default();
    let j = h
        .add(
            name("jtag"),
            jtag::definition(),
            InputSpec::Wire {
                source: Box::new(w),
                local,
            },
            jtag::Jtag::factory(has_tdo, window),
            all(),
        )
        .unwrap();
    (h, j)
}

fn decoded(w: Wires, window: Tick) -> Counts {
    let (mut h, j) = host(w, window, None, true);
    h.run_until(|_| false).unwrap();
    assert!(h.finished(j).is_some());
    h.store(j).counts
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

fn bytes_of(els: &[Element]) -> Vec<u8> {
    els.chunks(8)
        .map(|c| (c.iter().enumerate()).fold(0, |b, (k, e)| b | e.value << k))
        .collect()
}

fn word32(n: u64) -> Option<Value> {
    Some(Value::Word { value: n, bits: 32 })
}

const SCANS: [KindId; 2] = [jtag::IR_SCAN, jtag::DR_SCAN];

#[test]
fn five_tms_highs_reach_reset_from_anywhere() {
    let mut set = 0xFFFF;
    for _ in 0..5 {
        set = jtag::step(set, true);
    }
    assert_eq!(set, 1, "Test-Logic-Reset alone");
    assert_ne!(jtag::step(0xFFFF, false).count_ones(), 1);
}

#[test]
fn the_session_decodes_its_scans_states_and_paths() {
    let (s, info) = session(FULL);
    let (mut h, j) = host(s.wires(), 50_000, None, true);
    h.run_until(|_| false).unwrap();
    assert_eq!(h.finished(j), Some(s.end));
    let scans = of(&h, j, &SCANS);
    let got: Vec<_> = (scans.iter())
        .map(|i| (i.id.kind, value(i, jtag::LENGTH), i.status))
        .collect();
    let complete = Some(Status::Complete);
    assert_eq!(
        got,
        vec![
            (jtag::IR_SCAN, word32(4), complete),
            (jtag::DR_SCAN, word32(32), complete),
            (jtag::IR_SCAN, word32(4), complete),
            (jtag::DR_SCAN, word32(16_384), complete),
            (
                jtag::DR_SCAN,
                word32(1_500),
                Some(Status::Incomplete(jtag::GAP_CUT))
            ),
            (jtag::IR_SCAN, word32(4), complete),
            (
                jtag::DR_SCAN,
                word32(3_000),
                Some(Status::Incomplete(jtag::ENDS_MID_SCAN))
            ),
        ]
    );
    assert_eq!(
        value(&scans[1], jtag::TDO_VALUE),
        Some(Value::Bits {
            bytes: IDCODE.to_le_bytes().to_vec(),
            bits: 32
        })
    );
    assert_eq!(scans[4].end, Some(info.gap.start), "the gap cuts it");
    let long = &scans[3];
    assert_eq!(bytes_of(long.seq(jtag::TDI_BITS)), info.payload_tdi);
    assert_eq!(bytes_of(long.seq(jtag::TDO_BITS)), info.payload_tdo);
    assert_eq!(
        value(long, jtag::TDI_HEAD),
        Some(Value::Bits {
            bytes: info.payload_tdi[..16].to_vec(),
            bits: 128
        })
    );
    assert_eq!(value(long, jtag::TDI_VALUE), None, "too long to be a field");
    // The pause leaves a hole between bits 8191 and 8192; the cells either side are contiguous.
    let tdi = long.seq(jtag::TDI_BITS);
    assert!(tdi[8192].cell.start - tdi[8191].cell.end > 2_000);
    assert_eq!(tdi[100].cell.end, tdi[101].cell.start);
    assert_eq!(
        tdi[100].at,
        escapement_tickwave_decode::Observation::Wire(s.rise(info.long_from + 100))
    );
    // The state is unknown at the start and again after the gap, until a reset.
    let states = of(&h, j, &[jtag::STATE]);
    let unknown: Vec<_> = (states.iter())
        .filter_map(|i| match value(i, jtag::TAP_STATE) {
            Some(Value::Unknown { reason, .. }) => Some((i.start, reason)),
            _ => None,
        })
        .collect();
    assert_eq!(
        unknown,
        vec![(0, jtag::MID_SESSION), (info.gap.end, jtag::LOST_STATE)]
    );
    // One-clock states travel as paths: no known state item lasts one clock but Shift and
    // Test-Logic-Reset.
    let one_clock: Vec<_> = (states.iter())
        .filter(|i| i.end.unwrap() - i.start <= 100)
        .filter(|i| !matches!(value(i, jtag::TAP_STATE), Some(Value::Name(0 | 4 | 11))))
        .map(|i| i.start)
        .collect();
    assert_eq!(one_clock, Vec::<Tick>::new());
    let to_shift_ir: Vec<Vec<u8>> = (of(&h, j, &[jtag::PATH]).iter())
        .filter(|i| value(i, jtag::TO) == Some(Value::Name(11)))
        .map(|i| i.seq(jtag::VIA).iter().map(|e| e.value).collect())
        .collect();
    assert!(to_shift_ir.contains(&vec![2, 9, 10]), "{to_shift_ir:?}");
    assert!(h.lost(j).contains(&info.gap));
}

#[test]
fn window_size_does_not_change_the_output() {
    let (s, _) = session(SMALL);
    let want = decoded(s.wires(), 1 << 40);
    for window in [1, 7, 64, 333, 5000] {
        assert_eq!(decoded(s.wires(), window), want, "windows of {window}");
    }
    let (s, _) = session(FULL);
    let want = decoded(s.wires(), 1 << 40);
    assert_eq!(decoded(s.wires(), 4999), want, "the full session");
}

#[test]
fn a_checkpoint_restores_only_under_the_settings_it_was_taken_with() {
    let (s, _) = session(SMALL);
    let make = |w: Wires, at_fall: bool| {
        let mut h = Host::default();
        let j = h
            .add(
                name("jtag"),
                jtag::definition(),
                InputSpec::Wire {
                    source: Box::new(w),
                    local: None,
                },
                jtag::Jtag::factory_with(true, 64, at_fall),
                all(),
            )
            .unwrap();
        (h, j)
    };
    let (mut h, j) = make(s.wires(), false);
    while h.consumed(j) < 500 {
        h.step().unwrap();
    }
    let cut = h.checkpoint(j).unwrap();
    assert!(make(s.wires(), false).0.restore(&cut).is_ok());
    assert!(matches!(
        make(s.wires(), true).0.restore(&cut),
        Err(CheckpointError::Incompatible)
    ));
}

/// The session with the edge that starts clock `k` on line `port` moved to the clock's rise, and
/// the session as it would be had that line held its level through clock `k`.
fn moved_and_held(s: &Session, k: usize, port: usize) -> (Wires, Wires) {
    let mut moved = s.wires();
    let edges = &mut moved.lines[port].edges;
    let e = edges.iter().position(|&x| x == s.clocks[k].0).unwrap();
    edges[e] = s.rise(k);
    let mut held = s.clone();
    held.clocks[k].1[port - 1] = s.clocks[k - 1].1[port - 1];
    (moved, held.wires())
}

#[test]
fn a_change_at_a_rise_counts_after_it_in_either_order() {
    let (s, info) = session(SMALL);
    let k0 = info.long_from;
    // A TDI and a TDO bit of the long scan that differ from the bits before them, and the TMS
    // high that leaves the scan.
    let differs = |port: usize| {
        (k0 + 1..k0 + 300)
            .find(|&k| s.clocks[k].1[port - 1] != s.clocks[k - 1].1[port - 1])
            .unwrap()
    };
    let tms = (k0..).find(|&k| s.clocks[k].1[0]).unwrap();
    for (port, k) in [(2, differs(2)), (3, differs(3)), (1, tms)] {
        let (mut moved, held) = moved_and_held(&s, k, port);
        let want = decoded(held, 100);
        assert_eq!(decoded(moved.clone(), 100), want, "port {port}");
        moved.reversed = true;
        assert_eq!(decoded(moved, 100), want, "port {port}, reversed");
    }
}

#[test]
fn a_gap_is_lost_input_that_cuts_the_scan_and_the_state() {
    let (s, info) = session(SMALL);
    let (mut h, j) = host(s.wires(), 64, None, true);
    h.run_until(|_| false).unwrap();
    assert_eq!(h.lost(j), &[info.gap]);
    let scans = of(&h, j, &SCANS);
    let cut = scans
        .iter()
        .find(|i| i.status == Some(Status::Incomplete(jtag::GAP_CUT)))
        .unwrap();
    assert_eq!(cut.end, Some(info.gap.start));
    assert_eq!(value(cut, jtag::LENGTH), word32(150));
    assert!(
        (of(&h, j, &[jtag::STATE, jtag::PATH]).iter())
            .all(|i| i.end.unwrap() <= info.gap.start || i.start >= info.gap.end),
        "nothing spans the gap"
    );
    let after = of(&h, j, &[jtag::STATE])
        .into_iter()
        .find(|i| i.start == info.gap.end)
        .unwrap();
    assert_eq!(
        value(&after, jtag::TAP_STATE),
        Some(Value::Unknown {
            reason: jtag::LOST_STATE,
            needs: None
        })
    );
    // Only TDO lost: without a TDO channel nothing is.
    let mut w = s.wires();
    for l in &mut w.lines[..3] {
        l.gaps.clear();
    }
    let (mut h, j) = host(w, 64, None, false);
    h.run_until(|_| false).unwrap();
    assert_eq!(h.lost(j), &[] as &[Span]);
}

#[test]
fn without_tdo_a_scan_says_why_its_tdo_is_unknown() {
    let (s, _) = session(SMALL);
    let (mut h, j) = host(s.wires(), 333, None, false);
    h.run_until(|_| false).unwrap();
    let scans = of(&h, j, &SCANS);
    assert_eq!(scans.len(), 7);
    for i in &scans {
        assert_eq!(
            value(i, jtag::TDO_VALUE),
            Some(Value::Unknown {
                reason: jtag::NO_TDO,
                needs: Some(jtag::NEEDS_TDO)
            })
        );
        assert!(i.seq(jtag::TDO_BITS).is_empty());
    }
    assert_eq!(
        value(&scans[1], jtag::TDI_VALUE),
        Some(Value::Bits {
            bytes: vec![0; 4],
            bits: 32
        })
    );
}

#[test]
fn a_checkpoint_inside_a_long_scan_restores_to_the_same_output() {
    let (s, info) = session(FULL);
    let want = decoded(s.wires(), 5000);
    let (start, end) = (s.rise(info.long_from), s.rise(info.long_from + 16_384));
    let (mut h, j) = host(s.wires(), 5000, None, true);
    while h.consumed(j) < start + (end - start) / 2 {
        h.step().unwrap();
    }
    let long = of(&h, j, &[jtag::DR_SCAN])
        .into_iter()
        .find(|i| i.end.is_none())
        .unwrap();
    // The open scan holds settlement at its start; new starts are already far past it.
    assert!(h.settled(j) <= long.start);
    assert!(h.start_frontier(j) >= start + (end - start) / 3);
    let cut = h.checkpoint(j).unwrap();
    h.run_until(|_| false).unwrap();
    assert_eq!(h.store(j).counts, want, "forward");
    h.restore(&cut).unwrap();
    h.run_until(|_| false).unwrap();
    assert_eq!(h.store(j).counts, want, "restored");
}

#[test]
fn a_local_start_does_not_know_the_state_until_a_reset() {
    let (s, info) = session(SMALL);
    let local = s.rise(info.long_from + 50);
    let (mut h, j) = host(s.wires(), 64, Some(local), true);
    h.run_until(|_| false).unwrap();
    let states = of(&h, j, &[jtag::STATE]);
    assert_eq!(states[0].start, local);
    assert_eq!(
        value(&states[0], jtag::TAP_STATE),
        Some(Value::Unknown {
            reason: jtag::MID_SESSION,
            needs: None
        })
    );
    // The long scan began before the start; the last two scans follow a reset.
    let scans: Vec<_> = (of(&h, j, &SCANS).iter())
        .map(|i| (i.id.kind, value(i, jtag::LENGTH)))
        .collect();
    assert!(scans.iter().all(|s| s.1 != word32(320)), "{scans:?}");
    assert!(
        scans.ends_with(&[(jtag::IR_SCAN, word32(4)), (jtag::DR_SCAN, word32(300))]),
        "{scans:?}"
    );
}

/// Runs `h` turn by turn until `a` finishes, raising its interrupt before each turn where
/// `interrupt`. Returns the turns taken.
fn turn_by_turn(h: &mut Host, a: AnalysisId, interrupt: bool) -> u64 {
    let mut turns = 0;
    while h.finished(a).is_none() {
        while let Some(r) = h.take_io().unwrap() {
            h.reply(r).unwrap();
        }
        h.interrupt(a).store(interrupt, Relaxed);
        h.turn(a).unwrap();
        turns += 1;
        assert!(turns < 1_000_000, "no progress");
    }
    turns
}

#[test]
fn an_interrupted_turn_carries_on_to_the_same_output() {
    let (s, _) = session(SMALL);
    let (mut h, j) = host(s.wires(), 5000, None, true);
    let plain = turn_by_turn(&mut h, j, false);
    let want = h.store(j).counts;
    let (mut h, j) = host(s.wires(), 5000, None, true);
    let interrupted = turn_by_turn(&mut h, j, true);
    assert_eq!(h.store(j).counts, want);
    assert_eq!(h.lost(j).len(), 1);
    assert!(
        interrupted > 20 * plain,
        "{interrupted} turns against {plain}"
    );
}

#[test]
fn a_tms_cycle_of_one_clock_states_is_cut_into_bounded_paths() {
    let mut s = Session::new(1_000, 100);
    // Five highs reach Test-Logic-Reset, a low Run-Test/Idle, a high Select-DR; then Select-IR,
    // Capture-IR, Exit1-IR, Update-IR, Select-DR and round again, one clock each.
    s.tms(&[1, 1, 1, 1, 1, 0, 1]);
    for _ in 0..60 {
        s.tms(&[1, 0, 1, 1, 1]);
    }
    s.end = s.t + 20;
    let (mut h, j) = host(s.wires(), 1000, None, true);
    h.run_until(|_| false).unwrap();
    let paths: Vec<usize> = (of(&h, j, &[jtag::PATH]).iter())
        .map(|i| i.seq(jtag::VIA).len())
        .collect();
    assert!(paths.iter().all(|&n| n <= jtag::PATH_AT_MOST), "{paths:?}");
    assert!(paths.iter().sum::<usize>() >= 300, "{paths:?}");
    assert!(h.store(j).counts.peak_open <= 2);
}
