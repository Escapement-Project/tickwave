//! Moving and trimming channels at conversion: where the recording starts, what each channel
//! keeps, what its entry states, and what the converter refuses. Expected values are worked by
//! hand from FORMAT.md, not from the converter's own arithmetic.

mod common;

use std::io::Cursor;
use std::sync::Arc;

use escapement_tickwave_format::decode::{BlockChannel, Footer, MAX_DECODED};
use escapement_tickwave_format::header::{ChannelKind, Header};
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Gap, Kept, Offset, Ratio, Recording, Samples, Shaping,
    Source, SourceFormat,
};
use escapement_tickwave_format::verify::verify;
use escapement_tickwave_import::convert::{self, Pass, Progress, Settings, Step, select};
use escapement_tickwave_import::shape::Plan;
use escapement_tickwave_import::sigrok;

use common::testdata;

struct Quiet;

impl Progress for Quiet {
    fn start(&self, _: Pass<'_>, _: u64) -> Step {
        Arc::new(|_| {})
    }
}

const LENGTH: i64 = 100;

/// 100 ticks at 1 GS/s, the source's own start 2 s. Digital A: low, high at 10 to 20 and 30 to
/// 40. Digital B: high, low from 5 to 50. Analog V: a sample every 1.5 ticks from tick 0, 60 of
/// them, sample i holding the value i, so sample i sits at tick floor(1.5 i).
fn source() -> Recording {
    let digital = |name: &str, initial, edges: Vec<i64>| Channel {
        name: name.into(),
        source_name: name.to_lowercase(),
        data: ChannelData::Digital(Digital {
            initial,
            end: LENGTH,
            edges: Box::new(edges.into_iter()),
            gaps: Vec::new(),
        }),
        shaping: Shaping::default(),
    };
    Recording {
        timebase: Ratio::integer(1_000_000_000).unwrap(),
        length: LENGTH,
        channels: vec![
            digital("A", false, vec![10, 20, 30, 40]),
            digital("B", true, vec![5, 50]),
            Channel {
                name: "V".into(),
                source_name: "v".into(),
                data: ChannelData::Analog(Analog {
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::new(3, 2).unwrap(),
                    count: 60,
                    breaks: Vec::new(),
                    samples: Samples::F32(Box::new((0..60).map(|i| i as f32))),
                }),
                shaping: Shaping::default(),
            },
        ],
        source: Source {
            begin_s: Some(2.0),
            ..Source::new(SourceFormat::Other("test".into()))
        },
    }
}

type KeepRow<'a> = (&'a str, Option<&'a str>, Option<&'a str>);

fn settings(offsets: &[(&str, &str)], keeps: &[KeepRow]) -> Settings {
    Settings {
        offsets: offsets.iter().map(|&(n, t)| (n.into(), t.into())).collect(),
        keeps: keeps
            .iter()
            .map(|&(n, f, t)| (n.into(), f.map(Into::into), t.map(Into::into)))
            .collect(),
        level: 3,
        ..Settings::default()
    }
}

/// What a conversion wrote, read back: each digital channel's edges, each analog channel's
/// samples as (tick, value). `base` is the plan's for the same source and settings, which the
/// positions, the length and `begin_s` each check apart from it.
struct Out {
    header: Header,
    base: i64,
    file: Vec<u8>,
    edges: Vec<Vec<i64>>,
    samples: Vec<Vec<(i64, f32)>>,
}

impl Out {
    fn digital(&self, c: usize) -> (bool, i64, &[Gap], &[i64]) {
        let ChannelKind::Digital {
            initial, end, gaps, ..
        } = &self.header.channels[c].data
        else {
            panic!("channel {c} is digital");
        };
        (*initial, *end, gaps, &self.edges[c])
    }

    fn shaping(&self, c: usize) -> &Shaping {
        &self.header.channels[c].shaping
    }

    /// Each channel's kept span, in channel order.
    fn kepts(&self) -> Vec<Option<Kept>> {
        self.header
            .channels
            .iter()
            .map(|c| c.shaping.kept)
            .collect()
    }
}

fn run(open: &dyn Fn() -> Result<Recording, convert::Error>, settings: &Settings) -> Out {
    try_run(open, settings).unwrap_or_else(|e| panic!("{e}"))
}

fn try_run(
    open: &dyn Fn() -> Result<Recording, convert::Error>,
    settings: &Settings,
) -> Result<Out, convert::Error> {
    let mut out = Cursor::new(Vec::new());
    let converted = convert::convert(open, settings, MAX_DECODED, &Quiet, &mut out)?;
    let base = Plan::new(&open()?, &settings.offsets, &settings.keeps)?.base();
    let file = out.into_inner();
    let head = Footer::parse_file(&file).unwrap();
    assert_eq!(head.header, converted.header);
    let n = head.header.channels.len();
    let (mut edges, mut samples) = (vec![Vec::new(); n], vec![Vec::new(); n]);
    for i in 0..head.rows.len() {
        let (offset, len) = head.block_location(i);
        let block = head
            .decode_block(i, &file[offset as usize..][..len as usize])
            .unwrap();
        for (c, got) in block.channels.into_iter().enumerate() {
            match got {
                BlockChannel::Digital { edges: e, .. } => edges[c].extend(e),
                BlockChannel::Analog {
                    first_index,
                    values,
                } => samples[c].extend(
                    (first_index..)
                        .zip(values)
                        .map(|(index, v)| (head.sample_tick(c, index).unwrap(), v)),
                ),
            }
        }
    }
    Ok(Out {
        header: head.header,
        base,
        file,
        edges,
        samples,
    })
}

fn gap(from: i64, to: i64, level: bool) -> Gap {
    Gap { from, to, level }
}

fn moved(ticks: i64) -> Option<Offset> {
    Some(Offset {
        ticks,
        kind: "delay".into(),
    })
}

fn kept(from: i64, to: i64) -> Option<Kept> {
    Some(Kept { from, to })
}

#[test]
fn an_offset_later_moves_one_channel_and_the_recording_keeps_its_start() {
    let out = run(&|| Ok(source()), &settings(&[("B", "7")], &[]));
    assert_eq!(out.base, 0);
    assert_eq!(out.header.length, 107);
    assert_eq!(out.header.source.begin_s, Some(2.0));
    // B, moved: unknown until its start, at 7.
    assert_eq!(
        out.digital(1),
        (false, 107, &[gap(0, 7, true)][..], &[12, 57][..])
    );
    assert_eq!(
        *out.shaping(1),
        Shaping {
            offset: moved(7),
            kept: kept(7, 107)
        }
    );
    // A and V as the source has them, not moved, each kept whole from the source's start.
    assert_eq!(out.digital(0), (false, 100, &[][..], &[10, 20, 30, 40][..]));
    assert_eq!(out.kepts(), [kept(0, 100), kept(7, 107), kept(0, 100)]);
    assert!(out.shaping(0).offset.is_none() && out.shaping(2).offset.is_none());
    assert_eq!(out.samples[2][..3], [(0, 0.0), (1, 1.0), (3, 2.0)]);
}

#[test]
fn an_offset_earlier_than_the_sources_start_starts_the_recording_there() {
    let out = run(&|| Ok(source()), &settings(&[("A", "-15 ns")], &[]));
    // Tick 0 is A's start, 15 ticks before the source's.
    assert_eq!(out.base, -15);
    assert_eq!(out.header.length, 115);
    assert_eq!(out.header.source.begin_s, Some(2.0 - 15e-9));
    assert_eq!(out.digital(0), (false, 100, &[][..], &[10, 20, 30, 40][..]));
    assert_eq!(
        *out.shaping(0),
        Shaping {
            offset: moved(-15),
            kept: kept(0, 100)
        }
    );
    // B and V, not moved, start 15 ticks in; nothing is known of B before.
    assert_eq!(
        out.digital(1),
        (false, 115, &[gap(0, 15, true)][..], &[20, 65][..])
    );
    // B's ticks before 15 lie before its source's data: its kept span says so.
    assert_eq!(out.kepts(), [kept(0, 100), kept(15, 115), kept(15, 115)]);
    assert!(out.shaping(1).offset.is_none());
    assert_eq!(out.samples[2][..3], [(15, 0.0), (16, 1.0), (18, 2.0)]);
    assert_eq!(out.samples[2].last(), Some(&(15 + 88, 59.0)));
}

#[test]
fn every_channel_cut_from_one_tick_starts_the_recording_at_the_cut() {
    let from = Some("20");
    let out = run(
        &|| Ok(source()),
        &settings(
            &[],
            &[("A", from, None), ("B", from, None), ("V", from, None)],
        ),
    );
    assert_eq!(out.base, 20);
    assert_eq!(out.header.length, 80);
    assert_eq!(out.header.source.begin_s, Some(2.0 + 20e-9));
    // A's edge at 20 is the cut's: A is low from tick 0.
    assert_eq!(out.digital(0), (false, 80, &[][..], &[10, 20][..]));
    assert_eq!(out.digital(1), (false, 80, &[][..], &[30][..]));
    assert_eq!(out.kepts(), [kept(0, 80); 3]);
    // V's first sample at or after 20 is sample 14, at 21, on its grid.
    assert_eq!(out.samples[2][0], (1, 14.0));
}

#[test]
fn a_cut_at_an_edge_folds_the_edge_into_the_gaps_level() {
    for (from, level, edges) in [
        // The edge at 20 takes A low: the level after it.
        ("20", false, &[30, 40][..]),
        ("30", true, &[40][..]),
        // Inside the pulse from 10 to 20: high, and its fall is kept.
        ("15", true, &[20, 30, 40][..]),
    ] {
        let out = run(&|| Ok(source()), &settings(&[], &[("A", Some(from), None)]));
        let at: i64 = from.parse().unwrap();
        assert_eq!(out.base, 0);
        assert_eq!(
            out.digital(0),
            (false, 100, &[gap(0, at, level)][..], edges),
            "cut at {from}"
        );
        assert_eq!(out.kepts(), [kept(at, 100), kept(0, 100), kept(0, 100)]);
        assert_eq!(out.header.length, 100);
    }
}

#[test]
fn a_cut_at_either_end_or_both_keeps_what_lies_between() {
    // Inside the pulse at 10 to 20, to inside the one at 30 to 40: A is known to the cut.
    let out = run(
        &|| Ok(source()),
        &settings(&[], &[("A", Some("15"), Some("35"))]),
    );
    assert_eq!(
        out.digital(0),
        (false, 35, &[gap(0, 15, true)][..], &[20, 30][..])
    );
    assert_eq!(out.kepts(), [kept(15, 35), kept(0, 100), kept(0, 100)]);
    assert_eq!(out.header.length, 100);

    // To B's edge at 50: the edge is past the cut, and B is known to it.
    let out = run(&|| Ok(source()), &settings(&[], &[("B", None, Some("50"))]));
    assert_eq!(out.digital(1), (true, 50, &[][..], &[5][..]));
    assert_eq!(out.kepts(), [kept(0, 100), kept(0, 50), kept(0, 100)]);

    // Every channel cut at its end: the recording ends at the latest cut.
    let out = run(
        &|| Ok(source()),
        &settings(
            &[],
            &[
                ("A", None, Some("60")),
                ("B", None, Some("70")),
                ("V", None, Some("10")),
            ],
        ),
    );
    assert_eq!(out.header.length, 70);
    // V's samples before 10: up to sample 6, at 9.
    assert_eq!(out.samples[2].last(), Some(&(9, 6.0)));
    assert_eq!(out.samples[2].len(), 7);
    assert_eq!(out.kepts(), [kept(0, 60), kept(0, 70), kept(0, 10)]);
}

#[test]
fn an_analog_cut_on_a_fractional_period_keeps_every_samples_tick() {
    // V's period is 3/2: a run's samples sit on its ticks every second sample.
    for (from, first, kept_from, count) in [
        // Sample 3, at 4, is odd: the cut moves on to sample 4, at 6.
        ("4", (6, 4.0), 6, 56),
        // Sample 2, at 3, is even.
        ("3", (3, 2.0), 3, 58),
        // Between samples: sample 2 is the first after the cut, which stays where it was put.
        ("2", (3, 2.0), 2, 58),
        // Rounded to the nearest tick: 4.4 ns is tick 4.
        ("4.4ns", (6, 4.0), 6, 56),
    ] {
        let out = run(&|| Ok(source()), &settings(&[], &[("V", Some(from), None)]));
        let samples = &out.samples[2];
        assert_eq!(samples[0], first, "{from}");
        assert_eq!(samples.len(), count, "{from}");
        assert_eq!(
            out.kepts(),
            [kept(0, 100), kept(0, 100), kept(kept_from, 100)],
            "{from}"
        );
        // Sample i of the source, holding i, at tick floor(1.5 i), wherever it is kept.
        for &(tick, value) in samples {
            assert_eq!(tick, (value as i64 * 3) / 2, "{from}: {samples:?}");
        }
    }
}

#[test]
fn a_shaped_conversion_checks_against_the_source_moved_and_cut() {
    let s = settings(
        &[("A", "-3"), ("V", "5")],
        &[("A", Some("12"), Some("90")), ("V", Some("4"), None)],
    );
    // `convert` checked the file it wrote; check it again, against the source shaped and not.
    let out = run(&|| Ok(source()), &s);
    let plan = Plan::new(&source(), &s.offsets, &s.keeps).unwrap();
    verify(&out.file[..], plan.apply(source())).unwrap();
    let err = verify(&out.file[..], source()).expect_err("the source as it is differs");
    assert!(
        err.to_string()
            .contains("the offset or the kept span differs"),
        "{err}"
    );
    // A plan that cuts elsewhere differs too.
    let other = settings(
        &[("A", "-3"), ("V", "5")],
        &[("A", Some("12"), Some("91")), ("V", Some("4"), None)],
    );
    let plan = Plan::new(&source(), &other.offsets, &other.keeps).unwrap();
    assert!(verify(&out.file[..], plan.apply(source())).is_err());
}

#[test]
fn moves_and_cuts_that_change_nothing_write_the_whole_conversions_bytes() {
    let whole = run(&|| Ok(source()), &settings(&[], &[]));
    let unchanged = run(
        &|| Ok(source()),
        &settings(
            &[("A", "0"), ("B", "0.004 ns")],
            &[("B", None, None), ("V", Some("-5"), Some("1000"))],
        ),
    );
    assert_eq!(unchanged.base, 0);
    assert!(whole.file == unchanged.file);
    // Nothing moved or cut: no channel states a kept span.
    assert_eq!(whole.kepts(), [None; 3]);
}

/// 100 ticks at 1 GS/s. Digital G: high, its level lost from 30 to 40 (low after) and from 60 to
/// 70 (high after), with edges at 10, 20, 50 and 80. Analog W: a run from tick 0 of samples 0 to
/// 4, then one from tick 30 of samples 5 to 9, every 1.5 ticks.
fn with_gaps() -> Recording {
    Recording {
        timebase: Ratio::integer(1_000_000_000).unwrap(),
        length: LENGTH,
        channels: vec![
            Channel {
                name: "G".into(),
                source_name: "g".into(),
                data: ChannelData::Digital(Digital {
                    initial: true,
                    end: LENGTH,
                    edges: Box::new([10, 20, 50, 80].into_iter()),
                    gaps: vec![gap(30, 40, false), gap(60, 70, true)],
                }),
                shaping: Shaping::default(),
            },
            Channel {
                name: "W".into(),
                source_name: "w".into(),
                data: ChannelData::Analog(Analog {
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::new(3, 2).unwrap(),
                    count: 10,
                    breaks: vec![escapement_tickwave_format::model::Break { index: 5, tick: 30 }],
                    samples: Samples::F32(Box::new((0..10).map(|i| i as f32))),
                }),
                shaping: Shaping::default(),
            },
        ],
        source: Source::new(SourceFormat::Other("test".into())),
    }
}

#[test]
fn an_instruments_gaps_and_breaks_are_cut_moved_and_joined_to_a_cut() {
    for (offsets, keeps, initial, gaps, edges, end) in [
        // The cut inside a gap, or at its start, joins it.
        (
            &[][..],
            &[("G", Some("35"), None)][..],
            false,
            &[gap(0, 40, false), gap(60, 70, true)][..],
            &[50, 80][..],
            100,
        ),
        (
            &[],
            &[("G", Some("30"), None)],
            false,
            &[gap(0, 40, false), gap(60, 70, true)],
            &[50, 80],
            100,
        ),
        // Before the gap: high after the edges at 10 and 20, and the gap stays its own.
        (
            &[],
            &[("G", Some("25"), None)],
            false,
            &[gap(0, 25, true), gap(30, 40, false), gap(60, 70, true)],
            &[50, 80],
            100,
        ),
        // A gap cut at the end ends what is known at its start. Not cut at its start, G is high
        // at tick 0 as the source has it.
        (
            &[],
            &[("G", None, Some("65"))],
            true,
            &[gap(30, 40, false)],
            &[10, 20, 50],
            60,
        ),
        // Moved 10 later as well: the joined gap and the rest move with the channel.
        (
            &[("G", "10")][..],
            &[("G", Some("35"), None)],
            false,
            &[gap(0, 50, false), gap(70, 80, true)],
            &[60, 90],
            110,
        ),
    ] {
        let out = run(&|| Ok(with_gaps()), &settings(offsets, keeps));
        assert_eq!(out.digital(0), (initial, end, gaps, edges), "{keeps:?}");
        assert_eq!(out.kepts()[1], kept(0, 100), "{keeps:?}");
    }

    for (keeps, samples, breaks) in [
        // From 31: sample 6 is 1 into its run, off the grid; the cut moves to sample 7, at 33.
        (
            &[("W", Some("31"), None)][..],
            &[(33, 7.0), (34, 8.0), (36, 9.0)][..],
            &[][..],
        ),
        // From 5: sample 4 at 6 is 4 into the first run, on the grid; the break stays.
        (
            &[("W", Some("5"), None)],
            &[
                (6, 4.0),
                (30, 5.0),
                (31, 6.0),
                (33, 7.0),
                (34, 8.0),
                (36, 9.0),
            ],
            &[(1, 30)],
        ),
        // From 7, after the first run's last sample: the second run starts the channel.
        (
            &[("W", Some("7"), None)],
            &[(30, 5.0), (31, 6.0), (33, 7.0), (34, 8.0), (36, 9.0)],
            &[],
        ),
        // To 31: the break's run keeps its first sample.
        (
            &[("W", None, Some("31"))],
            &[(0, 0.0), (1, 1.0), (3, 2.0), (4, 3.0), (6, 4.0), (30, 5.0)],
            &[(5, 30)],
        ),
    ] {
        let out = run(&|| Ok(with_gaps()), &settings(&[], keeps));
        assert_eq!(out.samples[1], samples, "{keeps:?}");
        let ChannelKind::Analog { breaks: got, .. } = &out.header.channels[1].data else {
            unreachable!()
        };
        let got: Vec<(u64, i64)> = got.iter().map(|b| (b.index, b.tick)).collect();
        assert_eq!(got, breaks, "{keeps:?}");
        assert_eq!(out.kepts()[0], kept(0, 100), "{keeps:?}");
    }
}

/// A digital channel can stop being known before the recording ends, as a Saleae export's can.
/// Kept only after that, it is never known: no gap, an end of 0, and its kept span still stated.
#[test]
fn a_channel_cut_after_its_instrument_stopped_is_never_known() {
    let open = || {
        let mut rec = source();
        let ChannelData::Digital(a) = &mut rec.channels[0].data else {
            unreachable!()
        };
        a.end = 45;
        Ok(rec)
    };
    let out = run(&open, &settings(&[], &[("A", Some("60"), None)]));
    assert_eq!(out.digital(0), (false, 0, &[][..], &[][..]));
    assert_eq!(out.kepts(), [kept(60, 100), kept(0, 100), kept(0, 100)]);
    // Cut before it stops: known from the cut to where it stopped.
    let out = run(&open, &settings(&[], &[("A", Some("35"), None)]));
    assert_eq!(
        out.digital(0),
        (false, 45, &[gap(0, 35, true)][..], &[40][..])
    );
}

fn refused(offsets: &[(&str, &str)], keeps: &[KeepRow]) -> String {
    match try_run(&|| Ok(source()), &settings(offsets, keeps)) {
        Ok(_) => panic!("{offsets:?} {keeps:?} converted"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn settings_that_cannot_be_followed_are_refused_plainly() {
    for (offsets, keeps, message) in [
        (
            &[("X", "1")][..],
            &[][..],
            r#"an offset for "X": the recording has no channel by that name; it has ["A", "B", "V"]"#,
        ),
        (
            &[],
            &[("x", Some("1"), None)][..],
            r#"a keep for "x": the recording has no channel by that name"#,
        ),
        (
            &[("A", "1"), ("A", "2")],
            &[],
            r#""A" is given an offset twice"#,
        ),
        (
            &[],
            &[("A", Some("1"), None), ("A", None, Some("9"))],
            r#""A" is given a keep twice"#,
        ),
        (
            &[],
            &[("A", Some("30"), Some("30"))],
            "A: kept from tick 30 to 30: its from must come before its to",
        ),
        (
            &[],
            &[("A", Some("30"), Some("20"))],
            "A: kept from tick 30 to 20: its from must come before its to",
        ),
        // Both round to tick 20.
        (
            &[],
            &[("A", Some("20.2ns"), Some("20.4 ns"))],
            "A: kept from tick 20 to 20: its from must come before its to",
        ),
        (
            &[],
            &[("A", Some("200"), None)],
            "A: kept from tick 200 to the end, which leaves nothing of the capture's ticks 0 to 100",
        ),
        (
            &[],
            &[("A", None, Some("-1 ns"))],
            "A: kept from the start to tick -1, which leaves nothing",
        ),
        (
            &[("A", "0.5 ns")],
            &[],
            r#"A: an offset of "0.5 ns" is 0.500 ticks, more than 0.01 of a tick off the grid"#,
        ),
        (
            &[("A", "-1.0100001ns")],
            &[],
            r#"A: an offset of "-1.0100001ns" is -1.010 ticks, more than 0.01 of a tick off the grid"#,
        ),
        (
            &[("A", "9007199254740992")],
            &[],
            "A: an offset of 9007199254740992 ticks is past 2^53",
        ),
        (
            &[("A", "40 parsecs")],
            &[],
            r#"A: "40 parsecs": unknown unit "parsecs""#,
        ),
        (&[], &[("A", Some("1.5"), None)], r#"A: "1.5": no unit"#),
        // Sample 3, at 4, is off V's grid, and sample 4 sits at 6, the cut's end.
        (
            &[],
            &[("V", Some("4"), Some("6"))],
            "V: kept from tick 4 to 6; its samples keep their ticks only from one a multiple of 2 \
             into its run, and none in that span is",
        ),
    ] {
        let got = refused(offsets, keeps);
        assert!(got.contains(message), "{offsets:?} {keeps:?}: {got}");
    }
    // Exactly 0.01 of a tick off is on the grid.
    let out = run(&|| Ok(source()), &settings(&[("A", "-1.01 ns")], &[]));
    assert_eq!(out.shaping(0).offset, moved(-1));
    assert_eq!(out.kepts(), [kept(0, 100), kept(1, 101), kept(1, 101)]);
}

/// A name is the one stored: a channel renamed is named by its new name, and one not taken is no
/// channel of the recording.
#[test]
fn a_name_is_one_the_recording_takes() {
    let path = testdata("demo-logic.sr");
    let open = || {
        select(
            sigrok::read(&path)?.recording,
            &[("D0".into(), "CLK".into())],
        )
    };
    for name in ["D0", "D1"] {
        let got = try_run(&open, &settings(&[(name, "1")], &[]))
            .err()
            .expect("refused")
            .to_string();
        assert_eq!(
            got,
            format!(
                "an offset for {name:?}: the recording has no channel by that name; it has [\"CLK\"]"
            )
        );
    }
    let out = run(&open, &settings(&[("CLK", "1")], &[]));
    assert_eq!(out.shaping(0).offset, moved(1));
}

/// A real capture, shaped, against the same capture converted whole: each edge kept is the whole
/// conversion's, moved.
#[test]
fn a_captures_channels_move_and_cut_as_the_whole_conversion_has_them() {
    let path = testdata("demo-mixed.sr");
    let open = || Ok(sigrok::read(&path)?.recording);
    let whole = run(&open, &settings(&[], &[]));
    let shaped = run(
        &open,
        &settings(
            &[("D3", "50"), ("A1", "-5")],
            &[
                ("D3", Some("100"), Some("2000")),
                ("A0", Some("1000"), None),
            ],
        ),
    );
    let names: Vec<&str> = whole
        .header
        .channels
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    let at = |name| names.iter().position(|n| *n == name).unwrap();
    // A1, moved 5 earlier, starts the recording.
    assert_eq!(shaped.base, -5);
    let (d3, a0, a1, d0) = (at("D3"), at("A0"), at("A1"), at("D0"));
    let want: Vec<i64> = whole.edges[d3]
        .iter()
        .filter(|&&t| 100 < t && t < 2000)
        .map(|t| t + 55)
        .collect();
    assert!(!want.is_empty());
    assert_eq!(shaped.edges[d3], want);
    assert_eq!(shaped.shaping(d3).kept, kept(155, 2055));
    // The rest state their spans too: A1 not moved past base, A0 cut from 1000, D0 whole.
    assert_eq!(
        [d0, a0, a1].map(|c| shaped.shaping(c).kept),
        [kept(5, 3005), kept(1005, 3005), kept(0, 3000)]
    );
    let want: Vec<i64> = whole.edges[d0].iter().map(|t| t + 5).collect();
    assert_eq!(shaped.edges[d0], want);
    assert_eq!(shaped.samples[a1], whole.samples[a1]);
    let want: Vec<(i64, f32)> = whole.samples[a0]
        .iter()
        .filter(|s| s.0 >= 1000)
        .map(|&(t, v)| (t + 5, v))
        .collect();
    assert_eq!(shaped.samples[a0], want);
}
