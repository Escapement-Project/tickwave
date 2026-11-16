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
use escapement_tickwave_view::{Bytes, Cache, MarkerQuery, Query, Recording};
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
    Query {
        window: window.clone(),
        columns: 1920,
        channels: vec![1, 4, 5, 6, 7],
        flags: vec![(1, 3)],
        strip: Some((1, 1920)),
        statistics: vec![window],
        markers: MarkerQuery {
            limit: 100,
            ..MarkerQuery::default()
        },
        aligned: false,
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
    let q = Query {
        window: 0..width,
        columns: 1920,
        channels: vec![escapement_tickwave_bench::DIO0],
        ..Query::default()
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

library_benchmark_group!(
    name = view,
    benchmarks = [
        query_warm,
        query_cold,
        open_recording,
        statistics_each_sample,
        markers_by_range,
        live_update,
        query_dense
    ]
);

// A flamegraph of each bench beside its Callgrind output, under target/gungraun/.
main!(
    config = LibraryBenchmarkConfig::default()
        .tool(Callgrind::default().flamegraph(FlamegraphConfig::default())),
    library_benchmark_groups = view
);
