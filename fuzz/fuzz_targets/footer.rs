//! `Footer::parse` and `parse_file`, and the importer's footer read from an input's end.
#![no_main]

use escapement_tickwave_format::decode::{DecodeError, Footer};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let file = escapement_tickwave_fuzz::file(data);
    let _ = Footer::parse_file(&file);
    let _ = Footer::parse_file_limited(&file, 1 << 16);
    // From the end, as the browser reads: some last bytes, then from where the footer starts.
    let len = file.len() as u64;
    let split = data
        .last()
        .map_or(0, |&b| file.len() * usize::from(b) / 255);
    if let Err(DecodeError::NeedFrom(at)) = Footer::parse(&file[split..], len)
        && at < split as u64
    {
        let _ = Footer::parse(&file[at as usize..], len);
    }
    let _ = escapement_tickwave_import::recording::footer(&file);
});
