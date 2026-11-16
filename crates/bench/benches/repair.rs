//! Repair of a file of 10,000 markers cut at 60 %, and reading its generator lines back from the
//! copy, as instructions and estimated cycles under Callgrind.
use std::hint::black_box;

use escapement_tickwave_bench::Capture;
use escapement_tickwave_format::decode::{DecodeError, Footer, from_memory};
use escapement_tickwave_format::repair::Repairer;
use escapement_tickwave_format::writer::Bundle;
use gungraun::prelude::*;
use gungraun::{Callgrind, FlamegraphConfig};

const DIR: &str = env!("CARGO_TARGET_TMPDIR");

fn cut(capture: Capture) -> Vec<u8> {
    std::fs::read(capture.path(DIR)).unwrap()
}

#[library_benchmark]
#[bench::cut_markers(args = (Capture::CutMarkers,), setup = cut)]
fn repair(cut: Vec<u8>) {
    let mut r = Repairer::new(3);
    r.bundle(Bundle::Generated {
        stem: "bench".into(),
    });
    r.feed(&cut).unwrap();
    black_box(r.finish().unwrap());
}

fn repaired(capture: Capture) -> Vec<u8> {
    let cut = cut(capture);
    let mut r = Repairer::new(3);
    r.feed(&cut).unwrap();
    let repaired = r.finish().unwrap();
    [&cut[..repaired.keep as usize], &repaired.tail].concat()
}

// The repaired copy's generator lines, a part at a time, as `tickwave repair` writes them.
#[library_benchmark]
#[bench::cut_markers(args = (Capture::CutMarkers,), setup = repaired)]
fn generated_lines(file: Vec<u8>) {
    let footer = Footer::parse_file(&file).unwrap();
    let mut n = 0usize;
    footer
        .each_generated_line(from_memory(&file), |_, line| {
            n += line.len();
            Ok::<_, DecodeError>(())
        })
        .unwrap();
    black_box(n);
}

library_benchmark_group!(name = repair_group, benchmarks = [repair, generated_lines]);

// A flamegraph of each bench beside its Callgrind output, under target/gungraun/.
main!(
    config = LibraryBenchmarkConfig::default()
        .tool(Callgrind::default().flamegraph(FlamegraphConfig::default())),
    library_benchmark_groups = repair_group
);
