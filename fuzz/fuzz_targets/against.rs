//! `verify --against`: a recording's header checked against what it says it came from, rebuilt
//! by `merge::expected` from two fixtures, or shaped by `Plan::of_header` from a capture. And a
//! recording that opens bundled with a sidecar, and merged with a fixture, as convert-wasm's
//! `merge` does.
#![no_main]

use std::sync::Arc;

use escapement_tickwave_format::bundle;
use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::model::Recording;
use escapement_tickwave_import::convert::{Pass, Progress, Step};
use escapement_tickwave_import::shape::Plan;
use escapement_tickwave_import::{merge, recording, vcd};
use libfuzzer_sys::fuzz_target;

static SWD: &[u8] = include_bytes!("../../web/fixtures/identify-swd.tickwave");
static ENERGY: &[u8] = include_bytes!("../../web/fixtures/energy.tickwave");
static VCD: &[u8] = include_bytes!("../../testdata/import/demo-logic.vcd");

struct Quiet;

impl Progress for Quiet {
    fn start(&self, _: Pass<'_>, _: u64) -> Step {
        Arc::new(|_| {})
    }
}

fn read(bytes: &'static [u8]) -> Recording {
    recording::read(Arc::new(bytes)).unwrap().1
}

fn capture() -> Recording {
    vcd::read_from(VCD, "demo-logic.vcd").unwrap().recording
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let file = escapement_tickwave_fuzz::file(rest);
    let Ok(footer) = Footer::parse_file(&file) else {
        return;
    };
    let sidecar = std::collections::BTreeMap::from([("r.annotations.toml".to_owned(), "x".into())]);
    if let Ok(bundled) = bundle::bundle(&file, &footer, sidecar) {
        Footer::parse_file(&bundled).expect("a bundled file reads");
    }
    let h = &footer.header;
    if mode % 3 == 2 {
        let input: Arc<[u8]> = file.into();
        let open = || {
            Ok(vec![
                merge::Input::read(Arc::new(Arc::clone(&input)), "fuzzed")?,
                merge::Input::read(Arc::new(SWD), "swd")?,
            ])
        };
        let settings = merge::Settings {
            level: 1,
            block_kib: 4,
            ..merge::Settings::default()
        };
        let mut out = std::io::Cursor::new(Vec::new());
        if merge::merge(&open, &settings, 1 << 20, &Quiet, &mut out).is_ok() {
            Footer::parse_file(out.get_ref()).expect("a merged file reads");
        }
    } else if mode & 1 == 0 {
        if let Ok(r) = merge::expected(h, vec![read(SWD), read(ENERGY)]) {
            escapement_tickwave_fuzz::drain(r);
        }
    } else if let Ok(mut plan) = Plan::of_header(&capture(), h) {
        if plan.cuts_edges() {
            plan.count(capture());
        }
        escapement_tickwave_fuzz::drain(plan.apply(capture()));
    }
});
