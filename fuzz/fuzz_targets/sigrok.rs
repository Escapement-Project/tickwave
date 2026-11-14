//! A sigrok session file, as it is or a zip made of members, and every item it gives.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let bytes = escapement_tickwave_fuzz::session(data);
    if let Ok(import) = escapement_tickwave_import::sigrok::read_bytes(bytes, "f.sr") {
        escapement_tickwave_fuzz::drain(import.recording);
    }
});
