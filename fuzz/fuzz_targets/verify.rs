//! `verify` of a file against the recording the importer reads from it, and against a fixture's.
#![no_main]

use std::sync::{Arc, LazyLock};

use escapement_tickwave_format::verify::verify_limited;
use libfuzzer_sys::fuzz_target;

static FIXTURE: &[u8] = include_bytes!("../../web/fixtures/identify-swd.tickwave");

fn recording(bytes: &[u8]) -> Option<escapement_tickwave_format::model::Recording> {
    let input: Arc<[u8]> = bytes.into();
    escapement_tickwave_import::recording::read(Arc::new(input))
        .ok()
        .map(|(_, r)| r)
}

static ONE: LazyLock<()> = LazyLock::new(|| {
    assert!(recording(FIXTURE).is_some());
});

fuzz_target!(|data: &[u8]| {
    LazyLock::force(&ONE);
    let file = escapement_tickwave_fuzz::file(data);
    if let Some(source) = recording(&file) {
        let _ = verify_limited(&file[..], source, 1 << 20);
    }
    let _ = verify_limited(&file[..], recording(FIXTURE).unwrap(), 1 << 20);
});
