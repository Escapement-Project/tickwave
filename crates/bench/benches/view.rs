//! The view's queries on small recordings, as instructions and estimated cycles under Callgrind.
//! Each set-up opens the recording and, for a warm query, asks it once first, so the bench counts
//! the query alone.
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use escapement_tickwave_bench::{CLOCK_HZ, Capture, RATE, Synth};
use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::snapshot::Snapshot;
use escapement_tickwave_view::roles::length_at;
use escapement_tickwave_view::{Answer, Bytes, Cache, MarkerQuery, Query, Recording};
use gungraun::prelude::*;
use gungraun::{Callgrind, FlamegraphConfig};

const DIR: &str = env!("CARGO_TARGET_TMPDIR");

fn open(capture: Capture) -> Recording {
    let bytes: Arc<dyn Bytes> = Arc::new(std::fs::read(capture.path(DIR)).unwrap());
    Recording::open(bytes, Cache::new(256 << 20)).unwrap()
}

/// timing.rs's query: 1,920 columns of I, DIO0 to DIO3 under it, I's range flag, the strip, the
/// window's statistics and up to 100 markers, `per` ticks a column about the middle.
fn query(rec: &Recording, per: i64) -> Query {
    let extent = rec.extent().unwrap();
    let width = per * 1920;
    let start = (extent.start + extent.end) / 2 - width / 2;
    let window = start..start + width;
    {
        let mut query = Query::new(window.clone(), 1920);
        query.channels = vec![1, 4, 5, 6, 7];
        query.flags = vec![(1, 3)];
        query.strip = Some((1, 1920));
        query.statistics = vec![window];
        query.markers = {
            let mut markers = MarkerQuery::default();
            markers.limit = 100;
            markers
        };
        query
    }
}

/// The recording just opened and a query `steps` levels in from its finest overview level's
/// edge.
fn cold(capture: Capture, steps: u8) -> (Recording, Query) {
    let bytes = std::fs::read(capture.path(DIR)).unwrap();
    let shift = Footer::parse_file(&bytes).unwrap().header.overview_shift;
    let rec = open(capture);
    let q = query(&rec, 1 << (shift - steps));
    (rec, q)
}

/// `cold`'s, the query asked once.
fn warm(capture: Capture, steps: u8) -> (Recording, Query) {
    let (rec, q) = cold(capture, steps);
    rec.query(&q, &AtomicBool::new(false)).unwrap();
    (rec, q)
}

#[library_benchmark]
#[bench::finest_edge(args = (Capture::Verge, 0), setup = warm)]
#[bench::one_step_in(args = (Capture::Verge, 1), setup = warm)]
fn query_warm((rec, q): (Recording, Query)) {
    black_box(rec.query(black_box(&q), &AtomicBool::new(false)).unwrap());
}

#[library_benchmark]
#[bench::one_step_in(args = (Capture::Verge, 1), setup = cold)]
fn query_cold((rec, q): (Recording, Query)) {
    black_box(rec.query(black_box(&q), &AtomicBool::new(false)).unwrap());
}

#[library_benchmark]
#[bench::verge(Capture::Verge)]
fn open_recording(capture: Capture) {
    let bytes: Arc<dyn Bytes> = Arc::new(std::fs::read(capture.path(DIR)).unwrap());
    black_box(Recording::open(bytes, Cache::new(256 << 20)).unwrap());
}

// A second's statistics on a grid of half ticks: every sample summed one by one, as a live
// window's open block is.
#[library_benchmark]
#[bench::second(args = (Capture::HalfTicks,), setup = open)]
fn statistics_each_sample(rec: Recording) {
    let window = (CLOCK_HZ / 2) as i64..(CLOCK_HZ * 3 / 2) as i64;
    black_box(rec.statistics(window, &AtomicBool::new(false)).unwrap());
}

fn markers_read(capture: Capture) -> Recording {
    let rec = open(capture);
    rec.markers(0..rec.length(), None, 512);
    rec
}

// The whole recording's first page of markers from 36,000, the parts in the cache.
#[library_benchmark]
#[bench::whole(args = (Capture::Markers,), setup = markers_read)]
fn markers_by_range(rec: Recording) {
    black_box(rec.markers(0..rec.length(), None, 512));
}

/// Verge's layout live: a second written with `markers` markers, every one still in lines no
/// part holds yet, and the recording made live from a snapshot then; and the snapshot after a
/// 10 ms batch more, to update it with. A recorder writing a marker a minute has 60 pending an
/// hour in, and 600 ten hours in.
fn live(markers: u64) -> (Recording, Snapshot) {
    let mut s = Synth::new(Ratio::integer(CLOCK_HZ / RATE).unwrap(), RATE, 1);
    s.second(0, true, markers);
    let first = s.writer.snapshot();
    let pending: usize = (first.unwritten_parts().unwrap().iter())
        .map(|p| p.lines().count())
        .sum();
    assert_eq!(pending as u64, markers, "every marker in pending lines");
    s.batch(1, RATE / 100, true);
    let next = s.writer.snapshot();
    let bytes: Arc<dyn Bytes> = Arc::new(s.file);
    let rec = Recording::live(first, bytes, Cache::new(64 << 20)).unwrap();
    (rec, next)
}

// One live update by a 10 ms batch with no marker in it. The recording is returned, so dropping
// it isn't counted.
#[library_benchmark]
#[bench::one_marker_pending(args = (1,), setup = live)]
#[bench::sixty_markers_pending(args = (60,), setup = live)]
#[bench::six_hundred_markers_pending(args = (600,), setup = live)]
fn live_update((mut rec, next): (Recording, Snapshot)) -> Recording {
    rec.update(black_box(next)).unwrap();
    rec
}

/// The clock's 1,920 columns of DIO0 alone, `per` ticks a column from the second's start: a
/// million edges, at its finest level's edge (`per` 2^13) and over the whole second.
fn clock(per: i64) -> (Recording, Query) {
    let rec = open(Capture::Clock);
    let width = per * 1920;
    let q = {
        let mut query = Query::new(0..width, 1920);
        query.channels = vec![escapement_tickwave_bench::DIO0];
        query
    };
    rec.query(&q, &AtomicBool::new(false)).unwrap();
    (rec, q)
}

// A dense digital channel's columns, warm: from its finest level's buckets, from the edges one
// step in, and the whole second from a level, as the model's edges and time high are summed.
#[library_benchmark]
#[bench::finest_edge(args = (1 << 13,), setup = clock)]
#[bench::one_step_in(args = (1 << 12,), setup = clock)]
#[bench::whole_second(args = (CLOCK_HZ as i64 / 1920,), setup = clock)]
fn query_dense((rec, q): (Recording, Query)) {
    black_box(rec.query(black_box(&q), &AtomicBool::new(false)).unwrap());
}

/// `query`'s, with DIO0 to DIO3's columns over the extent as egui's graph asks for its lanes.
fn with_fallbacks(mut q: Query) -> Query {
    q.fallbacks = (4..8).map(|c| (c, 1920)).collect();
    q
}

/// `warm`'s query with the lanes' columns over the extent, asked once at `before`'s window moved
/// by `moved` tenths of its width, so the one counted is the same query, or a pan from it.
fn fallbacks((capture, steps, moved): (Capture, u8, i64)) -> (Recording, Query) {
    let (rec, q) = cold(capture, steps);
    let q = with_fallbacks(q);
    let width = q.window.end - q.window.start;
    let mut before = q.clone();
    before.window = q.window.start - moved * width / 10..q.window.end - moved * width / 10;
    before.statistics = vec![before.window.clone()];
    rec.query(&before, &AtomicBool::new(false)).unwrap();
    (rec, q)
}

// The graph's query with its lanes' columns over the extent: asked again, and panned by a tenth.
#[library_benchmark]
#[bench::again(args = ((Capture::Verge, 0, 0),), setup = fallbacks)]
#[bench::panned(args = ((Capture::Verge, 0, 1),), setup = fallbacks)]
fn query_fallbacks((rec, q): (Recording, Query)) {
    black_box(rec.query(black_box(&q), &AtomicBool::new(false)).unwrap());
}

/// `live`'s recording asked the graph's query with its lanes' columns over the extent, then
/// updated by the next snapshot: the extent has grown, so the query counted reads them anew.
fn live_fallbacks(markers: u64) -> (Recording, Query) {
    let (mut rec, next) = live(markers);
    let extent = rec.extent().unwrap();
    let q = {
        let mut q = Query::new(extent.end - CLOCK_HZ as i64 / 2..extent.end, 1920);
        q.channels = vec![1, 4, 5, 6, 7];
        q.strip = Some((1, 1920));
        with_fallbacks(q)
    };
    rec.query(&q, &AtomicBool::new(false)).unwrap();
    rec.update(next).unwrap();
    (rec, q)
}

// After a live update: the query a following graph asks again, with its lanes' columns.
#[library_benchmark]
#[bench::after_update(args = (1,), setup = live_fallbacks)]
fn query_live_fallbacks((rec, q): (Recording, Query)) -> Recording {
    black_box(rec.query(black_box(&q), &AtomicBool::new(false)).unwrap());
    rec
}

/// `live`'s recording followed as egui's graph follows it: the last half second in aligned
/// columns, the trace's strip and the lanes' columns over the extent; the answer held, the
/// recording updated by the next snapshot, and the window moved to its new end.
fn followed(markers: u64) -> (Recording, (Answer, Query, Query)) {
    let (mut rec, next) = live(markers);
    let follow = |end: i64| {
        let mut q = Query::new(end - CLOCK_HZ as i64 / 2..end, 1920);
        q.channels = vec![1, 4, 5, 6, 7];
        q.strip = Some((1, 1920));
        q.aligned = true;
        with_fallbacks(q)
    };
    let asked = follow(rec.extent().unwrap().end);
    let held = rec.query(&asked, &AtomicBool::new(false)).unwrap();
    rec.update(next).unwrap();
    let q = follow(rec.extent().unwrap().end);
    // An extend, not a whole query.
    let extended = rec.extend(&held, &asked, &q, &AtomicBool::new(false));
    assert!(extended.unwrap().is_some(), "the answer extends");
    (rec, (held, asked, q))
}

// A followed answer extended after a live update, as egui's query thread does at each snapshot.
#[library_benchmark]
#[bench::with_fallbacks(args = (1,), setup = followed)]
fn extend_followed((rec, (held, asked, q)): (Recording, (Answer, Query, Query))) -> Recording {
    let cancel = AtomicBool::new(false);
    black_box(rec.extend(&held, &asked, black_box(&q), &cancel).unwrap());
    rec
}

/// Verge's layout live as verge-probe writes it, 1 MiB blocks closed only at its flush a second:
/// a second flushed, then `pairs` of the next unflushed, so the open block holds them; the
/// recording made live from a snapshot then and followed as `followed`'s is; and the snapshot
/// after a 10 ms batch more.
fn open_block(pairs: u64) -> (Recording, (Answer, Query, Snapshot)) {
    let mut s = Synth::new(Ratio::integer(CLOCK_HZ / RATE).unwrap(), RATE, 1);
    s.second(0, true, 1);
    s.batch(1, pairs, true);
    let first = s.writer.snapshot();
    s.batch(1, RATE / 100, false);
    let next = s.writer.snapshot();
    let bytes: Arc<dyn Bytes> = Arc::new(s.file);
    let rec = Recording::live(first, bytes, Cache::new(64 << 20)).unwrap();
    let end = rec.extent().unwrap().end;
    let mut q = Query::new(end - CLOCK_HZ as i64 / 2..end, 1920);
    q.channels = vec![1, 4, 5, 6, 7];
    q.strip = Some((1, 1920));
    q.aligned = true;
    let q = with_fallbacks(q);
    let held = rec.query(&q, &AtomicBool::new(false)).unwrap();
    (rec, (held, q, next))
}

// A live update by a 10 ms batch, the open block holding 10 ms or 900 ms of pairs before it.
#[library_benchmark]
#[bench::open_10ms(args = (RATE / 100,), setup = open_block)]
#[bench::open_900ms(args = (9 * RATE / 10,), setup = open_block)]
fn update_open((mut rec, (_, _, next)): (Recording, (Answer, Query, Snapshot))) -> Recording {
    rec.update(black_box(next)).unwrap();
    rec
}

/// `open_block`'s recording updated, and its followed query at the new end.
fn open_block_updated(pairs: u64) -> (Recording, (Answer, Query, Query)) {
    let (mut rec, (held, asked, next)) = open_block(pairs);
    rec.update(next).unwrap();
    let end = rec.extent().unwrap().end;
    let mut q = asked.clone();
    q.window = end - CLOCK_HZ as i64 / 2..end;
    q.statistics = vec![q.window.clone()];
    let extended = rec.extend(&held, &asked, &q, &AtomicBool::new(false));
    assert!(extended.unwrap().is_some(), "the answer extends");
    (rec, (held, asked, q))
}

// The follow-up after that update: the answer extended to the new end.
#[library_benchmark]
#[bench::open_10ms(args = (RATE / 100,), setup = open_block_updated)]
#[bench::open_900ms(args = (9 * RATE / 10,), setup = open_block_updated)]
fn extend_open((rec, (held, asked, q)): (Recording, (Answer, Query, Query))) -> Recording {
    let cancel = AtomicBool::new(false);
    black_box(rec.extend(&held, &asked, black_box(&q), &cancel).unwrap());
    rec
}

/// A row's line in `paths` of `points` points each, x rising a pixel a point and its length with
/// it, flat as `length_at` takes them.
fn line((paths, points): (usize, usize)) -> Vec<Vec<f64>> {
    (0..paths)
        .map(|p| {
            (0..points)
                .flat_map(|k| {
                    let x = (p * points + k) as f64;
                    [x, x * 1.5]
                })
                .collect()
        })
        .collect()
}

// Where a dash's phase starts along a long row: near its end, which a scan from the start
// reaches last.
#[library_benchmark]
#[bench::many_paths(args = ((4000, 8),), setup = line)]
#[bench::one_long_path(args = ((1, 32000),), setup = line)]
fn dash_phase(paths: Vec<Vec<f64>>) -> Vec<Vec<f64>> {
    let x = (paths.len() * paths[0].len() / 2) as f64 - 10.5;
    black_box(length_at(black_box(&paths), black_box(x)));
    paths
}

library_benchmark_group!(
    name = view,
    benchmarks = [
        query_warm,
        query_cold,
        open_recording,
        statistics_each_sample,
        markers_by_range,
        live_update,
        query_dense,
        query_fallbacks,
        query_live_fallbacks,
        extend_followed,
        update_open,
        extend_open,
        dash_phase
    ]
);

// A flamegraph of each bench beside its Callgrind output, under target/gungraun/.
main!(
    config = LibraryBenchmarkConfig::default()
        .tool(Callgrind::default().flamegraph(FlamegraphConfig::default())),
    library_benchmark_groups = view
);
