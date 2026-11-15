//! What other tools read of tickwave's output, run as they run it: the fields of `info --json`
//! that mculink-bench's tools/wire/sidecar.py reads, and the shape of `annotations check --json`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

fn tickwave(args: &[&std::ffi::OsStr]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_tickwave"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// identify-swd's capture converted with SWCLK moved 40 ns earlier, merged with the energy fixture
/// aligned 1 ms later.
fn merged(dir: &Path) -> PathBuf {
    let (a, m) = (dir.join("a.tickwave"), dir.join("m.tickwave"));
    let export = repo().join("testdata/saleae/identify-swd");
    let (ok, _, err) = tickwave(&[
        "convert".as_ref(),
        "saleae".as_ref(),
        export.as_ref(),
        "--channel".as_ref(),
        "digital_3_0=SWCLK".as_ref(),
        "--channel".as_ref(),
        "digital_3_1=SWDIO".as_ref(),
        "--offset".as_ref(),
        "SWCLK=-40ns".as_ref(),
        "--level".as_ref(),
        "3".as_ref(),
        "-o".as_ref(),
        a.as_ref(),
    ]);
    assert!(ok, "{err}");
    let energy = repo().join("web/fixtures/energy.tickwave");
    let (ok, _, err) = tickwave(&[
        "merge".as_ref(),
        a.as_ref(),
        energy.as_ref(),
        "--align".as_ref(),
        "2=1ms".as_ref(),
        "--level".as_ref(),
        "3".as_ref(),
        "-o".as_ref(),
        m.as_ref(),
    ]);
    assert!(ok, "{err}");
    m
}

/// sidecar.py reads `header`'s `channels[].name`, `.source` and `.offset` (0 where absent),
/// `sources[i].begin_s`, `.origin` and `.timebase` as `[num, den]`, `timebase` and
/// `content_id`, and nothing else.
#[test]
fn info_json_keeps_every_field_the_wire_decoders_read() {
    let dir = tempfile::tempdir().unwrap();
    let m = merged(dir.path());
    let (ok, out, err) = tickwave(&["info".as_ref(), m.as_ref(), "--json".as_ref()]);
    assert!(ok, "{err}");
    let report: Value = serde_json::from_str(&out).unwrap();
    let h = &report["header"];
    let channels: Vec<(&str, u64, i64)> = h["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap(),
                c["source"].as_u64().unwrap(),
                c.get("offset").map_or(0, |o| o.as_i64().unwrap()),
            )
        })
        .collect();
    assert_eq!(
        channels,
        [
            ("SWCLK", 0, -40),
            ("SWDIO", 0, 0),
            ("V", 1, 0),
            ("I", 1, 0),
            ("range", 1, 0),
            ("under_range", 1, 0),
            ("DIO0", 1, 0),
        ]
    );
    let sources: Vec<(Option<f64>, i64, (u64, u64))> = h["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let tb = s["timebase"].as_array().unwrap();
            (
                s.get("begin_s").map(|b| b.as_f64().unwrap()),
                s["origin"].as_i64().unwrap(),
                (tb[0].as_u64().unwrap(), tb[1].as_u64().unwrap()),
            )
        })
        .collect();
    // The capture's tick 0 lies 40 of its ticks in, 12 merged ticks each at 12 GHz; the energy
    // capture's 1 ms in. Its begin is the export's own.
    assert_eq!(
        sources,
        [
            (Some(0.07553184799994528), 480, (1_000_000_000, 1)),
            (None, 12_000_000, (96_000_000, 1)),
        ]
    );
    assert_eq!(h["timebase"], json!([12_000_000_000u64, 1]));
    let id = h["content_id"].as_str().unwrap();
    let hex = id.strip_prefix("blake3:").unwrap();
    assert!(
        hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "{id}"
    );
    let bytes = std::fs::read(&m).unwrap();
    let footer = escapement_tickwave_format::decode::Footer::parse_file(&bytes).unwrap();
    assert_eq!(id, footer.header.content_id.to_string());
}

/// `annotations check --json`: every annotation resolved, in the sidecar's order, generated ones
/// after the hand-written file's, each with every field it has, absent ones as null.
#[test]
fn annotations_check_json_gives_each_annotation_resolved_with_every_field() {
    let dir = tempfile::tempdir().unwrap();
    let recording = repo().join("web/fixtures/identify-swd.tickwave");
    let id = "blake3:ccc8b7d8c3e4cb30da7fc6088e54ebeba2858ad402eb3fb5a9aeb50be989accf";
    let sidecar = dir.path().join("rec.annotations.toml");
    std::fs::write(
        &sidecar,
        format!(
            "format = \"tickwave-annotations/1\"\nrecording_id = \"{id}\"\n\
             include = [\"rec.dec.jsonl\"]\n\n\
             [[annotation]]\nid = \"first\"\nref = \"dec/pulse/1\"\nlabel = \"The first pulse\"\n\n\
             [[annotation]]\nid = \"span\"\nstart = \"1 ms\"\nend = \"2.5 ms\"\n\
             channels = [\"SWCLK\"]\nkind = \"note\"\nlabel = \"A span\"\ntext = \"Some text.\"\n\
             paint = \"probe\"\nmeasure = {{ channel = \"SWCLK\", what = \"rate\" }}\n\
             values = {{ clock = \"4 MHz\" }}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("rec.dec.jsonl"),
        format!(
            "{{\"format\":\"tickwave-annotations/1\",\"source\":\"dec\",\"recording_id\":\"{id}\"}}\n\
             {{\"id\":\"pulse/1\",\"kind\":\"pulse\",\"start\":1000,\"end\":2000,\
             \"channels\":[\"SWDIO\"],\"label\":\"a pulse\"}}\n\
             {{\"id\":\"mark/3\",\"start\":3000}}\n"
        ),
    )
    .unwrap();
    let (ok, out, err) = tickwave(&[
        "annotations".as_ref(),
        "check".as_ref(),
        recording.as_ref(),
        sidecar.as_ref(),
        "--json".as_ref(),
    ]);
    assert!(ok, "{err}");
    let got: Value = serde_json::from_str(&out).unwrap();
    let none = Value::Null;
    assert_eq!(
        got,
        json!([
            {
                "id": "first", "source": none, "ref": "dec/pulse/1", "start": 1000, "end": 2000,
                "channels": ["SWDIO"], "label": "The first pulse", "kind": "pulse", "text": none,
                "paint": none, "measure": none, "values": none,
            },
            {
                "id": "span", "source": none, "start": 1_000_000, "end": 2_500_000,
                "channels": ["SWCLK"], "label": "A span", "kind": "note", "text": "Some text.",
                "paint": "probe", "measure": {"channel": "SWCLK", "what": "rate"},
                "values": {"clock": "4 MHz"},
            },
            {
                "id": "dec/pulse/1", "source": "dec", "start": 1000, "end": 2000,
                "channels": ["SWDIO"], "label": "a pulse", "kind": "pulse", "text": none,
                "paint": none, "measure": none, "values": none,
            },
            {
                "id": "dec/mark/3", "source": "dec", "start": 3000, "end": none, "channels": [],
                "label": none, "kind": none, "text": none, "paint": none, "measure": none,
                "values": none,
            },
        ])
    );
}
