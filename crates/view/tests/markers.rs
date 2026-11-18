//! A finished file's markers (FORMAT.md, "Annotations"): read a range at a time from the parts the
//! range meets, their span from the parts table with no part read, and whole on request; and the
//! markers that name a channel this version can't read left out (FORMAT.md, "Unavailable
//! channels").

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use escapement_tickwave_format::annotations::{self, Included};
use escapement_tickwave_format::decode::{DecodeError, Footer};
use escapement_tickwave_format::header::{
    AnalogCoding, ContentId, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, Header,
    IntCoding, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::parts::PartRow;
use escapement_tickwave_format::writer::{Bundle, Finish, WriteOptions, Writer, generator_file};
use escapement_tickwave_view::{
    Bytes, Cache, ChannelKind, Drawn, Marker, MarkerQuery, Query, Recording,
};

/// A file in memory that records each read.
struct Counted {
    bytes: Vec<u8>,
    reads: Mutex<Vec<Range<u64>>>,
}

impl Bytes for Counted {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.bytes.len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.reads
            .lock()
            .unwrap()
            .push(offset..offset + buf.len() as u64);
        let at = offset as usize;
        buf.copy_from_slice(&self.bytes[at..at + buf.len()]);
        Ok(())
    }
}

impl Counted {
    fn new(bytes: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            bytes,
            reads: Mutex::default(),
        })
    }

    /// The rows of `rows` whose part's frame was read since the last call.
    fn parts_read(&self, rows: &[PartRow]) -> Vec<usize> {
        let reads = std::mem::take(&mut *self.reads.lock().unwrap());
        (0..rows.len())
            .filter(|&k| reads.iter().any(|r| r.start == rows[k].offset))
            .collect()
    }
}

fn analog(name: &str) -> DeclaredChannel {
    DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period: Ratio::integer(10).unwrap(),
            coding: AnalogCoding::Int(IntCoding {
                scale: Exact::new(1, 1000),
                ..IntCoding::new(16, false)
            }),
            significant: None,
            phase: None,
        },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    }
}

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

fn declaration(channels: Vec<DeclaredChannel>, derived: Vec<Derived>) -> Declaration {
    let timebase = Ratio::integer(1000).unwrap();
    Declaration {
        timebase,
        channels,
        derived,
        sources: vec![SourceHeader::new(
            SourceFormat::Other("test".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
        requires: Vec::new(),
    }
}

fn object(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let serde_json::Value::Object(m) = v else {
        unreachable!()
    };
    m
}

/// A line of about 150 bytes, so a part of 64 KiB holds a few hundred.
fn line(id: &str, start: i64) -> String {
    format!(
        r#"{{"id":"{id}","start":{start},"text":"{}"}}"#,
        "x".repeat(100)
    )
}

/// V's samples from 100 to 190, every 10 ticks, finished at `length`: `marks` the generator
/// `verge`'s annotations, a writer's, or else `files` bundled.
fn written(length: i64, marks: &[i64], files: Option<BTreeMap<String, String>>) -> Vec<u8> {
    let mut w = Writer::new(
        declaration(vec![analog("V")], vec![]),
        WriteOptions::default(),
    )
    .unwrap();
    w.run(0, 100, 0).unwrap();
    for i in 0..10u64 {
        w.sample(0, i).unwrap();
    }
    for &t in marks {
        let m = object(
            serde_json::json!({"id": format!("m/{t}"), "start": t, "text": "x".repeat(100)}),
        );
        w.annotation("verge", m).unwrap();
    }
    let bundle = match files {
        Some(files) => Bundle::Files(files),
        None => Bundle::Generated { stem: "rec".into() },
    };
    let finish = Finish {
        bundle: Some(bundle),
        ..Finish::default()
    };
    let mut file = w.take_output();
    file.extend(w.finish(length, &[], finish).unwrap().tail);
    file
}

fn rows(file: &[u8]) -> Vec<PartRow> {
    Footer::parse_file(file)
        .unwrap()
        .part_rows(|at, len| Ok::<_, DecodeError>(file[at as usize..][..len as usize].to_vec()))
        .unwrap()
}

/// The markers that start in `range`, every one.
fn within(rec: &Recording, range: Range<i64>) -> Vec<Marker> {
    rec.markers(range, None, usize::MAX).entries
}

/// A finished file with no annotation bundle has no markers, read whole or by range, and nothing
/// left out.
#[test]
fn a_file_with_no_bundle_has_no_markers() {
    let mut w = Writer::new(
        declaration(vec![analog("V")], vec![]),
        WriteOptions::default(),
    )
    .unwrap();
    w.run(0, 100, 0).unwrap();
    for i in 0..10u64 {
        w.sample(0, i).unwrap();
    }
    let mut file = w.take_output();
    file.extend(w.finish(200, &[], Finish::default()).unwrap().tail);
    let bundle = Footer::parse_file(&file)
        .unwrap()
        .read_bundle(|at, len| Ok::<_, DecodeError>(file[at as usize..][..len as usize].to_vec()))
        .unwrap();
    assert!(bundle.is_none());
    let rec = Recording::open(Arc::new(file), Cache::new(1 << 20)).unwrap();
    rec.load_markers().unwrap();
    assert!(!rec.has_markers());
    assert!(within(&rec, 0..200).is_empty());
    assert!(rec.markers_left_out().is_empty());
}

/// A range's markers come from the parts the range meets and no other, each part read once; the
/// span the markers add to the extent comes from the parts table, no part read. Read whole
/// afterwards, the same markers come with the same cursors.
#[test]
fn a_range_reads_only_the_parts_it_meets_and_the_span_none() {
    let marks: Vec<i64> = (0..3000).map(|i| 100 * i).collect();
    let file = written(300_000, &marks, None);
    let rows = rows(&file);
    assert!(rows.len() >= 5, "{} parts", rows.len());
    let bytes = Counted::new(file);
    let rec = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    assert_eq!(rec.extent(), Some(0..299_901));
    assert_eq!(rec.markers_span(), Some(0..299_901));
    assert_eq!(bytes.parts_read(&rows), [] as [usize; 0]);

    let third = rows[2].start..rows[2].end + 1;
    let got = within(&rec, third.clone());
    assert_eq!(bytes.parts_read(&rows), [2]);
    let tried = rec.try_markers(third.clone(), None, usize::MAX).unwrap();
    assert_eq!(tried.entries, got);
    let ticks: Vec<i64> = got.iter().map(|m| m.cursor.tick).collect();
    let want: Vec<i64> = marks
        .iter()
        .copied()
        .filter(|t| third.contains(t))
        .collect();
    assert_eq!(ticks, want);
    // Again: from the cache.
    assert_eq!(within(&rec, third.clone()), got);
    assert_eq!(bytes.parts_read(&rows), [] as [usize; 0]);
    // A range across a part's end reads that part and the next.
    let across = rows[3].end..rows[4].start + 1;
    assert_eq!(within(&rec, across.clone()).len(), 2);
    assert_eq!(bytes.parts_read(&rows), [3, 4]);

    assert_eq!(rec.marker_count(), 3000);
    assert_eq!(within(&rec, third), got);
    assert_eq!(rec.extent(), Some(0..299_901));
}

/// The generators of a hand-written file that includes `rec.b.jsonl` then `rec.a.jsonl`, B's
/// lines spread over two parts.
struct Sidecar {
    file: Vec<u8>,
    rows: Vec<PartRow>,
}

/// `hand`'s annotations, B (generator `beta`) with `b` lines from tick 0 every 10, then `extra`,
/// and A (generator `alpha`) with `a`.
fn sidecar(hand: &str, b: usize, extra: &[String], a: &[String]) -> Sidecar {
    let plain = written(100_000, &[], Some(BTreeMap::new()));
    let footer = Footer::parse_file(&plain).unwrap();
    let id = footer.header.content_id;
    let mut beta: Vec<String> = (0..b)
        .map(|i| line(&format!("x/{i}"), 10 * i as i64))
        .collect();
    beta.extend_from_slice(extra);
    let files = BTreeMap::from([
        (
            "rec.annotations.toml".to_owned(),
            format!(
                "format = \"tickwave-annotations/1\"\nrecording_id = \"{id}\"\ninclude = \
                 [\"rec.b.jsonl\", \"rec.a.jsonl\"]\n{hand}"
            ),
        ),
        (
            "rec.b.jsonl".to_owned(),
            generator_file("beta", None, id, &beta),
        ),
        (
            "rec.a.jsonl".to_owned(),
            generator_file("alpha", None, id, a),
        ),
    ]);
    let file = escapement_tickwave_format::bundle::bundle(&plain, &footer, files, 3).unwrap();
    let rows = rows(&file);
    Sidecar { file, rows }
}

/// A hand-written `ref` shows once every part is read, and so does what makes the second of an
/// id left out: a range that reads the part with the second and not the one with the first shows
/// it (FORMAT.md, "Annotations"). The order within a tick is the sidecar's: the hand-written
/// file's, then each generator's in the order it includes them, whatever their names.
#[test]
fn a_ref_and_a_repeated_id_settle_once_every_part_is_read() {
    let hand = "[[annotation]]\nid = \"h\"\nstart = 30\n[[annotation]]\nid = \"r\"\nref = \
                \"beta/x/last\"\nlabel = \"followed\"\n";
    let s = sidecar(
        hand,
        800,
        &[line("x/3", 7990), line("x/last", 7995)],
        &[line("y/0", 30)],
    );
    // B's two parts, then A's.
    assert_eq!(s.rows.len(), 3, "{:?}", s.rows);
    let bytes = Counted::new(s.file);
    let rec = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();

    let second = s.rows[1].start..s.rows[1].end + 1;
    let ids = |ms: &[Marker]| {
        ms.iter()
            .map(|m| m.annotation.id.clone())
            .collect::<Vec<_>>()
    };
    let got = within(&rec, second.clone());
    assert_eq!(bytes.parts_read(&s.rows), [1]);
    let tail = &ids(&got)[got.len() - 2..];
    assert_eq!(tail, ["beta/x/3", "beta/x/last"]);
    // At tick 30: the hand-written annotation, B's line, A's line; read from B's first part and
    // A's.
    let at_30 = within(&rec, 30..31);
    assert_eq!(bytes.parts_read(&s.rows), [0, 2]);
    assert_eq!(ids(&at_30), ["h", "beta/x/3", "alpha/y/0"]);

    rec.load_markers().unwrap();
    let whole = within(&rec, second);
    let tail = &ids(&whole)[whole.len() - 2..];
    assert_eq!(tail, ["r", "beta/x/last"]);
    let r = &whole[whole.len() - 2];
    assert_eq!(
        (r.annotation.start, r.annotation.label.as_deref()),
        (7995, Some("followed"))
    );
    assert_eq!(
        rec.markers_left_out(),
        ["beta/x/3: an id its generator has given already"]
    );
    // What both read has the same cursor in both.
    for m in &got[..got.len() - 2] {
        assert!(whole.contains(m), "{m:?}");
    }
    assert_eq!(within(&rec, 30..31), at_30);
}

/// Read whole, a finished file's markers are the sidecar `Footer::read_sidecar` joins, resolved
/// by `resolve_lenient`: the same annotations in the same order, whatever breaks a rule. A `ref`
/// to an annotation left out is left out by both.
#[test]
fn read_whole_the_markers_are_the_joined_sidecar_resolved() {
    let plain = written(100_000, &[], Some(BTreeMap::new()));
    let footer = Footer::parse_file(&plain).unwrap();
    let id = footer.header.content_id;
    let other: ContentId =
        "blake3:0000000000000000000000000000000000000000000000000000000000000000"
            .parse()
            .unwrap();
    let hand = format!(
        "format = \"tickwave-annotations/1\"\nrecording_id = \"{id}\"\ninclude = \
         [\"rec.b.jsonl\", \"rec.a.jsonl\", \"rec.c.jsonl\"]\n\
         [[annotation]]\nid = \"h1\"\nstart = 5\n\
         [[annotation]]\nid = \"h2\"\nstart = 6\nchannels = [\"Y\"]\n\
         [[annotation]]\nid = \"r1\"\nref = \"beta/x/2\"\n\
         [[annotation]]\nid = \"r2\"\nref = \"nobody/z\"\n\
         [[annotation]]\nid = \"r3\"\nref = \"gamma/z/1\"\n\
         [[annotation]]\nid = \"r4\"\nref = \"beta/x/7\"\n"
    );
    let beta = [
        r#"{"id":"x/1","start":5}"#,
        r#"{"id":"x/2","start":6,"end":9}"#,
        r#"{"id":"x/1","start":7}"#,
        r#"{"id":"x/4","start":8,"colour":"red"}"#,
        r#"{"id":"x/5","start":100005}"#,
        r#"{"id":"x/6","start":99990,"end":100010}"#,
        r#"{"id":"x/7","start":10,"channels":["X"]}"#,
        r#"{"start":11}"#,
        r#"{"id":"x/9","start":100007}"#,
        r#"{"id":"x/9","start":12}"#,
    ];
    let files = BTreeMap::from([
        ("rec.annotations.toml".to_owned(), hand),
        (
            "rec.b.jsonl".to_owned(),
            generator_file("beta", None, id, &beta.map(String::from)),
        ),
        (
            "rec.a.jsonl".to_owned(),
            generator_file("alpha", None, id, &[r#"{"id":"y/1","start":5}"#.into()]),
        ),
        (
            "rec.c.jsonl".to_owned(),
            generator_file("gamma", None, other, &[r#"{"id":"z/1","start":5}"#.into()]),
        ),
    ]);
    let file = escapement_tickwave_format::bundle::bundle(&plain, &footer, files, 3).unwrap();

    let footer = Footer::parse_file(&file).unwrap();
    let sidecar = footer
        .read_sidecar(|at, len| Ok::<_, DecodeError>(file[at as usize..][..len as usize].to_vec()))
        .unwrap()
        .unwrap();
    let hand = &sidecar.files["rec.annotations.toml"];
    let included: Vec<Included<'_>> = sidecar
        .files
        .iter()
        .filter(|(name, _)| name.ends_with(".jsonl"))
        .map(|(name, text)| Included { name, text })
        .collect();
    let resolved = annotations::resolve_lenient(hand, &included, &footer.header, None);
    // r3 names gamma's line, which gamma's first line leaves out: r3 is left out. r4 names
    // beta's x/7, whose channel the recording lacks, and takes it, so it breaks that rule
    // itself. A line left out for the length claims no id: x/9 at 12 stays.
    let why = "annotation 5: ref \"gamma/z/1\" names an annotation left out";
    assert!(
        resolved.problems.iter().any(|p| p == why),
        "{why}: {:#?}",
        resolved.problems
    );
    // In sidecar order; sorted by start, stably.
    let mut want = resolved.annotations;
    want.sort_by_key(|a| a.start);
    let ids: Vec<&str> = want.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "h1",
            "beta/x/1",
            "alpha/y/1",
            "r1",
            "beta/x/2",
            "beta/x/9",
            "beta/x/6"
        ]
    );

    let rec = Recording::open(Arc::new(file), Cache::new(1 << 20)).unwrap();
    rec.load_markers().unwrap();
    let got: Vec<_> = within(&rec, i64::MIN..i64::MAX)
        .into_iter()
        .map(|m| m.annotation)
        .collect();
    assert_eq!(got, want);
    let left = rec.markers_left_out();
    for named in [
        "beta/x/1: an id its generator has given already",
        "beta/x/5: starts at or past the length",
        "colour",
        "no channel \"X\"",
        "no channel \"Y\"",
        "beta/(no id)",
        "ref \"nobody/z\" does not resolve",
        "ref \"gamma/z/1\" does not resolve",
        "ref \"beta/x/7\" does not resolve",
        "rec.c.jsonl: generated for",
    ] {
        assert!(left.iter().any(|l| l.contains(named)), "{named}: {left:#?}");
    }
}

/// V and D require a feature this version doesn't know, and P is over V: all three keep their
/// places and names, shown as unavailable, and the markers that name V or measure D are left out
/// and named, the rest kept (FORMAT.md, "Unavailable channels").
#[test]
fn a_channel_this_version_cant_read_keeps_its_place_and_its_markers_are_left_out() {
    let d = declaration(
        vec![analog("V"), analog("I"), digital("D")],
        vec![Derived::multiply("P", "W", "I", "V")],
    );
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    for c in 0..2 {
        w.run(c, 0, 0).unwrap();
    }
    for i in 0..10u64 {
        w.sample(0, i).unwrap();
        w.sample(1, i + 1).unwrap();
    }
    w.edge(2, 20).unwrap();
    for m in [
        serde_json::json!({"id": "a", "start": 1, "channels": ["I", "V"]}),
        serde_json::json!({"id": "b", "start": 2, "end": 5, "measure": {"channel": "D", "what": "rate"}}),
        serde_json::json!({"id": "c", "start": 3, "channels": ["I"]}),
        serde_json::json!({"id": "d", "start": 4}),
    ] {
        w.annotation("verge", object(m)).unwrap();
    }
    let mut file = w.take_output();
    let finish = Finish {
        bundle: Some(Bundle::Generated { stem: "rec".into() }),
        ..Finish::default()
    };
    file.extend(w.finish(100, &[100], finish).unwrap().tail);
    let mut header = Footer::parse_file(&file).unwrap().header;
    for c in [0, 2] {
        header.channels[c].requires = vec!["frobs".into()];
    }
    let file = with_header(&file, &header);

    let rec = Recording::open(Arc::new(file), Cache::new(1 << 20)).unwrap();
    let channels = rec.channels();
    let names: Vec<&str> = channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["V", "I", "D", "P"]);
    for c in [0, 2, 3] {
        assert_eq!(channels[c].kind, ChannelKind::Unavailable, "{c}");
        let why = channels[c].unavailable.as_deref().unwrap();
        assert!(why.contains("unavailable"), "{c}: {why}");
    }
    assert!(channels[1].unavailable.is_none());
    assert_eq!(rec.channel("I"), Some(1));

    let cancel = AtomicBool::new(false);
    let window = 0..100;
    let q = {
        let mut query = Query::new(window.clone(), 10);
        query.channels = vec![0, 1, 2, 3];
        query.statistics = vec![window];
        query.markers = {
            let mut markers = MarkerQuery::default();
            markers.limit = 10;
            markers
        };
        query
    };
    let a = rec.query(&q, &cancel).unwrap();
    for c in [0, 2, 3] {
        assert!(matches!(a.channels[c], Drawn::Unavailable), "{c}");
    }
    assert!(!matches!(a.channels[1], Drawn::Unavailable));
    let stats = &a.statistics[0].channels;
    assert!(stats[0].is_none() && stats[3].is_none());
    assert_eq!(stats[1].as_ref().unwrap().samples, 10);
    let ids: Vec<&str> = a
        .markers
        .window
        .entries
        .iter()
        .map(|m| m.annotation.id.as_str())
        .collect();
    assert_eq!(ids, ["verge/c", "verge/d"]);
    assert_eq!(rec.marker_count(), 2);
    let left = rec.markers_left_out();
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(
        left[0].starts_with("verge/a: names channel \"V\""),
        "{left:?}"
    );
    assert!(
        left[1].starts_with("verge/b: names channel \"D\""),
        "{left:?}"
    );
    let err = rec.mapping(0).unwrap_err().to_string();
    assert!(
        err.contains("channel 0 is unavailable to this version"),
        "{err}"
    );
}

/// `file` with its footer's header replaced by `header`, the top index and the strips kept.
fn with_header(file: &[u8], header: &Header) -> Vec<u8> {
    let t = file.len() - 24;
    let at = u64::from_le_bytes(file[t..t + 8].try_into().unwrap()) as usize;
    let len = u32::from_le_bytes(file[t + 8..t + 12].try_into().unwrap()) as usize;
    let payload = &file[at + 5..at + len - 8];
    let header_len = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
    let json = serde_json::to_vec(header).unwrap();
    let mut new = Vec::new();
    new.extend_from_slice(&(json.len() as u32).to_le_bytes());
    new.extend_from_slice(&payload[4..8]);
    new.extend_from_slice(&json);
    new.extend_from_slice(&payload[8 + header_len..]);
    let mut out = file[..at].to_vec();
    escapement_tickwave_format::record::write_footer(&mut out, 0, &new);
    out
}

/// A bundled sidecar of a later minor version gives the markers this version reads of it, and
/// names what it holds that this version doesn't know with what was left out (ANNOTATIONS.md,
/// "Versions").
#[test]
fn a_later_minor_version_gives_the_same_markers_and_names_what_it_left_out() {
    let plain = written(100_000, &[], Some(BTreeMap::new()));
    let footer = Footer::parse_file(&plain).unwrap();
    let id = footer.header.content_id;
    let bundled = |format: &str, extra: &str| {
        let files = BTreeMap::from([
            (
                "rec.annotations.toml".to_owned(),
                format!(
                    "format = \"{format}\"\nrecording_id = \"{id}\"\ninclude = [\"rec.b.jsonl\"]\n\
                     [[annotation]]\nid = \"h\"\nstart = 30\n{extra}"
                ),
            ),
            (
                "rec.b.jsonl".to_owned(),
                generator_file("beta", None, id, &[line("x/0", 10)]),
            ),
        ]);
        let file = escapement_tickwave_format::bundle::bundle(&plain, &footer, files, 3).unwrap();
        let rec = Recording::open(Arc::new(file), Cache::new(1 << 20)).unwrap();
        rec.load_markers().unwrap();
        rec
    };
    let known = bundled("tickwave-annotations/1", "");
    let later = bundled(
        "tickwave-annotations/1.1",
        "[[decode]]\nid = \"swd\"\ndecoder = \"swd\"\n",
    );
    assert_eq!(within(&later, 0..100_000), within(&known, 0..100_000));
    assert_eq!(within(&later, 0..100_000).len(), 2);
    assert!(known.markers_left_out().is_empty());
    assert_eq!(
        later.markers_left_out(),
        [
            "the TOML file: \"tickwave-annotations/1.1\" is newer than this reader, which leaves \
             out decode"
        ]
    );
}
