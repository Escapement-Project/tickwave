//! Merging recordings, each expected value worked by hand from FORMAT.md's mapping: input j's tick
//! `t` of channel c at merged tick `F_j t + A_j − base + offset_m(c)`.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use escapement_tickwave_format::decode::{BlockChannel, Footer, MAX_DECODED};
use escapement_tickwave_format::encode::{Bundle, Options, encode};
use escapement_tickwave_format::header::{ChannelKind, Header, Kept};
use escapement_tickwave_format::model::{
    Analog, Break, Channel, ChannelData, Derived, Digital, Exact, IntCoding, Offset, Ratio,
    Recording, Sample, Samples, Shaping, Source, SourceFormat,
};
use escapement_tickwave_format::verify::verify;
use escapement_tickwave_import::convert::{Pass, Progress, Step};
use escapement_tickwave_import::merge::{
    Error, Input, InputSettings, Merged, Settings, expected, merge, merged_timebase,
};
use escapement_tickwave_import::recording::read;

struct Quiet;

/// A .tickwave's bytes, its stem, and the sidecar beside it.
type File<'a> = (Vec<u8>, &'a str, Option<BTreeMap<String, String>>);
/// A channel's edges, and an analog channel's samples as (tick, value), by channel.
type Items = (Vec<Vec<i64>>, Vec<Vec<(i64, Sample)>>);
/// A resolved annotation: id, start, end, channels, and whether it keeps a measure.
type Resolved = (String, i64, Option<i64>, Vec<String>, bool);
/// The same with its measure's channel.
type Measured = (String, i64, Option<i64>, Vec<String>, Option<String>);

impl Progress for Quiet {
    fn start(&self, _: Pass<'_>, _: u64) -> Step {
        Arc::new(|_| {})
    }
}

fn digital(name: &str, initial: bool, edges: Vec<i64>, end: i64) -> Channel {
    Channel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: ChannelData::Digital(Digital {
            initial,
            end,
            edges: Box::new(edges.into_iter()),
            gaps: Vec::new(),
        }),
        shaping: Shaping::default(),
    }
}

fn source(format: &str, timebase: Ratio) -> Source {
    Source::new(SourceFormat::Other(format.into()), timebase)
}

/// An analyser at 500 MHz over 1,000 ticks: CLK rises at 100 and falls at 200. Its source states
/// its files, begin and converter, which a merge keeps.
fn analyser() -> Recording {
    let timebase = Ratio::integer(500_000_000).unwrap();
    Recording {
        timebase,
        length: 1_000,
        channels: vec![digital("CLK", false, vec![100, 200], 1_000)],
        derived: Vec::new(),
        sources: vec![Source {
            files: vec!["digital_0.bin".into()],
            begin_s: Some(0.5),
            converter: Some("the analyser's converter".into()),
            ..source("analyser", timebase)
        }],
    }
}

/// A probe at 96 MHz over 300 ticks: its own CLK, high from tick 0, flipping at 10, 20 and 30, and
/// a current I sampled every 3/2 ticks, 200 samples valued 0 to 199.
fn probe() -> Recording {
    let timebase = Ratio::integer(96_000_000).unwrap();
    Recording {
        timebase,
        length: 300,
        channels: vec![
            digital("CLK", true, vec![10, 20, 30], 300),
            Channel {
                name: "I".into(),
                source_name: "i".into(),
                source: 0,
                data: ChannelData::Analog(Analog {
                    unit: "A".into(),
                    first: 0,
                    period: Ratio::new(3, 2).unwrap(),
                    count: 200,
                    breaks: Vec::new(),
                    significant: None,
                    samples: Samples::F32(Box::new((0..200).map(|i| i as f32))),
                }),
                shaping: Shaping::default(),
            },
        ],
        derived: Vec::new(),
        sources: vec![source("probe", timebase)],
    }
}

fn file(recording: Recording) -> Vec<u8> {
    encode(
        recording,
        &Options {
            level: 3,
            ..Options::default()
        },
    )
    .unwrap()
}

/// Merges `files`, each a .tickwave with its stem and sidecar, as `settings` say.
fn run(files: &[File<'_>], settings: &Settings) -> Result<(Vec<u8>, Merged), Error> {
    let open = || -> Result<Vec<Input>, Error> {
        files
            .iter()
            .map(|(bytes, stem, sidecar)| {
                let mut input = Input::read(Arc::new(bytes.clone()), *stem)?;
                if input.bundled.is_none() {
                    input.bundled = sidecar.clone();
                }
                Ok(input)
            })
            .collect()
    };
    let mut out = Cursor::new(Vec::new());
    let merged = merge(&open, settings, MAX_DECODED, &Quiet, &mut out)?;
    Ok((out.into_inner(), merged))
}

fn settings(inputs: Vec<InputSettings>) -> Settings {
    Settings {
        inputs,
        level: 3,
        ..Settings::default()
    }
}

fn aligned(ticks: &str) -> InputSettings {
    InputSettings {
        align: Some(ticks.into()),
        channels: Vec::new(),
    }
}

/// Each channel's edges, and each analog channel's samples as (tick, value).
fn items(file: &[u8]) -> (Header, Items) {
    let head = Footer::parse_file(file).unwrap();
    let n = head.header.channels.len();
    let (mut edges, mut samples) = (vec![Vec::new(); n], vec![Vec::new(); n]);
    for i in 0..head.rows.len() {
        let (offset, len) = head.block_location(i);
        let block = head
            .decode_block(i, &file[offset as usize..][..len as usize])
            .unwrap();
        for (c, ch) in block.channels.into_iter().enumerate() {
            match ch {
                BlockChannel::Digital { edges: e, .. } => edges[c].extend(e),
                BlockChannel::Analog {
                    first_index,
                    values,
                } => samples[c].extend(
                    values
                        .iter()
                        .enumerate()
                        .map(|(k, v)| (head.sample_tick(c, first_index + k as u64).unwrap(), v)),
                ),
            }
        }
    }
    (head.header, (edges, samples))
}

fn kepts(h: &Header) -> Vec<(i64, i64)> {
    h.channels
        .iter()
        .map(|c| {
            let k = c
                .shaping
                .kept
                .expect("a merged channel states its kept span");
            (k.from, k.to)
        })
        .collect()
}

fn digital_of(h: &Header, c: usize) -> (bool, i64, Vec<(i64, i64, bool)>) {
    let ChannelKind::Digital {
        initial, end, gaps, ..
    } = &h.channels[c].data
    else {
        panic!("{} is digital", h.channels[c].name)
    };
    (
        *initial,
        *end,
        gaps.iter().map(|g| (g.from, g.to, g.level)).collect(),
    )
}

fn origins(h: &Header) -> Vec<(i64, Option<i64>)> {
    h.sources.iter().map(|s| (s.origin, s.align)).collect()
}

#[test]
fn two_rates_merge_at_their_least_common_multiple() {
    // 500 MHz = 2^8 5^9 and 96 MHz = 2^11 3 5^6: the multiple is 2^11 3 5^9 = 12 GHz.
    let tb = merged_timebase(&[
        Ratio::integer(500_000_000).unwrap(),
        Ratio::integer(96_000_000).unwrap(),
    ])
    .unwrap();
    assert_eq!(tb, Ratio::integer(12_000_000_000).unwrap());
    // 3/2 and 5/4 have 15/2; 6/4 reduces to 3/2 first.
    assert_eq!(
        merged_timebase(&[Ratio::new(3, 2).unwrap(), Ratio::new(5, 4).unwrap()]).unwrap(),
        Ratio::new(15, 2).unwrap()
    );
    assert_eq!(
        merged_timebase(&[Ratio::new(6, 4).unwrap()]).unwrap(),
        Ratio::new(3, 2).unwrap()
    );
    let err = merged_timebase(&[
        Ratio::integer((1 << 52) + 1).unwrap(),
        Ratio::integer((1 << 52) - 1).unwrap(),
    ])
    .unwrap_err();
    assert!(err.to_string().contains("past 2^53"), "{err}");
    // One rate alone is its own multiple, and is held to 2^53 as well.
    assert_eq!(
        merged_timebase(&[Ratio::integer(1 << 53).unwrap()])
            .unwrap_err()
            .to_string(),
        "the inputs tick 9007199254740992/1: their least common multiple is past 2^53"
    );

    let inputs = [
        (file(analyser()), "analyser", None),
        (file(probe()), "probe", None),
    ];
    let (out, merged) = run(&inputs, &settings(vec![])).unwrap();
    let (h, (edges, samples)) = items(&out);
    assert_eq!(h.timebase, tb);
    let names: Vec<(&str, u32)> = h
        .channels
        .iter()
        .map(|c| (c.name.as_str(), c.source))
        .collect();
    // The probe's CLK takes its stem: the analyser's has the name.
    assert_eq!(names, [("CLK", 0), ("probe/CLK", 1), ("I", 1)]);
    // Factors 24 and 125: the analyser's 1,000 ticks are 24,000, the probe's 300 are 37,500.
    assert_eq!(h.length, 37_500);
    assert_eq!(kepts(&h), [(0, 24_000), (0, 37_500), (0, 37_500)]);
    assert_eq!(edges[0], [2_400, 4_800]);
    assert_eq!(edges[1], [1_250, 2_500, 3_750]);
    assert_eq!(digital_of(&h, 0), (false, 24_000, vec![]));
    assert_eq!(digital_of(&h, 1), (true, 37_500, vec![]));
    // The current's period, 3/2 times 125, is 375/2: sample 1 at 187, finer than 125 times its
    // floored tick 1; sample 199 at floor(199 x 187.5) = 37,312.
    let ChannelKind::Analog { period, first, .. } = &h.channels[2].data else {
        panic!()
    };
    assert_eq!((*period, *first), (Ratio::new(375, 2).unwrap(), 0));
    assert_eq!(samples[2][1], (187, Sample::F32(1.0)));
    assert_eq!(samples[2][2], (375, Sample::F32(2.0)));
    assert_eq!(samples[2][199], (37_312, Sample::F32(199.0)));
    // Each source is kept whole, its origin and align restated.
    assert_eq!(origins(&h), [(0, None), (0, None)]);
    let a = &h.sources[0];
    assert_eq!(
        (
            a.format.as_str(),
            a.files.as_slice(),
            a.begin_s,
            a.timebase,
            a.converter.as_deref()
        ),
        (
            "analyser",
            &["digital_0.bin".to_owned()][..],
            Some(0.5),
            Ratio::integer(500_000_000).unwrap(),
            Some("the analyser's converter")
        )
    );
    // Another recording than either input, and the same one again.
    for (bytes, ..) in &inputs {
        assert_ne!(
            Footer::parse_file(bytes).unwrap().header.content_id,
            h.content_id
        );
    }
    let (again, _) = run(&inputs, &settings(vec![])).unwrap();
    assert_eq!(again, out);
    assert!(merged.notes.is_empty(), "{:?}", merged.notes);
    // Its statements map back to its inputs.
    let against = inputs
        .iter()
        .map(|(b, ..)| read(Arc::new(b.clone())).unwrap().1)
        .collect();
    let rebuilt = expected(&h, against).unwrap();
    assert_eq!(verify(&out[..], rebuilt).unwrap().content_id, h.content_id);
}

#[test]
fn an_alignment_moves_its_inputs_channels_and_the_start_follows_the_earliest() {
    let inputs = [
        (file(analyser()), "analyser", None),
        (file(probe()), "probe", None),
    ];
    // Later: the probe 1,000 merged ticks on. Its channels start there, unknown before, its CLK
    // high in the lead gap as it was at its tick 0.
    let (out, _) = run(
        &inputs,
        &settings(vec![InputSettings::default(), aligned("1000")]),
    )
    .unwrap();
    let (h, (edges, samples)) = items(&out);
    assert_eq!(h.length, 38_500);
    assert_eq!(kepts(&h), [(0, 24_000), (1_000, 38_500), (1_000, 38_500)]);
    assert_eq!(edges[1], [2_250, 3_500, 4_750]);
    assert_eq!(digital_of(&h, 1), (false, 38_500, vec![(0, 1_000, true)]));
    assert_eq!(samples[2][0].0, 1_000);
    assert_eq!(origins(&h), [(0, None), (1_000, Some(1_000))]);
    // Earlier: the probe 500 merged ticks before the analyser. The recording starts with it, so
    // the analyser's tick 0 lies 500 in, behind a lead gap at its level, low.
    let (out, _) = run(
        &inputs,
        &settings(vec![InputSettings::default(), aligned("-500")]),
    )
    .unwrap();
    let (h, (edges, samples)) = items(&out);
    assert_eq!(h.length, 37_500);
    assert_eq!(kepts(&h), [(500, 24_500), (0, 37_500), (0, 37_500)]);
    assert_eq!(edges[0], [2_900, 5_300]);
    assert_eq!(digital_of(&h, 0), (false, 24_500, vec![(0, 500, false)]));
    assert_eq!(edges[1], [1_250, 2_500, 3_750]);
    assert_eq!(samples[2][0].0, 0);
    assert_eq!(origins(&h), [(500, None), (0, Some(-500))]);
    // An alignment as a time at the merged rate: 1 ns is 12 ticks at 12 GHz.
    let (out, _) = run(
        &inputs,
        &settings(vec![InputSettings::default(), aligned("1 ns")]),
    )
    .unwrap();
    assert_eq!(origins(&items(&out).0), [(0, None), (12, Some(12))]);
    // A merge offset 1,200 earlier on I alone: I starts the recording, and every other channel
    // and both sources lie 1,200 in.
    let s = Settings {
        offsets: vec![("I".into(), "-1200".into())],
        ..settings(vec![])
    };
    let (out, _) = run(&inputs, &s).unwrap();
    let (h, (edges, samples)) = items(&out);
    assert_eq!(kepts(&h), [(1_200, 25_200), (1_200, 38_700), (0, 37_500)]);
    assert_eq!(edges[0], [3_600, 6_000]);
    assert_eq!(samples[2][0].0, 0);
    assert_eq!(h.channels[2].shaping.offset.as_ref().unwrap().ticks, -1_200);
    assert_eq!(origins(&h), [(1_200, None), (1_200, None)]);
    assert_eq!(h.length, 38_700);
}

/// A conversion at 500 MHz of D (edges 100, 300, 500, 700) and E (150, 450) over 1,000 ticks,
/// with D moved 10 earlier and E kept from 200 to 800: D starts the recording, so the source's
/// tick 0 lies 10 in, and E is kept from 210 to 810, high in its lead gap after its edge at 150.
fn shaped() -> Vec<u8> {
    use escapement_tickwave_import::convert::{self, Settings as ConvertSettings};
    let timebase = Ratio::integer(500_000_000).unwrap();
    let open = || {
        Ok(Recording {
            timebase,
            length: 1_000,
            channels: vec![
                digital("D", false, vec![100, 300, 500, 700], 1_000),
                digital("E", false, vec![150, 450], 1_000),
            ],
            derived: Vec::new(),
            sources: vec![source("shaped", timebase)],
        })
    };
    let mut out = Cursor::new(Vec::new());
    convert::convert(
        &open,
        &ConvertSettings {
            level: 3,
            offsets: vec![("D".into(), "-10".into())],
            keeps: vec![("E".into(), Some("200".into()), Some("800".into()))],
            ..ConvertSettings::default()
        },
        MAX_DECODED,
        &Quiet,
        &mut out,
    )
    .unwrap();
    out.into_inner()
}

#[test]
fn offsets_and_keeps_compose_with_an_input_moved_and_cut() {
    let s = shaped();
    let h = Footer::parse_file(&s).unwrap().header;
    assert_eq!(h.sources[0].origin, 10);
    assert_eq!(
        kepts(&h),
        [(0, 1_000), (210, 810)],
        "the input as worked out"
    );
    // At 12 GHz, a factor of 24. D moved 48 more merged ticks; E kept from 300 to 700 of its own
    // ticks, inside its own 210..810.
    let merge_settings = Settings {
        offsets: vec![("D".into(), "48".into())],
        keeps: vec![("E".into(), Some("300".into()), Some("700".into()))],
        ..settings(vec![])
    };
    let (out, _) = run(
        &[(s, "shaped", None), (file(probe()), "probe", None)],
        &merge_settings,
    )
    .unwrap();
    let (h, (edges, _)) = items(&out);
    // D's offset: 24 x -10 + 48 = -192, its kind kept. The probe starts at 0, so the base is 0.
    let d = &h.channels[0];
    assert_eq!(
        d.shaping.offset,
        Some(Offset {
            ticks: -192,
            kind: Offset::DELAY.into()
        })
    );
    // D: kept 24 x 0..1,000 + 48; its edges 24 t + 48; unknown before 48, low.
    assert_eq!(kepts(&h)[..2], [(48, 24_048), (7_200, 16_800)]);
    assert_eq!(edges[0], [2_448, 7_248, 12_048, 16_848]);
    assert_eq!(digital_of(&h, 0), (false, 24_048, vec![(0, 48, false)]));
    // E: from 300 of its own ticks, high there since its edge at 150, which the conversion took
    // into its lead gap's level; its edge at 460 (450 + 10) is 11,040; known to 700, so 16,800.
    assert_eq!(edges[1], [11_040]);
    assert_eq!(digital_of(&h, 1), (false, 16_800, vec![(0, 7_200, true)]));
    assert!(h.channels[1].shaping.offset.is_none());
    // The source's origin, 24 x 10, after no alignment and a base of 0.
    assert_eq!(origins(&h), [(240, None), (0, None)]);
    assert_eq!(h.length, 37_500);
}

/// A merged input: the analyser and the probe, the probe 1,000 ticks later at 12 GHz and its CLK
/// 5 more.
fn merged_input() -> Vec<u8> {
    let s = Settings {
        offsets: vec![("probe/CLK".into(), "5".into())],
        ..settings(vec![InputSettings::default(), aligned("1000")])
    };
    run(
        &[
            (file(analyser()), "analyser", None),
            (file(probe()), "probe", None),
        ],
        &s,
    )
    .unwrap()
    .0
}

#[test]
fn a_merged_input_joins_its_sources_and_its_offsets_and_aligns_scale_and_add() {
    let m = merged_input();
    let h = Footer::parse_file(&m).unwrap().header;
    assert_eq!(kepts(&h), [(0, 24_000), (1_005, 38_505), (1_000, 38_500)]);
    // C at 36 GHz: the multiple is 36 GHz, so M's factor is 3. M aligned 900 later, its probe/CLK
    // moved 24 more.
    let tb = Ratio::integer(36_000_000_000).unwrap();
    let c = Recording {
        timebase: tb,
        length: 3_000,
        channels: vec![digital("X", false, vec![10], 3_000)],
        derived: Vec::new(),
        sources: vec![source("third", tb)],
    };
    let s = Settings {
        offsets: vec![("probe/CLK".into(), "24".into())],
        ..settings(vec![aligned("900")])
    };
    let (out, _) = run(&[(m, "m", None), (file(c), "third", None)], &s).unwrap();
    let (h, (edges, _)) = items(&out);
    assert_eq!(h.timebase, tb);
    let names: Vec<&str> = h.channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["CLK", "probe/CLK", "I", "X"]);
    // M's two sources, then C's: origins 3 x origin + 900, aligns 3 x align + 900.
    assert_eq!(
        origins(&h),
        [(900, Some(900)), (3_900, Some(3_900)), (0, None)]
    );
    assert_eq!(
        h.channels.iter().map(|c| c.source).collect::<Vec<_>>(),
        [0, 1, 1, 2]
    );
    // probe/CLK's offset: 3 x 5 + 24 = 39; its span 3 x 1,005..38,505 + 924.
    assert_eq!(h.channels[1].shaping.offset.as_ref().unwrap().ticks, 39);
    assert_eq!(
        kepts(&h),
        [
            (900, 72_900),
            (3_939, 116_439),
            (3_900, 116_400),
            (0, 3_000)
        ]
    );
    // M's edges at 2,255, 3,505 and 4,755: 3 t + 924.
    assert_eq!(edges[1], [7_689, 11_439, 15_189]);
    assert_eq!(h.length, 116_439);
    // Each source's factor follows from the timebases: 36 GHz over 500 MHz is 72.
    assert_eq!(
        escapement_tickwave_format::header::factor(h.timebase, h.sources[0].timebase),
        Some(72)
    );
}

/// A verge-like input: V and I as integer codes, and P their product.
fn verge(stem_v: &str) -> Recording {
    let timebase = Ratio::integer(96_000_000).unwrap();
    let int = |den| IntCoding {
        scale: Exact::new(1, den),
        ..IntCoding::new(32, false)
    };
    let analog = |name: &str, coding: IntCoding, values: Vec<u64>| Channel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: ChannelData::Analog(Analog {
            unit: "V".into(),
            first: 0,
            period: Ratio::integer(960).unwrap(),
            count: values.len() as u64,
            breaks: Vec::new(),
            significant: Some(4),
            samples: Samples::U64(coding, Box::new(values.into_iter())),
        }),
        shaping: Shaping::default(),
    };
    Recording {
        timebase,
        length: 9_600,
        channels: vec![
            analog(stem_v, int(1000), vec![3300, 3301, 3302]),
            analog("I", int(1_000_000_000), vec![10, 20, 30]),
        ],
        derived: vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            product: ["I".into(), stem_v.into()],
            significant: None,
        }],
        sources: vec![source("verge-energy", timebase)],
    }
}

#[test]
fn names_clash_apart_and_derived_channels_follow_their_operands() {
    // Two inputs named alike: the second's default names take its stem, and a third clash on
    // the same stem takes `-2`.
    let a = file(verge("V"));
    let (out, merged) = run(
        &[
            (a.clone(), "verge", None),
            (a.clone(), "verge", None),
            (a.clone(), "verge", None),
        ],
        &settings(vec![]),
    )
    .unwrap();
    let h = Footer::parse_file(&out).unwrap().header;
    let names: Vec<&str> = h.channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        ["V", "I", "verge/V", "verge/I", "verge-2/V", "verge-2/I"]
    );
    // Each input's P carried, its operands renamed with them and its own name made unique.
    let derived: Vec<(&str, [&str; 2])> = h
        .derived
        .iter()
        .map(|d| {
            (
                d.name.as_str(),
                [d.product[0].as_str(), d.product[1].as_str()],
            )
        })
        .collect();
    assert_eq!(
        derived,
        [
            ("P", ["I", "V"]),
            ("verge/P", ["verge/I", "verge/V"]),
            ("verge-2/P", ["verge-2/I", "verge-2/V"])
        ]
    );
    assert!(merged.notes.is_empty());
    // Codings come over as the inputs have them.
    assert!(matches!(
        h.channels[2].coding(),
        Some(escapement_tickwave_format::header::AnalogCoding::Int(_))
    ));
    // An explicit name that another channel has is refused.
    let explicit = settings(vec![
        InputSettings::default(),
        InputSettings {
            align: None,
            channels: vec![
                ("V".into(), Some("I".into())),
                ("I".into(), Some("J".into())),
            ],
        },
    ]);
    let err = run(
        &[(a.clone(), "one", None), (a.clone(), "two", None)],
        &explicit,
    )
    .err()
    .unwrap();
    assert_eq!(
        err.to_string(),
        "input 2 (two): \"I\" is the name of another channel of the merge"
    );
    // P left out: its operand I not taken, then its operands moved apart.
    let only_v = settings(vec![
        InputSettings::default(),
        InputSettings {
            align: None,
            channels: vec![("V".into(), Some("V2".into()))],
        },
    ]);
    let (out, merged) = run(
        &[(a.clone(), "one", None), (a.clone(), "two", None)],
        &only_v,
    )
    .unwrap();
    assert_eq!(Footer::parse_file(&out).unwrap().header.derived.len(), 1);
    assert_eq!(
        merged
            .notes
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>(),
        ["input 2 (two): derived channel P left out: its operand I is not taken"]
    );
    let apart = Settings {
        offsets: vec![("two/V".into(), "8".into())],
        ..settings(vec![])
    };
    let (out, merged) = run(
        &[(a.clone(), "one", None), (a.clone(), "two", None)],
        &apart,
    )
    .unwrap();
    assert_eq!(Footer::parse_file(&out).unwrap().header.derived.len(), 1);
    assert_eq!(
        merged
            .notes
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>(),
        ["input 2 (two): derived channel P left out: its operands I and V are moved or cut apart"]
    );
    // An alignment moves both, and P stays.
    let (out, _) = run(
        &[(a.clone(), "one", None), (a, "two", None)],
        &settings(vec![InputSettings::default(), aligned("100")]),
    )
    .unwrap();
    assert_eq!(Footer::parse_file(&out).unwrap().header.derived.len(), 2);
}

#[test]
fn every_refusal_says_plainly_what_it_refuses() {
    let a = || (file(analyser()), "analyser", None);
    let p = || (file(probe()), "probe", None);
    let refused =
        |files: &[File<'_>], s: &Settings| run(files, s).err().expect("refused").to_string();
    assert_eq!(
        refused(&[a()], &settings(vec![])),
        "a merge takes at least two recordings; 1 given"
    );
    assert_eq!(
        refused(&[a(), p()], &settings(vec![InputSettings::default(); 3])),
        "settings for input 3, but 2 inputs"
    );
    let take = |channels: Vec<(&str, Option<&str>)>| InputSettings {
        align: None,
        channels: channels
            .into_iter()
            .map(|(n, m)| (n.to_owned(), m.map(str::to_owned)))
            .collect(),
    };
    assert_eq!(
        refused(&[a(), p()], &settings(vec![take(vec![("X", None)])])),
        "input 1 (analyser) has no channel \"X\"; it has [\"CLK\"]"
    );
    assert_eq!(
        refused(
            &[a(), p()],
            &settings(vec![take(vec![("CLK", None), ("CLK", Some("C2"))])])
        ),
        "input 1 (analyser): channel \"CLK\" is taken twice"
    );
    // A merged input whose probe source would keep nothing.
    assert_eq!(
        refused(
            &[(merged_input(), "m", None), p()],
            &settings(vec![take(vec![("CLK", None)])])
        ),
        "input 1 (m): its source 1 (probe) keeps no channel; take at least one of its channels"
    );
    let with = |f: &dyn Fn(&mut Settings)| {
        let mut s = settings(vec![]);
        f(&mut s);
        s
    };
    assert_eq!(
        refused(
            &[a(), p()],
            &with(&|s| s.offsets = vec![("Y".into(), "1".into())])
        ),
        "an offset for \"Y\": the merge has no channel by that name; it has [\"CLK\", \"probe/CLK\", \"I\"]"
    );
    assert_eq!(
        refused(
            &[a(), p()],
            &with(&|s| s.offsets = vec![("I".into(), "1".into()), ("I".into(), "2".into())])
        ),
        "\"I\" is given an offset twice"
    );
    assert_eq!(
        refused(
            &[a(), p()],
            &with(&|s| s.units = vec![("CLK".into(), "V".into())])
        ),
        "a unit for CLK: it is digital"
    );
    // 1 ps at 12 GHz is 0.012 of a tick.
    assert_eq!(
        refused(
            &[a(), p()],
            &with(&|s| s.offsets = vec![("I".into(), "1 ps".into())])
        ),
        "I: \"1 ps\" is 0.012 merged ticks, more than 0.01 of a merged tick off the grid"
    );
    assert_eq!(
        refused(
            &[a(), p()],
            &with(&|s| s.offsets = vec![("I".into(), "1 parsec".into())])
        ),
        "I: \"1 parsec\": unknown unit \"parsec\""
    );
    assert_eq!(
        refused(
            &[a(), p()],
            &with(&|s| s.offsets = vec![("I".into(), "9007199254740992".into())])
        ),
        "I: 9007199254740992 ticks is past 2^53"
    );
    assert_eq!(
        refused(
            &[a(), p()],
            &with(&|s| s.keeps = vec![("CLK".into(), Some("500".into()), Some("400".into()))])
        ),
        "CLK: kept from tick 500 to 400: its from must come before its to"
    );
    assert_eq!(
        refused(
            &[a(), p()],
            &with(&|s| s.keeps = vec![("CLK".into(), Some("2000".into()), None)])
        ),
        "CLK: kept from tick 2000 to the end, which leaves nothing of the ticks its input holds, 0 to 1000"
    );
    // I's samples sit at 0, 1, 3, 4, ...: from tick 1 only sample 1, which isn't on the grid of
    // a 3/2 period, comes before 2.
    assert_eq!(
        refused(
            &[a(), p()],
            &with(&|s| s.keeps = vec![("I".into(), Some("1".into()), Some("2".into()))])
        ),
        "I: kept from tick 1 to 2; its samples keep their ticks only from one a multiple of 2 into its run, and none in that span is"
    );
    // Rates whose multiple is past 2^53.
    let rate = |n: u64| {
        let tb = Ratio::integer(n).unwrap();
        (
            file(Recording {
                timebase: tb,
                length: 10,
                channels: vec![digital("A", false, vec![], 10)],
                derived: Vec::new(),
                sources: vec![source("r", tb)],
            }),
            "r",
            None,
        )
    };
    assert_eq!(
        refused(
            &[rate((1 << 52) + 1), rate((1 << 52) - 1)],
            &settings(vec![])
        ),
        "the inputs tick 4503599627370497/1, 4503599627370495/1: their least common multiple is past 2^53"
    );
    // A break at the tick of the sample before it, which twice the rate puts after it: samples at
    // 0 and 1 (1.5 floored), the run after starting at 1. At a factor of 2 the period is 3, so
    // sample 1 sits at 3, past the break at 2.
    let broken_data = || {
        ChannelData::Analog(Analog {
            unit: String::new(),
            first: 0,
            period: Ratio::new(3, 2).unwrap(),
            count: 3,
            breaks: vec![Break {
                index: 2,
                tick: 1,
                lost: 0,
            }],
            significant: None,
            samples: Samples::F32(Box::new([0.0, 1.0, 2.0].into_iter())),
        })
    };
    let broken_channel = || Channel {
        name: "W".into(),
        source_name: "w".into(),
        source: 0,
        data: broken_data(),
        shaping: Shaping::default(),
    };
    let broken_recording = || {
        let tb = Ratio::integer(1_000).unwrap();
        Recording {
            timebase: tb,
            length: 10,
            channels: vec![broken_channel()],
            derived: Vec::new(),
            sources: vec![source("w", tb)],
        }
    };
    let broken = broken_recording();
    assert_eq!(
        refused(&[(file(broken), "w", None), rate(2_000)], &settings(vec![])),
        "W: its run before sample 2 ends at merged tick 3, after the run that starts there at 2: the merged rate puts the input's sample before the break later than the break"
    );
    // At a period of 1 the same break stays on the sample before it, at merged tick 2, which
    // FORMAT allows: a break's tick is at least its sample before's.
    let level = Recording {
        channels: vec![Channel {
            data: ChannelData::Analog(Analog {
                period: Ratio::ONE,
                samples: Samples::F32(Box::new([0.0, 1.0, 2.0].into_iter())),
                ..match broken_data() {
                    ChannelData::Analog(a) => a,
                    ChannelData::Digital(_) => unreachable!(),
                }
            }),
            ..broken_channel()
        }],
        ..broken_recording()
    };
    let (out, _) = run(&[(file(level), "w", None), rate(2_000)], &settings(vec![])).unwrap();
    let (h, (_, samples)) = items(&out);
    let ChannelKind::Analog { breaks, .. } = &h.channels[0].data else {
        panic!()
    };
    assert_eq!(
        breaks.iter().map(|b| (b.index, b.tick)).collect::<Vec<_>>(),
        [(2, 2)]
    );
    assert_eq!(
        samples[0].iter().map(|(t, _)| *t).collect::<Vec<_>>(),
        [0, 2, 2]
    );
    // A 1 Hz input beside a 2^20 Hz one has a factor of 2^20: a period of 2^40 ticks becomes 2^60.
    let slow = |period: u64| {
        let tb = Ratio::ONE;
        file(Recording {
            timebase: tb,
            length: 10,
            channels: vec![Channel {
                name: "S".into(),
                source_name: "s".into(),
                source: 0,
                data: ChannelData::Analog(Analog {
                    unit: String::new(),
                    first: 0,
                    period: Ratio::integer(period).unwrap(),
                    count: 1,
                    breaks: Vec::new(),
                    significant: None,
                    samples: Samples::F32(Box::new([0.0].into_iter())),
                }),
                shaping: Shaping::default(),
            }],
            derived: Vec::new(),
            sources: vec![source("slow", tb)],
        })
    };
    assert_eq!(
        refused(
            &[(slow(1 << 40), "slow", None), rate(1 << 20)],
            &settings(vec![])
        ),
        "S: its period times 1048576 is past 2^53"
    );
    // A rate of 2^30 Hz beside one of 1/2^30 Hz: their multiple is 2^30 Hz, within 2^53, but each
    // of the slow input's ticks is 2^60 merged ticks.
    let tiny = Ratio::new(1, 1 << 30).unwrap();
    let crawl = file(Recording {
        timebase: tiny,
        length: 10,
        channels: vec![digital("C", false, vec![], 10)],
        derived: Vec::new(),
        sources: vec![source("crawl", tiny)],
    });
    assert_eq!(
        refused(&[rate(1 << 30), (crawl, "crawl", None)], &settings(vec![])),
        "input 2 ticks 1/1073741824: each of its ticks is 1152921504606846976 merged ticks, past 2^53"
    );
    // A merge offset that puts I's end past 2^53.
    assert_eq!(
        refused(
            &[a(), p()],
            &with(&|s| s.offsets = vec![("I".into(), "9007199254740000".into())])
        ),
        "the merged recording would run to tick 9007199254777500, past 2^53"
    );
}

/// A 1 kHz recording of one digital channel S over 10 ticks, with nothing in it but what its
/// header states: S's offset of `ticks` of `kind`, and its source's origin and align.
fn declared(kind: &str, ticks: i64, origin: i64, align: Option<i64>) -> Vec<u8> {
    use escapement_tickwave_format::header::{
        Declaration, DeclaredChannel, DeclaredKind, SourceHeader,
    };
    use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
    let tb = Ratio::integer(1_000).unwrap();
    let declaration = Declaration {
        timebase: tb,
        channels: vec![DeclaredChannel {
            name: "S".into(),
            source_name: "s".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Shaping {
                offset: Some(Offset {
                    ticks,
                    kind: kind.into(),
                }),
                kept: Some(Kept { from: 0, to: 10 }),
            },
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader {
            origin,
            align,
            ..SourceHeader::new(SourceFormat::Other("s".into()), tb)
        }],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
    };
    let mut w = Writer::new(declaration, WriteOptions::default()).unwrap();
    let mut out = w.take_output();
    out.extend(w.finish(10, &[10], Finish::default()).unwrap().tail);
    out
}

#[test]
fn an_offset_of_another_kind_than_delay_takes_no_merge_offset() {
    let skewed = |kind: &str| declared(kind, 2, 0, None);
    let s = Settings {
        offsets: vec![("S".into(), "1".into())],
        ..settings(vec![])
    };
    let err = run(
        &[(skewed("skew"), "s", None), (file(analyser()), "a", None)],
        &s,
    )
    .err()
    .unwrap();
    assert_eq!(
        err.to_string(),
        "S: its input states a \"skew\" offset, which the merge's own offset can't add to"
    );
    // A kind it doesn't move keeps, scaled: 500 kHz over 1 kHz... the factor here is 500,000.
    let (out, _) = run(
        &[(skewed("skew"), "s", None), (file(analyser()), "a", None)],
        &settings(vec![]),
    )
    .unwrap();
    let h = Footer::parse_file(&out).unwrap().header;
    assert_eq!(
        h.channels[0].shaping.offset,
        Some(Offset {
            ticks: 1_000_000,
            kind: "skew".into()
        })
    );
}

#[test]
fn ticks_past_2_53_on_the_way_to_the_merged_recording_are_refused() {
    let refused =
        |files: &[File<'_>], s: &Settings| run(files, s).err().expect("refused").to_string();
    let a = || (file(analyser()), "analyser", None);
    // Beside the analyser, S's factor is 500,000: its input offset of 2^50 ticks, and its
    // source's origin or align of 2^50, are each past 2^53 merged ticks.
    assert_eq!(
        refused(
            &[(declared("delay", 1 << 50, 0, None), "s", None), a()],
            &settings(vec![])
        ),
        "S's offset: 562949953421312000000 ticks is past 2^53"
    );
    assert_eq!(
        refused(
            &[(declared("delay", 2, 1 << 50, None), "s", None), a()],
            &settings(vec![])
        ),
        "input 1's source 0's origin: 562949953421312000000 ticks is past 2^53"
    );
    assert_eq!(
        refused(
            &[(declared("delay", 2, 0, Some(1 << 50)), "s", None), a()],
            &settings(vec![])
        ),
        "input 1's source 0's align: 562949953421312000000 ticks is past 2^53"
    );
    // The analyser's CLK moved almost 2^53 earlier and the probe aligned almost 2^53 later: CLK
    // starts the recording, so the probe lies twice that in.
    let s = Settings {
        offsets: vec![("CLK".into(), "-9007199254740000".into())],
        ..settings(vec![InputSettings::default(), aligned("9007199254740000")])
    };
    assert_eq!(
        refused(&[a(), (file(probe()), "probe", None)], &s),
        "input 2's move: 18014398509480000 ticks is past 2^53"
    );
    // The check: a header merged from S with an offset of 2 and origin and align 0, given an S
    // whose offset, origin or align is 2^50, recovers -500,000 x 2^50 or so.
    let small = declared("delay", 2, 0, None);
    let (out, _) = run(&[(small, "s", None), a()], &settings(vec![])).unwrap();
    let h = Footer::parse_file(&out).unwrap().header;
    let against = |s: Vec<u8>| -> Vec<Recording> {
        [s, file(analyser())]
            .iter()
            .map(|b| read(Arc::new(b.clone())).unwrap().1)
            .collect()
    };
    let check = |s: Vec<u8>| expected(&h, against(s)).err().unwrap().to_string();
    assert_eq!(
        check(declared("delay", 2, 1 << 50, None)),
        "input 1's move: -562949953421312000000 ticks is past 2^53"
    );
    assert_eq!(
        check(declared("delay", 2, 0, Some(1 << 50))),
        "input 1's align: -562949953421312000000 ticks is past 2^53"
    );
    assert_eq!(
        check(declared("delay", 1 << 50, 0, None)),
        "S's offset: -562949953421311000000 ticks is past 2^53"
    );
    // And a recording ticking 1/2^47 Hz given where a merge at 2^13 Hz had one at 1 Hz: the lcm
    // still holds, but each of its ticks would be 2^60 merged ticks.
    let at = |tb: Ratio| {
        file(Recording {
            timebase: tb,
            length: 10,
            channels: vec![digital("C", false, vec![], 10)],
            derived: Vec::new(),
            sources: vec![source("c", tb)],
        })
    };
    let fast = at(Ratio::integer(1 << 13).unwrap());
    let (out, _) = run(
        &[(fast.clone(), "fast", None), (at(Ratio::ONE), "slow", None)],
        &settings(vec![]),
    )
    .unwrap();
    let h = Footer::parse_file(&out).unwrap().header;
    let against = [fast, at(Ratio::new(1, 1 << 47).unwrap())]
        .iter()
        .map(|b| read(Arc::new(b.clone())).unwrap().1)
        .collect();
    assert_eq!(
        expected(&h, against).err().unwrap().to_string(),
        "input 2 ticks 1/140737488355328: each of its ticks is 1152921504606846976 merged ticks, past 2^53"
    );
}

/// A sidecar as a recorder bundles it: generator `dec`'s annotations, at the input's ticks. A
/// writer takes times in ticks only, so `text_lines` (a time with a unit, say) and `hand`, more of
/// the hand-written file, go into the bundle after it.
fn bundle_of(
    recording: Recording,
    lines: &[serde_json::Value],
    text_lines: &[&str],
    hand: Option<&str>,
) -> Vec<u8> {
    let annotations = lines
        .iter()
        .map(|l| ("dec".to_owned(), l.as_object().unwrap().clone()))
        .collect();
    let bytes = encode(
        recording,
        &Options {
            level: 3,
            annotations,
            bundle: Some(Bundle::Generated { stem: "in".into() }),
            ..Options::default()
        },
    )
    .unwrap();
    let footer = Footer::parse_file(&bytes).unwrap();
    let mut files = footer.header.annotations.clone().unwrap();
    for line in text_lines {
        let jsonl = files.get_mut("in.dec.jsonl").unwrap();
        jsonl.push_str(line);
        jsonl.push('\n');
    }
    if let Some(hand) = hand {
        files.get_mut("in.annotations.toml").unwrap().push_str(hand);
    }
    escapement_tickwave_format::bundle::bundle(&bytes, &footer, files).unwrap()
}

#[test]
fn generated_annotations_come_over_mapped_and_hand_written_ones_are_counted() {
    use serde_json::json;
    let a = bundle_of(
        analyser(),
        &[json!({"id": "edge/100", "kind": "edge", "start": 100, "end": 200, "channels": ["CLK"]})],
        &[],
        None,
    );
    let p = bundle_of(
        probe(),
        &[
            // Every channel of the probe: its CLK and I under their merged names.
            json!({"id": "all/10", "start": 10}),
            json!({"id": "two/20", "start": 20, "end": 30, "channels": ["CLK", "I"],
                   "measure": {"channel": "CLK", "what": "rate"}}),
            // On a channel the merge leaves out, and a measure on it.
            json!({"id": "clk/40", "start": 40, "channels": ["CLK"]}),
            json!({"id": "i/50", "start": 50, "end": 60, "channels": ["I"],
                   "measure": {"channel": "CLK", "what": "rate"}}),
        ],
        // A time with a unit at the probe's rate: 125 ns is 12 of its ticks.
        &[r#"{"id": "late/12", "start": "125 ns", "channels": ["I"]}"#],
        Some(
            "\n[[annotation]]\nid = \"by-hand\"\nstart = 5\n\n[[annotation]]\nid = \"also\"\nstart = 6\n",
        ),
    );
    // The probe takes only I, aligned 1,000 later.
    let s = Settings {
        stem: "merged".into(),
        ..settings(vec![
            InputSettings::default(),
            InputSettings {
                align: Some("1000".into()),
                channels: vec![("I".into(), None)],
            },
        ])
    };
    let (out, merged) = run(&[(a, "analyser", None), (p, "my probe", None)], &s).unwrap();
    let h = Footer::parse_file(&out).unwrap().header;
    let files = h.annotations.clone().expect("bundled");
    assert_eq!(
        files.keys().collect::<Vec<_>>(),
        [
            "merged.annotations.toml",
            "merged.dec.jsonl",
            "merged.my_probe-dec.jsonl"
        ]
    );
    let included: Vec<_> = files
        .iter()
        .filter(|(n, _)| n.ends_with(".jsonl"))
        .map(|(name, text)| escapement_tickwave_format::annotations::Included { name, text })
        .collect();
    let resolved = escapement_tickwave_format::annotations::resolve(
        &files["merged.annotations.toml"],
        &included,
        &h,
        Some("merged.tickwave"),
    )
    .unwrap();
    let got: Vec<Resolved> = resolved
        .iter()
        .map(|a| {
            (
                a.id.clone(),
                a.start,
                a.end,
                a.channels.clone(),
                a.measure.is_some(),
            )
        })
        .collect();
    let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // The analyser's at 24 t; the probe's at 125 t + 1,000, its generator renamed, whitespace in
    // its stem as `_`.
    assert_eq!(
        got,
        [
            (
                "dec/edge/100".into(),
                2_400,
                Some(4_800),
                s(&["CLK"]),
                false
            ),
            ("my_probe-dec/all/10".into(), 2_250, None, s(&["I"]), false),
            (
                "my_probe-dec/two/20".into(),
                3_500,
                Some(4_750),
                s(&["I"]),
                false
            ),
            (
                "my_probe-dec/i/50".into(),
                7_250,
                Some(8_500),
                s(&["I"]),
                false
            ),
            ("my_probe-dec/late/12".into(), 2_500, None, s(&["I"]), false),
        ]
    );
    let notes: Vec<String> = merged.notes.iter().map(|n| n.to_string()).collect();
    // Its files in name order: the hand-written one, then the generator's.
    assert_eq!(
        notes,
        [
            "input 1 (analyser): 1 generated annotation carried",
            "input 2 (my probe): 2 hand-written annotations left out, since their times are the input's",
            "input 2: generator dec renamed my_probe-dec, since an earlier input's has its name",
            "input 2 (my probe): 4 generated annotations carried",
            "input 2 (my probe): 1 generated annotation left out: none of their channels is taken",
        ]
    );
    // Without annotations to carry, nothing is bundled.
    let (out, _) = run(
        &[
            (file(analyser()), "analyser", None),
            (file(probe()), "probe", None),
        ],
        &settings(vec![]),
    )
    .unwrap();
    assert!(
        Footer::parse_file(&out)
            .unwrap()
            .header
            .annotations
            .is_none()
    );
}

/// Each annotation the bundle of `h` resolves to.
fn resolved_in(h: &Header) -> Vec<Measured> {
    let files = h.annotations.clone().expect("bundled");
    let toml = files
        .keys()
        .find(|n| n.ends_with(".annotations.toml"))
        .unwrap();
    let included: Vec<_> = files
        .iter()
        .filter(|(n, _)| n.ends_with(".jsonl"))
        .map(|(name, text)| escapement_tickwave_format::annotations::Included { name, text })
        .collect();
    escapement_tickwave_format::annotations::resolve(&files[toml], &included, h, None)
        .unwrap()
        .into_iter()
        .map(|a| {
            (
                a.id,
                a.start,
                a.end,
                a.channels,
                a.measure.map(|m| m.channel),
            )
        })
        .collect()
}

#[test]
fn annotations_follow_renamed_channels_and_what_stays_behind_is_noted() {
    use serde_json::json;
    let a = bundle_of(
        analyser(),
        &[json!({"id": "a/100", "start": 100})],
        &[],
        None,
    );
    // The probe's CLK, with a measure on it, and two lines that don't read: no JSON, and a time
    // in an unknown unit.
    let p = bundle_of(
        probe(),
        &[
            json!({"id": "c/10", "start": 10, "end": 20, "channels": ["CLK"],
                 "measure": {"channel": "CLK", "what": "rate"}}),
        ],
        &["not json", r#"{"id": "far", "start": "1 parsec"}"#],
        None,
    );
    // The probe twice under one stem: its CLK is probe/CLK, then probe-2/CLK, and its generator
    // probe-dec, then probe-2-dec.
    let (out, merged) = run(
        &[
            (a, "analyser", None),
            (p.clone(), "probe", None),
            (p.clone(), "probe", None),
        ],
        &settings(vec![]),
    )
    .unwrap();
    let h = Footer::parse_file(&out).unwrap().header;
    let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        resolved_in(&h),
        [
            ("dec/a/100".into(), 2_400, None, s(&["CLK"]), None),
            (
                "probe-2-dec/c/10".into(),
                1_250,
                Some(2_500),
                s(&["probe-2/CLK"]),
                Some("probe-2/CLK".into())
            ),
            (
                "probe-dec/c/10".into(),
                1_250,
                Some(2_500),
                s(&["probe/CLK"]),
                Some("probe/CLK".into())
            ),
        ]
    );
    let notes: Vec<String> = merged.notes.iter().map(|n| n.to_string()).collect();
    assert_eq!(
        notes,
        [
            "input 1 (analyser): 1 generated annotation carried",
            "input 2: generator dec renamed probe-dec, since an earlier input's has its name",
            "input 2 (probe): 1 generated annotation carried",
            "input 2 (probe): 2 generated annotations left out: they do not read",
            "input 3: generator dec renamed probe-2-dec, since an earlier input's has its name",
            "input 3 (probe): 1 generated annotation carried",
            "input 3 (probe): 2 generated annotations left out: they do not read",
        ]
    );

    // The probe kept to its first 100 ticks, so the analyser's end, 24,000, ends the recording:
    // a span from the probe's 150 to 250 ends there, and a point at 250 or at 2^60 lies past it.
    let p = bundle_of(
        probe(),
        &[
            json!({"id": "span/150", "start": 150, "end": 250}),
            json!({"id": "point/250", "start": 250}),
        ],
        &[
            r#"{"id": "far/2-60", "start": 1152921504606846976}"#,
            r#"{"id": "long/160", "start": 160, "end": 1152921504606846976}"#,
        ],
        None,
    );
    let s_keep = Settings {
        keeps: vec![
            ("probe/CLK".into(), None, Some("100".into())),
            ("I".into(), None, Some("100".into())),
        ],
        ..settings(vec![])
    };
    let (out, merged) = run(
        &[(file(analyser()), "analyser", None), (p, "probe", None)],
        &s_keep,
    )
    .unwrap();
    let h = Footer::parse_file(&out).unwrap().header;
    assert_eq!(h.length, 24_000);
    assert_eq!(
        resolved_in(&h),
        [
            (
                "dec/span/150".into(),
                18_750,
                Some(24_000),
                s(&["probe/CLK", "I"]),
                None
            ),
            (
                "dec/long/160".into(),
                20_000,
                Some(24_000),
                s(&["probe/CLK", "I"]),
                None
            )
        ]
    );
    let notes: Vec<String> = merged.notes.iter().map(|n| n.to_string()).collect();
    assert_eq!(
        notes,
        [
            "input 2 (probe): 2 generated annotations carried",
            "input 2 (probe): 2 generated annotations left out: they lie past the merged recording's end"
        ]
    );

    // A sidecar's generator file read past blank lines before its head, and a file whose head is
    // of another format, which is no generator's and no hand-written file either.
    let files = Footer::parse_file(&bundle_of(
        analyser(),
        &[json!({"id": "e/100", "start": 100, "channels": ["CLK"]})],
        &[],
        None,
    ))
    .unwrap()
    .header
    .annotations
    .unwrap();
    let dec = &files["in.dec.jsonl"];
    let (head, rest) = dec.split_once('\n').unwrap();
    let with_head = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut head: serde_json::Value = serde_json::from_str(head).unwrap();
        edit(&mut head);
        format!("{head}\n{rest}")
    };
    let mut sidecar = files.clone();
    sidecar.insert(
        "in.blank.jsonl".into(),
        format!("\n  \n{}", with_head(&|h| h["source"] = "blank".into())),
    );
    sidecar.insert(
        "in.other.jsonl".into(),
        with_head(&|h| {
            h["source"] = "other".into();
            h["format"] = "tickwave-annotations/9".into();
        }),
    );
    let (out, merged) = run(
        &[
            (file(analyser()), "analyser", Some(sidecar)),
            (file(probe()), "probe", None),
        ],
        &settings(vec![]),
    )
    .unwrap();
    let h = Footer::parse_file(&out).unwrap().header;
    let ids: Vec<String> = resolved_in(&h).into_iter().map(|a| a.0).collect();
    assert_eq!(ids, ["blank/e/100", "dec/e/100"]);
    let notes: Vec<String> = merged.notes.iter().map(|n| n.to_string()).collect();
    assert_eq!(
        notes,
        [
            "input 1 (analyser): its sidecar's in.other.jsonl does not read, and is left out",
            "input 1 (analyser): 2 generated annotations carried",
        ]
    );
}

#[test]
fn a_sidecar_given_beside_an_input_comes_over_as_a_bundled_one_does() {
    use serde_json::json;
    let bundled = bundle_of(
        analyser(),
        &[json!({"id": "edge/100", "start": 100, "channels": ["CLK"]})],
        &[],
        None,
    );
    let files = Footer::parse_file(&bundled)
        .unwrap()
        .header
        .annotations
        .unwrap();
    let (out, _) = run(
        &[
            (file(analyser()), "analyser", Some(files)),
            (file(probe()), "probe", None),
        ],
        &settings(vec![]),
    )
    .unwrap();
    let h = Footer::parse_file(&out).unwrap().header;
    let jsonl = &h.annotations.unwrap()["merged.dec.jsonl"];
    assert!(jsonl.contains(r#""start":2400"#), "{jsonl}");
}

#[test]
fn expected_refuses_inputs_that_do_not_fit_the_header() {
    let inputs = [
        (file(analyser()), "analyser", None),
        (file(probe()), "probe", None),
    ];
    let (out, _) = run(
        &inputs,
        &settings(vec![InputSettings::default(), aligned("1000")]),
    )
    .unwrap();
    let h = Footer::parse_file(&out).unwrap().header;
    let recordings = || -> Vec<Recording> {
        inputs
            .iter()
            .map(|(b, ..)| read(Arc::new(b.clone())).unwrap().1)
            .collect()
    };
    // The inputs swapped: the order check names both.
    let mut swapped = recordings();
    swapped.swap(0, 1);
    let err = expected(&h, swapped).err().unwrap();
    assert_eq!(
        err.to_string(),
        "input 1's source 0 is probe [], but the merged recording's source 0 is analyser \
         [digital_0.bin], its tick 0 at 0.5 s: the recordings are given in another order than \
         the merge's, or another recording"
    );
    // Two inputs alike but for when their capture began: swapped, only begin_s tells them apart.
    let later = || {
        let mut r = analyser();
        r.sources[0].begin_s = Some(0.7);
        r
    };
    let twins = [file(analyser()), file(later())];
    let (out2, _) = run(
        &[
            (twins[0].clone(), "early", None),
            (twins[1].clone(), "late", None),
        ],
        &settings(vec![]),
    )
    .unwrap();
    let h2 = Footer::parse_file(&out2).unwrap().header;
    let swapped: Vec<Recording> = twins
        .iter()
        .rev()
        .map(|b| read(Arc::new(b.clone())).unwrap().1)
        .collect();
    assert_eq!(
        expected(&h2, swapped).err().unwrap().to_string(),
        "input 1's source 0 is analyser [digital_0.bin], its tick 0 at 0.7 s, but the merged \
         recording's source 0 is analyser [digital_0.bin], its tick 0 at 0.5 s: the recordings \
         are given in another order than the merge's, or another recording"
    );
    let mut one = recordings();
    one.pop();
    assert_eq!(
        expected(&h, one).err().unwrap().to_string(),
        "the recordings given hold 1 sources, and the merged recording 2"
    );
    // The header changed: an origin, an align, a kept span.
    let changed = |edit: &dyn Fn(&mut Header)| {
        let mut h = h.clone();
        edit(&mut h);
        expected(&h, recordings()).err().map(|e| e.to_string())
    };
    // The probe's origin a tick later: its channels' kept spans no longer sit on its ticks.
    assert_eq!(
        changed(&|h| h.sources[1].origin = 1_001),
        Some("channel probe/CLK: its kept span 1000 to 38500 is not a whole number of its input's ticks".into())
    );
    // Its align a tick less rebuilds, but not to what the file states.
    let mut realigned = h.clone();
    realigned.sources[1].align = Some(999);
    let rebuilt = expected(&realigned, recordings()).unwrap();
    assert_eq!(
        verify(&out[..], rebuilt).unwrap_err().to_string(),
        "source 1: the format, timebase, origin or align differs"
    );
    assert_eq!(
        changed(&|h| h.channels[1].shaping.kept = Some(Kept { from: 1_001, to: 38_501 })),
        Some("channel probe/CLK: its kept span 1001 to 38501 is not a whole number of its input's ticks".into())
    );
    assert_eq!(
        changed(&|h| h.channels[1].shaping.kept = Some(Kept { from: 1_000, to: 38_501 })),
        Some("channel probe/CLK: its kept span 1000 to 38501 is not a whole number of its input's ticks".into())
    );
    assert_eq!(
        changed(&|h| h.channels[1].source_name = "nothing".into()),
        Some("channel probe/CLK: input 2 has no channel \"nothing\" of its source 0".into())
    );
    assert_eq!(
        changed(&|h| h.timebase = Ratio::integer(24_000_000_000).unwrap()),
        Some("the merged recording ticks 24000000000/1, not 12000000000/1, the least common multiple of the recordings given".into())
    );
    // A kept span moved by whole input ticks rebuilds, but not to what the file holds.
    let mut moved = h.clone();
    moved.channels[1].shaping.kept = Some(Kept {
        from: 1_125,
        to: 38_500,
    });
    let rebuilt = expected(&moved, recordings()).unwrap();
    assert!(verify(&out[..], rebuilt).is_err());
}

#[test]
fn a_merged_file_with_an_edge_moved_does_not_verify() {
    let inputs = [
        (file(analyser()), "analyser", None),
        (file(probe()), "probe", None),
    ];
    let (out, _) = run(&inputs, &settings(vec![])).unwrap();
    let (header, mut recording) = read(Arc::new(out.clone())).unwrap();
    // The same header's statements, CLK's second edge one tick later.
    if let ChannelData::Digital(d) = &mut recording.channels[0].data {
        let edges = std::mem::replace(&mut d.edges, Box::new(std::iter::empty()));
        d.edges = Box::new(edges.map(|t| if t == 4_800 { 4_801 } else { t }));
    }
    let tampered = encode(
        recording,
        &Options {
            level: 3,
            analog: header.channels.iter().map(|c| c.coding()).collect(),
            ..Options::default()
        },
    )
    .unwrap();
    let against = inputs
        .iter()
        .map(|(b, ..)| read(Arc::new(b.clone())).unwrap().1)
        .collect();
    let h = Footer::parse_file(&tampered).unwrap().header;
    let err = verify(&tampered[..], expected(&h, against).unwrap()).unwrap_err();
    assert_eq!(err.to_string(), "channel CLK: edge at tick 4801 differs");
}

#[test]
fn a_keep_is_in_its_inputs_time_inside_its_own_span_and_on_its_samples_grid() {
    let inputs = || {
        [
            (file(analyser()), "analyser", None),
            (file(probe()), "probe", None),
        ]
    };
    // 0.2 us at the analyser's 500 MHz is 100 of its ticks: 2,400 merged ticks, not 0.2 us at
    // 12 GHz.
    let s = Settings {
        keeps: vec![("CLK".into(), Some("0.2us".into()), None)],
        ..settings(vec![])
    };
    let (out, _) = run(&inputs(), &s).unwrap();
    assert_eq!(kepts(&items(&out).0)[0], (2_400, 24_000));
    // A keep reaching past the channel's end keeps to its end: the analyser's 1,000 ticks.
    let s = Settings {
        keeps: vec![("CLK".into(), Some("500".into()), Some("5000".into()))],
        ..settings(vec![])
    };
    let (out, _) = run(&inputs(), &s).unwrap();
    let h = items(&out).0;
    assert_eq!(kepts(&h)[0], (12_000, 24_000));
    assert_eq!(digital_of(&h, 0).1, 24_000);
    // I kept from tick 1: sample 1 sits there, off its period's grid of 2, so the cut moves to
    // sample 2 at tick 3, 375 merged ticks.
    let s = Settings {
        keeps: vec![("I".into(), Some("1".into()), None)],
        ..settings(vec![])
    };
    let (out, _) = run(&inputs(), &s).unwrap();
    let (h, (_, samples)) = items(&out);
    assert_eq!(kepts(&h)[2], (375, 37_500));
    assert_eq!(samples[2][0], (375, Sample::F32(2.0)));
    // A keep reaching before an input channel's own span keeps only its span: E is its
    // input's from 210.
    let s = Settings {
        keeps: vec![("E".into(), Some("100".into()), None)],
        ..settings(vec![])
    };
    let (out, _) = run(
        &[(shaped(), "shaped", None), (file(probe()), "probe", None)],
        &s,
    )
    .unwrap();
    assert_eq!(kepts(&items(&out).0)[1], (24 * 210, 24 * 810));
}

#[test]
fn an_input_neither_moved_nor_cut_keeps_its_edges_at_tick_0_and_one_moved_later_takes_them_in() {
    let pulse = || {
        let tb = Ratio::integer(96_000_000).unwrap();
        Recording {
            timebase: tb,
            length: 100,
            channels: vec![digital("P", false, vec![0, 0, 40], 100)],
            derived: Vec::new(),
            sources: vec![source("pulse", tb)],
        }
    };
    let inputs = [
        (file(pulse()), "pulse", None),
        (file(analyser()), "analyser", None),
    ];
    let (out, _) = run(&inputs, &settings(vec![])).unwrap();
    let (h, (edges, _)) = items(&out);
    assert_eq!(edges[0], [0, 0, 5_000]);
    assert_eq!(digital_of(&h, 0), (false, 12_500, vec![]));
    // Aligned 10 later: the lead gap's end can hold no edge, so the pulse is its level, low.
    let (out, _) = run(&inputs, &settings(vec![aligned("10")])).unwrap();
    let (h, (edges, _)) = items(&out);
    assert_eq!(edges[0], [5_010]);
    assert_eq!(digital_of(&h, 0), (false, 12_510, vec![(0, 10, false)]));
}

#[test]
fn gaps_and_breaks_map_with_their_inputs_factor() {
    use escapement_tickwave_format::model::Gap;
    // At 500 MHz beside the probe, a factor of 24. G rises at 100, is unknown from 200 to 400 and
    // high after, and falls at 600. The probe's I pauses: sample 100 starts a run at tick 150, two
    // after sample 99 at 148.
    let tb = Ratio::integer(500_000_000).unwrap();
    let gappy = Recording {
        timebase: tb,
        length: 1_000,
        channels: vec![Channel {
            data: ChannelData::Digital(Digital {
                initial: false,
                end: 1_000,
                edges: Box::new([100, 600].into_iter()),
                gaps: vec![Gap {
                    from: 200,
                    to: 400,
                    level: true,
                }],
            }),
            ..digital("G", false, vec![], 1_000)
        }],
        derived: Vec::new(),
        sources: vec![source("gappy", tb)],
    };
    let mut paused = probe();
    if let ChannelData::Analog(a) = &mut paused.channels[1].data {
        a.breaks = vec![Break {
            index: 100,
            tick: 150,
            lost: 3,
        }];
    }
    let inputs = [(file(gappy), "gappy", None), (file(paused), "paused", None)];
    let (out, _) = run(&inputs, &settings(vec![])).unwrap();
    let (h, (edges, samples)) = items(&out);
    assert_eq!(edges[0], [2_400, 14_400]);
    assert_eq!(
        digital_of(&h, 0),
        (false, 24_000, vec![(4_800, 9_600, true)])
    );
    // The break at 125 x 150; sample 101 a period of 375/2 after it, floored.
    let ChannelKind::Analog { breaks, .. } = &h.channels[2].data else {
        panic!()
    };
    assert_eq!(
        breaks
            .iter()
            .map(|b| (b.index, b.tick, b.lost))
            .collect::<Vec<_>>(),
        [(100, 18_750, 3)]
    );
    assert_eq!(samples[2][100].0, 18_750);
    assert_eq!(samples[2][101].0, 18_937);
    // G kept from 300, inside its gap: the gap joins the lead one from 0, and ends at 24 x 400.
    let s = Settings {
        keeps: vec![("G".into(), Some("300".into()), None)],
        ..settings(vec![])
    };
    let (out, _) = run(&inputs, &s).unwrap();
    let (h, (edges, _)) = items(&out);
    assert_eq!(kepts(&h)[0], (7_200, 24_000));
    assert_eq!(edges[0], [14_400]);
    assert_eq!(digital_of(&h, 0), (false, 24_000, vec![(0, 9_600, true)]));
}

#[test]
fn a_period_times_its_factor_is_in_lowest_terms() {
    // The probe beside a 192 MHz input: a factor of 2, so 3/2 becomes 3.
    let tb = Ratio::integer(192_000_000).unwrap();
    let fast = Recording {
        timebase: tb,
        length: 10,
        channels: vec![digital("F", false, vec![], 10)],
        derived: Vec::new(),
        sources: vec![source("fast", tb)],
    };
    let (out, _) = run(
        &[(file(probe()), "probe", None), (file(fast), "fast", None)],
        &settings(vec![]),
    )
    .unwrap();
    let ChannelKind::Analog { period, .. } = &items(&out).0.channels[1].data else {
        panic!()
    };
    assert_eq!((period.num(), period.den()), (3, 1));
}

#[test]
fn a_unit_is_a_claim_the_check_takes_from_the_header() {
    let s = Settings {
        units: vec![("I".into(), "mA".into())],
        ..settings(vec![])
    };
    let (out, _) = run(
        &[
            (file(analyser()), "analyser", None),
            (file(probe()), "probe", None),
        ],
        &s,
    )
    .unwrap();
    let ChannelKind::Analog { unit, .. } = &items(&out).0.channels[2].data else {
        panic!()
    };
    assert_eq!(unit, "mA");
}

#[test]
fn generated_annotations_before_the_merged_start_are_left_out_or_start_there() {
    use serde_json::json;
    let a = bundle_of(
        analyser(),
        &[
            json!({"id": "point/100", "start": 100}),
            json!({"id": "span/100", "start": 100, "end": 300}),
            // Ends where the recording starts: a span is [start, end), so it holds none of it.
            json!({"id": "touch/100", "start": 100, "end": 200}),
        ],
        &[],
        None,
    );
    // The analyser cut from 200 of its ticks, the probe aligned far later: the analyser's tick
    // 200 starts the recording, so its tick 100 is merged tick -2,400.
    let s = Settings {
        keeps: vec![("CLK".into(), Some("200".into()), None)],
        ..settings(vec![InputSettings::default(), aligned("100000")])
    };
    let (out, merged) = run(&[(a, "analyser", None), (file(probe()), "probe", None)], &s).unwrap();
    let h = Footer::parse_file(&out).unwrap().header;
    let jsonl = &h.annotations.unwrap()["merged.dec.jsonl"];
    let lines: Vec<&str> = jsonl.lines().skip(1).collect();
    assert_eq!(lines.len(), 1, "{jsonl}");
    // The span from 100 to 300 runs from -2,400 to 2,400: it starts at 0.
    assert!(
        lines[0].contains(r#""end":2400"#) && lines[0].contains(r#""start":0"#),
        "{jsonl}"
    );
    assert_eq!(
        merged
            .notes
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>(),
        [
            "input 1 (analyser): 1 generated annotation carried",
            "input 1 (analyser): 2 generated annotations left out: they lie before the merged recording's start"
        ]
    );
}

#[test]
fn expected_refuses_a_header_its_inputs_do_not_give() {
    // Two channels of one source under one source name: which is which can't be told.
    let tb = Ratio::integer(1_000).unwrap();
    let mut twins = Recording {
        timebase: tb,
        length: 10,
        channels: vec![
            digital("A", false, vec![], 10),
            digital("B", false, vec![], 10),
        ],
        derived: Vec::new(),
        sources: vec![source("twins", tb)],
    };
    twins.channels[1].source_name = "a".into();
    let err = run(
        &[
            (file(twins), "twins", None),
            (file(analyser()), "analyser", None),
        ],
        &settings(vec![]),
    )
    .err()
    .unwrap();
    assert_eq!(
        err.to_string(),
        "channel A: input 1 has more than one channel \"a\" of its source 0"
    );
    // A merged input's two sources moved apart.
    let m = merged_input();
    let tb = Ratio::integer(12_000_000_000).unwrap();
    let c_bytes = file(Recording {
        timebase: tb,
        length: 10,
        channels: vec![digital("X", false, vec![], 10)],
        derived: Vec::new(),
        sources: vec![source("third", tb)],
    });
    let (out, _) = run(
        &[(m.clone(), "m", None), (c_bytes.clone(), "third", None)],
        &settings(vec![]),
    )
    .unwrap();
    let h = Footer::parse_file(&out).unwrap().header;
    let recordings = |files: &[&Vec<u8>]| -> Vec<Recording> {
        files
            .iter()
            .map(|b| read(Arc::new((*b).clone())).unwrap().1)
            .collect()
    };
    let mut apart = h.clone();
    apart.sources[1].origin += 1;
    assert_eq!(
        expected(&apart, recordings(&[&m, &c_bytes]))
            .err()
            .unwrap()
            .to_string(),
        "input 1: its sources' origins or aligns state different moves, (0, 0) and (1, 0)"
    );
    // No kept span; a derived channel no input states; a derived channel's operands apart.
    let mut bare = h.clone();
    bare.channels[0].shaping.kept = None;
    assert_eq!(
        expected(&bare, recordings(&[&m, &c_bytes]))
            .err()
            .unwrap()
            .to_string(),
        "channel CLK states no kept span, which a merged recording states on every channel"
    );
    let mut extra = h.clone();
    extra.derived.push(Derived {
        name: "Q".into(),
        unit: "W".into(),
        product: ["I".into(), "I".into()],
        significant: None,
    });
    assert_eq!(
        expected(&extra, recordings(&[&m, &c_bytes]))
            .err()
            .unwrap()
            .to_string(),
        "derived channel Q: its input states no product of its channels I and I in \"W\""
    );
    let v = file(verge("V"));
    // P in another unit, and P over one input's I and the other's V: neither input states it.
    let (out, _) = run(
        &[(v.clone(), "one", None), (v.clone(), "two", None)],
        &settings(vec![]),
    )
    .unwrap();
    let both = Footer::parse_file(&out).unwrap().header;
    let mut unit = both.clone();
    unit.derived[0].unit = "mW".into();
    assert_eq!(
        expected(&unit, recordings(&[&v, &v]))
            .err()
            .unwrap()
            .to_string(),
        "derived channel P: its input states no product of its channels I and V in \"mW\""
    );
    let mut across = both.clone();
    across.derived[0].product = ["I".into(), "two/V".into()];
    assert_eq!(
        expected(&across, recordings(&[&v, &v]))
            .err()
            .unwrap()
            .to_string(),
        "derived channel P: its input states no product of its channels I and two/V in \"W\""
    );
    let (out, _) = run(
        &[(v.clone(), "v", None), (file(analyser()), "a", None)],
        &settings(vec![]),
    )
    .unwrap();
    let mut h = Footer::parse_file(&out).unwrap().header;
    h.channels[0].shaping.offset = Some(Offset {
        ticks: 125,
        kind: Offset::DELAY.into(),
    });
    let k = h.channels[0].shaping.kept.unwrap();
    h.channels[0].shaping.kept = Some(Kept {
        from: k.from + 125,
        to: k.to + 125,
    });
    assert_eq!(
        expected(&h, recordings(&[&v, &file(analyser())]))
            .err()
            .unwrap()
            .to_string(),
        "derived channel P: its operands I and V are moved or cut apart"
    );
}

#[test]
fn each_inputs_title_and_meta_go_with_its_sources_and_a_merged_inputs_keep_their_own() {
    let titled = |recording: Recording, title: &str, meta: &[(&str, &str)]| {
        encode(
            recording,
            &Options {
                level: 3,
                title: Some(title.into()),
                meta: meta.iter().map(|&(k, v)| (k.into(), v.into())).collect(),
                ..Options::default()
            },
        )
        .unwrap()
    };
    let a = titled(analyser(), "wire", &[("leads", "long"), ("probe", "verge")]);
    let b = titled(probe(), "energy", &[]);
    let (m, _) = run(
        &[(a, "analyser", None), (b, "probe", None)],
        &settings(Vec::new()),
    )
    .unwrap();
    let h = Footer::parse_file(&m).unwrap().header;
    // The merged recording's own title and meta are the merge's: none asked for.
    assert_eq!((h.title.as_deref(), h.meta.len()), (None, 0));
    let of = |h: &Header| {
        h.sources
            .iter()
            .map(|s| {
                let meta: Vec<String> = s.meta.iter().map(|(k, v)| format!("{k}={v}")).collect();
                (s.title.clone(), meta)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        of(&h),
        [
            (
                Some("wire".into()),
                vec!["leads=long".into(), "probe=verge".into()]
            ),
            (Some("energy".into()), vec![]),
        ]
    );
    // Merged again under a title of its own, beside a third: its sources keep theirs.
    let tb = Ratio::integer(1_000).unwrap();
    let c = Recording {
        timebase: tb,
        length: 10,
        channels: vec![digital("X", false, vec![1], 10)],
        derived: Vec::new(),
        sources: vec![source("third", tb)],
    };
    let m = {
        let mut head = Footer::parse_file(&m).unwrap();
        head.header.title = Some("both".into());
        escapement_tickwave_format::bundle::bundle(&m, &head, Default::default()).unwrap()
    };
    let (again, _) = run(
        &[(m, "both", None), (titled(c, "third", &[]), "third", None)],
        &settings(Vec::new()),
    )
    .unwrap();
    let h = Footer::parse_file(&again).unwrap().header;
    assert_eq!(
        of(&h).into_iter().map(|(t, _)| t).collect::<Vec<_>>(),
        [
            Some("wire".into()),
            Some("energy".into()),
            Some("third".into())
        ]
    );
}
