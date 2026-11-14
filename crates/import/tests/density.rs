//! Every importer gives the encoder each digital channel's edge count, so the finest overview
//! level follows the densest channel (FORMAT.md, "Overview"), a digital one included. Each
//! expected shift is worked from the file's decoded edges, not from the count the encoder took.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use escapement_tickwave_format::decode::{BlockChannel, Footer, MAX_DECODED};
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::ChannelKind;
use escapement_tickwave_format::model::{
    Channel, ChannelData, Digital, Ratio, Recording, Shaping, Source, SourceFormat,
};
use escapement_tickwave_import::convert::{self, Pass, Progress, Settings, Step};
use escapement_tickwave_import::merge::{self, Input, InputSettings};
use escapement_tickwave_import::{recording, saleae, sigrok, vcd};

struct Quiet;

impl Progress for Quiet {
    fn start(&self, _: Pass<'_>, _: u64) -> Step {
        Arc::new(|_| {})
    }
}

fn testdata(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata")
        .join(path)
}

/// The finest shift by FORMAT.md's rule, worked in floating point from what the file holds:
/// each channel's axis ticks an item, the densest of them, `round(log2(1,024 x that))`. A
/// digital channel's items are its decoded edges over its span on the axis.
fn by_hand(file: &[u8]) -> u8 {
    let head = Footer::parse_file(file).unwrap();
    let h = &head.header;
    let mut edges = vec![0u64; h.channels.len()];
    for i in 0..head.rows.len() {
        let (offset, len) = head.block_location(i);
        let block = head
            .decode_block(i, &file[offset as usize..][..len as usize])
            .unwrap();
        for (c, ch) in block.channels.into_iter().enumerate() {
            if let BlockChannel::Digital { edges: e, .. } = ch {
                edges[c] += e.len() as u64;
            }
        }
    }
    let mut densest = f64::INFINITY;
    for (c, channel) in h.channels.iter().enumerate() {
        let source = &h.sources[channel.source as usize];
        let m = h.m(channel.source as usize).unwrap();
        let m = m.num() as f64 / m.den() as f64;
        let offset = channel.shaping.offset.as_ref().map_or(0, |o| o.ticks);
        let axis = |k: i64| source.origin as f64 + ((k + offset) as f64 * m).floor();
        let spacing = match &channel.data {
            ChannelKind::Analog { period, count, .. } if *count > 0 => {
                period.num() as f64 / period.den() as f64 * m
            }
            ChannelKind::Digital { end, .. } if edges[c] > 0 => {
                let start = channel.shaping.kept.map_or(-source.origin, |k| k.from);
                (axis(*end) - axis(start)) / edges[c] as f64
            }
            _ => continue,
        };
        densest = densest.min(spacing);
    }
    assert!(densest.is_finite(), "no channel holds an item");
    (1024.0 * densest).log2().round().clamp(0.0, 62.0) as u8
}

fn shift(file: &[u8]) -> u8 {
    Footer::parse_file(file).unwrap().header.overview_shift
}

fn digital_only(r: &Recording) -> bool {
    r.channels
        .iter()
        .all(|c| matches!(c.data, ChannelData::Digital(_)))
}

fn encoded(recording: Recording) -> Vec<u8> {
    encode(
        recording,
        &Options {
            level: 3,
            ..Options::default()
        },
    )
    .unwrap()
}

#[test]
fn a_digital_only_import_from_each_reader_takes_its_density() {
    let sr = sigrok::read(&testdata("import/demo-logic.sr"))
        .unwrap()
        .recording;
    let vc = vcd::read(&testdata("import/demo-logic.vcd"))
        .unwrap()
        .recording;
    let names: Vec<(String, String)> = [
        ("digital_3_0", "SWCLK"),
        ("digital_3_1", "SWDIO"),
        ("digital_3_2", "NC"),
    ]
    .iter()
    .map(|(s, n)| (s.to_string(), n.to_string()))
    .collect();
    let sl = saleae::read(&testdata("saleae/identify-swd"), &names, None).unwrap();
    for (name, recording) in [("sigrok", sr), ("vcd", vc), ("saleae", sl)] {
        assert!(digital_only(&recording), "{name}");
        let file = encoded(recording);
        let (got, want) = (shift(&file), by_hand(&file));
        assert_eq!(got, want, "{name}");
        assert!(got < 40, "{name}: {got}");
    }
    // A recording read back from a file, as `recording::read` gives it to a merge or a
    // re-conversion.
    let fixture = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/fixtures/identify-swd.tickwave"),
    )
    .unwrap();
    let (_, again) = recording::read(Arc::new(fixture)).unwrap();
    assert!(digital_only(&again));
    let file = encoded(again);
    assert_eq!(shift(&file), by_hand(&file));
}

fn dense(name: &str, step: i64, edges: i64, end: i64) -> Channel {
    let ticks: Vec<i64> = (1..=edges).map(|i| i * step).collect();
    Channel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: ChannelData::Digital(Digital {
            initial: false,
            end,
            count: ticks.len() as u64,
            edges: Box::new(ticks.into_iter()),
            gaps: Vec::new(),
        }),
        shaping: Shaping::default(),
    }
}

fn one_source(timebase: Ratio, length: i64, channels: Vec<Channel>) -> Recording {
    Recording {
        timebase,
        length,
        channels,
        derived: Vec::new(),
        sources: vec![Source::new(SourceFormat::Other("test".into()), timebase)],
    }
}

#[test]
fn a_merge_whose_densest_channel_is_digital_takes_its_density() {
    // D: 499 edges 2 ticks apart at 500 MHz, beside a 96 MHz input whose densest channel is a
    // slow clock. At 12 GHz D's edges are 48 merged ticks apart, over its span of 1,000 ticks:
    // 24,000 / 499 a edge, 1,024 x that about 2^15.6, so a shift of 16.
    let fast = one_source(
        Ratio::integer(500_000_000).unwrap(),
        1_000,
        vec![dense("D", 2, 499, 1_000)],
    );
    let slow = one_source(
        Ratio::integer(96_000_000).unwrap(),
        300,
        vec![dense("CLK", 10, 3, 300)],
    );
    let files = [encoded(fast), encoded(slow)];
    let open = || -> Result<Vec<Input>, merge::Error> {
        files
            .iter()
            .zip(["fast", "slow"])
            .map(|(b, stem)| Input::read(Arc::new(b.clone()), stem))
            .collect()
    };
    let mut out = Cursor::new(Vec::new());
    merge::merge(
        &open,
        &merge::Settings {
            inputs: vec![InputSettings::default(); 2],
            level: 3,
            ..merge::Settings::default()
        },
        MAX_DECODED,
        &Quiet,
        &mut out,
    )
    .unwrap();
    let out = out.into_inner();
    assert_eq!(shift(&out), 16);
    assert_eq!(by_hand(&out), 16);
}

#[test]
fn a_cut_conversion_whose_densest_channel_is_digital_takes_its_density() {
    // D kept from 400 to 600 of 1,000 ticks: its 100 edges there, 2 ticks apart over 200
    // ticks, are its density, not its 500 over the whole capture. 1,024 x 2 is 2^11.
    let timebase = Ratio::integer(1_000_000).unwrap();
    let open = || {
        Ok(one_source(
            timebase,
            1_000,
            vec![dense("D", 2, 499, 1_000), dense("S", 100, 9, 1_000)],
        ))
    };
    let settings = Settings {
        level: 3,
        keeps: vec![("D".into(), Some("400".into()), Some("600".into()))],
        ..Settings::default()
    };
    let mut out = Cursor::new(Vec::new());
    convert::convert(&open, &settings, MAX_DECODED, &Quiet, &mut out).unwrap();
    let out = out.into_inner();
    assert_eq!(shift(&out), 11);
    assert_eq!(by_hand(&out), 11);
}
