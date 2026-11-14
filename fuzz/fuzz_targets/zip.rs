//! A zip of files to open: its directory, its limits and every entry it takes, unzipped.
#![no_main]

use std::sync::Arc;

use escapement_tickwave_import::zipped::{Data, Limits, Zipped};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let input: Arc<[u8]> = data.into();
    let limits = Limits {
        entries: 64,
        inflated: 1 << 24,
    };
    let Ok(mut zipped) = Zipped::open(Arc::new(input), &|_| true, limits) else {
        return;
    };
    for entry in zipped.entries().to_vec() {
        if entry.data == Data::Deflated {
            let _ = zipped.inflate(&entry, &mut |_| {});
        }
    }
});
