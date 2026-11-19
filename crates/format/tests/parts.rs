//! Annotation records and parts (FORMAT.md, "Annotations", "Index", "Repairing a cut-off file"
//! and "Re-cutting"): records and parts as bytes, the parts a writer writes and when, the parts
//! table and the read API over it, repair and re-cutting with parts, and the repair and writer
//! crashes the fuzzer found, each against what the spec says.

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use escapement_tickwave_format::compress::compress;
use escapement_tickwave_format::decode::{DecodeError, Footer, from_memory};
use escapement_tickwave_format::header::{
    ChannelKind, Declaration, DeclaredChannel, DeclaredKind, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::index::{Due, Page, Table, TableIndex, TableWriter};
use escapement_tickwave_format::model::{Gap, Ratio};
use escapement_tickwave_format::parts::{Part, PartRow};
use escapement_tickwave_format::reader::{Ending, Event, Reader};
use escapement_tickwave_format::record::{self, Kind};
use escapement_tickwave_format::recut::Recutter;
use escapement_tickwave_format::repair::Repairer;
use escapement_tickwave_format::writer::{
    Bundle, Finish, WriteError, WriteOptions, Writer, generator_file,
};

fn digital(name: &str) -> DeclaredChannel {
    DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    }
}

fn declaration() -> Declaration {
    let timebase = Ratio::integer(1_000_000).unwrap();
    Declaration {
        timebase,
        channels: vec![digital("D")],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
        requires: Vec::new(),
    }
}

fn options() -> WriteOptions {
    WriteOptions {
        level: 3,
        ..WriteOptions::default()
    }
}

fn mark(id: &str, start: i64, end: Option<i64>) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("id".into(), id.into());
    m.insert("start".into(), start.into());
    if let Some(end) = end {
        m.insert("end".into(), end.into());
    }
    m
}

fn stem() -> Option<Bundle> {
    Some(Bundle::Generated { stem: "rec".into() })
}

/// The whole file a writer gives, finished at `length` with D known to it, bundled as `rec`.
fn finished(mut w: Writer, length: i64) -> Vec<u8> {
    let mut file = w.take_output();
    let finish = Finish {
        bundle: stem(),
        ..Finish::default()
    };
    file.extend(w.finish(length, &[length], finish).unwrap().tail);
    file
}

fn read_in_order(file: &[u8]) -> Result<(Vec<Event>, Ending), DecodeError> {
    let mut r = Reader::default();
    r.feed(file);
    let mut events = Vec::new();
    while let Some(e) = r.next_event()? {
        events.push(e);
    }
    Ok((events, r.end()?))
}

/// Every record before the footer: where it starts, its kind and its payload.
fn records(file: &[u8]) -> Vec<(u64, u8, Vec<u8>)> {
    let mut at = 12;
    let mut out = Vec::new();
    while let record::Next::Record { kind, payload, len } = record::next(&file[at..], file.len()) {
        if kind == Kind::Footer as u8 {
            break;
        }
        out.push((at as u64, kind, payload.to_vec()));
        at += len;
    }
    out
}

fn annotation_record(source: &str, line: &str) -> Vec<u8> {
    let a = record::Annotation {
        source: source.into(),
        line: line.into(),
    };
    let mut out = Vec::new();
    record::write(&mut out, Kind::Annotation, &[&a.to_payload()]);
    out
}

/// A part record of `raw`, what its frame decodes to.
fn part_record_of(raw: &[u8]) -> Vec<u8> {
    let frame = compress(raw, 3).unwrap();
    let mut out = Vec::new();
    record::write(
        &mut out,
        Kind::Part,
        &[&(raw.len() as u32).to_le_bytes(), &frame],
    );
    out
}

fn part_record(source: &str, lines: &str) -> Vec<u8> {
    part_record_of(
        &Part {
            source: source.into(),
            lines: lines.into(),
        }
        .to_raw(),
    )
}

fn opening() -> Vec<u8> {
    Writer::new(declaration(), options()).unwrap().take_output()
}

/// The parts read in order, each as its generator, its lines and the records it covers.
fn parts_of(events: &[Event]) -> Vec<(String, String, u64)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Part { part, covers, .. } => {
                Some((part.source.clone(), part.lines.clone(), *covers))
            }
            _ => None,
        })
        .collect()
}

/// FORMAT.md, "Annotations": an annotation record's payload is the generator's name after its
/// u16 length, then one line and its newline; a part's frame decodes to the same name, then its
/// lines. A part covers its generator's records since its previous part, byte for byte, and a
/// reader in order refuses one that differs by a byte.
#[test]
fn records_and_parts_carry_raw_lines_and_a_part_covers_its_records_byte_for_byte() {
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.annotation("gen", mark("a/1", 1, None)).unwrap();
    w.annotation("gen", mark("a/2", 2, Some(5))).unwrap();
    let file = finished(w, 10);
    let lines = [
        r#"{"id":"a/1","start":1}"#,
        r#"{"end":5,"id":"a/2","start":2}"#,
    ];
    let payloads: Vec<Vec<u8>> = records(&file)
        .into_iter()
        .filter(|(_, k, _)| *k == Kind::Annotation as u8)
        .map(|(.., p)| p)
        .collect();
    let want: Vec<Vec<u8>> = lines
        .iter()
        .map(|l| [&[3, 0][..], b"gen", l.as_bytes(), b"\n"].concat())
        .collect();
    assert_eq!(payloads, want);
    let (events, ending) = read_in_order(&file).unwrap();
    assert_eq!(ending, Ending::Complete);
    let all = format!("{}\n{}\n", lines[0], lines[1]);
    assert_eq!(parts_of(&events), [("gen".into(), all.clone(), 2)]);
    let row = events
        .iter()
        .find_map(|e| match e {
            Event::Part { row, .. } => Some(*row),
            _ => None,
        })
        .unwrap();
    assert_eq!((row.start, row.end), (1, 5));
    // A part after the same records that differs by one byte: refused where it lies.
    let mut head = opening();
    for l in lines {
        head.extend(annotation_record("gen", l));
    }
    let at = head.len();
    let mut differs = head.clone();
    differs.extend(part_record("gen", &all.replace("a/2", "a/3")));
    assert_eq!(
        read_in_order(&differs).unwrap_err().to_string(),
        format!(
            "the annotation part at {at}: it differs from the 2 annotation records of generator \
             \"gen\" it covers"
        )
    );
    // One line short of them: refused too; all of them: read.
    let mut short = head.clone();
    short.extend(part_record("gen", &format!("{}\n", lines[0])));
    assert!(read_in_order(&short).is_err());
    let mut whole = head;
    whole.extend(part_record("gen", &all));
    let (events, _) = read_in_order(&whole).unwrap();
    assert_eq!(parts_of(&events), [("gen".into(), all, 2)]);
}

/// FORMAT.md, "Annotations": a part with no record of its generator between it and that
/// generator's previous part holds lines of its own. Another generator's records don't count.
#[test]
fn a_part_with_no_record_of_its_generator_before_it_stands_alone() {
    let mut f = opening();
    f.extend(annotation_record("other", r#"{"id":"o/1","start":1}"#));
    f.extend(part_record("gen", "{\"id\":\"s/1\",\"start\":3}\n"));
    // A record of gen, then a part that covers it, then another that stands alone.
    f.extend(annotation_record("gen", r#"{"id":"r/1","start":4}"#));
    f.extend(part_record("gen", "{\"id\":\"r/1\",\"start\":4}\n"));
    f.extend(part_record("gen", "{\"id\":\"s/2\",\"start\":9}\n"));
    let (events, ending) = read_in_order(&f).unwrap();
    assert_eq!(ending, Ending::CutOff { at: f.len() as u64 });
    let covers: Vec<(String, u64)> = parts_of(&events)
        .into_iter()
        .map(|(s, _, c)| (s, c))
        .collect();
    assert_eq!(
        covers,
        [("gen".into(), 0), ("gen".into(), 1), ("gen".into(), 0)]
    );
}

/// FORMAT.md, "Annotations": a reader refuses a record whose line holds a newline before its
/// end, has none at its end, or is not a JSON object, and a part whose lines aren't whole lines
/// of JSON objects with a start, since its row couldn't state their span.
#[test]
fn a_record_or_part_that_is_not_lines_of_json_objects_is_refused() {
    let refused = |record: Vec<u8>| {
        let mut f = opening();
        f.extend(record);
        read_in_order(&f).unwrap_err().to_string()
    };
    let raw = |payload: &[u8]| {
        let mut out = Vec::new();
        record::write(&mut out, Kind::Annotation, &[payload]);
        out
    };
    let named = |rest: &str| [&[3, 0][..], b"gen", rest.as_bytes()].concat();
    let at = opening().len();
    for (payload, want) in [
        (
            named("{\"start\":1}"),
            "its line does not end with a newline",
        ),
        (
            named("{\"start\":1}\n{\"start\":2}\n"),
            "its line holds a newline before its end",
        ),
        (named("[1]\n"), "its line is not a JSON object"),
        (
            named("{\"id\":\"a\",\"start\":1,\"id\":\"b\"}\n"),
            "its line names key \"id\" twice",
        ),
        (
            named("{\"start\":1,\"measure\":{\"what\":\"a\",\"what\":\"b\"}}\n"),
            "its line names key \"what\" twice",
        ),
        (vec![9, 0, b'g'], "too short for its generator's name"),
    ] {
        assert_eq!(
            refused(raw(&payload)),
            format!("an annotation record: {want}")
        );
    }
    for (lines, want) in [
        ("", "it holds no line"),
        ("{\"start\":1}", "its last line does not end with a newline"),
        ("{\"id\":\"x\"}\n", "line 1: no start"),
        ("{\"start\":\"1 parsec\"}\n", "line 1: start: "),
        ("not json\n", "line 1: not a JSON object"),
        (
            "{\"start\":1}\n{\"id\":\"a\",\"start\":2,\"id\":\"b\"}\n",
            "line 2: names key \"id\" twice",
        ),
    ] {
        let err = refused(part_record("gen", lines));
        assert!(
            err.starts_with(&format!("the annotation part at {at}: {want}")),
            "{lines:?}: {err}"
        );
    }
}

/// `line_span` as it read a line before it stopped building `serde_json::Value`s: the line as a
/// map of them, each of `start` and `end` taken from the map as a `Time`.
fn span_from_values(line: &str, timebase: Ratio) -> Result<(i64, i64), String> {
    use escapement_tickwave_format::times::{Time, ticks};
    let fields: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(line).map_err(|_| "not a JSON object")?;
    let time = |key: &str| -> Result<Option<i64>, String> {
        fields
            .get(key)
            .map(|v| {
                let t: Time =
                    serde_json::from_value(v.clone()).map_err(|e| format!("{key}: {e}"))?;
                ticks(&t, timebase).map_err(|e| format!("{key}: {e}"))
            })
            .transpose()
    };
    let start = time("start")?.ok_or("no start")?;
    let end = time("end")?.unwrap_or(start);
    Ok((start, end))
}

/// A part's row states its lines' span, so a reader reads each line's start and end as a JSON
/// object of any values would hold them: every value parsed whole, and why a time is refused in
/// `Time`'s words. A key named twice in any object of the line is refused, where such an object
/// would take the last.
#[test]
fn a_lines_span_reads_as_a_map_of_json_values_would() {
    let deep = format!(
        "{{\"start\":1,\"x\":{}1{}}}",
        "[".repeat(200),
        "]".repeat(200)
    );
    let lines = [
        r#"{"start":1}"#,
        r#"{"start":-5,"end":7}"#,
        r#"{"start":"1.5 us","end":"2 us"}"#,
        r#"{"end":5}"#,
        r#"{"start":1.5}"#,
        r#"{"start":true}"#,
        r#"{"start":null}"#,
        r#"{"start":[1,2]}"#,
        r#"{"start":{"a":1}}"#,
        r#"{"start":"1 parsec"}"#,
        r#"{"start":18446744073709551615}"#,
        r#"{"start":1e3}"#,
        r#"{"start":1,"end":null}"#,
        r#"{"start":4}"#,
        r#"{"start":1,"x":1e999}"#,
        r#"{"start":1,"x":[1e999]}"#,
        r#"{"start":[1e999]}"#,
        r#"{"start":1,"x":"\ud800"}"#,
        r#"{"start":1,"x":{"y":[true,null,"z",-1,2.5]}}"#,
        r#"{"start":1} x"#,
        r#"[1]"#,
        r#""start""#,
        r#"{"start":1,}"#,
        "",
        &deep,
    ];
    let timebase = Ratio::integer(1_000_000_000).unwrap();
    let mut why = std::collections::BTreeSet::new();
    let mut read = 0;
    for line in lines {
        let want = span_from_values(line, timebase);
        match &want {
            Ok(_) => read += 1,
            Err(e) => {
                why.insert(e.clone());
            }
        }
        assert_eq!(
            escapement_tickwave_format::parts::line_span(line, timebase),
            want,
            "{line}"
        );
    }
    // Not vacuous: five lines read, and the refusals worded eleven ways.
    assert_eq!((read, why.len()), (5, 11), "{why:#?}");
    for (line, key) in [
        (r#"{"start":"bad","start":3}"#, "start"),
        (r#"{"start":3,"start":"bad"}"#, "start"),
        (r#"{"start":1,"x":{"y":1,"y":2}}"#, "y"),
        (r#"{"start":1,"x":[{"y":1},{"y":2,"y":3}]}"#, "y"),
    ] {
        assert_eq!(
            escapement_tickwave_format::parts::line_span(line, timebase),
            Err(format!("names key {key:?} twice")),
            "{line}"
        );
    }
}

/// FORMAT.md, "tickwave's writer": a generator's part goes out once its lines since its last part
/// reach 64 KiB, right after the record whose line takes them there; before a line that would
/// take them past 4 MiB, the lines held go in a part first. A line alone past 4 MiB is refused.
#[test]
fn a_writer_writes_a_part_at_64_kib_and_before_a_line_that_would_pass_4_mib() {
    let label = "x".repeat(1000);
    let mut w = Writer::new(declaration(), options()).unwrap();
    for i in 0..150 {
        let mut m = mark(&format!("m/{i}"), i, None);
        m.insert("label".into(), label.clone().into());
        w.annotation("gen", m).unwrap();
    }
    let file = finished(w, 1000);
    let (events, _) = read_in_order(&file).unwrap();
    // The lines since the last part, and the length of the last one, as the records come.
    let (mut since, mut last) = (0usize, 0usize);
    let mut sizes = Vec::new();
    for e in &events {
        match e {
            Event::Annotation(a) => {
                last = a.line.len() + 1;
                since += last;
            }
            Event::Part { part, covers, .. } => {
                assert_eq!(part.lines.len(), since);
                sizes.push((since, since - last, *covers));
                since = 0;
            }
            _ => {}
        }
    }
    let (rest, full) = sizes.split_last().unwrap();
    assert_eq!(full.len(), 2, "{sizes:?}");
    for &(lines, before_last, covers) in full {
        // Written at the record that took the lines to 64 KiB, not later.
        assert!(lines >= 64 << 10 && before_last < 64 << 10, "{sizes:?}");
        assert_eq!(covers as usize, lines.div_ceil(last));
    }
    assert!(rest.0 < 64 << 10);

    // 10 KiB held, then a line that would take them past 4 MiB: they go first.
    let mut w = Writer::new(declaration(), options()).unwrap();
    for i in 0..10 {
        let mut m = mark(&format!("s/{i}"), i, None);
        m.insert("label".into(), label.clone().into());
        w.annotation("gen", m).unwrap();
    }
    let mut big = mark("big", 20, None);
    big.insert("label".into(), "y".repeat((4 << 20) - 5000).into());
    w.annotation("gen", big).unwrap();
    let file = finished(w, 1000);
    let (events, _) = read_in_order(&file).unwrap();
    let order: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            Event::Annotation(a) => Some(format!("r{}", a.line.len() > 1 << 20)),
            Event::Part { covers, row, .. } => {
                assert!(row.decoded_length as usize <= 4 << 20);
                Some(format!("p{covers}"))
            }
            _ => None,
        })
        .collect();
    let mut want = vec!["rfalse".to_owned(); 10];
    want.extend(["p10", "rtrue", "p1"].map(String::from));
    assert_eq!(order, want);
    // A line no part can hold.
    let mut w = Writer::new(declaration(), options()).unwrap();
    let mut huge = mark("huge", 1, None);
    huge.insert("label".into(), "z".repeat(4 << 20).into());
    match w.annotation("gen", huge) {
        Err(WriteError::Annotation(e)) => {
            assert!(e.contains("past what a part holds, 4 MiB"), "{e}")
        }
        other => panic!("{other:?}"),
    }
}

/// FORMAT.md, "Annotations" and "Writing": the generators go in the order of their first
/// annotations, in the finish's parts and in the hand-written file's `include`; each generator
/// file in the bundle is its first line alone; and the finish writes the last parts, then every
/// table's last pages, the parts table's included, then the bundle and the footer.
#[test]
fn generators_go_in_the_order_of_their_first_annotation_and_the_finish_writes_parts_pages_bundle() {
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.edge(0, 3).unwrap();
    w.annotation("zeta", mark("z/1", 5, None)).unwrap();
    w.annotation("alpha", mark("a/1", 1, None)).unwrap();
    w.annotation("zeta", mark("z/2", 7, None)).unwrap();
    let file = finished(w, 100);
    let footer = Footer::parse_file(&file).unwrap();
    assert_eq!(footer.header.annotation_parts, Some(2));
    let files = footer.read_bundle(from_memory(&file)).unwrap().unwrap();
    let id = footer.header.content_id;
    assert_eq!(
        files["rec.zeta.jsonl"],
        generator_file("zeta", None, id, &[])
    );
    assert_eq!(
        files["rec.alpha.jsonl"],
        generator_file("alpha", None, id, &[])
    );
    assert!(
        files["rec.annotations.toml"]
            .contains(r#"include = ["rec.zeta.jsonl", "rec.alpha.jsonl"]"#),
        "{}",
        files["rec.annotations.toml"]
    );
    let sources: Vec<String> = footer
        .part_rows(from_memory(&file))
        .unwrap()
        .iter()
        .map(|r| {
            let frame = &file[r.offset as usize..][..r.length as usize];
            footer.decode_part(r, frame).unwrap().source
        })
        .collect();
    assert_eq!(sources, ["zeta", "alpha"]);
    // After the last block and overview block: the two parts, then pages alone, then the bundle.
    let kinds: Vec<u8> = records(&file).iter().map(|r| r.1).collect();
    let data_end = kinds
        .iter()
        .rposition(|&k| k == Kind::Block as u8 || k == Kind::OverviewBlock as u8)
        .unwrap();
    let tail = &kinds[data_end + 1..];
    let parts = tail.iter().filter(|&&k| k == Kind::Part as u8).count();
    assert_eq!(&tail[..2], [Kind::Part as u8; 2], "{kinds:?}");
    assert_eq!(parts, 2);
    assert_eq!(tail.last(), Some(&(Kind::Bundle as u8)));
    assert!(
        tail[2..tail.len() - 1]
            .iter()
            .all(|&k| k == Kind::IndexPage as u8),
        "{kinds:?}"
    );
    // The parts table's last page among them.
    let parts_page = records(&file)
        .into_iter()
        .any(|(_, k, p)| k == Kind::IndexPage as u8 && p[4] == Table::Parts.code());
    assert!(parts_page);
}

/// FORMAT.md, "Index": the parts table's rows go in the order written, and a directory entry above
/// them states the earliest `start` and the latest `end` beneath it, so a reader fetches only the
/// pages whose span meets a view's. 70,000 rows: 18 pages under one directory page.
#[test]
fn the_parts_tables_entries_state_spans_so_a_reader_fetches_only_what_a_view_meets() {
    let n = 70_000u64;
    let mut file = vec![0u8; 12 + n as usize];
    let mut t: TableWriter<PartRow> = TableWriter::new(0, Table::Parts, false);
    let place = |file: &mut Vec<u8>, t: &mut TableWriter<PartRow>, mut due: Option<Due>| {
        while let Some(d) = due {
            let at = file.len() as u64;
            file.extend_from_slice(&d.record);
            due = t.placed(&d, at);
        }
    };
    for i in 0..n {
        // Each row spans its 10 ticks; row 3, early, reaches every tick.
        let start = i as i64 * 10;
        let end = if i == 3 { n as i64 * 10 } else { start + 5 };
        let due = t.push(PartRow {
            offset: 12 + i,
            length: 0,
            decoded_length: 0,
            start,
            end,
        });
        place(&mut file, &mut t, due);
    }
    loop {
        let due = t.finish_next(0);
        if due.is_none() {
            break;
        }
        place(&mut file, &mut t, due);
    }
    let (depth, top) = t.top().unwrap();
    assert_eq!((depth, top.len()), (1, 1));
    assert_eq!((top[0].first, top[0].number), (0, n * 10));
    let index = TableIndex::finished(0, Table::Parts, n, depth, top);
    let fetches = std::cell::Cell::new(0);
    let mut fetch = |at: u64, len: u32| -> Result<Vec<u8>, DecodeError> {
        fetches.set(fetches.get() + 1);
        Ok(file[at as usize..][..len as usize].to_vec())
    };
    let mut met = Vec::new();
    let view = 500_000..500_010;
    index
        .visit_in(view.clone(), &mut fetch, |at, page| {
            let Page::Parts(rows) = page else {
                panic!("a page of rows")
            };
            met.push(at.j);
            let mut starts: Vec<i64> = rows.iter().map(|r| r.start).collect();
            starts.retain(|&s| rows.iter().any(|r| r.start == s && r.meets(&view)));
            assert!(!starts.is_empty());
            Ok(ControlFlow::Continue(()))
        })
        .unwrap();
    // The directory page, page 0 for row 3, and page 12 for row 50,000.
    assert_eq!(met, [0, 12]);
    assert_eq!(fetches.get(), 3);
    // A page whose rows' span differs from the entry over it: refused when read.
    let directory = index.pages(0, &mut fetch).unwrap();
    let e = directory[12];
    let mut page = file[e.offset as usize..][..e.length as usize].to_vec();
    let record::Next::Record { payload, .. } = record::next(&page, page.len()) else {
        panic!()
    };
    let mut payload = payload.to_vec();
    // Its first row's start, 491,520, one tick later: the page then starts its span later.
    let row0 = escapement_tickwave_format::index::PAGE_HEAD + 16;
    let start = i64::from_le_bytes(payload[row0..row0 + 8].try_into().unwrap());
    payload[row0..row0 + 8].copy_from_slice(&(start + 1).to_le_bytes());
    page.clear();
    record::write(&mut page, Kind::IndexPage, &[&payload]);
    let err = index.parse_page(0, 12, &e, &page).unwrap_err().to_string();
    assert_eq!(
        err,
        "channel 0: page 12 at depth 0 of its annotation parts: its span differs from the one its \
         directory states"
    );
}

/// The format crate's read API for parts: the rows whose span meets a view, one part decoded and
/// held to its row, and the sidecar joined from the bundle and the parts, held to the length and
/// to an id's first annotation (FORMAT.md, "Annotations").
#[test]
fn the_read_api_finds_the_parts_a_span_meets_decodes_one_and_joins_the_sidecar() {
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.annotation("a", mark("a/1", 1, None)).unwrap();
    w.annotation("a", mark("a/2", 3, Some(5))).unwrap();
    w.annotation("b", mark("b/1", 25, Some(40))).unwrap();
    w.annotation("b", mark("b/2", 35, None)).unwrap();
    w.annotation("b", mark("b/1", 26, None)).unwrap();
    let file = finished(w, 30);
    let footer = Footer::parse_file(&file).unwrap();
    let meeting = |span: std::ops::Range<i64>| -> Vec<(i64, i64)> {
        footer
            .parts_meeting(span, from_memory(&file))
            .unwrap()
            .iter()
            .map(|r| (r.start, r.end))
            .collect()
    };
    assert_eq!(meeting(0..10), [(1, 5)]);
    assert_eq!(meeting(20..30), [(25, 40)]);
    // A part's start lies before the view's end, its end at or after the view's start.
    assert_eq!(meeting(5..26), [(1, 5), (25, 40)]);
    assert_eq!(meeting(6..25), []);
    let rows = footer.part_rows(from_memory(&file)).unwrap();
    let frame = &file[rows[0].offset as usize..][..rows[0].length as usize];
    let part = footer.decode_part(&rows[0], frame).unwrap();
    assert_eq!(
        part.lines().collect::<Vec<_>>(),
        [
            r#"{"id":"a/1","start":1}"#,
            r#"{"end":5,"id":"a/2","start":3}"#
        ]
    );
    let wrong = PartRow {
        start: 2,
        ..rows[0]
    };
    assert_eq!(
        footer.decode_part(&wrong, frame).unwrap_err().to_string(),
        format!(
            "the annotation part at {} covers ticks 1 to 5, its row says 2 to 5",
            rows[0].offset
        )
    );
    let sidecar = footer.read_sidecar(from_memory(&file)).unwrap().unwrap();
    let id = footer.header.content_id;
    assert_eq!(
        sidecar.files["rec.b.jsonl"],
        generator_file(
            "b",
            None,
            id,
            &[r#"{"end":30,"id":"b/1","start":25}"#.into()]
        )
    );
    assert_eq!(
        sidecar.files["rec.a.jsonl"],
        generator_file(
            "a",
            None,
            id,
            &[
                r#"{"id":"a/1","start":1}"#.into(),
                r#"{"end":5,"id":"a/2","start":3}"#.into()
            ]
        )
    );
    assert_eq!(
        sidecar.left_out,
        [
            "b/b/2: starts at or past the length, 30",
            "b/b/1: an id its generator has given already"
        ]
    );
}

/// FORMAT.md, "Annotations": a recording with annotation parts has a bundle record, and each
/// part's generator has a file in it; a reader refuses one that breaks either, in order or at
/// random.
#[test]
fn parts_without_a_bundle_or_a_generator_file_are_refused() {
    let w = Writer::new(declaration(), options()).unwrap();
    let plain = finished(w, 10);
    let mut header = Footer::parse_file(&plain).unwrap().header;
    header.annotation_parts = Some(1);
    let file = common::with_header(&plain, &header);
    assert_eq!(
        Footer::parse_file(&file).unwrap_err().to_string(),
        "the header states annotation parts, and the file holds no annotation bundle"
    );
    // In order: a part of gen, then a bundle without gen's file.
    let mut f = opening();
    f.extend(part_record("gen", "{\"start\":1}\n"));
    let files = BTreeMap::from([("rec.annotations.toml".to_owned(), String::new())]);
    let payload = escapement_tickwave_format::bundle::bundle_payload(&files, 3).unwrap();
    record::write(&mut f, Kind::Bundle, &[&payload]);
    assert_eq!(
        read_in_order(&f).unwrap_err().to_string(),
        "generator \"gen\" has annotation parts and no file in the annotation bundle"
    );
    // At random: a bundled file's bundle swapped for one without gen's file.
    let w = Writer::new(declaration(), options()).unwrap();
    let plain = finished(w, 10);
    let id = Footer::parse_file(&plain).unwrap().header.content_id;
    let toml = format!(
        "format = \"tickwave-annotations/1\"\nrecording_id = \"{id}\"\ninclude = [\"rec.gen.jsonl\"]\n"
    );
    let sidecar = BTreeMap::from([
        ("rec.annotations.toml".to_owned(), toml.clone()),
        (
            "rec.gen.jsonl".to_owned(),
            generator_file("gen", None, id, &[r#"{"id":"g/1","start":1}"#.into()]),
        ),
    ]);
    let footer = Footer::parse_file(&plain).unwrap();
    let bundled = escapement_tickwave_format::bundle::bundle(&plain, &footer, sidecar, 3).unwrap();
    let footer = Footer::parse_file(&bundled).unwrap();
    assert!(footer.read_sidecar(from_memory(&bundled)).is_ok());
    let (at, _) = footer.bundle.unwrap();
    let mut swapped = bundled[..at as usize].to_vec();
    let payload = escapement_tickwave_format::bundle::bundle_payload(
        &BTreeMap::from([("rec.annotations.toml".to_owned(), toml)]),
        3,
    )
    .unwrap();
    record::write(&mut swapped, Kind::Bundle, &[&payload]);
    let len = (swapped.len() as u64 - at) as u32;
    let f_at = footer.trailer.footer_offset as usize;
    let record::Next::Record { payload, .. } = record::next(&bundled[f_at..], bundled.len()) else {
        panic!()
    };
    let mut payload = payload.to_vec();
    let top = 8 + u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
    payload[top + 8..top + 12].copy_from_slice(&len.to_le_bytes());
    record::write_footer(&mut swapped, 0, &payload);
    let footer = Footer::parse_file(&swapped).unwrap();
    assert_eq!(
        footer
            .read_sidecar(from_memory(&swapped))
            .unwrap_err()
            .to_string(),
        "an annotation part of generator \"gen\", which has no file in the bundle"
    );
}

/// FORMAT.md, "Reading", "In order": the footer's parts table and the header's count are held to
/// the parts read before them.
#[test]
fn the_footers_parts_are_held_to_the_parts_read_in_order() {
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.annotation("gen", mark("g/1", 1, None)).unwrap();
    let file = finished(w, 10);
    read_in_order(&file).unwrap();
    let mut header = Footer::parse_file(&file).unwrap().header;
    header.annotation_parts = Some(2);
    // Two rows still make one page, so the footer parses at random.
    let file = common::with_header(&file, &header);
    Footer::parse_file(&file).unwrap();
    assert_eq!(
        read_in_order(&file).unwrap_err().to_string(),
        "the footer's annotation parts differ from the ones read"
    );
}

/// FORMAT.md, "Limits": a recording has at most 256 generators. The writer refuses the 257th's
/// first annotation, and a reader in order the 257th's first record.
#[test]
fn a_recording_has_at_most_256_generators() {
    let mut w = Writer::new(declaration(), options()).unwrap();
    for g in 0..256 {
        w.annotation(&format!("g{g}"), mark("m", 1, None)).unwrap();
    }
    w.annotation("g0", mark("m2", 1, None)).unwrap();
    match w.annotation("g256", mark("m", 1, None)) {
        Err(WriteError::Annotation(e)) => {
            assert!(e.ends_with("a recording has at most 256 generators"), "{e}")
        }
        other => panic!("{other:?}"),
    }
    let mut f = opening();
    for g in 0..257 {
        f.extend(annotation_record(
            &format!("g{g}"),
            r#"{"id":"m","start":1}"#,
        ));
    }
    assert_eq!(
        read_in_order(&f).unwrap_err().to_string(),
        "generator \"g256\": a recording has at most 256 generators"
    );
}

/// FORMAT.md, "Annotations": a part covers each annotation record, and a reader with random
/// access finds annotations through the parts alone. A reader in order refuses a footer after
/// a record no part covers: here one inserted before the bundle, the footer's place for the
/// bundle and the trailer moved with it.
#[test]
fn a_record_no_part_covers_is_refused_at_the_footer() {
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.annotation("a", mark("a/1", 1, None)).unwrap();
    let file = finished(w, 10);
    read_in_order(&file).unwrap();
    let (bundle_at, _, _) = records(&file)
        .into_iter()
        .find(|(_, kind, _)| *kind == Kind::Bundle as u8)
        .unwrap();
    let at = bundle_at as usize;
    let extra = annotation_record("a", r#"{"id":"a/2","start":2}"#);
    let footer = Footer::parse_file(&file).unwrap();
    let footer_at = footer.trailer.footer_offset as usize;
    let mut payload = file[footer_at + 5..file.len() - 24 - 8].to_vec();
    let header_len = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
    let top = 8 + header_len;
    let moved = u64::from_le_bytes(payload[top..top + 8].try_into().unwrap()) + extra.len() as u64;
    payload[top..top + 8].copy_from_slice(&moved.to_le_bytes());
    let mut uncovered = file[..at].to_vec();
    uncovered.extend(&extra);
    uncovered.extend(&file[at..footer_at]);
    record::write_footer(&mut uncovered, 0, &payload);
    // At random the file reads, without the record.
    Footer::parse_file(&uncovered).unwrap();
    assert_eq!(
        read_in_order(&uncovered).unwrap_err().to_string(),
        "generator \"a\": no part covers its annotation records after its last part"
    );
}

/// FORMAT.md, "Annotations": the part that covers an annotation record states its span in a row,
/// so the in-order reader refuses a record a row couldn't hold: no start, a time that is no tick
/// in 0 to 2^53 as parts convert it, or an end before its start. Repair then meets none before
/// its cut, and one after its cut stops repair as any record the reader refuses does.
#[test]
fn an_annotation_record_a_parts_row_cant_hold_is_refused() {
    let read = |line: &str| {
        let mut f = opening();
        f.extend(annotation_record("gen", line));
        read_in_order(&f).map(|(_, ending)| ending)
    };
    assert!(matches!(
        read(r#"{"id":"x","start":"0.5 us","end":3}"#),
        Ok(Ending::CutOff { .. })
    ));
    for (line, want) in [
        (r#"{"id":"x"}"#, "no start"),
        (r#"{"id":"x","start":"1 parsec"}"#, "start: "),
        (
            r#"{"id":"x","start":-5}"#,
            "ticks -5 to -5 lie outside 0 to 2^53, or end before they start",
        ),
        (
            r#"{"id":"x","start":9007199254740992}"#,
            "ticks 9007199254740992 to 9007199254740992 lie outside 0 to 2^53",
        ),
        (
            r#"{"id":"x","start":5,"end":3}"#,
            "ticks 5 to 3 lie outside 0 to 2^53, or end before they start",
        ),
    ] {
        let err = read(line).unwrap_err().to_string();
        assert!(
            err.starts_with(&format!(
                "an annotation record of generator \"gen\", which a part can't hold: {want}"
            )),
            "{line}: {err}"
        );
    }
    // Before and after a cut at a known-through record.
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.edge(0, 5).unwrap();
    w.flush(50).unwrap();
    let flushed = w.take_output();
    let bad = annotation_record("gen", r#"{"id":"x"}"#);
    let before = [&opening()[..], &bad, &flushed[opening().len()..]].concat();
    let after = [&flushed[..], &bad].concat();
    for (what, cut) in [("before", before), ("after", after)] {
        let mut r = Repairer::new(3);
        let err = r.feed(&cut).unwrap_err().to_string();
        assert!(
            err.contains(
                "an annotation record of generator \"gen\", which a part can't hold: no start"
            ),
            "{what}: {err}"
        );
    }
}

/// FORMAT.md, "Annotations": the bundle holds a generator's file's first line alone, its lines
/// being in its parts, and that line names each key once, as the header does. A reader refuses a
/// bundle otherwise, in order and at random: here the bundle record replaced, the footer's place
/// for it and the trailer moved with it.
#[test]
fn a_bundled_generator_file_is_its_first_line_alone_naming_each_key_once() {
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.annotation("a", mark("a/1", 1, None)).unwrap();
    let file = finished(w, 10);
    let footer = Footer::parse_file(&file).unwrap();
    let files = footer.read_bundle(from_memory(&file)).unwrap().unwrap();
    let head = files["rec.a.jsonl"].clone();
    assert!(head.ends_with('\n') && head.lines().count() == 1, "{head}");
    let (bundle_at, _) = footer.bundle.unwrap();
    let with = |name: &str, text: String| {
        let mut files = files.clone();
        files.insert(name.into(), text);
        let raw = serde_json::to_vec(&files).unwrap();
        let mut bundle = Vec::new();
        record::write(
            &mut bundle,
            Kind::Bundle,
            &[
                &(raw.len() as u32).to_le_bytes(),
                &compress(&raw, 3).unwrap(),
            ],
        );
        let footer_at = footer.trailer.footer_offset as usize;
        let mut payload = file[footer_at + 5..file.len() - 24 - 8].to_vec();
        let header_len = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
        let top = 8 + header_len;
        payload[top + 8..top + 12].copy_from_slice(&(bundle.len() as u32).to_le_bytes());
        let mut out = file[..bundle_at as usize].to_vec();
        out.extend(&bundle);
        record::write_footer(&mut out, 0, &payload);
        out
    };
    let refused = |bytes: &[u8]| {
        let random = Footer::parse_file(bytes)
            .unwrap()
            .read_sidecar(from_memory(bytes))
            .unwrap_err()
            .to_string();
        let in_order = read_in_order(bytes).unwrap_err().to_string();
        assert_eq!(random, in_order);
        in_order
    };
    let same = with("rec.a.jsonl", head.clone());
    read_in_order(&same).unwrap();
    Footer::parse_file(&same)
        .unwrap()
        .read_sidecar(from_memory(&same))
        .unwrap();
    assert_eq!(
        refused(&with(
            "rec.a.jsonl",
            format!("{head}{{\"id\":\"a/2\",\"start\":2}}\n")
        )),
        "the annotation bundle's \"rec.a.jsonl\" holds more than its generator's first line"
    );
    let twice = head.replacen('}', ",\"source\":\"a\"}", 1);
    assert_eq!(
        refused(&with("rec.a.jsonl", twice.clone())),
        "the annotation bundle's \"rec.a.jsonl\": its first line names key \"source\" twice"
    );
    // The writer refuses to bundle such a first line.
    let mut given = files.clone();
    given.insert("rec.a.jsonl".into(), twice);
    let w = Writer::new(declaration(), options()).unwrap();
    let finish = Finish {
        bundle: Some(Bundle::Files(given)),
        ..Finish::default()
    };
    match w.finish(10, &[10], finish) {
        Err(WriteError::Bundle(e)) => assert_eq!(
            e,
            "\"rec.a.jsonl\": its first line names key \"source\" twice"
        ),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("bundled"),
    }
}

fn repaired(cut: &[u8]) -> Vec<u8> {
    let mut r = Repairer::new(3);
    r.bundle(Bundle::Generated { stem: "rec".into() });
    r.feed(cut).unwrap();
    let repaired = r.finish().unwrap();
    [&cut[..repaired.keep as usize], &repaired.tail].concat()
}

/// FORMAT.md, "Repairing a cut-off file": past the cut, annotation records are written again,
/// a part that stands alone too, and a part that covers records goes, its lines coming back
/// from the records, so no annotation comes out twice. A part written again after the records
/// its generator holds since its last part gets a part of those first, so it still covers none.
#[test]
fn repair_keeps_records_and_standalone_parts_past_the_cut_and_drops_covering_parts() {
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.edge(0, 5).unwrap();
    w.flush(10).unwrap();
    let mut cut = w.take_output();
    let (a1, a2, a3) = (
        r#"{"id":"a/1","start":20}"#,
        r#"{"id":"a/2","start":21}"#,
        r#"{"id":"a/3","start":22}"#,
    );
    let (b1, s1) = (r#"{"id":"b/1","start":30}"#, r#"{"id":"s/1","start":23}"#);
    cut.extend(annotation_record("a", a1));
    cut.extend(annotation_record("a", a2));
    cut.extend(part_record("a", &format!("{a1}\n{a2}\n")));
    cut.extend(part_record("b", &format!("{b1}\n")));
    cut.extend(annotation_record("a", a3));
    cut.extend(part_record("a", &format!("{a3}\n")));
    cut.extend(part_record("a", &format!("{s1}\n")));
    let whole = repaired(&cut);
    let (events, ending) = read_in_order(&whole).unwrap();
    assert_eq!(ending, Ending::Complete);
    let parts: Vec<(String, u64)> = parts_of(&events)
        .into_iter()
        .map(|(s, _, c)| (s, c))
        .collect();
    // b's part again; a's three records in a part before s/1's, which then stands alone.
    assert_eq!(parts, [("b".into(), 0), ("a".into(), 3), ("a".into(), 0)]);
    let footer = Footer::parse_file(&whole).unwrap();
    assert_eq!(footer.header.length, 31);
    let side = footer.read_sidecar(from_memory(&whole)).unwrap().unwrap();
    let id = footer.header.content_id;
    assert_eq!(
        side.files["rec.a.jsonl"],
        generator_file("a", None, id, &[a1.into(), a2.into(), a3.into(), s1.into()])
    );
    assert_eq!(
        side.files["rec.b.jsonl"],
        generator_file("b", None, id, &[b1.into()])
    );
    assert!(side.left_out.is_empty());
}

/// A cut-off file whose only content before its known-through record is a part standing alone:
/// its lines are the file's annotations, so it repairs to one past their latest start, with no
/// block.
#[test]
fn repair_keeps_a_file_whose_only_annotations_are_a_part_standing_alone() {
    let mut cut = opening();
    let s1 = r#"{"id":"s/1","start":23}"#;
    cut.extend(part_record("s", &format!("{s1}\n")));
    record::write(
        &mut cut,
        Kind::KnownThrough,
        &[&record::known_through_bytes(5)],
    );
    let whole = repaired(&cut);
    let footer = Footer::parse_file(&whole).unwrap();
    assert_eq!(footer.header.length, 24);
    let side = footer.read_sidecar(from_memory(&whole)).unwrap().unwrap();
    assert_eq!(
        side.files["rec.s.jsonl"],
        generator_file("s", None, footer.header.content_id, &[s1.into()])
    );
}

/// FORMAT.md, "Repairing a cut-off file": a gap record past the cut whose `from` maps before the
/// latest known-through tick is written again, and its channel ends at that `from`; one whose
/// `from` lies past the tick goes.
#[test]
fn repair_writes_a_late_gap_again_and_ends_its_channel_at_its_from() {
    for (from, end, gap_records) in [(50, 50, 1), (150, 100, 0)] {
        let mut w = Writer::new(declaration(), options()).unwrap();
        w.edge(0, 5).unwrap();
        w.flush(100).unwrap();
        w.gap(
            0,
            Gap {
                from,
                to: 300,
                level: true,
            },
        )
        .unwrap();
        let whole = repaired(&w.take_output());
        let footer = Footer::parse_file(&whole).unwrap();
        let ChannelKind::Digital { end: e, gaps, .. } = &footer.header.channels[0].data else {
            panic!()
        };
        assert_eq!((*e, gaps.len(), footer.header.length), (end, 0, 100));
        let (events, _) = read_in_order(&whole).unwrap();
        let gaps = events
            .iter()
            .filter(|e| matches!(e, Event::Gap { .. }))
            .count();
        assert_eq!(gaps, gap_records, "from {from}");
    }
}

/// FORMAT.md, "Repairing a cut-off file": a record only a finish writes goes wherever it lies, a
/// table's last page and the bundle, so a file cut anywhere inside its finish repairs to the file
/// it was: after the known-through record the finish writes at the length, or, in a file never
/// flushed, which has none, after its last block. The repair crashes the fuzzer found were files
/// cut there (testdata/repair/).
#[test]
fn a_file_cut_anywhere_inside_its_finish_repairs_to_the_file_it_was() {
    for flushed in [true, false] {
        let mut w = Writer::new(declaration(), options()).unwrap();
        for t in 1..40 {
            w.edge(0, t * 3).unwrap();
        }
        w.annotation("a", mark("a/1", 5, None)).unwrap();
        w.annotation("b", mark("b/1", 7, Some(9))).unwrap();
        if flushed {
            w.flush(150).unwrap();
        }
        let mut file = w.take_output();
        file.extend(
            w.finish(
                200,
                &[200],
                Finish {
                    bundle: stem(),
                    ..Finish::default()
                },
            )
            .unwrap()
            .tail,
        );
        let records = records(&file);
        // From the record after the known-through record at the length, or after the last
        // block.
        let from = records
            .iter()
            .rposition(|(_, k, p)| match flushed {
                true => *k == Kind::KnownThrough as u8 && p[..] == 200i64.to_le_bytes(),
                false => *k == Kind::Block as u8,
            })
            .unwrap();
        let from = records[from + 1].0 as usize;
        for cut in from..file.len() {
            assert_eq!(
                repaired(&file[..cut]),
                file,
                "flushed {flushed}, cut at {cut}"
            );
        }
    }
}

fn testdata(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/repair")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The repair crashes the fuzzer found (testdata/repair/README.md): one repairs now and reads
/// back, in pieces as the target fed them and whole; one is refused at its bundle, and one at its
/// known-through record.
#[test]
fn the_repair_crash_files_repair_and_read_back_or_are_refused() {
    for name in ["kept-last-pages.tickwave"] {
        let file = testdata(name);
        for piece in [7, file.len()] {
            let mut r = Repairer::new(1);
            r.bundle(Bundle::Generated { stem: "r".into() });
            for chunk in file.chunks(piece) {
                r.feed(chunk).unwrap();
            }
            let repaired = r.finish().unwrap();
            let out = [&file[..repaired.keep as usize], &repaired.tail].concat();
            let footer = Footer::parse_file(&out).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(footer.header.content_id, repaired.header.content_id);
            let (_, ending) = read_in_order(&out).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(ending, Ending::Complete);
        }
    }
    // Its bundle decodes to a JSON object, then the bytes of a footer: a reader in order refuses
    // the bundle, as one at random does.
    let file = testdata("bundle-before-the-cut.tickwave");
    let err = Repairer::new(1).feed(&file).unwrap_err().to_string();
    assert!(
        err.starts_with("the annotation bundle: trailing characters"),
        "{err}"
    );
    let file = testdata("known-through-before-a-block-end.tickwave");
    let mut r = Repairer::new(1);
    assert_eq!(
        r.feed(&file).unwrap_err().to_string(),
        "a known-through record at tick 0 comes after a block that ends at 480000"
    );
}

/// The writer crash the fuzzer found (never-panic artifacts, p6-replay/writer-crash-752bb017):
/// its calls, by the fuzz target that made them. Flushes to 16,781,314, then a gap on c1 that
/// ends long before, which the writer took, and its finish panicked: the overview had settled
/// past the gap's end. The writer refuses the gap, and the finish refuses what its arguments
/// break rather than panicking.
#[test]
fn the_writer_crash_calls_refuse_the_late_gap_and_the_finish_returns() {
    let d: Declaration = serde_json::from_str(
        r#"{"timebase":[13087,31],"channels":[
            {"name":"c0","source_name":"s0","source":0,"kind":"digital","initial":false,
             "offset":0,"offset_kind":"delay","kept":[1,4288348170]},
            {"name":"c1","source_name":"s1","source":0,"kind":"digital","initial":false,
             "kept":[64097,242444744]}],
            "sources":[{"format":"fuzz","files":[],"timebase":[13087,31],"origin":-1,"align":1127}],
            "overview_shift":12}"#,
    )
    .unwrap();
    let mut w = Writer::new(
        d,
        WriteOptions {
            level: 1,
            block_bytes: 35209,
            promise_with_items: true,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    w.flush(2049).unwrap();
    for _ in 0..3 {
        let _ = w.snapshot();
        w.take_output();
    }
    w.flush(16_779_265).unwrap();
    w.flush(16_781_313).unwrap();
    assert!(w.edge(0, 2049).is_err());
    w.flush(16_781_314).unwrap();
    let _ = w.snapshot();
    let gap = Gap {
        from: 64_199,
        to: 129_735,
        level: true,
    };
    assert!(
        matches!(w.gap(1, gap), Err(WriteError::GapLate { .. })),
        "the gap after the flushes past its end"
    );
    assert!(w.edge(0, 1).is_err());
    w.take_output();
    // The kept spans run past this length, which the target never held them to: refused, as
    // the target's finish was once it took no gap.
    match w.finish(16_781_314, &[3, 64_099], Finish::default()) {
        Err(WriteError::Header(e)) => assert!(e.starts_with("channel c0 is kept from"), "{e}"),
        other => panic!("{other:?}"),
    }
}

fn recut(file: &[u8], options: WriteOptions) -> Vec<u8> {
    let mut rc = Recutter::new(options);
    rc.bundle(Bundle::Generated { stem: "rec".into() });
    rc.feed(file).unwrap();
    let mut out = rc.take_output();
    out.extend(rc.finish().unwrap().tail);
    out
}

/// FORMAT.md, "Re-cutting": a re-cutter writes the annotations as parts that stand alone and no
/// records, the sidecar the same, and the gaps its input's header lists. A gap after the
/// channel's last edge, which the header may leave out, waits for the finish, and the channel
/// isn't promised past its `from` until then, so no block closes past its end first.
#[test]
fn a_recut_writes_standalone_parts_and_the_gaps_the_header_lists() {
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.edge(0, 5).unwrap();
    w.annotation("a", mark("a/1", 1, None)).unwrap();
    w.annotation("b", mark("b/1", 2, None)).unwrap();
    w.annotation("a", mark("a/2", 3, None)).unwrap();
    w.gap(
        0,
        Gap {
            from: 50,
            to: 60,
            level: true,
        },
    )
    .unwrap();
    let mut input = w.take_output();
    input.extend(
        w.finish(
            100,
            &[80],
            Finish {
                bundle: stem(),
                ..Finish::default()
            },
        )
        .unwrap()
        .tail,
    );
    let tiny = WriteOptions {
        block_bytes: 1,
        ..options()
    };
    let out = recut(&input, tiny);
    let (events, ending) = read_in_order(&out).unwrap();
    assert_eq!(ending, Ending::Complete);
    assert!(!events.iter().any(|e| matches!(e, Event::Annotation(_))));
    let parts: Vec<(String, u64)> = parts_of(&events)
        .into_iter()
        .map(|(s, _, c)| (s, c))
        .collect();
    assert_eq!(parts, [("a".into(), 0), ("b".into(), 0)]);
    let (before, after) = (
        Footer::parse_file(&input).unwrap(),
        Footer::parse_file(&out).unwrap(),
    );
    assert_eq!(after.header.content_id, before.header.content_id);
    let ChannelKind::Digital { gaps, end, .. } = &after.header.channels[0].data else {
        panic!()
    };
    assert_eq!((gaps.len(), *end), (1, 80));
    assert_eq!(
        after.read_sidecar(from_memory(&out)).unwrap(),
        before.read_sidecar(from_memory(&input)).unwrap()
    );
}

/// ANNOTATIONS.md, "Bundling": a bundle splits each generator file's lines into parts that stand
/// alone, the generators in the order the hand-written file includes them, and bundling again
/// replaces those parts. A file whose parts a writer wrote among its data is written again
/// through a re-cutter, the files given in place of its annotations. A re-cut of a bundled file
/// keeps its sidecar, a time with a unit included.
#[test]
fn bundling_splits_generator_files_into_parts_and_replaces_its_own() {
    let w = Writer::new(declaration(), options()).unwrap();
    let plain = finished(w, 100);
    let footer = Footer::parse_file(&plain).unwrap();
    let id = footer.header.content_id;
    let sidecar = |order: [&str; 2]| {
        BTreeMap::from([
            (
                "rec.annotations.toml".to_owned(),
                format!(
                    "format = \"tickwave-annotations/1\"\nrecording_id = \"{id}\"\ninclude = \
                     [\"rec.{}.jsonl\", \"rec.{}.jsonl\"]\n\n[[annotation]]\nid = \"h\"\n\
                     start = 1\n",
                    order[0], order[1]
                ),
            ),
            (
                "rec.x.jsonl".to_owned(),
                generator_file("x", None, id, &[r#"{"id":"x/1","start":"2 us"}"#.into()]),
            ),
            (
                "rec.y.jsonl".to_owned(),
                generator_file("y", None, id, &[r#"{"id":"y/1","start":7}"#.into()]),
            ),
        ])
    };
    let sources = |file: &[u8]| -> Vec<(String, i64)> {
        let footer = Footer::parse_file(file).unwrap();
        footer
            .part_rows(from_memory(file))
            .unwrap()
            .iter()
            .map(|r| {
                let frame = &file[r.offset as usize..][..r.length as usize];
                (footer.decode_part(r, frame).unwrap().source, r.start)
            })
            .collect()
    };
    let once = escapement_tickwave_format::bundle::bundle(&plain, &footer, sidecar(["y", "x"]), 3)
        .unwrap();
    assert_eq!(sources(&once), [("y".into(), 7), ("x".into(), 2)]);
    let head = Footer::parse_file(&once).unwrap();
    assert_eq!(
        head.read_sidecar(from_memory(&once))
            .unwrap()
            .unwrap()
            .files,
        sidecar(["y", "x"])
    );
    let again =
        escapement_tickwave_format::bundle::bundle(&once, &head, sidecar(["x", "y"]), 3).unwrap();
    assert_eq!(sources(&again), [("x".into(), 2), ("y".into(), 7)]);
    let head = Footer::parse_file(&again).unwrap();
    assert_eq!(head.header.annotation_parts, Some(2));
    // In the place of the parts before, not after them.
    let parts = records(&again)
        .iter()
        .filter(|r| r.1 == Kind::Part as u8)
        .count();
    assert_eq!(parts, 2);
    assert_eq!(read_in_order(&again).unwrap().1, Ending::Complete);
    // A re-cut of the bundled file: the same bundle, its lines in parts again.
    let recut_once = recut(&once, options());
    let head = Footer::parse_file(&recut_once).unwrap();
    assert_eq!(
        head.read_sidecar(from_memory(&recut_once))
            .unwrap()
            .unwrap()
            .files,
        sidecar(["y", "x"])
    );
    assert_eq!(
        head.read_bundle(from_memory(&recut_once)).unwrap(),
        Footer::parse_file(&once)
            .unwrap()
            .read_bundle(from_memory(&once))
            .unwrap()
    );
    // A recorder's file, the same content: its parts lie among its data, before its last pages.
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.annotation("x", mark("x/9", 9, None)).unwrap();
    let recorded = finished(w, 100);
    let bundled = escapement_tickwave_format::bundle::bundle(
        &recorded,
        &Footer::parse_file(&recorded).unwrap(),
        sidecar(["x", "y"]),
        3,
    )
    .unwrap();
    let (events, ending) = read_in_order(&bundled).unwrap();
    assert_eq!(ending, Ending::Complete);
    assert!(!events.iter().any(|e| matches!(e, Event::Annotation(_))));
    assert_eq!(sources(&bundled), [("x".into(), 2), ("y".into(), 7)]);
    let head = Footer::parse_file(&bundled).unwrap();
    assert_eq!(head.header.content_id, id);
    assert_eq!(
        head.read_sidecar(from_memory(&bundled))
            .unwrap()
            .unwrap()
            .files,
        sidecar(["x", "y"])
    );
}

/// FORMAT.md, "Annotations" and "Re-cutting": a re-cutter writes each annotation line to a part
/// that stands alone as it reads it, a part going out as its generator's lines reach the writer's
/// target, so it holds no more than a part's lines; and it carries its input's bundle as it is.
#[test]
fn a_recut_writes_parts_as_it_reads_the_lines_and_carries_the_bundle() {
    let label = "x".repeat(1000);
    let mut w = Writer::new(declaration(), options()).unwrap();
    for t in 0..200 {
        w.edge(0, t * 10 + 1).unwrap();
        let mut m = mark(&format!("m/{t}"), t * 10, None);
        m.insert("label".into(), label.clone().into());
        w.annotation("gen", m).unwrap();
        if t % 20 == 19 {
            w.flush(t * 10 + 5).unwrap();
        }
    }
    let input = finished(w, 2100);
    let out = recut(&input, options());
    let records = records(&out);
    let last_block = records
        .iter()
        .rposition(|r| r.1 == Kind::Block as u8)
        .unwrap();
    let first_part = records
        .iter()
        .position(|r| r.1 == Kind::Part as u8)
        .unwrap();
    // Parts among the blocks, written as the lines came, not all at the finish.
    assert!(first_part < last_block, "{first_part} {last_block}");
    let (events, _) = read_in_order(&out).unwrap();
    assert!(!events.iter().any(|e| matches!(e, Event::Annotation(_))));
    assert!(parts_of(&events).iter().all(|(.., covers)| *covers == 0));
    let (before, after) = (
        Footer::parse_file(&input).unwrap(),
        Footer::parse_file(&out).unwrap(),
    );
    assert_eq!(
        after.read_bundle(from_memory(&out)).unwrap(),
        before.read_bundle(from_memory(&input)).unwrap()
    );
    assert_eq!(
        after.read_sidecar(from_memory(&out)).unwrap(),
        before.read_sidecar(from_memory(&input)).unwrap()
    );
}

/// `tickwave bundle` of a re-cut recording that had markers: its parts lie before its last pages,
/// so the copy is written again with the files given, and their sidecar reads back.
#[test]
fn bundling_a_recut_recording_with_markers_reads_the_given_sidecar_back() {
    let mut w = Writer::new(declaration(), options()).unwrap();
    for t in 1..30 {
        w.edge(0, t * 7).unwrap();
        w.annotation("verge", mark(&format!("marker/{t}"), t * 7, None))
            .unwrap();
    }
    w.flush(100).unwrap();
    let recorded = finished(w, 300);
    let recut_file = recut(&recorded, options());
    let footer = Footer::parse_file(&recut_file).unwrap();
    let id = footer.header.content_id;
    let files = BTreeMap::from([
        (
            "rec.annotations.toml".to_owned(),
            format!(
                "format = \"tickwave-annotations/1\"\nrecording_id = \"{id}\"\ninclude = \
                 [\"rec.notes.jsonl\"]\n\n[[annotation]]\nid = \"here\"\nstart = 5\n"
            ),
        ),
        (
            "rec.notes.jsonl".to_owned(),
            generator_file(
                "notes",
                None,
                id,
                &[
                    r#"{"id":"n/1","start":10}"#.into(),
                    r#"{"id":"n/2","start":20,"end":40}"#.into(),
                ],
            ),
        ),
    ]);
    let bundled =
        escapement_tickwave_format::bundle::bundle(&recut_file, &footer, files.clone(), 3).unwrap();
    let head = Footer::parse_file(&bundled).unwrap();
    assert_eq!(head.header.content_id, id);
    assert_eq!(
        head.read_sidecar(from_memory(&bundled))
            .unwrap()
            .unwrap()
            .files,
        files
    );
    let (events, ending) = read_in_order(&bundled).unwrap();
    assert_eq!(ending, Ending::Complete);
    let sources: Vec<String> = parts_of(&events).into_iter().map(|(s, ..)| s).collect();
    assert_eq!(sources, ["notes"]);
}

mod common;

/// FORMAT.md, "Re-cutting" and "tickwave's writer": an edgeless channel's gaps with flushes
/// between them, as a lossy link's range flags come. A re-cutter hands a gap on once a later
/// input block of its channel shows the input kept it, so it holds no more than two blocks' gaps,
/// and the writer's waiting gaps close blocks as they reach its target. 250,000 gaps: held until
/// the finish, more than 4 MiB of them would wait in the writer.
#[test]
fn a_recut_hands_an_edgeless_channels_gaps_on_as_its_blocks_pass() {
    let n = 250_000i64;
    let mut w = Writer::new(declaration(), options()).unwrap();
    for k in 0..n {
        w.gap(
            0,
            Gap {
                from: 3 * k + 1,
                to: 3 * k + 2,
                level: false,
            },
        )
        .unwrap();
        if k % 100 == 99 {
            w.flush(3 * k + 3).unwrap();
        }
    }
    let length = 3 * n + 10;
    let mut input = w.take_output();
    input.extend(w.finish(length, &[length], Finish::default()).unwrap().tail);
    let out = recut(&input, options());
    let (before, after) = (
        Footer::parse_file(&input).unwrap(),
        Footer::parse_file(&out).unwrap(),
    );
    assert_eq!(after.header.content_id, before.header.content_id);
    let ChannelKind::Digital { gap_count, .. } = &after.header.channels[0].data else {
        panic!()
    };
    assert_eq!(*gap_count, Some(n as u64));
}
