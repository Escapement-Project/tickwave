use tickwave_format::annotations::{Included, resolve};
use tickwave_format::header::{AnalogCoding, ChannelHeader, ChannelKind, Header, SourceHeader};

const ID: &str = "blake3:00ff";

fn header() -> Header {
    Header {
        timebase: [1_000_000_000, 1],
        length: 1_000_000,
        channels: vec![
            ChannelHeader {
                name: "SWCLK".into(),
                source_name: "digital_3_0".into(),
                data: ChannelKind::Digital {
                    initial: true,
                    end: 1_000_000,
                    edges: 10,
                },
            },
            ChannelHeader {
                name: "SWDIO".into(),
                source_name: "digital_3_1".into(),
                data: ChannelKind::Digital {
                    initial: true,
                    end: 1_000_000,
                    edges: 10,
                },
            },
            ChannelHeader {
                name: "V".into(),
                source_name: "analog_0".into(),
                data: ChannelKind::Analog {
                    unit: "V".into(),
                    first: 0,
                    period: [1, 1],
                    count: 1_000_000,
                    coding: AnalogCoding::F32,
                },
            },
        ],
        source: SourceHeader::default(),
        title: None,
        meta: Default::default(),
        content_id: ID.into(),
        blocks: 1,
        overview: vec![12],
        overview_blocks: vec![],
        annotations: None,
    }
}

const HAND: &str = r#"
format = "tickwave-annotations/1"
recording = "rec.tickwave"
recording_id = "blake3:00ff"
include = ["rec.wire_swd.jsonl"]

[[annotation]]
id = "read-ack-check"
start = "317.858 us"
end = "317.95 us"
channels = ["SWCLK", "SWDIO"]
label = "The 12-cycle period"
measure = { channel = "SWCLK", what = "rate" }

[[annotation]]
id = "first-misread"
ref = "wire_swd/misread/412000"
label = "Misreads at 25 MHz"
"#;

const GENERATED: &str = r#"{"format": "tickwave-annotations/1", "source": "wire_swd", "version": "9f3c2e1", "recording_id": "blake3:00ff"}
{"id": "misread/412000", "kind": "misread", "start": 412000, "end": 412900, "channels": ["SWDIO"], "label": "parity wrong"}

{"id": "drive/400000", "kind": "drive", "start": 400000, "end": 400369, "channels": ["SWDIO"], "paint": "target"}
"#;

fn included(text: &str) -> Vec<Included<'_>> {
    vec![Included {
        name: "rec.wire_swd.jsonl",
        text,
    }]
}

#[test]
fn resolves_hand_written_generated_and_refs() {
    let got = resolve(HAND, &included(GENERATED), &header(), Some("rec.tickwave")).unwrap();
    let ids: Vec<&str> = got.iter().map(|a| a.id.as_str()).collect();
    // Sidecar order, not time order: the generated misread at 412,000 comes before the drive at
    // 400,000 because its file lists it first.
    assert_eq!(
        ids,
        [
            "read-ack-check",
            "first-misread",
            "wire_swd/misread/412000",
            "wire_swd/drive/400000"
        ]
    );
    let check = &got[0];
    assert_eq!((check.start, check.end), (317_858, Some(317_950)));
    assert_eq!(
        (check.source.as_deref(), check.target.as_deref()),
        (None, None)
    );
    let alias = &got[1];
    assert_eq!((alias.start, alias.end), (412_000, Some(412_900)));
    assert_eq!(alias.target.as_deref(), Some("wire_swd/misread/412000"));
    assert_eq!(alias.channels, ["SWDIO"]);
    assert_eq!(alias.label.as_deref(), Some("Misreads at 25 MHz"));
    assert_eq!(alias.kind.as_deref(), Some("misread"));
    assert_eq!(got[2].target, None);
    assert_eq!(got[3].source.as_deref(), Some("wire_swd"));
    assert_eq!(got[3].paint.as_deref(), Some("target"));
    let json = serde_json::to_value(&got).unwrap();
    assert_eq!(json[1]["ref"], "wire_swd/misread/412000");
    assert!(json[0].get("ref").is_none());
}

#[test]
fn included_files_follow_the_include_list_not_the_callers_order() {
    let hand = HAND.replace(
        r#"include = ["rec.wire_swd.jsonl"]"#,
        r#"include = ["rec.b.jsonl", "rec.wire_swd.jsonl"]"#,
    );
    let b = r#"{"format": "tickwave-annotations/1", "source": "b", "recording_id": "blake3:00ff"}
{"id": "x/1", "start": 1}
"#;
    let given = [
        Included {
            name: "rec.wire_swd.jsonl",
            text: GENERATED,
        },
        Included {
            name: "rec.b.jsonl",
            text: b,
        },
    ];
    let got = resolve(&hand, &given, &header(), Some("rec.tickwave")).unwrap();
    let ids: Vec<&str> = got.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "read-ack-check",
            "first-misread",
            "b/x/1",
            "wire_swd/misread/412000",
            "wire_swd/drive/400000"
        ]
    );
}

/// Resolves, expecting problems, and returns them.
fn problems(hand: &str, generated: &str) -> Vec<String> {
    resolve(hand, &included(generated), &header(), Some("rec.tickwave")).unwrap_err()
}

fn assert_problem(found: &[String], needle: &str) {
    assert!(
        found.iter().any(|p| p.contains(needle)),
        "no problem mentions {needle:?}: {found:#?}"
    );
}

#[test]
fn reports_every_problem() {
    let hand = HAND
        .replace(
            "recording_id = \"blake3:00ff\"",
            "recording_id = \"blake3:aaaa\"",
        )
        .replace("id = \"first-misread\"", "id = \"read-ack-check\"")
        .replace("\"SWCLK\", \"SWDIO\"]", "\"SWCLK\", \"TCK\"]")
        .replace("end = \"317.95 us\"", "end = \"2 ms\"");
    let found = problems(&hand, GENERATED);
    assert_problem(&found, "the recording is blake3:00ff");
    assert_problem(&found, "is used twice");
    assert_problem(&found, "no channel \"TCK\"");
    assert_problem(&found, "fall outside the capture");
}

#[test]
fn a_dropped_generated_annotation_breaks_its_ref() {
    let generated = GENERATED.replace("misread/412000", "misread/412001");
    let found = problems(HAND, &generated);
    assert_problem(&found, "ref \"wire_swd/misread/412000\" does not resolve");
}

#[test]
fn hand_written_ids_cannot_take_a_slash() {
    let hand = HAND.replace("id = \"read-ack-check\"", "id = \"wire_swd/fake\"");
    assert_problem(&problems(&hand, GENERATED), "only generated ids carry");
}

#[test]
fn measure_wants_a_digital_span() {
    let hand = HAND.replace(
        "channel = \"SWCLK\", what = \"rate\"",
        "channel = \"V\", what = \"rate\"",
    );
    assert_problem(&problems(&hand, GENERATED), "\"V\" is analog");
    let hand = HAND.replace("end = \"317.95 us\"\n", "");
    assert_problem(&problems(&hand, GENERATED), "needs a span");
}

#[test]
fn measure_knows_rate_period_and_duration_only() {
    for what in ["period", "duration"] {
        let hand = HAND.replace("what = \"rate\"", &format!("what = \"{what}\""));
        resolve(&hand, &included(GENERATED), &header(), Some("rec.tickwave")).unwrap();
    }
    let hand = HAND.replace("what = \"rate\"", "what = \"mean\"");
    assert_problem(&problems(&hand, GENERATED), "\"mean\" is not one of");
}

#[test]
fn includes_must_match() {
    let found = resolve(HAND, &[], &header(), None).unwrap_err();
    assert_problem(&found, "included but not provided");
    let generated = GENERATED.replace("blake3:00ff", "blake3:bbbb");
    assert_problem(&problems(HAND, &generated), "generated for blake3:bbbb");
}

#[test]
fn the_file_name_must_match_when_known() {
    let found = resolve(
        HAND,
        &included(GENERATED),
        &header(),
        Some("other.tickwave"),
    )
    .unwrap_err();
    assert_problem(&found, "names \"rec.tickwave\", not \"other.tickwave\"");
}

#[test]
fn unknown_fields_are_refused() {
    let hand = HAND.replace("label = \"The 12-cycle period\"", "lable = \"typo\"");
    assert_problem(&problems(&hand, GENERATED), "unknown field");
}
