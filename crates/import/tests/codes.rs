mod common;

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use common::{testdata, verge_csv};
use escapement_tickwave_format::model::{ChannelData, Samples};
use escapement_tickwave_import::codes::{Codes, recover_codes};
use escapement_tickwave_import::sigrok;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

#[test]
fn hand_worked() {
    // Steps of 0.5 from 0: codes 0, 1, 2, 4.
    assert_eq!(
        recover_codes([1.0f32, 0.0, 2.0, 0.5, 1.0]),
        Some(Codes {
            bits: 3,
            scale: 0.5,
            offset: 0.0
        })
    );
    // Steps of 0.25 from -1.25: codes 0, 1, 5.
    assert_eq!(
        recover_codes([-1.25f32, -1.0, 0.0]),
        Some(Codes {
            bits: 3,
            scale: 0.25,
            offset: -1.25
        })
    );
    // 0.1 is no f32, so this checks rounding to f32, not exact arithmetic: codes 0, 1, 3.
    let found = recover_codes([0.1f32, 0.2, 0.4]).unwrap();
    assert_eq!((found.bits, found.scale, found.offset), (2, 0.1, 0.1));
    assert_eq!(recover_codes([7.5f32]).map(|c| c.bits), Some(1));
}

/// An offset of 0.0 gives +0.0 at code 0, never -0.0. The intervals allow it, so only the final
/// bit-for-bit check turns it down.
#[test]
fn negative_zero() {
    for values in [&[-0.0f32][..], &[-0.0, 1.0]] {
        let found = recover_codes(values.iter().copied()).expect("a coding exists");
        for &v in values {
            let code = found.encode(v).unwrap();
            assert_eq!(found.decode(code).to_bits(), v.to_bits(), "{found:?}");
        }
    }
}

#[test]
fn refuses_what_has_no_coding() {
    assert_eq!(recover_codes([]), None);
    assert_eq!(recover_codes([1.0, f32::NAN]), None);
    assert_eq!(recover_codes([1.0, f32::INFINITY]), None);
    assert_eq!(recover_codes([0.0, -0.0, 1.0]), None);
    // 65536 steps need 17 bits; 65535 fit in 16.
    assert_eq!(recover_codes([0.0, 1.0, 65536.0]), None);
    assert_eq!(recover_codes([0.0, 1.0, 65535.0]).map(|c| c.bits), Some(16));
    // 65537 distinct values cannot fit 16 bits whatever the steps; 65536 can.
    assert_eq!(recover_codes((0..65537).map(|i| i as f32)), None);
    assert_eq!(
        recover_codes((0..65536).map(|i| i as f32)).map(|c| c.bits),
        Some(16)
    );
    // 1, 2 and 3.5: the gaps are 1 and 1.5 steps of 1, or 2 and 3 steps of 0.5.
    assert_eq!(
        recover_codes([1.0f32, 2.0, 3.5]),
        Some(Codes {
            bits: 3,
            scale: 0.5,
            offset: 1.0
        })
    );
}

/// A 12-bit ADC's codes through `(offset + code × scale) as f32`, written by hand here, recovered
/// exactly; and with one value one ulp off, refused.
#[test]
fn adc_codes_and_one_ulp_off() {
    let (offset, scale) = (-3.828_125_f64, 0.002_490_04_f64);
    let codes: Vec<u16> = (0..4096u16).filter(|c| c % 7 != 3).collect();
    let values: Vec<f32> = codes
        .iter()
        .map(|&c| (offset + f64::from(c) * scale) as f32)
        .collect();
    let found = recover_codes(values.iter().copied()).expect("exact coding");
    assert_eq!(found.bits, 12);
    for (&value, &code) in values.iter().zip(&codes) {
        assert_eq!(found.encode(value), Some(code), "{value}");
        assert_eq!(found.decode(code).to_bits(), value.to_bits());
    }

    let mut nudged = values.clone();
    nudged[2000] = nudged[2000].next_up();
    assert_eq!(recover_codes(nudged), None);
}

fn analog(path: &Path, channel: &str) -> Vec<f32> {
    let recording = sigrok::read(path).unwrap().recording;
    for c in recording.channels {
        if let (true, ChannelData::Analog(a)) = (c.name == channel, c.data) {
            let Samples::F32(values) = a.samples else {
                panic!("f32 samples")
            };
            return values.collect();
        }
    }
    panic!("no {channel}")
}

/// verge stores `millivolts as f32 / 1e3`; its CSV holds the millivolts. The codes found must be
/// the CSV's millivolts less the smallest.
#[test]
fn verge_volts_are_millivolt_codes() {
    for name in ["marker", "markers-1000"] {
        let volts = analog(&testdata(&format!("{name}.sr")), "V");
        let rows = verge_csv(&format!("{name}.csv"));
        let found = recover_codes(volts.iter().copied()).expect("volts have codes");
        let least = rows.iter().map(|r| r.0).min().unwrap();
        for (value, row) in volts.iter().zip(&rows) {
            assert_eq!(found.encode(*value), Some((row.0 - least) as u16), "{name}");
        }
        println!("{name} V: {found:?}");
    }
}

/// verge's currents come from two ranges with different steps, rounded to whole nanoamps.
#[test]
fn verge_milliamps_have_no_codes() {
    for name in ["marker", "markers-1000"] {
        let milliamps = analog(&testdata(&format!("{name}.sr")), "mA");
        assert_eq!(recover_codes(milliamps), None, "{name}");
    }
}

/// verge's larger sigrok exports, outside the repository.
#[test]
#[ignore = "reads exports outside the repository"]
fn verge_exports() {
    let root = std::path::PathBuf::from(
        std::env::var_os("TICKWAVE_MARKER_EVIDENCE").expect("TICKWAVE_MARKER_EVIDENCE is unset"),
    );
    for dir in ["hardware-final", "hardware", "hardware-gui", "shots"] {
        let mut files: Vec<_> = std::fs::read_dir(root.join(dir))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "sr"))
            .collect();
        files.sort();
        for path in files {
            for channel in ["V", "mA"] {
                let values = analog(&path, channel);
                let distinct = distinct(&values);
                let found = recover_codes(values.iter().copied());
                if let Some(codes) = found {
                    let exact = values.iter().all(|&v| {
                        codes
                            .encode(v)
                            .is_some_and(|c| codes.decode(c).to_bits() == v.to_bits())
                    });
                    assert!(exact);
                }
                println!(
                    "{dir}/{} {channel}: {} samples, {distinct} distinct: {found:?}",
                    path.file_name().unwrap().to_string_lossy(),
                    values.len()
                );
            }
        }
    }
}

fn distinct(values: &[f32]) -> usize {
    let mut bits: Vec<u32> = values.iter().map(|v| v.to_bits()).collect();
    bits.sort_unstable();
    bits.dedup();
    bits.len()
}

/// A Saleae analog export's f32 volts, streamed: `<SALEAE>`, i32 version, i32 type 1, i64 chunk
/// count, f64 begin, f64 trigger, f64 sample rate, u64 downsample, u64 count, then the values.
fn saleae_analog(path: &Path) -> (u64, impl Iterator<Item = f32>) {
    let mut file = BufReader::with_capacity(1 << 20, File::open(path).unwrap());
    let mut header = [0u8; 64];
    file.read_exact(&mut header).unwrap();
    assert_eq!(&header[..8], b"<SALEAE>");
    assert_eq!(
        i32::from_le_bytes(header[12..16].try_into().unwrap()),
        1,
        "analog"
    );
    assert_eq!(
        u64::from_le_bytes(header[48..56].try_into().unwrap()),
        1,
        "downsample"
    );
    let count = u64::from_le_bytes(header[56..64].try_into().unwrap());
    let len = std::fs::metadata(path).unwrap().len();
    assert_eq!(len, 64 + 4 * count, "{}", path.display());
    let values = (0..count).map(move |_| {
        let mut b = [0u8; 4];
        file.read_exact(&mut b).unwrap();
        f32::from_le_bytes(b)
    });
    (count, values)
}

/// The MSO's analog exports. Every sample is checked twice: once through the distinct values
/// inside `recover_codes`, once here by encoding and decoding it again.
#[test]
#[ignore = "reads exports outside the repository, up to 400 MB each"]
fn mso_exports() {
    let root = std::path::PathBuf::from(
        std::env::var_os("TICKWAVE_MSO_EXPORTS").expect("TICKWAVE_MSO_EXPORTS is unset"),
    );
    let runs = [
        "swd50/trigger0",
        "swd50long/trigger0",
        "steady50-direct",
        "session50",
    ];
    let multi = MultiProgress::new();
    let files = multi.add(ProgressBar::new(runs.len() as u64 * 2));
    files.set_style(ProgressStyle::with_template("{bar:30} {pos}/{len} files").unwrap());
    for run in runs {
        for file in ["analog_0.bin", "analog_1.bin"] {
            let path = root.join(run).join(file);
            let (count, values) = saleae_analog(&path);
            let bar = multi.add(ProgressBar::new(count * 2));
            bar.set_style(
                ProgressStyle::with_template("{bar:30} {percent}% {per_sec} {msg}").unwrap(),
            );
            bar.set_message(format!("{run}/{file}"));
            let found = recover_codes(values.inspect(|_| bar.inc(1)));
            let mut checked = 0u64;
            let (mut low, mut high) = (u16::MAX, 0u16);
            if let Some(codes) = found {
                for value in saleae_analog(&path).1 {
                    bar.inc(1);
                    checked += 1;
                    let code = codes.encode(value).expect("every sample has a code");
                    assert_eq!(codes.decode(code).to_bits(), value.to_bits());
                    low = low.min(code);
                    high = high.max(code);
                }
                assert_eq!(checked, count);
            }
            bar.finish_and_clear();
            multi.suspend(|| {
                println!(
                    "{run}/{file}: {count} samples, {found:?}, {checked} rechecked, codes \
                     {low}..={high}"
                )
            });
            files.inc(1);
        }
    }
    files.finish_and_clear();
}

/// A channel holding one value, a disconnected probe's 0 V for one, takes code 0 of a 1-bit coding:
/// the format holds codes of 1 to 16 bits.
#[test]
fn one_value_takes_a_one_bit_coding() {
    for value in [0.0f32, 1.25, -3.3] {
        let codes = recover_codes([value; 5]).unwrap();
        assert_eq!(codes.bits, 1, "{value}");
        assert_eq!(codes.decode(0).to_bits(), value.to_bits(), "{value}");
    }
}
