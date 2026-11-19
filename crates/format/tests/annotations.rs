use escapement_tickwave_format::annotations::{
    Included, MeasureKind, generator_source, later_generator, lines_note, notes, notes_json,
    resolve, resolve_apart, resolve_apart_json, resolve_json, resolve_line, roles, view,
};
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
            },
            ChannelHeader {
                name: "SWDIO".into(),
                source_name: "digital_3_1".into(),
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
            },
            ChannelHeader {
                name: "V".into(),
                source_name: "analog_0".into(),
                source: 0,
                data: ChannelKind::Analog {
                    significant: None,
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::ONE,
                    count: 1_000_000,
                    breaks: Vec::new(),
                    coding: AnalogCoding::F32,
                    break_count: None,
                    lost_before: 0,
                    lost_after: 0,
                    phase: None,
                },
                shaping: Default::default(),
                overview_shift: None,
                blocks: 1,
                overview_blocks: Vec::new(),
                codec: None,
                requires: Vec::new(),
            },
        ],
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
        auxiliary: None,
        requires: Vec::new(),
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

const GENERATED: &str = r#"{"format": "tickwave-annotations/1", "source": "wire_swd", "version": "9f3c2e1", "recording_id": "blake3:00ff000000000000000000000000000000000000000000000000000000000000", "role": [{"name": "target"}]}
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

/// Resolved apart, the files come in sidecar order too: any the list doesn't name first, in the
/// order given, then the list's.
#[test]
fn files_resolved_apart_come_in_sidecar_order() {
    let hand = HAND.replace(
        r#"include = ["rec.wire_swd.jsonl"]"#,
        r#"include = ["rec.b.jsonl", "rec.wire_swd.jsonl"]"#,
    );
    let head = |source: &str| {
        format!(
            "{{\"format\": \"tickwave-annotations/1\", \"source\": \"{source}\", \"recording_id\": \"{ID}\"}}\n"
        )
    };
    let (b, c, d) = (head("b"), head("c"), head("d"));
    let given = [
        Included {
            name: "rec.wire_swd.jsonl",
            text: GENERATED,
        },
        Included {
            name: "rec.d.jsonl",
            text: &d,
        },
        Included {
            name: "rec.b.jsonl",
            text: &b,
        },
        Included {
            name: "rec.c.jsonl",
            text: &c,
        },
    ];
    let apart = resolve_apart(&hand, &given, &header(), &|_| None);
    let names: Vec<&str> = apart.files.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        [
            "rec.d.jsonl",
            "rec.c.jsonl",
            "rec.b.jsonl",
            "rec.wire_swd.jsonl"
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
fn values_are_a_decimal_and_its_unit_under_a_plain_name() {
    let with = |name: &str, value: &str| {
        HAND.replace(
            "values = { rate = \"26.7 MHz\" }",
            &format!("values = {{ \"{name}\" = \"{value}\" }}"),
        )
    };
    // A count has no unit: a marker's number.
    for good in [
        "50 MHz",
        "-0.5 V",
        "1238.6 KiB/s",
        "12 %",
        "0 runs",
        "17",
        "-3",
        "0.25",
    ] {
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
        "50 ",
        " 50",
        "1e3",
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
            &format!("rate = {bad:?} is not a decimal with one space and a unit where it has one"),
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
    // An entry's problem is reported where it was written: a generator's line, blank lines
    // counted, and the TOML file's annotation by its count.
    let found = problems(
        &HAND.replace("\"SWCLK\", \"SWDIO\"]", "\"SWCLK\", \"TCK\"]"),
        &GENERATED.replace(
            r#""channels": ["SWDIO"], "paint""#,
            r#""channels": ["NOPE"], "paint""#,
        ),
    );
    assert_problem(&found, "rec.wire_swd.jsonl, line 4: no channel \"NOPE\"");
    assert_problem(&found, "annotation 1: no channel \"TCK\"");
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

/// A generator file's head names the generator, and its file is named after it: a name holding
/// a path separator, of either kind, is refused.
#[test]
fn a_generator_name_holding_a_path_separator_is_refused() {
    for name in ["wire/swd", "wire\\swd"] {
        let text = GENERATED.replace("\"wire_swd\"", &serde_json::to_string(name).unwrap());
        let problems =
            resolve(HAND, &included(&text), &header(), Some("rec.tickwave")).unwrap_err();
        let want = format!(
            "rec.wire_swd.jsonl: generator name {name:?} holds a '/' or '\\', and its file is named \
             on its own"
        );
        assert!(problems.contains(&want), "{problems:?}");
    }
}

/// A lenient reader keeps what it can: a `ref` resolves only to an annotation it keeps, so a
/// `ref` to a generated annotation it leaves out is left out too, and named. Of an id given twice
/// it keeps the first, and a `ref` takes that one.
#[test]
fn a_lenient_ref_resolves_only_to_an_annotation_kept() {
    use escapement_tickwave_format::annotations::resolve_lenient;
    // The generator file is for another recording, so its lines are left out, and the alias with
    // them.
    let generated = GENERATED.replacen("00ff", "aaaa", 1);
    let got = resolve_lenient(HAND, &included(&generated), &header(), Some("rec.tickwave"));
    let ids: Vec<&str> = got.annotations.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, ["read-ack-check"]);
    assert_problem(
        &got.problems,
        "annotation 2: ref \"wire_swd/misread/412000\" names an annotation left out",
    );
    // The misread given twice, the second at 413,000: the first is kept, and the alias's span
    // is the first's.
    let second = r#"{"id": "misread/412000", "kind": "misread", "start": 413000, "end": 413900, "channels": ["SWDIO"]}"#;
    let generated = format!("{GENERATED}{second}\n");
    let got = resolve_lenient(HAND, &included(&generated), &header(), Some("rec.tickwave"));
    let alias = got
        .annotations
        .iter()
        .find(|a| a.id == "first-misread")
        .unwrap();
    assert_eq!((alias.start, alias.end), (412_000, Some(412_900)));
    assert_problem(&got.problems, "is used twice");
}

/// A page hands the hand-written file in as its TOML's JSON. Resolved apart from its generator
/// files' lines, it reads as its TOML does, and a problem in it names no line of the JSON, which
/// its author never saw.
#[test]
fn a_hand_written_file_resolved_apart_reads_the_same_from_its_json() {
    let json = serde_json::to_string(&toml::from_str::<toml::Value>(HAND).unwrap()).unwrap();
    let inc = included(GENERATED);
    let none = |_: &str| None;
    let from_toml = resolve_apart(HAND, &inc, &header(), &none);
    assert!(!from_toml.annotations.is_empty());
    assert_eq!(resolve_apart_json(&json, &inc, &header(), &none), from_toml);
    let bad = resolve_apart_json("{\"format\": 1}", &inc, &header(), &none);
    assert!(!bad.problems.is_empty());
    assert!(
        bad.problems.iter().all(|p| !p.contains(" at line ")),
        "{:#?}",
        bad.problems
    );
}

/// A hand-written file and a generator's head of a later minor version than this reader knows,
/// each with a table and a key it doesn't: what this reader does know reads as it would.
fn later(minor: &str) -> (String, String) {
    let hand = HAND.replace(
        "format = \"tickwave-annotations/1\"",
        &format!("format = \"tickwave-annotations/1.{minor}\""),
    ) + "\n[[decode]]\nanalysis = \"swd\"\nbinding = { clock = \"SWCLK\" }\n";
    let generated = GENERATED.replacen(
        "{\"format\": \"tickwave-annotations/1\", ",
        &format!("{{\"format\": \"tickwave-annotations/1.{minor}\", \"schema\": 3, "),
        1,
    );
    (hand, generated)
}

#[test]
fn a_later_minor_version_reads_as_this_one_without_what_it_adds() {
    let want = resolve(HAND, &included(GENERATED), &header(), Some("rec.tickwave")).unwrap();
    let (hand, generated) = later("1");
    let got = resolve(
        &hand,
        &included(&generated),
        &header(),
        Some("rec.tickwave"),
    )
    .unwrap();
    assert_eq!(got, want);
    let json = serde_json::to_string(&toml::from_str::<toml::Value>(&hand).unwrap()).unwrap();
    let got = resolve_json(
        &json,
        &included(&generated),
        &header(),
        Some("rec.tickwave"),
    )
    .unwrap();
    assert_eq!(got, want);
    // As a recording's bundle is read: the generated annotations and the roles stay too.
    let none = |_: &str| None;
    let apart = resolve_apart(&hand, &included(&generated), &header(), &none);
    assert_eq!(
        apart,
        resolve_apart(HAND, &included(GENERATED), &header(), &none)
    );
    assert_eq!(apart.files, [("rec.wire_swd.jsonl", true)]);
    assert_eq!(
        resolve_apart_json(&json, &included(&generated), &header(), &none),
        apart
    );
    let (roles, problems) = roles(&hand, &included(&generated));
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(roles, roles_of(HAND));
}

fn roles_of(hand: &str) -> Vec<escapement_tickwave_format::annotations::Role> {
    let (roles, problems) = roles(hand, &included(GENERATED));
    assert!(problems.is_empty(), "{problems:#?}");
    roles
}

/// What a reader leaves out of a later minor version it names, file by file. A file of a version it
/// knows, or of a later one holding nothing new, gets no note.
#[test]
fn what_this_reader_leaves_out_of_a_later_minor_version_is_noted() {
    let (hand, generated) = later("1");
    let want = [
        "the TOML file: \"tickwave-annotations/1.1\" is newer than this reader, which leaves out decode",
        "rec.wire_swd.jsonl: \"tickwave-annotations/1.1\" is newer than this reader, which leaves out schema",
    ];
    assert_eq!(notes(&hand, &included(&generated)), want);
    let json = serde_json::to_string(&toml::from_str::<toml::Value>(&hand).unwrap()).unwrap();
    assert_eq!(notes_json(&json, &included(&generated)), want);
    let bare = HAND.replace(
        "format = \"tickwave-annotations/1\"",
        "format = \"tickwave-annotations/1.1\"",
    );
    assert!(notes(&bare, &included(GENERATED)).is_empty());
    let (hand, generated) = later("0");
    assert!(notes(&hand, &included(&generated)).is_empty());
    assert!(notes(HAND, &included(GENERATED)).is_empty());
}

/// `later`, with keys this reader doesn't know inside the files too: of an annotation, of a
/// `measure`, of a role and of `[view]`, and a `measure` of a `what` it doesn't know, in the
/// hand-written file and in the generator's file, its first line and its lines.
fn later_inside(minor: &str) -> (String, String) {
    let (hand, generated) = later(minor);
    let hand =
        hand.replace(
            r#"measure = { channel = "SWCLK", what = "rate" }"#,
            r#"measure = { channel = "SWCLK", what = "rate", window = 3 }"#,
        )
        .replace(
            "values = { rate = \"26.7 MHz\" }\n",
            "values = { rate = \"26.7 MHz\" }\nconfidence = 0.9\n",
        ) + "\n[[annotation]]\nid = \"duty\"\nstart = 1000\nend = 2000\n\
           measure = { channel = \"SWCLK\", what = \"duty\" }\n\
           \n[[role]]\nname = \"probe\"\nglow = 2\n\n[view]\nzoom = 3\n";
    let generated = generated
        .replace(
            r#""role": [{"name": "target"}]"#,
            r#""role": [{"name": "target", "shade": 1}]"#,
        )
        .replace(
            r#""label": "parity wrong","#,
            r#""label": "parity wrong", "confidence": "0.9","#,
        )
        .replace(
            r#""paint": "target"}"#,
            r#""paint": "target", "measure": {"channel": "SWDIO", "what": "skew"}}"#,
        );
    (hand, generated)
}

/// ANNOTATIONS.md, "Versions": a later minor version may add keys of an annotation, a role,
/// `measure` and `[view]`, and values of `measure`'s `what`. A reader leaves each out, notes it
/// rather than calling it a problem, and keeps the annotation, role or view; at its own minor
/// version each is a problem, as an unknown top-level key is.
#[test]
fn a_later_minor_versions_keys_inside_the_files_are_left_out_and_noted() {
    let (hand, generated) = later_inside("1");
    let got = resolve(
        &hand,
        &included(&generated),
        &header(),
        Some("rec.tickwave"),
    )
    .unwrap();
    let ids: Vec<&str> = got.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "read-ack-check",
            "first-misread",
            "duty",
            "wire_swd/misread/412000",
            "wire_swd/drive/400000"
        ]
    );
    let measure = |id: &str| {
        let a = got.iter().find(|a| a.id == id).unwrap();
        a.measure.as_ref().map(|m| (m.channel.as_str(), m.what))
    };
    assert_eq!(
        measure("read-ack-check"),
        Some(("SWCLK", MeasureKind::Rate))
    );
    assert_eq!(measure("duty"), None);
    assert_eq!(measure("wire_swd/drive/400000"), None);
    let json = serde_json::to_string(&toml::from_str::<toml::Value>(&hand).unwrap()).unwrap();
    assert_eq!(
        resolve_json(
            &json,
            &included(&generated),
            &header(),
            Some("rec.tickwave")
        )
        .unwrap(),
        got
    );
    let (declared, refused) = roles(&hand, &included(&generated));
    assert!(refused.is_empty(), "{refused:#?}");
    let names: Vec<&str> = declared.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["probe", "target"]);
    assert_eq!(view(&hand, &header()), Ok(Some(Default::default())));
    let want = [
        "the TOML file: \"tickwave-annotations/1.1\" is newer than this reader, which leaves out \
         annotation.confidence, annotation.measure.what \"duty\", annotation.measure.window, \
         decode, role.glow, view.zoom",
        "rec.wire_swd.jsonl: \"tickwave-annotations/1.1\" is newer than this reader, which leaves \
         out annotation.confidence, annotation.measure.what \"skew\", role.shade, schema",
    ];
    assert_eq!(notes(&hand, &included(&generated)), want);
    assert_eq!(notes_json(&json, &included(&generated)), want);
    // A line read apart from its file, as a part's: its file's first line says how.
    assert!(later_generator(&generated));
    let line = generated.lines().nth(1).unwrap();
    let (a, left) = resolve_line("wire_swd", line, &header(), true).unwrap();
    assert_eq!(a.id, "wire_swd/misread/412000");
    assert_eq!(left, ["annotation.confidence".to_owned()].into());
    assert_eq!(
        lines_note("rec.wire_swd.jsonl", &generated, &left).as_deref(),
        Some(
            "rec.wire_swd.jsonl: \"tickwave-annotations/1.1\" is newer than this reader, which \
             leaves out annotation.confidence"
        )
    );
    let err = resolve_line("wire_swd", line, &header(), false).unwrap_err();
    assert!(err[0].contains("unknown field `confidence`"), "{err:?}");

    // At this minor version, each is a problem.
    let (hand, generated) = later_inside("0");
    assert!(!later_generator(&generated));
    assert!(notes(&hand, &included(&generated)).is_empty());
    for (from, to, needle) in [
        (
            "values = { rate = \"26.7 MHz\" }",
            "values = { rate = \"26.7 MHz\" }\nconfidence = 0.9",
            "unknown field `confidence`",
        ),
        (
            "what = \"rate\" }",
            "what = \"rate\", window = 3 }",
            "unknown field `window`",
        ),
        (
            "what = \"rate\"",
            "what = \"duty\"",
            "unknown variant `duty`",
        ),
        (
            "include = [\"rec.wire_swd.jsonl\"]",
            "include = [\"rec.wire_swd.jsonl\"]\nrole = [{ name = \"probe\", glow = 2 }]",
            "role \"probe\": unknown field \"glow\"",
        ),
        (
            "include = [\"rec.wire_swd.jsonl\"]",
            "include = [\"rec.wire_swd.jsonl\"]\nview = { zoom = 3 }",
            "view: unknown field \"zoom\"",
        ),
    ] {
        assert!(HAND.contains(from), "{from}");
        assert_problem(&problems(&HAND.replace(from, to), GENERATED), needle);
    }
    for (from, to, needle) in [
        (
            r#""label": "parity wrong","#,
            r#""label": "parity wrong", "confidence": "0.9","#,
            "unknown field `confidence`",
        ),
        (
            r#""paint": "target"}"#,
            r#""paint": "target", "measure": {"channel": "SWDIO", "what": "skew"}}"#,
            "unknown variant `skew`",
        ),
        (
            r#""role": [{"name": "target"}]"#,
            r#""role": [{"name": "target", "shade": 1}]"#,
            "role \"target\": unknown field \"shade\"",
        ),
    ] {
        assert!(GENERATED.contains(from), "{from}");
        assert_problem(&problems(HAND, &GENERATED.replace(from, to)), needle);
    }
}

/// FORMAT.md, "Unavailable channels": a channel of a kind this version doesn't know may have
/// edges in the version that defines it, so a measure on it is not held to be on a digital
/// channel. The channel stays one an annotation can name.
#[test]
fn a_measure_on_a_channel_of_a_kind_this_version_doesnt_know_is_not_refused() {
    let mut h = header();
    h.channels[0].data = ChannelKind::Other {
        kind: "events".into(),
    };
    let got = resolve(HAND, &included(GENERATED), &h, Some("rec.tickwave")).unwrap();
    assert_eq!(got[0].measure.as_ref().unwrap().channel, "SWCLK");
    // An analog channel's is refused, as ever.
    let hand = HAND.replace(r#"channel = "SWCLK", what"#, r#"channel = "V", what"#);
    assert_problem(
        &problems(&hand, GENERATED),
        "measure: \"V\" is analog; measures are taken from edges",
    );
}

/// FORMAT.md, "Annotations": an object in a generator's line, or in its file's first line, names
/// each key once, `values` included, whatever the file's minor version. A parse into a map would
/// keep the last, so every reader refuses such a line.
#[test]
fn a_key_named_twice_in_a_generators_line_is_refused_whatever_the_version() {
    let line = r#"{"id": "misread/412000", "kind": "misread", "start": 412000, "end": 412900, "channels": ["SWDIO"], "label": "parity wrong", "values": {"read_data": "24.9 MHz", "asked": "25 MHz"}}"#;
    assert!(GENERATED.contains(line));
    for minor in ["", ".1"] {
        let generated = GENERATED.replacen(
            "tickwave-annotations/1\"",
            &format!("tickwave-annotations/1{minor}\""),
            1,
        );
        for (twice, key) in [
            (r#""label": "parity wrong""#, "label"),
            (r#""asked": "25 MHz""#, "asked"),
        ] {
            let bad = generated.replacen(twice, &format!("{twice}, {twice}"), 1);
            assert_ne!(bad, generated);
            assert_problem(
                &problems(HAND, &bad),
                &format!("rec.wire_swd.jsonl, line 2: names key \"{key}\" twice"),
            );
            let read = bad.lines().nth(1).unwrap();
            let err = resolve_line("wire_swd", read, &header(), !minor.is_empty()).unwrap_err();
            assert_eq!(err, [format!("names key \"{key}\" twice")], "{minor}");
        }
        // Its first line.
        let bad = generated.replacen(
            r#""source": "wire_swd""#,
            r#""source": "wire_swd", "source": "x""#,
            1,
        );
        assert_problem(
            &problems(HAND, &bad),
            "rec.wire_swd.jsonl, line 1: names key \"source\" twice",
        );
    }
}

/// A key this reader doesn't know, in a file of a version it knows, is still a mistake.
#[test]
fn an_unknown_key_at_this_minor_version_is_refused() {
    let (hand, generated) = later("0");
    assert_problem(&problems(&hand, GENERATED), "unknown field `decode`");
    assert_problem(&problems(HAND, &generated), "unknown field `schema`");
    let bare = HAND.to_string() + "\n[[decode]]\nanalysis = \"swd\"\n";
    assert_problem(&problems(&bare, GENERATED), "unknown field `decode`");
}

#[test]
fn another_major_version_or_a_malformed_minor_is_another_format() {
    for format in [
        "2",
        "2.1",
        "1.",
        "1.x",
        "1.-1",
        "1.+1",
        "1.1.1",
        "10",
        "1.99999999999",
    ] {
        let hand = HAND.replace(
            "format = \"tickwave-annotations/1\"",
            &format!("format = \"tickwave-annotations/{format}\""),
        );
        assert_problem(&problems(&hand, GENERATED), "the TOML file's format is");
    }
}

/// A merge carries a generator's file by the name its head gives, from a later minor version too,
/// and never from another format.
#[test]
fn a_generator_file_is_named_by_its_head_of_this_major_version_only() {
    let (_, later) = later("1");
    assert_eq!(generator_source(&later).as_deref(), Some("wire_swd"));
    assert_eq!(generator_source(GENERATED).as_deref(), Some("wire_swd"));
    let other = GENERATED.replacen("tickwave-annotations/1", "tickwave-annotations/2", 1);
    assert_eq!(generator_source(&other), None);
}
