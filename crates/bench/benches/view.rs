//! The view's queries on small recordings, as instructions and estimated cycles under Callgrind.
//! Each set-up opens the recording and, for a warm query, asks it once first, so the bench counts
//! the query alone.
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use escapement_tickwave_bench::{CLOCK_HZ, Capture};
use escapement_tickwave_format::decode::Footer;
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

library_benchmark_group!(
    name = view,
    benchmarks = [
        query_warm,
        query_cold,
        open_recording,
        statistics_each_sample,
        markers_by_range
    ]
);

// A flamegraph of each bench beside its Callgrind output, under target/gungraun/.
main!(
    config = LibraryBenchmarkConfig::default()
        .tool(Callgrind::default().flamegraph(FlamegraphConfig::default())),
    library_benchmark_groups = view
);
