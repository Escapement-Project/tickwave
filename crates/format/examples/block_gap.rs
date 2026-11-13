//! Writes `block-gap.tickwave`, for the web's unit test of a gap that ends where a block starts: D
//! rises at 10, is not known from 50 to 100 and low after, a flush at 100 starts the second block
//! there, and D rises again at 150. Run from the repository:
//!
//!     cargo run -p escapement-tickwave-format --example block_gap -- DIR

use std::collections::BTreeMap;
use std::path::PathBuf;

use escapement_tickwave_format::header::{
    Declaration, DeclaredChannel, DeclaredKind, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{Gap, Ratio};
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("the directory to write into"),
    );
    let timebase = Ratio::integer(1000).unwrap();
    let declaration = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "D".into(),
            source_name: "d".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
        }],
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
    w.edge(0, 10).unwrap();
    w.flush(100).unwrap();
    w.gap(
        0,
        Gap {
            from: 50,
            to: 100,
            level: false,
        },
    )
    .unwrap();
    w.edge(0, 150).unwrap();
    let mut file = w.take_output();
    file.extend(w.finish(1000, &[1000], Finish::default()).unwrap().tail);
    std::fs::write(dir.join("block-gap.tickwave"), file).unwrap();
    println!("block-gap.tickwave");
}
