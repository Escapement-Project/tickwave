//! A recording whose channels' sources do not tick at the recording's rate, or start before its
//! start: what the view gives is axis ticks, each source tick `k` at
//! `origin + floor((k + offset) × m)` (FORMAT.md, "Times"), worked here by hand.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, IntCoding, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{
    Analog, Break, Channel, ChannelData, Digital, Gap, Kept, Offset, Ratio, Recording as Model,
    Samples, Shaping, Source,
};
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
use escapement_tickwave_view::runs::lost_spans;
use escapement_tickwave_view::spans::{ChannelPlace, known_spans};
use escapement_tickwave_view::{Cache, Recording, Sum};

fn open(file: &[u8]) -> Recording {
    Recording::open(Arc::new(file.to_vec()), Cache::new(64 << 20)).unwrap()
}

fn kept(from: i64, to: i64) -> Shaping {
    Shaping {
        offset: None,
        kept: Some(Kept { from, to }),
    }
}

fn source(name: &str, rate: u64) -> Source {
    Source::new(
        SourceFormat::Other(name.into()),
        Ratio::integer(rate).unwrap(),
    )
}

/// Source 1's tick `k` of a channel moved `offset` on the axis: origin 37, m = 1001/400.
fn b_axis(k: i64, offset: i64) -> i64 {
    (37 + (i128::from(k + offset) * 1001).div_euclid(400)) as i64
}

/// B1's sample `i`'s source tick: every 3/2 ticks from 0, and from 800 on from sample 500.
fn b1_tick(i: u64) -> i64 {
    match i {
        0..500 => (i * 3 / 2) as i64,
        _ => 800 + ((i - 500) * 3 / 2) as i64,
    }
}

/// A 1 kHz recording of three sources. Source 0 ticks at the rate: A0, edges at 100, 200 and
/// 5,000, unknown 1,000 to 1,200. Source 1 at 400 Hz, its clock stretched 1,000 ppm slow, so each
/// of its ticks is 1001/400 of the recording's, its tick 0 at 37: B1, 1,000 samples 3/2 ticks
/// apart, a second run from tick 800 after 3 lost; B2, moved 3 of its ticks, edges at 10, 11 and
/// 500, unknown 600 to 700. Source 2 at 250 Hz, 4 ticks each: C, 2,000 int codes 2 ticks apart.
fn three_sources() -> Vec<u8> {
    let tb = Ratio::integer(1_000).unwrap();
    let digital = |edges: Vec<i64>, end: i64, gaps: Vec<Gap>| {
        ChannelData::Digital(Digital {
            initial: false,
            end,
            count: edges.len() as u64,
            edges: Box::new(edges.into_iter()),
            gaps,
        })
    };
    let model = Model {
        timebase: tb,
        length: 16_000,
        channels: vec![
            Channel {
                name: "A0".into(),
                source_name: "a0".into(),
                source: 0,
                data: digital(
                    vec![100, 200, 5_000],
                    10_000,
                    vec![Gap {
                        from: 1_000,
                        to: 1_200,
                        level: true,
                    }],
                ),
                shaping: kept(0, 10_000),
            },
            Channel {
                name: "B1".into(),
                source_name: "b1".into(),
                source: 1,
                data: ChannelData::Analog(Analog {
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::new(3, 2).unwrap(),
                    count: 1_000,
                    breaks: vec![Break {
                        index: 500,
                        tick: 800,
                        lost: 3,
                        phase: 0,
                    }],
                    significant: None,
                    samples: Samples::F32(Box::new((0..1_000).map(|i| i as f32))),
                    lost_before: 0,
                    lost_after: 0,
                    phase: None,
                }),
                shaping: kept(0, 1_600),
            },
            Channel {
                name: "B2".into(),
                source_name: "b2".into(),
                source: 1,
                data: digital(
                    vec![10, 11, 500],
                    1_600,
                    vec![Gap {
                        from: 600,
                        to: 700,
                        level: false,
                    }],
                ),
                shaping: Shaping {
                    offset: Some(Offset {
                        ticks: 3,
                        kind: Offset::DELAY.into(),
                    }),
                    kept: Some(Kept { from: 0, to: 1_600 }),
                },
            },
            Channel {
                name: "C".into(),
                source_name: "c".into(),
                source: 2,
                data: ChannelData::Analog(Analog {
                    unit: "A".into(),
                    first: 0,
                    period: Ratio::integer(2).unwrap(),
                    count: 2_000,
                    breaks: Vec::new(),
                    significant: None,
                    samples: Samples::U64(
                        IntCoding::new(16, false),
                        Box::new((0..2_000).map(|i| i % 7)),
                    ),
                    lost_before: 0,
                    lost_after: 0,
                    phase: None,
                }),
                shaping: kept(0, 4_000),
            },
        ],
        derived: Vec::new(),
        sources: vec![
            source("a", 1_000),
            Source {
                origin: 37,
                align: Some(37),
                stretch: Ratio::new(1001, 1000),
                ..source("b", 400)
            },
            source("c", 250),
        ],
    };
    encode(
        model,
        &Options {
            level: 3,
            block_bytes: 1 << 10,
            analog: vec![
                None,
                None,
                None,
                Some(AnalogCoding::Int(IntCoding::new(16, false))),
            ],
            overview_shift: Some(6),
            ..Options::default()
        },
    )
    .unwrap()
}

#[test]
fn a_stretched_source_s_samples_and_edges_lie_where_its_mapping_puts_them() {
    let file = three_sources();
    let rec = open(&file);
    let cancel = AtomicBool::new(false);
    assert_eq!(rec.length(), 16_000);
    let b1 = rec.channel("B1").unwrap();
    let runs = rec.runs(b1).unwrap();
    let map = rec.mapping(b1).unwrap();
    assert_eq!(
        (map.m(), map.origin(), map.offset()),
        (Ratio::new(1001, 400).unwrap(), 37, 0)
    );
    // Sample 1 at source tick 1, 2.5025 ticks past 37; sample 500 at 800, 2,002 past it.
    assert_eq!((runs.tick(1), runs.tick(500)), (39, 2_039));
    for i in 0..1_000 {
        let t = b_axis(b1_tick(i), 0);
        assert_eq!(runs.tick(i), t, "sample {i}");
        // The first sample at or after its tick is itself, and one tick on, the next.
        assert_eq!(rec.index_at(b1, t).unwrap(), i, "sample {i}");
        assert_eq!(rec.index_at(b1, t + 1).unwrap(), i + 1, "sample {i}");
    }
    // Its runs: from 37 to the place after sample 499, source tick 750; then from the break's
    // tick, 800, to the place after sample 999, 1,550.
    let known = rec.known(b1).unwrap();
    assert_eq!(known.start(), 37);
    assert_eq!(
        known_spans(&known, 0, 16_000),
        [37..b_axis(750, 0), 2_039..b_axis(1_550, 0)]
    );
    assert_eq!((b_axis(750, 0), b_axis(1_550, 0)), (1_913, 3_915));
    // The 3 lost, places 500 to 503 on run 0's grid: source ticks 750 to 754.
    let lost: Vec<_> = lost_spans(&runs).iter().map(|l| (l.from, l.to)).collect();
    assert_eq!(lost, [(1_913, b_axis(754, 0))]);
    // Every sample's time inside the recording, each to the next place on the axis.
    let all = rec.statistics(0..16_000, &cancel).unwrap();
    let s = all.channels[b1].as_ref().unwrap();
    assert_eq!(
        (s.samples, s.measured),
        (1_000, (1_913 - 37) + (3_915 - 2_039))
    );
    let values = {
        let mut v = Vec::new();
        rec.each_sample(b1, 498, 502, &cancel, |i, x, _| {
            v.push((i, x));
            Ok(())
        })
        .unwrap();
        v
    };
    assert_eq!(
        values,
        [(498, 498.0), (499, 499.0), (500, 500.0), (501, 501.0)]
    );

    // B2, moved 3 of its source's ticks: its edges at 10, 11 and 500 lie at 13, 14 and 503
    // ticks of m past 37; its gap's ends at 603 and 703's.
    let b2 = rec.channel("B2").unwrap();
    assert_eq!(rec.mapping(b2).unwrap().offset(), 3);
    assert_eq!(
        rec.edges(b2, 0, 16_000, &cancel).unwrap(),
        (false, vec![69, 72, 1_295])
    );
    assert_eq!(
        [10, 11, 500].map(|k| b_axis(k, 3)),
        [69, 72, 1_295],
        "worked by hand"
    );
    let ChannelPlace::Digital { start, end, gaps } = rec.known(b2).unwrap() else {
        panic!("B2 is digital")
    };
    assert_eq!((start, end), (b_axis(0, 3), b_axis(1_600, 3)));
    assert_eq!(
        gaps.iter()
            .map(|g| (g.from, g.to, g.level))
            .collect::<Vec<_>>(),
        [(b_axis(600, 3), b_axis(700, 3), false)]
    );
    // Past the gap the level is the gap's.
    assert_eq!(
        rec.edges(b2, b_axis(700, 3), 16_000, &cancel).unwrap(),
        (false, vec![])
    );

    // A0's source ticks at the recording's rate: its ticks as they are.
    let a0 = rec.channel("A0").unwrap();
    assert!(rec.mapping(a0).unwrap().is_identity());
    assert_eq!(
        rec.edges(a0, 0, 16_000, &cancel).unwrap(),
        (false, vec![100, 200, 5_000])
    );

    // C: 4 ticks each, so 8 a sample, every sample whole: the overview's whole buckets count each
    // a full 8 ticks, as the samples do one by one.
    let c = rec.channel("C").unwrap();
    let sum: i128 = (0..2_000).map(|i| i128::from(i % 7) * 8).sum();
    for interval in [0..16_000, 64..15_936, 3..15_997] {
        let s = rec.statistics(interval.clone(), &cancel).unwrap().channels[c]
            .clone()
            .unwrap();
        let (a, b) = (interval.start, interval.end);
        // Each sample from 8 i to 8 i + 8, cut to the interval.
        let mut want = (0u64, 0i64, 0i128);
        for i in 0..2_000i64 {
            let ticks = (8 * i + 8).min(b) - (8 * i).max(a);
            if ticks > 0 {
                want.0 += 1;
                want.1 += ticks;
                want.2 += i128::from(i % 7) * i128::from(ticks);
            }
        }
        let Sum::Exact { codes_ticks, .. } = s.sum else {
            panic!("C's codes sum exactly")
        };
        assert_eq!((s.samples, s.measured, codes_ticks), want, "{interval:?}");
        if interval == (0..16_000) {
            assert_eq!(codes_ticks, sum);
        }
    }
    assert_eq!(rec.extent(), Some(0..16_000));
}

/// A recorder attached to a stream that was running: its source's tick 1,000 at the recording's
/// tick 0, so its origin is -1,000. V: 100 samples 10 ticks apart from 1,000, a second run from
/// 1,600 after 2 lost. D: edges at 1,005, 1,500 and 2,000, unknown from 1,200 to 1,300. E:
/// unknown from its start to 1,100, then high. Both known to 2,100. Written in source-tick order,
/// which is its axis order: a snapshot once every item is in, the last still in the writer's open
/// block, then the finished file.
fn attached() -> (
    escapement_tickwave_format::snapshot::Snapshot,
    Vec<u8>,
    Vec<u8>,
) {
    let tb = Ratio::integer(1_000).unwrap();
    let digital = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Shaping::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let declaration = Declaration {
        timebase: tb,
        channels: vec![
            DeclaredChannel {
                name: "V".into(),
                source_name: "v".into(),
                source: 0,
                data: DeclaredKind::Analog {
                    unit: "V".into(),
                    period: Ratio::integer(10).unwrap(),
                    coding: AnalogCoding::F32,
                    significant: None,
                    phase: None,
                },
                shaping: Shaping::default(),
                overview_shift: None,
                codec: None,
                requires: Vec::new(),
            },
            digital("D"),
            digital("E"),
        ],
        derived: Vec::new(),
        sources: vec![SourceHeader {
            origin: -1_000,
            ..SourceHeader::new(SourceFormat::Other("verge".into()), tb)
        }],
        title: None,
        meta: Default::default(),
        overview_shift: 4,
        requires: Vec::new(),
    };
    let mut w = Writer::new(declaration, WriteOptions::default()).unwrap();
    // (source tick, what, sample index): a gap, a sample, an edge.
    let mut items: Vec<(i64, u8, u64)> = (0..100u64)
        .map(|i| match i {
            0..50 => (1_000 + 10 * i as i64, 1, i),
            _ => (1_600 + 10 * (i as i64 - 50), 1, i),
        })
        .collect();
    items.extend([1_005, 1_500, 2_000].map(|t| (t, 2, 0)));
    items.extend([(1_000, 0, 2), (1_200, 0, 1)]);
    items.sort();
    for (tick, kind, i) in items {
        match kind {
            0 => w
                .gap(
                    i as usize,
                    Gap {
                        from: tick,
                        to: tick + 100,
                        level: true,
                    },
                )
                .unwrap(),
            1 => {
                match i {
                    0 => w.run(0, tick, 0).unwrap(),
                    50 => w.run(0, tick, 2).unwrap(),
                    _ => {}
                }
                w.sample(0, escapement_tickwave_format::model::Sample::F32(i as f32))
                    .unwrap();
            }
            _ => w.edge(1, tick).unwrap(),
        }
    }
    let snapshot = w.snapshot();
    let mut out = w.take_output();
    let live = out.clone();
    out.extend(
        w.finish(1_100, &[2_100, 2_100], Finish::default())
            .unwrap()
            .tail,
    );
    (snapshot, live, out)
}

#[test]
fn an_attached_stream_s_ticks_lie_its_origin_earlier_finished_and_live() {
    let (snapshot, live, file) = attached();
    let finished = open(&file);
    let live = Recording::live(snapshot, Arc::new(live), Cache::new(1 << 20)).unwrap();
    let cancel = AtomicBool::new(false);
    // Live, the recording ends past its latest item on the axis, sample 99 at 1,090.
    assert_eq!((finished.length(), live.length()), (1_100, 1_091));
    for (name, rec) in [("finished", &finished), ("live", &live)] {
        let v = rec.channel("V").unwrap();
        let runs = rec.runs(v).unwrap();
        assert_eq!(rec.mapping(v).unwrap().origin(), -1_000, "{name}");
        assert_eq!(
            (runs.first, runs.start(), runs.tick(49), runs.tick(50)),
            (1_000, 0, 490, 600),
            "{name}"
        );
        assert_eq!(rec.index_at(v, 595).unwrap(), 50, "{name}");
        assert_eq!(
            known_spans(&rec.known(v).unwrap(), 0, 1_100),
            [0..500, 600..rec.length()],
            "{name}"
        );
        let d = rec.channel("D").unwrap();
        assert_eq!(
            rec.edges(d, 0, 1_100, &cancel).unwrap(),
            (false, vec![5, 500, 1_000]),
            "{name}"
        );
        let ChannelPlace::Digital { start, gaps, .. } = rec.known(d).unwrap() else {
            panic!("D is digital")
        };
        assert_eq!(start, 0, "{name}");
        assert_eq!(
            gaps.iter().map(|g| (g.from, g.to)).collect::<Vec<_>>(),
            [(200, 300)],
            "{name}"
        );
        // A gap from E's start, source tick 1,000, says where its level is first known.
        let e = rec.channel("E").unwrap();
        let ChannelPlace::Digital { start, gaps, .. } = rec.known(e).unwrap() else {
            panic!("E is digital")
        };
        assert_eq!((start, gaps.len(), rec.gaps(e)), (100, 0, 1), "{name}");
        assert_eq!(
            rec.edges(e, 100, 1_100, &cancel).unwrap(),
            (true, vec![]),
            "{name}"
        );
    }
}
