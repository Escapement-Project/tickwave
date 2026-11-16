//! Small recordings and writers for the instruction-count benches in `benches/`, each one
//! pattern that has cost a round of measurement on full-size captures: a query over verge's
//! layout, statistics that sum each sample, markers by range, a block whose edge step shrinks
//! late, a long idle span, a snapshot with marker lines pending, and repair of a cut file.
//!
//! A recording is written once into the directory a bench names (its `CARGO_TARGET_TMPDIR`) and
//! read from there after, so only the first run pays for writing it, under Valgrind.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, IntCoding,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::writer::{Bundle, Finish, WriteOptions, Writer};

/// Verge's source: 96 MHz.
pub const CLOCK_HZ: u64 = 96_000_000;
/// Pairs a second in verge's layout here.
pub const RATE: u64 = 100_000;
/// DIO0's channel.
pub const DIO0: usize = 4;

/// Verge's layout: V and I, u32 codes in mV and nA, a pair every `period` ticks; their product P;
/// `range`, `under_range` and DIO0 to DIO3. The finest overview level by the recorder's rule.
pub fn declaration(period: Ratio) -> Declaration {
    let timebase = Ratio::integer(CLOCK_HZ).unwrap();
    let int = |den| {
        AnalogCoding::Int(IntCoding {
            scale: Exact::new(1, den),
            ..IntCoding::new(32, false)
        })
    };
    let analog = |name: &str, unit: &str, coding| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: unit.into(),
            period,
            coding,
            significant: Some(4),
            phase: None,
        },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let digital = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let ticks = period.num() as f64 / period.den() as f64;
    Declaration {
        timebase,
        channels: vec![
            analog("V", "V", int(1000)),
            analog("I", "A", int(1_000_000_000)),
            digital("range"),
            digital("under_range"),
            digital("DIO0"),
            digital("DIO1"),
            digital("DIO2"),
            digital("DIO3"),
        ],
        derived: vec![Derived::multiply("P", "W", "I", "V")],
        sources: vec![SourceHeader::new(
            SourceFormat::Other("verge-energy".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: ((1024.0 * ticks).log2().round() as u8).min(24),
        requires: Vec::new(),
    }
}

/// synth.rs's digital edges in second `s`, in order: (tick, channel).
pub fn edges(s: u64) -> Vec<(i64, usize)> {
    let second = (s * CLOCK_HZ) as i64;
    let mut out = Vec::new();
    let mut every = |channel: usize, period: u64, width: u64| {
        let mut t = 0;
        while t < CLOCK_HZ {
            out.push((second + t as i64, channel));
            out.push((second + (t + width) as i64, channel));
            t += period;
        }
    };
    every(4, CLOCK_HZ / 10, CLOCK_HZ / 20);
    every(5, CLOCK_HZ, CLOCK_HZ / 2);
    every(6, CLOCK_HZ / 4, CLOCK_HZ / 1000);
    if s.is_multiple_of(13) || s % 13 == 7 {
        out.push((second, 2));
    }
    if s % 3 == 1 {
        out.push((second + 1000, 3));
        out.push((second + 1000 + CLOCK_HZ as i64 / 1000, 3));
    }
    out.sort();
    out
}

/// A marker line as verge writes one, `number` from 1.
pub fn marker(number: u64, start: i64) -> serde_json::Map<String, serde_json::Value> {
    let serde_json::Value::Object(m) = serde_json::json!({
        "id": format!("marker/{number}"),
        "kind": "marker",
        "start": start,
        "label": format!("Marker {number}"),
        "values": {"number": number.to_string()},
    }) else {
        unreachable!("an object")
    };
    m
}

/// A writer of verge's layout, its runs begun at tick 0, and what it has written so far.
pub struct Synth {
    pub writer: Writer,
    pub file: Vec<u8>,
    period: Ratio,
    rate: u64,
    pairs: u64,
    noise: u64,
    markers: u64,
}

impl Synth {
    /// `rate` pairs a second, one every `period` ticks, at zstd `level`.
    pub fn new(period: Ratio, rate: u64, level: i32) -> Self {
        let mut writer = Writer::new(
            declaration(period),
            WriteOptions {
                level,
                block_bytes: 1 << 20,
                ..WriteOptions::default()
            },
        )
        .unwrap();
        let file = writer.take_output();
        writer.run(0, 0, 0).unwrap();
        writer.run(1, 0, 0).unwrap();
        Self {
            writer,
            file,
            period,
            rate,
            pairs: 0,
            noise: 0x2545_f491_4f6c_dd1d,
            markers: 0,
        }
    }

    /// Second `s`'s pairs, synth's edges where `with_edges`, `markers` markers spread over it,
    /// and the flush at its end.
    pub fn second(&mut self, s: u64, with_edges: bool, markers: u64) {
        let mut due = if with_edges { edges(s) } else { Vec::new() }
            .into_iter()
            .peekable();
        for _ in 0..self.rate {
            self.pair(s, &mut due);
        }
        let w = &mut self.writer;
        for (t, c) in due {
            w.edge(c, t).unwrap();
        }
        for j in 0..markers {
            self.markers += 1;
            let start = (s * CLOCK_HZ + j * CLOCK_HZ / markers) as i64;
            w.annotation("verge", marker(self.markers, start)).unwrap();
        }
        w.flush(((s + 1) * CLOCK_HZ) as i64).unwrap();
        self.file.extend(w.take_output());
    }

    /// The first `n` pairs of second `s`, with synth's edges up to the last of them where
    /// `with_edges`, as a recorder's batch: no flush.
    pub fn batch(&mut self, s: u64, n: u64, with_edges: bool) {
        let mut due = if with_edges { edges(s) } else { Vec::new() }
            .into_iter()
            .peekable();
        for _ in 0..n {
            self.pair(s, &mut due);
        }
        self.file.extend(self.writer.take_output());
    }

    /// The next pair of second `s`, after the edges in `due` up to its tick.
    fn pair(&mut self, s: u64, due: &mut std::iter::Peekable<std::vec::IntoIter<(i64, usize)>>) {
        let w = &mut self.writer;
        let tick = (u128::from(self.pairs) * u128::from(self.period.num())
            / u128::from(self.period.den())) as i64;
        while let Some(&(t, c)) = due.peek()
            && t <= tick
        {
            w.edge(c, t).unwrap();
            due.next();
        }
        let x = &mut self.noise;
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        let step = (s / 5) % 10;
        w.sample(0, 3300 - step + (*x & 7)).unwrap();
        w.sample(1, 1_000_000 + step * 5_000_000 + ((*x >> 8) & 0xfff))
            .unwrap();
        self.pairs += 1;
    }

    /// The whole file, finished `seconds` in.
    pub fn finish(mut self, seconds: u64) -> Vec<u8> {
        let length = (seconds * CLOCK_HZ) as i64;
        let finish = Finish {
            bundle: Some(Bundle::Generated {
                stem: "bench".into(),
            }),
            ..Finish::default()
        };
        let finished = self.writer.finish(length, &[length; 6], finish).unwrap();
        self.file.extend(finished.tail);
        self.file
    }
}

/// The recordings the benches read.
#[derive(Clone, Copy, Debug)]
pub enum Capture {
    /// Verge's layout, 10 s at 100k pairs a second, synth's edges, a marker a second.
    Verge,
    /// The same, 2 s, with V and I a pair every 937.5 ticks, 102,400 a second: a grid of half
    /// ticks, so a window's statistics sum each sample one by one rather than taking the
    /// overview's buckets.
    HalfTicks,
    /// 36 s at 1,000 pairs a second and 1,000 markers a second: 36,000 markers.
    Markers,
    /// 10 s at 1,000 pairs a second and 1,000 markers a second, cut to its first 60 % of bytes.
    CutMarkers,
}

impl Capture {
    fn name(self) -> &'static str {
        match self {
            Self::Verge => "verge",
            Self::HalfTicks => "half-ticks",
            Self::Markers => "markers",
            Self::CutMarkers => "cut-markers",
        }
    }

    fn write(self) -> Vec<u8> {
        let whole = |period: Ratio, rate, seconds, with_edges, markers| {
            let mut synth = Synth::new(period, rate, 1);
            for s in 0..seconds {
                synth.second(s, with_edges, markers);
            }
            synth.finish(seconds)
        };
        let every = |ticks| Ratio::integer(ticks).unwrap();
        match self {
            Self::Verge => whole(every(CLOCK_HZ / RATE), RATE, 10, true, 1),
            Self::HalfTicks => whole(Ratio::new(1875, 2).unwrap(), 102_400, 2, true, 1),
            Self::Markers => whole(every(CLOCK_HZ / 1000), 1000, 36, false, 1000),
            Self::CutMarkers => {
                let file = whole(every(CLOCK_HZ / 1000), 1000, 10, false, 1000);
                file[..file.len() * 6 / 10].to_vec()
            }
        }
    }

    /// The recording's path in `dir`, written there first if it isn't.
    pub fn path(self, dir: &str) -> PathBuf {
        let path = Path::new(dir).join(format!("{}.tickwave", self.name()));
        if !path.exists() {
            let partial = path.with_extension("partial");
            std::fs::File::create(&partial)
                .and_then(|mut f| f.write_all(&self.write()))
                .unwrap();
            std::fs::rename(&partial, &path).unwrap();
        }
        path
    }
}
