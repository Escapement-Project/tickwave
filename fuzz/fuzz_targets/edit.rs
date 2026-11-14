//! Sidecar edits: any hand-written file and any edit, as the annotation editor sends them.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let (file, op) = text.split_once('\0').unwrap_or((&text, ""));
    let _ = escapement_tickwave_format::edit::edit_json(file, op);
});
