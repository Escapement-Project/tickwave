//! A recording's paint roles, as its sidecar declares them, its paints over a window and its
//! stored view (ANNOTATIONS.md, "Roles", "The stored view"), from its bundle or a host's sidecar.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use escapement_tickwave_format::annotations::{Colour, Included, View};
use escapement_tickwave_format::decode::{DecodeError, Footer};
use escapement_tickwave_format::header::{
    Declaration, DeclaredChannel, DeclaredKind, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::writer::{Bundle, Finish, WriteOptions, Writer, generator_head};
use escapement_tickwave_view::{Bytes, Cache, MarkerQuery, Paint, Query, Recording};

const LENGTH: i64 = 100_000;

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

fn declaration() -> Declaration {
    let timebase = Ratio::integer(1000).unwrap();
    let digital = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    Declaration {
        timebase,
        channels: vec![digital("C"), digital("D")],
        derived: vec![],
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

/// The paint of the generator's line `i`, of 1,000: "late" first at 600 and "early" first at
/// 900, though "early"'s spans start sooner; probe and target the rest. Each line 150 bytes or so,
/// so a part of 64 KiB holds a few hundred.
fn line_paint(i: i64) -> &'static str {
    match i {
        600 => "late",
        900 => "early",
        _ if i % 2 == 0 => "probe",
        _ => "target",
    }
}

fn line_span(i: i64) -> (i64, i64) {
    if line_paint(i) == "early" {
        (10, 20)
    } else {
        (i * 90, i * 90 + 200)
    }
}

/// The sidecar's files: a hand-written file declaring a role and a view, with an annotation
/// painting with it and one painting "handy", which nothing declares, and generator `dec`'s file
/// declaring probe and target. The bundle is written as given, unchecked, as a writer bundles a
/// sidecar handed to it: a reader meets undeclared paints in such a file.
fn sidecar(id: escapement_tickwave_format::header::ContentId) -> BTreeMap<String, String> {
    let roles = serde_json::json!([{"name": "probe", "label": "probe drives"}, {"name": "target", "label": "target drives", "weight": 1}]);
    let mut jsonl = generator_head("dec", None, id, roles.as_array().unwrap()) + "\n";
    for i in 0..1000 {
        let (start, end) = line_span(i);
        let line = serde_json::json!({"id": format!("x/{i}"), "start": start, "end": end, "channels": ["D"], "paint": line_paint(i), "text": "x".repeat(80)});
        jsonl.push_str(&format!("{line}\n"));
    }
    let toml = format!(
        "format = \"tickwave-annotations/1\"\nrecording_id = \"{id}\"\ninclude = [\"rec.dec.jsonl\"]\n\n\
         [view]\nchannels = [\"D\"]\nstart = 100\nend = 150\ncolour = [\"#d9480f\", \"#ff8a50\"]\n\n\
         [[role]]\nname = \"neither\"\nlabel = \"neither drives\"\ndash = [2, 2]\n\n\
         [[annotation]]\nid = \"mine\"\nstart = 50000\nend = 50100\nchannels = [\"C\"]\npaint = \"neither\"\n\n\
         [[annotation]]\nid = \"stray\"\nstart = 50000\nend = 50100\nchannels = [\"C\"]\npaint = \"handy\"\n"
    );
    BTreeMap::from([
        ("rec.annotations.toml".into(), toml),
        ("rec.dec.jsonl".into(), jsonl),
    ])
}

/// The recording, its sidecar bundled, its generator's lines in parts.
fn written() -> Vec<u8> {
    // The content id comes from the data alone, so a first pass gives the one the sidecar names.
    let write = |files: Option<BTreeMap<String, String>>| {
        let mut w = Writer::new(declaration(), WriteOptions::default()).unwrap();
        w.edge(0, 10).unwrap();
        w.edge(1, 20).unwrap();
        let finish = Finish {
            bundle: files.map(Bundle::Files),
            ..Finish::default()
        };
        let mut file = w.take_output();
        file.extend(w.finish(LENGTH, &[LENGTH, LENGTH], finish).unwrap().tail);
        file
    };
    let id = Footer::parse_file(&write(None)).unwrap().header.content_id;
    write(Some(sidecar(id)))
}

fn opened() -> (Arc<Counted>, Recording) {
    let bytes = Arc::new(Counted {
        bytes: written(),
        reads: Mutex::default(),
    });
    let rec = Recording::open(bytes.clone(), Cache::new(1 << 24)).unwrap();
    (bytes, rec)
}

#[test]
fn the_stored_view_and_the_declared_roles_read_from_the_bundle_before_any_part() {
    let (bytes, rec) = opened();
    let file = &bytes.bytes;
    let parts = Footer::parse_file(file)
        .unwrap()
        .part_rows(|at, len| Ok::<_, DecodeError>(file[at as usize..][..len as usize].to_vec()))
        .unwrap();
    assert!(parts.len() >= 3, "{} parts", parts.len());
    bytes.reads.lock().unwrap().clear();
    assert_eq!(
        rec.stored_view().unwrap(),
        Some(View {
            channels: Some(vec!["D".into()]),
            start: Some(100),
            end: Some(150),
            colour: Some(Colour {
                day: "#d9480f".into(),
                night: "#ff8a50".into()
            }),
        })
    );
    let read = bytes.reads.lock().unwrap().clone();
    assert!(
        !parts
            .iter()
            .any(|p| read.iter().any(|r| r.start == p.offset)),
        "a part read for the view"
    );
}

#[test]
fn the_roles_are_those_declared_in_declaration_order_and_read_no_part() {
    let (bytes, rec) = opened();
    let file = &bytes.bytes;
    let parts = Footer::parse_file(file)
        .unwrap()
        .part_rows(|at, len| Ok::<_, DecodeError>(file[at as usize..][..len as usize].to_vec()))
        .unwrap();
    bytes.reads.lock().unwrap().clear();
    let got: Vec<(String, String)> = rec
        .roles()
        .unwrap()
        .into_iter()
        .map(|r| (r.name, r.label))
        .collect();
    let s = |n: &str, l: &str| (n.to_owned(), l.to_owned());
    // The hand-written file's declaration, then the generator's; none for "handy", "late" or
    // "early", which annotations paint with and nothing declares.
    assert_eq!(
        got,
        [
            s("neither", "neither drives"),
            s("probe", "probe drives"),
            s("target", "target drives"),
        ]
    );
    let read = bytes.reads.lock().unwrap().clone();
    assert!(
        !parts
            .iter()
            .any(|p| read.iter().any(|r| r.start == p.offset)),
        "a part read for the roles"
    );
}

#[test]
fn a_windows_paints_are_every_span_that_meets_it_in_sidecar_order_from_the_parts_it_meets() {
    let (bytes, rec) = opened();
    let file = &bytes.bytes;
    let parts = Footer::parse_file(file)
        .unwrap()
        .part_rows(|at, len| Ok::<_, DecodeError>(file[at as usize..][..len as usize].to_vec()))
        .unwrap();
    bytes.reads.lock().unwrap().clear();
    // 45,050 to 45,150: lines 499 (44,910 to 45,110, which starts before it), 500 and 501
    // (45,090 to 45,290). Line i spans 90 i to 90 i + 200.
    let got = rec.paints(45_050..45_150).unwrap();
    let want: Vec<(String, Range<i64>)> = vec![
        ("target".into(), 44_910..45_110),
        ("probe".into(), 45_000..45_200),
        ("target".into(), 45_090..45_290),
    ];
    assert_eq!(
        got.iter()
            .map(|p| (p.role.clone(), p.span.clone()))
            .collect::<Vec<_>>(),
        want
    );
    assert!(got[0].arrival < got[1].arrival);
    let read = bytes.reads.lock().unwrap().clone();
    let touched = parts
        .iter()
        .filter(|p| read.iter().any(|r| r.start == p.offset))
        .count();
    assert!(
        touched < parts.len(),
        "{touched} of {} parts read",
        parts.len()
    );
    // Over the hand-written ones, their paint with the generator's that meet it, in sidecar
    // order: the hand-written file first, and "handy", which nothing declares, left out.
    let got: Vec<Paint> = rec.paints(50_040..50_060).unwrap();
    assert_eq!(got[0].role, "neither");
    assert_eq!(got[0].channels, ["C"]);
    assert!(got.iter().all(|p| p.role != "handy"), "{got:?}");
    assert!(got.len() >= 2, "{got:?}");
    // Line 600 paints "late" over 54,000 to 54,200, which nothing declares: its neighbours alone.
    assert_eq!(
        rec.paints(54_120..54_180)
            .unwrap()
            .iter()
            .map(|p| (p.role.as_str(), p.span.clone()))
            .collect::<Vec<_>>(),
        [("target", 54_090..54_290)]
    );
    // Read whole, the same.
    rec.load_markers().unwrap();
    assert_eq!(
        rec.paints(45_050..45_150).unwrap(),
        rec.paints(45_050..45_150).unwrap()
    );
    assert_eq!(
        rec.paints(45_050..45_150)
            .unwrap()
            .iter()
            .map(|p| p.role.as_str())
            .collect::<Vec<_>>(),
        ["target", "probe", "target"]
    );
}

#[test]
fn a_query_asking_for_paints_has_every_one_over_its_columns() {
    let (_, rec) = opened();
    let mut q = Query::new(45_000..45_100, 100);
    q.markers = MarkerQuery::default();
    q.markers.paints = true;
    let a = rec
        .query(&q, &std::sync::atomic::AtomicBool::new(false))
        .unwrap();
    // `around` is the window and a width either side, 44,900 to 45,200: lines 497 (to 44,930)
    // to 502 (from 45,180).
    let roles: Vec<&str> = a.markers.paints.iter().map(|p| p.role.as_str()).collect();
    assert_eq!(
        roles,
        ["target", "probe", "target", "probe", "target", "probe"]
    );
    // Not asked, none.
    let a = rec
        .query(
            &Query::new(45_000..45_100, 100),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
    assert!(a.markers.paints.is_empty());
}

#[test]
fn a_hosts_sidecar_gives_its_own_roles_and_view() {
    let (bytes, _) = opened();
    let id = Footer::parse_file(&bytes.bytes).unwrap().header.content_id;
    let hand = format!(
        "format = \"tickwave-annotations/1\"\nrecording_id = \"{id}\"\n\n[view]\nchannels = [\"C\", \"D\"]\n\n\
         [[role]]\nname = \"bus\"\ncolour = \"#123456\"\n\n[[annotation]]\nid = \"a\"\nstart = 10\nend = 20\npaint = \"bus\"\n"
    );
    let open = || Recording::open(Arc::new(bytes.bytes.clone()), Cache::new(1 << 20)).unwrap();
    let rec = open().with_sidecar(&hand, &[] as &[Included<'_>]).unwrap();
    assert_eq!(
        rec.stored_view().unwrap().unwrap().channels,
        Some(vec!["C".into(), "D".into()])
    );
    let names: Vec<String> = rec.roles().unwrap().into_iter().map(|r| r.name).collect();
    assert_eq!(names, ["bus"]);
    assert_eq!(rec.paints(0..100).unwrap().len(), 1);
    // A host's sidecar is checked whole, so one that paints with a role it doesn't declare is
    // refused, saying so.
    let other = hand.replace("paint = \"bus\"", "paint = \"other\"");
    let refused = open().with_sidecar(&other, &[] as &[Included<'_>]);
    assert!(
        matches!(&refused, Err(escapement_tickwave_view::Error::Annotations(p)) if p.contains(r#"paint "other" names no role the sidecar declares"#)),
        "{:?}",
        refused.err()
    );
}

#[test]
fn a_writers_own_view_reads_back_as_the_stored_view() {
    let stored = View {
        channels: Some(vec!["D".into(), "C".into()]),
        start: Some(100),
        end: None,
        colour: Some(Colour {
            day: "#d9480f".into(),
            night: "#ff8a50".into(),
        }),
    };
    for (marked, view) in [
        (true, Some(stored.clone())),
        (false, Some(stored)),
        (true, None),
    ] {
        let mut w = Writer::new(declaration(), WriteOptions::default()).unwrap();
        w.edge(0, 10).unwrap();
        if marked {
            let serde_json::Value::Object(m) = serde_json::json!({"id": "m/1", "start": 20}) else {
                unreachable!()
            };
            w.annotation("verge", m).unwrap();
        }
        let finish = Finish {
            bundle: Some(Bundle::Generated { stem: "rec".into() }),
            view: view.clone(),
            ..Finish::default()
        };
        let mut file = w.take_output();
        file.extend(w.finish(LENGTH, &[LENGTH, LENGTH], finish).unwrap().tail);
        let rec = Recording::open(Arc::new(file), Cache::new(1 << 20)).unwrap();
        assert_eq!(rec.stored_view().unwrap(), view, "marked {marked}");
    }
}
