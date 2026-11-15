//! Writes each fuzz target's seed corpus from the repository's fixtures and testdata:
//! `cargo run --no-default-features --bin seeds -- <repository> <corpus root>`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_fuzz::{self as fz, RAW};
use escapement_tickwave_import::convert::{self, Pass, Progress, Settings, Step};
use escapement_tickwave_import::{merge, vcd};

struct Quiet;

impl Progress for Quiet {
    fn start(&self, _: Pass<'_>, _: u64) -> Step {
        Arc::new(|_| {})
    }
}

fn files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == ext))
        .collect();
    out.sort();
    out
}

struct Corpus {
    root: PathBuf,
    count: BTreeMap<&'static str, usize>,
}

impl Corpus {
    fn put(&mut self, target: &'static str, name: &str, data: &[u8]) {
        let dir = self.root.join(target);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("seed-{name}")), data).unwrap();
        *self.count.entry(target).or_default() += 1;
    }
}

/// A structured input holding `bytes` as they are, with no preamble of its own.
fn raw(bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![fz::NO_PREAMBLE, 0, RAW];
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

/// A sigrok session file's members, as the sigrok target's structured input holds them.
fn members(sr: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(sr)).unwrap();
    let mut out = vec![1, zip.len() as u8];
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).unwrap();
        let name = f.name().unwrap().into_owned();
        let deflated = f.compression() == zip::CompressionMethod::Deflated;
        let mut content = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut content).unwrap();
        out.push(name.len() as u8);
        out.extend_from_slice(name.as_bytes());
        out.push(u8::from(deflated));
        out.extend_from_slice(&(content.len() as u32).to_le_bytes());
        out.extend_from_slice(&content);
    }
    out
}

/// A session whose unitsize is 1,000,000,000, its logic member one byte whose headers state
/// 1,000,000,000: a size stated at no cost, which the reader once sized its scan by.
fn stated_unitsize() -> Vec<u8> {
    let meta = "[global]\nsigrok version=0.5.2\n\n[device 1]\ncapturefile=logic-1\n\
                total probes=3\nsamplerate=1 MHz\nprobe1=A\nprobe3=C\nunitsize=1000000000\n";
    let mut out = vec![1, 3];
    for (name, flags, content) in [
        ("version", 0u8, &b"2"[..]),
        ("metadata", 0, meta.as_bytes()),
        ("logic-1-1", 2, &[1][..]),
    ] {
        out.push(name.len() as u8);
        out.extend_from_slice(name.as_bytes());
        out.push(flags);
        out.extend_from_slice(&(content.len() as u32).to_le_bytes());
        out.extend_from_slice(content);
        if flags & 2 != 0 {
            out.extend_from_slice(&1_000_000_000u32.to_le_bytes());
        }
    }
    out
}

/// A version 1 file as a version 0 export writes it, as import/tests/saleae.rs's `v1_to_v0`: no
/// chunk count, and no rate in a digital file or trigger in an analog one.
fn v1_to_v0(v1: &[u8]) -> Vec<u8> {
    let kind = i32::from_le_bytes(v1[12..16].try_into().unwrap());
    let mut v0 = v1[..8].to_vec();
    v0.extend(0i32.to_le_bytes());
    v0.extend(kind.to_le_bytes());
    if kind == 0 {
        v0.extend(&v1[24..28]);
        v0.extend(&v1[36..]);
    } else {
        v0.extend(&v1[24..32]);
        let rate = f64::from_le_bytes(v1[40..48].try_into().unwrap());
        v0.extend((rate as u64).to_le_bytes());
        v0.extend(&v1[48..]);
    }
    v0
}

/// A Saleae export's files, as the saleae target's input holds them, after its rate byte; in
/// version 0 where `v0`.
fn export(dir: &Path, v0: bool) -> Vec<u8> {
    let bins = files(dir, "bin");
    let mut out = vec![bins.len() as u8];
    for p in bins {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = fs::read(&p).unwrap();
        let bytes = if v0 { v1_to_v0(&bytes) } else { bytes };
        out.push(name.len() as u8);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&bytes);
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (repo, root) = (PathBuf::from(&args[1]), PathBuf::from(&args[2]));
    let mut c = Corpus {
        root,
        count: BTreeMap::new(),
    };
    let fixtures = repo.join("web/fixtures");
    let mut recordings = files(&fixtures, "tickwave");
    recordings.extend(files(&fixtures.join("crafted"), "tickwave"));

    for path in &recordings {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let bytes = fs::read(path).unwrap();
        let s = fz::structure(&bytes).expect("a fixture structures");
        let rebuilt = fz::file(&s);
        let crafted = path.parent().unwrap().ends_with("crafted");
        if !crafted && let Err(e) = Footer::parse_file(&rebuilt) {
            panic!("{name}: rebuilt from its structure, it doesn't read: {e}");
        }
        let mut cut = Vec::new();
        // Cut before its footer, and at a few bytes inside a record.
        if let Ok(f) = Footer::parse_file(&bytes) {
            let at = f.trailer.footer_offset as usize;
            cut.push(("unfinished", fz::structure(&bytes[..at]).unwrap()));
            cut.push((
                "cut-mid",
                fz::structure(&bytes[..at.saturating_sub(37).max(12)]).unwrap(),
            ));
        }
        for target in [
            "footer", "block", "reader", "recut", "repair", "verify", "view", "against",
        ] {
            let prefixed = |data: &[u8]| -> Vec<u8> {
                match target {
                    "view" => [&[0u8; 48][..], data].concat(),
                    "against" => [&[0u8][..], data].concat(),
                    _ => data.to_vec(),
                }
            };
            c.put(target, &name, &prefixed(&s));
            if matches!(target, "footer" | "reader" | "repair" | "recut") {
                c.put(target, &format!("{name}-raw"), &prefixed(&raw(&bytes)));
            }
            if matches!(target, "reader" | "recut" | "repair") {
                for (what, data) in &cut {
                    c.put(target, &format!("{name}-{what}"), &prefixed(data));
                }
            }
        }
    }

    // A merge of two fixtures and a moved and cut conversion, for the target that checks a
    // header against what it says it came from.
    let read = |name: &str| -> merge::Input {
        let bytes: Arc<[u8]> = fs::read(fixtures.join(name)).unwrap().into();
        merge::Input::read(Arc::new(bytes), name.trim_end_matches(".tickwave")).unwrap()
    };
    let mut out = std::io::Cursor::new(Vec::new());
    merge::merge(
        &|| Ok(vec![read("identify-swd.tickwave"), read("energy.tickwave")]),
        &merge::Settings {
            level: 1,
            ..merge::Settings::default()
        },
        &Quiet,
        &mut out,
    )
    .expect("the fixtures merge");
    c.put(
        "against",
        "merged",
        &[&[0u8][..], &fz::structure(out.get_ref()).unwrap()].concat(),
    );
    let vcd_path = repo.join("testdata/import/demo-logic.vcd");
    let vcd_bytes = fs::read(&vcd_path).unwrap();
    let channels = vcd::read_from(&vcd_bytes[..], "demo-logic.vcd")
        .unwrap()
        .recording
        .channels
        .iter()
        .map(|c| c.name.clone())
        .collect::<Vec<_>>();
    let mut out = std::io::Cursor::new(Vec::new());
    convert::convert(
        &|| Ok(vcd::read_from(&vcd_bytes[..], "demo-logic.vcd")?.recording),
        &Settings {
            level: 1,
            block_kib: 1,
            offsets: vec![(channels[1].clone(), "3".into())],
            keeps: vec![(channels[0].clone(), Some("10".into()), Some("200".into()))],
            ..Settings::default()
        },
        &Quiet,
        &mut out,
    )
    .expect("the VCD converts, moved and cut");
    c.put(
        "against",
        "shaped",
        &[&[1u8][..], &fz::structure(out.get_ref()).unwrap()].concat(),
    );

    // Sidecars: the hand-written file, then each included file's name and text, apart by NULs.
    let toml = fs::read(fixtures.join("identify-swd.annotations.toml")).unwrap();
    let jsonl = fs::read(fixtures.join("identify-swd.swd.jsonl")).unwrap();
    let sidecar = [&toml[..], b"\0identify-swd.swd.jsonl\0", &jsonl[..]].concat();
    c.put("annotations", "identify-swd", &sidecar);
    c.put("annotations", "hand-only", &toml);
    for (name, op) in [
        (
            "insert",
            r#"{"op":"insert","index":1,"fields":{"id":"x","start":"1 ms","end":5,"channels":["SWCLK"],"measure":{"channel":"SWCLK","what":"rate"}}}"#,
        ),
        (
            "replace",
            r#"{"op":"replace","index":0,"fields":{"id":"y","ref":"swd/wait/379065159","label":"z"}}"#,
        ),
        ("remove", r#"{"op":"remove","index":2}"#),
        ("move", r#"{"op":"move","from":0,"to":3}"#),
    ] {
        c.put("edit", name, &[&toml[..], b"\0", op.as_bytes()].concat());
    }
    c.put(
        "edit",
        "start",
        br#"\0{"op":"start","recording":"r.tickwave","recording_id":"blake3:00"}"#,
    );

    for p in files(&fixtures.join("zip"), "zip") {
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        c.put("zip", &name, &fs::read(&p).unwrap());
    }
    let import = repo.join("testdata/import");
    for p in files(&import, "sr") {
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        let bytes = fs::read(&p).unwrap();
        c.put("zip", &name, &bytes);
        c.put(
            "sigrok",
            &format!("{name}-raw"),
            &[&[0u8][..], &bytes].concat(),
        );
        c.put("sigrok", &name, &members(&bytes));
        c.put(
            "convert",
            &name,
            &[&[1u8, 0][..], &members(&bytes)].concat(),
        );
    }
    c.put("sigrok", "stated-unitsize", &stated_unitsize());
    for p in files(&import, "vcd") {
        let name = p.file_stem().unwrap().to_string_lossy().into_owned();
        let bytes = fs::read(&p).unwrap();
        c.put("vcd", &name, &bytes);
        c.put(
            "convert",
            &format!("{name}-vcd"),
            &[&[2u8, 0][..], &bytes].concat(),
        );
    }
    for dir in ["identify-swd", "swd50-cut"] {
        for v0 in [false, true] {
            let e = export(&repo.join("testdata/saleae").join(dir), v0);
            let name = if v0 {
                format!("{dir}-v0")
            } else {
                dir.to_owned()
            };
            // Rate byte 0 takes what the files record, or infers it.
            c.put("saleae", &name, &[&[0u8][..], &e].concat());
            c.put("convert", &name, &[&[0u8, 0, 0][..], &e].concat());
        }
    }
    for (target, n) in &c.count {
        println!("{target}: {n} seeds");
    }
}
