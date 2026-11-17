//! Sidecars: the hand-written file and its included files, resolved against a fixture's header as
//! TOML and as JSON, leniently, their roles and their stored view, and the helpers that read one
//! file alone.
#![no_main]

use std::sync::LazyLock;

use escapement_tickwave_format::annotations::{self, Included};
use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::header::Header;
use libfuzzer_sys::fuzz_target;

static HEADER: LazyLock<Header> = LazyLock::new(|| {
    Footer::parse_file(include_bytes!("../../web/fixtures/identify-swd.tickwave"))
        .unwrap()
        .header
});

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let mut parts = text.split('\0');
    let hand = parts.next().unwrap_or_default();
    let rest: Vec<&str> = parts.collect();
    let included: Vec<Included<'_>> = rest
        .chunks(2)
        .map(|p| Included {
            name: p[0],
            text: p.get(1).copied().unwrap_or_default(),
        })
        .collect();
    let _ = annotations::resolve_lenient(hand, &included, &HEADER, None);
    let _ = annotations::roles(hand, &included);
    let _ = annotations::roles_json(hand, &included);
    let _ = annotations::view(hand, &HEADER);
    let _ = annotations::view_json(hand, &HEADER);
    let _ = annotations::resolve(hand, &included, &HEADER, Some("identify-swd.tickwave"));
    let _ = annotations::resolve_json(hand, &included, &HEADER, None);
    let _ = annotations::hand_written_count(hand);
    for i in &included {
        let _ = annotations::generator_source(i.text);
        let _ = annotations::generator_roles(i.text);
    }
    for time in hand.lines().take(8) {
        let _ = annotations::ticks(&annotations::Time::Text(time.into()), HEADER.timebase);
    }
});
