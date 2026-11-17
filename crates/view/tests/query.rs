//! A view's query of a recording: what each channel's columns show, and the rest of an answer.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use escapement_tickwave_format::decode::{Footer, Index};
use escapement_tickwave_format::header::{
    ChannelKind as HeaderKind, Declaration, DeclaredChannel, DeclaredKind, SourceFormat,
    SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::overview::AnalogOverview as Buckets;
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
use escapement_tickwave_view::columns::{self, AnalogOverview, Grid, Samples, State};
use escapement_tickwave_view::runs::Runs;
use escapement_tickwave_view::spans::{ChannelPlace, known_spans};
use escapement_tickwave_view::{
    Answer, Cache, ChannelKind, Drawn, Error, MarkerCursor, MarkerQuery, Query, Recording,
    Resolution,
};

mod common;
use common::{fixture, open, oracle};

fn query(window: std::ops::Range<i64>, columns: u32, channels: Vec<usize>) -> Query {
    let mut query = Query::new(window, columns);
    query.channels = channels;
    query
}

fn digital(d: &Drawn) -> &[State] {
    match d {
        Drawn::Digital { columns, .. } => &columns.state,
        other => panic!("not digital: {other:?}"),
    }
}

/// A digital channel D with a pulse of two edges at one tick, `at`, which a flush closes the
/// block right after, and samples of V either side: verge's S:1469-1514, at 2^20 ticks a second.
fn pulse_at_block_end(at: i64, shift: u8) -> Vec<u8> {
    let timebase = Ratio::integer(1 << 20).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![
            DeclaredChannel {
                name: "V".into(),
                source_name: "v".into(),
                source: 0,
                data: DeclaredKind::Analog {
                    unit: "V".into(),
                    period: Ratio::integer(1 << 18).unwrap(),
                    coding: escapement_tickwave_format::header::AnalogCoding::F32,
                    significant: None,
                    phase: None,
                },
                shaping: Default::default(),
                overview_shift: None,
                codec: None,
                requires: Vec::new(),
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
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("test".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: shift,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    w.run(0, 0, 0).unwrap();
    let sample = |w: &mut Writer, i: u64| w.sample(0, i as f32).unwrap();
    let mut i = 0;
    while ((i << 18) as i64) <= at {
        sample(&mut w, i);
        i += 1;
    }
    w.edge(1, at).unwrap();
    w.edge(1, at).unwrap();
    w.flush(at + 1).unwrap();
    while ((i << 18) as i64) < 5 << 20 {
        sample(&mut w, i);
        i += 1;
    }
    let mut out = w.take_output();
    out.extend(
        w.finish(5 << 20, &[5 << 20], Finish::default())
            .unwrap()
            .tail,
    );
    out
}

#[test]
fn a_pulse_at_a_blocks_end_shows_in_one_column_at_its_tick() {
    // Overview buckets of 2^12 on (2 and 4 columns read the 2^20 and 2^16 levels), or a finest
    // level of 2^20 (4 columns read the edges). At 2^20 the pulse sits on every bucket's edge.
    for (shift, at) in [(12, 1 << 20), (20, 1 << 20), (20, 1_000_000)] {
        let rec = open(&pulse_at_block_end(at, shift));
        let cancel = AtomicBool::new(false);
        for cols in [2u32, 4] {
            let a = rec
                .query(&query(0..2 << 20, cols, vec![1]), &cancel)
                .unwrap();
            let window = &digital(&a.channels[0])[cols as usize..2 * cols as usize];
            let busy: Vec<usize> = (0..window.len())
                .filter(|&c| window[c] != State::Low)
                .collect();
            assert_eq!(
                busy.len(),
                1,
                "shift {shift}, at {at}, {cols} columns: {window:?}"
            );
            assert_eq!(window[busy[0]], State::Busy);
            let start = (busy[0] as i64 * (2 << 20)) / i64::from(cols);
            let per = (2 << 20) / i64::from(cols);
            assert!((start..start + per).contains(&at), "{start}");
        }
    }
}

#[test]
fn a_zoom_answers_every_column_of_its_window() {
    // track.test.ts's clock: an edge every 1,000 ticks over 2^24, buckets of 2^12 and 2^16.
    let length = 1i64 << 24;
    let timebase = Ratio::integer(1_000_000_000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "CLK".into(),
            source_name: "clk".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
            overview_shift: None,
            codec: None,
            requires: Vec::new(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("test".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 12,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    for i in 1..length / 1000 {
        w.edge(0, i * 1000).unwrap();
    }
    let mut file = w.take_output();
    file.extend(w.finish(length, &[length], Finish::default()).unwrap().tail);
    let rec = open(&file);
    let cancel = AtomicBool::new(false);
    // The whole capture, zoomed in to a hundredth (168 ticks a column, finer than any level),
    // and out again.
    for (start, end) in [
        (0, length),
        (8_000_000, 8_000_000 + length / 100),
        (0, length),
    ] {
        let a = rec
            .query(&query(start..end, 1000, vec![0]), &cancel)
            .unwrap();
        let window = &digital(&a.channels[0])[1000..2000];
        assert!(window.iter().all(|&s| s != State::None), "{start}..{end}");
        assert_eq!(a.around, start - (end - start)..end + (end - start));
    }
}

#[test]
fn a_loss_pales_its_columns_and_a_pause_only_breaks_the_trace() {
    let rec = open(&fixture("energy.tickwave"));
    let cancel = AtomicBool::new(false);
    // 50 pairs lost from 960,000 to 1,008,000; a pause from 1,776,000 (the second run's end) to
    // 2,000,000. 100 columns of 4,000 ticks over each window.
    let a = rec
        .query(&query(800_000..1_200_000, 100, vec![0]), &cancel)
        .unwrap();
    let Drawn::Analog { columns, lost, .. } = &a.channels[0] else {
        panic!()
    };
    assert_eq!((lost.len(), lost[0].clone()), (1, 960_000..1_008_000));
    let pale: Vec<usize> = (0..300).filter(|&c| columns.pale[c]).collect();
    // Columns of `around` from 400,000: 960,000 is column 140, 1,008,000 starts column 152.
    assert_eq!(pale, (140..152).collect::<Vec<_>>());
    // Nothing across the loss: columns 141 to 151 hold nothing, and 152 doesn't join 151.
    assert!((141..152).all(|c| columns.min[c] > columns.max[c]));
    assert!(!columns.joined[152] && columns.joined[153] && columns.joined[139]);
    // `around` from 1,300,000, 4,000 ticks a column: the pause covers columns 119 to 174, and
    // the run at 2,000,000 starts column 175.
    let a = rec
        .query(&query(1_700_000..2_100_000, 100, vec![0]), &cancel)
        .unwrap();
    let Drawn::Analog { columns, lost, .. } = &a.channels[0] else {
        panic!()
    };
    assert!(lost.is_empty());
    assert!(columns.pale.iter().all(|&p| !p));
    assert!((119..175).all(|c| columns.min[c] > columns.max[c]));
    assert!(!columns.joined[175] && columns.joined[176] && columns.joined[118]);
}

#[test]
fn single_samples_come_with_their_flag_once_the_view_holds_few_enough() {
    let rec = open(&fixture("energy.tickwave"));
    let cancel = AtomicBool::new(false);
    // under_range is high from 300,000 to 300,960: of the pairs at 299,520, 300,480 and 301,440,
    // the one at 300,480 is flagged.
    let q = {
        let mut query = query(299_500..301_500, 10, vec![1]);
        query.flags = vec![(1, 3)];
        query
    };
    let a = rec.query(&q, &cancel).unwrap();
    let Drawn::Analog { samples, fit, .. } = &a.channels[0] else {
        panic!()
    };
    let points = samples.as_ref().unwrap();
    let flagged: Vec<(i64, bool)> = points.iter().map(|p| (p.tick, p.flagged)).collect();
    assert!(flagged.contains(&(300_480, true)) && flagged.contains(&(299_520, false)));
    assert!(flagged.contains(&(301_440, false)));
    assert!(points.iter().all(|p| p.end == p.tick + 960));
    // The fit takes the samples whose time meets the window.
    let shown: Vec<f64> = points
        .iter()
        .filter(|p| p.end > 299_500 && p.tick < 301_500)
        .map(|p| p.value)
        .collect();
    let lo = shown.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = shown.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    assert_eq!(*fit, Some((lo, hi)));
    // `around` from -9,600 to 19,200 holds the 20 pairs from 0: single samples with 21 columns
    // over it, none with 18.
    for (cols, some) in [(7, true), (6, false)] {
        let a = rec.query(&query(0..9_600, cols, vec![1]), &cancel).unwrap();
        let Drawn::Analog { samples, .. } = &a.channels[0] else {
            panic!()
        };
        assert_eq!(samples.is_some(), some, "{cols} columns");
    }
}

/// Analog channel `c`'s columns as the rule reads them: each known span's own, joined, then
/// together; from the channel's level `level`, finest first, or from every sample with None.
fn oracle_columns(
    file: &[u8],
    c: usize,
    g: &Grid,
    range: std::ops::Range<i64>,
    level: Option<usize>,
) -> (Vec<f64>, Vec<f64>) {
    let f = Index::of_file(file).unwrap().footer;
    let HeaderKind::Analog { first, count, .. } = &f.header.channels[c].data else {
        panic!()
    };
    let runs = Runs {
        first: *first,
        grid: f.grid(c).unwrap(),
        count: *count,
        breaks: f.breaks(c).unwrap(),
        map: f.header.mapping(c).unwrap(),
    };
    let levels = common::levels(file, c);
    let truth = oracle(file);
    let samples = &truth[c].as_ref().unwrap().samples;
    // Where the last sample's time ends, in whole ticks.
    let per = i128::try_from(truth[c].as_ref().unwrap().per).unwrap();
    let end = samples.last().unwrap().1.div_euclid(per) as i64;
    let values: Vec<f64> = samples.iter().map(|s| s.2).collect();
    let (mut min, mut max) = (
        vec![f64::INFINITY; g.count],
        vec![f64::NEG_INFINITY; g.count],
    );
    for span in known_spans(&ChannelPlace::Analog { runs, end }, range.start, range.end) {
        let cols = match level {
            None => columns::analog_from_samples(
                g,
                &Samples {
                    first: 0,
                    values: &values,
                    runs: None,
                },
                &runs,
                span.start as f64,
                span.end as f64,
            ),
            Some(l) => {
                let lv = &levels[l];
                let Some(Buckets::U64(b)) = lv.data.analog() else {
                    panic!()
                };
                let scale = 1e-9;
                let value = |x: &u64| *x as f64 * scale;
                let (lo, hi): (Vec<f64>, Vec<f64>) = b
                    .min
                    .iter()
                    .zip(&b.max)
                    .map(|(a, z)| {
                        if a > z {
                            (f64::INFINITY, f64::NEG_INFINITY)
                        } else {
                            (value(a), value(z))
                        }
                    })
                    .unzip();
                columns::analog_from_overview(
                    g,
                    &AnalogOverview {
                        bucket: 1 << lv.shift,
                        start: 0,
                        min: &lo,
                        max: &hi,
                    },
                    span.start as f64,
                    span.end as f64,
                )
            }
        };
        for k in 0..g.count {
            if cols.min[k] <= cols.max[k] {
                min[k] = min[k].min(cols.min[k]);
                max[k] = max[k].max(cols.max[k]);
            }
        }
    }
    (min, max)
}

#[test]
fn columns_from_samples_and_from_the_overview_hold_what_the_rule_gives() {
    let file = fixture("energy.tickwave");
    let rec = open(&file);
    let cancel = AtomicBool::new(false);
    // I's columns from every sample (500 ticks a column, below the 4,096 of its only level, the
    // strip), then from its buckets at 16,000 and 102,857 ticks a column. Its value is a nA code at
    // 1/10^9: the oracle reads the codes as I does.
    for (window, cols, level) in [
        (900_000..1_100_000, 400, None),
        (0..2_880_000, 180, Some(0)),
        (0..2_880_000, 28, Some(0)),
    ] {
        let a = rec
            .query(&query(window.clone(), cols, vec![1]), &cancel)
            .unwrap();
        let Drawn::Analog { columns, .. } = &a.channels[0] else {
            panic!()
        };
        let (min, max) = oracle_columns(&file, 1, &a.grid, a.around.clone(), level);
        // The views' values are I's through IntCoding::value, the oracle's code × 1e-9: equal to
        // the last bit only where both round alike, so compared to a part in 10^15.
        for k in 0..a.grid.count {
            let near = |x: f64, y: f64| x == y || ((x - y) / y).abs() < 1e-15;
            assert!(
                near(columns.min[k], min[k]) && near(columns.max[k], max[k]),
                "{window:?} column {k}: {} {} against {} {}",
                columns.min[k],
                columns.max[k],
                min[k],
                max[k]
            );
        }
    }
}

#[test]
fn the_strip_spans_the_extent() {
    let rec = open(&fixture("energy.tickwave"));
    let cancel = AtomicBool::new(false);
    let q = {
        let mut query = query(0..1000, 10, vec![]);
        query.strip = Some((1, 100));
        query
    };
    let a = rec.query(&q, &cancel).unwrap();
    let strip = a.strip.unwrap();
    assert_eq!(strip.extent, 0..2_768_000);
    assert_eq!(
        (strip.grid.x0, strip.grid.per, strip.grid.count),
        (0.0, 27_680.0, 100)
    );
    let Drawn::Analog {
        columns,
        samples,
        lost,
        ..
    } = &strip.channel
    else {
        panic!()
    };
    assert!(samples.is_none());
    assert_eq!((lost.len(), lost[0].clone()), (1, 960_000..1_008_000));
    // Every column holds samples but those inside the loss and the pause.
    let empty: Vec<usize> = (0..100)
        .filter(|&k| columns.min[k] > columns.max[k])
        .collect();
    assert_eq!(empty, [35, 65, 66, 67, 68, 69, 70, 71]);
}

#[test]
fn a_fallback_is_its_channels_columns_over_the_extent_as_a_strip_of_it_would_be() {
    let rec = open(&fixture("energy.tickwave"));
    let cancel = AtomicBool::new(false);
    let d = (rec.channels().iter())
        .position(|c| matches!(c.kind, ChannelKind::Digital))
        .unwrap();
    let mut q = query(0..1000, 10, vec![]);
    q.fallbacks = vec![(d, 100), (1, 50)];
    let a = rec.query(&q, &cancel).unwrap();
    assert_eq!(a.fallbacks.len(), 2);
    // Each, in the query's order, the strip a query of its channel would answer.
    for (got, &(c, n)) in a.fallbacks.iter().zip(&q.fallbacks) {
        let mut s = query(0..1000, 10, vec![]);
        s.strip = Some((c, n));
        assert_eq!(*got, rec.query(&s, &cancel).unwrap().strip.unwrap());
    }
    // And, read another way, the columns over the extent: a query of the whole extent in 100
    // columns lays 300 over the extent and a width either side, its middle 100 on the fallback's.
    let extent = a.fallbacks[0].extent.clone();
    let whole = rec
        .query(&query(extent.clone(), 100, vec![d]), &cancel)
        .unwrap();
    assert_eq!(whole.grid.per, a.fallbacks[0].grid.per);
    let states = digital(&a.fallbacks[0].channel);
    assert_eq!(states, &digital(&whole.channels[0])[100..200]);
    assert!(
        states
            .iter()
            .any(|s| matches!(s, State::Rise | State::Fall | State::Busy))
    );
}

#[test]
fn a_strip_column_around_cuts_is_read_again_outside_it() {
    let rec = open(&fixture("energy.tickwave"));
    let cancel = AtomicBool::new(false);
    // `around` from 600,000 to 1,500,000, inside the extent, 0 to 2,768,000; the strip's
    // columns 27,680 ticks wide, so both its ends fall inside a column.
    let mut q = query(900_000..1_200_000, 10, vec![]);
    q.strip = Some((1, 100));
    let a = rec.query(&q, &cancel).unwrap();
    assert_eq!(a.around, 600_000..1_500_000);
    let strip = a.strip.unwrap();
    let spans: Vec<_> = strip.cut.iter().map(|c| c.span.clone()).collect();
    // Column 21 is 581,280 to 608,960 and column 54 1,494,720 to 1,522,400, rounded up.
    assert_eq!(spans, [581_280..600_000, 1_500_000..1_522_400]);
    for cut in &strip.cut {
        let Drawn::Analog { columns, .. } = &cut.channel else {
            panic!("analog")
        };
        assert_eq!(columns.min.len(), 1);
        // Its levels are the span's, as its statistics give them.
        let mut s = query(cut.span.clone(), 10, vec![]);
        s.statistics = vec![cut.span.clone()];
        let stats = rec.query(&s, &cancel).unwrap().statistics[0].channels[1]
            .clone()
            .unwrap();
        assert_eq!(
            (Some(columns.min[0]), Some(columns.max[0])),
            (stats.min, stats.max),
            "{:?}",
            cut.span
        );
    }
    // A window whose `around` starts before the extent cuts at its end alone.
    let mut q = query(100_000..400_000, 10, vec![]);
    q.fallbacks = vec![(1, 100)];
    let a = rec.query(&q, &cancel).unwrap();
    assert!(a.around.start < 0);
    let ends: Vec<_> = a.fallbacks[0].cut.iter().map(|c| c.span.start).collect();
    assert_eq!(ends, [a.around.end]);
}

#[test]
fn markers_by_page_cursor_and_successor() {
    let rec = open(&fixture("energy.tickwave"));
    let cancel = AtomicBool::new(false);
    // A cursor is the host's to hand back, not to make: these come from the list.
    let every: Vec<MarkerCursor> = rec
        .markers(i64::MIN..i64::MAX, None, usize::MAX)
        .entries
        .iter()
        .map(|m| m.cursor)
        .collect();
    let ticks: Vec<i64> = every.iter().map(|c| c.tick).collect();
    assert_eq!(ticks, [96_000, 480_000, 1_008_000, 2_000_000, 2_700_000]);
    let q = {
        let mut query = query(0..2_880_000, 10, vec![]);
        query.markers = {
            let mut markers = MarkerQuery::default();
            markers.limit = 2;
            markers.list = Some((Some(every[1]), 2));
            markers.successor_of = Some(every[3]);
            markers
        };
        query
    };
    let a = rec.query(&q, &cancel).unwrap();
    let ticks = |p: &escapement_tickwave_view::Page| {
        p.entries.iter().map(|m| m.cursor.tick).collect::<Vec<_>>()
    };
    assert_eq!(ticks(&a.markers.window), [96_000, 480_000]);
    assert_eq!(a.markers.window.next, Some(every[1]));
    assert_eq!(a.markers.window.total, 5);
    let list = a.markers.list.unwrap();
    assert_eq!(ticks(&list), [1_008_000, 2_000_000]);
    let third = &list.entries[0].annotation;
    assert_eq!(
        (third.kind.as_deref(), third.label.as_deref()),
        (Some("marker-after-loss"), Some("Marker 3"))
    );
    assert_eq!(third.values.as_ref().unwrap()["number"], "3");
    let (asked, next) = a.markers.successor.unwrap();
    assert_eq!(asked, every[3]);
    assert_eq!(next.unwrap().cursor, every[4]);
    assert_eq!(rec.marker_after(every[4]), None);
    // The last page has no next.
    assert_eq!(
        rec.markers(0..i64::MAX, a.markers.window.next, 10).next,
        None
    );
}

#[test]
fn a_recording_states_its_channels_sources_and_counts() {
    let rec = open(&fixture("energy.tickwave"));
    let names: Vec<&str> = rec.channels().iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["V", "I", "range", "under_range", "DIO0", "P"]);
    assert_eq!(rec.channel("P"), Some(5));
    let p = &rec.channels()[5];
    assert_eq!(p.kind, ChannelKind::Derived { operands: (1, 0) });
    let Some(Resolution::Code { scale, .. }) = p.resolution else {
        panic!()
    };
    assert_eq!((scale.num(), scale.den()), (1, 1_000_000_000_000));
    assert_eq!((rec.lost(0), rec.lost(5), rec.lost(4)), (50, 50, 0));
    assert_eq!((rec.gaps(4), rec.gaps(0)), (1, 0));
    assert_eq!(rec.marker_count(), 5);
    assert_eq!(rec.sources()[0].extra["end_reason"], "stopped");
    assert_eq!(rec.extent(), Some(0..2_768_000));
}

#[test]
fn a_cancelled_query_stops() {
    let rec = open(&fixture("energy.tickwave"));
    let cancel = AtomicBool::new(true);
    let err = rec
        .query(&query(0..100_000, 100, vec![0]), &cancel)
        .err()
        .unwrap();
    assert!(matches!(err, Error::Cancelled), "{err}");
}

#[test]
fn a_cut_off_file_is_unfinished_and_another_file_is_not_a_recording() {
    let file = fixture("energy.tickwave");
    for at in [100, file.len() / 2, file.len() - 1] {
        let err = Recording::open(Arc::new(file[..at].to_vec()), Cache::new(1 << 20))
            .err()
            .unwrap();
        assert!(matches!(err, Error::Unfinished), "{at}: {err}");
    }
    let err = Recording::open(Arc::new(b"not a recording at all".to_vec()), Cache::new(0))
        .err()
        .unwrap();
    assert!(matches!(err, Error::Decode(_)), "{err}");
}

#[test]
fn a_file_reads_as_its_bytes_do_and_a_small_cache_holds_little() {
    let file = fixture("energy.tickwave");
    let path = std::env::temp_dir().join(format!("tickwave-view-{}.tickwave", std::process::id()));
    std::fs::write(&path, &file).unwrap();
    let cancel = AtomicBool::new(false);
    let q = query(0..2_880_000, 200, vec![0, 4]);
    let from_file = Recording::open(
        Arc::new(std::fs::File::open(&path).unwrap()),
        Cache::new(1 << 20),
    )
    .unwrap();
    let from_arc = Recording::open(
        Arc::new(Arc::<[u8]>::from(file.as_slice())),
        Cache::new(1 << 20),
    )
    .unwrap();
    let a = from_file.query(&q, &cancel).unwrap();
    let b = from_arc.query(&q, &cancel).unwrap();
    // Each names the recording it is from.
    assert_ne!(a.revision, b.revision);
    assert_eq!(
        a,
        Answer {
            revision: a.revision,
            ..b
        }
    );
    std::fs::remove_file(&path).unwrap();
    let cache = Cache::new(1000);
    let small = Recording::open(Arc::new(file.clone()), cache.clone()).unwrap();
    assert_eq!(
        small
            .query(&query(0..100_000, 4000, vec![0]), &cancel)
            .unwrap()
            .channels
            .len(),
        1
    );
    assert!(cache.bytes() <= 1000, "{}", cache.bytes());
}

#[test]
fn columns_come_from_the_coarsest_level_no_wider_than_a_column() {
    // The synthetic cases' A: levels 2^4 and 2^8 in overview blocks, and its strip at 2^12 (818
    // buckets at 2^12 end them). Its value is a code at 1/1000.
    let file = common::synth_cases(1 << 16);
    let rec = open(&file);
    let footer = Index::of_file(&file).unwrap().footer;
    let levels = common::levels(&file, 0);
    let shifts: Vec<u8> = levels.iter().map(|l| l.shift).collect();
    assert_eq!(
        (shifts, footer.channels[0].levels.len()),
        (vec![4, 8, 12], 2)
    );
    let cancel = AtomicBool::new(false);
    let HeaderKind::Analog { first, count, .. } = &footer.header.channels[0].data else {
        panic!()
    };
    let runs = Runs {
        first: *first,
        grid: footer.grid(0).unwrap(),
        count: *count,
        breaks: footer.breaks(0).unwrap(),
        map: footer.header.mapping(0).unwrap(),
    };
    let a0 = common::oracle(&file)[0].clone().unwrap();
    let end = a0
        .samples
        .last()
        .unwrap()
        .1
        .div_euclid(i128::try_from(a0.per).unwrap()) as i64;
    // Ticks a column, and A's level they read: 300 is past 2^8 and short of 2^12, and 70,000
    // past every level.
    for (per, level) in [(300, 1), (4_095, 1), (4_096, 2), (5_000, 2), (70_000, 2)] {
        let window = 500_000..500_000 + per * 40;
        let a = rec
            .query(&query(window.clone(), 40, vec![0]), &cancel)
            .unwrap();
        let Drawn::Analog { columns, .. } = &a.channels[0] else {
            panic!()
        };
        let lv = &levels[level];
        let Some(Buckets::U64(b)) = lv.data.analog() else {
            panic!()
        };
        let value = |x: u64| x as f64 / 1000.0;
        let (lo, hi): (Vec<f64>, Vec<f64>) = b
            .min
            .iter()
            .zip(&b.max)
            .map(|(&x, &z)| {
                if x > z {
                    (f64::INFINITY, f64::NEG_INFINITY)
                } else {
                    (value(x), value(z))
                }
            })
            .unzip();
        let ov = AnalogOverview {
            bucket: 1 << lv.shift,
            start: 0,
            min: &lo,
            max: &hi,
        };
        let (mut min, mut max) = (
            vec![f64::INFINITY; a.grid.count],
            vec![f64::NEG_INFINITY; a.grid.count],
        );
        for span in known_spans(
            &ChannelPlace::Analog { runs, end },
            a.around.start,
            a.around.end,
        ) {
            let cols =
                columns::analog_from_overview(&a.grid, &ov, span.start as f64, span.end as f64);
            for k in 0..a.grid.count {
                if cols.min[k] <= cols.max[k] {
                    min[k] = min[k].min(cols.min[k]);
                    max[k] = max[k].max(cols.max[k]);
                }
            }
        }
        assert_eq!(
            (&columns.min, &columns.max),
            (&min, &max),
            "{per} ticks a column"
        );
    }
}

#[test]
fn markers_outside_the_data_widen_the_extent() {
    let timebase = Ratio::integer(1000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "V".into(),
            source_name: "v".into(),
            source: 0,
            data: DeclaredKind::Analog {
                unit: "V".into(),
                period: Ratio::integer(10).unwrap(),
                coding: escapement_tickwave_format::header::AnalogCoding::F32,
                significant: None,
                phase: None,
            },
            shaping: Default::default(),
            overview_shift: None,
            codec: None,
            requires: Vec::new(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("test".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    let mark = |w: &mut Writer, t: i64| {
        let serde_json::Value::Object(m) = serde_json::json!({"id": format!("m/{t}"), "start": t})
        else {
            unreachable!()
        };
        w.annotation("verge", m).unwrap();
    };
    mark(&mut w, 3);
    w.run(0, 100, 0).unwrap();
    for i in 0..10 {
        w.sample(0, i as f32).unwrap();
    }
    mark(&mut w, 400);
    let mut file = w.take_output();
    let finish = Finish {
        bundle: Some(escapement_tickwave_format::writer::Bundle::Generated { stem: "rec".into() }),
        ..Finish::default()
    };
    file.extend(w.finish(500, &[], finish).unwrap().tail);
    let rec = open(&file);
    // Samples from 100 to 190, the last one's time to 200; marks at 3 and 400.
    assert_eq!(rec.marker_count(), 2);
    assert_eq!(rec.extent(), Some(3..401));
}

/// D: high at 10; not known from 50 to 100, then low, the gap's end at the second block's start;
/// high at 150; not known from 300 to 305, then low; high at 302 + 400. Blocks end at 100 and at
/// 200.
fn gaps_at_block_ends() -> Vec<u8> {
    let timebase = Ratio::integer(1000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "D".into(),
            source_name: "d".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
            overview_shift: None,
            codec: None,
            requires: Vec::new(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("test".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 12,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    w.edge(0, 10).unwrap();
    w.flush(100).unwrap();
    // Given once the block that holds 50 is written, as a live recorder learns of a loss.
    w.gap(
        0,
        escapement_tickwave_format::model::Gap {
            from: 50,
            to: 100,
            level: false,
        },
    )
    .unwrap();
    w.edge(0, 150).unwrap();
    w.flush(200).unwrap();
    w.gap(
        0,
        escapement_tickwave_format::model::Gap {
            from: 300,
            to: 305,
            level: false,
        },
    )
    .unwrap();
    w.edge(0, 702).unwrap();
    let mut file = w.take_output();
    file.extend(w.finish(1000, &[1000], Finish::default()).unwrap().tail);
    file
}

#[test]
fn a_gap_that_ends_at_a_blocks_start_sets_the_level_there() {
    let rec = open(&gaps_at_block_ends());
    let cancel = AtomicBool::new(false);
    // 10 ticks a column from 100: low from the gap's end, rising at 150 (column 5, at its start).
    let a = rec.query(&query(100..200, 10, vec![0]), &cancel).unwrap();
    let window = &digital(&a.channels[0])[10..20];
    use State::{High, Low, Rise};
    assert_eq!(
        window,
        [Low, Low, Low, Low, Low, Rise, High, High, High, High]
    );
    // Read from inside the block, past its start: the same.
    let a = rec.query(&query(120..170, 5, vec![0]), &cancel).unwrap();
    assert_eq!(&digital(&a.channels[0])[5..10], [Low, Low, Low, Rise, High]);
}

#[test]
fn a_column_across_a_short_gap_holds_both_sides() {
    let rec = open(&gaps_at_block_ends());
    let cancel = AtomicBool::new(false);
    // 20 ticks a column from 280: high to the gap at 300, low after 305, so the column from 300
    // to 320 is low, and the one from 280 high. With 50 a column from 280, the first holds high
    // to 300 and low from 305: both levels, busy.
    let a = rec.query(&query(280..380, 5, vec![0]), &cancel).unwrap();
    assert_eq!(&digital(&a.channels[0])[5..7], [State::High, State::Low]);
    let a = rec.query(&query(280..430, 3, vec![0]), &cancel).unwrap();
    assert_eq!(
        &digital(&a.channels[0])[3..6],
        [State::Busy, State::Low, State::Low]
    );
}

#[test]
fn a_run_from_the_last_samples_tick_shades_nothing() {
    // The synthetic cases' third run starts at its second run's last tick, 949,983: a pause with
    // no time in it. A run that states lost samples starts past where the first of them would
    // sit (FORMAT.md, "Gaps"), so a loss always has time; the writer refuses one that doesn't.
    let rec = open(&common::synth_cases(1 << 16));
    let cancel = AtomicBool::new(false);
    let a = rec
        .query(&query(949_900..950_100, 20, vec![0]), &cancel)
        .unwrap();
    let Drawn::Analog { columns, lost, .. } = &a.channels[0] else {
        panic!()
    };
    assert!(lost.is_empty(), "{lost:?}");
    assert!(columns.pale.iter().all(|&p| !p));
    // The fourth run's 50.
    assert_eq!(rec.lost(0), 50);
}

#[test]
fn columns_hold_what_the_rule_gives_at_every_zoom_across_runs() {
    let file = fixture("energy.tickwave");
    let rec = open(&file);
    let cancel = AtomicBool::new(false);
    let length = 2_880_000;
    // Wide columns hold the loss and the pause inside one, with a run on either side; narrow
    // ones put a run's edge on or between columns.
    for width in [length, length / 3, 400_000, 120_000] {
        for start in (-width..length).step_by((width / 5).max(1) as usize) {
            for cols in [3, 7, 40] {
                let a = rec
                    .query(&query(start..start + width, cols, vec![1]), &cancel)
                    .unwrap();
                let Drawn::Analog { columns, .. } = &a.channels[0] else {
                    panic!()
                };
                // The view's level: I's only one, its strip at 2^12 ticks, once a column is as
                // wide.
                let level = (a.grid.per >= 4096.0).then_some(0);
                let (min, max) = oracle_columns(&file, 1, &a.grid, a.around.clone(), level);
                for k in 0..a.grid.count {
                    let near = |x: f64, y: f64| x == y || ((x - y) / y).abs() < 1e-15;
                    assert!(
                        near(columns.min[k], min[k]) && near(columns.max[k], max[k]),
                        "{start}+{width} at {cols} columns, column {k}: {} {} against {} {}",
                        columns.min[k],
                        columns.max[k],
                        min[k],
                        max[k]
                    );
                }
            }
        }
    }
}

/// A footer whose analog channel claims 4,000,000,000 samples in a block that holds one, over
/// 2^33 ticks so they fit in its ticks: the file opens, and a query narrower than a bucket reads
/// samples. Their values grow with what the block gives, which it refuses, rather than taking
/// the claimed count up front.
#[test]
fn a_sample_count_the_blocks_do_not_back_is_not_allocated_before_they_decode() {
    use escapement_tickwave_format::header::AnalogCoding;
    let timebase = Ratio::integer(1000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "V".into(),
            source_name: "v".into(),
            source: 0,
            data: DeclaredKind::Analog {
                unit: "V".into(),
                period: Ratio::ONE,
                coding: AnalogCoding::F32,
                significant: None,
                phase: None,
            },
            shaping: Default::default(),
            overview_shift: None,
            codec: None,
            requires: Vec::new(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 12,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    w.run(0, 0, 0).unwrap();
    w.sample(0, 1.5f32).unwrap();
    let mut file = w.take_output();
    file.extend(w.finish(1 << 33, &[], Finish::default()).unwrap().tail);
    use escapement_tickwave_format::header::BlockRow;
    use escapement_tickwave_format::index::PAGE_HEAD;
    use escapement_tickwave_format::record::{self, Kind, PAYLOAD_AT};
    const CLAIMED: u32 = 4_000_000_000;
    // The block's row in its index page claims them, its record's check made again.
    let footer = Footer::parse_file(&file).unwrap();
    let page = footer.channels[0].blocks.as_ref().unwrap().levels[0][0];
    let at = page.offset as usize;
    let mut payload = file[at + PAYLOAD_AT..at + page.length as usize - 8].to_vec();
    let row_at = PAGE_HEAD..PAGE_HEAD + BlockRow::SIZE;
    let mut row = BlockRow::parse(&payload[row_at.clone()]);
    row.word = CLAIMED;
    let mut bytes = Vec::new();
    row.write(&mut bytes);
    payload[row_at].copy_from_slice(&bytes);
    let mut fixed = Vec::new();
    record::write(&mut fixed, Kind::IndexPage, &[&payload]);
    file[at..at + fixed.len()].copy_from_slice(&fixed);
    // And the footer's header, written again.
    let keep = footer.trailer.footer_offset as usize;
    let payload = &file[keep + PAYLOAD_AT..keep + footer.trailer.footer_len as usize - 8];
    let header_len = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
    let mut header = footer.header.clone();
    let HeaderKind::Analog { count, .. } = &mut header.channels[0].data else {
        panic!()
    };
    *count = u64::from(CLAIMED);
    let json = serde_json::to_vec(&header).unwrap();
    let mut new = (json.len() as u32).to_le_bytes().to_vec();
    new.extend_from_slice(&payload[4..8]);
    new.extend_from_slice(&json);
    new.extend_from_slice(&payload[8 + header_len..]);
    file.truncate(keep);
    let mut tail = Vec::new();
    record::write_footer(&mut tail, keep as u64, &new);
    file.extend(tail);
    let rec = open(&file);
    let err = rec
        .query(&query(0..4096, 1000, vec![0]), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(err, Error::Decode(_)), "{err}");
}

/// Markers as verge's recorder writes them: ids `marker/<tick>`, and `marker/<tick>.<n>` for the
/// n-th further one on a tick, kind `marker` or `marker-after-loss`, a label with control
/// characters, backslashes and bytes that aren't UTF-8 escaped as `\xNN`, a `number` value, and
/// no end or channels. The writer takes each, its finish at the last marker's tick and one, and
/// the bundle opens in the view with every marker.
#[test]
fn a_verge_recorder_s_markers_write_bundle_and_open() {
    let timebase = Ratio::integer(96_000_000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![
            DeclaredChannel {
                name: "V".into(),
                source_name: "v".into(),
                source: 0,
                data: DeclaredKind::Analog {
                    unit: "V".into(),
                    period: Ratio::integer(960).unwrap(),
                    coding: escapement_tickwave_format::header::AnalogCoding::F32,
                    significant: None,
                    phase: None,
                },
                shaping: Default::default(),
                overview_shift: None,
                codec: None,
                requires: Vec::new(),
            },
            DeclaredChannel {
                name: "DIO0".into(),
                source_name: "dio0".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Default::default(),
                overview_shift: None,
                codec: None,
                requires: Vec::new(),
            },
        ],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("verge-energy".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 20,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    w.run(0, 0, 0).unwrap();
    for i in 0..6 {
        w.sample(0, i as f32).unwrap();
    }
    w.edge(1, 5_000).unwrap();
    for i in 6..100 {
        w.sample(0, i as f32).unwrap();
    }
    let ticks = [960, 9_600, 9_600, 9_600, 48_000, 95_040];
    let mut seen: BTreeMap<i64, u32> = BTreeMap::new();
    for (n, &tick) in ticks.iter().enumerate() {
        let again = seen.entry(tick).or_default();
        let id = match *again {
            0 => format!("marker/{tick}"),
            k => format!("marker/{tick}.{k}"),
        };
        *again += 1;
        let kind = if n == 4 {
            "marker-after-loss"
        } else {
            "marker"
        };
        let serde_json::Value::Object(m) = serde_json::json!({
            "id": id,
            "kind": kind,
            "start": tick,
            "label": "step \\x07 \\\\ \\xff",
            "values": {"number": n.to_string()},
        }) else {
            unreachable!()
        };
        w.annotation("verge", m).unwrap();
    }
    let mut file = w.take_output();
    let finish = Finish {
        bundle: Some(escapement_tickwave_format::writer::Bundle::Generated { stem: "rec".into() }),
        ..Finish::default()
    };
    let length = ticks.iter().max().unwrap() + 1;
    file.extend(w.finish(length, &[length], finish).unwrap().tail);
    let rec = open(&file);
    assert_eq!(rec.marker_count(), ticks.len());
}
