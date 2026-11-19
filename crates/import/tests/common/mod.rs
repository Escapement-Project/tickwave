#![allow(dead_code)]

use std::path::PathBuf;

use escapement_tickwave_format::model::{ChannelData, Ratio, Recording, Samples};

/// The folder the environment variable `var` names: data outside the repository, which an ignored
/// test reads. Unset, the test can't run, and says which variable to set.
pub fn outside(var: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(dir) => PathBuf::from(dir),
        None => panic!("{var} is unset: set it to the folder this test reads, as its doc says"),
    }
}

pub fn testdata(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/import")
        .join(name)
}

/// What libsigrok reads from a fixture, as `testdata/import/make-oracles.py` wrote it.
pub struct Oracle {
    pub samplerate: u64,
    pub logic_samples: i64,
    /// (name, initial level, edges in samples)
    pub digital: Vec<(String, bool, Vec<i64>)>,
    /// (name, f32 bit patterns)
    pub analog: Vec<(String, Vec<u32>)>,
}

impl Oracle {
    pub fn load(name: &str) -> Self {
        let text = std::fs::read_to_string(testdata(&format!("{name}.oracle.txt"))).unwrap();
        let mut oracle = Oracle {
            samplerate: 0,
            logic_samples: 0,
            digital: Vec::new(),
            analog: Vec::new(),
        };
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let mut words = line.split(' ');
            match words.next().unwrap() {
                "samplerate" => oracle.samplerate = words.next().unwrap().parse().unwrap(),
                "logic_samples" => oracle.logic_samples = words.next().unwrap().parse().unwrap(),
                "digital" => {
                    let name = words.next().unwrap().to_owned();
                    let initial = words.next().unwrap() == "1";
                    let edges = words.map(|w| w.parse().unwrap()).collect();
                    oracle.digital.push((name, initial, edges));
                }
                "analog" => {
                    let name = words.next().unwrap().to_owned();
                    let values = words.map(|w| u32::from_str_radix(w, 16).unwrap()).collect();
                    oracle.analog.push((name, values));
                }
                other => panic!("oracle line {other:?}"),
            }
        }
        oracle
    }
}

/// A recording's channels drained: (name, initial, end, edges) and (name, values as bits).
pub struct Drained {
    pub digital: Vec<(String, bool, i64, Vec<i64>)>,
    pub analog: Vec<(String, Vec<u32>)>,
}

pub fn drain(recording: Recording) -> Drained {
    let mut drained = Drained {
        digital: Vec::new(),
        analog: Vec::new(),
    };
    for channel in recording.channels {
        match channel.data {
            ChannelData::Digital(d) => {
                drained
                    .digital
                    .push((channel.name, d.initial, d.end, d.edges.collect()))
            }
            ChannelData::Analog(a) => {
                let Samples::F32(values) = a.samples else {
                    panic!("f32 samples")
                };
                let values: Vec<u32> = values.map(f32::to_bits).collect();
                assert_eq!(values.len() as u64, a.count, "{}: count", channel.name);
                assert_eq!((a.first, a.period), (0, Ratio::ONE));
                drained.analog.push((channel.name, values));
            }
        }
    }
    drained
}

/// verge's CSV sample rows: (millivolts, nanoamps, [dio0, dio1, dio2, dio3, dio_gap]).
pub fn verge_csv(name: &str) -> Vec<(i64, i64, [bool; 5])> {
    let mut reader = csv::Reader::from_path(testdata(name)).unwrap();
    let headers = reader.headers().unwrap().clone();
    let col = |name: &str| headers.iter().position(|h| h == name).unwrap();
    let (seq, mv, na) = (col("sequence"), col("millivolts"), col("nanoamps"));
    let dio = ["dio0", "dio1", "dio2", "dio3", "dio_gap"].map(col);
    reader
        .records()
        .map(Result::unwrap)
        .filter(|r| !r[seq].is_empty())
        .map(|r| {
            (
                r[mv].parse().unwrap(),
                r[na].parse().unwrap(),
                dio.map(|c| &r[c] == "1"),
            )
        })
        .collect()
}
