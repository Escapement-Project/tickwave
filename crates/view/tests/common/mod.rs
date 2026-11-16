//! Recordings for the data layer's tests, and an oracle that reads them through the format crate
//! alone: every block decoded, every sample's time from `Grid::sample_time`, exactly.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use escapement_tickwave_format::decode::{BlockChannel, Index};
use escapement_tickwave_format::header::{
    AnalogCoding, ChannelKind, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact,
    IntCoding, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{Ratio, Sample};
use escapement_tickwave_format::overview::Level;
use escapement_tickwave_format::samples::value_f64;
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
use escapement_tickwave_view::{Cache, Recording, Ticks};

pub fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../web/fixtures")
        .join(name);
    std::fs::read(path).unwrap()
}

pub fn open(file: &[u8]) -> Recording {
    Recording::open(Arc::new(file.to_vec()), Cache::new(64 << 20)).unwrap()
}

/// Channel `o`'s overview levels in a finished file, finest first, each whole: those in overview
/// blocks, then the strip.
pub fn levels(file: &[u8], o: usize) -> Vec<Level> {
    let index = Index::of_file(file).unwrap();
    let mut out: Vec<Level> = (0..index.footer.channels[o].levels.len())
        .map(|li| index.level_in(file, o, li).unwrap())
        .collect();
    out.push(index.footer.strip(o).unwrap());
    out
}

/// One analog or derived channel's samples, as the oracle reads them.
#[derive(Clone, Debug)]
pub struct Oracle {
    /// The units of its times: `1 / per` axis ticks.
    pub per: u128,
    /// Each sample: where its time starts and ends, in those units, its value, and its code where
    /// it has one.
    pub samples: Vec<(i128, i128, f64, Option<i128>)>,
}

/// What the oracle sums over an interval: samples, measured time, min, max, Σ code × time in the
/// oracle's units where every sample has a code, else Σ value × ticks.
#[derive(Clone, Debug, PartialEq)]
pub struct Truth {
    pub samples: u64,
    pub measured: Ticks,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub codes: Option<i128>,
    pub float: f64,
}

impl Oracle {
    pub fn over(&self, a: i64, b: i64) -> Truth {
        let mut t = Truth {
            samples: 0,
            measured: Ticks {
                units: 0,
                per: std::num::NonZeroU128::new(self.per).unwrap(),
            },
            min: None,
            max: None,
            codes: self.samples.first().and_then(|s| s.3).map(|_| 0),
            float: 0.0,
        };
        let per = i128::try_from(self.per).unwrap();
        let (a, b) = (i128::from(a) * per, i128::from(b) * per);
        // Starts and the ends of the samples' times both only grow, so the samples that reach
        // past `a` start here, and none at or past `b` counts.
        let first = self.samples.partition_point(|s| s.1 <= a);
        for &(start, end, value, code) in &self.samples[first..] {
            if start >= b {
                break;
            }
            let units = end.min(b) - start.max(a);
            if units <= 0 || value.is_nan() {
                continue;
            }
            t.samples += 1;
            t.measured.units += units;
            t.min = Some(t.min.map_or(value, |m: f64| m.min(value)));
            t.max = Some(t.max.map_or(value, |m: f64| m.max(value)));
            match (&mut t.codes, code) {
                (Some(s), Some(c)) => *s += c * units,
                _ => t.float += value * (units as f64 / self.per as f64),
            }
        }
        t
    }
}

/// Every analog and derived channel's samples of a finished file, by channel index; None for a
/// digital channel.
pub fn oracle(file: &[u8]) -> Vec<Option<Oracle>> {
    let index = Index::of_file(file).unwrap();
    let f = &index.footer;
    let h = &f.header;
    let n = h.channels.len();
    let mut values: Vec<Vec<Sample>> = vec![Vec::new(); n + h.derived.len()];
    for (c, channel) in values[..n].iter_mut().enumerate() {
        for block in index.channel_blocks(file, c).unwrap() {
            if let BlockChannel::Analog { values: v, .. } = &block.data {
                channel.extend(v.iter());
            }
        }
    }
    for (d, (a, b, product)) in f.products.iter().map(|p| p.unwrap()).enumerate() {
        values[n + d] = values[a]
            .iter()
            .zip(&values[b])
            .map(|(&x, &y)| product.sample(x, y))
            .collect();
    }
    (0..n + h.derived.len())
        .map(|c| {
            let (stored, product) = if c < n {
                (c, None)
            } else {
                (
                    f.products[c - n].unwrap().0,
                    Some(f.products[c - n].unwrap().2),
                )
            };
            let ChannelKind::Analog {
                first,
                count,
                coding,
                ..
            } = &h.channels[stored].data
            else {
                return None;
            };
            let breaks = f.breaks(stored).unwrap();
            let grid = f.grid(stored).unwrap();
            let map = h.mapping(stored).unwrap();
            let per = u128::from(grid.d()) * u128::from(map.m().den());
            let cap = i128::from(h.length) * i128::try_from(per).unwrap();
            let at = |u: i128| map.axis_time(u, grid.d()).unwrap().min(cap);
            let samples = values[c]
                .iter()
                .enumerate()
                .map(|(i, &s)| {
                    let i = i as u64;
                    let time = grid.sample_time(*first, breaks, *count, i, None).unwrap();
                    let (tick, end) = (at(time.start), at(time.end));
                    let value = match product {
                        Some(p) => p.value(s),
                        None => value_f64(s, *coding),
                    };
                    let code = match s {
                        Sample::U64(x) => Some(i128::from(x)),
                        Sample::I64(x) => Some(i128::from(x)),
                        _ => None,
                    };
                    let code = match (coding, product) {
                        (AnalogCoding::Int(_), None) => code,
                        (_, Some(escapement_tickwave_format::samples::Product::Int { .. })) => code,
                        _ => None,
                    };
                    (tick, end, value, code)
                })
                .collect::<Vec<_>>();
            assert!(
                samples
                    .windows(2)
                    .all(|w| w[0].0 <= w[1].0 && w[0].1 <= w[1].1),
                "ticks and ends only grow"
            );
            Some(Oracle { per, samples })
        })
        .collect()
}

/// A run of a synthetic channel: how many samples, the ticks from its last sample to the next
/// run's tick, and the samples lost before that run.
pub struct Run {
    pub count: u64,
    pub then: i64,
    pub lost: u64,
}

/// A recording with A (u32 codes at 1/1000), B (i16 codes at −1/7) and their product P, every
/// `p` ticks in `runs`; F, f32, every 3/2 ticks; G, u8 codes, every 7/3 ticks from a third of a
/// tick, with a break to a run at two thirds; and a digital channel D with an edge every 777
/// ticks. Its length ends `cut` ticks after A's last sample.
pub fn synth(p: u64, shift: u8, runs: &[Run], cut: i64, block_bytes: usize) -> Vec<u8> {
    let timebase = Ratio::integer(1_000_000).unwrap();
    let analog = |name: &str, period: Ratio, coding| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period,
            coding,
            significant: None,
            phase: None,
        },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let int = |bits, signed, num, den| {
        AnalogCoding::Int(IntCoding {
            scale: Exact::new(num, den),
            ..IntCoding::new(bits, signed)
        })
    };
    let d = Declaration {
        timebase,
        channels: vec![
            analog("A", Ratio::integer(p).unwrap(), int(32, false, 1, 1000)),
            analog("B", Ratio::integer(p).unwrap(), int(16, true, -1, 7)),
            analog("F", Ratio::new(3, 2).unwrap(), AnalogCoding::F32),
            DeclaredChannel {
                data: DeclaredKind::Analog {
                    unit: "V".into(),
                    period: Ratio::new(7, 3).unwrap(),
                    coding: int(8, false, 1, 1),
                    significant: None,
                    phase: Some(Ratio::new(1, 3).unwrap()),
                },
                ..analog("G", Ratio::new(7, 3).unwrap(), AnalogCoding::F32)
            },
            DeclaredChannel {
                name: "D".into(),
                source_name: "d".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Default::default(),
                overview_shift: None,
                codec: None,
                requires: Vec::new(),
            },
        ],
        derived: vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            op: escapement_tickwave_format::header::Op::Multiply,
            operands: vec!["A".into(), "B".into()],
            params: Default::default(),
            requires: Vec::new(),
            significant: None,
        }],
        sources: vec![SourceHeader::new(
            SourceFormat::Other("synth".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: shift,
        requires: Vec::new(),
    };
    let mut w = Writer::new(
        d,
        WriteOptions {
            level: 1,
            block_bytes,
            promise_with_items: true,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    // Every item with its tick, in tick order: (tick, order, what).
    enum Item {
        Run(usize, i64, u64),
        /// A run at a phase in thirds of a tick.
        RunAt(usize, i64, u32, u64),
        Pair(u64),
        F(u64),
        G(u64),
        Edge(i64),
    }
    let mut items: Vec<(i64, u8, Item)> = Vec::new();
    let mut tick = 0i64;
    let mut index = 0u64;
    for (k, r) in runs.iter().enumerate() {
        if k > 0 {
            // After the run before's last pair where that sits at the same tick: the sort is
            // stable.
            let lost = runs[k - 1].lost;
            items.push((tick, 1, Item::Run(0, tick, lost)));
            items.push((tick, 1, Item::Run(1, tick, lost)));
        }
        for j in 0..r.count {
            items.push((tick + (j * p) as i64, 1, Item::Pair(index)));
            index += 1;
        }
        tick += ((r.count - 1) * p) as i64 + r.then;
    }
    let last = tick - runs.last().unwrap().then;
    let length = last + cut;
    let f_count = 20_000.min((length as u64 * 2) / 3);
    for i in 0..f_count {
        items.push(((i * 3 / 2) as i64, 2, Item::F(i)));
    }
    // G: a run from a third of a tick, a break at 5,000 with 4 lost, the rest from 20,000 and
    // two thirds. Each sample at the whole tick its place lies in.
    let mut g = 0u64;
    for i in 0..5_000u64 {
        items.push(((i * 7 + 1).div_euclid(3) as i64, 3, Item::G(g)));
        g += 1;
    }
    items.push((20_000, 0, Item::RunAt(3, 20_000, 2, 4)));
    for i in 0..5_000u64 {
        items.push((20_000 + (i * 7 + 2).div_euclid(3) as i64, 3, Item::G(g)));
        g += 1;
    }
    let mut e = 777;
    while e < length {
        items.push((e, 4, Item::Edge(e)));
        e += 777;
    }
    items.sort_by_key(|&(t, o, _)| (t, o));
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    w.run(2, 0, 0).unwrap();
    w.run(3, 0, 0).unwrap();
    for (_, _, item) in items {
        match item {
            Item::Run(c, t, lost) => w.run(c, t, lost).unwrap(),
            Item::RunAt(c, t, phase, lost) => w.run_at(c, t, phase, lost).unwrap(),
            Item::Pair(i) => {
                w.sample(0, (i * 7919) % 100_000).unwrap();
                w.sample(1, ((i * 31) % 2000) as i64 - 1000).unwrap();
            }
            Item::F(i) => w.sample(2, ((i % 97) as f32) * 0.25 - 3.0).unwrap(),
            Item::G(i) => w.sample(3, (i * 13) % 251).unwrap(),
            Item::Edge(t) => w.edge(4, t).unwrap(),
        }
    }
    let mut out = w.take_output();
    out.extend(w.finish(length, &[length], Finish::default()).unwrap().tail);
    out
}

/// The synthetic recording the statistics tests read: runs ending a full period, cut short by
/// the next run, at the next run's tick (a sample whose time is none, and two lost samples whose
/// time is none), a loss, a pause, and the length three ticks after the last sample.
pub fn synth_cases(block_bytes: usize) -> Vec<u8> {
    let runs = [
        Run {
            count: 40_000,
            then: 10,
            lost: 0,
        },
        Run {
            count: 25_000,
            then: 3,
            lost: 0,
        },
        Run {
            count: 30_000,
            then: 0,
            lost: 0,
        },
        Run {
            count: 50_000,
            then: 1_000,
            lost: 50,
        },
        Run {
            count: 60_000,
            then: 100_000,
            lost: 0,
        },
        Run {
            count: 120_000,
            then: 0,
            lost: 0,
        },
    ];
    synth(10, 4, &runs, 3, block_bytes)
}
