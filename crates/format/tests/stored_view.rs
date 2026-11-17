//! The stored view (ANNOTATIONS.md, "The stored view"): the hand-written file's `[view]` table,
//! held to the recording, and refused by the check where it doesn't hold.

use escapement_tickwave_format::annotations::{Colour, View, resolve, view};
use escapement_tickwave_format::header::{
    ChannelHeader, ChannelKind, Header, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;

const ID: &str = "blake3:00ff000000000000000000000000000000000000000000000000000000000000";

/// A recording of 1 ms at 1 ns a tick: TCKC and TMSC.
fn header() -> Header {
    let digital = |name: &str| ChannelHeader {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: ChannelKind::Digital {
            initial: true,
            end: 1_000_000,
            edges: 10,
            gaps: Vec::new(),
            gap_count: None,
        },
        shaping: Default::default(),
        overview_shift: None,
        blocks: 1,
        overview_blocks: Vec::new(),
        codec: None,
        requires: Vec::new(),
    };
    Header {
        timebase: Ratio::integer(1_000_000_000).unwrap(),
        length: 1_000_000,
        channels: vec![digital("TCKC"), digital("TMSC")],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Vcd,
            Ratio::integer(1_000_000_000).unwrap(),
        )],
        title: None,
        meta: Default::default(),
        content_id: ID.parse().unwrap(),
        overview_shift: 12,
        annotation_parts: None,
        requires: Vec::new(),
    }
}

fn hand(view_table: &str) -> String {
    format!(
        "format = \"tickwave-annotations/1\"\nrecording_id = \"{ID}\"\n\n[view]\n{view_table}\n"
    )
}

#[test]
fn a_view_reads_its_channels_span_and_colour_in_ticks() {
    let got = view(
        &hand(
            "channels = [\"TMSC\", \"TCKC\"]\nstart = \"0.25 ms\"\nend = 750000\ncolour = [\"#D9480F\", \"#ff8a50\"]",
        ),
        &header(),
    );
    assert_eq!(
        got,
        Ok(Some(View {
            channels: Some(vec!["TMSC".into(), "TCKC".into()]),
            start: Some(250_000),
            end: Some(750_000),
            colour: Some(Colour {
                day: "#d9480f".into(),
                night: "#ff8a50".into()
            }),
        }))
    );
    // Each field may be left out; so may the table.
    assert_eq!(
        view(&hand("colour = \"#112233\""), &header())
            .unwrap()
            .unwrap()
            .channels,
        None
    );
    assert_eq!(
        view(
            &format!("format = \"tickwave-annotations/1\"\nrecording_id = \"{ID}\"\n"),
            &header()
        ),
        Ok(None)
    );
}

#[test]
fn a_view_that_doesnt_hold_is_refused_saying_why() {
    for (table, says) in [
        ("channels = [\"TDO\"]", "view: no channel \"TDO\""),
        (
            "channels = [\"TCKC\", \"TCKC\"]",
            "view: channel \"TCKC\" is listed twice",
        ),
        ("channels = []", "view: channels lists none"),
        ("channels = \"TCKC\"", "view: channels is a list of names"),
        (
            "start = 600000\nend = 500000",
            "view: ends before it starts",
        ),
        (
            "start = 500000\nend = 500000",
            "view: ends before it starts",
        ),
        (
            "end = 1000001",
            "view: ticks 0 to 1000001 fall outside the capture, 0..1000000",
        ),
        (
            "start = -1",
            "view: ticks -1 to 1000000 fall outside the capture",
        ),
        (
            "start = 1000000",
            "view: ticks 1000000 to 1000000 fall outside the capture",
        ),
        ("start = \"12 minutes\"", "view: start:"),
        ("colour = \"orange\"", "view: colour is \"#rrggbb\""),
        (
            "colour = [\"#000000\", \"#fff\"]",
            "view: colour: each of a pair",
        ),
        ("zoom = 2", "view: unknown field \"zoom\""),
    ] {
        let problems = view(&hand(table), &header()).unwrap_err();
        assert!(
            problems.iter().any(|p| p.starts_with(says)),
            "{table}: {problems:?}"
        );
        // The check refuses the sidecar for it.
        let refused = resolve(&hand(table), &[], &header(), None).unwrap_err();
        assert!(
            refused.iter().any(|p| p.starts_with(says)),
            "{table}: {refused:?}"
        );
    }
    // At the capture's ends, fine.
    assert!(view(&hand("start = 0\nend = 1000000"), &header()).is_ok());
    assert!(resolve(&hand("start = 0\nend = 1000000"), &[], &header(), None).is_ok());
}
