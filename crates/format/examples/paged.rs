//! Writes recordings for the web's unit tests of a reader that reads a page at a time.
//!
//! `lists.tickwave` has channels whose gaps or breaks are in list pages, and `lists.json` holds
//! the lists and edges it was written with. At 1 MHz:
//!
//! - A and B, 16-bit codes every 10 ticks: run k of 20 samples starts at tick 100 + 250 k, 10,001
//!   runs, so 10,000 breaks in three pages. Two samples lost at each break but every fifth, which
//!   only pauses; three lost before the first sample and two after the last. Sample n's code is
//!   1000 + n % 97 on A and 2000 + (31 n) % 89 on B.
//! - P, A times B.
//! - D, not known until tick 30, then an edge every 250 ticks from 200 and a gap from 50 to 100
//!   ticks after each: 9,001 gaps in three pages, each gap's level after it drawn at random.
//!
//! `blocks.tickwave` has a channel whose blocks need a directory page: F, at 1 kHz, an edge every
//! 10 ticks from 5 and a flush after each, so 70,000 blocks, past the 16 pages a top index lists.
//! G has an edge at 100 and one at 600,000. It bundles a sidecar that doesn't resolve: its
//! hand-written file names another recording's id.
//!
//! `phased-lists.tickwave` has a channel whose breaks, each with a phase, are in list pages: C, at
//! 1 MHz, 16-bit codes every 5/2 ticks, its declared phase 3/4 of a tick, so a grid of quarter
//! ticks. Run k of 20 samples starts at tick 100 + 100 k, 200 runs: the first at the declared
//! phase, run k after it k % 4 quarters past its tick. Sample n's code is n. Each run's sample 11
//! starts a block at its own tick, 28 ticks into the run where its phase is 2/4 or 3/4, else 27.
//!
//! Run from the repository:
//!
//!     cargo run -p escapement-tickwave-format --example paged -- DIR

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, IntCoding, SourceFormat,
    SourceHeader,
};
use escapement_tickwave_format::model::{Gap, Ratio};
use escapement_tickwave_format::writer::{Bundle, Finish, WriteOptions, Writer};

const RUN: u64 = 20;
const BREAKS: u64 = 10_000;
const GAPS: i64 = 9_000;

fn run_tick(k: u64) -> i64 {
    100 + (k * (RUN * 10 + 50)) as i64
}

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("the directory to write into"),
    );
    let timebase = Ratio::integer(1_000_000).unwrap();
    let analog = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period: Ratio::integer(10).unwrap(),
            coding: AnalogCoding::Int(IntCoding::new(16, false)),
            significant: None,
            phase: None,
        },
        shaping: Default::default(),
        overview_shift: Some(6),
        codec: None,
        requires: Vec::new(),
    };
    let declaration = Declaration {
        timebase,
        channels: vec![
            analog("A"),
            analog("B"),
            DeclaredChannel {
                name: "D".into(),
                source_name: "d".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Default::default(),
                overview_shift: Some(6),
                codec: None,
                requires: Vec::new(),
            },
        ],
        derived: vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            op: escapement_tickwave_format::header::Op::Multiply,
            operands: vec!["A".into(), "B".into()],
            params: Default::default(),
            requires: Vec::new(),
            significant: None,
        }],
        sources: vec![SourceHeader::new(
            SourceFormat::Other("crafted".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 6,
        requires: Vec::new(),
    };
    let mut w = Writer::new(
        declaration,
        WriteOptions {
            block_bytes: 4096,
            promise_with_items: true,
            level: 1,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let mut file = w.take_output();
    // Every item in axis order: (tick, what, n), what 0 a sample pair, 1 an edge, 2 a gap from it.
    let mut items: Vec<(i64, u8, u64)> = Vec::new();
    for k in 0..=BREAKS {
        for i in 0..RUN {
            items.push((run_tick(k) + 10 * i as i64, 0, k * RUN + i));
        }
    }
    for g in 0..GAPS {
        let t = 200 + 250 * g;
        items.push((t, 1, g as u64));
        items.push((t + 50, 2, g as u64));
    }
    items.sort();
    let mut breaks = String::new();
    let mut gaps = String::from("[0,30,false]");
    let mut edges = String::new();
    w.gap(
        2,
        Gap {
            from: 0,
            to: 30,
            level: false,
        },
    )
    .unwrap();
    let mut x = 0x2545_f491_4f6c_dd1du64;
    for (t, what, n) in items {
        match what {
            0 => {
                if n.is_multiple_of(RUN) {
                    let k = n / RUN;
                    let lost = match k {
                        0 => 3,
                        k if k.is_multiple_of(5) => 0,
                        _ => 2,
                    };
                    w.run(0, t, lost).unwrap();
                    w.run(1, t, lost).unwrap();
                    if k > 0 {
                        let sep = if breaks.is_empty() { "" } else { "," };
                        write!(breaks, "{sep}[{n},{t},{lost}]").unwrap();
                    }
                }
                w.sample(0, 1000 + n % 97).unwrap();
                w.sample(1, 2000 + (n * 31) % 89).unwrap();
            }
            1 => {
                w.edge(2, t).unwrap();
                let sep = if edges.is_empty() { "" } else { "," };
                write!(edges, "{sep}{t}").unwrap();
            }
            _ => {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let level = x.is_multiple_of(3);
                w.gap(
                    2,
                    Gap {
                        from: t,
                        to: t + 50,
                        level,
                    },
                )
                .unwrap();
                write!(gaps, ",[{t},{},{level}]", t + 50).unwrap();
            }
        }
        if n.is_multiple_of(1000) {
            file.extend(w.take_output());
        }
    }
    let length = run_tick(BREAKS) + 10 * RUN as i64 + 100;
    let finish = Finish {
        lost_after: BTreeMap::from([(0, 2), (1, 2)]),
        ..Finish::default()
    };
    file.extend(&w.finish(length, &[length], finish).unwrap().tail);
    std::fs::write(dir.join("lists.tickwave"), file).unwrap();
    let json = format!(
        "{{\"length\":{length},\"breaks\":[{breaks}],\"gaps\":[{gaps}],\"edges\":[{edges}]}}\n"
    );
    std::fs::write(dir.join("lists.json"), json).unwrap();
    println!("lists.tickwave");
    blocks(&dir);
    phased_lists(&dir);
}

fn phased_lists(dir: &std::path::Path) {
    const RUNS: u64 = 200;
    let timebase = Ratio::integer(1_000_000).unwrap();
    let declaration = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "C".into(),
            source_name: "c".into(),
            source: 0,
            data: DeclaredKind::Analog {
                unit: "V".into(),
                period: Ratio::new(5, 2).unwrap(),
                coding: AnalogCoding::Int(IntCoding::new(16, false)),
                significant: None,
                phase: Some(Ratio::new(3, 4).unwrap()),
            },
            shaping: Default::default(),
            overview_shift: Some(6),
            codec: None,
            requires: Vec::new(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("crafted".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 6,
        requires: Vec::new(),
    };
    let mut w = Writer::new(
        declaration,
        WriteOptions {
            level: 1,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let mut file = w.take_output();
    for k in 0..RUNS {
        let tick = 100 + 100 * k as i64;
        if k == 0 {
            w.run(0, tick, 0).unwrap();
        } else {
            w.run_at(0, tick, (k % 4) as u32, 0).unwrap();
        }
        // Sample 11's place is 27.5 ticks and the run's phase past its tick: at 28 where the phase
        // is 2/4 or 3/4, 27 otherwise. A flush there starts a block at it.
        let phase = if k == 0 { 3 } else { k % 4 };
        for j in 0..RUN {
            if j == 11 {
                w.flush(tick + if phase >= 2 { 28 } else { 27 }).unwrap();
            }
            w.sample(0, k * RUN + j).unwrap();
        }
    }
    file.extend(w.take_output());
    let length = 100 + 100 * RUNS as i64;
    file.extend(&w.finish(length, &[], Finish::default()).unwrap().tail);
    std::fs::write(dir.join("phased-lists.tickwave"), file).unwrap();
    println!("phased-lists.tickwave");
}

fn blocks(dir: &std::path::Path) {
    const EDGES: i64 = 70_000;
    let timebase = Ratio::integer(1000).unwrap();
    let digital = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let declaration = Declaration {
        timebase,
        channels: vec![digital("F"), digital("G")],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("crafted".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 12,
        requires: Vec::new(),
    };
    let mut w = Writer::new(declaration, WriteOptions::default()).unwrap();
    let mut file = w.take_output();
    w.edge(1, 100).unwrap();
    for k in 0..EDGES {
        w.edge(0, 5 + 10 * k).unwrap();
        // A flush promises every channel, so G's edge goes in before the one past it.
        if 10 * k == 600_000 {
            w.edge(1, 600_000).unwrap();
        }
        w.flush(10 + 10 * k).unwrap();
        file.extend(w.take_output());
    }
    let length = 10 * EDGES + 10;
    let elsewhere = format!(
        "format = \"tickwave-annotations/1\"\nrecording_id = \"blake3:{}\"\n",
        "00".repeat(32)
    );
    let finish = Finish {
        bundle: Some(Bundle::Files(BTreeMap::from([(
            "blocks.annotations.toml".to_owned(),
            elsewhere,
        )]))),
        ..Finish::default()
    };
    file.extend(&w.finish(length, &[length, length], finish).unwrap().tail);
    std::fs::write(dir.join("blocks.tickwave"), file).unwrap();
    println!("blocks.tickwave");
}
