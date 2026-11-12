use escapement_tickwave_format::annotations::{Included, resolve, resolve_json};
use escapement_tickwave_format::header::{
    AnalogCoding, ChannelHeader, ChannelKind, Header, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;

const ID: &str = "blake3:00ff000000000000000000000000000000000000000000000000000000000000";

fn header() -> Header {
    Header {
        timebase: Ratio::integer(1_000_000_000).unwrap(),
        length: 1_000_000,
        channels: vec![
            ChannelHeader {
                name: "SWCLK".into(),
                source_name: "digital_3_0".into(),
                data: ChannelKind::Digital {
                    initial: true,
                    end: 1_000_000,
                    edges: 10,
                    gaps: Vec::new(),
                },
            },
            ChannelHeader {
                name: "SWDIO".into(),
                source_name: "digital_3_1".into(),
                data: ChannelKind::Digital {
                    initial: true,
                    end: 1_000_000,
                    edges: 10,
                    gaps: Vec::new(),
                },
            },
            ChannelHeader {
                name: "V".into(),
                source_name: "analog_0".into(),
                data: ChannelKind::Analog {
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::ONE,
                    count: 1_000_000,
                    breaks: Vec::new(),
                    coding: AnalogCoding::F32,
                },
            },
        ],
        source: SourceHeader::new(SourceFormat::Vcd),
        title: None,
        meta: Default::default(),
        content_id: ID.parse().unwrap(),
        blocks: 1,
        overview_shift: 12,
        overview: vec![12],
        overview_blocks: vec![],
        annotations: None,
    }
}

const HAND: &str = r#"
format = "tickwave-annotations/1"
recording = "rec.tickwave"
recording_id = "blake3:00ff000000000000000000000000000000000000000000000000000000000000"
include = ["rec.wire_swd.jsonl"]

[[annotation]]
id = "read-ack-check"
start = "317.858 us"
end = "317.95 us"
channels = ["SWCLK", "SWDIO"]
label = "The 12-cycle period"
measure = { channel = "SWCLK", what = "rate" }
values = { rate = "26.7 MHz" }

[[annotation]]
id = "first-misread"
ref = "wire_swd/misread/412000"
label = "Misreads at 25 MHz"
"#;

const GENERATED: &str = r#"{"format": "tickwave-annotations/1", "source": "wire_swd", "version": "9f3c2e1", "recording_id": "blake3:00ff000000000000000000000000000000000000000000000000000000000000"}
{"id": "misread/412000", "kind": "misread", "start": 412000, "end": 412900, "channels": ["SWDIO"], "label": "parity wrong", "values": {"read_data": "24.9 MHz", "asked": "25 MHz"}}

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
    let b = r#"{"format": "tickwave-annotations/1", "source": "b", "recording_id": "blake3:00ff000000000000000000000000000000000000000000000000000000000000"}
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
            "recording_id = \"blake3:00ff000000000000000000000000000000000000000000000000000000000000\"",
            "recording_id = \"blake3:aaaa000000000000000000000000000000000000000000000000000000000000\"",
        )
        .replace("id = \"first-misread\"", "id = \"read-ack-check\"")
        .replace("\"SWCLK\", \"SWDIO\"]", "\"SWCLK\", \"TCK\"]")
        .replace("end = \"317.95 us\"", "end = \"2 ms\"");
    let found = problems(&hand, GENERATED);
    assert_problem(
        &found,
        "the recording is blake3:00ff000000000000000000000000000000000000000000000000000000000000",
    );
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
    assert_problem(&problems(&hand, GENERATED), "unknown variant `mean`");
}

#[test]
fn includes_must_match() {
    let found = resolve(HAND, &[], &header(), None).unwrap_err();
    assert_problem(&found, "included but not provided");
    let generated = GENERATED.replace(
        "blake3:00ff000000000000000000000000000000000000000000000000000000000000",
        "blake3:bbbb000000000000000000000000000000000000000000000000000000000000",
    );
    assert_problem(
        &problems(HAND, &generated),
        "generated for blake3:bbbb000000000000000000000000000000000000000000000000000000000000",
    );
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

/// The browser parses the TOML in JavaScript and hands the resolver JSON: a sidecar resolves to
/// the same annotations, or the same problems, either way.
#[test]
fn the_toml_as_json_resolves_alike() {
    let broken = HAND
        .replace("id = \"first-misread\"", "id = \"read-ack-check\"")
        .replace("end = \"317.95 us\"", "end = \"2 ms\"");
    for hand in [HAND.to_string(), broken] {
        let value: serde_json::Value = toml::from_str(&hand).unwrap();
        let json = serde_json::to_string(&value).unwrap();
        let resolved = |r: fn(&str, &[Included<'_>], &Header, Option<&str>) -> _, text: &str| {
            r(text, &included(GENERATED), &header(), Some("rec.tickwave"))
        };
        assert_eq!(resolved(resolve_json, &json), resolved(resolve, &hand));
    }
}

/// The figures an annotation states, as a chart's data takes them.
fn values(a: &escapement_tickwave_format::annotations::Annotation) -> Option<Vec<(&str, &str)>> {
    a.values
        .as_ref()
        .map(|v| v.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect())
}

#[test]
fn values_resolve_and_an_alias_takes_its_targets() {
    let got = resolve(HAND, &included(GENERATED), &header(), Some("rec.tickwave")).unwrap();
    assert_eq!(values(&got[0]), Some(vec![("rate", "26.7 MHz")]));
    // The alias states none of its own, so it takes the generated annotation's.
    assert_eq!(
        values(&got[1]),
        Some(vec![("asked", "25 MHz"), ("read_data", "24.9 MHz")])
    );
    assert_eq!(values(&got[1]), values(&got[2]));
    assert_eq!(values(&got[3]), None);
    let json = serde_json::to_value(&got).unwrap();
    assert_eq!(json[1]["values"]["read_data"], "24.9 MHz");
    assert!(json[3]["values"].is_null());
}

#[test]
fn values_are_a_decimal_and_a_unit_under_a_plain_name() {
    let with = |name: &str, value: &str| {
        HAND.replace(
            "values = { rate = \"26.7 MHz\" }",
            &format!("values = {{ \"{name}\" = \"{value}\" }}"),
        )
    };
    for good in ["50 MHz", "-0.5 V", "1238.6 KiB/s", "12 %", "0 runs"] {
        let got = resolve(
            &with("read_data2", good),
            &included(GENERATED),
            &header(),
            None,
        );
        assert_eq!(values(&got.unwrap()[0]), Some(vec![("read_data2", good)]));
    }
    for bad in [
        "50MHz",
        "fifty MHz",
        "50",
        "1e3 Hz",
        "50  MHz",
        ".5 V",
        "5. V",
        "+5 V",
        "50 MHz each",
        "",
    ] {
        let found = problems(&with("rate", bad), GENERATED);
        assert_problem(
            &found,
            &format!("rate = {bad:?} is not a decimal and a unit"),
        );
    }
    for bad in ["Asked", "read-data", "1st", "_x", ""] {
        let found = problems(&with(bad, "50 MHz"), GENERATED);
        assert_problem(&found, &format!("name {bad:?} is not lower-case letters"));
    }
    // Micro is U+00B5, as in times; Greek mu U+03BC would compare unequal.
    resolve(
        &with("rate", "1.5 \u{b5}s"),
        &included(GENERATED),
        &header(),
        None,
    )
    .unwrap();
    let found = problems(&with("rate", "1.5 \u{3bc}s"), GENERATED);
    assert_problem(&found, "writes micro as Greek mu (U+03BC)");
    let hand = HAND.replace("rate = \"26.7 MHz\"", "rate = 26.7");
    assert_problem(&problems(&hand, GENERATED), "expected a string");
}

#[test]
fn a_misspelt_measure_field_is_refused() {
    let hand = HAND.replace("what = \"rate\" }", "what = \"rate\", windw = 3 }");
    assert_problem(&problems(&hand, GENERATED), "unknown field `windw`");
}

#[test]
fn a_ref_names_a_generated_annotation_only() {
    let hand = HAND.replace(
        "ref = \"wire_swd/misread/412000\"",
        "ref = \"read-ack-check\"",
    );
    assert_problem(
        &problems(&hand, GENERATED),
        "ref \"read-ack-check\" names a hand-written annotation, not a generated one",
    );
}

#[test]
fn a_time_too_large_for_the_arithmetic_is_refused_not_wrapped() {
    // 2 × the numerator passes i128 where the tick would not: a wrapped sum once read this as a
    // tick inside the capture.
    for start in [
        "-9.0000000000000000000000000000 s",
        "9.0000000000000000000000000000 s",
    ] {
        let hand = HAND.replace("start = \"317.858 us\"", &format!("start = \"{start}\""));
        assert_problem(&problems(&hand, GENERATED), "out of range");
    }
}

#[test]
fn an_included_file_sits_beside_the_hand_written_one() {
    for name in [
        "../rec.wire_swd.jsonl",
        "sub/rec.wire_swd.jsonl",
        "..\\\\rec.wire_swd.jsonl",
    ] {
        let hand = HAND.replace(
            "include = [\"rec.wire_swd.jsonl\"]",
            &format!("include = [{name:?}]"),
        );
        assert_problem(&problems(&hand, GENERATED), "is named on its own");
    }
}

#[test]
fn messages_point_at_what_the_author_wrote() {
    // A time of the wrong kind says what a time is.
    let hand = HAND.replace("start = \"317.858 us\"", "start = 1.5");
    assert_problem(
        &problems(&hand, GENERATED),
        "a whole tick count or a decimal with a unit",
    );
    // A generator file's head is reported at its own line, after any blank ones.
    let found = problems(
        HAND,
        &format!("\n\n\n{}", GENERATED.replace("\"source\"", "\"sauce\"")),
    );
    assert_problem(&found, "rec.wire_swd.jsonl, line 4:");
    // The page's JSON of the TOML file has no line the author could look up.
    let found = escapement_tickwave_format::annotations::resolve_json(
        "{\"format\": 1}",
        &included(GENERATED),
        &header(),
        None,
    )
    .unwrap_err();
    assert!(found.iter().all(|p| !p.contains(" at line ")), "{found:#?}");
}

#[test]
fn a_recording_id_that_is_no_content_id_is_refused() {
    let hand = HAND.replace(
        &format!("recording_id = \"{ID}\""),
        "recording_id = \"blake3:00ff\"",
    );
    assert_problem(&problems(&hand, GENERATED), "is not a content id");
}

#[test]
fn a_span_may_end_at_the_length_and_a_point_may_not_sit_there() {
    let last = format!("{GENERATED}{{\"id\": \"edge/1\", \"start\": 999999, \"end\": 1000000}}\n");
    resolve(HAND, &included(&last), &header(), Some("rec.tickwave")).unwrap();
    let past = format!("{GENERATED}{{\"id\": \"edge/2\", \"start\": 1000000}}\n");
    assert_problem(&problems(HAND, &past), "fall outside the capture");
}
