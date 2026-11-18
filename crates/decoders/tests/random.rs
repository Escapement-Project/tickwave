//! Each reference decoder stack, run through the host on seeded random wires: clocked lines with
//! random data, coincident edges, gaps anywhere, empty windows and an end anywhere. Reading never
//! panics, and the host admits every batch, which it checks against every rule.

mod common;

use common::*;
use escapement_tickwave_decode::{AnalysisId, Host, InputSpec, Span, Status, Tick, name};
use escapement_tickwave_decoders::{adiv5, cjtag, jtag, jtagdp, swd};
use std::panic::{AssertUnwindSafe, catch_unwind};

const CASES: u64 = 2000;

fn all() -> Vec<Span> {
    vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]
}

/// A clock and `data` other lines, on a coarse grid so that edges on different lines often fall
/// at one tick, with the clock's period, phases and stalls random.
fn clocked(r: &mut fastrand::Rng, data: usize) -> Wires {
    let grid = r.i64(1..=4);
    let clocks = r.usize(0..=160);
    let mut lines = vec![Line {
        initial: r.bool(),
        ..Line::default()
    }];
    let mut at = r.i64(0..=20) * grid;
    for _ in 0..clocks {
        at += r.i64(1..=6) * grid;
        lines[0].edges.push(at);
        if r.u8(..) < 8 {
            at += r.i64(10..=200) * grid;
        }
    }
    let span = at + r.i64(1..=40) * grid;
    for _ in 0..data {
        let mut l = Line {
            initial: r.bool(),
            ..Line::default()
        };
        let n = r.usize(0..=clocks.max(1) * 2);
        l.edges = (0..n).map(|_| r.i64(0..=span / grid) * grid).collect();
        lines.push(l);
    }
    Wires {
        lines,
        end: span,
        reversed: r.bool(),
    }
}

/// Edges dropped and added, gaps put anywhere, and the end moved, so that what was valid is cut
/// anywhere.
fn perturb(r: &mut fastrand::Rng, mut w: Wires) -> Wires {
    let span = w.end.max(1);
    for l in &mut w.lines {
        if r.u8(..) < 64 {
            l.edges.retain(|_| r.u8(..) > 8);
        }
        for _ in 0..r.usize(0..=3) {
            l.edges.push(r.i64(0..=span));
        }
        l.edges.retain(|&e| e > 0);
        l.edges.sort_unstable();
        l.edges.dedup();
        let mut gaps = vec![];
        let mut at = 0;
        for _ in 0..r.usize(0..=3) {
            let start = at + r.i64(0..=span / 4 + 1);
            let end = start + r.i64(1..=span / 8 + 2);
            gaps.push(Span { start, end });
            at = end + 1;
        }
        l.gaps = gaps;
    }
    if r.bool() {
        w.end = r.i64(1..=span + 10);
    }
    w
}

/// A window from one tick to more than the whole input, and a local start now and then.
fn reading(r: &mut fastrand::Rng, w: &Wires) -> (Tick, Option<Tick>) {
    let window = match r.u8(0..4) {
        0 => 1,
        1 => r.i64(1..=16),
        2 => r.i64(1..=2000),
        _ => 1 << 40,
    };
    let local = (r.u8(..) < 40).then(|| r.i64(0..=w.end.max(1) + 100));
    (window, local)
}

/// Runs `cases` random cases from `seed`, each building a host with `build` and running it to
/// the end. A panic or a refused batch names its seed and case.
fn cases(seed: u64, build: impl Fn(&mut fastrand::Rng) -> (Host, Vec<AnalysisId>)) -> Reached {
    let mut r = fastrand::Rng::with_seed(seed);
    let mut reached = Reached::default();
    for case in 0..CASES {
        let mut cr = fastrand::Rng::with_seed(r.u64(..));
        let (mut h, analyses) = build(&mut cr);
        let ran = catch_unwind(AssertUnwindSafe(|| h.run_until(|_| false)));
        let ran = match ran {
            Ok(ran) => ran,
            Err(_) => panic!("seed {seed}, case {case}: a panic"),
        };
        if let Err(why) = ran {
            panic!("seed {seed}, case {case}: refused: {why:?}");
        }
        assert!(
            h.finished(analyses[0]).is_some(),
            "seed {seed}, case {case}: the wire analysis did not finish"
        );
        for (k, &a) in analyses.iter().enumerate() {
            let c = h.store(a).counts;
            reached.items[k] += c.opens;
            reached.incomplete[k] += c.incomplete;
            reached.lost[k] += u64::from(!h.lost(a).is_empty());
            let lost = h.lost(a);
            assert!(
                lost.windows(2).all(|w| w[0].end <= w[1].start)
                    && lost.iter().all(|l| l.start < l.end),
                "seed {seed}, case {case}: lost input out of order: {lost:?}"
            );
            let swd = h.definition(a).name == "swd";
            for i in h.store(a).items.values() {
                assert!(
                    i.end.is_none_or(|e| e >= i.start),
                    "seed {seed}, case {case}: {:?} ends before it starts",
                    i.id
                );
                assert!(
                    !(swd && i.status == Some(Status::Incomplete(swd::DROPPED))),
                    "seed {seed}, case {case}: SWD no longer held the rises of {:?}",
                    i.id
                );
            }
        }
    }
    reached
}

/// What the cases reached, by analysis: items, items closed incomplete, and cases with lost
/// input, so that a generator that stopped reaching the decoders shows.
#[derive(Debug, Default)]
struct Reached {
    items: [u64; 3],
    incomplete: [u64; 3],
    lost: [u64; 3],
}

fn swd_stack(
    w: Wires,
    window: Tick,
    local: Option<Tick>,
    turnaround: u8,
) -> (Host, Vec<AnalysisId>) {
    let mut h = Host::default();
    let s = h
        .add(
            name("swd"),
            swd::definition(),
            InputSpec::Wire {
                source: Box::new(w),
                local,
            },
            swd::Swd::factory(window),
            &[swd::turnaround(turnaround)],
            all(),
        )
        .unwrap();
    let a = h
        .add(
            name("adi"),
            adiv5::adi_definition(),
            InputSpec::Upstream(adiv5::adi_subscription(s)),
            adiv5::Adi::factory(),
            &[],
            all(),
        )
        .unwrap();
    let m = h
        .add(
            name("mem"),
            adiv5::mem_definition(),
            InputSpec::Upstream(adiv5::mem_subscription(a)),
            adiv5::Mem::factory(),
            &[],
            all(),
        )
        .unwrap();
    (h, vec![s, a, m])
}

/// A window the forward pass has not reached starts without context, and says so: ADIv5 cannot
/// know SELECT before the window, MEM-AP cannot know the AP.
#[test]
fn a_window_ahead_of_the_forward_pass_says_what_it_cannot_know() {
    let ts = vec![
        dp_write(0x8, 0x0000_0000),
        ap_write(0x4, 0x2000_0000),
        ap_read(0xC, 0),
        dp_read(0xC, 0x1111_1111),
    ];
    let (b, starts) = bits(&ts);
    let (mut h, a) = swd_stack(wires(&b, 10), 64, None, 1);
    h.keep_checkpoints(
        a[0],
        escapement_tickwave_decode::Keeping::new(50, usize::MAX),
    )
    .unwrap();
    let start = rise(starts[1], 10) - 10;
    let window = Span {
        start,
        end: start + 2000,
    };
    let d = h.derive(a[0], window).unwrap();
    assert!(d.provisional);
    let w = d.window;
    let mem = |h: &Host| {
        let r = h.window_run(w, a[2]).unwrap();
        r.settled() >= window.end || r.finished().is_some()
    };
    h.run_until(mem).unwrap();
    let unknown = |x: AnalysisId| {
        (h.window_run(w, x).unwrap().store().items.values())
            .filter(|i| {
                (i.fields.iter())
                    .any(|f| matches!(f.value, escapement_tickwave_decode::Value::Unknown { .. }))
            })
            .count()
    };
    assert!(unknown(a[1]) > 0, "ADIv5 says SELECT is unknown");
    assert!(unknown(a[2]) > 0, "MEM-AP says the AP is unknown");
}

/// The fuzz targets' first finding: every stack, started locally past the end of its input, closed
/// what it held at the input's end, before it started.
#[test]
fn a_local_start_past_the_end_of_the_input_finishes_cleanly() {
    let empty = |ports| Wires {
        lines: vec![Line::default(); ports],
        end: 1,
        reversed: false,
    };
    let (h, a) = swd_stack(empty(2), 10_074, Some(9_735), 1);
    let mut hosts = vec![(h, a[0])];
    let mut h = Host::default();
    let j = h
        .add(
            name("jtag"),
            jtag::definition(),
            InputSpec::Wire {
                source: Box::new(empty(4)),
                local: Some(1_279),
            },
            jtag::Jtag::factory(1_150),
            &[],
            all(),
        )
        .unwrap();
    hosts.push((h, j));
    let mut h = Host::default();
    let c = h
        .add(
            name("cjtag"),
            cjtag::definition(),
            InputSpec::Wire {
                source: Box::new(empty(2)),
                local: Some(65_535),
            },
            cjtag::Cjtag::factory(65_535),
            &[],
            all(),
        )
        .unwrap();
    h.add(
        name("jtag"),
        cjtag::jtag_definition(),
        InputSpec::Upstream(cjtag::jtag_subscription(c)),
        cjtag::JtagOnCjtag::factory(),
        &[],
        all(),
    )
    .unwrap();
    hosts.push((h, c));
    for (mut h, root) in hosts {
        h.run_until(|_| false).unwrap();
        assert!(h.finished(root).is_some());
    }
}

#[test]
fn swd_with_adiv5_and_mem_ap_never_panics_and_never_breaks_a_rule() {
    let reached = cases(0x5d_0001, |r| {
        let w = if r.bool() {
            clocked(r, 1)
        } else {
            let transfers: Vec<T> = (0..r.usize(0..=12))
                .map(|_| {
                    let addr = r.u8(0..4) * 4;
                    let data = r.u32(..);
                    match r.u8(0..4) {
                        0 => dp_write(addr, data),
                        1 => dp_read(addr, data),
                        2 => ap_write(addr, data),
                        _ => ap_read(addr, data),
                    }
                })
                .collect();
            let (bits, _) = bits(&transfers);
            let period = r.i64(2..=20);
            perturb(r, wires(&bits, period))
        };
        let (window, local) = reading(r, &w);
        swd_stack(w, window, local, r.u8(1..=4))
    });
    assert!(reached.items.iter().all(|&n| n > 0), "{reached:?}");
}

#[test]
fn jtag_never_panics_and_never_breaks_a_rule() {
    let reached = cases(0x1a_0002, |r| {
        let w = clocked(r, 3);
        let w = if r.bool() { perturb(r, w) } else { w };
        let (window, local) = reading(r, &w);
        let has_tdo = r.bool();
        let edge = [
            jtag::TdoEdge::Rise,
            jtag::TdoEdge::Fall,
            jtag::TdoEdge::IrCapture,
        ][r.usize(..3)];
        // Short chains, which random IR scans fit now and then; with a 4-bit TAP nearest TDO,
        // JTAG-DP and ADIv5 stacked on it.
        let chain: &[u8] = [&[][..], &[1], &[2, 1], &[1, 1, 1], &[4], &[4, 1]][r.usize(..6)];
        let mut h = Host::default();
        let j = h
            .add(
                name("jtag"),
                jtag::definition(),
                InputSpec::Wire {
                    source: if has_tdo {
                        Box::new(w)
                    } else {
                        Box::new(Without {
                            wires: w,
                            port: jtag::TDO,
                        })
                    },
                    local,
                },
                jtag::Jtag::factory(window),
                &[jtag::tdo_edge(edge), jtag::chain(chain)],
                all(),
            )
            .unwrap();
        if chain.first() != Some(&4) {
            return (h, vec![j]);
        }
        let dp = h
            .add(
                name("jtag-dp"),
                jtagdp::definition(),
                InputSpec::Upstream(jtagdp::subscription(j)),
                jtagdp::JtagDp::factory(),
                &[],
                all(),
            )
            .unwrap();
        let adi = h
            .add(
                name("adiv5"),
                adiv5::adi_definition(),
                InputSpec::Upstream(adiv5::adi_subscription(dp)),
                adiv5::Adi::factory(),
                &[adiv5::reads(true)],
                all(),
            )
            .unwrap();
        (h, vec![j, dp, adi])
    });
    assert!(reached.items[0] > 0, "{reached:?}");
}

#[test]
fn cjtag_with_jtag_over_it_never_panics_and_never_breaks_a_rule() {
    let reached = cases(0xc1_0003, |r| {
        let w = clocked(r, 1);
        let w = if r.bool() { perturb(r, w) } else { w };
        let (window, local) = reading(r, &w);
        let mut h = Host::default();
        let c = h
            .add(
                name("cjtag"),
                cjtag::definition(),
                InputSpec::Wire {
                    source: Box::new(w),
                    local,
                },
                cjtag::Cjtag::factory(window),
                &[cjtag::assume_qualified(r.bool())],
                all(),
            )
            .unwrap();
        let mut analyses = vec![c];
        if r.u8(..) < 200 {
            let j = h
                .add(
                    name("jtag"),
                    cjtag::jtag_definition(),
                    InputSpec::Upstream(cjtag::jtag_subscription(c)),
                    cjtag::JtagOnCjtag::factory(),
                    &[],
                    all(),
                )
                .unwrap();
            analyses.push(j);
        }
        (h, analyses)
    });
    assert!(reached.items[..2].iter().all(|&n| n > 0), "{reached:?}");
}
