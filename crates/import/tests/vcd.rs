mod common;

use chrono_format::model::Ratio;
use chrono_import::sigrok;
use chrono_import::vcd::{self, Import, Skipped};
use common::{Oracle, drain, testdata};

/// sigrok-cli's VCD of each fixture, read by this reader, against the `.sr` read by the sigrok
/// reader and against sigrok-cli's own reading. sigrok-cli writes a time of
/// `round(sample / samplerate × timescale)`, so a VCD tick is that, not a sample number.
#[test]
fn sigrok_cli_vcd_matches_sr() {
    for (name, vcd_rate) in [
        ("marker", 1_000),
        ("markers-1000", 1_000),
        ("demo-mixed", 10_000_000),
        ("demo-logic", 10_000_000),
    ] {
        let oracle = Oracle::load(name);
        let to_vcd = |sample: i64| {
            let (num, den) = (sample as i128 * vcd_rate as i128, oracle.samplerate as i128);
            ((2 * num + den) / (2 * den)) as i64
        };
        let import = vcd::read(&testdata(&format!("{name}.vcd"))).unwrap();
        assert!(import.skipped.is_empty(), "{name}: {:?}", import.skipped);
        assert_eq!(
            import.recording.timebase,
            Ratio::integer(vcd_rate),
            "{name}"
        );
        assert_eq!(
            import.recording.length,
            to_vcd(oracle.logic_samples),
            "{name}"
        );
        assert_eq!(import.recording.source.begin_s, Some(0.0));
        let from_vcd = drain(import.recording).digital;
        let from_sr = drain(
            sigrok::read(&testdata(&format!("{name}.sr")))
                .unwrap()
                .recording,
        )
        .digital;
        assert_eq!(from_vcd.len(), oracle.digital.len(), "{name}");
        for ((vcd_name, initial, end, edges), (sr_name, sr_initial, sr_end, sr_edges)) in
            from_vcd.iter().zip(&from_sr)
        {
            assert_eq!(*vcd_name, format!("libsigrok.{sr_name}"));
            assert_eq!(initial, sr_initial, "{vcd_name}");
            assert_eq!(*end, to_vcd(*sr_end), "{vcd_name}");
            let mapped: Vec<i64> = sr_edges.iter().map(|&e| to_vcd(e)).collect();
            assert_eq!(*edges, mapped, "{name} {vcd_name}: against the .sr");
        }
        for ((vcd_name, _, _, edges), (_, _, oracle_edges)) in from_vcd.iter().zip(&oracle.digital)
        {
            let mapped: Vec<i64> = oracle_edges.iter().map(|&e| to_vcd(e)).collect();
            assert_eq!(
                *edges, mapped,
                "{name} {vcd_name}: against sigrok-cli -O bits"
            );
        }
    }
}

fn read(text: &str) -> Import {
    vcd::read_from(text.as_bytes(), "test.vcd").unwrap()
}

fn error(text: &str) -> String {
    match vcd::read_from(text.as_bytes(), "test.vcd") {
        Ok(_) => panic!("accepted"),
        Err(e) => e.to_string(),
    }
}

fn header(timescale: &str, vars: &str) -> String {
    format!(
        "$timescale {timescale} $end\n$scope module top $end\n{vars}$upscope $end\n$enddefinitions $end\n"
    )
}

const TWO: &str = "$var wire 1 ! a $end\n$var reg 1 \" b $end\n";

fn digital(import: Import) -> Vec<(String, bool, i64, Vec<i64>)> {
    drain(import.recording).digital
}

#[test]
fn timescales() {
    for (timescale, ratio) in [
        ("1 ns", Ratio::new(1_000_000_000, 1)),
        ("10 ps", Ratio::new(100_000_000_000, 1)),
        ("100 us", Ratio::new(10_000, 1)),
        ("1 s", Ratio::new(1, 1)),
        ("100 s", Ratio::new(1, 100)),
        ("10fs", Ratio::new(100_000_000_000_000, 1)),
    ] {
        let import = read(&format!("{}#0 0! 1\"\n#5\n", header(timescale, TWO)));
        assert_eq!(import.recording.timebase, ratio, "{timescale}");
    }
    assert!(
        error("$scope module top $end\n$upscope $end\n$enddefinitions $end\n#0\n")
            .contains("no $timescale")
    );
}

#[test]
fn edges_and_collapse() {
    let text = format!(
        "{}#0 0! 1\"\n#3 1!\n#7 0! 1! 0!\n#9 1! 0\" 1\"\n#12 0!\n#20\n",
        header("1 ns", TWO)
    );
    // a: 0, up at 3, three changes at 7 net down, up at 9, down at 12. b: 1, unchanged.
    assert_eq!(
        digital(read(&text)),
        [
            ("top.a".to_owned(), false, 20, vec![3, 7, 9, 12]),
            ("top.b".to_owned(), true, 20, vec![]),
        ]
    );
}

#[test]
fn x_and_z() {
    let vars = format!("{TWO}$var wire 1 # c $end\n$var wire 1 $ d $end\n");
    let text = format!(
        "{}#0 x! 0! x\" $dumpvars 1# z# 0$ $end\n#4 1\"\n#6 z$\n#8 0$ 1!\n#10\n",
        header("1 ns", &vars)
    );
    let import = read(&text);
    assert_eq!(
        import.skipped,
        [
            Skipped {
                name: "top.b".to_owned(),
                reason: "x at tick 0; the model holds only 0 and 1".to_owned()
            },
            Skipped {
                name: "top.c".to_owned(),
                reason: "z at tick 0; the model holds only 0 and 1".to_owned()
            },
            Skipped {
                name: "top.d".to_owned(),
                reason: "z at tick 6; the model holds only 0 and 1".to_owned()
            },
        ]
    );
    // a's x collapses into the 0 at the same time.
    assert_eq!(digital(import), [("top.a".to_owned(), false, 10, vec![8])]);
}

#[test]
fn late_values_and_offsets() {
    // Values before the first timestamp happen at time 0; b first gets a value at 15.
    let text = format!("{}0!\n#10 1!\n#15 1\"\n#30\n", header("1 us", TWO));
    let import = read(&text);
    assert_eq!(
        import.skipped,
        [Skipped {
            name: "top.b".to_owned(),
            reason: "no value until tick 15".to_owned()
        }]
    );
    assert_eq!(import.recording.length, 30);
    assert_eq!(digital(import), [("top.a".to_owned(), false, 30, vec![10])]);

    // A dump starting at 100 starts tick 0 there.
    let text = format!("{}#100 1! 1\"\n#110 0!\n#130\n", header("10 ns", TWO));
    let import = read(&text);
    assert_eq!(import.recording.length, 30);
    let begin = import.recording.source.begin_s.unwrap();
    assert!((begin - 1e-6).abs() < 1e-20, "{begin}");
    assert_eq!(import.recording.source.extra["start time"], "100");
    assert_eq!(digital(import)[0], ("top.a".to_owned(), true, 30, vec![10]));

    let text = format!("{}#0 1!\n", header("1 ns", TWO));
    assert_eq!(
        read(&text).skipped,
        [Skipped {
            name: "top.b".to_owned(),
            reason: "never given a value".to_owned()
        }]
    );
}

#[test]
fn vectors_and_reals_are_skipped() {
    let vars = "$var wire 8 # bus [7:0] $end\n$var real 64 % volts $end\n\
                $var integer 32 & count $end\n$var wire 1 ' bit [3] $end\n\
                $scope module inner $end\n$var wire 1 ( deep $end\n$var wire 1 ! alias $end\n\
                $upscope $end\n";
    let text = format!(
        "{}#0 0! b0 ' 1( b10100101 # r1.5 % b0 &\n#2 1! b1 ' r2.25 %\n#4\n",
        header("1 ns", &format!("{TWO}{vars}"))
    );
    let import = read(&text);
    assert_eq!(
        import.skipped,
        [
            Skipped {
                name: "top.bus[7:0]".to_owned(),
                reason: "8-bit wire: only 1-bit signals are read".to_owned()
            },
            Skipped {
                name: "top.volts".to_owned(),
                reason: "real: only 1-bit nets and regs are read".to_owned()
            },
            Skipped {
                name: "top.count".to_owned(),
                reason: "integer: only 1-bit nets and regs are read".to_owned()
            },
            Skipped {
                name: "top.b".to_owned(),
                reason: "never given a value".to_owned()
            },
        ]
    );
    assert_eq!(
        digital(import),
        [
            ("top.a".to_owned(), false, 4, vec![2]),
            ("top.bit[3]".to_owned(), false, 4, vec![2]),
            ("top.inner.deep".to_owned(), true, 4, vec![]),
            ("top.inner.alias".to_owned(), false, 4, vec![2]),
        ]
    );
}

#[test]
fn refuses_malformed_dumps() {
    let h = header("1 ns", TWO);
    assert!(error(&format!("{h}#5 0!\n#3 1!\n")).contains("time 3 after time 5"));
    assert!(error(&format!("{h}#0 0#\n")).contains("undeclared id `#`"));
    assert!(error(&format!("{h}#0 b01 !\n")).contains("2-bit value for 1-bit id `!`"));
    assert!(error(&format!("{h}#0 r1.0 !\n")).contains("non-binary value"));
    assert!(error(&format!("{h}#0 $var wire 1 # c $end\n")).contains("after $enddefinitions"));
    assert!(error(&format!("{h}#0 q!\n")).contains("line"));
}

/// sigrok-cli's VCD of each of verge's exports, read here, against the `.sr` read by the sigrok
/// reader, edge for edge. Their rates are decades, so a VCD tick is a sample.
#[test]
#[ignore = "reads exports outside the repository and runs sigrok-cli"]
fn verge_exports_through_sigrok_cli_vcd() {
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
        let out = std::process::Command::new("sigrok-cli")
            .args(["-i", path.to_str().unwrap(), "-O", "vcd"])
            .output()
            .expect("sigrok-cli runs");
        // sigrok-cli writes analog samples into the VCD as text lines, which are not VCD.
        let text: String = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|line| !line.ends_with(" V DC"))
            .map(|line| format!("{line}\n"))
            .collect();
        let import = vcd::read_from(text.as_bytes(), "x.vcd").unwrap();
        assert!(import.skipped.is_empty(), "{label}: {:?}", import.skipped);
        let sr = sigrok::read(&path).unwrap().recording;
        assert_eq!(import.recording.timebase, sr.timebase, "{label}");
        let from_vcd = drain(import.recording).digital;
        let from_sr = drain(sr).digital;
        assert_eq!(from_vcd.len(), from_sr.len(), "{label}");
        let mut edges = 0;
        for (v, s) in from_vcd.iter().zip(&from_sr) {
            assert_eq!(v.0, format!("libsigrok.{}", s.0), "{label}");
            assert_eq!((v.1, v.2, &v.3), (s.1, s.2, &s.3), "{label} {}", s.0);
            edges += v.3.len();
        }
        bar.suspend(|| println!("{label}: {} channels, {edges} edges agree", from_vcd.len()));
        bar.inc(1);
    }
    bar.finish_and_clear();
}
