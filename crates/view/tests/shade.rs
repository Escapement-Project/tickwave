//! A digital column's edges and its time high, as a query gives them for the model in `shade`,
//! against the channel's own edges: exactly from the edges, and from the overview within what a
//! density and a duty can say. identify-swd's SWCLK, whose finest level is its strip, at 2^21.

mod common;

use std::sync::atomic::AtomicBool;

use escapement_tickwave_view::columns::State;
use escapement_tickwave_view::{Drawn, Query, Recording};

const SHIFT: u32 = 21;

/// SWCLK's columns over the recording, `per` ticks each from tick 0, and the grid's first column
/// at tick 0.
fn columns(
    rec: &Recording,
    per: i64,
) -> (escapement_tickwave_view::columns::DigitalColumns, usize) {
    let c = rec.channel("SWCLK").unwrap();
    let n = (rec.length() + per - 1) / per;
    let q = {
        let mut query = Query::new(0..n * per, n as u32);
        query.channels = vec![c];
        query
    };
    let a = rec.query(&q, &AtomicBool::new(false)).unwrap();
    assert_eq!(a.grid.per, per as f64);
    let Drawn::Digital { columns, .. } = &a.channels[0] else {
        panic!("digital")
    };
    (columns.clone(), (-a.grid.x0 / a.grid.per) as usize)
}

/// SWCLK's edges in [from, to), and its ticks high there.
fn truth(rec: &Recording, from: i64, to: i64) -> (usize, i64) {
    let c = rec.channel("SWCLK").unwrap();
    let (level, edges) = rec.edges(c, from, to, &AtomicBool::new(false)).unwrap();
    let (mut high, mut at, mut on) = (0, from, level);
    for &t in &edges {
        if on {
            high += t - at;
        }
        (at, on) = (t, !on);
    }
    if on {
        high += to - at;
    }
    (edges.len(), high)
}

#[test]
fn from_edges_a_columns_edges_and_time_high_are_exact() {
    let rec = common::open(&common::fixture("identify-swd.tickwave"));
    let per = 1i64 << (SHIFT - 1);
    let (cols, first) = columns(&rec, per);
    let end = rec.known(rec.channel("SWCLK").unwrap()).unwrap().end();
    let mut busy = 0;
    for k in 0..((end + per - 1) / per) as usize {
        let (from, to) = (k as i64 * per, ((k as i64 + 1) * per).min(end));
        let (n, high) = truth(&rec, from, to);
        let j = first + k;
        assert_eq!(cols.edges[j], n as f64, "column {k}");
        assert_eq!(cols.high[j], high as f64, "column {k}");
        assert_eq!(cols.known[j], (to - from) as f64, "column {k}");
        busy += usize::from(cols.state[j] == State::Busy);
    }
    assert!(busy > 3, "{busy} busy columns");
}

#[test]
fn from_the_overview_a_columns_edges_and_time_are_what_its_buckets_can_say() {
    let rec = common::open(&common::fixture("identify-swd.tickwave"));
    let per = 1i64 << SHIFT;
    let (cols, first) = columns(&rec, per);
    let end = rec.known(rec.channel("SWCLK").unwrap()).unwrap().end();
    // A density stands for every count within an eighth of a doubling of what it gives; a duty
    // for every share within 1/28 of its own.
    let bound = 2f64.powf(0.125) - 1.0;
    let (mut estimated, mut off) = (0, 0f64);
    for k in 0..((end + per - 1) / per) as usize {
        let (from, to) = (k as i64 * per, (k as i64 + 1) * per);
        let (n, high) = truth(&rec, from, to.min(end));
        let j = first + k;
        let got = cols.edges[j];
        match n {
            0 => assert_eq!(got, 0.0, "column {k}"),
            n => {
                let err = (got / n as f64 - 1.0).abs();
                assert!(err <= bound, "column {k}: {got} for {n}");
                off = off.max(err);
                estimated += 1;
            }
        }
        let known = (to.min(end) - from) as f64;
        assert!(
            (cols.high[j] / cols.known[j] - high as f64 / known).abs() <= 1.0 / 28.0 + 1e-12,
            "column {k}: {} of {} high, the edges say {high} of {known}",
            cols.high[j],
            cols.known[j]
        );
    }
    // Columns with edges, and counts the estimate didn't give exactly: the bound is tested.
    assert!(
        estimated > 3 && off > 0.0,
        "{estimated} columns, worst {off}"
    );
}

/// One digital channel D on a 1 MHz axis: an edge every 10 ticks from 0 to 1000, a gap from 1003
/// to 1007 after which D is high, then an edge every 10 ticks from 1010 to 2000.
fn gapped() -> Vec<u8> {
    use escapement_tickwave_format::header::{
        Declaration, DeclaredChannel, DeclaredKind, SourceFormat, SourceHeader,
    };
    use escapement_tickwave_format::model::{Gap, Ratio};
    use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
    let timebase = Ratio::integer(1_000_000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "D".into(),
            source_name: "d".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
            overview_shift: None,
            codec: None,
            requires: Vec::new(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("test".into()),
            timebase,
        )],
        title: None,
        meta: Default::default(),
        overview_shift: 4,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    let mut file = w.take_output();
    for t in (0..=1000).step_by(10) {
        w.edge(0, t).unwrap();
    }
    let gap = Gap {
        from: 1003,
        to: 1007,
        level: true,
    };
    w.gap(0, gap).unwrap();
    for t in (1010..=2000).step_by(10) {
        w.edge(0, t).unwrap();
    }
    file.extend(w.take_output());
    file.extend(w.finish(2001, &[2001], Finish::default()).unwrap().tail);
    file
}

#[test]
fn a_column_a_gap_splits_sums_both_sides_of_it() {
    let rec = common::open(&gapped());
    // Columns of 10 ticks, finer than the finest level's 16: read from the edges. Column 100,
    // [1000, 1010), is read in two pieces, [1000, 1003) and [1007, 1010).
    let q = {
        let mut query = Query::new(0..2010, 201);
        query.channels = vec![0];
        query
    };
    let a = rec.query(&q, &AtomicBool::new(false)).unwrap();
    let Drawn::Digital { columns, .. } = &a.channels[0] else {
        panic!("digital")
    };
    let k = ((1000.0 - a.grid.x0) / a.grid.per) as usize;
    // The edge at 1000 rises (the 101st from low), high to the gap; high after it.
    assert_eq!(
        (columns.edges[k], columns.known[k], columns.high[k]),
        (1.0, 6.0, 6.0),
        "column at 1000: {:?}",
        columns.state[k]
    );
}
