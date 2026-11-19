//! What a later version may put in a file, against the tools that read one and the tools that
//! write one again (FORMAT.md, "Preamble" and "Records"): a later minor version, an auxiliary
//! record (kinds 192 to 255) and the header's `auxiliary` object, and an optional record (kinds
//! 128 to 191). Readers take each. Re-cutting, repair, merging and bundling refuse a later minor
//! version and an optional record, and drop an auxiliary record and the `auxiliary` object.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::process::Command;
use std::sync::Arc;

use escapement_tickwave_format::bundle::bundle;
use escapement_tickwave_format::decode::{Footer, Index, from_memory};
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::{
    Declaration, DeclaredChannel, DeclaredKind, SourceHeader,
};
use escapement_tickwave_format::model::{
    Channel, ChannelData, Digital, Ratio, Recording, Shaping, Source, SourceFormat,
};
use escapement_tickwave_format::reader::{Ending, Event, Reader};
use escapement_tickwave_format::record::{self, Rewrite};
use escapement_tickwave_format::recut::{RecutError, Recutter};
use escapement_tickwave_format::repair::{RepairError, Repairer};
use escapement_tickwave_format::verify::verify;
use escapement_tickwave_format::writer::{WriteOptions, Writer};
use escapement_tickwave_import::convert::{Pass, Progress, Step};
use escapement_tickwave_import::merge::{self, Input};
use escapement_tickwave_import::recording::read;

struct Quiet;

impl Progress for Quiet {
    fn start(&self, _: Pass<'_>, _: u64) -> Step {
        Arc::new(|_| {})
    }
}

fn recording(name: &str, timebase: u64) -> Recording {
    let timebase = Ratio::integer(timebase).unwrap();
    Recording {
        timebase,
        length: 1_000,
        channels: vec![Channel {
            name: name.into(),
            source_name: name.to_lowercase(),
            source: 0,
            data: ChannelData::Digital(Digital {
                initial: false,
                end: 1_000,
                count: 3,
                edges: Box::new([100, 200, 300].into_iter()),
                gaps: Vec::new(),
            }),
            shaping: Shaping::default(),
        }],
        derived: Vec::new(),
        sources: vec![Source::new(SourceFormat::Other("t".into()), timebase)],
    }
}

fn plain() -> Vec<u8> {
    encode(recording("D", 1_000_000), &Options::default()).unwrap()
}

/// A record of `kind`, which this version doesn't define, its check right.
fn extension(kind: u8) -> Vec<u8> {
    let payload = [1u8, 2, 3];
    let mut out = vec![kind];
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    let check = blake3::Hasher::new()
        .update(&[kind])
        .update(&(payload.len() as u32).to_le_bytes())
        .update(&payload)
        .finalize();
    out.extend_from_slice(&check.as_bytes()[..8]);
    out
}

/// `file`, which holds no bundle, with a record of `kind` just before its footer, and an
/// `auxiliary` object in its footer's header where `auxiliary` says. Every place the footer gives
/// lies before the record.
fn with_record(file: &[u8], kind: u8, auxiliary: bool) -> Vec<u8> {
    let footer = Footer::parse_file(file).unwrap();
    assert!(footer.bundle.is_none());
    let at = footer.trailer.footer_offset as usize;
    let mut payload = file[at + 5..file.len() - 24 - 8].to_vec();
    if auxiliary {
        let header_len = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
        let mut json = payload[8..8 + header_len].to_vec();
        let close = json.pop().unwrap();
        assert_eq!(close, b'}');
        json.extend_from_slice(br#","auxiliary":{"cache":{"at":12}}}"#);
        let mut new = (json.len() as u32).to_le_bytes().to_vec();
        new.extend_from_slice(&payload[4..8]);
        new.extend_from_slice(&json);
        new.extend_from_slice(&payload[8 + header_len..]);
        payload = new;
    }
    let mut out = file[..at].to_vec();
    out.extend(extension(kind));
    record::write_footer(&mut out, 0, &payload);
    out
}

/// `file` stating minor version `minor` in its preamble and its trailer.
fn with_minor(file: &[u8], minor: u16) -> Vec<u8> {
    let mut out = file.to_vec();
    out[10..12].copy_from_slice(&minor.to_le_bytes());
    let t = out.len() - 24;
    out[t + 14..t + 16].copy_from_slice(&minor.to_le_bytes());
    out
}

/// Every reader's verdict, `Ok` where each takes the file: in order, at random, and `verify`
/// against what it reads at random. The kinds from 128 up the reader in order skipped.
fn read_by_all(file: &[u8]) -> Vec<u8> {
    let mut r = Reader::default();
    r.feed(file);
    let mut skipped = Vec::new();
    while let Some(e) = r.next_event().unwrap() {
        if let Event::Skipped { kind } = e {
            skipped.push(kind);
        }
    }
    assert_eq!(r.end().unwrap(), Ending::Complete);
    Index::of_file(file).unwrap();
    let (_, source) = read(Arc::new(file.to_vec())).unwrap();
    verify(file, source).unwrap();
    skipped
}

fn recut(file: &[u8]) -> Result<Vec<u8>, RecutError> {
    let mut r = Recutter::new(WriteOptions::default());
    r.feed(file)?;
    let mut out = r.take_output();
    out.extend(r.finish()?.tail);
    Ok(out)
}

/// `file` cut just before its footer, and repaired.
fn repaired(file: &[u8]) -> Result<Vec<u8>, RepairError> {
    let cut = &file[..Footer::parse_file(file).unwrap().trailer.footer_offset as usize];
    let mut r = Repairer::new(3);
    r.feed(cut)?;
    let repaired = r.finish()?;
    Ok([&cut[..repaired.keep as usize], &repaired.tail].concat())
}

/// `file` merged with another recording, at 2 MHz.
fn merged(file: &[u8]) -> Result<Vec<u8>, merge::Error> {
    let other = encode(recording("E", 2_000_000), &Options::default()).unwrap();
    let open = || -> Result<Vec<Input>, merge::Error> {
        Ok(vec![
            Input::read(Arc::new(file.to_vec()), "a")?,
            Input::read(Arc::new(other.clone()), "b")?,
        ])
    };
    let settings = merge::Settings {
        inputs: vec![merge::InputSettings::default(); 2],
        level: 3,
        ..merge::Settings::default()
    };
    let mut out = Cursor::new(Vec::new());
    merge::merge(&open, &settings, &Quiet, &mut out)?;
    Ok(out.into_inner())
}

/// `file` bundled with a hand-written file of no annotation.
fn bundled(file: &[u8]) -> Result<Vec<u8>, RecutError> {
    let footer = Footer::parse_file(file).unwrap();
    let hand = format!(
        "format = \"tickwave-annotations/1\"\nrecording_id = \"{}\"\n",
        footer.header.content_id
    );
    let files = BTreeMap::from([("rec.annotations.toml".to_owned(), hand)]);
    bundle(file, &footer, files, 3)
}

/// A copy holds no record from 128 up and no `auxiliary` object, and reads in order.
fn clean(copy: &[u8]) -> Footer {
    let footer = Footer::parse_file(copy).unwrap();
    assert_eq!(footer.records_from_128(from_memory(copy)).unwrap(), []);
    assert_eq!(footer.header.auxiliary, None);
    assert_eq!(read_by_all(copy), Vec::<u8>::new());
    footer
}

#[test]
fn an_auxiliary_record_and_object_are_read_past_and_dropped_by_every_rewriting_tool() {
    let plain = plain();
    let file = with_record(&plain, 200, true);
    assert_eq!(read_by_all(&file), [200]);
    let footer = Footer::parse_file(&file).unwrap();
    assert!(footer.header.auxiliary.is_some());
    let id = footer.header.content_id;
    assert_eq!(id, Footer::parse_file(&plain).unwrap().header.content_id);
    assert_eq!(clean(&recut(&file).unwrap()).header.content_id, id);
    // Cut before its footer, the record follows the last pages, where the finish began: repair
    // leaves it out with them.
    assert_eq!(clean(&repaired(&file).unwrap()).header.content_id, id);
    // The record lies among the data bundling keeps: the copy is re-cut.
    assert_eq!(clean(&bundled(&file).unwrap()).header.content_id, id);
    let merged = merged(&file).unwrap();
    clean(&merged);
    assert_eq!(merged, self::merged(&plain).unwrap());
}

/// A cut-off file with an auxiliary record before its latest known-through record: repair keeps
/// the bytes before the cut, so it refuses the file, and a re-cut writes it again without the
/// record. `tickwave repair` re-cuts it.
#[test]
fn repair_refuses_an_auxiliary_record_it_would_keep_and_tickwave_repair_re_cuts() {
    let d = Declaration {
        timebase: Ratio::integer(1_000_000).unwrap(),
        channels: vec![DeclaredChannel {
            name: "D".into(),
            source_name: "d".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
            overview_shift: None,
            codec: None,
            requires: Vec::new(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("t".into()),
            Ratio::integer(1_000_000).unwrap(),
        )],
        title: None,
        meta: Default::default(),
        overview_shift: 4,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    w.edge(0, 5).unwrap();
    w.flush(50).unwrap();
    let mut cut = w.take_output();
    let at = cut.len() as u64;
    // A cut-off file has no page yet, so nothing it holds places what follows.
    cut.extend(extension(200));
    w.edge(0, 60).unwrap();
    w.flush(80).unwrap();
    cut.extend(w.take_output());
    let mut r = Repairer::new(3);
    r.feed(&cut).unwrap();
    match r.finish() {
        Err(RepairError::AuxiliaryKept { kind: 200, at: a }) => assert_eq!(a, at),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("repaired"),
    }
    let copy = recut(&cut).unwrap();
    assert_eq!(clean(&copy).header.length, 80);
    let dir = tempfile::tempdir().unwrap();
    let (input, output) = (
        dir.path().join("cut.tickwave"),
        dir.path().join("rec.tickwave"),
    );
    std::fs::write(&input, &cut).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tickwave"))
        .args([
            "repair".as_ref(),
            input.as_os_str(),
            "-o".as_ref(),
            output.as_os_str(),
        ])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains("re-cut, as an auxiliary record, of kind 200, lies before the cut"),
        "{said}"
    );
    let repaired = std::fs::read(&output).unwrap();
    assert_eq!(
        clean(&repaired).header.content_id,
        clean(&copy).header.content_id
    );
}

#[test]
fn an_optional_record_is_read_past_and_refused_by_every_rewriting_tool() {
    let file = with_record(&plain(), 130, false);
    assert_eq!(read_by_all(&file), [130]);
    let refusal = |doing| Rewrite::Optional { kind: 130, doing }.to_string();
    assert_eq!(recut(&file).unwrap_err().to_string(), refusal("re-cutting"));
    assert_eq!(
        repaired(&file).unwrap_err().to_string(),
        refusal("repairing")
    );
    assert_eq!(bundled(&file).unwrap_err().to_string(), refusal("bundling"));
    assert_eq!(
        merged(&file).unwrap_err().to_string(),
        format!("a: {}", refusal("merging"))
    );
    assert_eq!(
        refusal("re-cutting"),
        "the file holds an optional record of kind 130, which this version doesn't know: \
         re-cutting would drop it"
    );
}

#[test]
fn a_later_minor_version_is_read_and_refused_by_every_rewriting_tool() {
    let file = with_minor(&plain(), 1);
    assert_eq!(read_by_all(&file), Vec::<u8>::new());
    let refusal = |doing| Rewrite::Newer { minor: 1, doing }.to_string();
    assert_eq!(recut(&file).unwrap_err().to_string(), refusal("re-cutting"));
    assert_eq!(
        repaired(&file).unwrap_err().to_string(),
        refusal("repairing")
    );
    assert_eq!(bundled(&file).unwrap_err().to_string(), refusal("bundling"));
    assert_eq!(
        merged(&file).unwrap_err().to_string(),
        format!("a: {}", refusal("merging"))
    );
    assert_eq!(
        refusal("bundling"),
        "the file is format 1.1, newer than this tool's 1.0: bundling would drop what it adds"
    );
    // Its own minor version and earlier are written again.
    recut(&with_minor(&plain(), 0)).unwrap();
}
