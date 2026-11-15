//! A conversion as the browser's converter runs it: a Saleae export, a sigrok session file or a
//! VCD, through the codes, the encoding and the check of the output against the source.
#![no_main]

use std::io::Cursor;
use std::sync::Arc;

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::model::Recording;
use escapement_tickwave_format::verify::verify;
use escapement_tickwave_import::convert::{self, Pass, Progress, Settings, Step};
use escapement_tickwave_import::{saleae, sigrok, vcd};
use libfuzzer_sys::fuzz_target;

struct Quiet;

impl Progress for Quiet {
    fn start(&self, _: Pass<'_>, _: u64) -> Step {
        Arc::new(|_| {})
    }
}

type Open<'a> = Box<dyn Fn() -> Result<Recording, convert::Error> + 'a>;

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
    let open: Open<'_> = match kind % 3 {
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
            Box::new(move || Ok(export.recording()))
        }
        1 => {
            let bytes: Arc<[u8]> = escapement_tickwave_fuzz::session(rest).into();
            Box::new(move || Ok(sigrok::read_bytes(Arc::clone(&bytes), "f.sr")?.recording))
        }
        _ => Box::new(|| Ok(vcd::read_from(rest, "f.vcd")?.recording)),
    };
    let mut out = Cursor::new(Vec::new());
    if convert::convert(&*open, &settings, &Quiet, &mut out).is_ok() {
        Footer::parse_file(out.get_ref()).expect("a converted file reads");
        // The settings neither move, cut nor set units, so the source as read is what the
        // output holds.
        let source = open().expect("a source that converted opens again");
        verify(&out.get_ref()[..], source).expect("a converted file matches its source");
    }
});
