mod common;

use std::io::{Cursor, Write};

use chrono_format::model::Ratio;
use chrono_import::sigrok::{self, Skipped};
use common::{Oracle, drain, testdata, verge_csv};
use zip::ZipArchive;
use zip::write::{SimpleFileOptions, ZipWriter};

const FIXTURES: [&str; 6] = [
    "marker",
    "markers-1000",
    "demo-mixed",
    "demo-disabled",
    "demo-analog",
    "demo-logic",
];

/// Every channel, edge and value agrees with what sigrok-cli reads from the same file.
#[test]
fn agrees_with_libsigrok() {
    for name in FIXTURES {
        let oracle = Oracle::load(name);
        let import = sigrok::read(&testdata(&format!("{name}.sr"))).unwrap();
        let recording = import.recording;
        assert_eq!(
            recording.timebase,
            Ratio::integer(oracle.samplerate),
            "{name}"
        );
        let analog_len = oracle.analog.iter().map(|(_, v)| v.len() as i64).max();
        assert_eq!(
            recording.length,
            oracle.logic_samples.max(analog_len.unwrap_or(0)),
            "{name}: length"
        );
        let got = drain(recording);
        let names: Vec<_> = got.digital.iter().map(|d| d.0.as_str()).collect();
        let expected: Vec<_> = oracle.digital.iter().map(|d| d.0.as_str()).collect();
        assert_eq!(names, expected, "{name}: digital channels");
        for ((channel, initial, end, edges), (_, oracle_initial, oracle_edges)) in
            got.digital.iter().zip(&oracle.digital)
        {
            assert_eq!(initial, oracle_initial, "{name} {channel}: initial");
            assert_eq!(*end, oracle.logic_samples, "{name} {channel}: end");
            assert_eq!(edges, oracle_edges, "{name} {channel}: edges");
        }
        let names: Vec<_> = got.analog.iter().map(|a| a.0.as_str()).collect();
        let expected: Vec<_> = oracle.analog.iter().map(|a| a.0.as_str()).collect();
        assert_eq!(names, expected, "{name}: analog channels");
        for ((channel, values), (_, oracle_values)) in got.analog.iter().zip(&oracle.analog) {
            assert_eq!(values.len(), oracle_values.len(), "{name} {channel}: count");
            if let Some(i) = (0..values.len()).find(|&i| values[i] != oracle_values[i]) {
                panic!(
                    "{name} {channel}: sample {i} is {:#010x}, sigrok-cli reads {:#010x}",
                    values[i], oracle_values[i]
                );
            }
        }
    }
}

/// verge's CSV reaches the same capture by another path: values as verge computed them, and DIO
/// levels at each sample.
#[test]
fn agrees_with_verge_csv() {
    for name in ["marker", "markers-1000"] {
        let rows = verge_csv(&format!("{name}.csv"));
        let got = drain(
            sigrok::read(&testdata(&format!("{name}.sr")))
                .unwrap()
                .recording,
        );
        let analog = |unit: &str| &got.analog.iter().find(|a| a.0 == unit).unwrap().1;
        let (volts, milliamps) = (analog("V"), analog("mA"));
        assert_eq!(volts.len(), rows.len(), "{name}: samples");
        for (i, &(mv, na, _)) in rows.iter().enumerate() {
            // verge-probe's src/record.rs: `millivolts as f32 / 1e3`, `nanoamps as f32 / 1e6`.
            assert_eq!(volts[i], (mv as f32 / 1e3).to_bits(), "{name} V at {i}");
            assert_eq!(
                milliamps[i],
                (na as f32 / 1e6).to_bits(),
                "{name} mA at {i}"
            );
        }
        for (pin, channel) in ["DIO0", "DIO1", "DIO2", "DIO3", "DIO_GAP"]
            .iter()
            .enumerate()
        {
            let (_, initial, _, edges) = got.digital.iter().find(|d| d.0 == *channel).unwrap();
            let mut level = *initial;
            let mut edges = edges.iter().peekable();
            let mut differ = Vec::new();
            for (i, row) in rows.iter().enumerate() {
                if edges.next_if_eq(&&(i as i64)).is_some() {
                    level = !level;
                }
                if row.2[pin] != level {
                    differ.push(i);
                }
            }
            // verge's two exports disagree here: the .sr marks the DIO levels unknown at the
            // first sample, the CSV does not. sigrok-cli reads the .sr as this reader does.
            let expected: &[usize] = if (name, *channel) == ("marker", "DIO_GAP") {
                &[0]
            } else {
                &[]
            };
            assert_eq!(
                differ, expected,
                "{name} {channel}: samples that differ from the CSV"
            );
        }
    }
}

#[test]
fn verge_metadata() {
    let import = sigrok::read(&testdata("markers-1000.sr")).unwrap();
    let source = import.recording.source;
    assert_eq!(source.format, "sigrok-sr");
    assert_eq!(source.files, ["markers-1000.sr"]);
    let extra: Vec<_> = source
        .extra
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        extra,
        [
            ("member markers.csv", "309 bytes, not read"),
            ("sigrok version", "0.5.2"),
            ("verge capture origin ns", "500000"),
        ]
    );
    assert!(import.skipped.is_empty(), "{:?}", import.skipped);
}

/// libsigrok's demo driver packs the enabled channels' bits together, but its srzip output names
/// them by channel index. The bits named D5 and D10 never change, and two unnamed bits carry the
/// data; the reader reads the names as libsigrok does and reports the unnamed bits.
#[test]
fn disabled_channels_are_reported() {
    let import = sigrok::read(&testdata("demo-disabled.sr")).unwrap();
    let toggles = |probe: u32| Skipped {
        name: format!("probe{probe}"),
        reason: format!("no probe{probe} key, so libsigrok treats it as disabled; its bit toggles"),
    };
    let zero = |probe: u32| Skipped {
        name: format!("probe{probe}"),
        reason: format!(
            "no probe{probe} key, so libsigrok treats it as disabled; its bit is always 0"
        ),
    };
    let mut expected = vec![toggles(1), toggles(3)];
    expected.extend([4, 5, 7, 8, 9, 10, 12].map(zero));
    assert_eq!(import.skipped, expected);
}

#[test]
fn opens_twice() {
    let path = testdata("demo-mixed.sr");
    let first = drain(sigrok::read(&path).unwrap().recording);
    let second = drain(sigrok::read(&path).unwrap().recording);
    assert_eq!(first.digital, second.digital);
    assert_eq!(first.analog, second.analog);
}

fn archive(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in members {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

const META: &str = "[global]\nsigrok version=0.5.2\n\n[device 1]\ncapturefile=logic-1\n\
                    total probes=3\nsamplerate=1 MHz\ntotal analog=1\nprobe1=A\nprobe3=C\n\
                    analog4=V\nunitsize=1\n";

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn read(members: &[(&str, &[u8])]) -> Result<sigrok::Import, sigrok::Error> {
    sigrok::read_bytes(archive(members), "test.sr")
}

/// Logic and analog chunks of different lengths, written by hand with expected values by hand.
#[test]
fn chunks_need_not_align() {
    let a = floats(&[0.5, 1.5, -2.0]);
    let b = floats(&[3.25, 0.0]);
    let import = read(&[
        ("version", b"2"),
        ("metadata", META.as_bytes()),
        ("logic-1-1", &[0b000, 0b001]),
        ("logic-1-2", &[0b101, 0b100, 0b001, 0b000]),
        ("analog-1-4-1", &a),
        ("analog-1-4-2", &b),
    ])
    .unwrap();
    assert_eq!(import.recording.timebase, Ratio::integer(1_000_000));
    assert_eq!(import.recording.length, 6);
    let got = drain(import.recording);
    assert_eq!(
        got.digital,
        [
            ("A".to_owned(), false, 6, vec![1, 3, 4, 5]),
            ("C".to_owned(), false, 6, vec![2, 4]),
        ]
    );
    let values: Vec<u32> = [0.5f32, 1.5, -2.0, 3.25, 0.0].map(f32::to_bits).to_vec();
    assert_eq!(got.analog, [("V".to_owned(), values)]);
    assert_eq!(
        import.skipped,
        [Skipped {
            name: "probe2".to_owned(),
            reason: "no probe2 key, so libsigrok treats it as disabled; its bit is always 0"
                .to_owned()
        }]
    );
}

#[test]
fn unchunked_members() {
    let import = read(&[
        ("version", b"1"),
        ("metadata", META.as_bytes()),
        ("logic-1", &[0b011, 0b010, 0b011]),
        ("analog-1-4", &floats(&[1.0])),
    ])
    .unwrap();
    assert_eq!(
        import.skipped,
        [Skipped {
            name: "probe2".to_owned(),
            reason: "no probe2 key, so libsigrok treats it as disabled; its bit is always 1"
                .to_owned()
        }]
    );
    let got = drain(import.recording);
    assert_eq!(
        got.digital,
        [
            ("A".to_owned(), true, 3, vec![1, 2]),
            ("C".to_owned(), false, 3, vec![]),
        ]
    );
}

fn refused(members: &[(&str, &[u8])], expect: &str) {
    match read(members) {
        Ok(_) => panic!("accepted; expected an error containing {expect:?}"),
        Err(e) => assert!(e.to_string().contains(expect), "{e}"),
    }
}

fn with_meta(meta: &str) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("version", b"2".to_vec()),
        ("metadata", meta.as_bytes().to_vec()),
        ("logic-1-1", vec![0; 4]),
        ("analog-1-4-1", vec![0; 16]),
    ]
}

fn refused_meta(meta: &str, expect: &str) {
    let members = with_meta(meta);
    let members: Vec<(&str, &[u8])> = members.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    refused(&members, expect);
}

#[test]
fn refuses_malformed_files() {
    refused(&[("metadata", META.as_bytes())], "no `version` member");
    refused(
        &[("version", b"3"), ("metadata", META.as_bytes())],
        "version \"3\"",
    );
    refused(&[("version", b"2")], "no `metadata` member");
    refused(
        &[
            ("version", b"2"),
            ("metadata", META.as_bytes()),
            ("logic-1-1", &[0]),
            ("logic-1-3", &[0]),
            ("analog-1-4-1", &floats(&[0.0])),
        ],
        "chunks stop at 1",
    );
    refused(
        &[
            ("version", b"2"),
            ("metadata", META.as_bytes()),
            ("logic-1", &[0]),
            ("logic-1-1", &[0]),
            ("analog-1-4-1", &floats(&[0.0])),
        ],
        "both `logic-1` and chunks",
    );
    refused(
        &[
            ("version", b"2"),
            ("metadata", META.as_bytes()),
            ("logic-1-1", &[0]),
        ],
        "no `analog-1-4` or `analog-1-4-1` member",
    );
    refused(
        &[
            ("version", b"2"),
            ("metadata", META.as_bytes()),
            ("logic-1-1", &[0]),
            ("analog-1-4-1", &[0; 6]),
        ],
        "6 bytes is not a whole number of 4-byte samples",
    );

    refused_meta(
        &META.replace("1 MHz", "1.5 Hz"),
        "not a whole number of hertz",
    );
    refused_meta(&META.replace("1 MHz", "1 mHz"), "refusing the ambiguity");
    refused_meta(&META.replace("samplerate=1 MHz\n", ""), "no samplerate");
    refused_meta(&META.replace("unitsize=1", "unitsize=0"), "unitsize 0");
    refused_meta(
        &META.replace("total probes=3", "total probes=9"),
        "unitsize 1 does not hold 9",
    );
    refused_meta(
        &META.replace("probe3=C", "probe4=C"),
        "probe4, but total probes is 3",
    );
    refused_meta(
        &META.replace("analog4", "analog5"),
        "analog5, but analog channels are",
    );
    refused_meta(&META.replace("probe3=C", "probe3x=C"), "probe3x");
    refused_meta(
        &META.replace("probe3=C", "probe3=C\nprobe3=D"),
        "repeats `probe3`",
    );
    refused_meta(&META.replace("[device 1]", "[device 2]"), "[device 2]");
    refused_meta(&META.replace("probe1=A", "probe1=A\\q"), "invalid escape");
}

#[test]
fn metadata_escapes_follow_gkeyfile() {
    let meta = META.replace("probe1=A", "probe1=\\sA\\\\B #1; \"q\"");
    let members = with_meta(&meta);
    let members: Vec<(&str, &[u8])> = members.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    let got = drain(read(&members).unwrap().recording);
    assert_eq!(got.digital[0].0, " A\\B #1; \"q\"");
}

/// A flipped byte inside a deflated member is caught when the file is opened.
#[test]
fn refuses_corrupted_member() {
    let bytes = std::fs::read(testdata("markers-1000.sr")).unwrap();
    let mut zip = ZipArchive::new(Cursor::new(&bytes)).unwrap();
    let file = zip.by_name("analog-1-7-1").unwrap();
    let at = (file.data_start().unwrap() + file.compressed_size() / 2) as usize;
    drop(file);
    let mut corrupted = bytes.clone();
    corrupted[at] ^= 0x10;
    let error = sigrok::read_bytes(corrupted, "x.sr")
        .err()
        .expect("accepted a corrupted member");
    assert!(error.to_string().contains("analog-1-7-1"), "{error}");
}

/// A stored member has no deflate stream to fail, so only its CRC catches a flipped byte.
#[test]
fn refuses_stored_member_with_bad_crc() {
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        ("version", &b"2"[..]),
        ("metadata", META.as_bytes()),
        ("logic-1-1", &[0x5a; 64]),
        ("analog-1-4-1", &floats(&[1.0])),
    ] {
        zip.start_file(name, stored).unwrap();
        zip.write_all(bytes).unwrap();
    }
    let mut bytes = zip.finish().unwrap().into_inner();
    assert!(sigrok::read_bytes(bytes.clone(), "x.sr").is_ok());
    let at = bytes.windows(64).position(|w| w == [0x5a; 64]).unwrap() + 10;
    bytes[at] ^= 0x01;
    let error = sigrok::read_bytes(bytes, "x.sr")
        .err()
        .expect("accepted a bad CRC");
    assert!(error.to_string().contains("logic-1-1"), "{error}");
}

fn sigrok_cli(args: &[&str]) -> Vec<u8> {
    let out = std::process::Command::new("sigrok-cli")
        .args(args)
        .output()
        .expect("sigrok-cli runs");
    out.stdout
}

/// Each logic channel's levels as sigrok-cli's `-O bits` reads them.
fn sigrok_cli_bits(path: &std::path::Path) -> Vec<(String, Vec<bool>)> {
    let out = sigrok_cli(&["-i", path.to_str().unwrap(), "-O", "bits:width=0"]);
    String::from_utf8(out)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let (name, bits) = line.split_once(':')?;
            if bits.is_empty() || !bits.bytes().all(|b| b == b'0' || b == b'1' || b == b' ') {
                return None;
            }
            Some((
                name.to_owned(),
                bits.bytes()
                    .filter(|&b| b != b' ')
                    .map(|b| b == b'1')
                    .collect(),
            ))
        })
        .collect()
}

/// verge's larger exports, against sigrok-cli and against the CSV beside each.
#[test]
#[ignore = "reads exports outside the repository and runs sigrok-cli"]
fn verge_exports() {
    let root = std::path::PathBuf::from(
        std::env::var_os("TICKWAVE_MARKER_EVIDENCE").expect("TICKWAVE_MARKER_EVIDENCE is unset"),
    );
    let mut files = Vec::new();
    for dir in ["hardware-final", "hardware", "hardware-gui", "shots"] {
        for entry in std::fs::read_dir(root.join(dir)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "sr") {
                files.push(path);
            }
        }
    }
    files.sort();
    let bar = indicatif::ProgressBar::new(files.len() as u64);
    for path in files {
        let label = path.strip_prefix(&root).unwrap().display().to_string();
        bar.set_message(label.clone());
        let got = drain(sigrok::read(&path).unwrap().recording);
        let bits = sigrok_cli_bits(&path);
        assert_eq!(bits.len(), got.digital.len(), "{label}");
        for ((name, initial, end, edges), (cli_name, levels)) in got.digital.iter().zip(&bits) {
            assert_eq!(name, cli_name, "{label}");
            assert_eq!(*end as usize, levels.len(), "{label} {name}");
            assert_eq!(*initial, levels[0], "{label} {name}");
            let cli_edges: Vec<i64> = (1..levels.len())
                .filter(|&i| levels[i] != levels[i - 1])
                .map(|i| i as i64)
                .collect();
            assert_eq!(*edges, cli_edges, "{label} {name}");
        }
        let csv = path.with_extension("csv");
        let mut note = String::from("no CSV");
        if csv.exists() {
            let rows = verge_csv(csv.to_str().unwrap());
            let analog = |unit: &str| &got.analog.iter().find(|a| a.0 == unit).unwrap().1;
            assert_eq!(analog("V").len(), rows.len(), "{label}");
            for (i, &(mv, na, _)) in rows.iter().enumerate() {
                assert_eq!(analog("V")[i], (mv as f32 / 1e3).to_bits(), "{label} V {i}");
                assert_eq!(
                    analog("mA")[i],
                    (na as f32 / 1e6).to_bits(),
                    "{label} mA {i}"
                );
            }
            let mut differ = Vec::new();
            for (pin, channel) in ["DIO0", "DIO1", "DIO2", "DIO3", "DIO_GAP"]
                .iter()
                .enumerate()
            {
                let Some((_, initial, _, edges)) = got.digital.iter().find(|d| d.0 == *channel)
                else {
                    continue;
                };
                let mut level = *initial;
                let mut edges = edges.iter().peekable();
                for (i, row) in rows.iter().enumerate() {
                    if edges.next_if_eq(&&(i as i64)).is_some() {
                        level = !level;
                    }
                    if row.2[pin] != level {
                        differ.push(format!("{channel}@{i}"));
                    }
                }
            }
            note = format!("CSV values equal; DIO differs at {differ:?}");
        }
        bar.suspend(|| {
            println!(
                "{label}: {} digital, {} analog, {note}",
                got.digital.len(),
                got.analog.len()
            )
        });
        bar.inc(1);
    }
    bar.finish_and_clear();
}
