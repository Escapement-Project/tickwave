//! The rules the format's v1 freeze added (FORMAT.md at a3ae666), each against what the spec
//! says, worked by hand: header terms in lowest terms, grids, unavailable channels, exact places
//! and runs, the content id's list digests, subtraction, the limits, and the order of records.

use std::collections::BTreeMap;

use escapement_tickwave_format::compress::compress;
use escapement_tickwave_format::decode::{DecodeError, Footer, Index, derived_values, from_memory};
use escapement_tickwave_format::header::{
    AnalogCoding, ChannelKind, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact,
    IntCoding, Kept, Op, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{Break, Gap, Ratio, Sample};
use escapement_tickwave_format::reader::{Ending, Event, Reader};
use escapement_tickwave_format::record::{self, BlockHead, Kind};
use escapement_tickwave_format::samples::{Product, Values};
use escapement_tickwave_format::writer::{Finish, WriteError, WriteOptions, Writer};

mod common;

fn ratio(n: u64, d: u64) -> Ratio {
    Ratio::new(n, d).unwrap()
}

fn analog(
    name: &str,
    coding: AnalogCoding,
    period: Ratio,
    phase: Option<Ratio>,
) -> DeclaredChannel {
    DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period,
            coding,
            significant: None,
            phase,
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

fn int(bits: u8, signed: bool, scale: (i64, i64)) -> AnalogCoding {
    AnalogCoding::Int(IntCoding {
        scale: Exact::new(scale.0, scale.1),
        ..IntCoding::new(bits, signed)
    })
}

fn declaration(channels: Vec<DeclaredChannel>, derived: Vec<Derived>) -> Declaration {
    let timebase = Ratio::integer(1_000_000).unwrap();
    Declaration {
        timebase,
        channels,
        derived,
        sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
        requires: Vec::new(),
    }
}

fn options() -> WriteOptions {
    WriteOptions {
        level: 3,
        ..WriteOptions::default()
    }
}

fn refused(d: Declaration) -> String {
    match Writer::new(d, options()) {
        Ok(_) => panic!("taken"),
        Err(e) => e.to_string(),
    }
}

/// The whole file a writer gives, finished at `length` with every digital channel known to it.
fn finished(mut w: Writer, length: i64, digital: usize) -> Vec<u8> {
    let mut file = w.take_output();
    file.extend(
        w.finish(length, &vec![length; digital], Finish::default())
            .unwrap()
            .tail,
    );
    file
}

/// Every event an in-order reader takes from `file`, or its first error.
fn read_in_order(file: &[u8]) -> Result<(Vec<Event>, Ending), DecodeError> {
    let mut r = Reader::default();
    r.feed(file);
    let mut events = Vec::new();
    while let Some(e) = r.next_event()? {
        events.push(e);
    }
    Ok((events, r.end()?))
}

/// The preamble and declaration a writer of `d` starts its file with.
fn opening(d: Declaration) -> Vec<u8> {
    Writer::new(d, options()).unwrap().take_output()
}

/// A block record of digital channel `c` over `start..end` holding no edge, at `level`.
fn empty_digital_block(c: u32, start: i64, end: i64, level: bool) -> Vec<u8> {
    let frame = compress(&[], 3).unwrap();
    let head = BlockHead {
        channel: c,
        start,
        end,
        word: if level { 1 << 31 } else { 0 },
        decoded_length: 0,
    };
    let mut out = Vec::new();
    record::write(&mut out, Kind::Block, &[&head.to_bytes(), &frame]);
    out
}

/// FORMAT.md, "Header": every `[num, den]` in lowest terms with a positive `den`, so one value has
/// one spelling and one content id; a period at least one tick; a phase below one tick.
#[test]
fn every_header_ratio_is_in_lowest_terms_and_a_period_at_least_a_tick() {
    let f32 = AnalogCoding::F32;
    let one = || declaration(vec![analog("V", f32, Ratio::ONE, None)], vec![]);
    Writer::new(one(), options()).unwrap();
    let mut d = one();
    d.timebase = ratio(2_000_000, 2);
    d.sources[0].timebase = ratio(2_000_000, 2);
    assert_eq!(
        refused(d),
        "the declaration: the timebase is not in lowest terms"
    );
    let mut d = one();
    d.sources[0].timebase = ratio(2_000_000, 2);
    assert!(refused(d).contains("source 0's timebase or stretch is not in lowest terms"));
    for (period, phase, why) in [
        (
            ratio(6, 4),
            None,
            "channel V's period is not in lowest terms, or is below one tick",
        ),
        (
            ratio(1, 2),
            None,
            "channel V's period is not in lowest terms, or is below one tick",
        ),
        (
            Ratio::ONE,
            Some(ratio(2, 4)),
            "channel V's phase is not in lowest terms, or not below one tick",
        ),
        (
            Ratio::ONE,
            Some(ratio(3, 2)),
            "channel V's phase is not in lowest terms, or not below one tick",
        ),
        (
            Ratio::ONE,
            Some(ratio(1, 1)),
            "channel V's phase is not in lowest terms, or not below one tick",
        ),
    ] {
        let d = declaration(vec![analog("V", f32, period, phase)], vec![]);
        assert_eq!(
            refused(d),
            format!("the declaration: {why}"),
            "{period:?} {phase:?}"
        );
    }
    for (scale, offset, why) in [
        (
            Exact::new(2, 4),
            None,
            "channel V's scale is not in lowest terms",
        ),
        (
            None,
            Exact::new(0, 5),
            "channel V's offset is not in lowest terms",
        ),
    ] {
        let coding = AnalogCoding::Int(IntCoding {
            scale,
            offset,
            ..IntCoding::new(12, false)
        });
        let d = declaration(vec![analog("V", coding, Ratio::ONE, None)], vec![]);
        assert_eq!(refused(d), format!("the declaration: {why}"));
    }
    // The reader holds a finished file's header to the same rule: V's period [3,2] stated [6,4].
    let d = declaration(vec![analog("V", f32, ratio(3, 2), None)], vec![]);
    let mut w = Writer::new(d, options()).unwrap();
    w.run(0, 0, 0).unwrap();
    w.sample(0, 1.0f32).unwrap();
    let file = finished(w, 10, 0);
    Footer::parse_file(&file).unwrap();
    let bad = common::patched(&file, r#""period":[3,2]"#, r#""period":[6,4]"#);
    let err = Footer::parse_file(&bad).unwrap_err().to_string();
    assert!(
        err.contains("channel V's period is not in lowest terms"),
        "{err}"
    );
}

/// A channel's grid, the least common multiple of its phase's and its period's denominators,
/// lies below 2^16; a source's `m` is at least 1/2 (FORMAT.md, "Header" and "Times").
#[test]
fn a_grid_lies_below_2_16_and_a_sources_m_at_least_a_half() {
    let at =
        |period, phase| declaration(vec![analog("V", AnalogCoding::F32, period, phase)], vec![]);
    Writer::new(at(ratio(65_536, 65_535), None), options()).unwrap();
    assert!(refused(at(ratio(65_537, 65_536), None)).contains("grid"));
    // 3/256 and a period of 7/256: 256. A phase of 1/3 beside a period of 4/3: 3.
    Writer::new(at(ratio(7, 2), Some(ratio(3, 256))), options()).unwrap();
    // A period's 256 and a phase's 257: 65,792.
    assert!(refused(at(ratio(257, 256), Some(ratio(1, 257)))).contains("grid"));
    // Two sources on an axis of 1,000 ticks a second: one at 3,000, whose m is 1/3.
    let mut d = declaration(vec![digital("A", false), digital("B", false)], vec![]);
    for (c, ch) in d.channels.iter_mut().enumerate() {
        ch.source = c as u32;
        ch.shaping.kept = Some(Kept { from: 0, to: 10 });
    }
    d.timebase = Ratio::integer(1000).unwrap();
    d.sources = vec![
        SourceHeader::new(
            SourceFormat::Other("a".into()),
            Ratio::integer(1000).unwrap(),
        ),
        SourceHeader::new(
            SourceFormat::Other("b".into()),
            Ratio::integer(3000).unwrap(),
        ),
    ];
    assert!(
        refused(d.clone())
            .contains("source 1's m, the axis ticks in one of its ticks, is below 1/2")
    );
    d.sources[1].timebase = Ratio::integer(2000).unwrap();
    Writer::new(d, options()).unwrap();
}

/// FORMAT.md, "Header" and "Unavailable channels": a recording that requires a feature this
/// version doesn't know is refused; a channel that requires one, or states a codec, is kept in
/// place and unavailable, and so is a derived channel over it, one of an operation this version
/// doesn't know, and one of a known operation given `params`. Only what lays out the tables is
/// checked of them. The writer writes none of them.
#[test]
fn an_unknown_feature_codec_or_operation_makes_a_channel_unavailable() {
    let d = declaration(
        vec![
            analog("V", int(16, false, (1, 1000)), Ratio::ONE, None),
            analog("I", int(16, false, (1, 1000)), Ratio::ONE, None),
            digital("D", false),
        ],
        vec![Derived::multiply("P", "W", "I", "V")],
    );
    let mut w = Writer::new(d.clone(), options()).unwrap();
    for c in 0..2 {
        w.run(c, 0, 0).unwrap();
    }
    for i in 0..4u64 {
        w.sample(0, i).unwrap();
        w.sample(1, i + 1).unwrap();
    }
    w.edge(2, 2).unwrap();
    let file = finished(w, 8, 1);
    let footer = Footer::parse_file(&file).unwrap();
    let with = |edit: &dyn Fn(&mut escapement_tickwave_format::header::Header)| {
        let mut h = footer.header.clone();
        edit(&mut h);
        Footer::parse_file(&common::with_header(&file, &h))
    };
    // The recording requires a feature: refused whole.
    let err = with(&|h| h.requires = vec!["frobs".into()])
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "the recording requires frobs, which this reader doesn't know"
    );
    // V requires one: V and P, which is over it, are unavailable; I and D read.
    let f = with(&|h| h.channels[0].requires = vec!["frobs".into()]).unwrap();
    assert_eq!(
        (0..4).map(|o| f.available(o)).collect::<Vec<_>>(),
        [false, true, true, false]
    );
    assert!(f.products[0].is_none());
    let err = f.strip(0).unwrap_err().to_string();
    assert!(
        err.contains("channel V is unavailable to this reader"),
        "{err}"
    );
    assert!(
        f.strip(3)
            .unwrap_err()
            .to_string()
            .contains("channel P is unavailable")
    );
    f.strip(1).unwrap();
    let index = Index::read(f, from_memory(&file)).unwrap();
    assert!(index.block_in(&file, 0, 0).is_err());
    index.block_in(&file, 1, 0).unwrap();
    // A codec: the same.
    let f = with(&|h| h.channels[1].codec = Some("delta".into())).unwrap();
    assert_eq!(
        (f.available(1), f.available(3), f.available(0)),
        (false, false, true)
    );
    // An operation this version doesn't know, of three operands, and a known one given params.
    let f = with(&|h| {
        h.derived[0].op = Op::Other("integrate".into());
        h.derived[0].operands = vec!["I".into(), "V".into(), "D".into()];
    })
    .unwrap();
    assert!(!f.available(3) && f.available(0));
    let f = with(&|h| {
        h.derived[0].params = [("k".to_owned(), "2".to_owned())].into();
    })
    .unwrap();
    assert!(!f.available(3));
    // What is checked of every derived channel: an operand, each naming a channel; and of a
    // known operation over channels this version reads, two of them.
    let err = with(&|h| h.derived[0].operands.clear())
        .unwrap_err()
        .to_string();
    assert_eq!(err, "derived channel P names no operand");
    let err = with(&|h| h.derived[0].operands[1] = "X".into())
        .unwrap_err()
        .to_string();
    assert_eq!(err, "derived channel P: no channel X");
    let err = with(&|h| h.derived[0].operands.push("V".into()))
        .unwrap_err()
        .to_string();
    assert_eq!(err, "derived channel P: multiply takes two operands, not 3");
    // The writer writes none of them.
    let mut coded = d.clone();
    coded.channels[2].codec = Some("delta".into());
    assert!(refused(coded).contains("channel D states a feature or codec"));
    let mut unknown = d.clone();
    unknown.derived[0].op = Op::Other("integrate".into());
    assert!(refused(unknown).contains("derived channel P states an operation"));
    // The header states no zero count of annotation parts.
    let err = with(&|h| h.annotation_parts = Some(0))
        .unwrap_err()
        .to_string();
    assert!(err.contains("states 0 annotation parts"), "{err}");
}

/// A break reads and writes as three terms with a phase of 0, and as four with another.
#[test]
fn a_break_states_its_phase_where_it_has_one() {
    let b = Break::new(5, 30, 2);
    assert_eq!(serde_json::to_string(&b).unwrap(), "[5,30,2]");
    let b = Break { phase: 3, ..b };
    assert_eq!(serde_json::to_string(&b).unwrap(), "[5,30,2,3]");
    assert_eq!(serde_json::from_str::<Break>("[5,30,2,3]").unwrap(), b);
    assert_eq!(serde_json::from_str::<Break>("[5,30,2]").unwrap().phase, 0);
    assert!(serde_json::from_str::<Break>("[5,30,2,3,4]").is_err());
    assert!(serde_json::from_str::<Break>("[5,30]").is_err());
}

/// FORMAT.md, "Header" and "Gaps": sample i of the run that starts at `tick` with phase `f` sits
/// at `tick + f / D + (i − index) × period` exactly, and on the axis at the tick that place maps
/// to. Here a period of 3/2 and a phase of 1/4, a grid of 4: the first run's places are 0.25,
/// 1.75, 3.25; a run from sample 3 at 4 and phase 1/4, 4.25, 5.75.
#[test]
fn a_samples_place_is_exact_on_its_runs_grid() {
    let d = declaration(
        vec![analog(
            "V",
            AnalogCoding::F32,
            ratio(3, 2),
            Some(ratio(1, 4)),
        )],
        vec![],
    );
    let mut w = Writer::new(d, options()).unwrap();
    w.run(0, 0, 0).unwrap();
    for i in 0..3 {
        w.sample(0, i as f32).unwrap();
    }
    w.run_at(0, 4, 1, 0).unwrap();
    w.sample(0, 3.0f32).unwrap();
    w.sample(0, 4.0f32).unwrap();
    let file = finished(w, 20, 0);
    let mut footer = Footer::parse_file(&file).unwrap();
    footer.load_lists(from_memory(&file)).unwrap();
    let ChannelKind::Analog {
        first,
        phase,
        breaks,
        ..
    } = &footer.header.channels[0].data
    else {
        panic!()
    };
    assert_eq!((*first, *phase), (0, Some(ratio(1, 4))));
    assert_eq!(
        breaks,
        &[Break {
            phase: 1,
            ..Break::new(3, 4, 0)
        }]
    );
    // Places in quarters of a tick, and the ticks they map to.
    let places: Vec<i128> = (0..5).map(|i| footer.sample_place(0, i).unwrap()).collect();
    assert_eq!(places, [1, 7, 13, 17, 23]);
    let ticks: Vec<i64> = (0..5)
        .map(|i| footer.sample_axis_tick(0, i).unwrap())
        .collect();
    assert_eq!(ticks, [0, 1, 3, 4, 5]);
    // A sample's time: from its place to the next, cut at the next run's start.
    let grid = footer.grid(0).unwrap();
    assert_eq!(grid.d(), 4);
    assert_eq!(grid.sample_time(0, breaks, 5, 1, None), Some(7..13));
    // Sample 2's next place would be 19, past the run at 17: it ends at 17.
    assert_eq!(grid.sample_time(0, breaks, 5, 2, None), Some(13..17));
    // The last, cut at a kept span's `to` of 6.5 ticks.
    assert_eq!(grid.sample_time(0, breaks, 5, 4, Some(26)), Some(23..26));
    assert_eq!(grid.sample_time(0, breaks, 5, 5, None), None);
}

/// FORMAT.md, "Gaps" and "Gap and run records": each run starts at or after the place of the
/// sample before it, and where it states lost samples, past where the first of them would sit;
/// its phase lies below D, and the first run's is the declared one. The writer refuses the rest,
/// and so does the in-order reader.
#[test]
fn a_run_starts_where_the_rules_put_it_or_is_refused() {
    let d = || {
        declaration(
            vec![analog(
                "V",
                AnalogCoding::F32,
                ratio(3, 2),
                Some(ratio(1, 4)),
            )],
            vec![],
        )
    };
    // Samples 0 and 1 at 0.25 and 1.75; sample 2 on that run would sit at 3.25.
    let started = || {
        let mut w = Writer::new(d(), options()).unwrap();
        w.run(0, 0, 0).unwrap();
        w.sample(0, 0.0f32).unwrap();
        w.sample(0, 1.0f32).unwrap();
        w
    };
    assert!(matches!(
        started().run_at(0, 2, 4, 0),
        Err(WriteError::RunPhase { phase: 4, d: 4, .. })
    ));
    let mut w = Writer::new(d(), options()).unwrap();
    assert!(matches!(
        w.run_at(0, 0, 0, 0),
        Err(WriteError::RunPhase { .. })
    ));
    // At 1.5, before sample 1's place: refused. At 1.75, its place: a pause of no time.
    assert!(matches!(
        started().run_at(0, 1, 2, 0),
        Err(WriteError::RunOrder { .. })
    ));
    started().run_at(0, 1, 3, 0).unwrap();
    // Lost samples: a start at 3.25, where the first would sit, is refused; at 3.5 taken.
    assert!(matches!(
        started().run_at(0, 3, 1, 2),
        Err(WriteError::LostRunStart { lost: 2, .. })
    ));
    let mut w = started();
    w.run_at(0, 3, 2, 2).unwrap();
    w.sample(0, 2.0f32).unwrap();
    let file = finished(w, 20, 0);
    read_in_order(&file).unwrap();
    // The in-order reader, the run record's phase made 1 (3.25, where the loss would start).
    let run_at = file
        .windows(5)
        .rposition(|w| w == [4, 32, 0, 0, 0])
        .expect("the second run record");
    let doctored = rerecorded(&file, run_at, |p| {
        p[28..32].copy_from_slice(&1u32.to_le_bytes())
    });
    let err = read_in_order(&doctored).unwrap_err().to_string();
    assert!(
        err.contains("starts no later than where the first of them would sit"),
        "{err}"
    );
    // A phase of D.
    let doctored = rerecorded(&file, run_at, |p| {
        p[28..32].copy_from_slice(&4u32.to_le_bytes())
    });
    let err = read_in_order(&doctored).unwrap_err().to_string();
    assert_eq!(err, "channel 0: a run is out of place");
    // The first run's phase not the declared one.
    let first_at = file.windows(5).position(|w| w == [4, 32, 0, 0, 0]).unwrap();
    let doctored = rerecorded(&file, first_at, |p| {
        p[28..32].copy_from_slice(&0u32.to_le_bytes())
    });
    let err = read_in_order(&doctored).unwrap_err().to_string();
    assert_eq!(err, "channel 0: a run is out of place");
}

/// `file` with the record at `at` given a payload edited by `edit`, its check made again.
fn rerecorded(file: &[u8], at: usize, edit: impl FnOnce(&mut [u8])) -> Vec<u8> {
    let n = u32::from_le_bytes(file[at + 1..at + 5].try_into().unwrap()) as usize;
    let mut payload = file[at + 5..at + 5 + n].to_vec();
    edit(&mut payload);
    let mut rec = Vec::new();
    record::write(&mut rec, Kind::from_byte(file[at]).unwrap(), &[&payload]);
    let mut out = file.to_vec();
    out[at..at + rec.len()].copy_from_slice(&rec);
    out
}

fn put(h: &mut blake3::Hasher, s: &str) {
    h.update(&(s.len() as u64).to_le_bytes());
    h.update(s.as_bytes());
}

/// FORMAT.md, "Content id", worked by hand: a channel's gaps or breaks go into a digest of their
/// own, which the channel's takes with their count after its end; an analog channel hashes its
/// phase after its period; a derived channel's digest follows the channels'. A gap the end leaves
/// out is in neither.
#[test]
fn the_content_id_takes_each_list_as_a_digest_and_each_derived_channel_after_the_channels() {
    let d = declaration(
        vec![
            digital("D", false),
            analog("A", int(8, false, (1, 1)), ratio(3, 2), Some(ratio(1, 2))),
            analog("B", int(8, false, (1, 1)), ratio(3, 2), Some(ratio(1, 2))),
        ],
        vec![Derived::subtract("S", "V", "A", "B")],
    );
    let mut w = Writer::new(d, options()).unwrap();
    w.edge(0, 3).unwrap();
    w.gap(
        0,
        Gap {
            from: 5,
            to: 8,
            level: true,
        },
    )
    .unwrap();
    w.gap(
        0,
        Gap {
            from: 12,
            to: 40,
            level: false,
        },
    )
    .unwrap();
    for c in 1..3 {
        w.run(c, 0, 0).unwrap();
        w.sample(c, 7u64).unwrap();
        w.run_at(c, 6, 0, 1).unwrap();
        w.sample(c, 9u64).unwrap();
    }
    let mut file = w.take_output();
    // D's end at 30 leaves out the gap from 12, whose block is still open: D is known to 12.
    let f = w.finish(30, &[30], Finish::default()).unwrap();
    file.extend(&f.tail);
    let ChannelKind::Digital { end, gaps, .. } = &f.header.channels[0].data else {
        panic!()
    };
    assert_eq!(
        (*end, gaps.as_slice()),
        (
            12,
            &[Gap {
                from: 5,
                to: 8,
                level: true
            }][..]
        )
    );

    let mut dd = blake3::Hasher::new();
    put(&mut dd, "D");
    put(&mut dd, "digital");
    dd.update(&[0]);
    dd.update(&3i64.to_le_bytes());
    dd.update(&12i64.to_le_bytes());
    dd.update(&1u64.to_le_bytes());
    let mut list = blake3::Hasher::new();
    list.update(&5i64.to_le_bytes());
    list.update(&8i64.to_le_bytes());
    list.update(&[1]);
    dd.update(list.finalize().as_bytes());
    let analog_digest = |name: &str| {
        let mut h = blake3::Hasher::new();
        put(&mut h, name);
        put(&mut h, "analog");
        put(&mut h, "V");
        for v in [3u64, 2, 1, 2] {
            h.update(&v.to_le_bytes());
        }
        put(&mut h, "int");
        h.update(&[8, 0]);
        for v in [
            1i64.to_le_bytes(),
            1u64.to_le_bytes(),
            0i64.to_le_bytes(),
            1u64.to_le_bytes(),
        ] {
            h.update(&v);
        }
        h.update(&7u64.to_le_bytes());
        h.update(&9u64.to_le_bytes());
        h.update(&0i64.to_le_bytes());
        h.update(&2u64.to_le_bytes());
        h.update(&1u64.to_le_bytes());
        let mut list = blake3::Hasher::new();
        list.update(&1u64.to_le_bytes());
        list.update(&6i64.to_le_bytes());
        list.update(&1u64.to_le_bytes());
        list.update(&0u32.to_le_bytes());
        h.update(list.finalize().as_bytes());
        h.finalize()
    };
    let mut s = blake3::Hasher::new();
    put(&mut s, "S");
    put(&mut s, "V");
    put(&mut s, "subtract");
    s.update(&2u64.to_le_bytes());
    put(&mut s, "A");
    put(&mut s, "B");
    s.update(&0u64.to_le_bytes());
    let mut id = blake3::Hasher::new();
    id.update(&1_000_000u64.to_le_bytes());
    id.update(&1u64.to_le_bytes());
    id.update(&30i64.to_le_bytes());
    for digest in [
        dd.finalize(),
        analog_digest("A"),
        analog_digest("B"),
        s.finalize(),
    ] {
        id.update(digest.as_bytes());
    }
    assert_eq!(
        f.header.content_id.to_string(),
        format!("blake3:{}", id.finalize().to_hex())
    );
    // The file reads in order to the same footer, and verify's reader agrees.
    read_in_order(&file).unwrap();
    // Swapping a subtraction's operands, or changing its operation, changes the id.
    let id_with = |edit: &dyn Fn(&mut Derived)| {
        let mut d = declaration(
            vec![
                analog("A", int(8, false, (1, 1)), Ratio::ONE, None),
                analog("B", int(8, false, (1, 1)), Ratio::ONE, None),
            ],
            vec![Derived::subtract("S", "V", "A", "B")],
        );
        edit(&mut d.derived[0]);
        let mut w = Writer::new(d, options()).unwrap();
        for c in 0..2 {
            w.run(c, 0, 0).unwrap();
            w.sample(c, 1u64).unwrap();
        }
        w.finish(4, &[], Finish::default())
            .unwrap()
            .header
            .content_id
    };
    let plain = id_with(&|_| {});
    assert_ne!(plain, id_with(&|d| d.operands.reverse()));
    assert_ne!(plain, id_with(&|d| d.op = Op::Multiply));
    assert_eq!(plain, id_with(&|_| {}));
}

/// FORMAT.md, "tickwave's writer": a gap is hashed once the block holding its end closes, and
/// an end that would leave out a hashed gap is refused; one whose end the overview has taken is
/// refused too. A gap whose end lies before where the channel is settled is refused as it comes.
#[test]
fn a_gap_once_hashed_stays_in_the_header() {
    let d = || declaration(vec![digital("D", false)], vec![]);
    // The gap's block closes at the flush at 50: an end at 30 would leave it out.
    let mut w = Writer::new(d(), options()).unwrap();
    w.edge(0, 3).unwrap();
    w.gap(
        0,
        Gap {
            from: 10,
            to: 40,
            level: true,
        },
    )
    .unwrap();
    w.edge(0, 45).unwrap();
    w.flush(50).unwrap();
    let err = w.finish(60, &[30], Finish::default());
    assert!(matches!(
        err,
        Err(WriteError::EndLeavesOutGap {
            from: 10,
            to: 40,
            end: 30,
            ..
        })
    ));
    // A flush past the gap's end closes the block that holds it, though the channel holds no
    // item: the gap is hashed, and the end can't leave it out either.
    let mut w = Writer::new(d(), options()).unwrap();
    w.gap(
        0,
        Gap {
            from: 10,
            to: 20,
            level: true,
        },
    )
    .unwrap();
    w.flush(50).unwrap();
    let err = w.finish(60, &[15], Finish::default());
    assert!(
        matches!(err, Err(WriteError::EndLeavesOutGap { .. })),
        "{err:?}"
    );
    // Its block still open, and nothing settled past it, the end leaves it out: at a length its
    // `to` reaches.
    let mut w = Writer::new(d(), options()).unwrap();
    w.gap(
        0,
        Gap {
            from: 10,
            to: 20,
            level: true,
        },
    )
    .unwrap();
    w.flush(15).unwrap();
    let f = w.finish(20, &[18], Finish::default()).unwrap();
    let ChannelKind::Digital { end, gaps, .. } = &f.header.channels[0].data else {
        panic!()
    };
    assert_eq!((*end, gaps.len()), (10, 0));
    // A gap left out whose end lies inside the recording is refused: its record is in the file,
    // and the header leaves out only a gap whose `to` maps at or past the length.
    let mut w = Writer::new(d(), options()).unwrap();
    w.gap(
        0,
        Gap {
            from: 10,
            to: 20,
            level: true,
        },
    )
    .unwrap();
    assert!(matches!(
        w.finish(60, &[18], Finish::default()),
        Err(WriteError::GapPastEnd { .. })
    ));
    // After a known-through record at 50, a gap ending at 40 is refused; one ending at 50 taken.
    let mut w = Writer::new(d(), options()).unwrap();
    w.edge(0, 3).unwrap();
    w.flush(50).unwrap();
    assert!(matches!(
        w.gap(
            0,
            Gap {
                from: 10,
                to: 40,
                level: true
            }
        ),
        Err(WriteError::GapLate {
            to: 40,
            closed: 50,
            ..
        })
    ));
    w.gap(
        0,
        Gap {
            from: 10,
            to: 50,
            level: true,
        },
    )
    .unwrap();
    // A flush that closes no block, the channel holding nothing: the gap still can't end before
    // the known-through tick.
    let mut w = Writer::new(d(), options()).unwrap();
    w.flush(50).unwrap();
    assert!(matches!(
        w.gap(
            0,
            Gap {
                from: 10,
                to: 40,
                level: true
            }
        ),
        Err(WriteError::GapLate {
            to: 40,
            closed: 50,
            ..
        })
    ));
}

/// FORMAT.md, "Gap and run records" and "Index": a page of a gap list comes after the block
/// that holds its last gap's end, and a page of a break list after the block that holds its
/// last break's sample, so no page written holds an entry the end leaves out.
#[test]
fn a_list_page_comes_after_the_block_that_holds_its_last_entry() {
    let d = declaration(
        vec![
            digital("D", false),
            analog("V", int(8, false, (1, 1)), Ratio::ONE, None),
        ],
        vec![],
    );
    let mut w = Writer::new(d, options()).unwrap();
    // 4,100 gaps of D, an edge after each, and 4,100 runs of V, each after a pause; a flush every
    // 1,000 ticks.
    w.run(1, 0, 0).unwrap();
    w.sample(1, 0u64).unwrap();
    for k in 0..4100i64 {
        let t = 10 * k + 1;
        w.gap(
            0,
            Gap {
                from: t,
                to: t + 3,
                level: false,
            },
        )
        .unwrap();
        w.edge(0, t + 5).unwrap();
        w.run(1, t + 2, 0).unwrap();
        w.sample(1, 1u64).unwrap();
        if k % 100 == 99 {
            w.flush(t + 9).unwrap();
        }
    }
    let file = finished(w, 50_000, 1);
    let (events, ending) = read_in_order(&file).unwrap();
    assert_eq!(ending, Ending::Complete);
    // Each list's first page, of 4,096 entries, and the blocks before it.
    let mut ends = [0i64; 2];
    let mut pages = Vec::new();
    for e in &events {
        match e {
            Event::Block(b) => ends[b.head.channel as usize] = b.head.end,
            Event::Page { head, .. } if head.table == 2 || head.table == 3 => {
                pages.push((head.table, head.count, ends[head.channel as usize]));
            }
            _ => {}
        }
    }
    // D's gap 4,095 ends at 40,954, in the block closed at the flush at 40,999 (k = 4,099 is
    // the last flush past it); V's break 4,096 has its sample at 40,953. Each page comes once a
    // block reaching past them is in.
    let gaps = pages.iter().find(|p| p.0 == 2).unwrap();
    let breaks = pages.iter().find(|p| p.0 == 3).unwrap();
    assert_eq!(gaps.1, 4096);
    assert!(gaps.2 > 40_954, "{gaps:?}");
    assert_eq!(breaks.1, 4096);
    assert!(breaks.2 > 40_953, "{breaks:?}");
    // Repair follows the pages as a reader does: cut before the footer, it gives the same content.
    let footer = Footer::parse_file(&file).unwrap();
    let at = footer.trailer.footer_offset as usize;
    let fixed = escapement_tickwave_format::repair::repair(&file[..at], 3).unwrap();
    assert_eq!(fixed.header.content_id, footer.header.content_id);
}

/// FORMAT.md, "tickwave's writer": at most 4 MiB of gaps wait, 17 bytes each, for the blocks
/// that hold their ends.
#[test]
fn gaps_waiting_past_4_mib_are_refused() {
    let mut w = Writer::new(declaration(vec![digital("D", false)], vec![]), options()).unwrap();
    let most = (4 << 20) / 17;
    for k in 0..most as i64 {
        w.gap(
            0,
            Gap {
                from: 3 * k,
                to: 3 * k + 1,
                level: false,
            },
        )
        .unwrap();
    }
    let k = most as i64;
    assert!(matches!(
        w.gap(
            0,
            Gap {
                from: 3 * k,
                to: 3 * k + 1,
                level: false
            }
        ),
        Err(WriteError::GapsWaiting { .. })
    ));
    // A flush past them closes the block that holds their ends, though it holds no item: they
    // are hashed, and more fit.
    w.flush(3 * k).unwrap();
    w.gap(
        0,
        Gap {
            from: 3 * k,
            to: 3 * k + 1,
            level: false,
        },
    )
    .unwrap();
}

/// FORMAT.md, "tickwave's writer": the gaps a block has taken that end before a promise count
/// toward its target at 17 bytes each, so a producer that promises never meets the waiting-gap
/// limit; and a flush closes a block that holds a gap's end before its tick. The gaps count only
/// toward the target: an encoder's promise with every item cuts no block a gap.
#[test]
fn a_blocks_gaps_count_toward_its_target_and_a_flush_closes_at_one() {
    let gap = |k: i64| Gap {
        from: 3 * k,
        to: 3 * k + 1,
        level: false,
    };
    // A target of 1,700 bytes: 100 gaps. Gap k from 3k, a promise at 3k + 2 after each: a block
    // closes at the promise after gaps 99, 199, ... 999, and the last at the finish.
    let mut w = Writer::new(
        declaration(vec![digital("D", false)], vec![]),
        WriteOptions {
            block_bytes: 1700,
            ..options()
        },
    )
    .unwrap();
    for k in 0..1000 {
        w.gap(0, gap(k)).unwrap();
        w.promise(3 * k + 2).unwrap();
    }
    let f = w.finish(4000, &[4000], Finish::default()).unwrap();
    assert_eq!(f.header.channels[0].blocks, 11);
    // Gaps that end past the promise don't count: 100 gaps taken ahead, a promise at 5 has two
    // before it, and closes nothing.
    let mut w = Writer::new(
        declaration(vec![digital("D", false)], vec![]),
        WriteOptions {
            block_bytes: 1700,
            ..options()
        },
    )
    .unwrap();
    for k in 0..100 {
        w.gap(0, gap(k)).unwrap();
    }
    w.promise(5).unwrap();
    let f = w.finish(400, &[400], Finish::default()).unwrap();
    assert_eq!(f.header.channels[0].blocks, 1);
    // The encoder, a promise with every edge: 1,000 gaps and edges, one block.
    use escapement_tickwave_format::encode::{Options, encode};
    use escapement_tickwave_format::model::{Channel, ChannelData, Digital, Recording, Source};
    let timebase = Ratio::integer(1_000_000).unwrap();
    let rec = Recording {
        timebase,
        length: 4000,
        channels: vec![Channel {
            name: "D".into(),
            source_name: "d".into(),
            source: 0,
            data: ChannelData::Digital(Digital {
                initial: false,
                end: 4000,
                edges: Box::new((0..1000).map(|k| 3 * k + 2)),
                count: 1000,
                gaps: (0..1000).map(gap).collect(),
            }),
            shaping: Default::default(),
        }],
        derived: Vec::new(),
        sources: vec![Source::new(SourceFormat::Other("t".into()), timebase)],
    };
    let file = encode(rec, &Options::default()).unwrap();
    let footer = Footer::parse_file(&file).unwrap();
    assert_eq!(footer.header.channels[0].blocks, 1);
    // A flush closes the block that holds a gap's end before its tick, a block of no item: the
    // gap from 3 to 4 closes none at a flush at 3 or at its end, one at 5, and none at 10, with
    // nothing new.
    let mut w = Writer::new(declaration(vec![digital("D", false)], vec![]), options()).unwrap();
    w.gap(0, gap(1)).unwrap();
    w.flush(3).unwrap();
    w.flush(4).unwrap();
    w.flush(5).unwrap();
    w.flush(10).unwrap();
    let f = w.finish(20, &[20], Finish::default()).unwrap();
    // Closed at 5, then at the finish.
    assert_eq!(f.header.channels[0].blocks, 2);
}

/// FORMAT.md, "Derived channels": the difference of two `int` channels, `ka × a − kb × b` with
/// `s` the greatest common divisor of their scales, `bits` the fewest that hold every code, and
/// the offsets' difference; f64 otherwise.
#[test]
fn a_difference_of_int_channels_is_exact_on_the_greatest_common_scale() {
    // Scales 1/1000 and 1/1500: s = gcd(1500, 1000) / 1,500,000 = 1/3000, ka 3, kb 2. 16-bit
    // codes: 3 × [0, 65535] − 2 × [0, 65535] = [−131,070, 196,605], 19 bits signed.
    let a = int(16, false, (1, 1000));
    let b = AnalogCoding::Int(IntCoding {
        offset: Exact::new(1, 4),
        ..IntCoding {
            scale: Exact::new(1, 1500),
            ..IntCoding::new(16, false)
        }
    });
    let Product::Difference {
        ka,
        kb,
        bits,
        num,
        den,
        onum,
        oden,
    } = Product::difference(a, b)
    else {
        panic!("an integer difference")
    };
    assert_eq!(
        (ka, kb, bits, num, den, onum, oden),
        (3, 2, 19, 1.0, 3000.0, -1.0, 4.0)
    );
    let rule = Product::difference(a, b);
    // 3 × 1000 − 2 × 600 = 1800 codes of 1/3000: 0.6, less the offset's 1/4.
    assert_eq!(
        rule.sample(Sample::U64(1000), Sample::U64(600)),
        Sample::I64(1800)
    );
    assert_eq!(rule.value(Sample::I64(1800)), 1800.0 / 3000.0 - 0.25);
    // A negative scale: ka negative, the range flips, and the bits hold it.
    let neg = int(8, true, (-1, 2));
    let Product::Difference { ka, kb, bits, .. } = Product::difference(neg, int(8, true, (1, 3)))
    else {
        panic!()
    };
    // s = gcd(1 × 3, 1 × 2) / 6 = 1/6; ka = −3, kb = 2: −3 × [−128, 127] − 2 × [−128, 127] spans
    // [−381 − 254, 384 + 256] = [−635, 640], 11 bits.
    assert_eq!((ka, kb, bits), (-3, 2, 11));
    // Past 64 bits, or of floats: f64.
    let wide = Product::difference(int(64, false, (1, 3)), int(64, false, (1, 2)));
    assert!(matches!(wide, Product::F64Difference { .. }));
    let floats = Product::difference(AnalogCoding::F32, a);
    assert!(matches!(floats, Product::F64Difference { .. }));
    assert_eq!(
        floats.sample(Sample::F32(2.5), Sample::U64(500)),
        Sample::F64(2.5 - 0.5)
    );
    // A writer's file: the derived channel's samples and overview from the operands'.
    let d = declaration(
        vec![
            analog("A", a, Ratio::ONE, None),
            analog("B", b, Ratio::ONE, None),
        ],
        vec![Derived::subtract("S", "V", "A", "B")],
    );
    let mut w = Writer::new(d, options()).unwrap();
    for c in 0..2 {
        w.run(c, 0, 0).unwrap();
    }
    for (x, y) in [(1000u64, 600u64), (0, 65535), (65535, 0)] {
        w.sample(0, x).unwrap();
        w.sample(1, y).unwrap();
    }
    let file = finished(w, 16, 0);
    let index = Index::of_file(&file).unwrap();
    let (s, _, _) = index.footer.products[0].unwrap();
    assert_eq!(s, 0);
    let (_, _, rule) = index.footer.products[0].unwrap();
    let a_block = index.block_in(&file, 0, 0).unwrap();
    let b_block = index.block_in(&file, 1, 0).unwrap();
    assert_eq!(
        derived_values(&a_block.data, &b_block.data, rule).unwrap(),
        Values::I64(vec![1800, -131_070, 196_605])
    );
    let strip = index.footer.strip(2).unwrap();
    let escapement_tickwave_format::overview::AnalogOverview::I64(c) = strip.data.analog().unwrap()
    else {
        panic!("signed codes")
    };
    assert_eq!((c.min[0], c.max[0], c.sum[0]), (-131_070, 196_605, 67_335));
}

/// FORMAT.md, "Limits" and "Records": a record of a kind this version defines is no longer than
/// its kind holds, and an in-order reader refuses one past that before it reads the payload.
#[test]
fn a_record_past_what_its_kind_holds_stops_the_reader_before_its_payload() {
    let d = declaration(vec![digital("D", false)], vec![]);
    let head = opening(d);
    let cut_at = |f: &[u8]| match read_in_order(f).unwrap() {
        (_, Ending::CutOff { at }) => at,
        (_, ending) => panic!("{ending:?}"),
    };
    // A gap of 22 bytes, a known-through record of 9, a run of 28: each record checks, and each
    // stops the reader where it starts, as a record that doesn't check does.
    for (kind, n) in [(3u8, 22u32), (8, 9), (4, 28)] {
        let mut f = head.clone();
        record::write(
            &mut f,
            Kind::from_byte(kind).unwrap(),
            &[&vec![0; n as usize]],
        );
        assert_eq!(cut_at(&f), head.len() as u64, "kind {kind} of {n} bytes");
    }
    // A declaration's 64 MiB and one byte: the file holds no record.
    let mut f = head[..12].to_vec();
    f.push(1);
    f.extend_from_slice(&((64u32 << 20) + 1).to_le_bytes());
    assert_eq!(cut_at(&f), 12);
    // An annotation record of 4 MiB and one byte, with no payload there at all.
    let mut f = head.clone();
    f.push(6);
    f.extend_from_slice(&((4u32 << 20) + 1).to_le_bytes());
    assert_eq!(cut_at(&f), head.len() as u64);
    // A crash's tail of bytes that read as a block record of 2 GiB: repair cuts the file there,
    // and gives what it gives without them.
    let mut w = Writer::new(declaration(vec![digital("D", false)], vec![]), options()).unwrap();
    w.edge(0, 3).unwrap();
    w.flush(10).unwrap();
    w.edge(0, 12).unwrap();
    w.flush(20).unwrap();
    let file = w.take_output();
    let mut tailed = file.clone();
    tailed.push(2);
    tailed.extend_from_slice(&(1u32 << 31).to_le_bytes());
    tailed.extend_from_slice(&[0xa5; 300]);
    assert_eq!(cut_at(&tailed), file.len() as u64);
    let fixed = escapement_tickwave_format::repair::repair(&tailed, 3).unwrap();
    let clean = escapement_tickwave_format::repair::repair(&file, 3).unwrap();
    assert_eq!(fixed.keep, file.len() as u64);
    assert_eq!(fixed.header.content_id, clean.header.content_id);
}

/// FORMAT.md, "Known-through records": every block record before one ends at or before its
/// tick, and a gap record after one has its end at that tick or after.
#[test]
fn a_known_through_record_after_a_longer_block_or_before_a_late_gap_is_refused() {
    let head = opening(declaration(vec![digital("D", false)], vec![]));
    let mut f = head.clone();
    f.extend(empty_digital_block(0, 0, 100, false));
    record::write(
        &mut f,
        Kind::KnownThrough,
        &[&record::known_through_bytes(50)],
    );
    let err = read_in_order(&f).unwrap_err().to_string();
    assert_eq!(
        err,
        "a known-through record at tick 50 comes after a block that ends at 100"
    );
    let mut f = head.clone();
    f.extend(empty_digital_block(0, 0, 100, false));
    record::write(
        &mut f,
        Kind::KnownThrough,
        &[&record::known_through_bytes(100)],
    );
    read_in_order(&f).unwrap();
    // A gap record after it ending at 80, before 100: refused; at 100, taken.
    let gap = |to| {
        record::gap_bytes(
            0,
            Gap {
                from: 10,
                to,
                level: false,
            },
        )
    };
    let mut late = f.clone();
    record::write(&mut late, Kind::Gap, &[&gap(80)]);
    let err = read_in_order(&late).unwrap_err().to_string();
    assert_eq!(err, "channel 0: a gap from 10 to 80 is out of place");
    record::write(&mut f, Kind::Gap, &[&gap(100)]);
    read_in_order(&f).unwrap();
    // With no block before it, the known-through tick alone settles where a gap may end.
    let mut f = head.clone();
    record::write(
        &mut f,
        Kind::KnownThrough,
        &[&record::known_through_bytes(100)],
    );
    record::write(&mut f, Kind::Gap, &[&gap(80)]);
    let err = read_in_order(&f).unwrap_err().to_string();
    assert_eq!(err, "channel 0: a gap from 10 to 80 is out of place");
}

/// FORMAT.md, "Records": a file whose first record is not the declaration is refused whatever
/// its kind, and a record of a kind from 128 up after it is skipped, hashed as it comes without
/// being held, however long.
#[test]
fn an_extension_record_first_is_refused_and_one_after_is_skipped_unheld() {
    let head = opening(declaration(vec![digital("D", false)], vec![]));
    let mut f = head[..12].to_vec();
    f.extend(extension(200, &[1, 2, 3]));
    let err = read_in_order(&f).unwrap_err().to_string();
    assert_eq!(err, "the first record is not the declaration");
    // 1 MiB of an extension record, to a reader holding 64 KiB of a record.
    let mut f = head.clone();
    f.extend(extension(200, &vec![7u8; 1 << 20]));
    f.extend(empty_digital_block(0, 0, 10, false));
    let mut r = Reader::new(64 << 10);
    let mut kinds = Vec::new();
    // Fed in pieces, as a file grows.
    for piece in f.chunks(10_000) {
        r.feed(piece);
        while let Some(e) = r.next_event().unwrap() {
            kinds.push(match e {
                Event::Declaration(_) => 1,
                Event::Skipped { kind } => kind,
                Event::Block(_) => 2,
                _ => 0,
            });
        }
    }
    assert_eq!(kinds, [1, 200, 2]);
    // Its check spoilt: the reader stops there, the file cut off where the record starts.
    let mut bad = f.clone();
    let at = head.len() + 5 + 1000;
    bad[at] ^= 1;
    let mut r = Reader::new(64 << 10);
    r.feed(&bad);
    while r.next_event().unwrap().is_some() {}
    assert_eq!(
        r.end().unwrap(),
        Ending::CutOff {
            at: head.len() as u64
        }
    );
    // Cut off inside it, as a file still being written: cut where it starts.
    let mut r = Reader::new(64 << 10);
    r.feed(&f[..head.len() + 100_000]);
    while r.next_event().unwrap().is_some() {}
    assert_eq!(
        r.end().unwrap(),
        Ending::CutOff {
            at: head.len() as u64
        }
    );
}

/// A record of `kind`, which this version doesn't define, holding `payload`, its check right.
fn extension(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![kind];
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    let check = blake3::Hasher::new()
        .update(&[kind])
        .update(&(payload.len() as u32).to_le_bytes())
        .update(payload)
        .finalize();
    out.extend_from_slice(&check.as_bytes()[..8]);
    out
}

/// FORMAT.md, "Trailer": a reader that reads the preamble too refuses a trailer whose versions
/// differ from it; one that reads only the end takes it.
#[test]
fn a_trailer_whose_versions_differ_from_the_preamble_is_refused_in_order() {
    let mut w = Writer::new(declaration(vec![digital("D", false)], vec![]), options()).unwrap();
    w.edge(0, 3).unwrap();
    let mut file = finished(w, 10, 1);
    read_in_order(&file).unwrap();
    let n = file.len();
    file[n - 10..n - 8].copy_from_slice(&1u16.to_le_bytes());
    Footer::parse_file(&file).unwrap();
    let err = read_in_order(&file).unwrap_err().to_string();
    assert_eq!(err, "the trailer's versions differ from the preamble's");
}

/// FORMAT.md, "Limits": a derived channel's operand runs ahead of its partner by at most 4 MiB
/// of its codes.
#[test]
fn an_operand_more_than_4_mib_ahead_is_refused() {
    let d = declaration(
        vec![
            analog("V", int(32, false, (1, 1000)), Ratio::ONE, None),
            analog("I", int(32, false, (1, 1000)), Ratio::ONE, None),
        ],
        vec![Derived::multiply("P", "W", "I", "V")],
    );
    let mut w = Writer::new(d, options()).unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    for i in 0..(1u64 << 20) {
        w.sample(0, i).unwrap();
    }
    assert!(matches!(
        w.sample(0, 0u64),
        Err(WriteError::OperandAhead { .. })
    ));
    // The partner's sample pairs one, and one more fits once a promise lets V's full block close.
    w.sample(1, 0u64).unwrap();
    w.promise(1).unwrap();
    w.sample(0, 0u64).unwrap();
}

/// FORMAT.md, "Limits": at most 4,096 channels and derived channels together.
#[test]
fn more_than_4096_channels_are_refused() {
    let channels: Vec<_> = (0..4097)
        .map(|c| digital(&format!("D{c}"), false))
        .collect();
    let err = refused(declaration(channels, vec![]));
    assert!(
        err.contains("4097 channels and 0 derived channels, more than the 4096"),
        "{err}"
    );
    let channels: Vec<_> = (0..4096)
        .map(|c| digital(&format!("D{c}"), false))
        .collect();
    Writer::new(declaration(channels, vec![]), options()).unwrap();
}

/// FORMAT.md, "Index": a page's length is what its count gives, checked before the page is
/// fetched, by a reader that walks to a span and by one that reads every page.
#[test]
fn a_page_whose_length_differs_from_its_shape_is_refused_before_it_is_fetched() {
    use escapement_tickwave_format::index::{Entry, Table, TableIndex};
    use std::ops::ControlFlow;
    // 32 bytes of framing and head, then the entries: one page of 5 rows of 28 bytes.
    let entry = Entry {
        first: 0,
        number: 0,
        offset: 100,
        length: 32 + 28 * 5 + 1,
    };
    let t = TableIndex::finished(0, Table::Blocks, 5, 0, vec![entry]);
    let mut fetch = |_: u64, _: u32| -> Result<Vec<u8>, DecodeError> { panic!("fetched") };
    let err = t
        .visit(&mut fetch, |_, _| Ok(ControlFlow::Continue(())))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("is 173 bytes, where its entries take 172"),
        "{err}"
    );
    let err = t.read_all(&mut fetch).unwrap_err().to_string();
    assert!(
        err.contains("page 0 at depth 0 of its blocks is 173 bytes, where its entries take 172"),
        "{err}"
    );
    // 18 pages, more than the top index's 16: a directory listing them, of 18 entries of 28
    // bytes.
    let directory = Entry {
        length: 32 + 28 * 18 + 1,
        ..entry
    };
    let t = TableIndex::finished(0, Table::Blocks, 17 * 4096 + 5, 1, vec![directory]);
    for err in [
        t.read_all(&mut fetch).unwrap_err().to_string(),
        t.pages(0, &mut fetch).unwrap_err().to_string(),
    ] {
        assert!(
            err.contains(
                "page 0 at depth 1 of its blocks is 537 bytes, where its entries take 536"
            ),
            "{err}"
        );
    }
}

/// FORMAT.md, "Derived channels": a derived channel's operands share their breaks. The in-order
/// reader, and so repair and verify, compares each operand's run record with its partner's of the
/// same index, and refuses one that has a run where its partner, past that sample, has none. With
/// the breaks in list pages a header compares only their counts, so a repair would otherwise write
/// operands whose breaks differ.
#[test]
fn a_derived_channels_operands_runs_are_compared_record_by_record() {
    let pair = || {
        declaration(
            vec![
                analog(
                    "V",
                    int(16, false, (1, 1000)),
                    Ratio::integer(10).unwrap(),
                    None,
                ),
                analog(
                    "I",
                    int(16, false, (1, 1000)),
                    Ratio::integer(10).unwrap(),
                    None,
                ),
            ],
            vec![Derived::multiply("P", "W", "I", "V")],
        )
    };
    // 70 runs of two pairs, a pause between: run k from tick 100 k; a flush every ten runs. Cut
    // before the finish, so no header or list page holds the breaks.
    let mut w = Writer::new(pair(), options()).unwrap();
    for k in 0..70i64 {
        for c in 0..2 {
            w.run(c, 100 * k, 0).unwrap();
        }
        for _ in 0..2 {
            w.sample(0, 1u64).unwrap();
            w.sample(1, 2u64).unwrap();
        }
        if k % 10 == 9 {
            w.flush(100 * k + 50).unwrap();
        }
    }
    let cut = w.take_output();
    let clean = escapement_tickwave_format::repair::repair(&cut, 3).unwrap();
    let ChannelKind::Analog { break_count, .. } = &clean.header.channels[1].data else {
        panic!()
    };
    assert_eq!(*break_count, Some(69));
    // I's run at sample 80, tick 4,000, moved a tick on: still a run on its own, but not V's.
    let mut needle = vec![4u8, 32, 0, 0, 0, 1, 0, 0, 0];
    needle.extend_from_slice(&80u64.to_le_bytes());
    let at = cut.windows(needle.len()).position(|w| w == needle).unwrap();
    let moved = rerecorded(&cut, at, |p| {
        p[12..20].copy_from_slice(&4001i64.to_le_bytes())
    });
    let why = "channels 1 and 0, a derived channel's operands: their runs at sample 80 differ";
    assert_eq!(read_in_order(&moved).unwrap_err().to_string(), why);
    let Err(err) = escapement_tickwave_format::repair::repair(&moved, 3) else {
        panic!("repaired");
    };
    assert!(err.to_string().contains(why), "{err}");

    // By hand: V and I's first runs, V's second from sample 2 at tick 25, and four samples
    // each: one operand with a run its partner, past that sample, lacks.
    let run = |c: u32, b: Break| {
        let mut out = Vec::new();
        record::write(&mut out, Kind::Run, &[&record::run_bytes(c, b)]);
        out
    };
    let block = |c: u32| {
        // Four 16-bit codes of 1, in byte planes.
        let raw = [1u8, 1, 1, 1, 0, 0, 0, 0];
        let head = BlockHead {
            channel: c,
            start: 0,
            end: 40,
            word: 4,
            decoded_length: 8,
        };
        let mut out = Vec::new();
        record::write(
            &mut out,
            Kind::Block,
            &[&head.to_bytes(), &compress(&raw, 3).unwrap()],
        );
        out
    };
    let head = opening(pair());
    let with = |records: &[Vec<u8>]| {
        let mut f = head.clone();
        for r in records {
            f.extend_from_slice(r);
        }
        f
    };
    let firsts = [run(0, Break::new(0, 0, 0)), run(1, Break::new(0, 0, 0))];
    let second = |c| run(c, Break::new(2, 25, 0));
    // Both runs: read.
    let both = with(&[
        firsts[0].clone(),
        firsts[1].clone(),
        second(0),
        second(1),
        block(0),
        block(1),
    ]);
    read_in_order(&both).unwrap();
    // I's block reaches sample 2, where V has a run and I none.
    let late = with(&[
        firsts[0].clone(),
        firsts[1].clone(),
        second(0),
        block(0),
        block(1),
    ]);
    let err = read_in_order(&late).unwrap_err().to_string();
    assert_eq!(
        err,
        "channels 1 and 0, a derived channel's operands: their runs at sample 2 differ"
    );
    // V's run at sample 2 comes after I's block has passed it with none.
    let after = with(&[
        firsts[0].clone(),
        firsts[1].clone(),
        block(1),
        second(0),
        block(0),
    ]);
    let err = read_in_order(&after).unwrap_err().to_string();
    assert_eq!(
        err,
        "channels 0 and 1, a derived channel's operands: their runs at sample 2 differ"
    );
}

/// `file` with its declaration record's JSON edited by `edit` and framed again. The records after
/// it keep their bytes, so a footer's places past it no longer hold: for readers in order.
fn with_declaration(file: &[u8], edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let at = escapement_tickwave_format::header::PREAMBLE_LEN;
    let len = u32::from_le_bytes(file[at + 1..at + 5].try_into().unwrap()) as usize;
    let mut json: serde_json::Value = serde_json::from_slice(&file[at + 5..at + 5 + len]).unwrap();
    edit(&mut json);
    let mut out = file[..at].to_vec();
    record::write(
        &mut out,
        Kind::Declaration,
        &[&serde_json::to_vec(&json).unwrap()],
    );
    out.extend_from_slice(&file[at + 5 + len + record::CHECK_LEN..]);
    out
}

/// A declaration that names a feature, a codec or an operation this version doesn't know is one
/// repair and the re-cutter can't write again: each refuses it, where both once panicked taking
/// its derived channel's operands (the fuzzer's artifacts/p5c-short2b/recut/crash-b97c791e and
/// repair/crash-d33e5722).
#[test]
fn repair_and_recut_refuse_a_declaration_they_cant_write_again() {
    use escapement_tickwave_format::recut::Recutter;
    use escapement_tickwave_format::repair::repair;
    let d = declaration(
        vec![
            analog("V", int(16, false, (1, 1000)), Ratio::ONE, None),
            analog("I", int(16, false, (1, 1000)), Ratio::ONE, None),
        ],
        vec![Derived::multiply("P", "W", "I", "V")],
    );
    let mut w = Writer::new(d, options()).unwrap();
    for c in 0..2 {
        w.run(c, 0, 0).unwrap();
    }
    for i in 0..4u64 {
        w.sample(0, i).unwrap();
        w.sample(1, i + 1).unwrap();
    }
    w.flush(4).unwrap();
    // As a crash leaves it: through the flush, no index page yet.
    let unfinished = w.output().to_vec();
    let file = finished(w, 8, 0);
    type Edit = fn(&mut serde_json::Value);
    let edits: [(&str, Edit); 3] = [
        ("requires", |v| {
            v["channels"][0]["requires"] = serde_json::json!(["frobs"])
        }),
        ("codec", |v| v["channels"][1]["codec"] = "delta".into()),
        ("op", |v| v["derived"][0]["op"] = "integrate".into()),
    ];
    for (what, edit) in edits {
        let Err(err) = repair(&with_declaration(&unfinished, edit), 3) else {
            panic!("repair, {what}: repaired")
        };
        let err = err.to_string();
        assert!(
            err.contains("unavailable to this version"),
            "repair, {what}: {err}"
        );
        let mut r = Recutter::new(options());
        let err = r
            .feed(&with_declaration(&file, edit))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("which this version doesn't define"),
            "recut, {what}: {err}"
        );
    }
}

/// Verify reads every channel, so it refuses a recording that holds one this version can't read,
/// a stored channel or a derived one, as repair does, where it once panicked taking the derived
/// channel's operands.
#[test]
fn verify_refuses_a_channel_it_cant_read() {
    use escapement_tickwave_format::model::{
        Analog, Channel, ChannelData, Recording, Samples, Source,
    };
    use escapement_tickwave_format::verify::verify;
    let coding = || int(16, false, (1, 1000));
    let d = declaration(
        vec![
            analog("V", coding(), Ratio::ONE, None),
            analog("I", coding(), Ratio::ONE, None),
        ],
        vec![Derived::multiply("P", "W", "I", "V")],
    );
    let mut w = Writer::new(d.clone(), options()).unwrap();
    for c in 0..2 {
        w.run(c, 0, 0).unwrap();
    }
    for i in 0..4u64 {
        w.sample(0, i).unwrap();
        w.sample(1, i + 1).unwrap();
    }
    let file = finished(w, 8, 0);
    let source = |derived: Vec<Derived>| Recording {
        timebase: d.timebase,
        length: 8,
        channels: ["V", "I"]
            .into_iter()
            .zip([0u64, 1])
            .map(|(name, first)| Channel {
                name: name.into(),
                source_name: name.to_lowercase(),
                source: 0,
                data: ChannelData::Analog(Analog {
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::ONE,
                    count: 4,
                    breaks: Vec::new(),
                    significant: None,
                    samples: match coding() {
                        AnalogCoding::Int(int) => Samples::U64(int, Box::new(first..first + 4)),
                        _ => unreachable!(),
                    },
                    lost_before: 0,
                    lost_after: 0,
                    phase: None,
                }),
                shaping: Default::default(),
            })
            .collect(),
        derived,
        sources: vec![Source::new(SourceFormat::Other("t".into()), d.timebase)],
    };
    verify(&file[..], source(d.derived.clone())).unwrap();
    let with = |edit: fn(&mut Derived)| {
        let mut p = d.derived.clone();
        edit(&mut p[0]);
        p
    };
    type Edit = fn(&mut serde_json::Value);
    let edits: [(&str, Edit, Vec<Derived>, &str); 4] = [
        (
            "requires",
            |v| v["channels"][0]["requires"] = serde_json::json!(["frobs"]),
            d.derived.clone(),
            "V",
        ),
        (
            "codec",
            |v| v["channels"][1]["codec"] = "delta".into(),
            d.derived.clone(),
            "I",
        ),
        (
            "op",
            |v| v["derived"][0]["op"] = "integrate".into(),
            with(|p| p.op = Op::Other("integrate".into())),
            "P",
        ),
        (
            "derived requires",
            |v| v["derived"][0]["requires"] = serde_json::json!(["frobs"]),
            with(|p| p.requires = vec!["frobs".into()]),
            "P",
        ),
    ];
    for (what, edit, derived, name) in edits {
        let err = verify(&with_declaration(&file, edit)[..], source(derived))
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!("channel {name} is unavailable to this version, which can't check it"),
            "{what}"
        );
    }
}
