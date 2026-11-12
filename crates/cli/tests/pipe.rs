//! tickwave's output read through a pipe by a program that may stop reading early.

use std::path::PathBuf;
use std::process::{Command, Stdio};

#[test]
fn a_reader_that_stops_early_ends_the_output_quietly() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/fixtures");
    let recording = fixtures.join("identify-swd.tickwave");
    let sidecar = fixtures.join("identify-swd.annotations.toml");
    let runs: [&[&std::ffi::OsStr]; 3] = [
        &["info".as_ref(), recording.as_ref()],
        &["info".as_ref(), recording.as_ref(), "--json".as_ref()],
        &[
            "annotations".as_ref(),
            "check".as_ref(),
            recording.as_ref(),
            sidecar.as_ref(),
            "--json".as_ref(),
        ],
    ];
    for args in runs {
        // The reading end closes before tickwave starts, so its first write finds the pipe closed.
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let out = Command::new(env!("CARGO_BIN_EXE_tickwave"))
            .args(args)
            .stdout(writer)
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{args:?}: {:?}, {stderr}", out.status);
        assert_eq!(stderr, "", "{args:?}");
    }
}
