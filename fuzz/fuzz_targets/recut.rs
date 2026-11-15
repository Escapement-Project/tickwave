//! The `Recutter` fed a file in chunks, its output drained as it goes. What it finishes reads
//! back, at the content id it says.
#![no_main]

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::reader::{Ending, Reader};
use escapement_tickwave_format::recut::Recutter;
use escapement_tickwave_format::writer::{Bundle, Compression, WriteOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let file = escapement_tickwave_fuzz::file(data);
    let knob = data.first().copied().unwrap_or(0);
    let mut r = Recutter::new(WriteOptions {
        level: 1,
        block_bytes: 16 << (knob & 15),
        compression: if knob >> 6 == 0 {
            Compression::Spread
        } else {
            Compression::Offload
        },
        ..WriteOptions::default()
    });
    if knob & 16 != 0 {
        r.bundle(Bundle::Generated { stem: "r".into() });
    }
    let mut out = Vec::new();
    let seed = data.len() as u64 ^ u64::from(knob) << 32;
    for piece in escapement_tickwave_fuzz::chunks(&file, seed) {
        if r.feed(piece).is_err() {
            return;
        }
        out.extend(r.take_output());
    }
    let Ok(finished) = r.finish() else {
        return;
    };
    out.extend(&finished.tail);
    // As in the repair target: millions of overview blocks cost more to read back than to make.
    if out.len() > 16 << 20 {
        return;
    }
    let footer = Footer::parse_file(&out).expect("a recut file reads");
    assert_eq!(footer.header.content_id, finished.header.content_id);
    let mut reader = Reader::default();
    reader.feed(&out);
    while reader
        .next_event()
        .expect("a recut file reads in order")
        .is_some()
    {}
    assert_eq!(reader.end().unwrap(), Ending::Complete);
});
