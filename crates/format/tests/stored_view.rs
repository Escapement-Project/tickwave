//! The stored view (ANNOTATIONS.md, "The stored view"): the hand-written file's `[view]` table,
//! held to the recording, and refused by the check where it doesn't hold; and a writer's, written
//! into the bundle it generates.

use std::collections::BTreeMap;

use escapement_tickwave_format::annotations::{Colour, Included, View, resolve, view};
use escapement_tickwave_format::decode::{Footer, from_memory};
use escapement_tickwave_format::header::{
    AnalogCoding, ChannelHeader, ChannelKind, Declaration, DeclaredChannel, DeclaredKind, Header,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::writer::{Bundle, Finish, WriteError, WriteOptions, Writer};

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

/// A writer's recording of D and V, 10 ticks long, with a marker from `verge` where `marked`,
/// finished with `finish`: its header and its bundle's files, or the writer's refusal.
fn written(marked: bool, finish: Finish) -> Result<(Header, BTreeMap<String, String>), WriteError> {
    let channel = |name: &str, data| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data,
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let d = Declaration {
        timebase: Ratio::ONE,
        channels: vec![
            channel("D", DeclaredKind::Digital { initial: false }),
            channel(
                "V",
                DeclaredKind::Analog {
                    unit: "V".into(),
                    period: Ratio::ONE,
                    coding: AnalogCoding::F32,
                    significant: None,
                    phase: None,
                },
            ),
        ],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("t".into()),
            Ratio::ONE,
        )],
        title: None,
        meta: Default::default(),
        overview_shift: 10,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    let mut file = w.take_output();
    w.run(1, 0, 0).unwrap();
    for k in 0..10 {
        w.sample(1, k as f32).unwrap();
    }
    w.edge(0, 4).unwrap();
    if marked {
        let serde_json::Value::Object(m) = serde_json::json!({"id": "m/1", "start": 2}) else {
            unreachable!()
        };
        w.annotation("verge", m).unwrap();
    }
    let finished = w.finish(10, &[10], finish)?;
    file.extend(finished.tail);
    let files = Footer::parse_file(&file)
        .unwrap()
        .read_sidecar(from_memory(&file))
        .unwrap()
        .map(|s| s.files)
        .unwrap_or_default();
    Ok((finished.header, files))
}

/// `files`' hand-written file checked as `annotations check` checks it, and its view.
fn checked(header: &Header, files: &BTreeMap<String, String>) -> Option<View> {
    let (hand, _) = files.iter().find(|(n, _)| n.ends_with(".toml")).unwrap();
    let included: Vec<Included<'_>> = files
        .iter()
        .filter(|(n, _)| n.ends_with(".jsonl"))
        .map(|(name, text)| Included { name, text })
        .collect();
    resolve(&files[hand], &included, header, None).unwrap();
    view(&files[hand], header).unwrap()
}

#[test]
fn a_writers_view_reads_back_from_the_bundle_it_generates() {
    let stored = View {
        channels: Some(vec!["V".into(), "D".into()]),
        start: Some(2),
        end: Some(8),
        colour: Some(Colour {
            day: "#d9480f".into(),
            night: "#ff8a50".into(),
        }),
    };
    for (marked, bundle, stem) in [
        (true, Some(Bundle::Generated { stem: "rec".into() }), "rec"),
        // With no annotation the bundle is the hand-written file alone, named after the default
        // stem where none is given.
        (false, Some(Bundle::Generated { stem: "rec".into() }), "rec"),
        (false, None, "recording"),
    ] {
        for v in [stored.clone(), View::default()] {
            let finish = Finish {
                bundle: bundle.clone(),
                view: Some(v.clone()),
                ..Finish::default()
            };
            let (header, files) = written(marked, finish).unwrap();
            let at = format!("marked {marked}, {bundle:?}, {v:?}");
            let names: Vec<&str> = files.keys().map(String::as_str).collect();
            let hand = format!("{stem}.annotations.toml");
            let jsonl = format!("{stem}.verge.jsonl");
            let want: Vec<&str> = match marked {
                true => vec![&hand, &jsonl],
                false => vec![&hand],
            };
            assert_eq!(names, want, "{at}");
            assert_eq!(checked(&header, &files), Some(v), "{at}");
        }
    }
}

#[test]
fn a_bundle_given_no_view_holds_no_view_table() {
    let finish = Finish {
        bundle: Some(Bundle::Generated { stem: "rec".into() }),
        ..Finish::default()
    };
    let (header, files) = written(true, finish).unwrap();
    assert!(!files["rec.annotations.toml"].contains("[view]"));
    assert_eq!(checked(&header, &files), None);
    // Nor, with no annotation, a bundle.
    let finish = Finish {
        bundle: Some(Bundle::Generated { stem: "rec".into() }),
        ..Finish::default()
    };
    assert!(written(false, finish).unwrap().1.is_empty());
}

#[test]
fn a_view_the_check_would_refuse_is_refused_with_its_problems() {
    let (header, files) = written(
        true,
        Finish {
            bundle: Some(Bundle::Generated { stem: "rec".into() }),
            ..Finish::default()
        },
    )
    .unwrap();
    for (table, says, v) in [
        (
            "channels = [\"D\", \"X\"]",
            "view: no channel \"X\"",
            View {
                channels: Some(vec!["D".into(), "X".into()]),
                ..View::default()
            },
        ),
        (
            "start = 8\nend = 2",
            "view: ends before it starts",
            View {
                start: Some(8),
                end: Some(2),
                ..View::default()
            },
        ),
        (
            "colour = [\"#d9480f\", \"orange\"]",
            "view: colour: each of a pair",
            View {
                colour: Some(Colour {
                    day: "#d9480f".into(),
                    night: "orange".into(),
                }),
                ..View::default()
            },
        ),
    ] {
        // The same recording's sidecar with the table, as `annotations check` reads it.
        let hand = format!("{}\n[view]\n{table}\n", files["rec.annotations.toml"]);
        let included = [Included {
            name: "rec.verge.jsonl",
            text: &files["rec.verge.jsonl"],
        }];
        let problems = resolve(&hand, &included, &header, None).unwrap_err();
        assert_eq!(problems.len(), 1, "{table}: {problems:?}");
        assert!(problems[0].starts_with(says), "{table}: {problems:?}");
        let finish = Finish {
            bundle: Some(Bundle::Generated { stem: "rec".into() }),
            view: Some(v),
            ..Finish::default()
        };
        let refused = written(true, finish).unwrap_err();
        assert_eq!(
            refused.to_string(),
            format!("the annotation bundle: {}", problems.join("; ")),
            "{table}"
        );
    }
}

#[test]
fn a_view_given_with_files_to_bundle_is_refused() {
    let files = BTreeMap::from([(
        "a.annotations.toml".to_string(),
        "format = \"tickwave-annotations/1\"\n".to_string(),
    )]);
    let finish = Finish {
        bundle: Some(Bundle::Files(files)),
        view: Some(View::default()),
        ..Finish::default()
    };
    let refused = written(false, finish).unwrap_err();
    assert!(
        matches!(&refused, WriteError::Bundle(why) if why.starts_with("a view given with files")),
        "{refused}"
    );
}
