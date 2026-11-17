//! Writes web/fixtures/burst.tickwave: a long idle recording with one concentrated burst, whose
//! finest overview level, chosen from the channel's average density, is far coarser than the
//! burst. Nothing in it comes from a capture. Run from the repository:
//!
//!     cargo run --release -p escapement-tickwave-format --example burst -- web/fixtures
//!
//! - One source at 1 GHz, an hour long.
//! - D: an edge every 10 s, from tick 10^10, and from tick 1,800,000,000,001 (30 minutes and a
//!   tick) a burst of 20,000,000 edges 2 ticks apart, a 250 MHz clock for 40 ms. Low before its
//!   first edge.
//! - Q: an edge every 10 s alone.
//!
//! A read of D past its finest level holds the whole burst until the view is under 10 ms or so
//! wide, and the burst holds 10,000,000 periods.

use std::path::PathBuf;

use escapement_tickwave_format::encode::{Options, encode_to};
use escapement_tickwave_format::model::{
    Channel, ChannelData, Digital, Ratio, Recording, Source, SourceFormat,
};

const HZ: i64 = 1_000_000_000;
const LENGTH: i64 = 3_600 * HZ;
const AT: i64 = 1_800 * HZ + 1;
const BURST: i64 = 20_000_000;

/// An edge every 10 s in [from, to).
fn idle(from: i64, to: i64) -> impl Iterator<Item = i64> + Clone + Send {
    (1..)
        .map(|k| k * 10 * HZ)
        .skip_while(move |&t| t < from)
        .take_while(move |&t| t < to)
}

fn digital(name: &str, edges: impl Iterator<Item = i64> + Clone + Send + 'static) -> Channel {
    Channel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: ChannelData::Digital(Digital {
            initial: false,
            end: LENGTH,
            count: edges.clone().count() as u64,
            edges: Box::new(edges),
            gaps: Vec::new(),
        }),
        shaping: Default::default(),
    }
}

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("the directory to write into"),
    );
    let burst = idle(0, AT)
        .chain((0..BURST).map(|i| AT + 2 * i))
        .chain(idle(AT + 2 * BURST, LENGTH));
    let timebase = Ratio::integer(HZ as u64).unwrap();
    let r = Recording {
        timebase,
        length: LENGTH,
        channels: vec![digital("D", burst), digital("Q", idle(0, LENGTH))],
        derived: Vec::new(),
        sources: vec![Source {
            files: vec!["burst".into()],
            begin_s: Some(0.0),
            ..Source::new(SourceFormat::Other("synthetic".into()), timebase)
        }],
    };
    let file = std::fs::File::create(dir.join("burst.tickwave")).unwrap();
    encode_to(r, &Options::default(), std::io::BufWriter::new(file)).unwrap();
}
