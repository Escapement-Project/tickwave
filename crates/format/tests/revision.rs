//! The format revision's additions, each against what FORMAT.md says, worked by hand: codings and
//! their extremes, overview sums, derived channels, lost counts, edges at tick 0, the content id,
//! repair's new length, and the header rules a reader refuses.

use std::collections::BTreeMap;

use escapement_tickwave_format::decode::{
    BlockChannel, Footer, Index, derived_values, from_memory,
};
use escapement_tickwave_format::header::{
    AnalogCoding, ChannelKind, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, Header,
    IntCoding, Kept, Offset, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{
    Analog, Break, Channel, ChannelData, Digital, Gap, Ratio, Recording, Sample, Samples, Source,
};
use escapement_tickwave_format::overview::{AnalogOverview, Columns, DigitalBucket, Num};
use escapement_tickwave_format::reader::{Event, Reader};
use escapement_tickwave_format::recut::{RecutError, Recutter};
use escapement_tickwave_format::repair::{Repairer, repair};
use escapement_tickwave_format::samples::Values;
use escapement_tickwave_format::verify::verify;
use escapement_tickwave_format::writer::{Bundle, Finish, WriteError, WriteOptions, Writer};

mod common;

fn int(
    bits: u8,
    signed: bool,
    scale: Option<(i64, i64)>,
    offset: Option<(i64, i64)>,
) -> AnalogCoding {
    AnalogCoding::Int(IntCoding {
        bits,
        signed,
        scale: scale.map(|(n, d)| Exact::new(n, d).unwrap()),
        offset: offset.map(|(n, d)| Exact::new(n, d).unwrap()),
    })
}

fn analog(name: &str, coding: AnalogCoding, period: u64) -> DeclaredChannel {
    DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period: Ratio::integer(period).unwrap(),
            coding,
            significant: None,
            phase: None,
        },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    }
}

fn digital(name: &str, initial: bool) -> DeclaredChannel {
    DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    }
}

fn declaration(channels: Vec<DeclaredChannel>, derived: Vec<Derived>, shift: u8) -> Declaration {
    let timebase = Ratio::integer(1_000_000).unwrap();
    Declaration {
        timebase,
        channels,
        derived,
        sources: vec![SourceHeader::new(
            SourceFormat::Other("test".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: shift,
        requires: Vec::new(),
    }
}

fn options() -> WriteOptions {
    WriteOptions {
        level: 3,
        block_bytes: 1 << 20,
        ..WriteOptions::default()
    }
}

/// Writes `samples[c]` to analog channel c from tick 0, and `edges[c]` to digital channel c, in
/// tick order, then finishes at `length` with every digital channel known to it.
fn write(
    declaration: Declaration,
    samples: &[Vec<Sample>],
    edges: &[Vec<i64>],
    length: i64,
) -> Vec<u8> {
    let periods: Vec<Option<i64>> = declaration
        .channels
        .iter()
        .map(|c| match &c.data {
            DeclaredKind::Analog { period, .. } => Some(period.num() as i64),
            DeclaredKind::Digital { .. } => None,
        })
        .collect();
    let digital = periods.iter().filter(|p| p.is_none()).count();
    let mut w = Writer::new(declaration, options()).unwrap();
    let mut items: Vec<(i64, usize, Option<Sample>)> = Vec::new();
    for (c, period) in periods.iter().enumerate() {
        match period {
            Some(p) => {
                w.run(c, 0, 0).unwrap();
                for (i, &s) in samples[c].iter().enumerate() {
                    items.push((i as i64 * p, c, Some(s)));
                }
            }
            None => items.extend(edges[c].iter().map(|&t| (t, c, None))),
        }
    }
    items.sort_by_key(|&(t, c, _)| (t, c));
    for (t, c, s) in items {
        match s {
            Some(s) => w.sample(c, s).unwrap(),
            None => w.edge(c, t).unwrap(),
        }
    }
    let mut out = w.take_output();
    out.extend(
        w.finish(length, &vec![length; digital], Finish::default())
            .unwrap()
            .tail,
    );
    out
}

/// Each channel's items, decoded from every block: edges, or samples.
fn decoded(file: &[u8]) -> (Footer, Vec<Vec<i64>>, Vec<Vec<Sample>>) {
    let index = Index::of_file(file).unwrap();
    let n = index.blocks.len();
    let (mut edges, mut samples) = (vec![Vec::new(); n], vec![Vec::new(); n]);
    for c in 0..n {
        for block in index.channel_blocks(file, c).unwrap() {
            match block.data {
                BlockChannel::Digital { edges: e, .. } => edges[c].extend(e),
                BlockChannel::Analog { values, .. } => samples[c].extend(values.iter()),
            }
        }
    }
    (index.footer, edges, samples)
}

/// A sample's stored bits, so NaNs and signed zeros compare.
fn bits(s: &Sample) -> (u8, u64) {
    match *s {
        Sample::F32(v) => (0, u64::from(v.to_bits())),
        Sample::F64(v) => (1, v.to_bits()),
        Sample::U64(c) => (2, c),
        Sample::I64(c) => (3, c as u64),
    }
}

/// Channel `c`'s strip, its coarsest level.
fn analog_overview(head: &Footer, c: usize) -> AnalogOverview {
    head.strip(c).unwrap().data.analog().unwrap().clone()
}

fn buckets<T: Num>(c: &Columns<T>) -> Vec<(T, T, T::Sum)> {
    (0..c.len())
        .map(|i| (c.min[i], c.max[i], c.sum[i]))
        .collect()
}

/// The model of what `write` writes, for `verify`.
fn recording(file: &[u8]) -> Recording {
    let (head, edges, samples) = decoded(file);
    let h = head.header;
    let channels = h
        .channels
        .iter()
        .enumerate()
        .map(|(c, ch)| Channel {
            name: ch.name.clone(),
            source_name: ch.source_name.clone(),
            source: ch.source,
            shaping: ch.shaping.clone(),
            data: match &ch.data {
                ChannelKind::Digital {
                    initial, end, gaps, ..
                } => ChannelData::Digital(Digital {
                    initial: *initial,
                    end: *end,
                    edges: Box::new(edges[c].clone().into_iter()),
                    count: edges[c].len() as u64,
                    gaps: gaps.clone(),
                }),
                ChannelKind::Analog {
                    unit,
                    first,
                    period,
                    count,
                    breaks,
                    coding,
                    significant,
                    ..
                } => {
                    let s = samples[c].clone();
                    ChannelData::Analog(Analog {
                        unit: unit.clone(),
                        first: *first,
                        period: *period,
                        count: *count,
                        breaks: breaks.clone(),
                        significant: *significant,
                        samples: match coding {
                            AnalogCoding::F32 | AnalogCoding::Codes { .. } => {
                                Samples::F32(Box::new(s.into_iter().map(|s| match s {
                                    Sample::F32(v) => v,
                                    _ => unreachable!(),
                                })))
                            }
                            AnalogCoding::F64 => {
                                Samples::F64(Box::new(s.into_iter().map(|s| match s {
                                    Sample::F64(v) => v,
                                    _ => unreachable!(),
                                })))
                            }
                            AnalogCoding::Int(i) if i.signed => Samples::I64(
                                *i,
                                Box::new(s.into_iter().map(|s| match s {
                                    Sample::I64(v) => v,
                                    _ => unreachable!(),
                                })),
                            ),
                            AnalogCoding::Int(i) => Samples::U64(
                                *i,
                                Box::new(s.into_iter().map(|s| match s {
                                    Sample::U64(v) => v,
                                    _ => unreachable!(),
                                })),
                            ),
                        },
                        lost_before: 0,
                        lost_after: 0,
                        phase: None,
                    })
                }
            },
        })
        .collect();
    Recording {
        timebase: h.timebase,
        length: h.length,
        channels,
        derived: h.derived.iter().map(|d| d.declared()).collect(),
        sources: h
            .sources
            .iter()
            .map(|s| Source {
                origin: s.origin,
                align: s.align,
                ..Source::new(s.format.clone(), s.timebase)
            })
            .collect(),
    }
}

#[test]
fn every_coding_round_trips_its_extremes_and_its_overview_is_formats() {
    let codes = AnalogCoding::Codes {
        bits: 12,
        scale: 0.5,
        offset: -1.0,
    };
    let channels = vec![
        analog("U1", int(1, false, None, None), 10),
        analog("S1", int(1, true, None, None), 10),
        analog("U64", int(64, false, None, None), 10),
        analog("S64", int(64, true, None, None), 10),
        analog("NEG", int(12, true, Some((-3, 7)), Some((5, 2))), 10),
        analog("D", AnalogCoding::F64, 10),
        analog("F", AnalogCoding::F32, 10),
        analog("C", codes, 10),
    ];
    let u = |v: &[u64]| v.iter().map(|&c| Sample::U64(c)).collect::<Vec<_>>();
    let s = |v: &[i64]| v.iter().map(|&c| Sample::I64(c)).collect::<Vec<_>>();
    let samples = vec![
        u(&[0, 1, 1, 0]),
        s(&[-1, 0, -1, 0]),
        u(&[u64::MAX, 0, 1, u64::MAX - 1]),
        s(&[i64::MIN, i64::MAX, -1, 0]),
        s(&[-2048, 2047, -1, 0]),
        [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0]
            .map(Sample::F64)
            .to_vec(),
        [1.5, f32::NAN, f32::NEG_INFINITY, 3.25]
            .map(Sample::F32)
            .to_vec(),
        // Codes 0, 3, 4095 and 2: values -1, 0.5, 2046.5 and 0.
        [-1.0, 0.5, 2046.5, 0.0].map(Sample::F32).to_vec(),
    ];
    // Buckets of 16 ticks over 32: samples at 0 and 10 in bucket 0, 20 and 30 in bucket 1.
    let file = write(declaration(channels, vec![], 4), &samples, &[], 32);
    let (head, _, got) = decoded(&file);
    for (c, want) in samples.iter().enumerate() {
        assert_eq!(
            got[c].iter().map(bits).collect::<Vec<_>>(),
            want.iter().map(bits).collect::<Vec<_>>(),
            "{}",
            head.header.channels[c].name
        );
    }
    let AnalogOverview::U64(c) = &analog_overview(&head, 2) else {
        panic!()
    };
    // 2^64 - 1 twice: max + 0, and 1 + (max - 1). Past a u64, exact in the i128 sums.
    let max = i128::from(u64::MAX);
    assert_eq!(buckets(c), [(0, u64::MAX, max), (1, u64::MAX - 1, max)]);
    let AnalogOverview::I64(c) = &analog_overview(&head, 3) else {
        panic!()
    };
    assert_eq!(buckets(c), [(i64::MIN, i64::MAX, -1), (-1, 0, -1)]);
    // An int channel sums its codes, not its values: -2048 + 2047, -1 + 0.
    let AnalogOverview::I64(c) = &analog_overview(&head, 4) else {
        panic!()
    };
    assert_eq!(buckets(c), [(-2048, 2047, -1), (-1, 0, -1)]);
    // NaN is left out; infinities count. Bucket 1: -inf and -0.0, so the max is -0.0 and the sum
    // 0 + -inf + -0.0 = -inf.
    let AnalogOverview::F64(c) = &analog_overview(&head, 5) else {
        panic!()
    };
    assert_eq!(
        buckets(c),
        [
            (f64::INFINITY, f64::INFINITY, f64::INFINITY),
            (f64::NEG_INFINITY, -0.0, f64::NEG_INFINITY)
        ]
    );
    assert!(c.max[1].is_sign_negative());
    let AnalogOverview::F32(c) = &analog_overview(&head, 6) else {
        panic!()
    };
    assert_eq!(
        buckets(c),
        [
            (1.5, 1.5, 1.5),
            (f32::NEG_INFINITY, 3.25, f64::NEG_INFINITY)
        ]
    );
    // A codes channel's buckets are of its f32 values: -1 + 0.5, 2046.5 + 0.
    let AnalogOverview::F32(c) = &analog_overview(&head, 7) else {
        panic!()
    };
    assert_eq!(buckets(c), [(-1.0, 0.5, -0.5), (0.0, 2046.5, 2046.5)]);
    // A value of the negative scale, by FORMAT.md's rule: the code times the numerator,
    // -2048 x -3 = 6144, over the denominator, plus the offset's 5 over 2.
    let AnalogCoding::Int(neg) = head.header.channels[4].coding().unwrap() else {
        panic!()
    };
    assert_eq!(neg.value(-2048), 6144.0 / 7.0 + 5.0 / 2.0);
    let id = head.header.content_id;
    assert_eq!(verify(&file[..], recording(&file)).unwrap().content_id, id);
}

#[test]
fn an_int_value_in_f64_is_the_code_times_the_numerator_over_the_denominator() {
    // An ADC's counts come out as the f64 nearest their value: 3300 mV is 3.3 V, where
    // 3300 x (1/1000) would be 3.3000000000000003.
    let mv = IntCoding {
        scale: Exact::new(1, 1000),
        ..IntCoding::new(32, false)
    };
    assert_eq!(mv.value(3300), 3.3);
    assert_ne!(3300.0 * (1.0 / 1000.0), 3.3);
    // An offset adds its numerator over its denominator, once: 7/10 + 1/3.
    let offset = IntCoding {
        scale: Exact::new(1, 10),
        offset: Exact::new(1, 3),
        ..IntCoding::new(8, false)
    };
    assert_eq!(offset.value(7), 0.7 + 1.0 / 3.0);
    // No offset adds nothing, not even +0: a zero code at a negative scale stays -0.
    let negative = IntCoding {
        scale: Exact::new(-3, 7),
        ..IntCoding::new(8, true)
    };
    assert!(negative.value(0).is_sign_negative());
    // verge's power, 3,304,033,619 pW, is the f64 nearest 0.003304033619 W: the code over 10^12
    // in one division, where code x 10^-12 gives 0.0033040336189999998.
    let p = escapement_tickwave_format::samples::Product::of(
        int(32, false, Some((1, 1_000_000_000)), None),
        int(32, false, Some((1, 1000)), None),
    );
    assert_eq!(p.value(Sample::U64(3_304_033_619)), 0.003304033619);
    assert_ne!(3_304_033_619.0 * 1e-12, 0.003304033619);
}

#[test]
fn a_sample_outside_its_coding_or_of_another_type_is_refused_by_the_writer() {
    let mut w = Writer::new(
        declaration(
            vec![
                analog("U", int(12, false, None, None), 10),
                analog("S", int(12, true, None, None), 10),
            ],
            vec![],
            4,
        ),
        options(),
    )
    .unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    for (c, s, why) in [
        (
            0,
            Sample::U64(4096),
            "U: sample 0: code 4096 is wider than 12 bits",
        ),
        (
            1,
            Sample::I64(2048),
            "S: sample 0: code 2048 lies outside 12 signed bits",
        ),
        (
            1,
            Sample::I64(-2049),
            "S: sample 0: code -2049 lies outside 12 signed bits",
        ),
        (
            0,
            Sample::F32(1.0),
            "U: sample 0: a f32 sample for a channel stored as unsigned 12-bit int",
        ),
    ] {
        assert_eq!(w.sample(c, s).unwrap_err().to_string(), why);
    }
    w.sample(0, 4095u64).unwrap();
    w.sample(1, -2048i64).unwrap();
    // A run lost after the first is a loss; the first has nothing before it to lose.
    let mut w = Writer::new(
        declaration(vec![analog("V", AnalogCoding::F64, 10)], vec![], 4),
        options(),
    )
    .unwrap();
    assert_eq!(
        w.run(0, 0, 2).unwrap_err().to_string(),
        "V: the 2 samples lost before the first run would start before source or axis tick 0"
    );
    w.run(0, 0, 0).unwrap();
    w.sample(0, 1.0).unwrap();
    w.run(0, 50, 2).unwrap();
}

/// FORMAT.md's content id, computed here from its text alone.
mod by_hand {
    pub fn string(h: &mut blake3::Hasher, s: &str) {
        h.update(&(s.len() as u64).to_le_bytes());
        h.update(s.as_bytes());
    }

    /// The "analog" part a channel's digest starts with: the period, then the phase, 0 and 1
    /// where there is none.
    pub fn analog(h: &mut blake3::Hasher, name: &str, unit: &str, period: (u64, u64)) {
        string(h, name);
        string(h, "analog");
        string(h, unit);
        h.update(&period.0.to_le_bytes());
        h.update(&period.1.to_le_bytes());
        h.update(&0u64.to_le_bytes());
        h.update(&1u64.to_le_bytes());
    }

    /// `first`, `count`, then the breaks' count and their own digest: each break's index, tick,
    /// lost and phase (0 here).
    pub fn end_analog(h: &mut blake3::Hasher, first: i64, count: u64, breaks: &[(u64, i64, u64)]) {
        h.update(&first.to_le_bytes());
        h.update(&count.to_le_bytes());
        h.update(&(breaks.len() as u64).to_le_bytes());
        let mut list = blake3::Hasher::new();
        for &(index, tick, lost) in breaks {
            list.update(&index.to_le_bytes());
            list.update(&tick.to_le_bytes());
            list.update(&lost.to_le_bytes());
            list.update(&0u32.to_le_bytes());
        }
        h.update(list.finalize().as_bytes());
    }

    /// A digital channel's end, then its gaps' count and their own digest.
    pub fn end_digital(h: &mut blake3::Hasher, end: i64, gaps: &[(i64, i64, bool)]) {
        h.update(&end.to_le_bytes());
        h.update(&(gaps.len() as u64).to_le_bytes());
        let mut list = blake3::Hasher::new();
        for &(from, to, level) in gaps {
            list.update(&from.to_le_bytes());
            list.update(&to.to_le_bytes());
            list.update(&[u8::from(level)]);
        }
        h.update(list.finalize().as_bytes());
    }

    pub fn id(timebase: (u64, u64), length: i64, digests: &[blake3::Hash]) -> String {
        let mut h = blake3::Hasher::new();
        h.update(&timebase.0.to_le_bytes());
        h.update(&timebase.1.to_le_bytes());
        h.update(&length.to_le_bytes());
        for d in digests {
            h.update(d.as_bytes());
        }
        format!("blake3:{}", h.finalize().to_hex())
    }
}

#[test]
fn the_content_id_of_f64_and_int_channels_and_lost_samples_is_formats() {
    // One run of two, a break losing 3 samples, then one more: at ticks 0, 10, then 50.
    let channels = vec![
        analog("D", AnalogCoding::F64, 10),
        analog("U", int(29, false, Some((1, 1000)), None), 10),
        analog("S", int(12, true, None, Some((-1, 3))), 10),
        digital("E", false),
    ];
    let mut w = Writer::new(declaration(channels, vec![], 4), options()).unwrap();
    let values = [(0.25, 7u64, -5i64), (-1.5, 1 << 28, 2047), (8.0, 0, -2048)];
    for (i, &(d, u, s)) in values.iter().enumerate() {
        if i == 2 {
            for c in 0..3 {
                w.run(c, 50, 3).unwrap();
            }
        } else if i == 0 {
            for c in 0..3 {
                w.run(c, 0, 0).unwrap();
            }
        }
        if i == 1 {
            w.edge(3, 0).unwrap();
            w.edge(3, 0).unwrap();
        }
        w.sample(0, d).unwrap();
        w.sample(1, u).unwrap();
        w.sample(2, s).unwrap();
    }
    let f = w.finish(60, &[60], Finish::default()).unwrap();
    let breaks = [(2u64, 50i64, 3u64)];

    let mut d = blake3::Hasher::new();
    by_hand::analog(&mut d, "D", "V", (10, 1));
    by_hand::string(&mut d, "f64");
    for &(v, _, _) in &values {
        d.update(&f64::to_bits(v).to_le_bytes());
    }
    by_hand::end_analog(&mut d, 0, 3, &breaks);

    let mut u = blake3::Hasher::new();
    by_hand::analog(&mut u, "U", "V", (10, 1));
    by_hand::string(&mut u, "int");
    u.update(&[29, 0]);
    // scale 1/1000, offset 0/1 where none is stated: num as i64, den as u64.
    u.update(&1i64.to_le_bytes());
    u.update(&1000u64.to_le_bytes());
    u.update(&0i64.to_le_bytes());
    u.update(&1u64.to_le_bytes());
    for &(_, v, _) in &values {
        u.update(&v.to_le_bytes());
    }
    by_hand::end_analog(&mut u, 0, 3, &breaks);

    let mut s = blake3::Hasher::new();
    by_hand::analog(&mut s, "S", "V", (10, 1));
    by_hand::string(&mut s, "int");
    s.update(&[12, 1]);
    s.update(&1i64.to_le_bytes());
    s.update(&1u64.to_le_bytes());
    s.update(&(-1i64).to_le_bytes());
    s.update(&3u64.to_le_bytes());
    for &(_, _, v) in &values {
        s.update(&v.to_le_bytes());
    }
    by_hand::end_analog(&mut s, 0, 3, &breaks);

    // Digital: the name, "digital", the initial level, the edges at tick 0, then end and gaps.
    let mut e = blake3::Hasher::new();
    by_hand::string(&mut e, "E");
    by_hand::string(&mut e, "digital");
    e.update(&[0]);
    e.update(&0i64.to_le_bytes());
    e.update(&0i64.to_le_bytes());
    by_hand::end_digital(&mut e, 60, &[]);

    let id = by_hand::id(
        (1_000_000, 1),
        60,
        &[d.finalize(), u.finalize(), s.finalize(), e.finalize()],
    );
    assert_eq!(f.header.content_id.to_string(), id);
    let ChannelKind::Analog { breaks: got, .. } = &f.header.channels[0].data else {
        panic!()
    };
    assert_eq!(
        got,
        &[Break {
            index: 2,
            tick: 50,
            lost: 3,
            phase: 0,
        }]
    );
}

/// Two sources, at 1 MHz and 250 kHz, the second aligned by 8 ticks: every channel states its
/// kept span in its source's ticks, and its digest its source's m, origin, align and stretch.
/// B's ticks are the slow source's, 4 axis ticks each, from axis tick 8.
#[test]
fn a_channel_of_a_recording_of_several_sources_hashes_its_placement() {
    let timebase = Ratio::integer(1_000_000).unwrap();
    let mut a = digital("A", false);
    a.shaping.kept = Some(Kept { from: 0, to: 100 });
    let mut b = digital("B", false);
    b.source = 1;
    b.shaping.kept = Some(Kept { from: 0, to: 23 });
    let mut slow = SourceHeader::new(
        SourceFormat::Other("slow".into()),
        Ratio::integer(250_000).unwrap(),
    );
    slow.align = Some(8);
    slow.origin = 8;
    let declaration = Declaration {
        timebase,
        channels: vec![a, b],
        derived: vec![],
        sources: vec![
            SourceHeader::new(SourceFormat::Other("fast".into()), timebase),
            slow,
        ],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
        requires: Vec::new(),
    };
    let mut w = Writer::new(declaration, options()).unwrap();
    w.edge(0, 5).unwrap();
    // B is not known at its start, axis tick 8, until its source tick 1, axis tick 12.
    w.gap(
        1,
        Gap {
            from: 0,
            to: 1,
            level: true,
        },
    )
    .unwrap();
    w.edge(1, 2).unwrap();
    let mut file = w.take_output();
    let f = w.finish(100, &[100, 23], Finish::default()).unwrap();
    file.extend_from_slice(&f.tail);

    // Flags 2 (kept) and 4 (several sources), then kept, then m, origin, align and stretch.
    let digest = |name: &str,
                  kept: (i64, i64),
                  (m, origin, align): (u64, i64, i64),
                  edge: i64,
                  end: i64,
                  gaps: &[(i64, i64, bool)]| {
        let mut h = blake3::Hasher::new();
        by_hand::string(&mut h, name);
        by_hand::string(&mut h, "digital");
        h.update(&[0, 2 | 4]);
        h.update(&kept.0.to_le_bytes());
        h.update(&kept.1.to_le_bytes());
        h.update(&m.to_le_bytes());
        h.update(&1u64.to_le_bytes());
        h.update(&origin.to_le_bytes());
        h.update(&align.to_le_bytes());
        h.update(&1u64.to_le_bytes());
        h.update(&1u64.to_le_bytes());
        h.update(&edge.to_le_bytes());
        by_hand::end_digital(&mut h, end, gaps);
        h.finalize()
    };
    let id = by_hand::id(
        (1_000_000, 1),
        100,
        &[
            digest("A", (0, 100), (1, 0, 0), 5, 100, &[]),
            digest("B", (0, 23), (4, 8, 8), 2, 23, &[(0, 1, true)]),
        ],
    );
    assert_eq!(f.header.content_id.to_string(), id);
    // B's edge at source tick 2 lies at axis tick 16.
    let (_, edges, _) = decoded(&file);
    assert_eq!(edges, [vec![5], vec![16]]);
}

/// verge's declaration (FORMAT-REVIEW.md): V int 32 u [1, 1000] V, I int 32 u [1, 1e9] A, P the
/// product of I and V.
fn verge() -> Declaration {
    let mut d = declaration(
        vec![
            analog("V", int(32, false, Some((1, 1000)), None), 960),
            analog("I", int(32, false, Some((1, 1_000_000_000)), None), 960),
        ],
        vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            op: escapement_tickwave_format::header::Op::Multiply,
            operands: vec!["I".into(), "V".into()],
            params: Default::default(),
            requires: Vec::new(),
            significant: None,
        }],
        12,
    );
    d.timebase = Ratio::integer(96_000_000).unwrap();
    d.sources[0].timebase = d.timebase;
    d
}

#[test]
fn a_derived_product_of_verges_codes_is_exact_and_its_sums_are_i128() {
    let full = u64::from(u32::MAX);
    let v = [full, 5, 7];
    let i = [full, 2, 3];
    let samples = vec![v.map(Sample::U64).to_vec(), i.map(Sample::U64).to_vec()];
    // Three pairs at 0, 960 and 1920: one bucket of 4,096 ticks.
    let file = write(verge(), &samples, &[], 4096);
    let (head, _, _) = decoded(&file);
    // By hand: (2^32 - 1)^2 = 18,446,744,065,119,617,025, then 10 and 21; the sum adds 31.
    let AnalogOverview::U64(p) = &analog_overview(&head, 2) else {
        panic!("an unsigned integer product")
    };
    assert_eq!(
        buckets(p),
        [(10, 18_446_744_065_119_617_025, 18_446_744_065_119_617_056)]
    );
    let index = Index::of_file(&file).unwrap();
    let (v, i) = (
        index.block_in(&file, 0, 0).unwrap(),
        index.block_in(&file, 1, 0).unwrap(),
    );
    assert_eq!(
        derived_values(&i.data, &v.data, head.products[0].unwrap().2).unwrap(),
        Values::U64(vec![18_446_744_065_119_617_025, 10, 21])
    );
    // Its scale is 1/10^12: one code a picowatt.
    let (_, _, product) = head.products[0].unwrap();
    assert_eq!(product.value(Sample::U64(21)), 21.0 / 1e12);
}

#[test]
fn a_derived_product_past_64_bits_or_of_floats_is_an_f64_rounded_once() {
    let d = declaration(
        vec![
            analog("A", AnalogCoding::F32, 10),
            analog("B", AnalogCoding::F64, 10),
        ],
        vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            op: escapement_tickwave_format::header::Op::Multiply,
            operands: vec!["A".into(), "B".into()],
            params: Default::default(),
            requires: Vec::new(),
            significant: None,
        }],
        4,
    );
    // 0.1f32 is 0.100000001490116119384765625 exactly; times 3 it rounds once in f64.
    let samples = vec![
        vec![Sample::F32(0.1), Sample::F32(-2.0)],
        vec![Sample::F64(3.0), Sample::F64(0.5)],
    ];
    let file = write(d, &samples, &[], 16);
    let (head, _, _) = decoded(&file);
    let once = f64::from(0.1f32) * 3.0;
    assert_ne!(once, f64::from(0.1f32 * 3.0f32));
    let AnalogOverview::F64(p) = &analog_overview(&head, 2) else {
        panic!("an f64 product")
    };
    assert_eq!(buckets(p), [(-1.0, once, 0.0 + once + -1.0)]);
    // A 33-bit and a 32-bit int: past 64 bits, so f64. 2^32 + 3 times 2^31 is exact in f64 only
    // past rounding: (2^32 + 3) as f64 is exact, the product 2^63 + 3 x 2^31 is too.
    let d = declaration(
        vec![
            analog("A", int(33, false, None, None), 10),
            analog("B", int(32, false, None, None), 10),
        ],
        vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            op: escapement_tickwave_format::header::Op::Multiply,
            operands: vec!["A".into(), "B".into()],
            params: Default::default(),
            requires: Vec::new(),
            significant: None,
        }],
        4,
    );
    let samples = vec![vec![Sample::U64((1 << 32) + 3)], vec![Sample::U64(1 << 31)]];
    let file = write(d, &samples, &[], 16);
    let (head, _, _) = decoded(&file);
    let AnalogOverview::F64(p) = &analog_overview(&head, 2) else {
        panic!("an f64 product")
    };
    assert_eq!(p.max[0], 9_223_372_043_297_226_752.0);
}

#[test]
fn operands_that_do_not_share_their_samples_are_refused() {
    let mut w = Writer::new(verge(), options()).unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 960, 0).unwrap();
    w.sample(0, 1u64).unwrap();
    let err = w.sample(1, 1u64).unwrap_err();
    assert_eq!(
        err.to_string(),
        "derived channel P: I's sample 0 starts or sits on another run than V's"
    );
    // One operand a sample short: refused when the block closes.
    let mut w = Writer::new(verge(), options()).unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    w.sample(0, 1u64).unwrap();
    w.sample(1, 1u64).unwrap();
    w.sample(0, 2u64).unwrap();
    let err = w.finish(4096, &[], Finish::default()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "derived channel P: V has sample 1 that I lacks"
    );
}

#[test]
fn a_pulse_at_tick_0_keeps_both_edges_through_write_read_verify_and_repair() {
    let d = declaration(vec![digital("E", false)], vec![], 4);
    let file = write(d.clone(), &[], &[vec![0, 0, 5]], 40);
    let (head, edges, _) = decoded(&file);
    assert_eq!(edges, [vec![0, 0, 5]]);
    let ChannelKind::Digital {
        initial, edges: n, ..
    } = head.header.channels[0].data
    else {
        panic!()
    };
    assert_eq!((initial, n), (false, 3));
    // Bucket 0 holds all three edges, both at tick 0 taken before that tick reads low, then high
    // from 5: 11 of 16 ticks, duty floor(324 / 32) = 10, and density 7.
    let strip = head.strip(0).unwrap();
    let bucket = |density, duty| Some(DigitalBucket { density, duty });
    assert_eq!(strip.data.digital(0), bucket(7, 10));
    assert_eq!(strip.data.digital(1), bucket(0, 14));
    verify(&file[..], recording(&file)).unwrap();
    // Cut off in its footer: repair gives the file back, both edges and all.
    let at = head.trailer.footer_offset as usize;
    let fixed = repair(&file[..at + 3], 3).unwrap();
    assert_eq!(fixed.header, head.header);
    let mut reader = Reader::default();
    reader.feed(&file);
    let mut blocks = 0;
    while let Some(e) = reader.next_event().unwrap() {
        if let Event::Block(b) = e {
            assert!(matches!(&b.data, BlockChannel::Digital { edges, .. } if edges == &[0, 0, 5]));
            blocks += 1;
        }
    }
    assert_eq!(blocks, 1);
    // A gap from tick 0 cannot follow them: no edge lies in a gap.
    let mut w = Writer::new(d, options()).unwrap();
    w.edge(0, 0).unwrap();
    assert!(matches!(
        w.gap(
            0,
            Gap {
                from: 0,
                to: 3,
                level: true
            }
        ),
        Err(WriteError::GapOrder { .. })
    ));
}

#[test]
fn repair_keeps_marks_past_the_last_block_and_a_file_of_marks_alone() {
    let mark = |start: i64| {
        let serde_json::Value::Object(m) =
            serde_json::json!({"id": format!("m/{start}"), "start": start})
        else {
            unreachable!()
        };
        m
    };
    let d = declaration(
        vec![
            digital("E", false),
            analog("V", int(16, false, None, None), 10),
        ],
        vec![],
        4,
    );
    // Data to tick 200, then marks at 150 and 350, then cut off.
    let session = |w: &mut Writer| {
        w.run(1, 0, 0).unwrap();
        for i in 0..20u64 {
            if i == 4 {
                w.edge(0, 33).unwrap();
            }
            w.sample(1, i).unwrap();
        }
        w.flush(200).unwrap();
        w.annotation("verge", mark(150)).unwrap();
        w.annotation("verge", mark(350)).unwrap();
    };
    let mut w = Writer::new(d.clone(), options()).unwrap();
    session(&mut w);
    let cut = w.take_output();
    let fixed = repair(&cut, 3).unwrap();
    // The length is one past the last mark's start, an empty block from 200 runs to it, and the
    // digital channel is known to the last block's end.
    assert_eq!(fixed.header.length, 351);
    assert_eq!(
        (
            fixed.header.channels[0].blocks,
            fixed.header.channels[1].blocks
        ),
        (2, 2)
    );
    let ChannelKind::Digital { end, .. } = fixed.header.channels[0].data else {
        panic!()
    };
    assert_eq!(end, 200);
    assert_eq!(fixed.generated["verge"].len(), 2);
    let file = [&cut[..fixed.keep as usize], &fixed.tail].concat();
    let index = Index::of_file(&file).unwrap();
    assert_eq!(index.blocks[0][1].start, 200);
    // What a writer finishing there writes.
    let mut w = Writer::new(d.clone(), options()).unwrap();
    session(&mut w);
    let mut written = w.take_output();
    written.extend(w.finish(351, &[200], Finish::default()).unwrap().tail);
    assert_eq!(file, written);

    // Marks alone: no block. Tick 0 to one past the last.
    let mut w = Writer::new(d.clone(), options()).unwrap();
    w.annotation("verge", mark(10)).unwrap();
    w.annotation("verge", mark(100)).unwrap();
    let cut = w.take_output();
    let fixed = repair(&cut, 3).unwrap();
    assert_eq!(
        (fixed.header.length, fixed.header.channels[0].blocks),
        (101, 1)
    );
    let ChannelKind::Digital { end, initial, .. } = fixed.header.channels[0].data else {
        panic!()
    };
    assert_eq!((end, initial), (0, false));
    let mut w = Writer::new(d.clone(), options()).unwrap();
    w.annotation("verge", mark(10)).unwrap();
    w.annotation("verge", mark(100)).unwrap();
    let f = w.finish(101, &[0], Finish::default()).unwrap();
    assert_eq!(fixed.header, f.header);
    // The latest start counts, not the last record's: marks at 100, then 10.
    let mut w = Writer::new(d.clone(), options()).unwrap();
    w.annotation("verge", mark(100)).unwrap();
    w.annotation("verge", mark(10)).unwrap();
    let fixed = repair(&w.take_output(), 3).unwrap();
    assert_eq!(fixed.header.length, 101);
    assert_eq!(fixed.generated["verge"].len(), 2);
    // No block, and a digital channel declared high: it is never known, and keeps its declared
    // level.
    let mut high = d.clone();
    high.channels[0].data = DeclaredKind::Digital { initial: true };
    let mut w = Writer::new(high, options()).unwrap();
    w.annotation("verge", mark(10)).unwrap();
    let cut = w.take_output();
    let fixed = repair(&cut, 3).unwrap();
    let file = [&cut[..fixed.keep as usize], &fixed.tail].concat();
    let e = &Footer::parse_file(&file).unwrap().header.channels[0];
    assert!(
        matches!(
            e.data,
            ChannelKind::Digital {
                initial: true,
                end: 0,
                edges: 0,
                ..
            }
        ),
        "{:?}",
        e.data
    );
    // Neither a block nor a mark: nothing to repair.
    let w = Writer::new(d, options()).unwrap();
    let err = repair(w.output(), 3).err().unwrap();
    assert_eq!(
        err.to_string(),
        "the file holds neither a whole block nor an annotation record"
    );
}

/// Re-cuts `file` whole, with `bundle` for a cut-off one: the file it writes, and its header.
fn recut(file: &[u8], bundle: Option<Bundle>) -> Result<(Vec<u8>, Header), RecutError> {
    let mut r = Recutter::new(options());
    if let Some(b) = bundle {
        r.bundle(b);
    }
    r.feed(file)?;
    let mut out = r.take_output();
    let finished = r.finish()?;
    out.extend(finished.tail);
    Ok((out, finished.header))
}

#[test]
fn a_gap_recorded_past_the_last_block_leaves_nothing_known_beyond_it_and_repair_recuts() {
    let mark = |start: i64| {
        let serde_json::Value::Object(m) =
            serde_json::json!({"id": format!("m/{start}"), "start": start})
        else {
            unreachable!()
        };
        m
    };
    let d = declaration(
        vec![
            digital("E", false),
            analog("V", int(16, false, None, None), 10),
        ],
        vec![],
        4,
    );
    // A recorder that holds its last sample back: the block closes at 190, below it. Five
    // samples lost after 190 put a gap on E from place 20, past the block's end, to the next
    // run at 300. A mark at 350, then cut off before the block that holds the gap's `to`.
    let mut w = Writer::new(d, options()).unwrap();
    w.run(1, 0, 0).unwrap();
    for i in 0..19u64 {
        if i == 4 {
            w.edge(0, 33).unwrap();
        }
        w.sample(1, i).unwrap();
    }
    w.flush(190).unwrap();
    w.sample(1, 19u64).unwrap();
    w.gap(
        0,
        Gap {
            from: 200,
            to: 300,
            level: true,
        },
    )
    .unwrap();
    w.run(1, 300, 5).unwrap();
    w.sample(1, 20u64).unwrap();
    w.annotation("verge", mark(350)).unwrap();
    let cut = w.take_output();

    // E is known to the last block's end, not to the gap's `from` past it, and the gap is left
    // out of the header.
    let fixed = repair(&cut, 3).unwrap();
    assert_eq!(fixed.header.length, 351);
    let ChannelKind::Digital { end, ref gaps, .. } = fixed.header.channels[0].data else {
        panic!()
    };
    assert_eq!((end, gaps.len()), (190, 0));
    // The repaired file reads through, and re-cuts to the same content: the gap record it keeps
    // reaches its channel's end inside the length the mark gave.
    let file = [&cut[..fixed.keep as usize], &fixed.tail].concat();
    let (_, again) = recut(&file, None).unwrap();
    assert_eq!(again.content_id, fixed.header.content_id);
    // A re-cut of the cut-off file finishes as repair does.
    let (_, direct) = recut(&cut, None).unwrap();
    assert_eq!(direct.content_id, fixed.header.content_id);
    // The same channels, cut into their own blocks.
    let data = |h: &Header| {
        h.channels
            .iter()
            .map(|c| c.data.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(data(&direct), data(&fixed.header));
    // And bundles what repair bundles, in one pass.
    let stem = || Bundle::Generated { stem: "rec".into() };
    let mut r = Repairer::new(3);
    r.bundle(stem());
    r.feed(&cut).unwrap();
    let bundled = r.finish().unwrap();
    let (file, direct) = recut(&cut, Some(stem())).unwrap();
    let theirs = [&cut[..bundled.keep as usize], &bundled.tail].concat();
    let bundle = |f: &[u8]| {
        Footer::parse_file(f)
            .unwrap()
            .read_bundle(from_memory(f))
            .unwrap()
    };
    assert!(bundle(&file).is_some());
    assert_eq!(bundle(&file), bundle(&theirs));
    assert_eq!(direct.content_id, bundled.header.content_id);
}

#[test]
fn a_writer_bundles_its_annotations_with_a_sidecar_that_checks() {
    let mut w = Writer::new(verge(), options()).unwrap();
    for (start, kind) in [(100, "marker"), (200, "marker-after-loss")] {
        let serde_json::Value::Object(m) = serde_json::json!({
            "id": format!("{kind}/{start}"), "kind": kind, "start": start,
            "values": {"number": "17"},
        }) else {
            unreachable!()
        };
        w.annotation("verge", m).unwrap();
    }
    let mut file = w.take_output();
    let f = w
        .finish(
            1000,
            &[],
            Finish {
                extra: vec![],
                bundle: Some(Bundle::Generated { stem: "cap".into() }),
                drop_gaps_past_end: false,
                lost_after: Default::default(),
            },
        )
        .unwrap();
    file.extend(&f.tail);
    let head = Footer::parse_file(&file).unwrap();
    let files = head.read_bundle(from_memory(&file)).unwrap().unwrap();
    assert_eq!(
        files.keys().collect::<Vec<_>>(),
        ["cap.annotations.toml", "cap.verge.jsonl"]
    );
    let hand = &files["cap.annotations.toml"];
    let included = [escapement_tickwave_format::annotations::Included {
        name: "cap.verge.jsonl",
        text: &files["cap.verge.jsonl"],
    }];
    let resolved = escapement_tickwave_format::annotations::resolve(
        hand,
        &included,
        &head.header,
        Some("cap.tickwave"),
    )
    .unwrap();
    assert_eq!(
        resolved.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        ["verge/marker/100", "verge/marker-after-loss/200"]
    );
    // Bundling leaves the content as it is.
    let mut plain = Writer::new(verge(), options()).unwrap();
    for start in [100, 200] {
        let serde_json::Value::Object(m) =
            serde_json::json!({"id": format!("x/{start}"), "start": start})
        else {
            unreachable!()
        };
        plain.annotation("verge", m).unwrap();
    }
    assert_eq!(
        plain
            .finish(1000, &[], Finish::default())
            .unwrap()
            .header
            .content_id,
        head.header.content_id
    );
}

/// Each rule FORMAT.md states of a header, broken one at a time: the writer refuses the
/// declaration as the reader would.
#[test]
fn a_declaration_breaking_a_header_rule_is_refused_plainly() {
    let refused = |d: Declaration| match Writer::new(d, options()) {
        Err(WriteError::Declaration(why)) => why,
        Err(e) => panic!("refused for another reason: {e}"),
        Ok(_) => panic!("accepted"),
    };
    type Edit = Box<dyn Fn(&mut Declaration)>;
    let cases: Vec<(Edit, &str)> = vec![
        (
            Box::new(|d| d.sources[0].timebase = Ratio::integer(7).unwrap()),
            "the recording has one source, ticking 7/1 times a second, and ticks 96000000/1 \
             itself: a recording of one source ticks at its rate",
        ),
        (
            Box::new(|d| d.sources[0].stretch = Ratio::new(1 << 53, 1)),
            "source 0's stretch has a term past 2^53",
        ),
        (
            Box::new(|d| d.sources[0].stretch = Ratio::new(1_000_001, 1_000_000)),
            "channel V states no kept span, but its source is stretched: a recording with a \
             stretched source states one on every channel",
        ),
        (
            Box::new(|d| d.sources[0].origin = 5),
            "the recording states no kept span, so it starts where its source's data does, but \
             its source's origin is 5, after the axis's tick 0",
        ),
        (
            Box::new(|d| {
                for c in &mut d.channels {
                    c.shaping.kept = Some(Kept { from: 0, to: 10 });
                }
                d.channels[0].shaping.offset = Some(Offset {
                    ticks: 1,
                    kind: Offset::DELAY.into(),
                });
            }),
            "derived channel P: I and V come from different sources, or have different periods, \
             phases or moves",
        ),
        (
            Box::new(|d| d.channels[1].source = 1),
            "channel I's source is 1, but the recording lists 1 sources",
        ),
        (
            Box::new(|d| {
                let extra = d.sources[0].clone();
                d.sources.push(extra);
            }),
            "source 1 is no channel's source",
        ),
        (
            Box::new(|d| d.sources[0].origin = 1 << 53),
            "source 0's origin or align is past 2^53",
        ),
        (
            Box::new(|d| d.sources[0].align = Some(-(1 << 53))),
            "source 0's origin or align is past 2^53",
        ),
        (
            Box::new(|d| {
                // 2^52 ticks a second over a source of 4 ticks a second: an m of 2^54.
                d.timebase = Ratio::integer(1 << 52).unwrap();
                d.sources[0].timebase = Ratio::new(1, 4).unwrap();
            }),
            "source 0's m, the axis ticks in one of its ticks, has a term past 2^53 in lowest \
             terms",
        ),
        (
            Box::new(|d| d.channels[1].name = "V".into()),
            "channel V: another channel has its name",
        ),
        (
            Box::new(|d| {
                if let DeclaredKind::Analog { significant, .. } = &mut d.channels[1].data {
                    *significant = Some(0);
                }
            }),
            "channel I states 0 significant figures; it states at least 1",
        ),
        (
            Box::new(|d| d.derived[0].significant = Some(0)),
            "derived channel P states 0 significant figures; it states at least 1",
        ),
        (
            Box::new(|d| d.derived[0].name = "V".into()),
            "derived channel V: another channel has its name",
        ),
        (
            Box::new(|d| {
                let twice = d.derived[0].clone();
                d.derived.push(twice);
            }),
            "derived channel P: another channel has its name",
        ),
        (
            Box::new(|d| d.derived[0].operands[1] = "W".into()),
            "derived channel P: no channel W",
        ),
        (
            Box::new(|d| {
                if let DeclaredKind::Analog { period, .. } = &mut d.channels[1].data {
                    *period = Ratio::integer(480).unwrap();
                }
            }),
            "derived channel P: I and V come from different sources, or have different periods, phases or moves",
        ),
        (
            Box::new(|d| {
                if let DeclaredKind::Analog { coding, .. } = &mut d.channels[0].data {
                    *coding = int(65, false, None, None);
                }
            }),
            "channel V's codes are 65 bits; an int coding holds 1 to 64",
        ),
        (
            Box::new(|d| {
                if let DeclaredKind::Analog { coding, .. } = &mut d.channels[0].data {
                    *coding = int(0, true, None, None);
                }
            }),
            "channel V's codes are 0 bits; an int coding holds 1 to 64",
        ),
        (
            Box::new(|d| {
                if let DeclaredKind::Analog { coding, .. } = &mut d.channels[0].data {
                    *coding = int(32, false, Some((0, 1)), None);
                }
            }),
            "channel V's scale is zero",
        ),
        (
            Box::new(|d| {
                if let DeclaredKind::Analog { coding, .. } = &mut d.channels[0].data {
                    *coding = int(32, false, Some((1, 1 << 53)), None);
                }
            }),
            "channel V's scale has a term past 2^53",
        ),
        (
            Box::new(|d| {
                if let DeclaredKind::Analog { coding, .. } = &mut d.channels[0].data {
                    *coding = int(32, false, None, Some((-(1 << 53), 1)));
                }
            }),
            "channel V's offset has a term past 2^53",
        ),
    ];
    for (edit, why) in cases {
        let mut d = verge();
        edit(&mut d);
        assert_eq!(refused(d), why);
    }
    // Significant figures of 1 are within the rules.
    let mut d = verge();
    d.derived[0].significant = Some(1);
    Writer::new(d, options()).unwrap();
    // Several sources, and a channel without a kept span.
    let mut d = verge();
    d.channels.push(digital("E", false));
    d.channels[2].source = 1;
    let mut slow = d.sources[0].clone();
    slow.timebase = Ratio::integer(1_000_000).unwrap();
    d.sources.push(slow);
    assert_eq!(
        refused(d.clone()),
        "channel V states no kept span, but the recording has several sources: a recording of \
         several sources states one on every channel"
    );
    for c in &mut d.channels {
        c.shaping.kept = Some(Kept { from: 0, to: 10 });
    }
    Writer::new(d.clone(), options()).unwrap();
    // The slow source's m: 2^52 is within the rules, 2^53 is not.
    // 96,000,000 / 2^52 and 48,000,000 / 2^52 in lowest terms.
    d.sources[1].timebase = Ratio::new(46_875, 1 << 41).unwrap();
    Writer::new(d.clone(), options()).unwrap();
    d.sources[1].timebase = Ratio::new(46_875, 1 << 42).unwrap();
    assert_eq!(
        refused(d),
        "source 1's m, the axis ticks in one of its ticks, has a term past 2^53 in lowest terms"
    );
}

#[test]
fn a_header_breaking_a_rule_only_the_end_settles_is_refused_by_the_reader() {
    let samples = vec![vec![Sample::U64(1); 3], vec![Sample::U64(2); 3]];
    let file = write(verge(), &samples, &[], 4096);
    Footer::parse_file(&file).unwrap();
    // Two channels named V: the reader refuses it as the writer does.
    let twice = common::patched(&file, r#""name":"I""#, r#""name":"V""#);
    let err = Footer::parse_file(&twice).unwrap_err();
    assert_eq!(err.to_string(), "channel V: another channel has its name");
    // V's count one short of I's: the derived channel's operands differ.
    let bad = common::patched(
        &file,
        r#""period":[960,1],"count":3"#,
        r#""period":[960,1],"count":2"#,
    );
    let err = Footer::parse_file(&bad).unwrap_err();
    assert!(
        err.to_string()
            .contains("derived channel P: I and V differ in first, phase, count, breaks or losses"),
        "{err}"
    );
    // A first run at tick 0 that loses a sample: the loss would start before tick 0.
    let mut r = Reader::default();
    let mut doctored = file.clone();
    let run = doctored
        .windows(4)
        .position(|w| w == [4, 32, 0, 0])
        .expect("a run record");
    doctored[run + 5 + 20] = 1;
    let check = run + 5 + 32;
    let mut h = blake3::Hasher::new();
    h.update(&[4]);
    h.update(&32u32.to_le_bytes());
    h.update(&doctored[run + 5..check]);
    doctored[check..check + 8].copy_from_slice(&h.finalize().as_bytes()[..8]);
    r.feed(&doctored);
    let err = loop {
        match r.next_event() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("read through"),
            Err(e) => break e,
        }
    };
    assert_eq!(err.to_string(), "channel 0: a run is out of place");
}

/// `file` with `from` replaced by `to`, of the same length, in the first record that holds it,
/// that record's check made again.
fn rewritten(file: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    use escapement_tickwave_format::record::{self, Kind, Next};
    let mut out = file[..12].to_vec();
    let mut pos = 12;
    let mut done = false;
    while let Next::Record { kind, payload, len } = record::next(&file[pos..], usize::MAX) {
        match payload
            .windows(from.len())
            .position(|w| w == from)
            .filter(|_| !done)
        {
            Some(at) => {
                let mut payload = payload.to_vec();
                payload[at..at + to.len()].copy_from_slice(to);
                record::write(&mut out, Kind::from_byte(kind).unwrap(), &[&payload]);
                done = true;
            }
            None => out.extend_from_slice(&file[pos..pos + len]),
        }
        pos += len;
    }
    assert!(done, "no record holds the bytes");
    out.extend_from_slice(&file[pos..]);
    out
}

#[test]
fn a_gap_record_from_tick_0_after_an_edge_there_is_refused_by_the_reader() {
    use escapement_tickwave_format::record::{self, Kind};
    let mut w = Writer::new(declaration(vec![digital("E", false)], vec![], 4), options()).unwrap();
    w.edge(0, 0).unwrap();
    w.flush(10).unwrap();
    let mut file = w.take_output();
    // What the writer refuses to write: a gap from tick 0, where an edge lies.
    record::write(
        &mut file,
        Kind::Gap,
        &[&record::gap_bytes(
            0,
            Gap {
                from: 0,
                to: 20,
                level: true,
            },
        )],
    );
    let mut r = Reader::default();
    r.feed(&file);
    let err = loop {
        match r.next_event() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("read through"),
            Err(e) => break e,
        }
    };
    assert_eq!(
        err.to_string(),
        "channel 0: a gap from 0 to 20 is out of place"
    );
}

#[test]
fn an_operand_may_run_ahead_of_the_other_and_one_left_unpaired_is_refused_at_the_finish() {
    let mut d = verge();
    d.channels.push(digital("E", false));
    let mut w = Writer::new(
        d,
        WriteOptions {
            block_bytes: 1,
            ..options()
        },
    )
    .unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    w.sample(0, 1u64).unwrap();
    // Each channel in its own order: V's sample waits for I's, past E's edge at 5 and V's own
    // full block, and pairs once it comes.
    w.edge(2, 5).unwrap();
    w.sample(0, 2u64).unwrap();
    w.sample(1, 3u64).unwrap();
    w.sample(1, 4u64).unwrap();
    // A sample of V that I never gives is refused at the finish.
    w.sample(0, 5u64).unwrap();
    let err = w.finish(4096, &[4096], Finish::default()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "derived channel P: V has sample 2 that I lacks"
    );
}

#[test]
fn verify_checks_each_sources_mapping() {
    let samples = vec![vec![Sample::U64(1); 3], vec![Sample::U64(2); 3]];
    let file = write(verge(), &samples, &[], 4096);
    verify(&file[..], recording(&file)).unwrap();
    let mut moved = recording(&file);
    moved.sources[0].origin = 1;
    let err = verify(&file[..], moved).unwrap_err();
    assert_eq!(
        err.to_string(),
        "source 0: the format, timebase, origin, align or stretch differs"
    );
    // The coding is the source's: another scale is another recording.
    let mut model = recording(&file);
    if let ChannelData::Analog(a) = &mut model.channels[0].data
        && let Samples::U64(int, _) = &mut a.samples
    {
        int.scale = Exact::new(1, 100);
    }
    let err = verify(&file[..], model).unwrap_err();
    assert_eq!(
        err.to_string(),
        "channel V: the kind, initial level, sampling or coding differs"
    );
}

#[test]
fn a_declaration_a_reader_meets_is_held_to_the_rules_before_anything_relies_on_it() {
    let mut w = Writer::new(verge(), options()).unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    w.sample(0, 1u64).unwrap();
    w.sample(1, 1u64).unwrap();
    w.flush(10).unwrap();
    let cut = w.take_output();
    // The derived channel names a channel there isn't: repair, which would build the overview
    // from it, refuses it plainly.
    let doctored = rewritten(&cut, br#"["I","V"]"#, br#"["I","W"]"#);
    let err = repair(&doctored, 3).err().unwrap();
    assert_eq!(err.to_string(), "derived channel P: no channel W");
}
