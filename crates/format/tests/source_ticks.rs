//! Each channel in its own source's ticks (FORMAT.md, "Times"): the mapping to the axis with its
//! floor, the digital section's step, the content id's placement of a source, a recording's size
//! at a timebase k times finer, the writer's refusal of ticks past the format's bounds, and a
//! reader given extreme values in a header.

use escapement_tickwave_format::codec::get_uleb;
use escapement_tickwave_format::decode::{BlockChannel, Footer, Index, from_memory, inflate};
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::{
    Declaration, DeclaredChannel, DeclaredKind, Kept, Offset, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Gap, Ratio, Recording, Samples, Shaping, Source,
};

use escapement_tickwave_format::writer::{Finish, WriteError, WriteOptions, Writer};

mod common;
use common::patched;

fn rate(hz: u64) -> Ratio {
    Ratio::integer(hz).unwrap()
}

fn source(name: &str, hz: u64) -> Source {
    Source::new(SourceFormat::Other(name.into()), rate(hz))
}

fn kept(from: i64, to: i64, offset: i64) -> Shaping {
    Shaping {
        offset: (offset != 0).then(|| Offset {
            ticks: offset,
            kind: Offset::DELAY.into(),
        }),
        kept: Some(Kept { from, to }),
    }
}

fn digital(name: &str, source: u32, edges: Vec<i64>, end: i64, shaping: Shaping) -> Channel {
    Channel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source,
        data: ChannelData::Digital(Digital {
            initial: false,
            end,
            count: edges.len() as u64,
            edges: Box::new(edges.into_iter()),
            gaps: Vec::new(),
        }),
        shaping,
    }
}

fn analog(
    name: &str,
    source: u32,
    first: i64,
    period: u64,
    values: Vec<f32>,
    shaping: Shaping,
) -> Channel {
    Channel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source,
        data: ChannelData::Analog(Analog {
            unit: "V".into(),
            first,
            period: rate(period),
            count: values.len() as u64,
            breaks: Vec::new(),
            significant: None,
            samples: Samples::F32(Box::new(values.into_iter())),
            lost_before: 0,
            lost_after: 0,
            phase: None,
        }),
        shaping,
    }
}

/// Every block's channels, decoded.
fn blocks(file: &[u8]) -> (Footer, Vec<Vec<BlockChannel>>) {
    let index = Index::of_file(file).unwrap();
    let blocks = (0..index.blocks.len())
        .map(|c| {
            index
                .channel_blocks(file, c)
                .unwrap()
                .into_iter()
                .map(|b| b.data)
                .collect()
        })
        .collect();
    (index.footer, blocks)
}

/// A channel's edges over every block: axis ticks, and source ticks. `blocks` holds each
/// channel's blocks.
fn edges(blocks: &[Vec<BlockChannel>], c: usize) -> (Vec<i64>, Vec<i64>) {
    let (mut axis, mut source) = (Vec::new(), Vec::new());
    for block in &blocks[c] {
        let BlockChannel::Digital { edges, .. } = block else {
            panic!("channel {c} is digital");
        };
        axis.extend(edges);
        source.extend(block.source_ticks().unwrap());
    }
    (axis, source)
}

/// A at 1 GHz and B at 96 MHz on the least common multiple of their rates, 12 GHz: A's tick is
/// 12 axis ticks and B's 125, or with `stretch` B's is that times the stretch. B's tick 0 lies at
/// axis tick 1,000, and its channel is moved 3 of its ticks earlier. Both are kept whole.
fn merged(stretch: Option<Ratio>) -> Recording {
    let mut b = source("b", 96_000_000);
    b.origin = 1000;
    b.align = Some(1000);
    b.stretch = stretch;
    Recording {
        timebase: rate(12_000_000_000),
        length: 4_000_000,
        channels: vec![
            digital("A", 0, vec![0, 5, 77, 1000], 160_000, kept(0, 160_000, 0)),
            digital(
                "B",
                1,
                vec![0, 1, 2, 100, 7_000],
                15_000,
                kept(0, 15_000, -3),
            ),
            analog(
                "V",
                1,
                10,
                3,
                (0..100).map(|i| i as f32).collect(),
                kept(0, 15_000, -3),
            ),
        ],
        derived: Vec::new(),
        sources: vec![source("a", 1_000_000_000), b],
    }
}

/// `origin + floor((k + offset) × num / den)`, the floor taken by hand: toward −∞.
fn by_hand(origin: i64, offset: i64, (num, den): (i128, i128), k: i64) -> i64 {
    let x = i128::from(k + offset) * num;
    let q = x / den;
    let floor = if x % den != 0 && x < 0 { q - 1 } else { q };
    origin + floor as i64
}

#[test]
fn a_channel_keeps_its_sources_ticks_and_maps_them_with_a_floor() {
    for (stretch, m) in [
        (None, (125, 1)),
        // 1,234 ppm slow: m = 125 × 500,617 / 500,000 = 500,617 / 4,000.
        (Ratio::new(500_617, 500_000), (500_617, 4_000)),
    ] {
        let file = encode(merged(stretch), &Options::default()).unwrap();
        let (head, blocks) = blocks(&file);
        assert_eq!(head.header.sources[1].stretch, stretch);
        assert_eq!(head.header.m(0), Ratio::new(12, 1));
        assert_eq!(head.header.m(1), Ratio::new(m.0 as u64, m.1 as u64));
        let (axis, source) = edges(&blocks, 0);
        assert_eq!(source, [0, 5, 77, 1000]);
        assert_eq!(axis, [0, 60, 924, 12_000]);
        let (axis, source) = edges(&blocks, 1);
        assert_eq!(source, [0, 1, 2, 100, 7_000]);
        let want: Vec<i64> = source.iter().map(|&k| by_hand(1000, -3, m, k)).collect();
        assert_eq!(axis, want, "stretch {stretch:?}");
        // V's samples, a source tick 10 + 3i each, by the same mapping.
        for i in [0u64, 1, 50, 99] {
            let place = 10 + 3 * i as i64;
            assert_eq!(head.sample_tick(2, i), Some(place));
            assert_eq!(
                head.sample_axis_tick(2, i),
                Some(by_hand(1000, -3, m, place))
            );
        }
    }
    // The row where the floor and truncation must disagree: B's tick 0 moved 3 earlier is
    // -3 × 500,617 / 4,000 = -375.46 axis ticks from its origin: the floor gives 624, truncation
    // would give 625.
    assert_eq!(by_hand(1000, -3, (500_617, 4_000), 0), 624);
    let file = encode(merged(Ratio::new(500_617, 500_000)), &Options::default()).unwrap();
    let (_, blocks) = blocks(&file);
    assert_eq!(edges(&blocks, 1).0[0], 624);
}

/// The step of the first block's section of channel `c`: the section's first number.
fn first_step(file: &[u8], c: usize) -> u64 {
    let index = Index::of_file(file).unwrap();
    let row = index.blocks[c][0].row;
    let raw = inflate(
        &file[row.offset as usize..][..row.length as usize],
        row.decoded_length as usize,
    )
    .unwrap();
    get_uleb(&raw, &mut 0).unwrap()
}

/// One digital channel at `hz`, its edges `edges`, known to `end`.
fn single(hz: u64, edges: Vec<i64>, end: i64) -> Recording {
    Recording {
        timebase: rate(hz),
        length: end,
        channels: vec![digital("D", 0, edges, end, Shaping::default())],
        derived: Vec::new(),
        sources: vec![source("s", hz)],
    }
}

#[test]
fn a_sections_step_takes_out_what_every_interval_shares() {
    // A source declared at 8 times its rate: every tick a multiple of 8.
    let ticks: Vec<i64> = [3i64, 4, 4, 9, 20].iter().map(|t| t * 8).collect();
    let file = encode(single(8_000_000, ticks.clone(), 400), &Options::default()).unwrap();
    assert_eq!(first_step(&file, 0), 8);
    // One edge off the multiple: the step is the greatest common divisor, 4 here.
    let mut odd = ticks.clone();
    odd.push(164);
    let file = encode(single(8_000_000, odd, 400), &Options::default()).unwrap();
    assert_eq!(first_step(&file, 0), 4);
    // No interval but zero ones: 1.
    let file = encode(single(8_000_000, vec![24, 24], 400), &Options::default()).unwrap();
    assert_eq!(first_step(&file, 0), 1);
    let (_, blocks) = blocks(&file);
    assert_eq!(edges(&blocks, 0).1, [24, 24]);
}

/// The same data at a timebase `k` times finer: every tick `k` times larger. A digital channel's
/// edges at intervals from a fixed generator, an analog channel's samples by index, and, merged,
/// a second source at 96 MHz with a channel of its own.
fn same_data(k: u64, merged: bool) -> Recording {
    let base = 1_000_000_000u64;
    let length = 1_000_000_000i64;
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut t = 0i64;
    let mut ticks = Vec::new();
    while ticks.len() < 50_000 {
        t += 1 + (next() % 20_000) as i64;
        ticks.push(t);
    }
    let values: Vec<f32> = (0..20_000)
        .map(|_| (next() % 4096) as f32 / 1024.0)
        .collect();
    let k_i = k as i64;
    let scaled: Vec<i64> = ticks.iter().map(|t| t * k_i).collect();
    let fast = rate(base * k);
    if !merged {
        return Recording {
            timebase: fast,
            length: length * k_i,
            channels: vec![
                digital("D", 0, scaled, length * k_i, Shaping::default()),
                analog("V", 0, 0, 50_000 * k, values, Shaping::default()),
            ],
            derived: Vec::new(),
            sources: vec![Source::new(SourceFormat::Other("fast".into()), fast)],
        };
    }
    // The axis: the least common multiple of the two rates.
    let lcm = |a: u64, b: u64| {
        let (mut x, mut y) = (a, b);
        while y != 0 {
            (x, y) = (y, x % y);
        }
        a / x * b
    };
    let axis = lcm(base * k, 96_000_000);
    let m_fast = (axis / (base * k)) as i64;
    let slow_ticks: Vec<i64> = ticks.iter().step_by(3).map(|t| t * 96 / 1000).collect();
    let slow_end = length * 96 / 1000;
    let axis_length = length * k_i * m_fast;
    Recording {
        timebase: rate(axis),
        length: axis_length,
        channels: vec![
            digital("D", 0, scaled, length * k_i, kept(0, length * k_i, 0)),
            analog("V", 0, 0, 50_000 * k, values, kept(0, length * k_i, 0)),
            digital("S", 1, slow_ticks, slow_end, kept(0, slow_end, 0)),
        ],
        derived: Vec::new(),
        sources: vec![
            Source::new(SourceFormat::Other("fast".into()), fast),
            source("slow", 96_000_000),
        ],
    }
}

/// The acceptance in the brief: the same data at timebase x1, x8, x125 and x1000, single source
/// and merged, within a few % of x1's size. Blocks of 64 KiB, so each holds a first edge.
#[test]
fn the_same_data_at_a_finer_timebase_comes_out_about_as_large() {
    let options = {
        let mut o = Options::default();
        o.block_bytes = 64 << 10;
        o
    };
    for merged in [false, true] {
        let size = |k| encode(same_data(k, merged), &options).unwrap().len();
        let x1 = size(1);
        for k in [8, 125, 1000] {
            let finer = size(k);
            let ratio = finer as f64 / x1 as f64;
            assert!(
                (0.97..1.03).contains(&ratio),
                "merged {merged}: x{k} is {finer} bytes, x1 {x1}: {ratio:.4}"
            );
        }
    }
}

#[test]
fn a_sources_placement_is_part_of_the_content() {
    let id = |rec: Recording| {
        Footer::parse_file(&encode(rec, &Options::default()).unwrap())
            .unwrap()
            .header
            .content_id
    };
    // A stretch alone changes the id.
    let plain = id(merged(None));
    let stretched = id(merged(Ratio::new(500_617, 500_000)));
    assert_ne!(plain, stretched);
    // Stated unreduced, a stretch is refused: one value has one spelling, and one id.
    let err = encode(
        merged(Ratio::new(1_001_234, 1_000_000)),
        &Options::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("source 1's timebase or stretch is not in lowest terms"),
        "{err}"
    );
    // The same m, 250, split two ways: B at 96 MHz stretched twofold, or at 48 MHz. Every tick
    // maps alike, so only the stretch the id pins tells them apart.
    let doubled = id(merged(Ratio::new(2, 1)));
    let mut half = merged(None);
    half.sources[1].timebase = rate(48_000_000);
    assert_ne!(doubled, id(half));
    // B's align alone, every tick of the file the same otherwise.
    let mut moved = merged(None);
    moved.sources[1].align = Some(999);
    assert_ne!(plain, id(moved));
    // One source and no move or cut: two origins with the same data are two recordings.
    let at = |origin: i64| {
        let mut rec = single(1_000_000, vec![200, 300], 900);
        rec.sources[0].origin = origin;
        rec
    };
    assert_ne!(id(at(-100)), id(at(-50)));
}

fn declaration(stretch: Option<Ratio>) -> Declaration {
    let mut s = SourceHeader::new(SourceFormat::Other("s".into()), rate(1_000_000));
    s.stretch = stretch;
    let kept = stretch.map(|_| Kept {
        from: 0,
        to: 1 << 40,
    });
    Declaration {
        timebase: rate(1_000_000),
        channels: vec![
            DeclaredChannel {
                name: "D".into(),
                source_name: "d".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Shaping { offset: None, kept },
                overview_shift: None,
                codec: None,
                requires: Vec::new(),
            },
            DeclaredChannel {
                name: "V".into(),
                source_name: "v".into(),
                source: 0,
                data: DeclaredKind::Analog {
                    unit: "V".into(),
                    period: rate(10),
                    coding: escapement_tickwave_format::header::AnalogCoding::F32,
                    significant: None,
                    phase: None,
                },
                shaping: Shaping { offset: None, kept },
                overview_shift: None,
                codec: None,
                requires: Vec::new(),
            },
        ],
        derived: Vec::new(),
        sources: vec![s],
        title: None,
        meta: Default::default(),
        overview_shift: 12,
        requires: Vec::new(),
    }
}

fn writer(stretch: Option<Ratio>) -> Writer {
    Writer::new(
        declaration(stretch),
        WriteOptions {
            level: 3,
            block_bytes: 1 << 16,
            ..WriteOptions::default()
        },
    )
    .unwrap()
}

/// FORMAT.md's bounds, refused as an error when the item comes in: a source tick outside
/// `0..2^53`, one that maps outside the axis's, and a flush or a length past 2^53.
#[test]
fn the_writer_refuses_a_tick_past_the_formats_bounds_as_it_comes_in() {
    let past = 1i64 << 53;
    for tick in [past, i64::MAX, -1, i64::MIN] {
        let mut w = writer(None);
        assert!(
            matches!(w.edge(0, tick), Err(WriteError::SourceTick { .. })),
            "edge at {tick}"
        );
        let gap = Gap {
            from: 0,
            to: tick,
            level: false,
        };
        assert!(matches!(
            w.gap(0, gap),
            Err(WriteError::SourceTick { .. }) | Err(WriteError::GapSpan { .. })
        ));
        assert!(matches!(
            w.run(1, tick, 0),
            Err(WriteError::SourceTick { .. })
        ));
    }
    // A sample whose place runs past 2^53.
    let mut w = writer(None);
    w.run(1, past - 5, 0).unwrap();
    w.sample(1, 1.0f32).unwrap();
    assert!(matches!(
        w.sample(1, 2.0f32),
        Err(WriteError::SourceTick { .. })
    ));
    // A source tick inside the bounds that a stretch maps past them.
    let mut w = writer(Ratio::new(2, 1));
    assert!(matches!(
        w.edge(0, past / 2),
        Err(WriteError::AxisTick { .. })
    ));
    w.edge(0, past / 8).unwrap();
    // A stretch lies in 1/2 to 2: past either end, the declaration is refused.
    for (n, d) in [(4, 1), (1, 4), (2001, 1000), (499, 1000)] {
        let err = Writer::new(declaration(Ratio::new(n, d)), WriteOptions::default())
            .err()
            .expect("refused")
            .to_string();
        assert!(
            err.contains("source 0's stretch lies outside 1/2 to 2"),
            "{n}/{d}: {err}"
        );
    }
    for (n, d) in [(2, 1), (1, 2)] {
        Writer::new(declaration(Ratio::new(n, d)), WriteOptions::default()).unwrap();
    }
    let mut w = writer(None);
    w.edge(0, 5).unwrap();
    assert!(matches!(w.flush(past), Err(WriteError::TooLarge { .. })));
    assert!(matches!(
        w.flush(i64::MAX),
        Err(WriteError::TooLarge { .. })
    ));
    assert!(matches!(
        writer(None).finish(past, &[0], Finish::default()),
        Err(WriteError::TooLarge { .. })
    ));
    assert!(matches!(
        writer(None).finish(10, &[i64::MAX], Finish::default()),
        Err(WriteError::End { .. })
    ));
}

/// A header with extreme values where the mapping reads them: each comes back as an error or
/// reads, and decoding every block of one that reads never panics.
#[test]
fn a_header_with_extreme_values_is_refused_without_a_panic() {
    let file = encode(merged(Ratio::new(500_617, 500_000)), &Options::default()).unwrap();
    let head = Footer::parse_file(&file).unwrap();
    let extremes = [
        i64::MIN,
        i64::MIN + 1,
        -(1 << 53),
        -(1 << 53) + 1,
        -1,
        0,
        1,
        (1 << 53) - 1,
        1 << 53,
        i64::MAX - 1,
        i64::MAX,
    ];
    let ratios = [
        (1, 1),
        (1, u64::MAX),
        (u64::MAX, 1),
        ((1 << 53) - 1, 1),
        (1, (1 << 53) - 1),
    ];
    type Edit = Box<dyn Fn(&mut escapement_tickwave_format::header::Header, i64, (u64, u64))>;
    let edits: Vec<Edit> = vec![
        Box::new(|h, v, _| h.sources[1].origin = v),
        Box::new(|h, v, _| h.sources[0].origin = v),
        Box::new(|h, v, _| {
            h.channels[1].shaping.offset.as_mut().unwrap().ticks = v;
        }),
        Box::new(|h, v, _| h.channels[1].shaping.kept.as_mut().unwrap().from = v),
        Box::new(|h, v, _| h.channels[1].shaping.kept.as_mut().unwrap().to = v),
        Box::new(|h, v, _| h.length = v),
        Box::new(|h, v, _| {
            if let escapement_tickwave_format::header::ChannelKind::Digital { end, .. } =
                &mut h.channels[1].data
            {
                *end = v;
            }
        }),
        Box::new(|h, v, _| {
            if let escapement_tickwave_format::header::ChannelKind::Digital { gaps, .. } =
                &mut h.channels[1].data
            {
                gaps.push(Gap {
                    from: v,
                    to: v.saturating_add(1),
                    level: false,
                });
            }
        }),
        Box::new(|h, v, _| {
            if let escapement_tickwave_format::header::ChannelKind::Analog { first, .. } =
                &mut h.channels[2].data
            {
                *first = v;
            }
        }),
        Box::new(|h, _, (n, d)| h.sources[1].stretch = Ratio::new(n, d)),
        Box::new(|h, _, (n, d)| h.sources[1].timebase = Ratio::new(n, d).unwrap()),
        Box::new(|h, _, (n, d)| h.timebase = Ratio::new(n, d).unwrap()),
        Box::new(|h, _, (n, d)| {
            if let escapement_tickwave_format::header::ChannelKind::Analog { period, .. } =
                &mut h.channels[2].data
            {
                *period = Ratio::new(n, d).unwrap();
            }
        }),
    ];
    let mut read = 0;
    for edit in &edits {
        for &v in &extremes {
            for &r in &ratios {
                let mut h = head.header.clone();
                edit(&mut h, v, r);
                let bad = common::with_header(&file, &h);
                if let Ok(f) = Footer::parse_file(&bad) {
                    read += 1;
                    if let Ok(index) = Index::read(f, from_memory(&bad)) {
                        for c in 0..index.blocks.len() {
                            let _ = index.channel_blocks(&bad, c);
                            let _ = index.footer.sample_axis_tick(c, 0);
                            let _ = index.footer.sample_axis_tick(c, u64::MAX);
                        }
                    }
                }
            }
        }
    }
    // The edits that leave the file as it was, and a few harmless ones, read; most are refused.
    let tried = edits.len() * extremes.len() * ratios.len();
    assert!(read > 0 && read < tried / 2, "{read} of {tried} read");
}

/// A stretch with a zero or a negative term, so an `m` of zero or below: the header does not read.
#[test]
fn a_stretch_with_a_term_not_above_zero_is_refused() {
    let file = encode(merged(Ratio::new(500_617, 500_000)), &Options::default()).unwrap();
    let stated = r#""stretch":[500617,500000]"#;
    for bad in [
        r#""stretch":[     0,500000]"#,
        r#""stretch":[500617,     0]"#,
        r#""stretch":[-50061,500000]"#,
    ] {
        let err = Footer::parse_file(&patched(&file, stated, bad))
            .expect_err(bad)
            .to_string();
        assert!(err.contains("expected a nonzero u64"), "{bad}: {err}");
    }
}
