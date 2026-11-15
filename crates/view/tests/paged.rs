//! What a view reads of a finished file (FORMAT.md, "Reading"): `open` the trailer and the
//! footer and no page; a query a channel's pages for the span it covers, its directory pages on
//! the way, and the blocks there, each read and decoded once however many of the query's parts
//! take it.

use std::collections::BTreeMap;
use std::ops::{ControlFlow, Range};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use escapement_tickwave_format::decode::{DecodeError, Footer, from_memory};
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, IntCoding, SourceFormat,
    SourceHeader,
};
use escapement_tickwave_format::index::Entry;
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
use escapement_tickwave_view::{Bytes, Cache, MarkerQuery, Query, Recording};

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
    fn take(&self) -> Vec<Range<u64>> {
        std::mem::take(&mut *self.reads.lock().unwrap())
    }
}

const V: usize = 0;
const I: usize = 1;
const D: usize = 2;
const P: usize = 3;
/// V and I a pair every 10 ticks, a block each: 80,000 blocks each, in 20 pages under a
/// directory.
const PAIRS: u64 = 80_000;
/// D an edge every 10 ticks, a block each: 70,001 blocks in 18 pages under a directory.
const EDGES: i64 = 70_000;
const LENGTH: i64 = 800_000;

/// V and I, unsigned 32-bit codes, V's finest overview level 2^4 and I's 2^8, P their product,
/// and D a digital channel at the recording's 2^6.
fn recording() -> Vec<u8> {
    let timebase = Ratio::integer(1_000_000).unwrap();
    let analog = |name: &str, shift| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period: Ratio::integer(10).unwrap(),
            coding: AnalogCoding::Int(IntCoding::new(32, false)),
            significant: None,
        },
        shaping: Default::default(),
        overview_shift: Some(shift),
    };
    let d = Declaration {
        timebase,
        channels: vec![
            analog("V", 4),
            analog("I", 8),
            DeclaredChannel {
                name: "D".into(),
                source_name: "d".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Default::default(),
                overview_shift: None,
            },
        ],
        derived: vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            product: ["I".into(), "V".into()],
            significant: None,
        }],
        sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 6,
    };
    let mut w = Writer::new(
        d,
        WriteOptions {
            block_bytes: 1,
            promise_with_items: true,
            level: 1,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let mut file = w.take_output();
    w.run(V, 0, 0).unwrap();
    w.run(I, 0, 0).unwrap();
    for i in 0..PAIRS {
        // D's edge first where it shares the pair's tick: the encoder's order.
        if (i as i64) < EDGES {
            w.edge(D, 10 * i as i64).unwrap();
        }
        w.sample(V, 3300 + i % 17).unwrap();
        w.sample(I, 1_000_000 + (i * 7919) % 100_000).unwrap();
        if i % 1000 == 0 {
            file.extend(w.take_output());
        }
    }
    file.extend(w.take_output());
    file.extend(&w.finish(LENGTH, &[LENGTH], Finish::default()).unwrap().tail);
    file
}

/// Every page and directory page of the file, each its record's bytes, by channel: from the top
/// index down, read as the format crate reads them.
fn pages(file: &[u8]) -> Vec<(usize, Range<u64>)> {
    let footer = Footer::parse_file(file).unwrap();
    let mut out = Vec::new();
    let span = |e: &Entry| e.offset..e.offset + u64::from(e.length);
    for (o, ch) in footer.channels.iter().enumerate() {
        let tables = ch.blocks.iter().chain(ch.levels.iter().map(|l| &l.1));
        for table in tables {
            for depth in 0..table.levels.len() as u8 {
                for e in table.pages(depth, &mut from_memory(file)).unwrap() {
                    out.push((o, span(&e)));
                }
            }
            table
                .visit(&mut from_memory(file), |at, _| {
                    out.push((o, span(&at.entry)));
                    Ok::<_, DecodeError>(ControlFlow::Continue(()))
                })
                .unwrap();
        }
    }
    out.sort_by_key(|p| p.1.start);
    out.dedup();
    out
}

/// The pages `reads` touch, by channel.
fn touched(pages: &[(usize, Range<u64>)], reads: &[Range<u64>]) -> Vec<(usize, Range<u64>)> {
    pages
        .iter()
        .filter(|(_, p)| reads.iter().any(|r| r.start < p.end && p.start < r.end))
        .cloned()
        .collect()
}

#[test]
fn open_reads_the_trailer_and_the_footer_and_no_page() {
    let file = recording();
    let footer = Footer::parse_file(&file).unwrap();
    for c in [V, I, D] {
        let table = footer.channels[c].blocks.as_ref().unwrap();
        assert_eq!(table.levels.len(), 2, "channel {c} has a directory");
    }
    let all = pages(&file);
    assert!(all.len() > 30, "{} pages", all.len());
    let bytes = Arc::new(Counted {
        bytes: file.clone(),
        reads: Mutex::default(),
    });
    let rec = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    let reads = bytes.take();
    let footer_at = footer.trailer.footer_offset;
    let tail = (file.len() as u64).saturating_sub(32 << 10);
    // The preamble's magic, then the last 32 KiB, which hold the footer here: the last pages
    // before it come with them, and no page before those is read.
    assert!(footer_at > tail, "the footer lies in the last 32 KiB");
    assert_eq!(reads, [0..8, tail..file.len() as u64]);
    let before: Vec<_> = all.iter().filter(|p| p.1.end <= tail).collect();
    assert!(before.len() > 20, "{} pages before the tail", before.len());
    // The extent and the channels come from the footer alone.
    assert_eq!(rec.extent(), Some(0..LENGTH));
    assert_eq!(rec.channels().len(), 4);
    assert_eq!(bytes.take(), []);
}

fn query(window: Range<i64>, columns: u32) -> Query {
    Query {
        statistics: vec![window.clone()],
        window,
        columns,
        channels: vec![V, I, P, D],
        flags: vec![(V, D)],
        strip: Some((P, 64)),
        markers: MarkerQuery::default(),
        aligned: false,
    }
}

#[test]
fn a_query_reads_the_pages_of_the_span_it_covers() {
    let file = recording();
    let all = pages(&file);
    let bytes = Arc::new(Counted {
        bytes: file.clone(),
        reads: Mutex::default(),
    });
    let rec = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    bytes.take();
    let cancel = AtomicBool::new(false);
    // 2,000 ticks in 100 columns, 20 a column, inside page 3 of each channel's blocks (blocks of
    // 10 ticks, 4,096 a page): I, P and D from their samples and edges, V from its 2^4 level
    // and, for the statistics, its samples.
    let at = 3 * 40_960 + 10_000;
    rec.query(&query(at..at + 2000, 100), &cancel).unwrap();
    let read = touched(&all, &bytes.take());
    let footer = Footer::parse_file(&file).unwrap();
    let page = |c: usize, j: usize| {
        let table = footer.channels[c].blocks.as_ref().unwrap();
        let mut found = None;
        let mut k = 0;
        table
            .visit(&mut from_memory(&file), |at, _| {
                if k == j {
                    found = Some(at.entry.offset..at.entry.offset + u64::from(at.entry.length));
                }
                k += 1;
                Ok::<_, DecodeError>(ControlFlow::Continue(()))
            })
            .unwrap();
        (c, found.unwrap())
    };
    let directory = |c: usize| {
        let e = footer.channels[c].blocks.as_ref().unwrap().levels[1][0];
        (c, e.offset..e.offset + u64::from(e.length))
    };
    // The window and a width either side, 130,880 to 136,880: blocks 13,088 to 13,688 of each,
    // on page 3 (from block 12,288), each through its channel's directory. No other page of
    // blocks.
    let block_pages: Vec<_> = (0..20)
        .map(|j| page(V, j))
        .chain((0..20).map(|j| page(I, j)))
        .chain((0..18).map(|j| page(D, j)))
        .collect();
    let mut read_block_pages: Vec<_> = read
        .iter()
        .filter(|p| block_pages.contains(p))
        .cloned()
        .collect();
    read_block_pages.sort_by_key(|p| p.1.start);
    let mut want = vec![page(V, 3), page(I, 3), page(D, 3)];
    want.sort_by_key(|p| p.1.start);
    assert_eq!(read_block_pages, want);
    for c in [V, I, D] {
        assert!(read.contains(&directory(c)), "channel {c}: {read:?}");
    }
    // Again: the pages are kept.
    rec.query(&query(at + 100..at + 2100, 100), &cancel)
        .unwrap();
    assert_eq!(touched(&all, &bytes.take()), []);
}

#[test]
fn a_query_decodes_each_block_once_whatever_the_cache_keeps() {
    let file = recording();
    let footer = Footer::parse_file(&file).unwrap();
    let frames: Vec<Range<u64>> = {
        let mut f = footer.clone();
        f.load_lists(from_memory(&file)).unwrap();
        (0..3)
            .flat_map(|c| f.block_refs(c, from_memory(&file)).unwrap())
            .map(|b| b.row.offset..b.row.offset + u64::from(b.row.length))
            .collect()
    };
    let bytes = Arc::new(Counted {
        bytes: file.clone(),
        reads: Mutex::default(),
    });
    // A cache that keeps nothing: each part of the query that takes a block would decode it
    // again.
    let rec = Recording::open(bytes.clone(), Cache::new(0)).unwrap();
    let cancel = AtomicBool::new(false);
    let at = 300_000;
    rec.query(&query(at..at + 2000, 100), &cancel).unwrap();
    let reads = bytes.take();
    let mut blocks: Vec<&Range<u64>> = reads.iter().filter(|r| frames.contains(r)).collect();
    let n = blocks.len();
    assert!(n > 50, "{n} block reads");
    blocks.sort_by_key(|r| r.start);
    blocks.dedup();
    assert_eq!(blocks.len(), n, "every block read once");
}

/// Each frame of the file, block or overview block, as (what, channel, level): level None for a
/// block's.
fn frames(file: &[u8]) -> Vec<(Range<u64>, usize, Option<u8>)> {
    let mut footer = Footer::parse_file(file).unwrap();
    footer.load_lists(from_memory(file)).unwrap();
    let mut out = Vec::new();
    for c in 0..3 {
        for b in footer.block_refs(c, from_memory(file)).unwrap() {
            out.push((
                b.row.offset..b.row.offset + u64::from(b.row.length),
                c,
                None,
            ));
        }
    }
    for o in 0..footer.channels.len() {
        for (li, (level, _)) in footer.channels[o].levels.iter().enumerate() {
            for r in footer.level_refs(o, li, from_memory(file)).unwrap() {
                let span = r.row.offset..r.row.offset + u64::from(r.row.length);
                out.push((span, o, Some(level.shift)));
            }
        }
    }
    out
}

/// FORMAT.md, "Reading": each channel takes the coarsest of its own levels no wider than a
/// column, or its items below its finest; a derived channel its own ladder, from the coarser of
/// its operands' finest. V's levels are 2^4 and 2^8 in blocks, I's 2^8, P's its own 2^8, D's 2^6.
#[test]
fn each_channel_takes_its_own_level_for_a_column() {
    let file = recording();
    let frames = frames(&file);
    let bytes = Arc::new(Counted {
        bytes: file.clone(),
        reads: Mutex::default(),
    });
    let cancel = AtomicBool::new(false);
    // What one channel's query of 100 columns of `per` ticks from tick 300,000 reads, the file
    // opened afresh: the channels whose blocks, and the (channel, shift) of the overview blocks.
    let read = |c: usize, per: i64| {
        let rec = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
        bytes.take();
        let window = 300_000..300_000 + 100 * per;
        let q = Query {
            window,
            columns: 100,
            channels: vec![c],
            ..Query::default()
        };
        rec.query(&q, &cancel).unwrap();
        let reads = bytes.take();
        let mut blocks: Vec<usize> = Vec::new();
        let mut levels: Vec<(usize, u8)> = Vec::new();
        for (span, o, level) in &frames {
            if reads.contains(span) {
                match level {
                    None => blocks.push(*o),
                    Some(s) => levels.push((*o, *s)),
                }
            }
        }
        blocks.dedup();
        levels.dedup();
        (blocks, levels)
    };
    // 32 ticks a column: V from its 2^4, I, P and D from their items, P's from V's and I's.
    assert_eq!(read(V, 32), (vec![], vec![(V, 4)]));
    assert_eq!(read(I, 32), (vec![I], vec![]));
    assert_eq!(read(P, 32), (vec![V, I], vec![]));
    assert_eq!(read(D, 32), (vec![D], vec![]));
    // 512: each from its 2^8, D its 2^6; P from its own overview, no block read.
    assert_eq!(read(V, 512), (vec![], vec![(V, 8)]));
    assert_eq!(read(I, 512), (vec![], vec![(I, 8)]));
    assert_eq!(read(P, 512), (vec![], vec![(P, 8)]));
    assert_eq!(read(D, 512), (vec![], vec![(D, 6)]));
}

/// A first draw: the sketch of the whole extent, every channel and the strip, reads nothing past
/// what `open` read, the last 32 KiB here; each analog column's range holds what the exact
/// answer's does, and each digital column the exact one's level or busy where it changes.
#[test]
fn a_first_draw_reads_nothing_past_the_footer_and_holds_the_exact_picture() {
    use escapement_tickwave_view::Drawn;
    use escapement_tickwave_view::columns::State;
    let file = recording();
    let bytes = Arc::new(Counted {
        bytes: file.clone(),
        reads: Mutex::default(),
    });
    let rec = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    let opened: u64 = bytes.take().iter().map(|r| r.end - r.start).sum();
    assert!(opened <= 256 << 10, "{opened} bytes");
    let cancel = AtomicBool::new(false);
    let extent = rec.extent().unwrap();
    let q = Query {
        strip: Some((V, 1920)),
        ..query(extent, 1920)
    };
    let sketch = rec.sketch(&q, &cancel).unwrap();
    assert_eq!(bytes.take(), []);
    assert!(sketch.statistics.is_empty());
    let exact = rec.query(&q, &cancel).unwrap();
    assert_eq!((&sketch.window, &sketch.grid), (&exact.window, &exact.grid));
    let mut drawn = 0;
    for (i, (s, e)) in sketch.channels.iter().zip(&exact.channels).enumerate() {
        match (s, e) {
            (Drawn::Analog { columns: s, .. }, Drawn::Analog { columns: e, .. }) => {
                for k in 0..e.min.len() {
                    if e.min[k] <= e.max[k] {
                        assert!(
                            s.min[k] <= e.min[k] && s.max[k] >= e.max[k],
                            "{i}, column {k}"
                        );
                        drawn += 1;
                    }
                }
            }
            (Drawn::Digital { columns: s, .. }, Drawn::Digital { columns: e, .. }) => {
                for k in 0..e.state.len() {
                    if e.state[k] != State::None {
                        assert!(
                            s.state[k] == e.state[k] || s.state[k] == State::Busy,
                            "{i}, {k}"
                        );
                        drawn += 1;
                    }
                }
            }
            _ => panic!("channel {i}: different kinds"),
        }
    }
    assert!(drawn > 4 * 1900, "{drawn} columns drawn");
}

/// V's samples from 100 to 190, every 10 ticks, and marks at 3 and 400, finished at 500 with
/// `bundle`.
fn marked(bundle: escapement_tickwave_format::writer::Bundle) -> Vec<u8> {
    let timebase = Ratio::integer(1000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "V".into(),
            source_name: "v".into(),
            source: 0,
            data: DeclaredKind::Analog {
                unit: "V".into(),
                period: Ratio::integer(10).unwrap(),
                coding: AnalogCoding::F32,
                significant: None,
            },
            shaping: Default::default(),
            overview_shift: None,
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("test".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    let mark = |w: &mut Writer, t: i64| {
        let serde_json::Value::Object(m) = serde_json::json!({"id": format!("m/{t}"), "start": t})
        else {
            unreachable!()
        };
        w.annotation("verge", m).unwrap();
    };
    mark(&mut w, 3);
    w.run(0, 100, 0).unwrap();
    for i in 0..10 {
        w.sample(0, i as f32).unwrap();
    }
    mark(&mut w, 400);
    let mut file = w.take_output();
    let finish = Finish {
        bundle: Some(bundle),
        ..Finish::default()
    };
    file.extend(w.finish(500, &[], finish).unwrap().tail);
    file
}

fn markers_query() -> Query {
    Query {
        window: 0..500,
        columns: 50,
        channels: vec![0],
        markers: MarkerQuery {
            limit: 10,
            ..MarkerQuery::default()
        },
        ..Query::default()
    }
}

/// The bundle is read once markers are asked for, not by `open` or a sketch; whether there are
/// any is known from the footer.
#[test]
fn a_finished_files_markers_are_read_when_first_asked_for() {
    use escapement_tickwave_format::writer::Bundle;
    let file = marked(Bundle::Generated { stem: "rec".into() });
    let (at, _) = Footer::parse_file(&file).unwrap().bundle.unwrap();
    let bytes = Arc::new(Counted {
        bytes: file.clone(),
        reads: Mutex::default(),
    });
    let read_bundle = |reads: &[Range<u64>]| reads.iter().filter(|r| r.start == at).count();
    let rec = Recording::open(bytes.clone(), Cache::new(1 << 20)).unwrap();
    let cancel = AtomicBool::new(false);
    assert!(rec.has_markers());
    let sketch = rec.sketch(&markers_query(), &cancel).unwrap();
    assert_eq!(sketch.extent, Some(100..200), "the data's");
    assert_eq!(read_bundle(&bytes.take()), 0);
    let a = rec.query(&markers_query(), &cancel).unwrap();
    assert_eq!(read_bundle(&bytes.take()), 1);
    assert_eq!(a.markers.window.total, 2);
    assert_eq!(a.extent, Some(3..401), "widened by the marks");
    assert_eq!(rec.marker_count(), 2);
    rec.query(&markers_query(), &cancel).unwrap();
    assert_eq!(read_bundle(&bytes.take()), 0);
}

/// A bundle that doesn't resolve leaves the recording open: the queries that ask for markers say
/// why, and the rest answer.
#[test]
fn a_bundle_that_does_not_resolve_fails_only_the_queries_that_ask_for_markers() {
    use escapement_tickwave_format::writer::Bundle;
    let files = BTreeMap::from([("rec.verge.jsonl".to_owned(), "{}\n".to_owned())]);
    let rec = Recording::open(Arc::new(marked(Bundle::Files(files))), Cache::new(1 << 20)).unwrap();
    let cancel = AtomicBool::new(false);
    let err = rec
        .query(&markers_query(), &cancel)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no hand-written file"), "{err}");
    let q = Query {
        markers: MarkerQuery::default(),
        ..markers_query()
    };
    assert!(rec.query(&q, &cancel).is_ok());
    assert_eq!(rec.marker_count(), 0);
    assert_eq!(rec.extent(), Some(100..200));
}

/// Reads `bytes`, counting the reads at `slow` and taking 20 ms over each, so that threads asking
/// at once all meet the read in flight.
struct Slow {
    bytes: Vec<u8>,
    slow: u64,
    reads: std::sync::atomic::AtomicUsize,
}

impl Bytes for Slow {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.bytes.len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if offset == self.slow {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let at = offset as usize;
        buf.copy_from_slice(&self.bytes[at..at + buf.len()]);
        Ok(())
    }
}

/// `load_markers` says why a bundle doesn't resolve on every call, and the markers' readers give
/// none; `has_markers` says there are markers, lost. Threads that ask at once read the bundle
/// once.
#[test]
fn load_markers_says_why_on_every_call_and_threads_read_the_bundle_once() {
    use escapement_tickwave_format::writer::Bundle;
    let files = BTreeMap::from([("rec.verge.jsonl".to_owned(), "{}\n".to_owned())]);
    let rec = Recording::open(Arc::new(marked(Bundle::Files(files))), Cache::new(1 << 20)).unwrap();
    for call in 0..2 {
        let err = rec.load_markers().unwrap_err().to_string();
        assert!(err.contains("no hand-written file"), "call {call}: {err}");
    }
    assert!(rec.has_markers());
    assert_eq!(rec.marker_count(), 0);
    let file = marked(Bundle::Generated { stem: "rec".into() });
    let (at, _) = Footer::parse_file(&file).unwrap().bundle.unwrap();
    let bytes = Arc::new(Slow {
        bytes: file,
        slow: at,
        reads: Default::default(),
    });
    let rec = Recording::open(bytes.clone(), Cache::new(1 << 20)).unwrap();
    let start = std::sync::Barrier::new(8);
    std::thread::scope(|s| {
        for k in 0..8 {
            let (rec, start) = (&rec, &start);
            s.spawn(move || {
                start.wait();
                match k % 2 {
                    0 => rec.load_markers().unwrap(),
                    _ => assert_eq!(rec.marker_count(), 2),
                }
            });
        }
    });
    assert_eq!(bytes.reads.load(std::sync::atomic::Ordering::Relaxed), 1);
}
