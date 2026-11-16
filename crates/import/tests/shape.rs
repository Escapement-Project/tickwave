//! Moving and trimming channels at conversion: where the recording starts, what each channel
//! keeps, what its entry states, and what the converter refuses. Expected values are worked by
//! hand from FORMAT.md, not from the converter's own arithmetic. A channel keeps its source's
//! ticks, which the file stores; its axis ticks are `origin + tick + offset`.

mod common;

use std::io::Cursor;
use std::sync::Arc;

use escapement_tickwave_format::decode::{BlockChannel, Index};
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
        source: 0,
        data: ChannelData::Digital(Digital {
            initial,
            end: LENGTH,
            count: edges.len() as u64,
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
                source: 0,
                data: ChannelData::Analog(Analog {
                    significant: None,
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::new(3, 2).unwrap(),
                    count: 60,
                    breaks: Vec::new(),
                    samples: Samples::F32(Box::new((0..60).map(|i| i as f32))),
                    lost_before: 0,
                    lost_after: 0,
                    phase: None,
                }),
                shaping: Shaping::default(),
            },
        ],
        derived: Vec::new(),
        sources: vec![Source {
            begin_s: Some(2.0),
            ..Source::new(
                SourceFormat::Other("test".into()),
                Ratio::integer(1_000_000_000).unwrap(),
            )
        }],
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

/// What a conversion wrote, read back: each digital channel's edges in its source's ticks, as the
/// file stores them, and on the axis; each analog channel's samples as (place, value) and as
/// (axis tick, value). `base` is the plan's for the same source and settings, which the
/// positions, the length and the source's `origin` each check apart from it.
struct Out {
    header: Header,
    base: i64,
    file: Vec<u8>,
    edges: Vec<Vec<i64>>,
    axis: Vec<Vec<i64>>,
    samples: Vec<Vec<(i64, f32)>>,
    axis_samples: Vec<Vec<(i64, f32)>>,
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
    let converted = convert::convert(open, settings, &Quiet, &mut out)?;
    let base = Plan::new(&open()?, &settings.offsets, &settings.keeps)?.base();
    let file = out.into_inner();
    // Checked against the source, moved and cut as its header says, as `tickwave verify` does.
    let source = || convert::set_units(open()?, &settings.units);
    let mut plan = Plan::of_header(&source()?, &converted.header)?;
    if plan.cuts_edges() {
        plan.count(source()?);
    }
    verify(&file[..], plan.apply(source()?))
        .unwrap_or_else(|e| panic!("the conversion does not match its source: {e}"));
    let index = Index::of_file(&file).unwrap();
    let head = &index.footer;
    assert_eq!(head.header, converted.header);
    let n = head.header.channels.len();
    let (mut edges, mut axis) = (vec![Vec::new(); n], vec![Vec::new(); n]);
    let (mut samples, mut axis_samples) = (vec![Vec::new(); n], vec![Vec::new(); n]);
    for c in 0..n {
        for block in index.channel_blocks(&file, c).unwrap() {
            let got = &block.data;
            match got {
                BlockChannel::Digital { edges: e, .. } => {
                    edges[c].extend(got.source_ticks().unwrap());
                    axis[c].extend(e);
                }
                BlockChannel::Analog {
                    first_index,
                    values,
                } => {
                    for (i, &v) in (*first_index..).zip(values.as_f32().expect("f32 values")) {
                        samples[c].push((head.sample_tick(c, i).unwrap(), v));
                        axis_samples[c].push((head.sample_axis_tick(c, i).unwrap(), v));
                    }
                }
            }
        }
    }
    Ok(Out {
        header: index.footer.header,
        base,
        file,
        edges,
        axis,
        samples,
        axis_samples,
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
    // The source's tick 0 is the recording's, and its own time there stays.
    assert_eq!(
        (out.header.sources[0].origin, out.header.sources[0].begin_s),
        (0, Some(2.0))
    );
    // B, moved: its ticks as the source has them, and on the axis 7 later, from its start there.
    // Before it, its kept span says, it holds nothing of the source.
    assert_eq!(out.digital(1), (true, 100, &[][..], &[5, 50][..]));
    assert_eq!(out.axis[1], [12, 57]);
    assert_eq!(
        *out.shaping(1),
        Shaping {
            offset: moved(7),
            kept: kept(0, 100)
        }
    );
    // A and V as the source has them, not moved, each kept whole from the source's start.
    assert_eq!(out.digital(0), (false, 100, &[][..], &[10, 20, 30, 40][..]));
    assert_eq!(out.axis[0], [10, 20, 30, 40]);
    assert_eq!(out.kepts(), [kept(0, 100); 3]);
    assert!(out.shaping(0).offset.is_none() && out.shaping(2).offset.is_none());
    assert_eq!(out.axis_samples[2][..3], [(0, 0.0), (1, 1.0), (3, 2.0)]);
}

#[test]
fn an_offset_earlier_than_the_sources_start_starts_the_recording_there() {
    let out = run(&|| Ok(source()), &settings(&[("A", "-15 ns")], &[]));
    // Tick 0 is A's start, 15 ticks before the source's.
    assert_eq!(out.base, -15);
    assert_eq!(out.header.length, 115);
    // The source's tick 0 lies 15 ticks into the recording; its own time there stays.
    assert_eq!(
        (out.header.sources[0].origin, out.header.sources[0].begin_s),
        (15, Some(2.0))
    );
    // A's ticks as the source has them, at 15 + t − 15 on the axis.
    assert_eq!(out.digital(0), (false, 100, &[][..], &[10, 20, 30, 40][..]));
    assert_eq!(out.axis[0], [10, 20, 30, 40]);
    assert_eq!(
        *out.shaping(0),
        Shaping {
            offset: moved(-15),
            kept: kept(0, 100)
        }
    );
    // B and V, not moved, start 15 ticks in: their kept spans, from the source's tick 0, say
    // the axis before holds nothing of the source.
    assert_eq!(out.digital(1), (true, 100, &[][..], &[5, 50][..]));
    assert_eq!(out.axis[1], [20, 65]);
    assert_eq!(out.kepts(), [kept(0, 100); 3]);
    assert!(out.shaping(1).offset.is_none());
    assert_eq!(out.samples[2][..3], [(0, 0.0), (1, 1.0), (3, 2.0)]);
    assert_eq!(out.axis_samples[2][..3], [(15, 0.0), (16, 1.0), (18, 2.0)]);
    assert_eq!(out.axis_samples[2].last(), Some(&(15 + 88, 59.0)));
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
    // The source's tick 0 lies 20 ticks before the recording's.
    assert_eq!(
        (out.header.sources[0].origin, out.header.sources[0].begin_s),
        (-20, Some(2.0))
    );
    // A's edge at 20 lies in its kept span, so it stays, and A's level at the cut is the one
    // just before it: high, after the edge at 10.
    assert_eq!(out.digital(0), (true, 100, &[][..], &[20, 30, 40][..]));
    assert_eq!(out.axis[0], [0, 10, 20]);
    // B is low from its edge at 5.
    assert_eq!(out.digital(1), (false, 100, &[][..], &[50][..]));
    assert_eq!(out.axis[1], [30]);
    // V's first sample at or after 20 is sample 14, at 21, on its grid, so its cut stays at 20.
    assert_eq!(out.kepts(), [kept(20, 100); 3]);
    assert_eq!(out.samples[2][0], (21, 14.0));
    assert_eq!(out.axis_samples[2][0], (1, 14.0));
}

#[test]
fn a_cut_at_an_edge_keeps_the_edge_and_starts_at_the_level_before_it() {
    for (from, initial, edges) in [
        // The edge at 20, which takes A low, lies in the span: A starts high, before it.
        ("20", true, &[20, 30, 40][..]),
        ("30", false, &[30, 40][..]),
        // Inside the pulse from 10 to 20: high, and its fall is kept.
        ("15", true, &[20, 30, 40][..]),
    ] {
        let out = run(&|| Ok(source()), &settings(&[], &[("A", Some(from), None)]));
        let at: i64 = from.parse().unwrap();
        assert_eq!(out.base, 0);
        assert_eq!(
            out.digital(0),
            (initial, 100, &[][..], edges),
            "cut at {from}"
        );
        assert_eq!(out.kepts(), [kept(at, 100), kept(0, 100), kept(0, 100)]);
        assert_eq!(out.header.length, 100);
    }
}

#[test]
fn a_cut_at_the_end_alone_keeps_the_edges_at_tick_0_as_the_source_has_them() {
    // P pulses at tick 0, a rise and a fall there, then rises at 40. Kept to 50, its front is the
    // source's, and so are the edges at 0.
    let pulsed = || {
        let mut r = source();
        r.channels[0].data = ChannelData::Digital(Digital {
            initial: false,
            end: LENGTH,
            edges: Box::new([0, 0, 40].into_iter()),
            count: 3,
            gaps: Vec::new(),
        });
        Ok(r)
    };
    let out = run(&pulsed, &settings(&[], &[("A", None, Some("50"))]));
    assert_eq!(out.digital(0), (false, 50, &[][..], &[0, 0, 40][..]));
    assert_eq!(out.kepts(), [kept(0, 50), kept(0, 100), kept(0, 100)]);
}

#[test]
fn a_cut_at_either_end_or_both_keeps_what_lies_between() {
    // Inside the pulse at 10 to 20, to inside the one at 30 to 40: A starts high and is known
    // to the cut.
    let out = run(
        &|| Ok(source()),
        &settings(&[], &[("A", Some("15"), Some("35"))]),
    );
    assert_eq!(out.digital(0), (true, 35, &[][..], &[20, 30][..]));
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
fn an_analog_cut_on_a_fractional_period_keeps_every_sample_at_its_place() {
    // V's period is 3/2: sample i's place is 1.5 i, drawn at the tick it maps to, 1.5 i's floor.
    for (from, first, kept_from, count) in [
        // Sample 3, at 4.5, is the first at or after 4: it starts the run at tick 4, phase 1/2.
        ("4", (4, 3.0), 4, 57),
        // Sample 2, at 3.
        ("3", (3, 2.0), 3, 58),
        // Between samples: sample 2 is the first after the cut, which stays where it was put.
        ("2", (3, 2.0), 2, 58),
        // Rounded to the nearest tick: 4.4 ns is tick 4.
        ("4.4ns", (4, 3.0), 4, 57),
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
    let mut plan = Plan::new(&source(), &s.offsets, &s.keeps).unwrap();
    plan.count(source());
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
    let mut plan = Plan::new(&source(), &other.offsets, &other.keeps).unwrap();
    plan.count(source());
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
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: true,
                    end: LENGTH,
                    edges: Box::new([10, 20, 50, 80].into_iter()),
                    count: 4,
                    gaps: vec![gap(30, 40, false), gap(60, 70, true)],
                }),
                shaping: Shaping::default(),
            },
            Channel {
                name: "W".into(),
                source_name: "w".into(),
                source: 0,
                data: ChannelData::Analog(Analog {
                    significant: None,
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::new(3, 2).unwrap(),
                    count: 10,
                    breaks: vec![escapement_tickwave_format::model::Break {
                        index: 5,
                        tick: 30,
                        lost: 0,
                        phase: 0,
                    }],
                    samples: Samples::F32(Box::new((0..10).map(|i| i as f32))),
                    lost_before: 0,
                    lost_after: 0,
                    phase: None,
                }),
                shaping: Shaping::default(),
            },
        ],
        derived: Vec::new(),
        sources: vec![Source::new(
            SourceFormat::Other("test".into()),
            Ratio::integer(1_000_000_000).unwrap(),
        )],
    }
}

#[test]
fn an_instruments_gaps_and_breaks_are_cut_and_a_cut_in_a_gap_starts_the_channel_with_it() {
    for (offsets, keeps, initial, gaps, edges, end) in [
        // The cut inside a gap, or at its start, starts the channel with the gap, from the cut.
        (
            &[][..],
            &[("G", Some("35"), None)][..],
            false,
            &[gap(35, 40, false), gap(60, 70, true)][..],
            &[50, 80][..],
            100,
        ),
        (
            &[],
            &[("G", Some("30"), None)],
            false,
            &[gap(30, 40, false), gap(60, 70, true)],
            &[50, 80],
            100,
        ),
        // Before the gap: high after the edges at 10 and 20, and the gap stays its own.
        (
            &[],
            &[("G", Some("25"), None)],
            true,
            &[gap(30, 40, false), gap(60, 70, true)],
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
        // Moved 10 later as well: the ticks stay the source's, and the move is the offset.
        (
            &[("G", "10")][..],
            &[("G", Some("35"), None)],
            false,
            &[gap(35, 40, false), gap(60, 70, true)],
            &[50, 80],
            100,
        ),
    ] {
        let out = run(&|| Ok(with_gaps()), &settings(offsets, keeps));
        assert_eq!(out.digital(0), (initial, end, gaps, edges), "{keeps:?}");
        assert_eq!(out.kepts()[1], kept(0, 100), "{keeps:?}");
        let moved: i64 = offsets.first().map_or(0, |o| o.1.parse().unwrap());
        let on_axis: Vec<i64> = edges.iter().map(|t| t + moved).collect();
        assert_eq!(out.axis[0], on_axis, "{keeps:?}");
    }

    for (keeps, samples, breaks) in [
        // From 31: sample 6, at 31.5, one into its run, starts the channel there.
        (
            &[("W", Some("31"), None)][..],
            &[(31, 6.0), (33, 7.0), (34, 8.0), (36, 9.0)][..],
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
/// Kept only after that, it is never known: no gap, its end at its start, and its kept span still
/// stated.
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
    assert_eq!(out.digital(0), (false, 60, &[][..], &[][..]));
    assert_eq!(out.kepts(), [kept(60, 100), kept(0, 100), kept(0, 100)]);
    // Cut before it stops: known from the cut, high after the edges at 10, 20 and 30, to where
    // it stopped.
    let out = run(&open, &settings(&[], &[("A", Some("35"), None)]));
    assert_eq!(out.digital(0), (true, 45, &[][..], &[40][..]));
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
    ] {
        let got = refused(offsets, keeps);
        assert!(got.contains(message), "{offsets:?} {keeps:?}: {got}");
    }
    // Exactly 0.01 of a tick off is on the grid.
    let out = run(&|| Ok(source()), &settings(&[("A", "-1.01 ns")], &[]));
    assert_eq!(out.shaping(0).offset, moved(-1));
    assert_eq!(out.kepts(), [kept(0, 100); 3]);
    assert_eq!(out.header.sources[0].origin, 1);
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
    // A1, moved 5 earlier, starts the recording: the source's tick 0 lies at axis tick 5.
    assert_eq!(shaped.base, -5);
    assert_eq!(shaped.header.sources[0].origin, 5);
    let (d3, a0, a1, d0) = (at("D3"), at("A0"), at("A1"), at("D0"));
    // D3 keeps the whole conversion's edges in its span, one at its `from` included, at their
    // source ticks, and lies 5 + 50 later on the axis.
    let want: Vec<i64> = whole.edges[d3]
        .iter()
        .copied()
        .filter(|&t| (100..2000).contains(&t))
        .collect();
    assert!(!want.is_empty());
    assert_eq!(shaped.edges[d3], want);
    let on_axis: Vec<i64> = want.iter().map(|t| t + 55).collect();
    assert_eq!(shaped.axis[d3], on_axis);
    assert_eq!(shaped.shaping(d3).kept, kept(100, 2000));
    // The rest state their spans too, in source ticks: D0 and A1 whole, A0 cut from 1000.
    assert_eq!(
        [d0, a0, a1].map(|c| shaped.shaping(c).kept),
        [kept(0, 3000), kept(1000, 3000), kept(0, 3000)]
    );
    assert_eq!(shaped.edges[d0], whole.edges[d0]);
    let on_axis: Vec<i64> = whole.edges[d0].iter().map(|t| t + 5).collect();
    assert_eq!(shaped.axis[d0], on_axis);
    assert_eq!(shaped.samples[a1], whole.samples[a1]);
    assert_eq!(shaped.axis_samples[a1], whole.samples[a1]);
    let want: Vec<(i64, f32)> = whole.samples[a0]
        .iter()
        .copied()
        .filter(|s| s.0 >= 1000)
        .collect();
    assert_eq!(shaped.samples[a0], want);
    let on_axis: Vec<(i64, f32)> = want.iter().map(|&(t, v)| (t + 5, v)).collect();
    assert_eq!(shaped.axis_samples[a0], on_axis);
}

/// A cut keeps every sample whose place lies in its span, and states where the first kept one
/// sits as its run's tick and phase (FORMAT.md, "Moved and trimmed channels"). A cut that starts
/// the channel on a run whose phase gives a coarser grid than a later run's phase needs can't be
/// stated, and is refused.
#[test]
fn a_cut_whose_first_run_cannot_state_a_later_runs_phase_is_refused() {
    // X: a period of 2 and a phase of 1/2, a grid of halves: samples at 0.5 and 2.5, a run from
    // 10 at phase 0 (10, 12), and one from 20 at phase 1/2 (20.5, 22.5).
    let open = || {
        let mut rec = source();
        rec.channels.push(Channel {
            name: "X".into(),
            source_name: "x".into(),
            source: 0,
            data: ChannelData::Analog(Analog {
                unit: "V".into(),
                first: 0,
                phase: Ratio::new(1, 2),
                period: Ratio::integer(2).unwrap(),
                count: 6,
                breaks: vec![
                    escapement_tickwave_format::model::Break::new(2, 10, 0),
                    escapement_tickwave_format::model::Break {
                        phase: 1,
                        ..escapement_tickwave_format::model::Break::new(4, 20, 0)
                    },
                ],
                significant: None,
                samples: Samples::F32(Box::new((0..6).map(|i| i as f32))),
                lost_before: 0,
                lost_after: 0,
            }),
            shaping: Shaping::default(),
        });
        Ok(rec)
    };
    // From 1: sample 1, at 2.5, starts the channel on the grid of halves, and both runs stay.
    let out = run(&open, &settings(&[], &[("X", Some("1"), None)]));
    let ChannelKind::Analog {
        first,
        phase,
        breaks,
        ..
    } = &out.header.channels[3].data
    else {
        panic!()
    };
    let breaks: Vec<(u64, i64, u32)> = breaks.iter().map(|b| (b.index, b.tick, b.phase)).collect();
    assert_eq!((*first, *phase), (2, Ratio::new(1, 2)));
    assert_eq!(breaks, [(1, 10, 0), (3, 20, 1)]);
    // From 9: sample 2, at 10, starts the channel at phase 0, a grid of whole ticks, which the
    // run at 20.5 can't sit on.
    let err = match try_run(&open, &settings(&[], &[("X", Some("9"), None)])) {
        Ok(_) => panic!("converted"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("X: kept from tick 9")
            && err
                .contains("its first kept sample's phase gives a coarser grid than a later run's"),
        "{err}"
    );
}

/// The cuts in testdata/format/places.json: each keeps the samples at or after its tick, the
/// first starting the channel at its whole tick and phase, and every later run kept as it was.
#[test]
fn the_reference_cuts_keep_what_the_vectors_say() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/format/places.json");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let int = |v: &serde_json::Value| v.as_i64().unwrap();
    let ratio = |v: &serde_json::Value| Ratio::new(int(&v[0]) as u64, int(&v[1]) as u64);
    for case in v["trims"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let c = case["channel"].clone();
        let open = move || {
            let mut rec = source();
            let count = int(&c["count"]) as u64;
            let breaks = c["breaks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| escapement_tickwave_format::model::Break {
                    phase: int(&b[3]) as u32,
                    ..escapement_tickwave_format::model::Break::new(
                        int(&b[0]) as u64,
                        int(&b[1]),
                        int(&b[2]) as u64,
                    )
                })
                .collect();
            rec.channels.push(Channel {
                name: "X".into(),
                source_name: "x".into(),
                source: 0,
                data: ChannelData::Analog(Analog {
                    unit: "V".into(),
                    first: int(&c["first"]),
                    phase: c["phase"].as_array().and_then(|_| ratio(&c["phase"])),
                    period: ratio(&c["period"]).unwrap(),
                    count,
                    breaks,
                    significant: None,
                    samples: Samples::F32(Box::new((0..count).map(|i| i as f32))),
                    lost_before: 0,
                    lost_after: 0,
                }),
                shaping: Shaping::default(),
            });
            Ok(rec)
        };
        let from = int(&case["channel"]["from"]).to_string();
        let out = run(&open, &settings(&[], &[("X", Some(&from), None)]));
        let e = &case["expect"];
        let ChannelKind::Analog {
            first,
            phase,
            breaks,
            count,
            ..
        } = &out.header.channels[3].data
        else {
            panic!("{name}: X is analog")
        };
        let breaks: Vec<[i64; 4]> = breaks
            .iter()
            .map(|b| [b.index as i64, b.tick, b.lost as i64, i64::from(b.phase)])
            .collect();
        let want: Vec<[i64; 4]> = e["breaks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| [int(&b[0]), int(&b[1]), int(&b[2]), int(&b[3])])
            .collect();
        assert_eq!(
            (*first, *phase, *count as i64, breaks),
            (
                int(&e["first"]),
                e["phase"].as_array().and_then(|_| ratio(&e["phase"])),
                int(&e["count"]),
                want
            ),
            "{name}"
        );
        // Each kept sample at the whole tick its place lies in, in order.
        let d = int(&e["d"]);
        let ticks: Vec<i64> = out.samples[3].iter().map(|&(t, _)| t).collect();
        let want: Vec<i64> = e["places"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| int(p).div_euclid(d))
            .collect();
        assert_eq!(ticks, want, "{name}");
    }
}
