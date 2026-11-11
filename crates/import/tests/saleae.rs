use std::fs;
use std::path::{Path, PathBuf};

use escapement_tickwave_format::model::{ChannelData, RateOrigin, Ratio, Samples, SourceFormat};
use escapement_tickwave_import::saleae::{self, Error, SampleRate};

fn rate(hz: u64) -> SampleRate {
    SampleRate::new(hz).unwrap()
}

const DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../testdata/saleae/identify-swd"
);

fn names() -> Vec<(String, String)> {
    [
        ("digital_3_0", "SWCLK"),
        ("digital_3_1", "SWDIO"),
        ("digital_3_2", "NC"),
    ]
    .iter()
    .map(|(s, n)| (s.to_string(), n.to_string()))
    .collect()
}

struct Expect {
    initial: bool,
    count: usize,
    end: i64,
    first: &'static [i64],
    last: i64,
    sum: i64,
    xor: i64,
}

// Computed with numpy from the same files, round((t - begin) * rate), independently of the reader.
const EXPECT: [Expect; 3] = [
    Expect {
        initial: true,
        count: 15286,
        end: 523583070,
        first: &[376719436, 376720063, 376720710],
        last: 389758629,
        sum: 5868433063009,
        xor: 26084423,
    },
    Expect {
        initial: true,
        count: 1874,
        end: 523583100,
        first: &[376845415, 376846682, 376851176],
        last: 389756645,
        sum: 719527514550,
        xor: 7039266,
    },
    Expect {
        initial: false,
        count: 0,
        end: 523583030,
        first: &[],
        last: 0,
        sum: 0,
        xor: 0,
    },
];

#[test]
fn reads_identify_swd_as_numpy_does() {
    let rec = saleae::read(Path::new(DIR), &names(), None).unwrap();
    assert_eq!(rec.timebase, Ratio::integer(1_000_000_000).unwrap());
    assert_eq!(rec.length, 523583100);
    assert_eq!(rec.source.begin_s, Some(0.07553184799994528));
    assert_eq!(rec.channels.len(), 3);
    for (channel, want) in rec.channels.into_iter().zip(&EXPECT) {
        let ChannelData::Digital(d) = channel.data else {
            panic!("{} is not digital", channel.name)
        };
        assert_eq!(d.initial, want.initial, "{}", channel.name);
        assert_eq!(d.end, want.end, "{}", channel.name);
        let edges: Vec<i64> = d.edges.collect();
        assert_eq!(edges.len(), want.count, "{}", channel.name);
        assert_eq!(&edges[..want.first.len()], want.first, "{}", channel.name);
        assert_eq!(
            edges.last().copied().unwrap_or(0),
            want.last,
            "{}",
            channel.name
        );
        assert_eq!(edges.iter().sum::<i64>(), want.sum, "{}", channel.name);
        assert_eq!(
            edges.iter().fold(0, |a, e| a ^ e),
            want.xor,
            "{}",
            channel.name
        );
    }
}

/// A copy of the identify-swd fixture in a fresh directory, with `edit` applied to one file.
fn broken(test: &str, file: &str, edit: impl FnOnce(&mut Vec<u8>)) -> PathBuf {
    broken_from(DIR, test, file, edit)
}

fn broken_from(src: &str, test: &str, file: &str, edit: impl FnOnce(&mut Vec<u8>)) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dir.join(entry.file_name())).unwrap();
    }
    let path = dir.join(file);
    let mut bytes = fs::read(&path).unwrap();
    edit(&mut bytes);
    fs::write(&path, bytes).unwrap();
    dir
}

fn time_offset(index: usize) -> usize {
    60 + 8 * index
}

fn get_time(bytes: &[u8], index: usize) -> f64 {
    let at = time_offset(index);
    f64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn set_time(bytes: &mut [u8], index: usize, t: f64) {
    let at = time_offset(index);
    bytes[at..at + 8].copy_from_slice(&t.to_le_bytes());
}

#[test]
fn refuses_an_edge_off_the_grid() {
    let dir = broken("off_grid", "digital_3_0.bin", |b| {
        let t = get_time(b, 100);
        set_time(b, 100, t + 0.4e-9);
    });
    let err = saleae::read(&dir, &names(), None).unwrap_err();
    assert!(matches!(err, Error::OffGrid { index: 100, .. }), "{err}");
}

#[test]
fn accepts_an_edge_just_inside_the_tolerance() {
    let dir = broken("near_grid", "digital_3_0.bin", |b| {
        let t = get_time(b, 100);
        set_time(b, 100, t + 0.005e-9);
    });
    saleae::read(&dir, &names(), None).unwrap();
}

#[test]
fn refuses_edges_out_of_order() {
    let dir = broken("out_of_order", "digital_3_1.bin", |b| {
        let (a, c) = (get_time(b, 10), get_time(b, 11));
        set_time(b, 10, c);
        set_time(b, 11, a);
    });
    let err = saleae::read(&dir, &names(), None).unwrap_err();
    assert!(
        matches!(err, Error::NotIncreasing { index: 11, .. }),
        "{err}"
    );
}

#[test]
fn refuses_a_truncated_file() {
    let dir = broken("truncated", "digital_3_1.bin", |b| b.truncate(b.len() - 8));
    let err = saleae::read(&dir, &names(), None).unwrap_err();
    assert!(matches!(err, Error::Length { .. }), "{err}");
}

#[test]
fn refuses_channels_with_different_begins() {
    let dir = broken("begin", "digital_3_1.bin", |b| {
        let begin = f64::from_le_bytes(b[36..44].try_into().unwrap());
        b[36..44].copy_from_slice(&(begin + 1e-9).to_le_bytes());
    });
    let err = saleae::read(&dir, &names(), None).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Mismatch {
                what: "begin time",
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn refuses_an_unknown_channel_type() {
    let dir = broken("kind", "digital_3_0.bin", |b| {
        b[12..16].copy_from_slice(&2i32.to_le_bytes())
    });
    let err = saleae::read(&dir, &names(), None).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err}");
}

const SWD50: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../testdata/saleae/swd50-cut"
);

fn swd50_names() -> Vec<(String, String)> {
    [
        ("digital_4_0", "NRST"),
        ("analog_0", "SWCLK"),
        ("analog_1", "SWDIO"),
    ]
    .iter()
    .map(|(s, n)| (s.to_string(), n.to_string()))
    .collect()
}

// f64, because every f32 widens to f64 exactly, so equality still checks every bit.
struct ExpectAnalog {
    first: f64,
    at_2000: f64,
    last: f64,
    bit_sum: u64,
}

// Computed with numpy from the same files, independently of the reader.
const EXPECT_ANALOG: [ExpectAnalog; 2] = [
    ExpectAnalog {
        first: 3.2742061614990234,
        at_2000: 2.09637451171875,
        last: 3.3015975952148438,
        bit_sum: 54999831885490,
    },
    ExpectAnalog {
        first: 3.2841920852661133,
        at_2000: 2.8150784969329834,
        last: 3.2643353939056396,
        bit_sum: 53956932916676,
    },
];

#[test]
fn reads_analog_beside_digital_as_numpy_does() {
    let rec = saleae::read(Path::new(SWD50), &swd50_names(), None).unwrap();
    assert_eq!(rec.timebase, Ratio::integer(1_000_000_000).unwrap());
    // The digital channel runs to 11.03 ms; the analog cut stops at 50 us.
    assert_eq!(rec.length, 11031886);
    assert_eq!(rec.source.begin_s, Some(-1.9995944403895518e-06));
    assert_eq!(rec.source.trigger_s, Some(0.0));
    let mut channels = rec.channels.into_iter();
    let ChannelData::Digital(nrst) = channels.next().unwrap().data else {
        panic!("NRST is not digital")
    };
    assert!(nrst.initial);
    assert_eq!(nrst.end, 11031886);
    assert_eq!(nrst.edges.count(), 0);
    for (channel, want) in channels.zip(&EXPECT_ANALOG) {
        let ChannelData::Analog(a) = channel.data else {
            panic!("{} is not analog", channel.name)
        };
        assert_eq!(a.unit, "V");
        assert_eq!(a.first, 0, "{}", channel.name);
        assert_eq!(a.period, Ratio::ONE);
        assert_eq!(a.count, 50_000);
        let Samples::F32(samples) = a.samples;
        let v: Vec<f32> = samples.collect();
        assert_eq!(v.len(), 50_000);
        assert_eq!(f64::from(v[0]), want.first, "{}", channel.name);
        assert_eq!(f64::from(v[2000]), want.at_2000, "{}", channel.name);
        assert_eq!(f64::from(v[49_999]), want.last, "{}", channel.name);
        let sum: u64 = v.iter().map(|x| u64::from(x.to_bits())).sum();
        assert_eq!(sum, want.bit_sum, "{}", channel.name);
    }
}

#[test]
fn analog_alone_starts_at_its_own_begin() {
    let names = vec![("analog_1".to_string(), "SWDIO".to_string())];
    let rec = saleae::read(Path::new(SWD50), &names, None).unwrap();
    assert_eq!(rec.length, 50_000);
    assert_eq!(rec.source.begin_s, Some(-1.999594440162178e-06));
}

#[test]
fn refuses_analog_off_the_digital_grid() {
    let dir = broken_from(SWD50, "analog_off_grid", "analog_0.bin", |b| {
        let begin = f64::from_le_bytes(b[24..32].try_into().unwrap());
        b[24..32].copy_from_slice(&(begin + 0.4e-9).to_le_bytes());
    });
    let err = saleae::read(&dir, &swd50_names(), None).unwrap_err();
    assert!(matches!(err, Error::AnalogOffGrid { .. }), "{err}");
}

#[test]
fn refuses_a_downsampled_analog_file() {
    let dir = broken_from(SWD50, "downsample", "analog_1.bin", |b| {
        b[48..56].copy_from_slice(&2i64.to_le_bytes())
    });
    let err = saleae::read(&dir, &swd50_names(), None).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err}");
}

#[test]
fn refuses_a_truncated_analog_file() {
    let dir = broken_from(SWD50, "analog_truncated", "analog_0.bin", |b| {
        b.truncate(b.len() - 4)
    });
    let err = saleae::read(&dir, &swd50_names(), None).unwrap_err();
    assert!(matches!(err, Error::Length { .. }), "{err}");
}

/// The whole of session50 (800 MB), when its export is on this machine.
#[test]
#[ignore]
fn reads_session50() {
    let dir = std::path::PathBuf::from(
        std::env::var_os("TICKWAVE_SESSION50").expect("TICKWAVE_SESSION50 is unset"),
    );
    let names: Vec<_> = [
        ("digital_4_0", "NRST"),
        ("analog_0", "SWCLK"),
        ("analog_1", "SWDIO"),
    ]
    .iter()
    .map(|(s, n)| (s.to_string(), n.to_string()))
    .collect();
    let rec = saleae::read(&dir, &names, None).unwrap();
    for channel in rec.channels {
        match channel.data {
            ChannelData::Digital(d) => assert_eq!(d.edges.count(), 0),
            ChannelData::Analog(a) => {
                let Samples::F32(samples) = a.samples;
                assert_eq!(a.first, 0);
                assert_eq!(samples.filter(|v| v.is_finite()).count(), 100_002_000);
            }
        }
    }
}

/// Rewrites a version 1 file as Logic 8, Logic Pro 8 and Logic Pro 16 write it, in version 0:
/// no chunk count, and no rate in a digital file or trigger in an analog one.
fn v1_to_v0(v1: &[u8]) -> Vec<u8> {
    let kind = i32::from_le_bytes(v1[12..16].try_into().unwrap());
    let mut v0 = v1[..8].to_vec();
    v0.extend(0i32.to_le_bytes());
    v0.extend(kind.to_le_bytes());
    match kind {
        // initial, begin, end, count, times
        0 => {
            v0.extend(&v1[24..28]);
            v0.extend(&v1[36..]);
        }
        // begin, rate as u64, downsample, count, samples
        1 => {
            v0.extend(&v1[24..32]);
            let rate = f64::from_le_bytes(v1[40..48].try_into().unwrap());
            v0.extend((rate as u64).to_le_bytes());
            v0.extend(&v1[48..]);
        }
        _ => panic!("type {kind}"),
    }
    v0
}

/// A version 0 copy of the export in `src`, in a fresh directory.
fn v0_copy(src: &str, test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let v1 = fs::read(entry.path()).unwrap();
        fs::write(dir.join(entry.file_name()), v1_to_v0(&v1)).unwrap();
    }
    dir
}

fn write_v0_digital(path: &Path, initial: u32, end: f64, times: &[f64]) {
    let mut b = b"<SALEAE>".to_vec();
    b.extend(0i32.to_le_bytes());
    b.extend(0i32.to_le_bytes());
    b.extend(initial.to_le_bytes());
    b.extend(0f64.to_le_bytes());
    b.extend(end.to_le_bytes());
    b.extend((times.len() as u64).to_le_bytes());
    for t in times {
        b.extend(t.to_le_bytes());
    }
    fs::write(path, b).unwrap();
}

/// What a recording holds, with its edges and samples read, to compare two.
#[derive(Debug, PartialEq)]
enum Read {
    Digital(bool, i64, Vec<i64>),
    Analog(i64, u64, Vec<u32>),
}

fn contents(rec: escapement_tickwave_format::model::Recording) -> (Ratio, i64, Vec<Read>) {
    let channels = rec
        .channels
        .into_iter()
        .map(|c| match c.data {
            ChannelData::Digital(d) => Read::Digital(d.initial, d.end, d.edges.collect()),
            ChannelData::Analog(a) => {
                let Samples::F32(s) = a.samples;
                Read::Analog(a.first, a.count, s.map(f32::to_bits).collect())
            }
        })
        .collect();
    (rec.timebase, rec.length, channels)
}

fn rate_from(rec: &escapement_tickwave_format::model::Recording) -> Option<RateOrigin> {
    rec.source.rate
}

#[test]
fn v0_infers_its_rate_and_reads_as_v1_does() {
    let dir = v0_copy(DIR, "v0_infer");
    let v0 = saleae::read(&dir, &names(), None).unwrap();
    assert_eq!(v0.source.format, SourceFormat::SaleaeBinary0);
    assert_eq!(rate_from(&v0), Some(RateOrigin::Inferred));
    let v1 = saleae::read(Path::new(DIR), &names(), None).unwrap();
    assert_eq!(v1.source.format, SourceFormat::SaleaeBinary1);
    assert_eq!(rate_from(&v1), None);
    assert_eq!(v0.source.begin_s, v1.source.begin_s);
    assert_eq!(contents(v0), contents(v1));
}

#[test]
fn v0_takes_a_given_rate() {
    let dir = v0_copy(DIR, "v0_given");
    let v0 = saleae::read(&dir, &names(), Some(rate(1_000_000_000))).unwrap();
    assert_eq!(rate_from(&v0), Some(RateOrigin::Given));
    let v1 = saleae::read(Path::new(DIR), &names(), None).unwrap();
    assert_eq!(contents(v0), contents(v1));
}

#[test]
fn v0_refuses_a_given_rate_its_edges_are_off() {
    let dir = v0_copy(DIR, "v0_given_low");
    let err = saleae::read(&dir, &names(), Some(rate(500_000_000))).unwrap_err();
    assert!(matches!(err, Error::OffGrid { .. }), "{err}");
}

#[test]
fn v0_infers_from_every_digital_file_in_the_export() {
    // digital_3_2 has no edges of its own; the rate comes from its neighbours'.
    let dir = v0_copy(DIR, "v0_pool");
    let names = vec![("digital_3_2".to_string(), "NC".to_string())];
    let rec = saleae::read(&dir, &names, None).unwrap();
    assert_eq!(rec.timebase, Ratio::integer(1_000_000_000).unwrap());
    assert_eq!(rate_from(&rec), Some(RateOrigin::Inferred));
}

#[test]
fn v0_inference_passes_over_files_that_are_no_channel() {
    let dir = v0_copy(DIR, "v0_strays");
    fs::write(dir.join("notes.bin"), b"not a capture").unwrap();
    fs::copy(
        Path::new(DIR).join("digital_3_0.bin"),
        dir.join("digital_9_9.bin"),
    )
    .unwrap();
    let rec = saleae::read(&dir, &names(), None).unwrap();
    assert_eq!(rec.timebase, Ratio::integer(1_000_000_000).unwrap());
}

#[test]
fn v0_infers_the_lowest_rate_its_edges_fit() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("v0_lowest");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let names = vec![("digital_0".to_string(), "A".to_string())];
    let at = |ticks: &[u64], rate: f64| ticks.iter().map(|&k| k as f64 / rate).collect::<Vec<_>>();
    for (ticks, rate, want) in [
        (&[3u64, 7, 10, 25, 1001][..], 50e6, 50_000_000),
        (&[2, 6, 10, 24, 1000], 50e6, 25_000_000),
        (&[1, 2, 4, 5, 3001], 1e9, 1_000_000_000),
    ] {
        write_v0_digital(&dir.join("digital_0.bin"), 0, 0.01, &at(ticks, rate));
        let rec = saleae::read(&dir, &names, None).unwrap();
        assert_eq!(
            rec.timebase,
            Ratio::integer(want).unwrap(),
            "{ticks:?} at {rate}"
        );
        let ChannelData::Digital(d) = rec.channels.into_iter().next().unwrap().data else {
            panic!("not digital")
        };
        let scale = want as f64 / rate;
        let edges: Vec<i64> = d.edges.collect();
        let expect: Vec<i64> = ticks.iter().map(|&k| (k as f64 * scale) as i64).collect();
        assert_eq!(edges, expect, "{ticks:?} at {rate}");
    }
}

#[test]
fn v0_refuses_to_infer_from_edges_no_divisor_of_1_ghz_fits() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("v0_no_grid");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // 3 MHz does not divide 1 GHz, and ticks that are no multiple of 3 fit no rate that does.
    let times: Vec<f64> = [1u64, 2, 4, 5, 3001]
        .iter()
        .map(|&k| k as f64 / 3e6)
        .collect();
    write_v0_digital(&dir.join("digital_0.bin"), 0, 0.01, &times);
    let names = vec![("digital_0".to_string(), "A".to_string())];
    let err = saleae::read(&dir, &names, None).unwrap_err();
    assert!(matches!(err, Error::NoRate(_)), "{err}");
    let rec = saleae::read(&dir, &names, Some(rate(3_000_000))).unwrap();
    assert_eq!(rec.timebase, Ratio::integer(3_000_000).unwrap());
}

#[test]
fn v0_with_no_edges_needs_a_rate() {
    let dir = v0_copy(SWD50, "v0_flat");
    let names = vec![("digital_4_0".to_string(), "NRST".to_string())];
    let err = saleae::read(&dir, &names, None).unwrap_err();
    assert!(matches!(err, Error::NoRate(_)), "{err}");
    let rec = saleae::read(&dir, &names, Some(rate(1_000_000_000))).unwrap();
    assert_eq!(rec.timebase, Ratio::integer(1_000_000_000).unwrap());
}

#[test]
fn v0_analog_gives_the_digital_channels_their_rate() {
    let dir = v0_copy(SWD50, "v0_analog");
    let v0 = saleae::read(&dir, &swd50_names(), None).unwrap();
    assert_eq!(rate_from(&v0), None);
    assert_eq!(v0.source.trigger_s, None);
    let v1 = saleae::read(Path::new(SWD50), &swd50_names(), None).unwrap();
    assert_eq!(contents(v0), contents(v1));
}

#[test]
fn refuses_a_given_rate_a_file_contradicts() {
    let dir = v0_copy(SWD50, "v0_contradicted");
    let err = saleae::read(&dir, &swd50_names(), Some(rate(500_000_000))).unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateDiffers { given, .. } if given.hz() == 500_000_000
        ),
        "{err}"
    );
}

#[test]
fn refuses_files_of_two_versions() {
    let dir = broken_from(SWD50, "mixed_versions", "analog_0.bin", |b| {
        *b = v1_to_v0(b);
    });
    let err = saleae::read(&dir, &swd50_names(), None).unwrap_err();
    assert!(
        matches!(
            err,
            Error::Mismatch {
                what: "export version",
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn refuses_a_truncated_v0_file() {
    let dir = v0_copy(DIR, "v0_truncated");
    let path = dir.join("digital_3_1.bin");
    let mut b = fs::read(&path).unwrap();
    b.truncate(b.len() - 8);
    fs::write(&path, b).unwrap();
    let err = saleae::read(&dir, &names(), None).unwrap_err();
    assert!(matches!(err, Error::Length { .. }), "{err}");
}

#[test]
fn names_a_channel_the_export_lacks() {
    let names = vec![("digital_7_7".to_string(), "X".to_string())];
    let err = saleae::read(Path::new(DIR), &names, None).unwrap_err();
    assert!(
        matches!(err, Error::NoFile(ref f) if f == "digital_7_7.bin"),
        "{err}"
    );
}

#[test]
fn parses_rates() {
    for (s, hz) in [
        ("50000000", 50_000_000),
        ("50M", 50_000_000),
        ("12.5M", 12_500_000),
        ("781.25k", 781_250),
        (" 2 k ", 2_000),
        ("1G", 1_000_000_000),
        ("1.000M", 1_000_000),
    ] {
        assert_eq!(s.parse::<SampleRate>().map(SampleRate::hz), Ok(hz), "{s:?}");
    }
    for s in [
        "",
        "M",
        "0",
        "1.5",
        "50m",
        "50 MHz",
        "-5",
        "1e6",
        "9007199254740992",
        ".5M",
    ] {
        assert!(s.parse::<SampleRate>().is_err(), "{s:?}");
    }
}

/// The version 0 exports in the project's notes, captured at 50 MS/s, when they are on this
/// machine.
#[test]
#[ignore]
fn infers_50_mhz_for_the_notes_v0_exports() {
    let root = std::path::PathBuf::from(
        std::env::var_os("TICKWAVE_SALEAE_V0").expect("TICKWAVE_SALEAE_V0 is unset"),
    );
    for (dir, channels) in [
        ("oscan5/ground", 0..4),
        ("dio/coupling-1", 4..8),
        ("dio/loop-1", 4..8),
    ] {
        let dir = root.join(dir);
        let names: Vec<_> = channels
            .map(|n| (format!("digital_{n}"), format!("D{n}")))
            .collect();
        let rec = saleae::read(&dir, &names, None).unwrap();
        assert_eq!(
            rec.timebase,
            Ratio::integer(50_000_000).unwrap(),
            "{}",
            dir.display()
        );
        let err = saleae::read(&dir, &names, Some(rate(25_000_000))).unwrap_err();
        assert!(
            matches!(err, Error::OffGrid { .. }),
            "{}: {err}",
            dir.display()
        );
    }
}
