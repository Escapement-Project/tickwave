//! What a window reads against the whole walk: its known spans, lost spans and gaps are those of
//! the whole channel that reach into it, and nothing in an answer joins across a run's edge.

use std::ops::Range;
use std::sync::atomic::AtomicBool;

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::header::ChannelKind as HeaderKind;
use escapement_tickwave_format::mapping::Mapping;
use escapement_tickwave_format::model::{Break, Gap, Grid, Ratio};
use escapement_tickwave_view::runs::{Runs, lost_spans_in, place_after};
use escapement_tickwave_view::spans::{ChannelPlace, known_spans};
use escapement_tickwave_view::{Drawn, Query};

mod common;
use common::{fixture, open};

/// Runs of samples every 3/2 ticks: 5 from 0, 2 lost; 3 from 30, a pause; 4 from 40, 6 lost,
/// cut short by the run at 47; 3 from 47; 1 lost; 2 from 60.
const BREAKS: [Break; 4] = [
    Break {
        index: 5,
        tick: 30,
        lost: 2,
        phase: 0,
    },
    Break {
        index: 8,
        tick: 40,
        lost: 0,
        phase: 0,
    },
    Break {
        index: 12,
        tick: 47,
        lost: 6,
        phase: 0,
    },
    Break {
        index: 15,
        tick: 60,
        lost: 1,
        phase: 0,
    },
];

fn runs() -> Runs<'static> {
    Runs {
        first: 0,
        grid: Grid::new(Ratio::new(3, 2).unwrap(), None).unwrap(),
        count: 17,
        breaks: &BREAKS,
        map: Mapping::IDENTITY,
    }
}

/// Every window `a..b` with both ends in `lo..hi`.
fn windows(lo: i64, hi: i64) -> impl Iterator<Item = Range<i64>> {
    (lo..hi).flat_map(move |a| (a..hi).map(move |b| a..b))
}

#[test]
fn a_windows_lost_spans_are_the_channels_that_reach_into_it() {
    let r = runs();
    let all: Vec<Range<i64>> = (0..BREAKS.len())
        .filter_map(|k| r.grid.lost_time(r.first, r.breaks, k))
        .map(|t| r.at(t.start)..r.at(t.end))
        .collect();
    for w in windows(-3, 70) {
        let want: Vec<Range<i64>> = all
            .iter()
            .filter(|s| s.end > w.start && s.start < w.end)
            .cloned()
            .collect();
        let got: Vec<Range<i64>> = lost_spans_in(&r, w.clone())
            .iter()
            .map(|l| l.from..l.to)
            .collect();
        assert_eq!(got, want, "{w:?}");
    }
}

/// Every span of `c`, clipped to [start, end): the walk over each run or gap, from the first.
fn known_whole(c: &ChannelPlace, start: i64, end: i64) -> Vec<Range<i64>> {
    let mut spans = Vec::new();
    let stop = end.min(c.end());
    let mut keep = |a: i64, b: i64| {
        let (s, e) = (a.max(start).max(c.start()), b.min(stop));
        if e > s {
            spans.push(s..e);
        }
    };
    match c {
        ChannelPlace::Analog { runs, end } => {
            let b = runs.breaks;
            for k in 0..=b.len() {
                let from = if k == 0 { runs.first } else { b[k - 1].tick };
                let to = match b.get(k) {
                    Some(next) => next.tick.min(place_after(runs, next.index - 1)),
                    None => *end,
                };
                keep(from, to);
            }
        }
        ChannelPlace::Digital { gaps, .. } => {
            let mut at = i64::MIN;
            for g in *gaps {
                keep(at, g.from);
                at = g.to;
            }
            keep(at, i64::MAX);
        }
    }
    spans
}

#[test]
fn a_windows_known_spans_are_the_whole_walks_clipped() {
    let r = runs();
    let analog = ChannelPlace::Analog {
        runs: r,
        end: place_after(&r, r.count - 1),
    };
    let gaps = [
        Gap {
            from: 5,
            to: 9,
            level: true,
        },
        Gap {
            from: 20,
            to: 21,
            level: false,
        },
        Gap {
            from: 22,
            to: 40,
            level: true,
        },
    ];
    let digital = ChannelPlace::Digital {
        start: 2,
        end: 60,
        gaps: &gaps,
    };
    for c in [analog, digital] {
        for w in windows(-3, 70) {
            assert_eq!(
                known_spans(&c, w.start, w.end),
                known_whole(&c, w.start, w.end),
                "{c:?}, {w:?}"
            );
        }
    }
}

/// The columns of a grid from `x0`, `per` ticks each, that any tick of `span` falls in.
fn columns_of(x0: f64, per: f64, count: usize, span: &Range<i64>) -> Range<usize> {
    let c0 = ((span.start as f64 - x0) / per).floor().max(0.0) as usize;
    let c1 = ((span.end as f64 - x0) / per)
        .ceil()
        .clamp(0.0, count as f64) as usize;
    c0..c1.max(c0)
}

#[test]
fn an_answer_joins_nothing_across_a_runs_edge_and_gives_the_gaps_it_reaches() {
    let file = fixture("energy.tickwave");
    let f = Footer::parse_file(&file).unwrap();
    let rec = open(&file);
    let cancel = AtomicBool::new(false);
    let HeaderKind::Analog {
        first,
        count,
        breaks,
        ..
    } = &f.header.channels[0].data
    else {
        panic!("V is analog")
    };
    let runs = Runs {
        first: *first,
        grid: f.grid(0).unwrap(),
        count: *count,
        breaks,
        map: f.header.mapping(0).unwrap(),
    };
    let place = ChannelPlace::Analog {
        runs,
        end: place_after(&runs, count - 1).min(f.header.length),
    };
    let HeaderKind::Digital { gaps, .. } = &f.header.channels[4].data else {
        panic!("DIO0 is digital")
    };
    let length = f.header.length;
    // Windows from a whole one down to a few samples' worth, at starts that put the loss, the
    // pause and the gap's ends on and between column edges.
    // `around` starts a width before the window: these put its start on each end and edge of
    // the loss, the pause and DIO0's gap, and a tick either side.
    let events = [960_000, 1_008_000, 1_776_000, 2_000_000];
    for width in [length, length / 3, 120_000, 9_600, 960] {
        let near = events
            .iter()
            .flat_map(|&t| [t - 1, t, t + 1])
            .map(|t| t + width);
        for start in (-width..length)
            .step_by((width / 7).max(1) as usize)
            .chain(near)
        {
            for columns in [7, 40] {
                let q = {
                    let mut query = Query::new(start..start + width, columns);
                    query.channels = vec![0, 4];
                    query
                };
                let a = rec.query(&q, &cancel).unwrap();
                let at = format!("{:?} at {columns} columns", q.window);
                let Drawn::Analog {
                    columns: v,
                    samples,
                    ..
                } = &a.channels[0]
                else {
                    panic!("V is analog")
                };
                let spans = known_whole(&place, a.around.start, a.around.end);
                let (x0, per, n) = (a.grid.x0, a.grid.per, a.grid.count);
                for (k, _) in v.joined.iter().enumerate().skip(1).filter(|(_, j)| **j) {
                    assert!(
                        spans.iter().any(|s| {
                            let c = columns_of(x0, per, n, s);
                            c.contains(&(k - 1)) && c.contains(&k)
                        }),
                        "{at}: column {k} joins {} across a run's edge",
                        k - 1
                    );
                }
                for (i, p) in samples.iter().flatten().enumerate() {
                    if i == 0 || breaks.iter().any(|b| runs.tick(b.index) == p.tick) {
                        assert!(!p.joined, "{at}: the sample at {} is joined", p.tick);
                    }
                }
                let Drawn::Digital { gaps: got, .. } = &a.channels[1] else {
                    panic!("DIO0 is digital")
                };
                let want: Vec<Range<i64>> = gaps
                    .iter()
                    .filter(|g| g.from < a.around.end && g.to > a.around.start)
                    .map(|g| g.from..g.to)
                    .collect();
                assert_eq!(got, &want, "{at}");
            }
        }
    }
}

#[test]
fn a_run_that_shares_a_column_with_the_one_before_joins_from_its_own_values() {
    use escapement_tickwave_format::header::{
        AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, IntCoding, SourceFormat,
        SourceHeader,
    };
    use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
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
                coding: AnalogCoding::Int(IntCoding::new(32, false)),
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
            SourceFormat::Other("crafted".into()),
            timebase,
        )],
        title: None,
        meta: Default::default(),
        overview_shift: 12,
        requires: Vec::new(),
    };
    // A run of ten 0s from tick 0, a pause, then 100 at 95 and 10 at 105 and 115.
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    w.run(0, 0, 0).unwrap();
    for _ in 0..10 {
        w.sample(0, 0u64).unwrap();
    }
    w.run(0, 95, 0).unwrap();
    for v in [100u64, 10, 10] {
        w.sample(0, v).unwrap();
    }
    let mut file = w.take_output();
    file.extend(w.finish(130, &[], Finish::default()).unwrap().tail);
    let rec = open(&file);
    // `around` 0..120 in six columns of 20 ticks: column 4 holds the first run's last 0 and the
    // second's 100, column 5 its 10s.
    let q = {
        let mut query = Query::new(40..80, 2);
        query.channels = vec![0];
        query
    };
    let a = rec.query(&q, &AtomicBool::new(false)).unwrap();
    assert_eq!((a.grid.x0, a.grid.per, a.grid.count), (0.0, 20.0, 6));
    let Drawn::Analog { columns: c, .. } = &a.channels[0] else {
        panic!("V is analog")
    };
    // The second run's trace falls from its 100 in column 4 to the 10s in column 5, so column 5
    // reaches up to 100; the first run's 0 in column 4 is no part of that.
    assert_eq!((c.min[4], c.max[4]), (0.0, 100.0));
    assert_eq!((c.min[5], c.max[5]), (10.0, 100.0));
    assert!(c.joined[5]);
}

#[test]
fn a_gap_from_tick_0_sets_the_level_from_its_end() {
    use escapement_tickwave_view::columns::State;
    // crates/format/examples/gaps.rs, as web/test/wasm.test.ts reads it: A is declared low and
    // not known until 300 µs, high there, and runs a 10 µs clock from 400 µs.
    let rec = open(&fixture("leading-gap.tickwave"));
    let cancel = AtomicBool::new(false);
    assert_eq!(
        rec.edges(0, 300_000, 410_000, &cancel).unwrap(),
        (true, vec![400_000, 405_000])
    );
    assert_eq!(
        rec.edges(0, 300_000, 300_001, &cancel).unwrap(),
        (true, vec![])
    );
    // Drawn from the edges: high from the gap's end to the clock's first fall.
    let q = {
        let mut query = Query::new(300_000..400_000, 100);
        query.channels = vec![0];
        query
    };
    let a = rec.query(&q, &cancel).unwrap();
    let Drawn::Digital { columns, .. } = &a.channels[0] else {
        panic!("A is digital")
    };
    // `around` from 200,000 in columns of 1,000 ticks: 300,000 to 400,000 is columns 100 to 199.
    assert!(
        (100..200).all(|k| columns.state[k] == State::High),
        "{:?}",
        &columns.state[100..200]
    );
}

#[test]
fn a_live_recordings_gap_from_tick_0_sets_the_level_from_its_end() {
    use escapement_tickwave_format::header::{
        Declaration, DeclaredChannel, DeclaredKind, SourceFormat, SourceHeader,
    };
    use escapement_tickwave_format::writer::{WriteOptions, Writer};
    use escapement_tickwave_view::{Cache, Recording};
    use std::sync::Arc;
    let timebase = Ratio::integer(1000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "A".into(),
            source_name: "a".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
            overview_shift: None,
            codec: None,
            requires: Vec::new(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("crafted".into()),
            timebase,
        )],
        title: None,
        meta: Default::default(),
        overview_shift: 4,
        requires: Vec::new(),
    };
    // Declared low, not known until 300 and high there, then edges at 400 and 405; a flush first
    // so the recording is made before the gap and takes it in an update.
    struct Growing(std::sync::Mutex<Vec<u8>>);
    impl escapement_tickwave_view::Bytes for Growing {
        fn size(&self) -> std::io::Result<u64> {
            Ok(self.0.lock().unwrap().len() as u64)
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
            let file = self.0.lock().unwrap();
            buf.copy_from_slice(&file[offset as usize..][..buf.len()]);
            Ok(())
        }
    }
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    let file = Arc::new(Growing(std::sync::Mutex::new(w.take_output())));
    let mut rec = Recording::live(w.snapshot(), file.clone(), Cache::new(1 << 20)).unwrap();
    w.gap(
        0,
        Gap {
            from: 0,
            to: 300,
            level: true,
        },
    )
    .unwrap();
    w.edge(0, 400).unwrap();
    w.edge(0, 405).unwrap();
    w.flush(410).unwrap();
    let snapshot = w.snapshot();
    file.0.lock().unwrap().extend(w.take_output());
    let fresh = Recording::live(snapshot.clone(), file.clone(), Cache::new(1 << 20)).unwrap();
    rec.update(snapshot).unwrap();
    let cancel = AtomicBool::new(false);
    for r in [&rec, &fresh] {
        assert_eq!(
            r.edges(0, 300, 410, &cancel).unwrap(),
            (true, vec![400, 405])
        );
    }
}
