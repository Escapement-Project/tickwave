//! The probe's searches over a recording: probe.test.ts's async cases, on its recordings written
//! to files. Expected values are worked by hand there.

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

use escapement_tickwave_format::header::{
    Declaration, DeclaredChannel, DeclaredKind, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
use escapement_tickwave_view::probe::{EdgeAt, Known, Pulse, complete_pulse, find_edge};

mod common;
use common::open;

const LENGTH: i64 = 60_000_000;
const WHOLE: Known = Known {
    start: 0,
    end: LENGTH,
};

/// One digital channel over 60 ms at 1 GS/s with `edges`, its finest level 2^12 as a converter
/// would pick, in blocks of `block_bytes`.
fn digital(edges: &[i64], block_bytes: usize) -> Vec<u8> {
    let timebase = Ratio::integer(1_000_000_000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "CLK".into(),
            source_name: "clk".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("test".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 12,
    };
    let mut w = Writer::new(
        d,
        WriteOptions {
            block_bytes,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    for &t in edges {
        w.edge(0, t).unwrap();
    }
    let mut out = w.take_output();
    out.extend(w.finish(LENGTH, &[LENGTH], Finish::default()).unwrap().tail);
    out
}

/// A clock, then a long quiet stretch, then three edges: 10 ns high, 15 low, from tick 100.
fn clock() -> Vec<i64> {
    let mut ticks = Vec::new();
    for t in (100..100 + 25 * 40).step_by(25) {
        ticks.extend([t, t + 10]);
    }
    ticks.extend([50_000_000, 50_000_627, 50_001_274]);
    ticks
}

const UNREAD: Pulse = Pulse {
    level: false,
    start: EdgeAt::Unread,
    end: EdgeAt::Unread,
    next: EdgeAt::Unread,
};

#[test]
fn the_edges_of_a_long_quiet_stretch_are_found_through_the_overview() {
    for block_bytes in [1 << 20, 64] {
        let rec = open(&digital(&clock(), block_bytes));
        let cancel = AtomicBool::new(false);
        let find = |lo, hi, forward| find_edge(&rec, 0, lo, hi, forward, &cancel).unwrap();
        assert_eq!(find(10_000_000, 60_000_001, true), Some(50_000_000));
        assert_eq!(find(1, 10_000_001, false), Some(1_085));
        assert_eq!(find(50_001_275, 60_000_001, true), None);
        // Inside one bucket and across one: the edges themselves.
        assert_eq!(find(110, 135, true), Some(110));
        assert_eq!(find(110, 135, false), Some(125));
        let p = complete_pulse(&rec, 0, &UNREAD, 10_000_050, WHOLE, &cancel).unwrap();
        assert_eq!(
            p,
            Pulse {
                level: false,
                start: EdgeAt::Tick(1_085),
                end: EdgeAt::Tick(50_000_000),
                next: EdgeAt::Tick(50_000_627),
            }
        );
    }
}

#[test]
fn a_pulse_the_source_stamped_with_one_time_ends_its_period_there() {
    // High from 10, a low pulse at 20 too short to time, high to 30; then quiet until a pair at
    // 50,000,000. Found by search, the period ends at the pair's second edge, not the edge after.
    let rec = open(&digital(
        &[10, 20, 20, 30, 50_000_000, 50_000_000, 50_000_500],
        1 << 20,
    ));
    let cancel = AtomicBool::new(false);
    let p = complete_pulse(&rec, 0, &UNREAD, 10_000_000, WHOLE, &cancel).unwrap();
    assert_eq!(
        p,
        Pulse {
            level: false,
            start: EdgeAt::Tick(30),
            end: EdgeAt::Tick(50_000_000),
            next: EdgeAt::Tick(50_000_000),
        }
    );
    // Past the last edge the pulse runs to the end, and so does its period.
    let p = complete_pulse(&rec, 0, &UNREAD, 55_000_000, WHOLE, &cancel).unwrap();
    assert_eq!(
        (p.start, p.end, p.next),
        (EdgeAt::Tick(50_000_500), EdgeAt::Open, EdgeAt::Open)
    );
}

/// A search whose span runs past the buckets the overview has: a live recording's tail, which no
/// block or overview block holds yet, and a finished one's past its end. The part past them is
/// searched in the edges, not through the overview again, which found nothing new there and
/// recursed until the stack ran out.
#[test]
fn a_search_past_the_buckets_the_overview_has_reads_the_edges_there() {
    let rec = open(&digital(&clock(), 1 << 20));
    let cancel = AtomicBool::new(false);
    let find = |lo, hi, forward| find_edge(&rec, 0, lo, hi, forward, &cancel).unwrap();
    assert_eq!(find(50_001_275, LENGTH + 10_000_000, true), None);
    assert_eq!(find(0, LENGTH + 10_000_000, false), Some(50_001_274));
    assert_eq!(find(i64::MIN, i64::MAX, true), Some(100));

    let timebase = Ratio::integer(1_000_000_000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "CLK".into(),
            source_name: "clk".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("test".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 0,
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    w.edge(0, 10).unwrap();
    w.flush(20).unwrap();
    // Past the flush, in the open block: the overview's buckets end at 20.
    w.edge(0, 100_000).unwrap();
    w.edge(0, 200_000).unwrap();
    let snapshot = w.snapshot();
    let file = w.take_output();
    let rec = escapement_tickwave_view::Recording::live(
        snapshot,
        std::sync::Arc::new(file),
        escapement_tickwave_view::Cache::new(1 << 20),
    )
    .unwrap();
    assert_eq!(rec.length(), 200_001);
    let find = |lo, hi, forward| find_edge(&rec, 0, lo, hi, forward, &cancel).unwrap();
    assert_eq!(find(11, 200_001, true), Some(100_000));
    assert_eq!(find(0, 199_999, false), Some(100_000));
}

/// A search on a channel past the last, wide enough to go through the overview: no such channel,
/// as a narrow one reading the edges says.
#[test]
fn a_search_of_no_digital_channel_says_so() {
    let rec = open(&digital(&clock(), 1 << 20));
    let cancel = AtomicBool::new(false);
    for (lo, hi) in [(0, 10), (0, LENGTH)] {
        let err = find_edge(&rec, 1, lo, hi, true, &cancel).unwrap_err();
        assert!(
            matches!(err, escapement_tickwave_view::Error::NoChannel(1)),
            "{lo}..{hi}: {err}"
        );
    }
}
