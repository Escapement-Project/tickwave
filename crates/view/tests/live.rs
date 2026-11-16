//! A live recording against the same file finished where the live one ends, after every op of
//! the format crate's live.rs sequence.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, IntCoding,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{Gap, Ratio};
use escapement_tickwave_format::writer::{Bundle, Compression, Finish, WriteOptions, Writer};
use escapement_tickwave_view::{Answer, Cache, Drawn, Query, Recording};

/// What a recorder pushes, in order.
#[derive(Clone, Debug)]
enum Op {
    Run(i64, u64),
    Pair(u64, u64),
    Edge(i64),
    Gap(Gap),
    Flush(i64),
    Mark(i64),
}

/// format/tests/live.rs's session: V, I and P as verge has them, at a pair every 10 ticks, and
/// DIO0, with a pulse at tick 0, a gap, a loss and a pause, and flushes.
fn session() -> (Declaration, Vec<Op>) {
    let timebase = Ratio::integer(96_000_000).unwrap();
    let int = |den| {
        AnalogCoding::Int(IntCoding {
            scale: Exact::new(1, den),
            ..IntCoding::new(32, false)
        })
    };
    let analog = |name: &str, coding| DeclaredChannel {
        name: name.into(),
        source_name: name.into(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period: Ratio::integer(10).unwrap(),
            coding,
            significant: Some(4),
            phase: None,
        },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let d = Declaration {
        timebase,
        channels: vec![
            analog("V", int(1000)),
            analog("I", int(1_000_000_000)),
            DeclaredChannel {
                name: "DIO0".into(),
                source_name: "dio0".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Default::default(),
                overview_shift: None,
                codec: None,
                requires: Vec::new(),
            },
        ],
        derived: vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            op: escapement_tickwave_format::header::Op::Multiply,
            operands: vec!["I".into(), "V".into()],
            params: Default::default(),
            requires: Vec::new(),
            significant: None,
        }],
        sources: vec![SourceHeader::new(
            SourceFormat::Other("verge-energy".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
        requires: Vec::new(),
    };
    let mut ops = vec![Op::Run(0, 0), Op::Edge(0), Op::Edge(0)];
    let mut index = 0u64;
    let mut pairs = |ops: &mut Vec<Op>, n: u64| {
        for _ in 0..n {
            ops.push(Op::Pair(3300 + index % 5, 1_000_000 + index * 37));
            index += 1;
        }
    };
    pairs(&mut ops, 7);
    ops.push(Op::Edge(65));
    ops.push(Op::Mark(66));
    pairs(&mut ops, 5);
    ops.push(Op::Flush(125));
    ops.push(Op::Gap(Gap {
        from: 120,
        to: 160,
        level: true,
    }));
    ops.push(Op::Run(160, 4));
    pairs(&mut ops, 9);
    ops.push(Op::Edge(245));
    ops.push(Op::Edge(245));
    ops.push(Op::Flush(250));
    ops.push(Op::Run(400, 0));
    pairs(&mut ops, 30);
    ops.push(Op::Mark(500));
    ops.push(Op::Edge(695));
    pairs(&mut ops, 2);
    (d, ops)
}

fn apply(w: &mut Writer, op: &Op) {
    match *op {
        Op::Run(t, lost) => {
            w.run(0, t, lost).unwrap();
            w.run(1, t, lost).unwrap();
        }
        Op::Pair(v, i) => {
            w.sample(0, v).unwrap();
            w.sample(1, i).unwrap();
        }
        Op::Edge(t) => w.edge(2, t).unwrap(),
        Op::Gap(g) => w.gap(2, g).unwrap(),
        Op::Flush(t) => w.flush(t).unwrap(),
        Op::Mark(t) => {
            let serde_json::Value::Object(m) =
                serde_json::json!({"id": format!("marker/{t}"), "start": t, "kind": "marker"})
            else {
                unreachable!()
            };
            w.annotation("verge", m).unwrap();
        }
    }
}

fn query(window: std::ops::Range<i64>, columns: u32) -> Query {
    Query {
        window,
        columns,
        channels: vec![0, 1, 2, 3],
        ..Query::default()
    }
}

/// Columns of `a` and `b` equal, those that end by `upto` where it is given.
fn same_columns(
    a: &Drawn,
    b: &Drawn,
    grid: &escapement_tickwave_view::columns::Grid,
    upto: Option<i64>,
    at: &str,
) {
    let ends = |k: usize| grid.x0 + (k + 1) as f64 * grid.per;
    let keep = |k: usize| upto.is_none_or(|u| ends(k) <= u as f64);
    match (a, b) {
        (Drawn::Analog { columns: x, .. }, Drawn::Analog { columns: y, .. }) => {
            for k in (0..grid.count).filter(|&k| keep(k)) {
                assert_eq!(
                    (
                        x.min[k].to_bits(),
                        x.max[k].to_bits(),
                        x.joined[k],
                        x.pale[k]
                    ),
                    (
                        y.min[k].to_bits(),
                        y.max[k].to_bits(),
                        y.joined[k],
                        y.pale[k]
                    ),
                    "{at}, column {k}"
                );
            }
        }
        (Drawn::Digital { columns: x, .. }, Drawn::Digital { columns: y, .. }) => {
            for k in (0..grid.count).filter(|&k| keep(k)) {
                assert_eq!(
                    (x.state[k], x.at[k].to_bits()),
                    (y.state[k], y.at[k].to_bits()),
                    "{at}, column {k}"
                );
            }
        }
        _ => panic!("{at}: different kinds"),
    }
}

#[test]
fn a_live_recording_reads_as_the_same_file_finished_where_it_ends() {
    let (d, ops) = session();
    let cancel = AtomicBool::new(false);
    let mut compared = 0;
    for (block_bytes, compression) in [
        (64, Compression::Spread),
        (64, Compression::Offload),
        (1 << 20, Compression::Spread),
    ] {
        let options = WriteOptions {
            level: 3,
            block_bytes,
            compression,
            promise_with_items: true,
        };
        for n in 0..ops.len() {
            let at = format!("{block_bytes} {compression:?}, after op {n} ({:?})", ops[n]);
            let mut w = Writer::new(d.clone(), options.clone()).unwrap();
            let mut file = w.take_output();
            for op in &ops[..=n] {
                apply(&mut w, op);
            }
            let snapshot = w.snapshot();
            file.extend(w.take_output());
            // Where each channel's finest level's settled buckets end.
            let settled: Vec<i64> = (0..d.channels.len() + d.derived.len())
                .map(|o| {
                    let l = &snapshot.overview(o).unwrap().levels[0];
                    ((l.held.first + l.held.buckets()) as i64) << l.shift
                })
                .collect();
            let live =
                Recording::live(snapshot, Arc::new(file.clone()), Cache::new(1 << 20)).unwrap();
            let length = live.length();
            if length == 0 {
                continue;
            }
            // The same writes, finished at the live recording's length.
            let mut f = Writer::new(d.clone(), options.clone()).unwrap();
            let mut finished_file = f.take_output();
            for op in &ops[..=n] {
                apply(&mut f, op);
            }
            finished_file.extend(f.take_output());
            let finish = Finish {
                bundle: Some(Bundle::Generated { stem: "rec".into() }),
                drop_gaps_past_end: true,
                ..Finish::default()
            };
            finished_file.extend(f.finish(length, &[length], finish).unwrap().tail);
            let finished = Recording::open(Arc::new(finished_file), Cache::new(1 << 20)).unwrap();
            assert_eq!(finished.length(), length, "{at}");

            // A finished file drops what a recorder marked at or past its length. Its arrivals
            // number its parts' lines, so only their order is the same.
            let all = live.markers(i64::MIN..i64::MAX, None, usize::MAX).entries;
            let kept: Vec<_> = all
                .iter()
                .filter(|m| m.cursor.tick < length)
                .map(|m| &m.annotation)
                .collect();
            assert_eq!(
                finished
                    .markers(i64::MIN..i64::MAX, None, usize::MAX)
                    .entries
                    .iter()
                    .map(|m| &m.annotation)
                    .collect::<Vec<_>>(),
                kept,
                "{at}"
            );
            if kept.len() == all.len() {
                assert_eq!(finished.extent(), live.extent(), "{at}");
            }
            for (c, (x, y)) in live.channels().iter().zip(finished.channels()).enumerate() {
                assert_eq!(x, y, "{at}, channel {c}");
                assert_eq!(
                    (live.lost(c), live.gaps(c) >= finished.gaps(c)),
                    (finished.lost(c), true),
                    "{at}"
                );
            }

            let mut intervals = vec![(0, length), (-5, length + 5)];
            for a in (0..length).step_by(7) {
                for w in [1, 5, 13, 40, 200] {
                    intervals.push((a, a + w));
                }
            }
            for (a, b) in intervals {
                assert_eq!(
                    live.statistics(a..b, &cancel).unwrap(),
                    finished.statistics(a..b, &cancel).unwrap(),
                    "{at}, statistics {a}..{b}"
                );
            }

            // A tick a column and finer: every sample, the same in both.
            for (window, columns) in [
                (0..length, length as u32),
                (length / 3..2 * length / 3 + 1, 50),
            ] {
                let (x, y) = (
                    live.query(&query(window.clone(), columns), &cancel)
                        .unwrap(),
                    finished
                        .query(&query(window.clone(), columns), &cancel)
                        .unwrap(),
                );
                assert_eq!(x.grid, y.grid);
                for (c, (p, q)) in x.channels.iter().zip(&y.channels).enumerate() {
                    same_columns(
                        p,
                        q,
                        &x.grid,
                        None,
                        &format!("{at}, {window:?}, channel {c}"),
                    );
                    if let (
                        Drawn::Analog {
                            samples: s,
                            lost: l,
                            fit: f,
                            ..
                        },
                        Drawn::Analog {
                            samples: t,
                            lost: m,
                            fit: g,
                            ..
                        },
                    ) = (p, q)
                    {
                        assert_eq!((s, l, f), (t, m, g), "{at}, {window:?}, channel {c}");
                    }
                }
                compared += 1;
            }
            // From the overview, where the live level holds the buckets: past them the live view
            // reads its samples, which a column can show finer than a bucket does.
            for columns in [2, 3, 5] {
                let q = query(0..length, columns);
                let (x, y) = (
                    live.query(&q, &cancel).unwrap(),
                    finished.query(&q, &cancel).unwrap(),
                );
                if x.grid.per < 16.0 {
                    continue;
                }
                for (c, (p, r)) in x.channels.iter().zip(&y.channels).enumerate() {
                    same_columns(
                        p,
                        r,
                        &x.grid,
                        Some(settled[c]),
                        &format!("{at}, {columns} columns, channel {c}"),
                    );
                }
                compared += 1;
            }
        }
    }
    assert!(compared > 150, "{compared}");
}

#[test]
fn a_long_live_recording_reads_the_levels_the_file_finished_there_keeps() {
    // 10,000 pairs over 100,000 ticks, finest level 2^4: a file that long keeps 2^4 and 2^8 (391
    // buckets), and its writer holds 2^12 as well. Zoomed out to 5,000 ticks a column, the live
    // view reads 2^8 as the finished file does, not 2^12, whose buckets reach across columns.
    let (d, _) = session();
    let options = WriteOptions {
        level: 1,
        block_bytes: 1 << 12,
        ..WriteOptions::default()
    };
    let write = |w: &mut Writer| {
        w.run(0, 0, 0).unwrap();
        w.run(1, 0, 0).unwrap();
        for i in 0..10_000u64 {
            if i % 1000 == 999 {
                w.flush(i as i64 * 10 - 5).unwrap();
            }
            w.sample(0, 3300 + i % 7).unwrap();
            w.sample(1, 1_000_000 + i * 37).unwrap();
        }
    };
    let mut w = Writer::new(d.clone(), options.clone()).unwrap();
    let mut file = w.take_output();
    write(&mut w);
    let snapshot = w.snapshot();
    file.extend(w.take_output());
    let finest = &snapshot.overview(1).unwrap().levels[0];
    let settled = ((finest.held.first + finest.held.buckets()) as i64) << finest.shift;
    let live = Recording::live(snapshot, Arc::new(file), Cache::new(1 << 20)).unwrap();
    let length = live.length();
    let mut f = Writer::new(d, options).unwrap();
    let mut finished_file = f.take_output();
    write(&mut f);
    finished_file.extend(f.take_output());
    finished_file.extend(f.finish(length, &[length], Finish::default()).unwrap().tail);
    let finished = Recording::open(Arc::new(finished_file), Cache::new(1 << 20)).unwrap();
    let cancel = AtomicBool::new(false);
    let q = query(0..length, 20);
    let (x, y) = (
        live.query(&q, &cancel).unwrap(),
        finished.query(&q, &cancel).unwrap(),
    );
    assert!(
        x.grid.per > 4096.0 && settled > 90_000,
        "{} {settled}",
        x.grid.per
    );
    same_columns(&x.channels[1], &y.channels[1], &x.grid, Some(settled), "I");
}

/// A file a writer still appends to, as a recorder's is on disk: reads see what it holds now.
struct Growing(std::sync::Mutex<Vec<u8>>);

impl escapement_tickwave_view::Bytes for Growing {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.0.lock().unwrap().len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let file = self.0.lock().unwrap();
        let at = offset as usize;
        buf.copy_from_slice(&file[at..at + buf.len()]);
        Ok(())
    }
}

#[test]
fn one_recording_updated_with_each_snapshot_reads_as_one_made_from_it() {
    let (d, mut ops) = session();
    // A mark that comes after a later one, and one past the data, which widens the extent.
    ops.extend([Op::Mark(30), Op::Mark(5_000)]);
    let cancel = AtomicBool::new(false);
    for (block_bytes, compression) in [
        (64, Compression::Spread),
        (64, Compression::Offload),
        (1 << 20, Compression::Spread),
    ] {
        let options = WriteOptions {
            level: 3,
            block_bytes,
            compression,
            promise_with_items: true,
        };
        let mut w = Writer::new(d.clone(), options).unwrap();
        let file = Arc::new(Growing(std::sync::Mutex::new(w.take_output())));
        let mut updated: Option<Recording> = None;
        for (n, op) in ops.iter().enumerate() {
            let at = format!("{block_bytes} {compression:?}, after op {n} ({op:?})");
            apply(&mut w, op);
            let snapshot = w.snapshot();
            file.0.lock().unwrap().extend(w.take_output());
            let fresh =
                Recording::live(snapshot.clone(), file.clone(), Cache::new(1 << 20)).unwrap();
            match &mut updated {
                None => {
                    updated =
                        Some(Recording::live(snapshot, file.clone(), Cache::new(1 << 20)).unwrap())
                }
                Some(u) => u.update(snapshot).unwrap(),
            }
            let u = updated.as_ref().unwrap();
            assert_eq!(
                (u.length(), u.extent(), u.channels()),
                (fresh.length(), fresh.extent(), fresh.channels()),
                "{at}"
            );
            for c in 0..u.channels().len() {
                assert_eq!(
                    (u.lost(c), u.gaps(c)),
                    (fresh.lost(c), fresh.gaps(c)),
                    "{at}"
                );
            }
            assert_eq!(
                u.markers(i64::MIN..i64::MAX, None, usize::MAX),
                fresh.markers(i64::MIN..i64::MAX, None, usize::MAX),
                "{at}"
            );
            // Both go through one update, so the marks also against the ops: in order of their
            // ticks, then of their arrival, and inside the extent. One generator's arrivals
            // follow the order its writer took them in.
            let mut marks: Vec<i64> = ops[..=n]
                .iter()
                .filter_map(|op| match op {
                    Op::Mark(t) => Some(*t),
                    _ => None,
                })
                .collect();
            marks.sort_unstable();
            let entries = u.markers(i64::MIN..i64::MAX, None, usize::MAX).entries;
            let got: Vec<i64> = entries.iter().map(|m| m.cursor.tick).collect();
            assert_eq!(got, marks, "{at}");
            assert!(
                entries.windows(2).all(|w| w[0].cursor < w[1].cursor),
                "{at}: cursors in order, each its own"
            );
            if let (Some(extent), Some(&last)) = (u.extent(), marks.iter().max()) {
                assert!(
                    extent.end > last && extent.start <= marks[0],
                    "{at}: {extent:?}"
                );
            }
            let length = u.length();
            if length == 0 {
                continue;
            }
            for window in [0..length, length / 3..length, -5..length + 5] {
                let mut q = query(window.clone(), 16);
                q.statistics = vec![window.clone()];
                // Each names the recording it is from.
                let want = fresh.query(&q, &cancel).unwrap();
                let got = Answer {
                    revision: want.revision,
                    ..u.query(&q, &cancel).unwrap()
                };
                assert_eq!(got, want, "{at}, {window:?}");
            }
        }
    }
}

#[test]
fn a_long_recording_updated_with_each_snapshot_reads_as_one_made_from_it() {
    // 70,000 pairs, a pair a finest bucket, alike from 20,000 to 60,000, so a level's run of
    // alike overview blocks grows from one snapshot to the next; 64 KiB blocks, which take
    // several calls to compress, so a snapshot taken just after a flush closes one holds it
    // not yet written.
    let (mut d, _) = session();
    for c in &mut d.channels[..2] {
        if let DeclaredKind::Analog { period, .. } = &mut c.data {
            *period = Ratio::integer(16).unwrap();
        }
    }
    let options = WriteOptions {
        level: 1,
        block_bytes: 1 << 16,
        compression: Compression::Spread,
        promise_with_items: true,
    };
    let mut w = Writer::new(d, options).unwrap();
    let file = Arc::new(Growing(std::sync::Mutex::new(w.take_output())));
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    let cancel = AtomicBool::new(false);
    let mut updated: Option<Recording> = None;
    let (mut unwritten, mut runs) = (0, 0);
    for i in 0..70_000u64 {
        if i % 5000 == 0 {
            w.edge(2, i as i64 * 16).unwrap();
        }
        if i % 1000 == 999 {
            w.flush(i as i64 * 16 - 5).unwrap();
        }
        let x = if (20_000..60_000).contains(&i) {
            0
        } else {
            i * 7 % 13
        };
        w.sample(0, 3300 + x).unwrap();
        w.sample(1, 1_000_000 + x * 37).unwrap();
        // Each pair for a while from each flush, where blocks close and wait to be compressed.
        if (i + 1) % 1000 >= 12 {
            continue;
        }
        let snapshot = w.snapshot();
        file.0.lock().unwrap().extend(w.take_output());
        unwritten += (0..2)
            .filter(|&c| snapshot.channel(c).unwrap().unwritten.len() > 1)
            .count();
        runs += snapshot
            .overview(1)
            .unwrap()
            .levels
            .iter()
            .filter(|l| l.in_blocks && l.run.as_ref().is_some_and(|r| r.n > 1))
            .count();
        let fresh = Recording::live(snapshot.clone(), file.clone(), Cache::new(1 << 20)).unwrap();
        match &mut updated {
            None => {
                updated =
                    Some(Recording::live(snapshot, file.clone(), Cache::new(1 << 20)).unwrap())
            }
            Some(u) => u.update(snapshot).unwrap(),
        }
        let u = updated.as_ref().unwrap();
        let length = u.length();
        // The whole of it from the strip, and the end, where the blocks not yet written are,
        // from each level in blocks: 4,000 ticks a column from 2^8, 100 from 2^4.
        for (window, columns) in [
            (0..length, 20),
            (length - 400_000..length, 100),
            (length - 20_000..length, 200),
        ] {
            let q = query(window.clone(), columns);
            let want = fresh.query(&q, &cancel).unwrap();
            let got = Answer {
                revision: want.revision,
                ..u.query(&q, &cancel).unwrap()
            };
            assert_eq!(got, want, "after pair {i}, {window:?} in {columns} columns");
        }
    }
    assert!(
        unwritten > 10 && runs > 100,
        "{unwritten} channels held a closed block not yet written, {runs} levels a run"
    );
}
