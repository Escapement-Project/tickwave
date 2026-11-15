use escapement_tickwave_format::annotations::{Included, resolve};
use escapement_tickwave_format::edit::edit_json;
use escapement_tickwave_format::header::{
    ChannelHeader, ChannelKind, Header, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;

const ID: &str = "blake3:00ff000000000000000000000000000000000000000000000000000000000000";

// A hand-written file in five pieces, split by hand where each table's text starts: the comment
// and blank lines above a header are that table's. Comments, odd spacing, a quoted header, an
// unmanaged `values` inline and as a table of its own, and a comment closing the file.
const HEAD: &str = r#"# The hand-written annotations for rec.tickwave.
format = "tickwave-annotations/1"
recording   =   'rec.tickwave'   # checked against the file name
recording_id = "blake3:00ff000000000000000000000000000000000000000000000000000000000000"
include = [ "rec.wire_swd.jsonl" ]
"#;
const A: &str = r#"
# Spans the page cites.

[[annotation]]
id = "read-ack-check"
start = "317.858 us"
end = "317.95 us"
channels = ["SWCLK", "SWDIO"]
label = "The 12-cycle period"   # shown in the inset
kind = "period"
measure = { channel = "SWCLK", what = "rate" }
"#;
const B: &str = r#"
# A name for the decoder's find.
[[annotation]]
  id="first-misread"
  ref = 'wire_swd/misread/412000'
  label = "Misreads at 25 MHz"
  values = { asked = "25 MHz" }
"#;
const C: &str = r#"
[[ annotation ]]
id = "stretch"
start = 500
kind = "stall"

[annotation.values]
stretched = "3 us"
"#;
const TAIL: &str = "\n# Kept at the end of the file.\n";

fn fixture() -> String {
    [HEAD, A, B, C, TAIL].concat()
}

/// The fields each fixture table holds now, as the page would send them.
const A_FIELDS: &str = r#"{"id": "read-ack-check", "start": "317.858 us", "end": "317.95 us",
    "channels": ["SWCLK", "SWDIO"], "label": "The 12-cycle period", "kind": "period",
    "measure": {"channel": "SWCLK", "what": "rate"}}"#;
const B_FIELDS: &str =
    r#"{"id": "first-misread", "ref": "wire_swd/misread/412000", "label": "Misreads at 25 MHz"}"#;
const C_FIELDS: &str = r#"{"id": "stretch", "start": 500, "kind": "stall"}"#;

const GENERATED: &str = r#"{"format": "tickwave-annotations/1", "source": "wire_swd", "recording_id": "blake3:00ff000000000000000000000000000000000000000000000000000000000000"}
{"id": "misread/412000", "kind": "misread", "start": 412000, "end": 412900, "channels": ["SWDIO"]}
"#;

fn header() -> Header {
    let digital = |name: &str| ChannelHeader {
        name: name.into(),
        source_name: name.into(),
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
    };
    Header {
        timebase: Ratio::integer(1_000_000_000).unwrap(),
        length: 1_000_000,
        channels: vec![digital("SWCLK"), digital("SWDIO")],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Vcd,
            Ratio::integer(1_000_000_000).unwrap(),
        )],
        title: None,
        meta: Default::default(),
        content_id: ID.parse().unwrap(),
        overview_shift: 12,
    }
}

/// The ids of `text`'s annotations once resolved, with each one's `values`, so an edit is known
/// to leave a sidecar that still checks, with every table's keys where they belong.
fn resolved(text: &str) -> Vec<(String, Option<String>)> {
    let included = [Included {
        name: "rec.wire_swd.jsonl",
        text: GENERATED,
    }];
    resolve(text, &included, &header(), Some("rec.tickwave"))
        .unwrap_or_else(|problems| panic!("{problems:?}\n{text}"))
        .into_iter()
        .filter(|a| a.source.is_none())
        .map(|a| (a.id, a.values.map(|v| format!("{v:?}"))))
        .collect()
}

fn ok(text: &str, op: &str) -> String {
    edit_json(text, op).unwrap_or_else(|e| panic!("{op}: {e}"))
}

fn refused(text: &str, op: &str) -> String {
    match edit_json(text, op) {
        Ok(out) => panic!("{op} was applied:\n{out}"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn start_then_insert_lays_out_a_new_file_as_the_spec_does() {
    let text = ok(
        "",
        &format!(r#"{{"op": "start", "recording": "rec.tickwave", "recording_id": "{ID}"}}"#),
    );
    assert_eq!(
        text,
        format!(
            "format = \"tickwave-annotations/1\"\nrecording = \"rec.tickwave\"\nrecording_id = \
             \"{ID}\"\n"
        )
    );
    // Fields in another order than the one written, a time as text and one as ticks.
    let text = ok(
        &text,
        r#"{"op": "insert", "index": 0, "fields": {"measure": {"channel": "SWCLK", "what": "rate"},
            "kind": "period", "end": 317950, "label": "The 12-cycle period", "start": "317.858 us",
            "channels": ["SWCLK", "SWDIO"], "id": "read-ack-check", "text": "Two\nlines",
            "paint": "probe"}}"#,
    );
    let text = ok(
        &text,
        r#"{"op": "insert", "index": 1, "fields": {"ref": "wire_swd/misread/412000",
            "id": "first-misread", "label": "NXP's firmware misreads at 25 MHz"}}"#,
    );
    assert_eq!(
        text,
        format!(
            r#"format = "tickwave-annotations/1"
recording = "rec.tickwave"
recording_id = "{ID}"

[[annotation]]
id = "read-ack-check"
start = "317.858 us"
end = 317950
channels = ["SWCLK", "SWDIO"]
label = "The 12-cycle period"
kind = "period"
text = """
Two
lines"""
paint = "probe"
measure = {{ channel = "SWCLK", what = "rate" }}

[[annotation]]
id = "first-misread"
ref = "wire_swd/misread/412000"
label = "NXP's firmware misreads at 25 MHz"
"#
        )
    );
}

#[test]
fn start_refuses_a_file_that_has_text() {
    let op = format!(r#"{{"op": "start", "recording": "rec.tickwave", "recording_id": "{ID}"}}"#);
    assert_eq!(
        refused(&fixture(), &op),
        "start makes a new file, and this one is not empty"
    );
    assert!(ok(" \n", &op).starts_with("format = "));
}

const NEW: &str = "\n[[annotation]]\nid = \"new\"\nstart = 10\n";
const INSERT: &str = r#"{"op": "insert", "index": INDEX, "fields": {"id": "new", "start": 10}}"#;

#[test]
fn insert_puts_a_table_between_the_others_and_leaves_them_alone() {
    let text = fixture();
    let at = |i: usize| ok(&text, &INSERT.replace("INDEX", &i.to_string()));
    assert_eq!(at(0), [HEAD, NEW, A, B, C, TAIL].concat());
    assert_eq!(at(1), [HEAD, A, NEW, B, C, TAIL].concat());
    assert_eq!(at(2), [HEAD, A, B, NEW, C, TAIL].concat());
    // After the last table's own `[annotation.values]`, which stays with it, and before the
    // comment that closes the file.
    assert_eq!(at(3), [HEAD, A, B, C, NEW, TAIL].concat());
    let ids = resolved(&at(3));
    assert_eq!(ids[2].1.as_deref(), Some(r#"{"stretched": "3 us"}"#));
    assert_eq!(ids[3], ("new".into(), None));
}

#[test]
fn replace_with_a_tables_own_fields_changes_nothing() {
    let text = fixture();
    for (i, fields) in [A_FIELDS, B_FIELDS, C_FIELDS].into_iter().enumerate() {
        let op = format!(r#"{{"op": "replace", "index": {i}, "fields": {fields}}}"#);
        assert_eq!(ok(&text, &op), text, "{op}");
    }
}

#[test]
fn replace_sets_removes_and_adds_fields_in_place() {
    let text = fixture();
    // The label changes and keeps its comment; kind and measure go with their lines; text is
    // new and goes after label; the times change type and keep their places.
    let out = ok(
        &text,
        r#"{"op": "replace", "index": 0, "fields": {"id": "read-ack-check", "start": 317858,
            "end": "317.96 us", "channels": ["SWCLK", "SWDIO"], "label": "The read's check",
            "text": "Twelve cycles"}}"#,
    );
    let a = r#"
# Spans the page cites.

[[annotation]]
id = "read-ack-check"
start = 317858
end = "317.96 us"
channels = ["SWCLK", "SWDIO"]
label = "The read's check"   # shown in the inset
text = "Twelve cycles"
"#;
    assert_eq!(out, [HEAD, a, B, C, TAIL].concat());
    // A new kind goes after label, before the unmanaged values; the indented keys keep their
    // indent.
    let out = ok(
        &text,
        r#"{"op": "replace", "index": 1, "fields": {"id": "first-misread",
            "ref": "wire_swd/misread/412000", "label": "Misreads at 25 MHz", "kind": "misread"}}"#,
    );
    let b = r#"
# A name for the decoder's find.
[[annotation]]
  id="first-misread"
  ref = 'wire_swd/misread/412000'
  label = "Misreads at 25 MHz"
kind = "misread"
  values = { asked = "25 MHz" }
"#;
    assert_eq!(out, [HEAD, A, b, C, TAIL].concat());
    // A new end goes after start, and the table's own `[annotation.values]` stays under it.
    let out = ok(
        &text,
        r#"{"op": "replace", "index": 2, "fields": {"id": "stretch", "start": 500, "end": 600,
            "kind": "stall"}}"#,
    );
    let c = r#"
[[ annotation ]]
id = "stretch"
start = 500
end = 600
kind = "stall"

[annotation.values]
stretched = "3 us"
"#;
    assert_eq!(out, [HEAD, A, B, c, TAIL].concat());
    assert_eq!(
        resolved(&out)[2],
        ("stretch".into(), Some(r#"{"stretched": "3 us"}"#.into()))
    );
}

#[test]
fn a_measure_written_as_dotted_keys_or_a_table_stays_until_it_changes() {
    let head = "format = 'x'\n\n[[annotation]]\nid = \"a\"\n";
    let dotted =
        format!("{head}measure.channel = \"SWCLK\"\nmeasure.what = \"rate\"\nlabel = \"l\"\n");
    let table = format!(
        "{head}label = \"l\"\n\n[annotation.measure]\nchannel = \"SWCLK\"\nwhat = \"rate\"\n"
    );
    let measure = |what: &str| {
        format!(
            r#"{{"op": "replace", "index": 0, "fields": {{"id": "a", "label": "l",
                "measure": {{"what": "{what}", "channel": "SWCLK"}}}}}}"#
        )
    };
    assert_eq!(ok(&dotted, &measure("rate")), dotted);
    assert_eq!(ok(&table, &measure("rate")), table);
    assert_eq!(
        ok(&dotted, &measure("period")),
        format!("{head}measure = {{ channel = \"SWCLK\", what = \"period\" }}\nlabel = \"l\"\n")
    );
    assert_eq!(
        ok(&table, &measure("period")),
        format!("{head}label = \"l\"\nmeasure = {{ channel = \"SWCLK\", what = \"period\" }}\n")
    );
}

#[test]
fn remove_takes_the_table_with_the_lines_above_it_and_the_tables_under_it() {
    let text = fixture();
    let remove = |i: usize| ok(&text, &format!(r#"{{"op": "remove", "index": {i}}}"#));
    assert_eq!(remove(0), [HEAD, B, C, TAIL].concat());
    assert_eq!(remove(1), [HEAD, A, C, TAIL].concat());
    assert_eq!(remove(2), [HEAD, A, B, TAIL].concat());
    let one = ok(&remove(2), r#"{"op": "remove", "index": 1}"#);
    assert_eq!(
        ok(&one, r#"{"op": "remove", "index": 0}"#),
        [HEAD, TAIL].concat()
    );
}

#[test]
fn move_carries_a_table_whole() {
    let text = fixture();
    let mv = |from: usize, to: usize| {
        ok(
            &text,
            &format!(r#"{{"op": "move", "from": {from}, "to": {to}}}"#),
        )
    };
    assert_eq!(mv(0, 1), [HEAD, B, A, C, TAIL].concat());
    assert_eq!(mv(1, 0), [HEAD, B, A, C, TAIL].concat());
    assert_eq!(mv(0, 2), [HEAD, B, C, A, TAIL].concat());
    assert_eq!(mv(2, 0), [HEAD, C, A, B, TAIL].concat());
    assert_eq!(mv(1, 2), [HEAD, A, C, B, TAIL].concat());
    assert_eq!(mv(1, 1), text);
    // `[annotation.values]` moved with its table: the table now before it doesn't take it.
    let ids = resolved(&mv(2, 0));
    assert_eq!(
        ids[0],
        ("stretch".into(), Some(r#"{"stretched": "3 us"}"#.into()))
    );
    assert_eq!(ids[2].0, "first-misread");
    assert_eq!(ids[2].1.as_deref(), Some(r#"{"asked": "25 MHz"}"#));
}

#[test]
fn a_crlf_file_keeps_its_line_ends() {
    let crlf = |s: &str| s.replace('\n', "\r\n");
    let text = crlf(&fixture());
    assert_eq!(
        ok(&text, &INSERT.replace("INDEX", "3")),
        crlf(&[HEAD, A, B, C, NEW, TAIL].concat())
    );
    let out = ok(
        &text,
        r#"{"op": "replace", "index": 2, "fields": {"id": "stretch", "start": 500}}"#,
    );
    let c = "\n[[ annotation ]]\nid = \"stretch\"\nstart = 500\n\n[annotation.values]\nstretched = \"3 us\"\n";
    assert_eq!(out, crlf(&[HEAD, A, B, c, TAIL].concat()));
    assert_eq!(
        ok(&text, r#"{"op": "move", "from": 2, "to": 0}"#),
        crlf(&[HEAD, C, A, B, TAIL].concat())
    );
}

#[test]
fn a_file_without_a_last_line_end_gains_one_where_a_table_follows() {
    let text = "format = 'x'\n\n[[annotation]]\nid = \"a\"";
    assert_eq!(
        ok(text, &INSERT.replace("INDEX", "1")),
        "format = 'x'\n\n[[annotation]]\nid = \"a\"\n\n[[annotation]]\nid = \"new\"\nstart = 10\n"
    );
    assert_eq!(
        ok(
            &format!("{text}\n{NEW}"),
            r#"{"op": "move", "from": 0, "to": 1}"#
        ),
        format!("format = 'x'\n{NEW}\n[[annotation]]\nid = \"a\"\n")
    );
    assert_eq!(
        ok(
            text,
            r#"{"op": "replace", "index": 0, "fields": {"id": "a", "start": 1}}"#
        ),
        "format = 'x'\n\n[[annotation]]\nid = \"a\"\nstart = 1\n"
    );
    assert_eq!(
        ok(text, r#"{"op": "remove", "index": 0}"#),
        "format = 'x'\n"
    );
    // A table first in the file gets no blank line above it.
    assert_eq!(
        ok("", &INSERT.replace("INDEX", "0")),
        "[[annotation]]\nid = \"new\"\nstart = 10\n"
    );
}

#[test]
fn an_index_out_of_range_is_refused() {
    let text = fixture();
    assert_eq!(
        refused(&text, &INSERT.replace("INDEX", "4")),
        "index 4 is out of range: the file has 3 annotations, so an insert takes 0 to 3"
    );
    for op in [
        r#"{"op": "replace", "index": 3, "fields": {}}"#,
        r#"{"op": "remove", "index": 3}"#,
        r#"{"op": "move", "from": 3, "to": 0}"#,
        r#"{"op": "move", "from": 0, "to": 3}"#,
    ] {
        assert_eq!(
            refused(&text, op),
            "index 3 is out of range: the file has 3 annotations",
            "{op}"
        );
    }
    assert_eq!(
        refused(HEAD, r#"{"op": "remove", "index": 0}"#),
        "index 0 is out of range: the file has 0 annotations"
    );
    let one = [HEAD, A].concat();
    assert_eq!(
        refused(&one, r#"{"op": "move", "from": 0, "to": 1}"#),
        "index 1 is out of range: the file has 1 annotation"
    );
}

#[test]
fn annotations_written_other_than_as_tables_are_refused() {
    let insert = INSERT.replace("INDEX", "0");
    let inline = format!("{HEAD}annotation = [{{ id = \"a\", start = 1 }}]\n");
    assert_eq!(
        refused(&inline, &insert),
        "`annotation` is an inline array, `annotation = [...]`; the editor edits \
         `[[annotation]]` tables only"
    );
    let not_tables = "`annotation` is not an array of `[[annotation]]` tables, which is what the \
                      editor edits";
    for text in [
        format!("{HEAD}\n[annotation]\nid = \"a\"\n"),
        format!("{HEAD}annotation = 3\n"),
        format!("{HEAD}annotation.id = \"a\"\n"),
    ] {
        assert_eq!(refused(&text, &insert), not_tables, "{text}");
    }
}

#[test]
fn a_parse_error_keeps_the_parsers_message() {
    let text = format!("{HEAD}\n[[annotation]]\nid = \n");
    let message = refused(&text, &INSERT.replace("INDEX", "0"));
    assert!(
        message.starts_with("TOML parse error at line 8, column 6"),
        "{message}"
    );
}

#[test]
fn an_edit_that_is_not_one_is_refused() {
    let text = fixture();
    for (op, wanted) in [
        (r#"{"op": "sort"}"#, "the edit: unknown variant `sort`"),
        (
            r#"{"op": "insert", "index": 0, "fields": {"values": {}}}"#,
            "the edit: unknown field `values`",
        ),
        (
            r#"{"op": "insert", "index": 0, "fields": {"start": 1.5}}"#,
            "the edit: invalid type: floating point `1.5`",
        ),
        (
            r#"{"op": "remove", "index": 0, "to": 1}"#,
            "the edit: unknown field `to`",
        ),
    ] {
        let message = refused(&text, op);
        assert!(message.starts_with(wanted), "{op}: {message}");
    }
}
