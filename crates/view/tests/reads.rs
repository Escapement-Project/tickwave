//! What a view decodes to answer: a selection's edge count from the index, with only the blocks
//! at its ends decoded; and a window's samples, with the block before it read only where a
//! sample there reaches into it.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use escapement_tickwave_format::decode::{DecodeError, Footer};
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
use escapement_tickwave_view::{Bytes, Cache, Drawn, Query, Recording};

/// A file in memory that records each read.
struct Counted {
    bytes: Vec<u8>,
    reads: Mutex<Vec<Range<u64>>>,
}

impl Bytes for Counted {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.bytes.len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.reads
            .lock()
            .unwrap()
            .push(offset..offset + buf.len() as u64);
        let at = offset as usize;
        buf.copy_from_slice(&self.bytes[at..at + buf.len()]);
        Ok(())
    }
}

impl Counted {
    fn new(bytes: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            bytes,
            reads: Mutex::default(),
        })
    }

    /// How many of the frames at `offsets` were read since the last call.
    fn frames_read(&self, offsets: &[u64]) -> usize {
        let reads = std::mem::take(&mut *self.reads.lock().unwrap());
        offsets
            .iter()
            .filter(|&&o| reads.iter().any(|r| r.start == o))
            .count()
    }
}

fn declaration(data: DeclaredKind) -> Declaration {
    let timebase = Ratio::integer(1000).unwrap();
    Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "C".into(),
            source_name: "c".into(),
            source: 0,
            data,
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
        meta: BTreeMap::new(),
        overview_shift: 4,
        requires: Vec::new(),
    }
}

/// Every block's frame offset in `file`'s channel 0.
fn block_offsets(file: &[u8]) -> Vec<u64> {
    Footer::parse_file(file)
        .unwrap()
        .block_refs(0, |at, len| {
            Ok::<_, DecodeError>(file[at as usize..][..len as usize].to_vec())
        })
        .unwrap()
        .iter()
        .map(|b| b.row.offset)
        .collect()
}

/// A selection's edges are counted from the index: across thousands of blocks on two pages, only
/// the blocks holding its ends are decoded, none where an end falls on a block's start, and the
/// count is the edges written there.
#[test]
fn a_selections_edges_are_counted_from_the_index() {
    let options = WriteOptions {
        level: 1,
        block_bytes: 1,
        promise_with_items: true,
        ..WriteOptions::default()
    };
    let mut w = Writer::new(
        declaration(DeclaredKind::Digital { initial: false }),
        options,
    )
    .unwrap();
    let edges: Vec<i64> = (0..10_000).map(|k| 4 * k + k % 3).collect();
    for &t in &edges {
        w.edge(0, t).unwrap();
    }
    let length = 40_100;
    let mut file = w.take_output();
    file.extend(w.finish(length, &[length], Finish::default()).unwrap().tail);
    let blocks = block_offsets(&file);
    assert!(blocks.len() > 4096, "{} blocks", blocks.len());
    let bytes = Counted::new(file);
    let rec = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    let cancel = AtomicBool::new(false);
    let truth = |a: i64, b: i64| edges.iter().filter(|&&t| a <= t && t < b).count() as u64;
    for (a, b, decoded) in [
        (5, 39_000, 2),
        (-100, 50_000, 0),
        (17_001, 17_002, 1),
        (0, length, 0),
        (30_000, 20_000, 0),
    ] {
        let start = bytes.frames_read(&blocks);
        assert_eq!(start, 0);
        let n = rec.edge_count(0, a, b, &cancel).unwrap();
        assert_eq!(n, truth(a, b), "{a}..{b}");
        assert!(
            bytes.frames_read(&blocks) <= decoded,
            "{a}..{b}: more than {decoded} blocks decoded"
        );
    }
    // Against the edges read, at every few ticks.
    for a in (0..length).step_by(997) {
        let b = a + 3_001;
        let read = rec.edges(0, a, b, &cancel).unwrap().1.len() as u64;
        assert_eq!(rec.edge_count(0, a, b, &cancel).unwrap(), read, "{a}..{b}");
    }
}

/// A window past the last sample, or between a break's runs, reads no block: no sample outside
/// it reaches into it, by its line or its time. Inside a run, between two samples, both are read,
/// and so is the last sample where its time reaches in.
#[test]
fn a_window_reads_a_neighbour_only_where_it_reaches_in() {
    let options = WriteOptions {
        level: 1,
        block_bytes: 1,
        promise_with_items: true,
        ..WriteOptions::default()
    };
    let d = declaration(DeclaredKind::Analog {
        unit: "V".into(),
        period: Ratio::integer(10).unwrap(),
        coding: AnalogCoding::F32,
        significant: None,
        phase: None,
    });
    let mut w = Writer::new(d, options).unwrap();
    // Samples at 0, 10, ..., 90, then a break to 1,000, ..., 1,090.
    w.run(0, 0, 0).unwrap();
    for i in 0..10 {
        w.sample(0, i as f32).unwrap();
    }
    w.run(0, 1000, 0).unwrap();
    for i in 0..10 {
        w.sample(0, 10.0 + i as f32).unwrap();
    }
    let mut file = w.take_output();
    file.extend(w.finish(10_000, &[], Finish::default()).unwrap().tail);
    let blocks = block_offsets(&file);
    let bytes = Counted::new(file);
    let rec = Recording::open(bytes.clone(), Cache::new(0)).unwrap();
    let cancel = AtomicBool::new(false);
    let points = |window: Range<i64>| {
        let q = Query {
            window,
            columns: 100,
            channels: vec![0],
            ..Query::default()
        };
        bytes.frames_read(&blocks);
        let a = rec.query(&q, &cancel).unwrap();
        let Drawn::Analog {
            samples: Some(s), ..
        } = &a.channels[0]
        else {
            panic!("{:?}", a.channels[0]);
        };
        let ticks: Vec<i64> = s.iter().map(|p| p.tick).collect();
        (ticks, bytes.frames_read(&blocks))
    };
    // A query's samples are `around`'s, the window and a width either side. Past the last
    // sample's time, and inside the break past the first run's last.
    assert_eq!(points(5_000..6_000), (vec![], 0));
    assert_eq!(points(400..600), (vec![], 0));
    assert_eq!(points(101..102), (vec![], 0));
    // The last sample's time, to 1,100, reaches into it.
    assert_eq!(points(1_099..1_100).0, [1_090]);
    // Between two samples of a run: the line joins them.
    assert_eq!(points(44..46).0, [40, 50]);
    // Up to a break: the run's last two, and not the next run's first, which no line joins.
    assert_eq!(points(92..95).0, [80, 90]);
}

/// A live recording's edges are counted from the blocks a writer holds and has written.
#[test]
fn a_live_recordings_edges_are_counted_as_read() {
    let options = WriteOptions {
        level: 1,
        block_bytes: 1,
        promise_with_items: true,
        ..WriteOptions::default()
    };
    let mut w = Writer::new(
        declaration(DeclaredKind::Digital { initial: false }),
        options,
    )
    .unwrap();
    let edges: Vec<i64> = (0..300).map(|k| 4 * k + k % 3).collect();
    for &t in &edges {
        w.edge(0, t).unwrap();
    }
    let snapshot = w.snapshot();
    let file = w.take_output();
    let rec = Recording::live(snapshot, Arc::new(file), Cache::new(1 << 20)).unwrap();
    let cancel = AtomicBool::new(false);
    for (a, b) in [(0, 2000), (5, 600), (601, 602), (900, 1200)] {
        let truth = edges.iter().filter(|&&t| a <= t && t < b).count() as u64;
        let read = rec.edges(0, a, b, &cancel).unwrap().1.len() as u64;
        assert_eq!(
            (rec.edge_count(0, a, b, &cancel).unwrap(), read),
            (truth, truth),
            "{a}..{b}"
        );
    }
}
