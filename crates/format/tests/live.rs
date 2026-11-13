//! A live recorder's file afterwards and while it is written: the Recutter that writes a flushed
//! file again with full-size blocks, and the snapshot a view in the same process reads.

use std::collections::BTreeMap;
use std::path::PathBuf;

use escapement_tickwave_format::decode::{Block, BlockChannel, Footer, OverviewBlock};
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, IntCoding,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{Break, Gap, Ratio, Sample};
use escapement_tickwave_format::overview::{AnalogOverview, BLOCK_BUCKETS, HEAD_BUCKETS, Level};
use escapement_tickwave_format::recut::{RecutError, Recutter};
use escapement_tickwave_format::repair::repair;
use escapement_tickwave_format::samples::Values;
use escapement_tickwave_format::snapshot::Snapshot;
use escapement_tickwave_format::writer::{Finish, Finished, WriteOptions, Writer};

mod common;

fn energy() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/fixtures/energy.tickwave");
    std::fs::read(path).unwrap()
}

fn options(block_bytes: usize) -> WriteOptions {
    WriteOptions {
        level: 3,
        block_bytes,
        batch: 1,
    }
}

/// `file` fed to a Recutter in pieces of `piece` bytes: its output and what it finished with.
fn recut(
    file: &[u8],
    piece: usize,
    options: WriteOptions,
) -> Result<(Vec<u8>, Finished), RecutError> {
    let mut r = Recutter::new(options);
    let mut out = Vec::new();
    for chunk in file.chunks(piece) {
        r.feed(chunk)?;
        out.extend_from_slice(r.output());
        let n = r.output().len();
        r.consume(n);
    }
    let f = r.finish()?;
    out.extend_from_slice(&f.tail);
    Ok((out, f))
}

/// Every channel's items: edges, or samples, from every block of a finished file.
fn items(file: &[u8]) -> (Vec<Vec<i64>>, Vec<Vec<Sample>>) {
    let head = Footer::parse_file(file).unwrap();
    let n = head.header.channels.len();
    let (mut edges, mut samples) = (vec![Vec::new(); n], vec![Vec::new(); n]);
    for i in 0..head.rows.len() {
        let (offset, len) = head.block_location(i);
        let block = head
            .decode_block(i, &file[offset as usize..][..len as usize])
            .unwrap();
        for (c, ch) in block.channels.into_iter().enumerate() {
            match ch {
                BlockChannel::Digital { edges: e, .. } => edges[c].extend(e),
                BlockChannel::Analog { values, .. } => samples[c].extend(values.iter()),
            }
        }
    }
    (edges, samples)
}

#[test]
fn a_flushed_file_recut_has_full_size_blocks_and_the_same_content() {
    let file = energy();
    let before = Footer::parse_file(&file).unwrap();
    assert_eq!(before.header.blocks, 6, "the fixture's flushes");
    for piece in [1 << 20, 997] {
        let (out, f) = recut(&file, piece, options(1 << 20)).unwrap();
        let after = Footer::parse_file(&out).unwrap();
        assert_eq!(after.header.content_id, before.header.content_id);
        assert_eq!(f.header, after.header);
        // Every item in one block of 1 MiB.
        assert_eq!(after.header.blocks, 1);
        assert_eq!(items(&out), items(&file));
        // The same header, apart from where the blocks are: the bundle, the extras and the
        // derived channel come over.
        let mut header = after.header.clone();
        header.blocks = before.header.blocks;
        assert_eq!(header, before.header);
        assert_eq!(after.overview, before.overview);
    }
}

/// A recorder's file flushed every second: 10 kHz pairs for 20 s, at 96 MHz.
fn flushed_each_second() -> Vec<u8> {
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
            period: Ratio::integer(9600).unwrap(),
            coding,
            significant: None,
        },
        shaping: Default::default(),
    };
    let d = Declaration {
        timebase,
        channels: vec![analog("V", int(1000)), analog("I", int(1_000_000_000))],
        derived: vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            product: ["I".into(), "V".into()],
            significant: None,
        }],
        sources: vec![SourceHeader::new(
            SourceFormat::Other("verge-energy".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 16,
    };
    let mut w = Writer::new(d, options(1 << 20)).unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    let mut out = Vec::new();
    for i in 0..200_000u64 {
        if i > 0 && i % 10_000 == 0 {
            w.flush(i as i64 * 9600).unwrap();
            out.extend(w.take_output());
        }
        w.sample(0, 3300 + i % 7).unwrap();
        w.sample(1, 1_000_000 + (i * 7919) % 100_000).unwrap();
    }
    out.extend(w.take_output());
    let length = 200_000 * 9600;
    out.extend(w.finish(length, &[], Finish::default()).unwrap().tail);
    out
}

#[test]
fn a_file_flushed_each_second_recut_comes_out_in_blocks_of_the_size_asked_for() {
    let file = flushed_each_second();
    let before = Footer::parse_file(&file).unwrap();
    assert_eq!(before.header.blocks, 20);
    // Each second's block holds 10,000 pairs: 80,000 bytes of codes and an 8-byte table.
    assert!(before.rows.iter().all(|r| r.decoded_length == 80_008));
    let target = 256 << 10;
    let (out, _) = recut(&file, 1 << 16, options(target)).unwrap();
    let after = Footer::parse_file(&out).unwrap();
    assert_eq!(after.header.content_id, before.header.content_id);
    // 1.6 MB in blocks closed once they reach 256 KiB: a pair is 8 bytes, so 32,768 pairs, then
    // the next pair closes it. Six full blocks, then the rest.
    let sizes: Vec<u32> = after.rows.iter().map(|r| r.decoded_length).collect();
    let (full, last) = sizes.split_at(sizes.len() - 1);
    assert_eq!(full.len(), 6, "{sizes:?}");
    assert!(full.iter().all(|&s| s == 8 + 8 * 32_768), "{sizes:?}");
    assert_eq!(last[0], 8 + 8 * (200_000 - 6 * 32_768), "{sizes:?}");
    assert_eq!(items(&out), items(&file));
}

#[test]
fn a_cut_off_file_recut_gives_what_repair_gives() {
    let file = energy();
    let footer_at = Footer::parse_file(&file).unwrap().trailer.footer_offset as usize;
    for at in [footer_at / 3, footer_at / 2, footer_at - 5, footer_at + 9] {
        let cut = &file[..at];
        let repaired = repair(cut, 3).unwrap_or_else(|e| panic!("cut at {at}: {e}"));
        let (out, f) = recut(cut, 4096, options(1 << 20)).unwrap();
        let head = Footer::parse_file(&out).unwrap();
        assert_eq!(
            head.header.content_id, repaired.header.content_id,
            "cut at {at}"
        );
        assert_eq!(
            head.header.channels, repaired.header.channels,
            "cut at {at}"
        );
        assert_eq!(head.header.length, repaired.header.length, "cut at {at}");
        assert_eq!(f.generated, repaired.generated, "cut at {at}");
        let fixed = [&cut[..repaired.keep as usize], &repaired.tail].concat();
        assert_eq!(items(&out), items(&fixed), "cut at {at}");
    }
}

#[test]
fn a_file_whose_items_do_not_give_its_footers_content_id_is_refused() {
    let file = energy();
    let id = Footer::parse_file(&file)
        .unwrap()
        .header
        .content_id
        .to_string();
    let other = format!("blake3:{}", "0".repeat(64));
    // The footer states another id; every record still checks.
    let doctored = common::patched(
        &file,
        &format!("\"content_id\":\"{id}\""),
        &format!("\"content_id\":\"{other}\""),
    );
    Footer::parse_file(&doctored).unwrap();
    let err = recut(&doctored, 1 << 20, options(1 << 20)).err().unwrap();
    assert!(matches!(err, RecutError::ContentId { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        format!("the input's items give content id {id}, but its footer says {other}")
    );
}

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

/// V, I and P as verge has them, at a pair every 10 ticks, and DIO0, with a pulse at tick 0, a
/// gap, a loss and a pause, and flushes.
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
        },
        shaping: Default::default(),
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
            },
        ],
        derived: vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            product: ["I".into(), "V".into()],
            significant: None,
        }],
        sources: vec![SourceHeader::new(
            SourceFormat::Other("verge-energy".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
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
    // 4 pairs lost after the one at 110: the run starts again at 160.
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
    // A pause.
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
                serde_json::json!({"id": format!("marker/{t}"), "start": t})
            else {
                unreachable!()
            };
            w.annotation("verge", m).unwrap();
        }
    }
}

/// The edges, codes and products a view should have after `ops`.
fn pushed(ops: &[Op]) -> (Vec<i64>, Vec<u64>, Vec<u64>, Vec<u64>) {
    let (mut edges, mut v, mut i) = (Vec::new(), Vec::new(), Vec::new());
    for op in ops {
        match *op {
            Op::Edge(t) => edges.push(t),
            Op::Pair(a, b) => {
                v.push(a);
                i.push(b);
            }
            _ => {}
        }
    }
    let p = i.iter().zip(&v).map(|(a, b)| a * b).collect();
    (edges, v, i, p)
}

fn codes(values: &Values) -> Vec<u64> {
    match values {
        Values::U64(v) => v.clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_snapshot_with_the_bytes_written_so_far_gives_every_item_pushed() {
    let (d, ops) = session();
    // Blocks close on their size too: 64 bytes is a few pairs.
    for (block_bytes, batch) in [(64, 1), (64, 3), (1 << 20, 1)] {
        let mut w = Writer::new(
            d.clone(),
            WriteOptions {
                level: 3,
                block_bytes,
                batch,
            },
        )
        .unwrap();
        let mut file = w.take_output();
        let mut rows_seen = 0;
        for (n, op) in ops.iter().enumerate() {
            apply(&mut w, op);
            let snapshot = w.snapshot();
            // The caller writes what the writer has given out, then publishes the snapshot.
            file.extend(w.take_output());
            assert_eq!(snapshot.position(), file.len() as u64);
            let mut blocks: Vec<Block> = (0..snapshot.block_count())
                .map(|i| {
                    let row = snapshot.row(i).unwrap();
                    let frame = &file[row.offset as usize..][..row.length as usize];
                    snapshot.decode_block(i, frame).unwrap()
                })
                .collect();
            blocks.extend(snapshot.queued_blocks().unwrap());
            blocks.push(snapshot.open_block().unwrap());
            for pair in blocks.windows(2) {
                assert_eq!(pair[0].end, pair[1].start, "after op {n}");
            }
            let (mut edges, mut v, mut i, mut p) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for b in &blocks {
                if let BlockChannel::Digital { edges: e, .. } = &b.channels[2] {
                    edges.extend(e);
                }
                v.extend(codes(b.channels[0].values().unwrap()));
                i.extend(codes(b.channels[1].values().unwrap()));
                p.extend(codes(&snapshot.derived(b, 0).unwrap()));
            }
            assert_eq!((edges, v, i, p), pushed(&ops[..=n]), "after op {n}");
            let gaps: Vec<Gap> = snapshot.gaps(2).copied().collect();
            let want: Vec<Gap> = ops[..=n]
                .iter()
                .filter_map(|op| match op {
                    Op::Gap(g) => Some(*g),
                    _ => None,
                })
                .collect();
            assert_eq!(gaps, want, "after op {n}");
            let runs: Vec<Break> = snapshot.runs(0).copied().collect();
            assert_eq!(runs, snapshot.runs(1).copied().collect::<Vec<_>>());
            let marks = ops[..=n]
                .iter()
                .filter(|op| matches!(op, Op::Mark(_)))
                .count();
            assert_eq!(snapshot.annotations().count(), marks, "after op {n}");
            rows_seen = rows_seen.max(snapshot.block_count());
        }
        assert!(rows_seen >= 2, "{block_bytes} {batch}: blocks were written");
        let last = w.snapshot();
        let runs: Vec<Break> = last.runs(0).copied().collect();
        assert_eq!(
            runs,
            [
                Break {
                    index: 0,
                    tick: 0,
                    lost: 0
                },
                Break {
                    index: 12,
                    tick: 160,
                    lost: 4
                },
                Break {
                    index: 21,
                    tick: 400,
                    lost: 0
                }
            ]
        );
        // The held finest level is the footer's finest level, as far as it has settled: one
        // chunk, since 45 buckets of 16 ticks cover the session.
        let held = &last.overview()[0];
        assert_eq!((held.first, held.chunks.len()), (0, 1));
        let settled = held.buckets() / 8 * 8;
        assert!(settled > 0);
        let f = w.finish(720, &[720], Finish::default()).unwrap();
        file.extend(&f.tail);
        let footer = Footer::parse_file(&file).unwrap();
        assert_eq!(
            footer.overview[0].slice(0, settled),
            held.chunks[0].slice(0, settled)
        );
    }
}

#[test]
fn a_snapshot_goes_to_another_thread() {
    fn shared<T: Send + Sync>() {}
    shared::<Snapshot>();
}

/// Bucket `i` of each of `level`'s channels, to compare one bucket at a time.
fn bucket(level: &Level, i: u64) -> Vec<String> {
    level
        .channels
        .iter()
        .map(|c| match c.analog() {
            None => format!("{:?}", c.digital(i).unwrap()),
            Some(AnalogOverview::F32(a)) => format!("{:?}", a.bucket(i as usize)),
            Some(AnalogOverview::F64(a)) => format!("{:?}", a.bucket(i as usize)),
            Some(AnalogOverview::U64(a)) => format!("{:?}", a.bucket(i as usize)),
            Some(AnalogOverview::I64(a)) => format!("{:?}", a.bucket(i as usize)),
        })
        .collect()
}

/// `piece`'s buckets are the finished level's from bucket `first` on. Its blocks hold the
/// finished level, 65,536 buckets each.
fn same_as_finished(piece: &Level, first: u64, finished: &[OverviewBlock]) {
    let block = &finished[(first / BLOCK_BUCKETS) as usize];
    let at = first - block.first;
    let whole = piece.buckets / 8 * 8;
    assert_eq!(
        piece.slice(0, whole),
        block.level.slice(at, whole),
        "buckets {first} to {}",
        first + whole
    );
    for i in whole..piece.buckets {
        assert_eq!(
            bucket(piece, i),
            bucket(&block.level, at + i),
            "bucket {}",
            first + i
        );
    }
}

/// The session's channels at a pair every 100 ticks for 6,000,000 ticks: past 2^21, where the
/// finest level, 2^4, has more than 2^17 buckets and goes in overview blocks, and into its sixth.
#[test]
fn a_snapshot_past_the_heads_bucket_count_reads_every_finest_bucket_the_finished_file_has() {
    let (mut d, _) = session();
    for c in &mut d.channels {
        if let DeclaredKind::Analog { period, .. } = &mut c.data {
            *period = Ratio::integer(100).unwrap();
        }
    }
    const PAIRS: u64 = 60_000;
    // Blocks close on their size, a few hundred pairs each, and at a flush every 2,000 pairs. In
    // batches of 3, an overview block that settles with the first or second of a batch waits
    // with it, unwritten, until the third or the flush.
    for batch in [1, 3] {
        let mut w = Writer::new(
            d.clone(),
            WriteOptions {
                level: 3,
                block_bytes: 4096,
                batch,
            },
        )
        .unwrap();
        let mut file = w.take_output();
        w.run(0, 0, 0).unwrap();
        w.run(1, 0, 0).unwrap();
        let mut kept: Vec<Snapshot> = Vec::new();
        // Each different count of written and queued blocks, while some are queued.
        let mut queued_seen = Vec::new();
        for i in 0..PAIRS {
            let t = i as i64 * 100;
            if i > 0 && i % 2000 == 0 {
                w.flush(t).unwrap();
            }
            w.sample(0, 3300 + i % 7).unwrap();
            w.sample(1, 1_000_000 + (i * 7919) % 100_000).unwrap();
            if i % 37 == 0 {
                w.edge(2, t + 50).unwrap();
            }
            // A queued overview block waits a block at least, a few hundred pairs.
            if i % 50 != 49 {
                continue;
            }
            let s = w.snapshot();
            file.extend(w.take_output());
            let written = s
                .overview_blocks()
                .first()
                .map_or(0, |l| u64::from(l.blocks));
            let queued = (s.overview()[0].first > written * BLOCK_BUCKETS)
                .then_some((written, s.overview()[0].first));
            if queued.is_some_and(|q| !queued_seen.contains(&q)) || i % 6000 == 5999 {
                queued_seen.extend(queued);
                kept.push(s);
            }
        }
        if batch > 1 {
            assert!(
                queued_seen.iter().any(|&(written, _)| written > 0),
                "a snapshot held a queued overview block after a written one: {queued_seen:?}"
            );
        }
        let f = w.finish(PAIRS as i64 * 100, &[PAIRS as i64 * 100], Finish::default());
        file.extend(f.unwrap().tail);
        let footer = Footer::parse_file(&file).unwrap();
        assert_eq!(footer.header.overview_blocks[0].shift, 4);
        let finished: Vec<OverviewBlock> = (0..footer.header.overview_blocks[0].blocks as usize)
            .map(|k| {
                let (offset, len) = footer.overview_block_location(0, k);
                let frame = &file[offset as usize..][..len as usize];
                footer.decode_overview_block(0, k, frame).unwrap()
            })
            .collect();
        let mut past_head = 0;
        let mut decoded = 0;
        for s in &kept {
            let held = &s.overview()[0];
            assert_eq!(held.shift, 4);
            let mut blocks = 0;
            if let Some(l) = s.overview_blocks().first() {
                assert_eq!(l.shift, 4);
                // A written block's frame is the finished file's: decode each once.
                for (k, want) in finished.iter().enumerate().take(l.blocks as usize) {
                    let at = s.overview_block_location(0, k);
                    assert_eq!(at, footer.overview_block_location(0, k), "block {k}");
                    if k >= decoded {
                        let frame = &file[at.0 as usize..][..at.1 as usize];
                        let block = s.decode_overview_block(0, k, frame).unwrap();
                        assert_eq!(block.first, k as u64 * BLOCK_BUCKETS);
                        assert_eq!(block.level, want.level, "block {k}, batch {batch}");
                        decoded = k + 1;
                    }
                }
                let queued = s.queued_overview_blocks(0).unwrap();
                for (j, block) in queued.iter().enumerate() {
                    let k = l.blocks as usize + j;
                    assert_eq!(block.first, k as u64 * BLOCK_BUCKETS);
                    assert_eq!(block.level, finished[k].level, "queued {k}, batch {batch}");
                }
                blocks = l.blocks as u64 + queued.len() as u64;
            }
            // The held buckets follow the blocks, and run to the last settled bucket: the
            // last before the open block's start.
            assert_eq!(held.first, blocks * BLOCK_BUCKETS);
            let mut first = held.first;
            for chunk in &held.chunks {
                same_as_finished(chunk, first, &finished);
                first += chunk.buckets;
            }
            assert_eq!(
                first,
                s.open_block().unwrap().start as u64 >> 4,
                "batch {batch}"
            );
            if first > HEAD_BUCKETS {
                past_head += 1;
            }
        }
        assert!(
            past_head >= 5,
            "batch {batch}: {past_head} snapshots past 2^17 buckets"
        );
    }
}

/// A recorder's file cut off after a gap whose end no block reached, with a mark past it: repair
/// drops the gap and ends what is known at its start, and the Recutter does the same.
#[test]
fn a_cut_off_input_recut_follows_repairs_gap_rule() {
    let (d, _) = session();
    let mut w = Writer::new(d, options(1 << 20)).unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    for i in 0..20u64 {
        if i == 3 {
            w.edge(2, 25).unwrap();
        }
        w.sample(0, 3300 + i).unwrap();
        w.sample(1, 1_000_000 + i).unwrap();
    }
    w.flush(200).unwrap();
    // DIO0 is lost from 150 to 300; the block that would hold 300 never comes.
    w.gap(
        2,
        Gap {
            from: 150,
            to: 300,
            level: true,
        },
    )
    .unwrap();
    let serde_json::Value::Object(m) = serde_json::json!({"id": "marker/400", "start": 400}) else {
        unreachable!()
    };
    w.annotation("verge", m).unwrap();
    let cut = w.take_output();
    let repaired = repair(&cut, 3).unwrap();
    // Length one past the mark; DIO0 known to the gap's start, the gap left out.
    assert_eq!(repaired.header.length, 401);
    let escapement_tickwave_format::header::ChannelKind::Digital { end, gaps, .. } =
        &repaired.header.channels[2].data
    else {
        panic!()
    };
    assert_eq!((*end, gaps.len()), (150, 0));
    let (out, f) = recut(&cut, 4096, options(1 << 20)).unwrap();
    assert_eq!(f.header.content_id, repaired.header.content_id);
    assert_eq!(f.header.channels, repaired.header.channels);
    assert_eq!(f.header.length, repaired.header.length);
    let fixed = [&cut[..repaired.keep as usize], &repaired.tail].concat();
    assert_eq!(items(&out), items(&fixed));
}

#[test]
fn a_cut_off_input_with_no_block_and_a_channel_declared_high_is_refused() {
    let (mut d, _) = session();
    d.channels[2].data = DeclaredKind::Digital { initial: true };
    let mut w = Writer::new(d, options(1 << 20)).unwrap();
    let serde_json::Value::Object(m) = serde_json::json!({"id": "marker/4", "start": 4}) else {
        unreachable!()
    };
    w.annotation("verge", m).unwrap();
    let err = recut(&w.take_output(), 4096, options(1 << 20))
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "the input is cut off with no block, and channel DIO0 is declared high at tick 0: no \
         block carries that level, so it isn't finished"
    );
}
