//! Writes web/fixtures/phased.tickwave: analog samples whose places fall between ticks, which a
//! reader maps to the axis at the tick at or before each (FORMAT.md, "Times"). Nothing in it comes
//! from a capture. Run from the repository:
//!
//!     cargo run -p escapement-tickwave-format --example phased -- web/fixtures
//!
//! - One source at 1 GHz, the axis's rate, 400 ticks.
//! - A, `int` 16-bit codes in mV, a sample every 5/2 ticks, its declared phase 3/4 of a tick: its
//!   grid is a quarter tick. Ten samples from tick 100, at places 100.75, 103.25, 105.75, ...;
//!   then a break at tick 140 with a phase of 1/4, after two samples lost, and ten more at
//!   140.25, 142.75, ... Sample `i` holds 1,000 + `i` mV.
//! - B, the same period with no phase: twenty samples from tick 100, at 100, 102.5, ... Sample
//!   `i` holds 2,000 + `i` mV.
//! - D, digital: edges at 210 and 260.
//!
//! A's second sample lies at tick 103, where a reader that drops the phase puts it at 102; its
//! twelfth at 142, where one that takes the declared phase for the break's puts it at 143.

use std::collections::BTreeMap;
use std::path::PathBuf;

use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Exact, IntCoding, SourceFormat,
    SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};

const LENGTH: i64 = 400;

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("the directory to write into"),
    );
    let analog = |name: &str, phase: Option<Ratio>| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period: Ratio::new(5, 2).unwrap(),
            coding: AnalogCoding::Int(IntCoding {
                scale: Exact::new(1, 1000),
                ..IntCoding::new(16, false)
            }),
            significant: None,
            phase,
        },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let timebase = Ratio::integer(1_000_000_000).unwrap();
    let declaration = Declaration {
        timebase,
        channels: vec![
            analog("A", Some(Ratio::new(3, 4).unwrap())),
            analog("B", None),
            DeclaredChannel {
                name: "D".into(),
                source_name: "d".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Default::default(),
                overview_shift: None,
                codec: None,
                requires: Vec::new(),
            },
        ],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("phased".into()),
            timebase,
        )],
        title: Some("Samples between ticks".into()),
        meta: BTreeMap::new(),
        overview_shift: 4,
        requires: Vec::new(),
    };
    let mut w = Writer::new(
        declaration,
        WriteOptions {
            level: 19,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    w.run(0, 100, 0).unwrap();
    for i in 0..10u64 {
        w.sample(0, 1000 + i).unwrap();
    }
    // A quarter tick, in the grid's quarters.
    w.run_at(0, 140, 1, 2).unwrap();
    for i in 10..20u64 {
        w.sample(0, 1000 + i).unwrap();
    }
    w.run(1, 100, 0).unwrap();
    for i in 0..20u64 {
        w.sample(1, 2000 + i).unwrap();
    }
    for t in [210, 260] {
        w.edge(2, t).unwrap();
    }
    let mut out = w.take_output();
    let finished = w.finish(LENGTH, &[LENGTH], Finish::default()).unwrap();
    out.extend(finished.tail);
    std::fs::write(dir.join("phased.tickwave"), &out).unwrap();
    println!(
        "phased.tickwave: {} bytes, {}",
        out.len(),
        finished.header.content_id
    );
}
