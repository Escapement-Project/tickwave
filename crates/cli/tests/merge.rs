//! `tickwave merge` and `tickwave verify` run as a user runs them, on the committed fixtures.

use std::path::PathBuf;
use std::process::Command;

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

#[test]
fn two_fixtures_merge_verify_and_describe_their_sources() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/fixtures");
    // identify-swd's sidecar includes the SWD decoder's annotations, which web/scripts/generate.ts
    // writes rather than commits: without them there is nothing of identify-swd's to carry.
    let generated = fixtures.join("identify-swd.swd.jsonl");
    if !generated.exists() {
        eprintln!(
            "skipped: {} is missing; run `npm run generate` in web/ first",
            generated.display()
        );
        return;
    }
    let (swd, energy) = (
        fixtures.join("identify-swd.tickwave"),
        fixtures.join("energy.tickwave"),
    );
    let dir = tempfile::tempdir().unwrap();
    let merged = dir.path().join("merged.tickwave");
    let (ok, _, err) = tickwave(&[
        "merge".as_ref(),
        swd.as_ref(),
        energy.as_ref(),
        "-o".as_ref(),
        merged.as_ref(),
        "--align".as_ref(),
        "2=1ms".as_ref(),
        "--level".as_ref(),
        "3".as_ref(),
    ]);
    assert!(ok, "{err}");
    // identify-swd's decoder's annotations come from its sidecar, the energy capture's markers
    // from its bundle, and the merge says how many of each.
    assert!(
        err.contains("input 1 (identify-swd): 687 generated annotations carried"),
        "{err}"
    );
    assert!(
        err.contains("input 2 (energy): 5 generated annotations carried"),
        "{err}"
    );
    let (ok, _, err) = tickwave(&[
        "verify".as_ref(),
        merged.as_ref(),
        "--against".as_ref(),
        swd.as_ref(),
        "--against".as_ref(),
        energy.as_ref(),
    ]);
    assert!(ok, "{err}");
    let (ok, _, err) = tickwave(&[
        "verify".as_ref(),
        merged.as_ref(),
        "--against".as_ref(),
        energy.as_ref(),
        "--against".as_ref(),
        swd.as_ref(),
    ]);
    assert!(!ok);
    assert!(err.contains("another order than the merge's"), "{err}");
    let (ok, out, err) = tickwave(&["info".as_ref(), merged.as_ref()]);
    assert!(ok, "{err}");
    // identify-swd's 1 GHz and the energy capture's 96 MHz: 12 GHz, factors 12 and 125. The
    // energy capture, 1 ms later, has its tick 0 at 12,000,000.
    assert!(out.contains("at 12000000000/1 ticks a second"), "{out}");
    assert!(
        out.contains("(12 of the recording's ticks each), its tick 0 at 0"),
        "{out}"
    );
    assert!(
        out.contains(
            "(125 of the recording's ticks each), its tick 0 at 12000000, aligned by 12000000"
        ),
        "{out}"
    );
    // The energy capture's title goes with its source, and its files with the bundle.
    assert!(
        out.contains("\n    from A synthetic energy capture\n"),
        "{out}"
    );
    assert!(
        out.contains("  bundled merged.verge.jsonl, 6 lines"),
        "{out}"
    );
    // A capture is converted first.
    let sr = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/import/demo-logic.sr");
    let (ok, _, err) = tickwave(&[
        "merge".as_ref(),
        sr.as_ref(),
        energy.as_ref(),
        "-o".as_ref(),
        merged.as_ref(),
    ]);
    assert!(!ok);
    assert!(
        err.contains("demo-logic.sr: it isn't a tickwave recording: convert a capture with"),
        "{err}"
    );
    assert_eq!(err.matches("demo-logic.sr").count(), 1, "{err}");
    let (ok, _, err) = tickwave(&[
        "merge".as_ref(),
        swd.as_ref(),
        "-o".as_ref(),
        merged.as_ref(),
    ]);
    assert!(!ok && err.contains("2 values required"), "{err}");
}
