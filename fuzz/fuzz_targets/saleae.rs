//! A Saleae binary export, version 0 and 1: its files' headers, the rate inferred or given, every
//! edge checked, and every item a recording of it gives.
#![no_main]

use escapement_tickwave_import::saleae;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&rate, rest)) = data.split_first() else {
        return;
    };
    let (files, channels) = escapement_tickwave_fuzz::export(rest);
    for (path, input) in &files {
        let _ = saleae::file_info(path.clone(), input.clone());
    }
    let Ok(headers) = saleae::Headers::read(files, &channels, escapement_tickwave_fuzz::rate(rate))
    else {
        return;
    };
    let _ = headers.edges();
    if let Ok(export) = headers.check(&mut |_| {}) {
        escapement_tickwave_fuzz::drain(export.recording());
    }
});
