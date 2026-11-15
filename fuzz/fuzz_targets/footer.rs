//! `Footer::parse` and `parse_file`, each strip decoded whole and held to 64 KiB, the index the
//! footer points at, and the importer's footer read from an input's end.
#![no_main]

use escapement_tickwave_format::decode::{self, DecodeError, Footer, Index};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let file = escapement_tickwave_fuzz::file(data);
    if let Ok(footer) = Footer::parse_file(&file) {
        for o in 0..footer.overview_channels() {
            let _ = footer.strip(o);
            let _ = footer.strip_with(o, |f, n| decode::inflate_limited(f, n, 1 << 16));
        }
        let _ = Index::read(footer, decode::from_memory(&file));
    }
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
