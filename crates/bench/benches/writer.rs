//! The writer's calls that have run long, one call each, and its per-item path, as instructions
//! and estimated cycles under Callgrind. Each set-up builds the writer up to the call. A call's
//! count stands for its time only as a relative figure: the 2 ms bound is checked in wall time
//! on full-size runs.
use std::hint::black_box;

use escapement_tickwave_bench::{CLOCK_HZ, DIO0, RATE, Synth};
use escapement_tickwave_format::compress::{SLICE, Slicer};
use escapement_tickwave_format::model::Ratio;
use gungraun::prelude::*;
use gungraun::{Callgrind, FlamegraphConfig};

fn verge(level: i32) -> Synth {
    Synth::new(Ratio::integer(CLOCK_HZ / RATE).unwrap(), RATE, level)
}

/// A writer of verge's layout at `level`, a second written.
fn second_written(level: i32) -> Synth {
    let mut s = verge(level);
    s.second(0, true, 1);
    s
}

// 100 ms more of V and I: 20,000 sample calls, then the flush and `take_output`.
#[library_benchmark]
#[bench::level_1(args = (1,), setup = second_written)]
fn hundred_ms_of_pairs(mut s: Synth) {
    let w = &mut s.writer;
    for k in 0..RATE / 10 {
        w.sample(0, 3300 + (k & 7)).unwrap();
        w.sample(1, 1_000_000 + (k & 0xfff)).unwrap();
    }
    w.flush((CLOCK_HZ + CLOCK_HZ / 10) as i64).unwrap();
    black_box(w.take_output());
}

/// DIO0 with 100,000 edges every 96 ticks in its open block, and none past them.
fn dense_edges(_: ()) -> Synth {
    let mut s = verge(16);
    for k in 0..100_000 {
        s.writer.edge(DIO0, k * 96).unwrap();
    }
    s
}

// The edge off the 96-tick grid that shrinks the open block's step to 1.
#[library_benchmark]
#[bench::hundred_thousand_edges(args = ((),), setup = dense_edges)]
fn late_step_shrink(mut s: Synth) {
    s.writer.edge(DIO0, 100_000 * 96 + 1).unwrap();
    black_box(s);
}

// The flush that closes that block.
#[library_benchmark]
#[bench::hundred_thousand_edges(args = ((),), setup = dense_edges)]
fn flush_dense_block(mut s: Synth) {
    s.writer.flush(100_000 * 96).unwrap();
    black_box(s);
}

/// `dense_edges`, then the edge off the grid that shrinks the step to 1.
fn shrunk(_: ()) -> Synth {
    let mut s = dense_edges(());
    s.writer.edge(DIO0, 100_000 * 96 + 1).unwrap();
    s
}

// The flush that closes the block, its intervals now coded over a step of 1.
#[library_benchmark]
#[bench::hundred_thousand_edges(args = ((),), setup = shrunk)]
fn flush_shrunk_block(mut s: Synth) {
    s.writer.flush(100_000 * 96 + 2).unwrap();
    black_box(s);
}

/// A second written, then V and I's runs a day on, one pair there.
fn day_idle(_: ()) -> Synth {
    let mut s = verge(16);
    s.second(0, true, 1);
    let day = (86_400 * CLOCK_HZ) as i64;
    s.writer.run(0, day, 0).unwrap();
    s.writer.run(1, day, 0).unwrap();
    s.writer.sample(0, 3300u64).unwrap();
    s.writer.sample(1, 1_000_000u64).unwrap();
    s
}

// The flush past the idle day.
#[library_benchmark]
#[bench::a_day(args = ((),), setup = day_idle)]
fn flush_after_idle(mut s: Synth) {
    s.writer.flush((86_401 * CLOCK_HZ) as i64).unwrap();
    black_box(s);
}

/// A second written with 500 markers, most of them still pending lines.
fn markers_pending(_: ()) -> Synth {
    let mut s = verge(16);
    s.second(0, true, 500);
    s
}

#[library_benchmark]
#[bench::five_hundred_markers(args = ((),), setup = markers_pending)]
fn snapshot(s: Synth) {
    black_box(s.writer.snapshot());
}

/// A slice of each kind of data a writer's call compresses, and a context at verge's level for
/// it: marker lines, I's codes in byte planes, DIO0's edges. The recording they come from is
/// written at level 1, which leaves their decoded bytes as they are.
fn slice_of(kind: &str) -> (Slicer, Vec<u8>) {
    let mut s = verge(1);
    s.second(0, true, 1000);
    let file = s.finish(1);
    let index = escapement_tickwave_format::decode::Index::of_file(&file).unwrap();
    let raw = match kind {
        "markers" => {
            let footer = &index.footer;
            let rows = footer
                .part_rows(escapement_tickwave_format::decode::from_memory(&file))
                .unwrap();
            let row = &rows[0];
            let frame = &file[row.offset as usize..][..row.length as usize];
            footer.decode_part(row, frame).unwrap().lines.into_bytes()
        }
        _ => {
            let c = if kind == "codes" { 1 } else { DIO0 };
            let row = &index.blocks[c][0].row;
            let frame = &file[row.offset as usize..][..row.length as usize];
            escapement_tickwave_format::decode::inflate_limited(
                frame,
                row.decoded_length as usize,
                1 << 30,
            )
            .unwrap()
        }
    };
    let mut slicer = Slicer::new(16).unwrap();
    slicer.begin(raw.len()).unwrap();
    (slicer, raw[..SLICE.min(raw.len())].to_vec())
}

// One 8 KiB slice compressed at zstd 16, the most a writer's call compresses with spread
// compression.
#[library_benchmark]
#[bench::marker_lines(args = ("markers",), setup = slice_of)]
#[bench::codes_in_planes(args = ("codes",), setup = slice_of)]
#[bench::edges(args = ("edges",), setup = slice_of)]
fn slice_at_16((mut slicer, slice): (Slicer, Vec<u8>)) {
    let mut out = Vec::new();
    slicer.slice(&slice, false, &mut out).unwrap();
    black_box(out);
}

library_benchmark_group!(
    name = writer,
    benchmarks = [
        hundred_ms_of_pairs,
        late_step_shrink,
        flush_dense_block,
        flush_shrunk_block,
        flush_after_idle,
        snapshot,
        slice_at_16
    ]
);

// A flamegraph of each bench beside its Callgrind output, under target/gungraun/.
main!(
    config = LibraryBenchmarkConfig::default()
        .tool(Callgrind::default().flamegraph(FlamegraphConfig::default())),
    library_benchmark_groups = writer
);
