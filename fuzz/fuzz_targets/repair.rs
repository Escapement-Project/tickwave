//! `repair` of any file. What it repairs reads back, at random and in order.
#![no_main]

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::reader::{Ending, Reader};
use escapement_tickwave_format::repair::Repairer;
use escapement_tickwave_format::writer::Bundle;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let file = escapement_tickwave_fuzz::file(data);
    let mut r = Repairer::new(1);
    if data.first().is_some_and(|b| b & 1 == 1) {
        r.bundle(Bundle::Generated { stem: "r".into() });
    }
    for piece in escapement_tickwave_fuzz::chunks(&file, data.len() as u64) {
        if r.feed(piece).is_err() {
            return;
        }
    }
    let Ok(repaired) = r.finish() else {
        return;
    };
    let mut out = file[..repaired.keep as usize].to_vec();
    out.extend(&repaired.tail);
    // A span near the most overview blocks a footer indexes repairs to millions of them, whose
    // reading back costs the target more than the repair took.
    if out.len() > 16 << 20 {
        return;
    }
    let footer = Footer::parse_file(&out).expect("a repaired file reads");
    assert_eq!(footer.header.content_id, repaired.header.content_id);
    let mut reader = Reader::default();
    reader.feed(&out);
    while reader
        .next_event()
        .expect("a repaired file reads in order")
        .is_some()
    {}
    assert_eq!(reader.end().unwrap(), Ending::Complete);
});
