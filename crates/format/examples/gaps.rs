//! Writes web/fixtures/leading-gap.tickwave and middle-gap.tickwave: two digital channels, one
//! whose level is not known from the capture's start, as a moved or trimmed channel's is, and one
//! with a gap in the middle. Run from the repository:
//!
//!     cargo run -p escapement-tickwave-format --example gaps -- web/fixtures

use std::collections::BTreeMap;
use std::path::PathBuf;

use escapement_tickwave_format::header::{
    Declaration, DeclaredChannel, DeclaredKind, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{Gap, Ratio};
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};

const LENGTH: i64 = 1_000_000;

/// Two digital channels at 1 GS/s over 1 ms. A has `gap`, then a clock of 10 µs periods from
/// 400 µs; B is known throughout, with one pulse from 100 to 200 µs.
fn write(gap: Gap) -> Vec<u8> {
    let channel = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Default::default(),
    };
    let timebase = Ratio::integer(1_000_000_000).unwrap();
    let declaration = Declaration {
        timebase,
        channels: vec![channel("A"), channel("B")],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("crafted".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 12,
    };
    let mut w = Writer::new(declaration, WriteOptions::default()).unwrap();
    // The writer takes a gap where the items reach it; no edge lies in `from..=to`.
    let (from, to) = (gap.from, gap.to);
    let mut gap = Some(gap);
    let mut b = [100_000, 200_000].into_iter().peekable();
    for t in (400_000..LENGTH).step_by(5_000) {
        while let Some(&e) = b.peek().filter(|&&e| e < t) {
            w.edge(1, e).unwrap();
            b.next();
        }
        if let Some(g) = gap.filter(|g| g.from <= t) {
            w.gap(0, g).unwrap();
            gap = None;
        }
        if (from..=to).contains(&t) {
            continue;
        }
        w.edge(0, t).unwrap();
    }
    let mut out = w.take_output();
    out.extend(
        w.finish(LENGTH, &[LENGTH, LENGTH], Finish::default())
            .unwrap()
            .tail,
    );
    out
}

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("the directory to write into"),
    );
    for (name, gap) in [
        // Known from 300 µs, high there; the clock starts at 400 µs.
        ("leading-gap", (0, 300_000, true).into()),
        // Not known from 600 to 700 µs, high after.
        ("middle-gap", (600_000, 700_000, true).into()),
    ] {
        std::fs::write(dir.join(format!("{name}.tickwave")), write(gap)).unwrap();
        println!("{name}.tickwave");
    }
}
