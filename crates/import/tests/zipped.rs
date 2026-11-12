//! Reading zips as the tools people use write them: web/fixtures/zip/make.sh's.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use escapement_tickwave_import::input::Input;
use escapement_tickwave_import::zipped::{Data, Error, Limits, Zipped};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/fixtures")
}

fn zip(name: &str) -> Vec<u8> {
    fs::read(fixtures().join("zip").join(name)).unwrap()
}

/// What the page opens.
fn opened(name: &str) -> bool {
    [".tickwave", ".toml", ".jsonl", ".sr", ".vcd", ".bin"]
        .iter()
        .any(|end| name.ends_with(end))
}

fn open(bytes: Vec<u8>) -> Result<Zipped, Error> {
    Zipped::open(Arc::new(bytes), &opened, Limits::default())
}

/// Each file taken, by name, with its bytes: a stored one's sliced from the zip, a deflated one
/// unzipped.
fn unzipped(bytes: Vec<u8>) -> Vec<(String, Vec<u8>)> {
    let input: Arc<dyn Input> = Arc::new(bytes.clone());
    let mut zipped = Zipped::open(input, &opened, Limits::default()).unwrap();
    let entries = zipped.entries().to_vec();
    entries
        .iter()
        .map(|e| {
            let data = match e.data {
                Data::Stored { start } => bytes[start as usize..(start + e.size) as usize].to_vec(),
                Data::Deflated => {
                    let mut out = Vec::new();
                    zipped
                        .inflate(e, &mut |chunk| out.extend_from_slice(chunk))
                        .unwrap();
                    out
                }
            };
            (e.name.clone(), data)
        })
        .collect()
}

fn recording() -> Vec<u8> {
    fs::read(fixtures().join("identify-swd.tickwave")).unwrap()
}

#[test]
fn a_recording_and_its_sidecar_come_out_as_they_went_in_from_every_writer() {
    for name in ["stored.zip", "deflated.zip", "streamed.zip", "macos.zip"] {
        let files = unzipped(zip(name));
        let names: Vec<_> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["identify-swd.tickwave", "identify-swd.annotations.toml"],
            "{name}"
        );
        assert!(files[0].1 == recording(), "{name}: the recording's bytes");
        let sidecar = String::from_utf8(files[1].1.clone()).unwrap();
        assert!(sidecar.contains("id = \"from-a-zip\""), "{name}: {sidecar}");
    }
}

#[test]
fn a_stored_file_starts_after_its_own_header_not_the_central_directorys_copy() {
    // Info-ZIP's local header carries 28 bytes of extra fields, its central entry 24: the slice
    // starts where the local header says.
    let bytes = zip("stored.zip");
    let zipped = open(bytes.clone()).unwrap();
    let e = &zipped.entries()[0];
    let Data::Stored { start } = e.data else {
        panic!("{e:?} is stored")
    };
    let local = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as u64;
    assert_eq!(&bytes[..4], b"PK\x03\x04");
    assert_eq!((local(26), local(28)), (21, 28));
    assert_eq!(start, 30 + 21 + 28);
}

#[test]
fn folders_flatten_and_macos_resource_forks_are_passed_over() {
    let zipped = open(zip("macos.zip")).unwrap();
    let paths: Vec<_> = zipped.entries().iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "identify-swd/identify-swd.tickwave",
            "identify-swd/identify-swd.annotations.toml"
        ]
    );
    let set = open(zip("set.zip")).unwrap();
    let names: Vec<_> = set.entries().iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "identify-jtag.tickwave",
            "identify-swd.tickwave",
            "identify-swd.annotations.toml"
        ]
    );
    let capture = unzipped(zip("capture.zip"));
    assert_eq!(capture[0].0, "demo-logic.vcd");
    let vcd = fs::read(fixtures().join("../../testdata/import/demo-logic.vcd")).unwrap();
    assert!(capture[0].1 == vcd);
}

#[test]
fn a_file_tickwave_cannot_unzip_refuses_the_zip_only_when_it_is_wanted() {
    let err = open(zip("encrypted.zip")).err().unwrap();
    assert_eq!(err.to_string(), "identify-swd.tickwave is encrypted");
    let err = open(zip("bzip2.zip")).err().unwrap();
    assert_eq!(
        err.to_string(),
        "demo-logic.vcd is compressed with Bzip2, and tickwave reads only stored and deflated files"
    );
    let err = open(zip("duplicate.zip")).err().unwrap();
    assert_eq!(
        err.to_string(),
        "a/identify-swd.tickwave and b/identify-swd.tickwave share a name, so either could be the \
         one meant"
    );
    // Files the page doesn't open are passed over, however they are stored.
    let none = |_: &str| false;
    for name in ["encrypted.zip", "bzip2.zip", "duplicate.zip"] {
        let zipped = Zipped::open(Arc::new(zip(name)), &none, Limits::default()).unwrap();
        assert!(zipped.entries().is_empty(), "{name}");
    }
}

#[test]
fn a_zip_over_its_limits_is_refused_before_anything_is_unzipped() {
    let entries = Limits {
        entries: 3,
        ..Limits::default()
    };
    let err = Zipped::open(Arc::new(zip("set.zip")), &opened, entries)
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "it holds 4 entries, more than the 3 tickwave reads"
    );
    // deflated.zip's two files unzip to 5,568 bytes, as `unzip -l` totals them.
    let total = 5_568;
    let at = |inflated| Limits {
        inflated,
        ..Limits::default()
    };
    assert!(Zipped::open(Arc::new(zip("deflated.zip")), &opened, at(total)).is_ok());
    let err = Zipped::open(Arc::new(zip("deflated.zip")), &opened, at(total - 1))
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        format!(
            "its files would take {total} bytes unzipped, more than the {} tickwave unzips",
            total - 1
        )
    );
}

/// The central directory's entry for the file at `index`, as bytes into `zip`.
fn central(zip: &[u8], index: usize) -> usize {
    (0..zip.len() - 4)
        .filter(|&i| zip[i..i + 4] == *b"PK\x01\x02")
        .nth(index)
        .unwrap()
}

#[test]
fn a_file_whose_stated_sizes_lie_is_refused() {
    // A stored file that runs past the end of the zip.
    let mut bytes = zip("stored.zip");
    let at = central(&bytes, 0);
    for field in [20, 24] {
        bytes[at + field..at + field + 4].copy_from_slice(&0x7fff_0000u32.to_le_bytes());
    }
    let err = open(bytes).err().unwrap();
    assert_eq!(
        err.to_string(),
        "identify-swd.tickwave is cut short: the zip ends inside it"
    );

    // A deflated file that unzips to more than it states: nothing past the stated size comes out.
    let mut bytes = zip("deflated.zip");
    let at = central(&bytes, 1);
    bytes[at + 24..at + 28].copy_from_slice(&10u32.to_le_bytes());
    let mut zipped = open(bytes).unwrap();
    let e = zipped.entries()[1].clone();
    let mut out = Vec::new();
    assert!(
        zipped
            .inflate(&e, &mut |c| out.extend_from_slice(c))
            .is_err()
    );
    assert!(out.len() <= 10, "{} bytes came out", out.len());
}
