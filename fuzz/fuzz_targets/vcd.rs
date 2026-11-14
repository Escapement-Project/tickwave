//! A value change dump, and every item it gives.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(import) = escapement_tickwave_import::vcd::read_from(data, "f.vcd") {
        escapement_tickwave_fuzz::drain(import.recording);
    }
});
