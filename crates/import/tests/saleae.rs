use std::fs;
use std::path::{Path, PathBuf};

use escapement_tickwave_format::model::{ChannelData, Ratio, Samples};
use escapement_tickwave_import::saleae::{self, Error};

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
    let rec = saleae::read(Path::new(DIR), &names()).unwrap();
    assert_eq!(rec.timebase, Ratio::integer(1_000_000_000));
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
    let err = saleae::read(&dir, &names()).unwrap_err();
    assert!(matches!(err, Error::OffGrid { index: 100, .. }), "{err}");
}

#[test]
fn accepts_an_edge_just_inside_the_tolerance() {
    let dir = broken("near_grid", "digital_3_0.bin", |b| {
        let t = get_time(b, 100);
        set_time(b, 100, t + 0.005e-9);
    });
    saleae::read(&dir, &names()).unwrap();
}

#[test]
fn refuses_edges_out_of_order() {
    let dir = broken("out_of_order", "digital_3_1.bin", |b| {
        let (a, c) = (get_time(b, 10), get_time(b, 11));
        set_time(b, 10, c);
        set_time(b, 11, a);
    });
    let err = saleae::read(&dir, &names()).unwrap_err();
    assert!(
        matches!(err, Error::NotIncreasing { index: 11, .. }),
        "{err}"
    );
}

#[test]
fn refuses_a_truncated_file() {
    let dir = broken("truncated", "digital_3_1.bin", |b| b.truncate(b.len() - 8));
    let err = saleae::read(&dir, &names()).unwrap_err();
    assert!(matches!(err, Error::Length { .. }), "{err}");
}

#[test]
fn refuses_channels_with_different_begins() {
    let dir = broken("begin", "digital_3_1.bin", |b| {
        let begin = f64::from_le_bytes(b[36..44].try_into().unwrap());
        b[36..44].copy_from_slice(&(begin + 1e-9).to_le_bytes());
    });
    let err = saleae::read(&dir, &names()).unwrap_err();
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
    let err = saleae::read(&dir, &names()).unwrap_err();
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
    let rec = saleae::read(Path::new(SWD50), &swd50_names()).unwrap();
    assert_eq!(rec.timebase, Ratio::integer(1_000_000_000));
    // The digital channel runs to 11.03 ms; the analog cut stops at 50 us.
    assert_eq!(rec.length, 11031886);
    assert_eq!(rec.source.begin_s, Some(-1.9995944403895518e-06));
    assert_eq!(
        rec.source.extra.get("trigger_s").map(String::as_str),
        Some("0")
    );
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
        assert_eq!(a.period, Ratio::integer(1));
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
    let rec = saleae::read(Path::new(SWD50), &names).unwrap();
    assert_eq!(rec.length, 50_000);
    assert_eq!(rec.source.begin_s, Some(-1.999594440162178e-06));
}

#[test]
fn refuses_analog_off_the_digital_grid() {
    let dir = broken_from(SWD50, "analog_off_grid", "analog_0.bin", |b| {
        let begin = f64::from_le_bytes(b[24..32].try_into().unwrap());
        b[24..32].copy_from_slice(&(begin + 0.4e-9).to_le_bytes());
    });
    let err = saleae::read(&dir, &swd50_names()).unwrap_err();
    assert!(matches!(err, Error::AnalogOffGrid { .. }), "{err}");
}

#[test]
fn refuses_a_downsampled_analog_file() {
    let dir = broken_from(SWD50, "downsample", "analog_1.bin", |b| {
        b[48..56].copy_from_slice(&2i64.to_le_bytes())
    });
    let err = saleae::read(&dir, &swd50_names()).unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err}");
}

#[test]
fn refuses_a_truncated_analog_file() {
    let dir = broken_from(SWD50, "analog_truncated", "analog_0.bin", |b| {
        b.truncate(b.len() - 4)
    });
    let err = saleae::read(&dir, &swd50_names()).unwrap_err();
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
    let rec = saleae::read(&dir, &names).unwrap();
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
