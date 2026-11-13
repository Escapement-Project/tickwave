//! Writes web/fixtures/energy.tickwave: a synthetic recording laid out as verge's energy captures
//! are, through the writer a live recorder uses. Nothing in it comes from a capture. Run from the
//! repository:
//!
//!     cargo run -p escapement-tickwave-format --example energy -- web/fixtures
//!
//! - One source, `verge-energy`, at 96 MHz, reading no file.
//! - V and I, `int` 32-bit unsigned codes in mV and nA (scales 1/1000 and 1/10^9), a pair every
//!   960 ticks (100 kHz), 2,600 pairs: 1,000 from tick 0, then 50 lost, 800 from tick 1,008,000,
//!   then a pause, 800 from tick 2,000,000.
//! - P, derived: I × V in W, the integer product of the codes.
//! - `range` and `under_range`, digital, a few edges each; DIO0, a pulse at tick 0 (two edges
//!   there), a gap while the pairs were lost, and edges either side.
//! - Five markers from `verge`, kind `marker` with a unitless `number`, the one after the loss
//!   `marker-after-loss`, bundled into the footer at finish.
//! - A flush every 5 ms, so six short blocks; `end_reason` added to the source at its end.

use std::collections::BTreeMap;
use std::path::PathBuf;

use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, IntCoding,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{Gap, Ratio};
use escapement_tickwave_format::writer::{Bundle, Finish, WriteOptions, Writer};

const LENGTH: i64 = 2_880_000;
const PERIOD: i64 = 960;
/// Each run: its first index, its tick, the samples lost before it, and its sample count.
const RUNS: [(u64, i64, u64, u64); 3] = [
    (0, 0, 0, 1000),
    (1000, 1_008_000, 50, 800),
    (1800, 2_000_000, 0, 800),
];
const FLUSH: i64 = 480_000;

/// V's code, in mV, and I's, in nA, for pair `i`: about 3.3 V, and 1 mA stepping up to 1.75 mA.
fn pair(i: u64) -> (u64, u64) {
    let step = (i / 50) % 4;
    (
        3300 - 5 * step + i % 3,
        1_000_000 + 250_000 * step + (i * 7919) % 1000,
    )
}

enum Op {
    Run(i64, u64),
    Pair(u64),
    Edge(usize, i64),
    Gap(usize, Gap),
    Flush(i64),
    Marker(i64, &'static str, u32),
}

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("the directory to write into"),
    );
    let timebase = Ratio::integer(96_000_000).unwrap();
    let int = |den| {
        AnalogCoding::Int(IntCoding {
            scale: Exact::new(1, den),
            ..IntCoding::new(32, false)
        })
    };
    let analog = |name: &str, source_name: &str, unit: &str, coding| DeclaredChannel {
        name: name.into(),
        source_name: source_name.into(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: unit.into(),
            period: Ratio::integer(PERIOD as u64).unwrap(),
            coding,
            significant: Some(4),
        },
        shaping: Default::default(),
    };
    let digital = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Default::default(),
    };
    let declaration = Declaration {
        timebase,
        channels: vec![
            analog("V", "voltage", "V", int(1000)),
            analog("I", "current", "A", int(1_000_000_000)),
            digital("range"),
            digital("under_range"),
            digital("DIO0"),
        ],
        derived: vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            product: ["I".into(), "V".into()],
            significant: None,
        }],
        sources: vec![SourceHeader::new(
            SourceFormat::Other("verge-energy".into()),
            timebase,
        )],
        title: Some("A synthetic energy capture".into()),
        meta: BTreeMap::new(),
        overview_shift: 12,
    };

    // Every item at its tick; at one tick, in the order listed. A flush goes before the items at
    // its tick, a gap before those after its start.
    let mut ops: Vec<(i64, u8, Op)> = Vec::new();
    for &(first, tick, lost, count) in &RUNS {
        ops.push((tick, 1, Op::Run(tick, lost)));
        for k in 0..count {
            ops.push((tick + k as i64 * PERIOD, 2, Op::Pair(first + k)));
        }
    }
    for t in [480_000, 1_400_000, 2_100_000] {
        ops.push((t, 3, Op::Edge(2, t)));
    }
    for t in [300_000, 300_960, 1_700_160, 1_702_080] {
        ops.push((t, 3, Op::Edge(3, t)));
    }
    for t in [
        0, 0, 100_000, 200_000, 500_000, 900_000, 1_500_000, 2_500_000,
    ] {
        ops.push((t, 3, Op::Edge(4, t)));
    }
    // DIO0 is not known while the pairs were lost, from one period after the last pair before
    // the loss to the first after it; high after.
    let gap = Gap {
        from: 999 * PERIOD + PERIOD,
        to: 1_008_000,
        level: true,
    };
    ops.push((gap.from, 0, Op::Gap(4, gap)));
    for f in (FLUSH..LENGTH).step_by(FLUSH as usize) {
        ops.push((f, 0, Op::Flush(f)));
    }
    for (t, kind, number) in [
        (96_000, "marker", 1),
        (480_000, "marker", 2),
        (1_008_000, "marker-after-loss", 3),
        (2_000_000, "marker", 4),
        (2_700_000, "marker", 5),
    ] {
        ops.push((t, 4, Op::Marker(t, kind, number)));
    }
    ops.sort_by_key(|&(t, order, _)| (t, order));

    let mut w = Writer::new(
        declaration,
        WriteOptions {
            level: 19,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    for (_, _, op) in ops {
        match op {
            Op::Run(tick, lost) => {
                w.run(0, tick, lost).unwrap();
                w.run(1, tick, lost).unwrap();
            }
            Op::Pair(i) => {
                let (v, a) = pair(i);
                w.sample(0, v).unwrap();
                w.sample(1, a).unwrap();
            }
            Op::Edge(c, t) => w.edge(c, t).unwrap(),
            Op::Gap(c, g) => w.gap(c, g).unwrap(),
            Op::Flush(t) => w.flush(t).unwrap(),
            Op::Marker(start, kind, number) => {
                let marker = serde_json::json!({
                    "id": format!("{kind}/{start}"),
                    "kind": kind,
                    "start": start,
                    "label": format!("Marker {number}"),
                    "values": { "number": number.to_string() },
                });
                let serde_json::Value::Object(marker) = marker else {
                    unreachable!("an object")
                };
                w.annotation("verge", marker).unwrap();
            }
        }
    }
    let mut out = w.take_output();
    let finished = w
        .finish(
            LENGTH,
            &[LENGTH; 3],
            Finish {
                extra: vec![[("end_reason".to_owned(), "stopped".to_owned())].into()],
                bundle: Some(Bundle::Generated {
                    stem: "energy".into(),
                }),
                drop_gaps_past_end: false,
            },
        )
        .unwrap();
    out.extend(finished.tail);
    std::fs::write(dir.join("energy.tickwave"), &out).unwrap();
    println!(
        "energy.tickwave: {} bytes, {} blocks, {}",
        out.len(),
        finished.header.blocks,
        finished.header.content_id
    );
}
