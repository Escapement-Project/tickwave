//! The drift between two sources that recorded one marker: each source's rises fitted to a line,
//! and the two periods compared on the merged axis. Each expected drift is the one the inputs were
//! built with, not one the fit computes.

use std::io::Cursor;
use std::sync::Arc;

use escapement_tickwave_format::decode::MAX_DECODED;
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::model::{
    Channel, ChannelData, Digital, Ratio, Recording, Shaping, Source, SourceFormat,
};
use escapement_tickwave_import::convert::{Pass, Progress, Step};
use escapement_tickwave_import::drift::{self, Drift};
use escapement_tickwave_import::merge::{self, Input, InputSettings};

struct Quiet;

impl Progress for Quiet {
    fn start(&self, _: Pass<'_>, _: u64) -> Step {
        Arc::new(|_| {})
    }
}

/// A marker high for 2 ms every 10 ms, 200 times, as a source at `rate` Hz whose clock reads
/// `ppm` fast sees it: each rise at `rate x 10 ms x (1 + ppm/1e6) x k`, plus `jitter(k)` ticks.
/// Rises where `missed(k)` holds are not recorded.
fn marker(
    name: &str,
    rate: u64,
    ppm: f64,
    jitter: &dyn Fn(i64) -> i64,
    missed: &dyn Fn(i64) -> bool,
) -> Vec<u8> {
    let scale = rate as f64 * 1e-3 * (1.0 + ppm / 1e6);
    let mut edges = Vec::new();
    for k in 1..=200i64 {
        if missed(k) {
            continue;
        }
        let rise = (10.0 * k as f64 * scale).round() as i64 + jitter(k);
        edges.extend([rise, rise + (2.0 * scale).round() as i64]);
    }
    let end = (2_020.0 * scale) as i64;
    let tb = Ratio::integer(rate).unwrap();
    encode(
        Recording {
            timebase: tb,
            length: end,
            channels: vec![Channel {
                name: name.into(),
                source_name: name.to_lowercase(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end,
                    count: edges.len() as u64,
                    edges: Box::new(edges.into_iter()),
                    gaps: Vec::new(),
                }),
                shaping: Shaping::default(),
            }],
            derived: Vec::new(),
            sources: vec![Source::new(SourceFormat::Other(name.into()), tb)],
        },
        &Options {
            level: 3,
            ..Options::default()
        },
    )
    .unwrap()
}

fn merged(files: &[Vec<u8>], inputs: Vec<InputSettings>) -> Vec<u8> {
    let open = || -> Result<Vec<Input>, merge::Error> {
        files
            .iter()
            .enumerate()
            .map(|(j, b)| Input::read(Arc::new(b.clone()), format!("in{j}")))
            .collect()
    };
    let mut out = Cursor::new(Vec::new());
    merge::merge(
        &open,
        &merge::Settings {
            inputs,
            level: 3,
            ..merge::Settings::default()
        },
        MAX_DECODED,
        &Quiet,
        &mut out,
    )
    .unwrap();
    out.into_inner()
}

fn drifts(file: Vec<u8>) -> Vec<Drift> {
    drift::of_file(Arc::new(file)).unwrap().1
}

/// A deterministic ±3 tick wander.
fn wander(k: i64) -> i64 {
    (k * 7919 % 7) - 3
}

#[test]
fn a_marker_two_sources_recorded_gives_the_rate_ratio_of_their_clocks() {
    // A at 1 MHz, its clock true, every 7th rise missed; B at 2 MHz, its clock 1,000 ppm fast,
    // wandering 3 ticks. On the 2 MHz axis A's period is 20,000 ticks and B's 20,020.
    let a = marker("A", 1_000_000, 0.0, &|_| 0, &|k| k % 7 == 0);
    let b = marker("B", 2_000_000, 1_000.0, &wander, &|_| false);
    let found = drifts(merged(&[a.clone(), b.clone()], vec![]));
    let [d] = found.as_slice() else {
        panic!("{found:?}")
    };
    assert_eq!((d.a.channel, d.b.channel), (0, 1));
    assert_eq!((d.a.rises, d.b.rises), (172, 200));
    assert_eq!(d.a.spanned, 199);
    assert!((d.ppm - 1_000.0).abs() < 0.5, "{d:?}");
    assert!(d.ppm_se < 0.5, "{d:?}");
    assert!((d.a.period - 10e-3).abs() < 1e-9, "{d:?}");
    // A stretched by what B's clock ran fast: nothing left to measure.
    let s = InputSettings {
        stretch: Some("+1000ppm".into()),
        ..InputSettings::default()
    };
    let found = drifts(merged(&[a, b], vec![s]));
    let [d] = found.as_slice() else {
        panic!("{found:?}")
    };
    assert!(d.ppm.abs() < 0.5, "{d:?}");
}

#[test]
fn markers_whose_periods_differ_by_more_than_a_percent_are_not_compared() {
    let a = marker("A", 1_000_000, 0.0, &|_| 0, &|_| false);
    let b = marker("B", 2_000_000, 20_000.0, &|_| 0, &|_| false);
    assert_eq!(drifts(merged(&[a.clone(), b], vec![])), []);
    // One source alone has no drift, and reads no block for it.
    assert_eq!(drifts(a), []);
}

/// What the drift would read is the blocks of the channels it compares: a third source's channel
/// of two edges, too few to fit, adds none of its own.
#[test]
fn what_the_drift_would_read_is_its_channels_blocks_alone() {
    use escapement_tickwave_format::decode::{BlockChannel, Index};
    let a = marker("A", 1_000_000, 0.0, &|_| 0, &|_| false);
    let b = marker("B", 2_000_000, 1_000.0, &|_| 0, &|_| false);
    // Two rises at 1 MHz, as a marker that fired once.
    let c = marker("C", 1_000_000, 0.0, &|_| 0, &|k| k > 1);
    let file = merged(&[a, b, c], vec![]);
    let index = Index::of_file(&file).unwrap();
    let edges: Vec<usize> = (0..3)
        .map(|ch| {
            index
                .channel_blocks(&file, ch)
                .unwrap()
                .iter()
                .map(|b| match &b.data {
                    BlockChannel::Digital { edges, .. } => edges.len(),
                    BlockChannel::Analog { .. } => 0,
                })
                .sum()
        })
        .collect();
    assert_eq!(edges, [400, 400, 2]);
    let found = drift::candidates(&index).unwrap();
    let blocks = |ch: usize| index.blocks[ch].len();
    let bytes = |ch: usize| -> u64 {
        index.blocks[ch]
            .iter()
            .map(|b| u64::from(b.row.length))
            .sum()
    };
    assert_eq!(
        (found.channels, found.blocks, found.bytes),
        (2, blocks(0) + blocks(1), bytes(0) + bytes(1))
    );
    assert!(blocks(2) > 0);
}
