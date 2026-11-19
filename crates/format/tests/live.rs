//! A live recorder's file afterwards and while it is written: the Recutter that writes a flushed
//! file again with full-size blocks, and the snapshot a view in the same process reads.

use std::collections::BTreeMap;
use std::path::PathBuf;

use escapement_tickwave_format::decode::{Block, BlockChannel, Footer, Index, from_memory};
use escapement_tickwave_format::header::{
    AnalogCoding, ChannelKind, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, Header,
    IntCoding, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{Break, Gap, Ratio, Sample};
use escapement_tickwave_format::overview::{BLOCK_BUCKETS, Level};
use escapement_tickwave_format::reader::{Ending, Reader};
use escapement_tickwave_format::recut::{RecutError, Recutter, shifts_for};
use escapement_tickwave_format::repair::{RepairError, repair};
use escapement_tickwave_format::samples::Values;
use escapement_tickwave_format::snapshot::Snapshot;
use escapement_tickwave_format::writer::{Compression, Finish, Finished, WriteOptions, Writer};

mod common;

fn energy() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/fixtures/energy.tickwave");
    std::fs::read(path).unwrap()
}

fn options(block_bytes: usize) -> WriteOptions {
    WriteOptions {
        level: 3,
        block_bytes,
        ..WriteOptions::default()
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
    let index = Index::of_file(file).unwrap();
    let n = index.blocks.len();
    let (mut edges, mut samples) = (vec![Vec::new(); n], vec![Vec::new(); n]);
    for c in 0..n {
        for block in index.channel_blocks(file, c).unwrap() {
            match block.data {
                BlockChannel::Digital { edges: e, .. } => edges[c].extend(e),
                BlockChannel::Analog { values, .. } => samples[c].extend(values.iter()),
            }
        }
    }
    (edges, samples)
}

/// Each channel's data, without where its blocks are.
fn data(h: &Header) -> Vec<ChannelKind> {
    h.channels.iter().map(|c| c.data.clone()).collect()
}

#[test]
fn a_flushed_file_recut_has_full_size_blocks_and_the_same_content() {
    let file = energy();
    let before = Footer::parse_file(&file).unwrap();
    assert_eq!(before.header.channels[0].blocks, 6, "the fixture's flushes");
    for piece in [1 << 20, 997] {
        let (out, f) = recut(&file, piece, options(1 << 20)).unwrap();
        let after = Footer::parse_file(&out).unwrap();
        assert_eq!(after.header.content_id, before.header.content_id);
        assert_eq!(f.header, after.header);
        // Every channel's items in one block of 1 MiB.
        assert!(after.header.channels.iter().all(|c| c.blocks == 1));
        assert_eq!(items(&out), items(&file));
        // The same header, apart from where the blocks are: the bundle, the extras and the
        // derived channel come over.
        let mut header = after.header.clone();
        for (a, b) in header.channels.iter_mut().zip(&before.header.channels) {
            a.blocks = b.blocks;
        }
        assert_eq!(header, before.header);
        for o in 0..before.overview_channels() {
            assert_eq!(after.strip(o).unwrap(), before.strip(o).unwrap());
        }
        assert_eq!(
            after
                .read_sidecar(from_memory(&out))
                .map(|s| s.map(|s| s.files))
                .unwrap(),
            before
                .read_sidecar(from_memory(&file))
                .map(|s| s.map(|s| s.files))
                .unwrap()
        );
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
            phase: None,
        },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let d = Declaration {
        timebase,
        channels: vec![analog("V", int(1000)), analog("I", int(1_000_000_000))],
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
        overview_shift: 16,
        requires: Vec::new(),
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
    let before = Index::of_file(&file).unwrap();
    for c in 0..2 {
        assert_eq!(before.header().channels[c].blocks, 20);
        // Each second's block holds 10,000 codes of 4 bytes.
        assert!(
            before.blocks[c]
                .iter()
                .all(|b| b.row.decoded_length == 40_000)
        );
    }
    let target = 256 << 10;
    let (out, _) = recut(&file, 1 << 16, options(target)).unwrap();
    let after = Index::of_file(&out).unwrap();
    assert_eq!(after.header().content_id, before.header().content_id);
    // 800 KB a channel in blocks closed once their items before a promise reach 256 KiB: the
    // Recutter promises each input block's end, every 10,000 codes, so a block closes at the
    // first such end past 65,536 codes, 70,000, both channels at once.
    for (c, blocks) in after.blocks.iter().enumerate().take(2) {
        let sizes: Vec<u32> = blocks.iter().map(|b| b.row.decoded_length).collect();
        assert_eq!(sizes, [280_000, 280_000, 240_000], "channel {c}");
    }
    assert_eq!(
        after.blocks[0].iter().map(|b| b.start).collect::<Vec<_>>(),
        after.blocks[1].iter().map(|b| b.start).collect::<Vec<_>>(),
        "V's and I's blocks close together"
    );
    assert_eq!(items(&out), items(&file));
}

/// The Recutter gives each channel the shift the 64-items rule gives its own items, or the
/// shifts it is told, and the content stays.
#[test]
fn a_recut_takes_each_channels_shift_from_its_items_or_as_given() {
    let file = energy();
    let before = Footer::parse_file(&file).unwrap();
    // V and I, a pair every 960 ticks: log2(64 x 960) = 15.9; the pins by their edges over
    // 2,880,000 ticks: range 3 edges, log2(61,440,000) = 25.9; under_range 4, 25.46; DIO0 8,
    // 24.46.
    let shifts = shifts_for(&before.header);
    assert_eq!(shifts, [16, 16, 26, 25, 24]);
    let mut r = Recutter::new(options(1 << 20));
    r.shifts(shifts.clone());
    r.feed(&file).unwrap();
    let mut out = r.take_output();
    out.extend(r.finish().unwrap().tail);
    let after = Footer::parse_file(&out).unwrap();
    assert_eq!(after.header.content_id, before.header.content_id);
    assert_eq!(
        (0..5)
            .map(|c| after.header.shift(c).unwrap())
            .collect::<Vec<_>>(),
        shifts
    );
    // P takes the coarser of its operands'.
    assert_eq!(after.header.shift(5), Some(16));
    // A count of shifts that isn't the channels' is refused.
    let mut r = Recutter::new(options(1 << 20));
    r.shifts(vec![4]);
    let err = r.feed(&file).unwrap_err();
    assert_eq!(err.to_string(), "1 overview shifts given, for 5 channels");
}

#[test]
fn a_cut_off_file_recut_gives_what_repair_gives() {
    let file = energy();
    let footer_at = Footer::parse_file(&file).unwrap().trailer.footer_offset as usize;
    for at in [footer_at / 3, footer_at / 2, footer_at - 5, footer_at + 9] {
        let cut = &file[..at];
        let (out, _) = recut(cut, 4096, options(1 << 20)).unwrap();
        let repaired = match repair(cut, 3) {
            // Halfway, DIO0's loss from 960,000 to 1,008,000 began before the latest
            // known-through tick, and its record lies before the cut, while an annotation after
            // the cut takes the length past the loss's end. The header can't leave the gap out,
            // nor repair take its record out of the bytes it keeps; recut writes the file again
            // without it.
            Err(RepairError::GapRecordKept { channel, to, .. }) if at == footer_at / 2 => {
                assert_eq!((channel.as_str(), to), ("DIO0", 1_008_000));
                let mut r = Reader::default();
                r.feed(&out);
                while r.next_event().unwrap().is_some() {}
                assert_eq!(r.end().unwrap(), Ending::Complete);
                continue;
            }
            repaired => repaired.unwrap_or_else(|e| panic!("cut at {at}: {e}")),
        };
        let head = Footer::parse_file(&out).unwrap();
        assert_eq!(
            head.header.content_id, repaired.header.content_id,
            "cut at {at}"
        );
        assert_eq!(data(&head.header), data(&repaired.header), "cut at {at}");
        assert_eq!(head.header.length, repaired.header.length, "cut at {at}");
        let fixed = [&cut[..repaired.keep as usize], &repaired.tail].concat();
        // The same generator files, joined from each one's bundle and parts, whatever they're
        // named.
        let generated = |f: &[u8]| -> Vec<String> {
            Footer::parse_file(f)
                .unwrap()
                .read_sidecar(from_memory(f))
                .unwrap()
                .map(|s| {
                    s.files
                        .into_iter()
                        .filter(|(name, _)| name.ends_with(".jsonl"))
                        .map(|(_, text)| text)
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(generated(&out), generated(&fixed), "cut at {at}");
        assert!(!generated(&out).is_empty());
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

/// Channel `c`'s blocks as `snapshot` gives them, those in `file` and those not yet written.
/// Every annotation line a snapshot holds, with its generator: the parts in `file`, then those
/// not yet written.
fn snapshot_lines(snapshot: &Snapshot, file: &[u8]) -> Vec<(String, String)> {
    let mut parts = Vec::new();
    for row in snapshot.part_rows_from(0, from_memory(file)).unwrap() {
        let frame = &file[row.offset as usize..][..row.length as usize];
        parts.push(snapshot.decode_part(&row, frame).unwrap());
    }
    parts.extend(snapshot.unwritten_parts().unwrap());
    parts
        .iter()
        .flat_map(|p| p.lines().map(|l| (p.source.clone(), l.to_owned())))
        .collect()
}

fn snapshot_blocks(snapshot: &Snapshot, file: &[u8], c: usize) -> Vec<Block> {
    let mut blocks: Vec<Block> = snapshot
        .block_refs(c, &[], from_memory(file))
        .unwrap()
        .iter()
        .map(|b| {
            let frame = &file[b.row.offset as usize..][..b.row.length as usize];
            snapshot.decode_block(c, b, frame).unwrap()
        })
        .collect();
    let unwritten = snapshot.unwritten_blocks(c).unwrap();
    // One at a time, the same.
    for (j, b) in unwritten.iter().enumerate() {
        assert_eq!(&snapshot.unwritten_block(c, j).unwrap(), b);
    }
    assert!(snapshot.unwritten_block(c, unwritten.len()).is_err());
    blocks.extend(unwritten);
    blocks
}

#[test]
fn a_snapshot_with_the_bytes_written_so_far_gives_every_item_pushed() {
    let (d, ops) = session();
    // Blocks close at flushes and, at 64 bytes and a promise each op, on their size.
    for (block_bytes, compression, promise) in [
        (64, Compression::Spread, true),
        (64, Compression::Offload, true),
        (64, Compression::Spread, false),
        (1 << 20, Compression::Spread, false),
    ] {
        let mut w = Writer::new(
            d.clone(),
            WriteOptions {
                level: 3,
                block_bytes,
                compression,
                ..WriteOptions::default()
            },
        )
        .unwrap();
        let mut file = w.take_output();
        let mut written_seen = 0;
        let mut pair_at: i64 = -10;
        for (n, op) in ops.iter().enumerate() {
            apply(&mut w, op);
            match op {
                Op::Run(t, _) => pair_at = t - 10,
                Op::Pair(..) => pair_at += 10,
                _ => {}
            }
            // Every item of every channel before the last pair's tick is in.
            if promise && matches!(op, Op::Pair(..)) && pair_at > w.promised() {
                w.promise(pair_at).unwrap();
            }
            let snapshot = w.snapshot();
            // The caller writes what the writer has given out, then publishes the snapshot.
            file.extend(w.take_output());
            assert!(snapshot.position() <= file.len() as u64);
            let mut channel_items = Vec::new();
            for c in 0..3 {
                let blocks = snapshot_blocks(&snapshot, &file, c);
                assert_eq!(blocks[0].start, 0);
                for pair in blocks.windows(2) {
                    assert_eq!(pair[0].end, pair[1].start, "channel {c}, after op {n}");
                }
                assert_eq!(blocks.last().unwrap().end, i64::MAX, "the open block");
                let mut before = 0;
                let mut items = Vec::new();
                for b in &blocks {
                    match &b.data {
                        BlockChannel::Analog {
                            first_index,
                            values,
                        } => {
                            assert_eq!(*first_index, before, "channel {c}, after op {n}");
                            before += values.len() as u64;
                            items.extend(codes(values));
                        }
                        BlockChannel::Digital { edges, .. } => {
                            items.extend(edges.iter().map(|&t| t as u64))
                        }
                    }
                }
                written_seen = written_seen.max(snapshot.channel(c).unwrap().blocks.rows);
                channel_items.push(items);
            }
            let p: Vec<u64> = channel_items[1]
                .iter()
                .zip(&channel_items[0])
                .map(|(a, b)| a * b)
                .collect();
            let edges: Vec<i64> = channel_items[2].iter().map(|&t| t as i64).collect();
            let (want_edges, v, i, want_p) = pushed(&ops[..=n]);
            assert_eq!(edges, want_edges, "after op {n}");
            assert_eq!(
                (&channel_items[0], &channel_items[1]),
                (&v, &i),
                "after op {n}"
            );
            assert_eq!(p, want_p, "after op {n}");
            let gaps: Vec<Gap> = snapshot.gaps_from(2, 0, from_memory(&file)).unwrap();
            let want: Vec<Gap> = ops[..=n]
                .iter()
                .filter_map(|op| match op {
                    Op::Gap(g) => Some(*g),
                    _ => None,
                })
                .collect();
            assert_eq!(gaps, want, "after op {n}");
            let runs: Vec<Break> = snapshot.runs_from(0, 0, from_memory(&file)).unwrap();
            assert_eq!(runs, snapshot.runs_from(1, 0, from_memory(&file)).unwrap());
            let marks = ops[..=n]
                .iter()
                .filter(|op| matches!(op, Op::Mark(_)))
                .count();
            assert_eq!(
                snapshot_lines(&snapshot, &file).len(),
                marks,
                "after op {n}"
            );
        }
        assert_eq!(written_seen, 0, "every page still open: few blocks");
        let last = w.snapshot();
        file.extend(w.take_output());
        let runs: Vec<Break> = last.runs_from(0, 0, from_memory(&file)).unwrap();
        assert_eq!(
            runs,
            [
                Break {
                    index: 0,
                    tick: 0,
                    lost: 0,
                    phase: 0,
                },
                Break {
                    index: 12,
                    tick: 160,
                    lost: 4,
                    phase: 0,
                },
                Break {
                    index: 21,
                    tick: 400,
                    lost: 0,
                    phase: 0,
                }
            ]
        );
        // V's finest level, 2^4, held as far as it has settled: 45 buckets of 16 ticks cover the
        // session, so it is the strip and none of it goes in blocks.
        let held = last.overview(0).unwrap().levels[0].held.clone();
        assert_eq!(held.first, 0);
        let settled = held.buckets() / 8 * 8;
        assert!(settled > 0);
        let f = w.finish(720, &[720], Finish::default()).unwrap();
        file.extend(&f.tail);
        let footer = Footer::parse_file(&file).unwrap();
        assert_eq!(
            footer.strip(0).unwrap().slice(0, settled),
            held.chunks[0].slice(0, settled)
        );
    }
}

#[test]
fn a_snapshot_goes_to_another_thread() {
    fn shared<T: Send + Sync>() {}
    shared::<Snapshot>();
}

/// The session's channels at a pair every 100 ticks for 6,000,000 ticks: V's finest level,
/// 2^4, has 375,000 buckets and goes in overview blocks, 92 of them. Then at a pair every 64
/// ticks, V the same from pair 10,000 to 40,000: its overview blocks there, 1,024 pairs each,
/// are alike, and a snapshot holds a run of them.
#[test]
fn a_snapshot_reads_every_settled_bucket_the_finished_file_has() {
    const PAIRS: u64 = 60_000;
    for (p, alike, compression) in [
        (100, 0..0, Compression::Spread),
        (100, 0..0, Compression::Offload),
        (64, 10_000..40_000, Compression::Spread),
    ] {
        let (mut d, _) = session();
        for c in &mut d.channels {
            if let DeclaredKind::Analog { period, .. } = &mut c.data {
                *period = Ratio::integer(p).unwrap();
            }
        }
        let mut w = Writer::new(
            d.clone(),
            WriteOptions {
                level: 3,
                block_bytes: 4096,
                compression,
                ..WriteOptions::default()
            },
        )
        .unwrap();
        let mut file = w.take_output();
        w.run(0, 0, 0).unwrap();
        w.run(1, 0, 0).unwrap();
        let mut kept: Vec<Snapshot> = Vec::new();
        for i in 0..PAIRS {
            let t = (i * p) as i64;
            if i > 0 && i % 2000 == 0 {
                w.flush(t).unwrap();
            }
            w.promise(t).unwrap();
            w.sample(
                0,
                if alike.contains(&i) {
                    3300
                } else {
                    3300 + i % 7
                },
            )
            .unwrap();
            w.sample(1, 1_000_000 + (i * 7919) % 100_000).unwrap();
            if i % 37 == 0 {
                w.edge(2, t + 50).unwrap();
            }
            if i % 1999 == 0 {
                kept.push(w.snapshot());
            }
            file.extend(w.take_output());
        }
        let length = (PAIRS * p) as i64;
        let f = w.finish(length, &[length], Finish::default());
        file.extend(f.unwrap().tail);
        let index = Index::of_file(&file).unwrap();
        assert_eq!(index.header().channels[0].overview_blocks[0].shift, 4);
        let finished = index.level_in(&file, 0, 0).unwrap();
        let (mut in_blocks, mut runs) = (0, 0);
        for s in &kept {
            let level = &s.overview(0).unwrap().levels[0];
            runs += usize::from(level.run.as_ref().is_some_and(|r| r.n > 1));
            assert_eq!(level.shift, 4);
            // Rows in the file, then blocks not yet written, then the run, then the buckets after.
            let mut got = Level::empty(4, s.overview(0).unwrap().column);
            for r in s.level_refs(0, 0, &[], from_memory(&file)).unwrap() {
                let frame = &file[r.row.offset as usize..][..r.row.length as usize];
                let b = s.decode_overview(0, 0, &r, frame).unwrap();
                assert_eq!(b.first, got.buckets);
                for _ in 0..b.n {
                    got.append(&b.level);
                }
            }
            let unwritten = s.unwritten_overview(0, 0).unwrap();
            for (j, b) in unwritten.iter().enumerate() {
                assert_eq!(b.first, got.buckets);
                for _ in 0..b.n {
                    got.append(&b.level);
                }
                // One at a time, the same.
                let one = s.unwritten_overview_block(0, 0, j).unwrap();
                assert_eq!((one.first, one.n, &one.level), (b.first, b.n, &b.level));
            }
            assert!(s.unwritten_overview_block(0, 0, unwritten.len()).is_err());
            if level.in_blocks {
                in_blocks += 1;
            }
            assert_eq!(level.held.first, got.buckets);
            for chunk in &level.held.chunks {
                got.append(chunk);
            }
            let n = got.buckets / 8 * 8;
            assert!(n <= finished.buckets);
            assert_eq!(got.slice(0, n), finished.slice(0, n), "{compression:?}");
            // Settled to what the known-through records and V's blocks say.
            let known = s.known_through().max(0) as u64;
            assert!(got.buckets >= known >> 4, "{} {}", got.buckets, known >> 4);
        }
        assert!(
            in_blocks >= 20,
            "{compression:?}: {in_blocks} snapshots in blocks"
        );
        assert!(
            alike.is_empty() || runs >= 5,
            "{runs} snapshots with a run of alike blocks"
        );
        let _ = BLOCK_BUCKETS;
    }
}

/// A recorder's file cut off after a gap whose end no block reached, reported after its last
/// flush, with a mark past it: repair cuts the file at that flush's known-through record and
/// writes the gap's record again after it, since its `from` lies before the flush (FORMAT.md,
/// "Repairing a cut-off file"), so the pin reads as known to the gap's `from` and the gap, which
/// reaches that end, is left out. The Recutter does the same.
#[test]
fn a_cut_off_input_recut_follows_repairs_cut() {
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
    // Length one past the mark; DIO0 known to the gap's `from`, the gap left out.
    assert_eq!(repaired.header.length, 401);
    let ChannelKind::Digital { end, gaps, .. } = &repaired.header.channels[2].data else {
        panic!()
    };
    assert_eq!((*end, gaps.len()), (150, 0));
    let (out, f) = recut(&cut, 4096, options(1 << 20)).unwrap();
    assert_eq!(f.header.content_id, repaired.header.content_id);
    assert_eq!(data(&f.header), data(&repaired.header));
    assert_eq!(f.header.length, repaired.header.length);
    let fixed = [&cut[..repaired.keep as usize], &repaired.tail].concat();
    assert_eq!(items(&out), items(&fixed));
}

/// A cut-off input with no block and a digital channel declared high recuts as repair finishes
/// it: the channel never known, its declared level kept.
#[test]
fn a_cut_off_input_with_no_block_and_a_channel_declared_high_recuts_as_repair_finishes_it() {
    let (mut d, _) = session();
    d.channels[2].data = DeclaredKind::Digital { initial: true };
    let mut w = Writer::new(d, options(1 << 20)).unwrap();
    let serde_json::Value::Object(m) = serde_json::json!({"id": "marker/4", "start": 4}) else {
        unreachable!()
    };
    w.annotation("verge", m).unwrap();
    let cut = w.take_output();
    let (_, f) = recut(&cut, 4096, options(1 << 20)).unwrap();
    let repaired = escapement_tickwave_format::repair::repair(&cut, 3).unwrap();
    assert_eq!(f.header.content_id, repaired.header.content_id);
    let ChannelKind::Digital { initial, end, .. } = f.header.channels[2].data else {
        panic!()
    };
    assert!(initial);
    assert_eq!(Some(end), f.header.start(2));
}

/// The writer holds no gap, break or annotation once its list or part is in the file
/// (FORMAT.md, "Memory"): a snapshot gives them from the file's pages and parts, read through a
/// fetch, then from the open page and what waits. Past 4,096 gaps and breaks the lists' pages
/// are in the file, and a snapshot holds only the open page and the waiting ones; a reader that
/// asks from any entry on gets the rest, page boundaries included. The writer keeps the runs its
/// open block's samples need.
#[test]
fn a_snapshots_gaps_breaks_and_annotations_come_from_the_files_pages() {
    let timebase = Ratio::integer(1000).unwrap();
    let channel = |name: &str, data| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data,
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let d = Declaration {
        timebase,
        channels: vec![
            channel("D", DeclaredKind::Digital { initial: false }),
            channel(
                "V",
                DeclaredKind::Analog {
                    unit: "V".into(),
                    period: Ratio::integer(1).unwrap(),
                    coding: AnalogCoding::F32,
                    significant: None,
                    phase: None,
                },
            ),
        ],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("test".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, options(64)).unwrap();
    let mut file = w.take_output();
    let (mut gaps, mut runs, mut lines) = (Vec::new(), Vec::new(), Vec::new());
    let n = 9_000i64;
    for k in 0..n {
        let t = 10 * k;
        w.edge(0, t).unwrap();
        let gap = Gap {
            from: t + 1,
            to: t + 5,
            level: k % 2 == 0,
        };
        w.gap(0, gap).unwrap();
        gaps.push(gap);
        let lost = u64::from(k % 3 == 0 && k > 0);
        w.run(1, t + 2, lost).unwrap();
        w.sample(1, k as f32).unwrap();
        runs.push(Break::new(k as u64, t + 2, lost));
        if k % 4 == 0 {
            let m =
                serde_json::json!({"id": format!("m/{k}"), "start": t, "text": "x".repeat(200)});
            let serde_json::Value::Object(m) = m else {
                unreachable!()
            };
            w.annotation("verge", m).unwrap();
            lines.push(t);
        }
        // Behind the items, so a block closes with samples of several runs left open.
        if t >= 25 {
            w.promise(t - 25).unwrap();
        }
        if k % 1000 != 999 && k != 4100 {
            continue;
        }
        let snapshot = w.snapshot();
        file.extend(w.take_output());
        let held = snapshot.channel(0).unwrap();
        let listed = held.gaps.table.as_ref().unwrap().rows;
        assert!(held.gaps.open.len() < 4096, "after {k}: one page open");
        assert!(
            held.gaps.waiting.len() < 100,
            "after {k}: {}",
            held.gaps.waiting.len()
        );
        let ran = snapshot.channel(1).unwrap();
        assert!(ran.breaks.open.len() < 4096 && ran.breaks.waiting.len() < 100);
        for from in [0, 1, 4095, 4096, 4097, 8191, 8192, 8193, gaps.len() as u64] {
            let from = from.min(gaps.len() as u64);
            let got = snapshot.gaps_from(0, from, from_memory(&file)).unwrap();
            assert_eq!(got, gaps[from as usize..], "after {k}, gaps from {from}");
            let got = snapshot.runs_from(1, from, from_memory(&file)).unwrap();
            assert_eq!(got, runs[from as usize..], "after {k}, runs from {from}");
        }
        if k >= 4100 {
            assert!(listed >= 4096, "after {k}: {listed} gaps in pages");
        }
        let got: Vec<i64> = snapshot_lines(&snapshot, &file)
            .iter()
            .map(|(_, l)| {
                let fields: serde_json::Value = serde_json::from_str(l).unwrap();
                fields["start"].as_i64().unwrap()
            })
            .collect();
        assert_eq!(got, lines, "after {k}");
        // Later, parts are in the file, and a reader from part 1 on gets every row but the
        // first.
        let rows = snapshot.part_rows_from(0, from_memory(&file)).unwrap();
        if k < 4999 {
            continue;
        }
        assert!(rows.len() > 1, "after {k}: {} parts", rows.len());
        assert_eq!(
            snapshot.part_rows_from(1, from_memory(&file)).unwrap(),
            rows[1..]
        );
    }
}

/// Each analog coding's open block as it grows from snapshot to snapshot, closing now and then
/// between two runs of samples: `grown_block`, given the block the snapshot before decoded, or
/// the next channel's, two of them alike, is the block decoded whole.
#[test]
fn an_open_block_grown_decodes_as_it_does_whole() {
    let timebase = Ratio::integer(1000).unwrap();
    let analog = |name: &str, coding| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period: Ratio::integer(10).unwrap(),
            coding,
            significant: None,
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
            analog("A", AnalogCoding::F32),
            analog("A2", AnalogCoding::F32),
            analog("B", AnalogCoding::F64),
            analog("C", AnalogCoding::Int(IntCoding::new(12, false))),
            analog("D", AnalogCoding::Int(IntCoding::new(20, true))),
            DeclaredChannel {
                name: "E".into(),
                source_name: "e".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Default::default(),
                overview_shift: None,
                codec: None,
                requires: Vec::new(),
            },
        ],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 6,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, options(512)).unwrap();
    for c in 0..5 {
        w.run(c, 0, 0).unwrap();
    }
    let mut x = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let (mut n, mut grew) = (0u64, 0);
    let mut held: Vec<Option<Block>> = (0..6).map(|_| None).collect();
    for batch in 0..300 {
        for half in 0..2 {
            for _ in 0..next() % 40 {
                let v = next();
                w.sample(0, (v % 1000) as f32 / 8.0).unwrap();
                w.sample(1, (v % 999) as f32 / 4.0).unwrap();
                w.sample(2, (v % 777) as f64 / 3.0).unwrap();
                w.sample(3, v % 4096).unwrap();
                w.sample(4, (v % (1 << 20)) as i64 - (1 << 19)).unwrap();
                if v % 7 == 0 {
                    w.edge(5, (10 * n) as i64).unwrap();
                }
                n += 1;
            }
            if half == 0 && next() % 3 == 0 {
                w.flush((10 * n) as i64).unwrap();
            }
        }
        let s = w.snapshot();
        for c in 0..6 {
            let last = s.channel(c).unwrap().unwritten.len() - 1;
            let whole = s.unwritten_block(c, last).unwrap();
            for h in [&held[c], &held[(c + 1) % 6]].into_iter().flatten() {
                assert_eq!(
                    s.grown_block(c, last, h).unwrap(),
                    whole,
                    "batch {batch}, channel {c}"
                );
            }
            if let (Some(h), BlockChannel::Analog { values, .. }) = (&held[c], &whole.data)
                && h.start == whole.start
                && matches!(&h.data, BlockChannel::Analog { values: v, .. } if v.len() < values.len())
            {
                grew += 1;
            }
            held[c] = Some(whole);
        }
    }
    assert!(grew > 200, "{grew} open blocks grew");
}
