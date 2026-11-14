//! A conversion as the browser's converter runs it: a Saleae export, a sigrok session file or a
//! VCD, through the codes, the encoding and the check of the output against the source.
#![no_main]

use std::io::Cursor;
use std::sync::Arc;

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_import::convert::{self, Pass, Progress, Settings, Step};
use escapement_tickwave_import::{saleae, sigrok, vcd};
use libfuzzer_sys::fuzz_target;

struct Quiet;

impl Progress for Quiet {
    fn start(&self, _: Pass<'_>, _: u64) -> Step {
        Arc::new(|_| {})
    }
}

fuzz_target!(|data: &[u8]| {
    let [kind, knob, rest @ ..] = data else {
        return;
    };
    let settings = Settings {
        level: 1,
        block_kib: 1 + u32::from(knob & 7),
        codes: knob & 8 != 0,
        ..Settings::default()
    };
    let mut out = Cursor::new(Vec::new());
    let converted = match kind % 3 {
        0 => {
            let [rate, export @ ..] = rest else {
                return;
            };
            let (files, channels) = escapement_tickwave_fuzz::export(export);
            let Ok(headers) =
                saleae::Headers::read(files, &channels, escapement_tickwave_fuzz::rate(*rate))
            else {
                return;
            };
            let Ok(export) = convert::check_saleae(headers, &Quiet) else {
                return;
            };
            convert::convert(
                &|| Ok(export.recording()),
                &settings,
                1 << 20,
                &Quiet,
                &mut out,
            )
        }
        1 => {
            let bytes: Arc<[u8]> = escapement_tickwave_fuzz::session(rest).into();
            let open = || Ok(sigrok::read_bytes(Arc::clone(&bytes), "f.sr")?.recording);
            convert::convert(&open, &settings, 1 << 20, &Quiet, &mut out)
        }
        _ => {
            let open = || Ok(vcd::read_from(rest, "f.vcd")?.recording);
            convert::convert(&open, &settings, 1 << 20, &Quiet, &mut out)
        }
    };
    if converted.is_ok() {
        Footer::parse_file(out.get_ref()).expect("a converted file reads");
    }
});
