//! The exact cursors against the view's own readers and the format crate, on tickwave's fixtures
//! and recordings built here: every edge with its source tick and ordinal, the known spans, each
//! sample at its place with the breaks and losses around it, a live recording's settled prefix
//! against the same writes finished, the pages a window reads, and the blocks it shares with the
//! view's cache.

mod common;

use std::collections::BTreeMap;
use std::ops::{ControlFlow, Range};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use escapement_tickwave_format::decode::{
    BlockChannel, DecodeError, Footer, Index, from_memory, lost_after_end, lost_before_start,
};
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::{
    AnalogCoding, ChannelKind, Declaration, DeclaredChannel, DeclaredKind, IntCoding, SourceFormat,
    SourceHeader,
};
use escapement_tickwave_format::index::Entry;
use escapement_tickwave_format::model::{
    self, Analog, Channel, ChannelData, Digital, Gap, Ratio, Samples, Shaping, Source,
};
use escapement_tickwave_format::writer::{Compression, Finish, WriteOptions, Writer};
use escapement_tickwave_view::cursor::{
    AnalogCursor, Budget, DigitalCursor, Edge, Exact, Groups, Merge, PortCursor, Sampled, Seen,
    Until, whole_line,
};
use escapement_tickwave_view::spans::known_spans;
use escapement_tickwave_view::{Bytes, Cache, Error, Query, Recording};
use num_bigint::BigInt;

static NEVER: AtomicBool = AtomicBool::new(false);

fn view(file: &[u8]) -> Recording {
    Recording::open(Arc::new(file.to_vec()), Cache::new(64 << 20)).unwrap()
}

fn budget(items: usize) -> Budget {
    Budget {
        items,
        block_bytes: u32::MAX,
    }
}

/// Digital channel `c` from axis tick `from` to `to`, in windows of `step` ticks, each read a
/// batch of `budget` at a time into one list.
fn all(rec: &Recording, c: usize, from: i64, to: i64, budget: &Budget, step: i64) -> Vec<Seen> {
    let mut cur = DigitalCursor::seek(rec, c, from, budget, &NEVER).unwrap();
    let mut out = vec![];
    let mut until = from;
    loop {
        until = until.saturating_add(step).min(to);
        while cur
            .next(rec, until, budget, &NEVER, &mut out)
            .unwrap()
            .reached
            < until
        {}
        if until >= to {
            return out;
        }
    }
}

fn edges(items: &[Seen]) -> Vec<Edge> {
    items
        .iter()
        .filter_map(|s| match s {
            Seen::Edge(e) => Some(*e),
            _ => None,
        })
        .collect()
}

fn samples(items: &[Seen]) -> Vec<Sampled> {
    items
        .iter()
        .filter_map(|s| match s {
            Seen::Sample(x) => Some(*x),
            _ => None,
        })
        .collect()
}

/// The known spans the observations state, on the axis.
fn known(items: &[Seen], end: i64) -> Vec<Range<i64>> {
    let mut out = vec![];
    let mut from = None;
    for s in items {
        match *s {
            Seen::Known { axis, .. } => from = from.or(Some(axis)),
            Seen::Unknown { axis, .. } => {
                if let Some(f) = from.take()
                    && axis > f
                {
                    out.push(f..axis);
                }
            }
            _ => {}
        }
    }
    if let Some(f) = from {
        out.push(f..end);
    }
    out
}

fn source() -> Source {
    Source::new(
        SourceFormat::Other("cursor-test".into()),
        Ratio::integer(1000).unwrap(),
    )
}

fn channel(source: u32, edges: Vec<i64>, end: i64) -> Channel {
    named(&format!("D{source}"), source, edges, end, vec![])
}

fn named(name: &str, source: u32, edges: Vec<i64>, end: i64, gaps: Vec<Gap>) -> Channel {
    Channel {
        name: name.into(),
        source_name: name.into(),
        source,
        data: ChannelData::Digital(Digital {
            initial: false,
            end,
            count: edges.len() as u64,
            edges: Box::new(edges.into_iter()),
            gaps,
        }),
        shaping: Shaping {
            kept: Some(model::Kept { from: 0, to: end }),
            ..Shaping::default()
        },
    }
}

fn encoded_with(
    channels: Vec<Channel>,
    sources: Vec<Source>,
    length: i64,
    block_bytes: usize,
) -> Vec<u8> {
    let mut options = Options::default();
    options.level = 1;
    options.block_bytes = block_bytes;
    encode(
        model::Recording {
            timebase: Ratio::integer(1000).unwrap(),
            length,
            channels,
            derived: vec![],
            sources,
        },
        &options,
    )
    .unwrap()
}

fn encoded(channels: Vec<Channel>, sources: Vec<Source>, length: i64) -> Vec<u8> {
    encoded_with(channels, sources, length, 128)
}

/// Whether recording `rec`'s channel `c` is digital.
fn digital(rec: &Recording, c: usize) -> bool {
    matches!(
        rec.channels()[c].kind,
        escapement_tickwave_view::ChannelKind::Digital
    )
}

#[test]
fn the_cursor_matches_the_view_reader_on_tickwaves_captures() {
    let mut compared = 0;
    for name in [
        "identify-swd.tickwave",
        "identify-swd.4k.tickwave",
        "identify-jtag.tickwave",
    ] {
        let file = common::fixture(name);
        let r = view(&file);
        let length = r.length();
        for c in (0..r.channels().len()).filter(|&c| digital(&r, c)) {
            let items = all(&r, c, 0, length, &Budget::LARGE, length);
            let found = edges(&items);
            let (before, expected) = r.edges(c, 0, length, &NEVER).unwrap();
            assert_eq!(
                found.iter().map(|e| e.axis).collect::<Vec<_>>(),
                expected,
                "{name} channel {c}"
            );
            assert!(found.iter().enumerate().all(|(i, e)| e.ordinal == i as u64));
            assert!(found.iter().all(|e| e.source_tick == e.axis));
            let mut level = before;
            for e in &found {
                level = !level;
                assert_eq!(e.level, level, "{name} channel {c}");
            }
            assert_eq!(
                known(&items, length),
                known_spans(&r.known(c).unwrap(), 0, length),
                "{name} channel {c}"
            );
            compared += found.len();
        }
    }
    assert!(compared > 70_000, "{compared} edges");
}

/// Edges at varied spacing with pulses two edges wide at one source tick, a gap, and a source
/// stretched by 1/2, so pairs of source ticks share an axis tick, in blocks of 128 bytes.
fn dense() -> (Vec<u8>, i64) {
    let mut edges = vec![];
    let mut t = 3i64;
    for k in 0..3000i64 {
        if (9000..=9100).contains(&t) {
            t = 9101;
        }
        edges.push(t);
        if k % 17 == 0 {
            edges.push(t);
        }
        t += 1 + (k * 7919) % 9;
    }
    let mut s = source();
    s.stretch = Ratio::new(1, 2);
    let end = t + 10;
    let gap = Gap {
        from: 9000,
        to: 9100,
        level: true,
    };
    let length = end / 2 + 1;
    (
        encoded(vec![named("D", 0, edges, end, vec![gap])], vec![s], length),
        length,
    )
}

#[test]
fn batch_sizes_and_windows_do_not_change_what_the_cursor_reports() {
    let (file, length) = dense();
    let r = view(&file);
    let whole = all(&r, 0, 0, length, &Budget::LARGE, length);
    assert!(edges(&whole).len() > 3000);
    assert!(whole.iter().any(|s| matches!(s, Seen::Unknown { .. })));
    for (items, step) in [
        (1, 1_000_003),
        (7, 333),
        (1000, 97),
        (usize::MAX, 1),
        (2, 1),
    ] {
        assert_eq!(
            all(&r, 0, 0, length, &budget(items), step),
            whole,
            "{items} {step}"
        );
    }
    // A real capture, in a window of its densest edges.
    let file = common::fixture("identify-jtag.tickwave");
    let r = view(&file);
    let (_, tck) = r.edges(0, 0, r.length(), &NEVER).unwrap();
    let (from, to) = (tck[20_000], tck[22_000]);
    let whole = all(&r, 0, from, to, &Budget::LARGE, to - from);
    assert_eq!(edges(&whole).len(), 2000);
    let span = to - from;
    for (items, step) in [(1, span / 7 + 1), (7, span / 333), (1000, span / 50 + 1)] {
        assert_eq!(
            all(&r, 0, from, to, &budget(items), step),
            whole,
            "{items} {step}"
        );
    }
}

#[test]
fn seeking_agrees_with_the_view_reader_on_levels_and_gaps() {
    let mut levels = 0;
    for name in [
        "middle-gap.tickwave",
        "leading-gap.tickwave",
        "identify-swd.4k.tickwave",
    ] {
        let file = common::fixture(name);
        let r = view(&file);
        let length = r.length();
        for c in (0..r.channels().len()).filter(|&c| digital(&r, c)) {
            let spans = known_spans(&r.known(c).unwrap(), 0, length);
            let whole = all(&r, c, 0, length, &Budget::LARGE, length);
            let boundaries: Vec<i64> = whole
                .iter()
                .filter(|s| !matches!(s, Seen::Edge(_)))
                .map(Seen::axis)
                .collect();
            let mut ticks: Vec<i64> = (0..64).map(|k| k * (length / 64)).collect();
            for &b in &boundaries {
                ticks.extend([b - 1, b, b + 1]);
            }
            for e in edges(&whole).iter().step_by(97) {
                ticks.extend([e.axis, e.axis + 1]);
            }
            for t in ticks.into_iter().filter(|&t| (0..length).contains(&t)) {
                let cur = DigitalCursor::seek(&r, c, t, &Budget::LARGE, &NEVER).unwrap();
                let rest: Vec<Seen> = whole.iter().filter(|s| s.axis() >= t).copied().collect();
                let mut got = vec![];
                cur.clone()
                    .next(&r, length, &Budget::LARGE, &NEVER, &mut got)
                    .unwrap();
                assert_eq!(got, rest, "{name} channel {c} at {t}");
                if boundaries.contains(&t) {
                    // A boundary at `t` is still to come: the level before it.
                    continue;
                }
                let expected = spans
                    .iter()
                    .any(|s| s.contains(&t))
                    .then(|| r.edges(c, t, t + 1, &NEVER).unwrap().0);
                assert_eq!(cur.level(), expected, "{name} channel {c} at {t}");
                levels += 1;
            }
        }
    }
    assert!(levels > 400, "{levels} levels");
}

#[test]
fn an_edge_at_the_channels_start_changes_the_level_the_start_states() {
    // A channel from source tick 0, an edge there and a pulse two edges wide at 10: the level
    // is high from 0 to 20.
    let file = encoded(
        vec![channel(0, vec![0, 10, 10, 20], 40)],
        vec![source()],
        40,
    );
    let r = view(&file);
    for t in 0..40 {
        let cur = DigitalCursor::seek(&r, 0, t, &Budget::LARGE, &NEVER).unwrap();
        let (level, _) = r.edges(0, t, t + 1, &NEVER).unwrap();
        if t > 0 {
            assert_eq!(cur.level(), Some(level), "at {t}");
        }
        // The level at a tick is the one before its edges.
        assert_eq!(
            cur.level(),
            (t > 0).then_some((1..=20).contains(&t)),
            "at {t}"
        );
    }
}

#[test]
fn edges_sharing_an_axis_tick_stay_distinct_and_resume_exactly() {
    let mut s = source();
    s.stretch = Ratio::new(1, 2);
    let file = encoded(vec![channel(0, vec![2, 3], 8)], vec![s], 4);
    let r = view(&file);
    let one = budget(1);
    let mut cur = DigitalCursor::seek(&r, 0, 0, &one, &NEVER).unwrap();
    let mut items = vec![];
    let mut reached = vec![];
    loop {
        let b = cur.next(&r, 4, &one, &NEVER, &mut items).unwrap();
        reached.push(b.reached);
        if b.reached >= 4 {
            break;
        }
    }
    let e = edges(&items);
    assert_eq!(
        e.iter()
            .map(|e| (e.ordinal, e.source_tick, e.axis))
            .collect::<Vec<_>>(),
        vec![(0, 2, 1), (1, 3, 1)]
    );
    // After the first edge the batch stops inside tick 1: the second is still to come.
    assert_eq!(reached[..3], [1, 1, 4]);
    let (a, b) = (cur.exact(&e[0]), cur.exact(&e[1]));
    assert!(a < b && a.floor() == b.floor());
    assert_eq!((a.num(), a.den(), b.num(), b.den()), (2, 2, 3, 2));
}

#[test]
fn a_timeline_bound_splits_two_edges_that_share_an_axis_tick() {
    let mut s = source();
    s.stretch = Ratio::new(1, 2);
    let file = encoded(vec![channel(0, vec![2, 3], 8)], vec![s], 4);
    let r = view(&file);
    let mut cur = DigitalCursor::seek(&r, 0, 0, &Budget::LARGE, &NEVER).unwrap();
    let mut a = vec![];
    let ba = cur
        .next_in(&r, Until::Source(3), &Budget::LARGE, &NEVER, &mut a)
        .unwrap();
    assert_eq!(ba.reached, 3);
    assert_eq!(
        edges(&a).iter().map(|e| e.source_tick).collect::<Vec<_>>(),
        vec![2]
    );
    let mut b = vec![];
    cur.next_in(&r, Until::Source(8), &Budget::LARGE, &NEVER, &mut b)
        .unwrap();
    assert_eq!(
        edges(&b).iter().map(|e| e.source_tick).collect::<Vec<_>>(),
        vec![3]
    );
    // Seeking the timeline lands between them, though both are on axis tick 1.
    let at3 = DigitalCursor::seek_timeline(&r, 0, 3, &Budget::LARGE, &NEVER).unwrap();
    assert_eq!((at3.next_ordinal(), at3.level()), (1, Some(true)));
}

#[test]
fn a_channels_end_and_timeline_carry_its_offset() {
    // Moved 7 ticks later: source tick k on the axis, and on the timeline, at k + 7.
    let mut ch = channel(0, vec![2, 3], 8);
    ch.shaping.offset = Some(escapement_tickwave_format::header::Offset {
        ticks: 7,
        kind: escapement_tickwave_format::header::Offset::DELAY.into(),
    });
    // An unmoved channel starts the recording at axis tick 0.
    let file = encoded(
        vec![ch, named("Z", 0, vec![], 16, vec![])],
        vec![source()],
        16,
    );
    let r = view(&file);
    let mut cur = DigitalCursor::seek(&r, 0, 0, &Budget::LARGE, &NEVER).unwrap();
    assert_eq!(cur.end(), Some(15));
    let mut seen = vec![];
    cur.next(&r, 16, &Budget::LARGE, &NEVER, &mut seen).unwrap();
    let e = edges(&seen);
    assert_eq!(
        e.iter()
            .map(|e| (e.source_tick, e.axis, cur.timeline(e.source_tick)))
            .collect::<Vec<_>>(),
        vec![(2, 9, 9), (3, 10, 10)]
    );
    assert_eq!(seen.last().map(Seen::axis), Some(15));
    // The channel's end on the timeline bounds a stretch read to it.
    let lines = whole_line(&r, 0, cur.end().unwrap(), &NEVER).unwrap();
    assert_eq!(lines.last().map(|s| s.to), Some(15));
}

#[test]
fn exact_times_order_sources_whose_denominators_pass_u64() {
    // Two sources: one on the axis's ticks, one stretched by (2^52 + 1) / 2^52, whose analog
    // samples sit 1/4099 of a tick past whole ticks: a denominator of 4099 × 2^52, past 2^64.
    let d = 1u64 << 52;
    let mut b = source();
    b.stretch = Ratio::new(d + 1, d);
    let analog = Channel {
        name: "A".into(),
        source_name: "A".into(),
        source: 1,
        data: ChannelData::Analog(Analog {
            unit: "V".into(),
            first: 0,
            phase: Ratio::new(1, 4099),
            period: Ratio::integer(1).unwrap(),
            count: 4,
            breaks: vec![],
            lost_before: 0,
            lost_after: 0,
            significant: None,
            samples: Samples::F64(Box::new([0.0, 1.0, 2.0, 3.0].into_iter())),
        }),
        shaping: Shaping {
            kept: Some(model::Kept { from: 0, to: 8 }),
            ..Shaping::default()
        },
    };
    let file = encoded(
        vec![channel(0, vec![1, 2, 3], 8), analog],
        vec![source(), b],
        8,
    );
    let r = view(&file);
    let mut dc = DigitalCursor::seek(&r, 0, 0, &Budget::LARGE, &NEVER).unwrap();
    let mut seen = vec![];
    dc.next(&r, 8, &Budget::LARGE, &NEVER, &mut seen).unwrap();
    let digital = edges(&seen);
    let mut ac = AnalogCursor::seek(&r, 1, 0, &NEVER).unwrap();
    let mut seen = vec![];
    ac.next(&r, 8, &Budget::LARGE, &NEVER, &mut seen).unwrap();
    let samples = samples(&seen);
    assert_eq!(samples.len(), 4);
    for (k, s) in samples.iter().enumerate().skip(1) {
        let x = ac.exact(s);
        assert!(x.den() > u128::from(u64::MAX));
        let e = dc.exact(&digital[k - 1]);
        // Sample k sits just after whole tick k: on the same axis tick as edge k, and later.
        assert_eq!((x.floor(), e.floor()), (k as i64, k as i64));
        assert!(e < x);
        assert_eq!(x.floor(), s.axis);
    }
    assert_eq!(
        Exact::new(3, 6).unwrap(),
        Exact::new(1 << 100, 1 << 101).unwrap()
    );
}

/// `a / b` against `c / d` in whole numbers, which no width bounds.
fn big_cmp((a, b): (i128, u128), (c, d): (i128, u128)) -> std::cmp::Ordering {
    (BigInt::from(a) * BigInt::from(d)).cmp(&(BigInt::from(c) * BigInt::from(b)))
}

#[test]
fn exact_times_compare_as_their_fractions_do() {
    let big = 1i128 << 120;
    let den = 1u128 << 69;
    let rows: Vec<((i128, u128), (i128, u128))> = vec![
        // Equal denominators, the common case.
        ((5, 1), (7, 1)),
        ((-5, 3), (-7, 3)),
        ((7, 1), (5, 1)),
        // Cross products that fit 128 bits.
        ((3, 6), (1, 2)),
        ((-1, 3), (-1, 2)),
        ((10, 7), (13, 9)),
        // Cross products past 128 bits, where a wrapping product reverses the order.
        ((big, den + 1), (big + 1, den + 3)),
        ((big + 1, den + 3), (big, den + 1)),
        ((-big, den + 1), (-big - 1, den + 3)),
        ((-big - 1, den + 3), (-big, den + 1)),
        ((big, den), (big, den - 1)),
        ((i128::MAX, 3), (i128::MAX - 1, 2)),
        ((i128::MIN, 3), (i128::MIN + 1, 2)),
        ((i128::MAX, 2), (i128::MIN, 3)),
        ((0, den), (-1, den + 1)),
        ((0, 5), (0, den)),
        // The largest denominator taken, and terms whose middle partial products carry.
        ((big, i128::MAX as u128), (big + 1, i128::MAX as u128 - 1)),
        (
            (i128::MAX, i128::MAX as u128),
            (i128::MAX - 1, i128::MAX as u128 - 1),
        ),
        (
            (i128::MAX - 5, i128::MAX as u128 - 7),
            (i128::MAX - 6, i128::MAX as u128 - 9),
        ),
        (
            (i128::MIN + 1, i128::MAX as u128),
            (i128::MIN + 2, i128::MAX as u128 - 1),
        ),
    ];
    let mut wide = 0;
    for (x, y) in rows {
        let (a, b) = (Exact::new(x.0, x.1).unwrap(), Exact::new(y.0, y.1).unwrap());
        assert_eq!(a.cmp(&b), big_cmp(x, y), "{x:?} against {y:?}");
        assert_eq!(b.cmp(&a), big_cmp(y, x), "{y:?} against {x:?}");
        let fits =
            x.0.checked_mul(y.1 as i128)
                .and(y.0.checked_mul(x.1 as i128));
        wide += usize::from(x.1 != y.1 && fits.is_none());
    }
    // The rows past 128 bits reach the wide comparison.
    assert!(wide >= 9, "{wide} rows compare in 256 bits");
    assert!(Exact::new(1, 0).is_none());
    assert!(Exact::new(1, i128::MAX as u128 + 1).is_none());
    assert_eq!(Exact::new(-7, 2).unwrap().floor(), -4);
}

#[test]
fn analog_samples_keep_their_places_and_breaks() {
    let file = common::fixture("phased.tickwave");
    let index = Index::of_file(&file).unwrap();
    let footer = &index.footer;
    let r = view(&file);
    let length = r.length();
    for c in 0..2 {
        let expected: Vec<(u64, i64)> = (0..20)
            .map(|i| (i, footer.sample_axis_tick(c, i).unwrap()))
            .collect();
        let values: Vec<_> = index
            .channel_blocks(&file, c)
            .unwrap()
            .into_iter()
            .flat_map(|b| match b.data {
                BlockChannel::Analog { values, .. } => values.iter().collect::<Vec<_>>(),
                BlockChannel::Digital { .. } => vec![],
            })
            .collect();
        let mut cur = AnalogCursor::seek(&r, c, 0, &NEVER).unwrap();
        let mut items = vec![];
        while cur
            .next(&r, length, &budget(3), &NEVER, &mut items)
            .unwrap()
            .reached
            < length
        {}
        let got: Vec<(u64, i64)> = samples(&items).iter().map(|s| (s.index, s.axis)).collect();
        assert_eq!(got, expected, "channel {c}");
        let got_values: Vec<_> = samples(&items).iter().map(|s| s.value).collect();
        assert_eq!(got_values, values);
        let breaks: Vec<(u64, u64)> = items
            .iter()
            .filter_map(|s| match s {
                Seen::Break { index, lost, .. } => Some((*index, *lost)),
                _ => None,
            })
            .collect();
        let header: Vec<(u64, u64)> = footer
            .breaks(c)
            .unwrap_or(&[])
            .iter()
            .map(|b| (b.index, b.lost))
            .collect();
        assert_eq!(breaks, header, "channel {c}");
        // Seeking lands on the first sample at or after the tick.
        for t in 0..length {
            let mut s = AnalogCursor::seek(&r, c, t, &NEVER).unwrap();
            let mut rest = vec![];
            s.next(&r, length, &Budget::LARGE, &NEVER, &mut rest)
                .unwrap();
            assert_eq!(
                samples(&rest).first().map(|s| s.index),
                expected.iter().find(|e| e.1 >= t).map(|e| e.0),
                "channel {c} at {t}"
            );
            assert!(rest.iter().all(|s| s.axis() >= t), "channel {c} at {t}");
        }
    }
}

/// A file in memory that records each read.
struct Counted {
    bytes: Vec<u8>,
    reads: Mutex<Vec<Range<u64>>>,
}

impl Bytes for Counted {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.bytes.len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.reads
            .lock()
            .unwrap()
            .push(offset..offset + buf.len() as u64);
        let at = offset as usize;
        buf.copy_from_slice(&self.bytes[at..at + buf.len()]);
        Ok(())
    }
}

impl Counted {
    fn new(file: &[u8]) -> Arc<Self> {
        Arc::new(Self {
            bytes: file.to_vec(),
            reads: Mutex::default(),
        })
    }

    fn take(&self) -> Vec<Range<u64>> {
        std::mem::take(&mut *self.reads.lock().unwrap())
    }
}

/// Every index, list and directory page of the file, each its record's bytes: from the top index
/// down, read as the format crate reads them.
fn pages(file: &[u8]) -> Vec<Range<u64>> {
    let footer = Footer::parse_file(file).unwrap();
    let mut out = Vec::new();
    let span = |e: &Entry| e.offset..e.offset + u64::from(e.length);
    for ch in &footer.channels {
        for table in ch.blocks.iter().chain(&ch.list) {
            for depth in 0..table.levels.len() as u8 {
                for e in table.pages(depth, &mut from_memory(file)).unwrap() {
                    out.push(span(&e));
                }
            }
            table
                .visit(&mut from_memory(file), |at, _| {
                    out.push(span(&at.entry));
                    Ok::<_, DecodeError>(ControlFlow::Continue(()))
                })
                .unwrap();
        }
    }
    out.sort_by_key(|p| p.start);
    out.dedup();
    out
}

/// The reads of `reads` that are pages, and those that are not, which are block frames.
fn split(pages: &[Range<u64>], reads: &[Range<u64>]) -> (usize, usize) {
    let page = |r: &Range<u64>| pages.iter().any(|p| p == r);
    let n = reads.iter().filter(|r| page(r)).count();
    (n, reads.len() - n)
}

/// A channel of `n` one-edge blocks: past 4,096 × 16, its index has a directory above its pages.
fn blocks(n: i64) -> (Vec<u8>, Vec<i64>) {
    let edges: Vec<i64> = (0..n).map(|k| 10 + 4 * k + (k % 3)).collect();
    let end = edges.last().unwrap() + 10;
    let file = encoded_with(vec![channel(0, edges.clone(), end)], vec![source()], end, 1);
    (file, edges)
}

fn many_blocks() -> (Vec<u8>, Vec<i64>) {
    blocks(70_000)
}

#[test]
fn a_cursor_reads_only_the_index_pages_and_blocks_a_window_needs() {
    let (file, expected) = many_blocks();
    let footer = Footer::parse_file(&file).unwrap();
    let table = footer.channels[0].blocks.clone().unwrap();
    assert!(table.levels.len() > 1, "no directory");
    let r = view(&file);
    let length = r.length();
    let whole = edges(&all(&r, 0, 0, length, &Budget::LARGE, length));
    assert_eq!(whole.iter().map(|e| e.axis).collect::<Vec<_>>(), expected);
    // A window in the middle, cold: the directory, one index page and the blocks it covers.
    let all_pages = pages(&file);
    let bytes = Counted::new(&file);
    let cold = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    bytes.take();
    let mid = expected[35_000];
    let window = edges(&all(&cold, 0, mid, mid + 200, &Budget::LARGE, 200));
    let inside = expected
        .iter()
        .filter(|&&e| (mid..mid + 200).contains(&e))
        .count();
    assert_eq!(window.len(), inside);
    assert_eq!(window[0].ordinal, 35_000);
    let (index_reads, frames) = split(&all_pages, &bytes.take());
    assert!(index_reads <= 3, "{index_reads} index reads");
    assert!(
        frames <= inside + 2,
        "{frames} blocks read for {inside} edges"
    );
    // Warm: the same window again reads nothing.
    let again = edges(&all(&cold, 0, mid, mid + 200, &Budget::LARGE, 200));
    assert_eq!(again, window);
    assert_eq!(bytes.take(), []);
}

/// Raises `cancel` once `after` reads have completed.
struct Cancelling {
    bytes: Vec<u8>,
    after: AtomicU64,
    cancel: Arc<AtomicBool>,
}

impl Bytes for Cancelling {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.bytes.len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if self.after.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.cancel.store(true, Ordering::Relaxed);
        }
        let at = offset as usize;
        buf.copy_from_slice(&self.bytes[at..at + buf.len()]);
        Ok(())
    }
}

#[test]
fn a_refused_or_cancelled_batch_leaves_the_cursor_where_it_was() {
    // With no cache each block's step reads its index page again: few blocks.
    let (file, expected) = blocks(1500);
    let cancel = Arc::new(AtomicBool::new(false));
    let bytes = Arc::new(Cancelling {
        bytes: file,
        after: AtomicU64::new(u64::MAX),
        cancel: cancel.clone(),
    });
    let cache = Cache::new(0);
    let r = Recording::open(bytes.clone(), cache.clone()).unwrap();
    let length = r.length();
    let mut cur = DigitalCursor::seek(&r, 0, 0, &Budget::LARGE, &cancel).unwrap();
    let mut got = vec![];
    cur.next(&r, 2000, &Budget::LARGE, &cancel, &mut got)
        .unwrap();
    assert!(got.len() > 400);
    let (at, held) = (cur.next_ordinal(), got.clone());
    // A block larger than the budget: refused, and neither the cursor nor the list has moved.
    let small = Budget {
        items: usize::MAX,
        block_bytes: 0,
    };
    match cur.next(&r, length, &small, &cancel, &mut got) {
        Err(Error::NeedsBudget { block_bytes, .. }) => assert!(block_bytes > 0),
        other => panic!("{other:?}"),
    }
    assert_eq!((cur.next_ordinal(), &got), (at, &held));
    // Cancelled after a few reads have landed: also not moved.
    bytes.after.store(3, Ordering::Relaxed);
    assert!(matches!(
        cur.next(&r, length, &Budget::LARGE, &cancel, &mut got),
        Err(Error::Cancelled)
    ));
    assert_eq!((cur.next_ordinal(), &got), (at, &held));
    cancel.store(false, Ordering::Relaxed);
    bytes.after.store(u64::MAX, Ordering::Relaxed);
    cur.next(&r, length, &Budget::LARGE, &cancel, &mut got)
        .unwrap();
    assert_eq!(
        edges(&got).iter().map(|e| e.axis).collect::<Vec<_>>(),
        expected
    );
    // With no cache the cursor still holds its current block, and nothing more is kept.
    assert_eq!(cache.bytes(), 0);
}

/// A digital channel with 40,000 gaps, ten list pages: an edge, then a gap, every 100 ticks.
fn many_gaps() -> Vec<u8> {
    let n = 40_000i64;
    let edges: Vec<i64> = (0..n).map(|k| 100 * k + 10).collect();
    let gaps = (0..n)
        .map(|k| Gap {
            from: 100 * k + 50,
            to: 100 * k + 60,
            level: k % 3 == 0,
        })
        .collect();
    let end = 100 * n + 10;
    encoded(vec![named("D0", 0, edges, end, gaps)], vec![source()], end)
}

#[test]
fn gaps_in_list_pages_read_only_the_pages_a_window_needs() {
    let file = many_gaps();
    let footer = Footer::parse_file(&file).unwrap();
    let table = footer.channels[0].list.clone().expect("a list in pages");
    let list_pages = table.rows.div_ceil(4096);
    assert!(list_pages >= 10, "{list_pages} list pages");
    // The cursor first, on a recording that hasn't read the list whole.
    let paged = view(&file);
    let length = paged.length();
    let whole = all(&paged, 0, 0, length, &Budget::LARGE, length);
    let r = view(&file);
    let (_, expected) = r.edges(0, 0, length, &NEVER).unwrap();
    assert_eq!(
        edges(&whole).iter().map(|e| e.axis).collect::<Vec<_>>(),
        expected
    );
    let spans = known_spans(&r.known(0).unwrap(), 0, length);
    assert_eq!(known(&whole, length), spans);
    // Batches of 7 observations, windows of 1,234 ticks: the same, from pages and from the
    // list read whole.
    assert_eq!(all(&paged, 0, 0, length, &budget(7), 1234), whole);
    assert_eq!(all(&r, 0, 0, length, &budget(7), 1234), whole);
    // Seeking agrees with the view, at gap boundaries and between them, from pages and whole.
    for rec in [&view(&file), &r] {
        for k in (0..40_000i64).step_by(997) {
            for t in [
                100 * k + 10,
                100 * k + 49,
                100 * k + 50,
                100 * k + 55,
                100 * k + 60,
                100 * k + 61,
            ] {
                let mut cur = DigitalCursor::seek(rec, 0, t, &Budget::LARGE, &NEVER).unwrap();
                let level = cur.level();
                let rest: Vec<Seen> = whole
                    .iter()
                    .filter(|s| s.axis() >= t)
                    .take(6)
                    .copied()
                    .collect();
                let mut got = vec![];
                cur.next(rec, length, &budget(6), &NEVER, &mut got).unwrap();
                assert_eq!(got, rest, "at {t}");
                if t % 100 != 50 && t % 100 != 60 && t % 100 != 10 {
                    let expected = spans
                        .iter()
                        .any(|s| s.contains(&t))
                        .then(|| r.edges(0, t, t + 1, &NEVER).unwrap().0);
                    assert_eq!(level, expected, "level at {t}");
                }
            }
        }
    }
    // A cold seek in the middle and 1,000 ticks read one or two list pages, not ten.
    let all_pages = pages(&file);
    let bytes = Counted::new(&file);
    let cold = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    bytes.take();
    let t = 100 * 20_000 + 55;
    let mut cur = DigitalCursor::seek(&cold, 0, t, &Budget::LARGE, &NEVER).unwrap();
    let mut got = vec![];
    cur.next(&cold, t + 1000, &Budget::LARGE, &NEVER, &mut got)
        .unwrap();
    assert_eq!(edges(&got).len(), 10);
    let (index_reads, _) = split(&all_pages, &bytes.take());
    assert!(index_reads <= 4, "{index_reads} index and list pages");
    // Past those pages the cursor reads the next, one at a time: a list page holds 409,600
    // ticks of gaps, so 500,000 ticks more cross one.
    cur.next(&cold, t + 500_000, &Budget::LARGE, &NEVER, &mut got)
        .unwrap();
    let (index_reads, _) = split(&all_pages, &bytes.take());
    assert!(
        (1..=3).contains(&index_reads),
        "{index_reads} index and list pages"
    );
}

/// An analog channel of 60,000 samples two ticks apart, a run every three samples, each run ten
/// ticks after the one before: 19,999 breaks, five list pages. Every other break lost a sample;
/// three were lost before the first and two after the last.
fn many_breaks() -> (Vec<u8>, u64) {
    let count = 60_000u64;
    let breaks: Vec<model::Break> = (1..count / 3)
        .map(|k| model::Break::new(3 * k, 10 * k as i64 + 10, k % 2))
        .collect();
    let end = 10 * (count / 3) as i64 + 20;
    let ch = Channel {
        name: "A".into(),
        source_name: "A".into(),
        source: 0,
        data: ChannelData::Analog(Analog {
            unit: "V".into(),
            first: 10,
            phase: None,
            period: Ratio::integer(2).unwrap(),
            count,
            breaks,
            lost_before: 3,
            lost_after: 2,
            significant: None,
            samples: Samples::F64(Box::new((0..count).map(|i| i as f64))),
        }),
        shaping: Shaping {
            kept: Some(model::Kept { from: 0, to: end }),
            ..Shaping::default()
        },
    };
    (encoded(vec![ch], vec![source()], end), count)
}

fn analog_all(rec: &Recording, c: usize, from: i64, to: i64, budget: &Budget) -> Vec<Seen> {
    let mut cur = AnalogCursor::seek(rec, c, from, &NEVER).unwrap();
    let mut out = vec![];
    while cur.next(rec, to, budget, &NEVER, &mut out).unwrap().reached < to {}
    out
}

#[test]
fn breaks_in_list_pages_and_lost_samples_match_the_format() {
    let (file, count) = many_breaks();
    let index = Index::of_file(&file).unwrap();
    let mut footer = index.footer.clone();
    let table = footer.channels[0].list.clone().expect("a list in pages");
    assert!(table.rows.div_ceil(4096) >= 5);
    footer.load_lists(from_memory(&file)).unwrap();
    let paged = view(&file);
    let length = paged.length();
    let seen = analog_all(&paged, 0, 0, length, &budget(5));
    let got: Vec<(u64, i64)> = samples(&seen).iter().map(|x| (x.index, x.axis)).collect();
    let expected: Vec<(u64, i64)> = (0..count)
        .map(|i| (i, footer.sample_axis_tick(0, i).unwrap()))
        .collect();
    assert_eq!(got, expected);
    let breaks = footer.breaks(0).unwrap();
    let got: Vec<(u64, u64)> = seen
        .iter()
        .filter_map(|s| match s {
            Seen::Break { index, lost, .. } => Some((*index, *lost)),
            _ => None,
        })
        .collect();
    assert_eq!(
        got,
        breaks.iter().map(|b| (b.index, b.lost)).collect::<Vec<_>>()
    );
    // Losses: three before the first sample, one at every other break, two after the last; at
    // the places the format gives.
    let grid = footer.grid(0).unwrap();
    let ChannelKind::Analog { first, .. } = footer.header.channels[0].data else {
        unreachable!()
    };
    let mut expected_lost = vec![(
        lost_before_start(grid, first, 3).unwrap(),
        grid.start(grid.first_run(first, 0)),
        3,
    )];
    for k in 0..breaks.len() {
        if let Some(r) = grid.lost_time(first, breaks, k) {
            expected_lost.push((r.start, r.end, breaks[k].lost));
        }
    }
    let last = *breaks.last().unwrap();
    expected_lost.push((
        grid.place(last, count),
        lost_after_end(grid, last, count, 2).unwrap(),
        2,
    ));
    let lost: Vec<(i128, i128, u64)> = seen
        .iter()
        .filter_map(|s| match s {
            Seen::Lost {
                from, to, count, ..
            } => Some((*from, *to, *count)),
            _ => None,
        })
        .collect();
    assert_eq!(lost, expected_lost);
    // Observations come in time order, and every loss before the run start it precedes.
    assert!(seen.windows(2).all(|w| w[0].axis() <= w[1].axis()));
    // The list read whole gives the same.
    let whole = view(&file);
    whole.runs(0).unwrap();
    assert_eq!(analog_all(&whole, 0, 0, length, &budget(5)), seen);
    // Seeking lands on the first observation at or after the tick, across pages and whole.
    for rec in [&view(&file), &whole] {
        for t in (0..length).step_by(7919).chain([0, 9, 10, 11, length - 1]) {
            let rest = analog_all(rec, 0, t, length, &Budget::LARGE);
            let want: Vec<Seen> = seen.iter().filter(|s| s.axis() >= t).copied().collect();
            assert_eq!(rest, want, "at {t}");
        }
    }
    let all_pages = pages(&file);
    let bytes = Counted::new(&file);
    let cold = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    bytes.take();
    let mid = analog_all(&cold, 0, length / 2, length / 2 + 1000, &Budget::LARGE);
    assert_eq!(samples(&mid).len(), 300);
    let (index_reads, _) = split(&all_pages, &bytes.take());
    assert!(index_reads <= 5, "{index_reads} index and list pages");
}

#[test]
fn a_cursors_blocks_come_from_the_views_cache() {
    let (file, expected) = many_blocks();
    let window = expected[30_000]..expected[30_100];
    // The view first, over the window and the tick after it, where the edge lies that ends the
    // cursor's last batch: the cursor reads nothing it read.
    let bytes = Counted::new(&file);
    let r = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    bytes.take();
    let past = window.end + 1;
    let (_, drawn) = r.edges(0, window.start, past, &NEVER).unwrap();
    let mut q = Query::new(window.start..past, (past - window.start) as u32);
    q.channels = vec![0];
    r.query(&q, &NEVER).unwrap();
    let (_, frames) = split(&pages(&file), &bytes.take());
    assert!(frames > 100, "{frames} blocks");
    let read = all(&r, 0, window.start, window.end, &Budget::LARGE, 13);
    assert_eq!(
        edges(&read).iter().map(|e| e.axis).collect::<Vec<_>>(),
        drawn[..drawn.len() - 1]
    );
    assert_eq!(bytes.take(), []);
    // The cursor first: it reads nothing twice, and the view's edges read nothing it read.
    let bytes = Counted::new(&file);
    let r = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    bytes.take();
    all(&r, 0, window.start, window.end, &Budget::LARGE, 13);
    let reads = bytes.take();
    let mut once = reads.clone();
    once.sort_by_key(|r| r.start);
    once.dedup();
    assert_eq!(once.len(), reads.len(), "a page or block read twice");
    let (_, frames) = split(&pages(&file), &reads);
    assert!(frames > 100, "{frames} blocks");
    r.edges(0, window.start, window.end, &NEVER).unwrap();
    assert_eq!(bytes.take(), []);
    r.query(&q, &NEVER).unwrap();
    let after = bytes.take();
    assert!(
        after.iter().all(|a| !reads.contains(a)),
        "the view read again what the cursor read"
    );
    // An analog channel's samples alike, after `each_sample`.
    let file = common::fixture("swd50-cut.tickwave");
    let bytes = Counted::new(&file);
    let r = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    r.each_sample(0, 20_000, 20_500, &NEVER, |_, _, _| Ok(()))
        .unwrap();
    let from = r.runs(0).unwrap().tick(20_000);
    let to = r.runs(0).unwrap().tick(20_500);
    bytes.take();
    let got = analog_all(&r, 0, from, to, &Budget::LARGE);
    assert_eq!(samples(&got).len(), 500);
    assert_eq!(samples(&got)[0].index, 20_000);
    assert_eq!(bytes.take(), []);
}

/// A file a writer appends to, read as it is now.
struct Growing(Mutex<Vec<u8>>);

impl Bytes for Growing {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.0.lock().unwrap().len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let file = self.0.lock().unwrap();
        let at = offset as usize;
        buf.copy_from_slice(&file[at..at + buf.len()]);
        Ok(())
    }
}

/// What a recorder pushes, in order: D's edges and gaps, A's runs and samples.
#[derive(Clone, Debug)]
enum Op {
    Edge(i64),
    Gap(Gap),
    Run(i64, u64),
    Sample(u64),
    Flush(i64),
}

/// D and A on one source at a microsecond: a pulse at D's start, a gap reported after the
/// flush past its start, A's first run with samples lost before it, a run after a loss and one
/// after a pause.
fn session() -> (Declaration, Vec<Op>) {
    let timebase = Ratio::integer(1_000_000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![
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
            DeclaredChannel {
                name: "A".into(),
                source_name: "a".into(),
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
            },
        ],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("cursor".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
        requires: Vec::new(),
    };
    let mut ops = vec![Op::Edge(0), Op::Edge(0), Op::Run(30, 3)];
    let mut v = 0;
    let mut samples = |ops: &mut Vec<Op>, n: u64| {
        for _ in 0..n {
            ops.push(Op::Sample(3300 + v % 7));
            v += 1;
        }
    };
    samples(&mut ops, 4);
    ops.push(Op::Edge(65));
    samples(&mut ops, 5);
    ops.push(Op::Flush(125));
    ops.push(Op::Gap(Gap {
        from: 120,
        to: 160,
        level: true,
    }));
    ops.push(Op::Run(200, 4));
    samples(&mut ops, 5);
    ops.push(Op::Edge(245));
    ops.push(Op::Edge(245));
    samples(&mut ops, 4);
    ops.push(Op::Flush(290));
    ops.push(Op::Run(400, 0));
    for k in 0..40 {
        samples(&mut ops, 1);
        if k % 3 == 0 {
            ops.push(Op::Edge(405 + 10 * k));
        }
    }
    ops.push(Op::Edge(900));
    (d, ops)
}

fn apply(w: &mut Writer, op: &Op) {
    match *op {
        Op::Edge(t) => w.edge(0, t).unwrap(),
        Op::Gap(g) => w.gap(0, g).unwrap(),
        Op::Run(t, lost) => w.run(1, t, lost).unwrap(),
        Op::Sample(v) => w.sample(1, v).unwrap(),
        Op::Flush(t) => w.flush(t).unwrap(),
    }
}

/// Port `p`'s cursor read on to where its recording is settled.
fn settle(rec: &Recording, p: &mut PortCursor, out: &mut Vec<Seen>) -> i64 {
    loop {
        let b = p
            .next_in(rec, Until::Axis(i64::MAX), &budget(3), &NEVER, out)
            .unwrap();
        if b.taken == 0 {
            return b.reached;
        }
    }
}

fn port(rec: &Recording, c: usize) -> PortCursor {
    if c == 0 {
        PortCursor::Digital(DigitalCursor::seek(rec, c, 0, &budget(3), &NEVER).unwrap())
    } else {
        PortCursor::Analog(AnalogCursor::seek(rec, c, 0, &NEVER).unwrap())
    }
}

/// Groups flat: each observation with its port and its group's time.
type Flat = Vec<(Exact, usize, Seen)>;

/// Every group of `m` before `until`, flat.
fn merged(rec: &Recording, m: &mut Merge, until: i64) -> (Flat, i64) {
    let mut groups = Groups::new();
    let reached = loop {
        let b = m.next(rec, until, &budget(2), &NEVER, &mut groups).unwrap();
        if b.taken == 0 {
            break b.reached;
        }
    };
    let flat = groups
        .iter()
        .flat_map(|g| g.seen.iter().map(move |&(p, s)| (g.at, p, s)))
        .collect();
    (flat, reached)
}

#[test]
fn a_live_recording_reads_as_the_same_writes_finished_up_to_where_it_is_settled() {
    let (d, ops) = session();
    let mut compared = 0;
    for (block_bytes, compression) in [
        (64, Compression::Spread),
        (64, Compression::Offload),
        (1 << 20, Compression::Spread),
    ] {
        let options = WriteOptions {
            level: 3,
            block_bytes,
            compression,
            promise_with_items: true,
        };
        let mut w = Writer::new(d.clone(), options.clone()).unwrap();
        let file = Arc::new(Growing(Mutex::new(w.take_output())));
        let mut updated: Option<Recording> = None;
        // One cursor of each channel and one merge of both, carried across every update.
        let mut carried: Vec<(PortCursor, Vec<Seen>)> = Vec::new();
        let mut merge: Option<(Merge, Flat)> = None;
        let mut reach = [i64::MIN; 3];
        for n in 0..ops.len() {
            let at = format!("{block_bytes} {compression:?}, after op {n} ({:?})", ops[n]);
            apply(&mut w, &ops[n]);
            let snapshot = w.snapshot();
            file.0.lock().unwrap().extend(w.take_output());
            let fresh =
                Recording::live(snapshot.clone(), file.clone(), Cache::new(1 << 20)).unwrap();
            match &mut updated {
                None => {
                    updated =
                        Some(Recording::live(snapshot, file.clone(), Cache::new(1 << 20)).unwrap())
                }
                Some(u) => u.update(snapshot).unwrap(),
            }
            let u = updated.as_ref().unwrap();
            if carried.is_empty() {
                carried = (0..2).map(|c| (port(u, c), Vec::new())).collect();
                let ports = carried.iter().map(|(p, _)| p.clone()).collect();
                merge = Some((Merge::with_ports(ports), Vec::new()));
            }
            // The same writes, finished past anything settled.
            let mut f = Writer::new(d.clone(), options.clone()).unwrap();
            let mut finished_file = f.take_output();
            for op in &ops[..=n] {
                apply(&mut f, op);
            }
            finished_file.extend(f.take_output());
            let length = fresh.length() + 1000;
            finished_file.extend(f.finish(length, &[length], Finish::default()).unwrap().tail);
            let finished = view(&finished_file);
            for c in 0..2 {
                let before = reach[c];
                let mut seen = Vec::new();
                let reached = settle(&fresh, &mut port(&fresh, c), &mut seen);
                assert!(
                    reached >= reach[c],
                    "{at}, channel {c}: settled back to {reached}"
                );
                reach[c] = reached;
                let mut want = Vec::new();
                port(&finished, c)
                    .next_in(
                        &finished,
                        Until::Axis(reached),
                        &Budget::LARGE,
                        &NEVER,
                        &mut want,
                    )
                    .unwrap();
                assert_eq!(seen, want, "{at}, channel {c} to {reached}");
                compared += seen.len();
                // The cursor carried across the updates has read the same.
                let (p, acc) = &mut carried[c];
                let had = acc.len();
                assert_eq!(settle(u, p, acc), reached, "{at}, channel {c}");
                assert_eq!(acc, &seen, "{at}, channel {c}");
                // Nothing new comes before where the last snapshot had reached.
                assert!(
                    acc[had..].iter().all(|s| s.axis() >= before),
                    "{at}, channel {c}: {:?} before {before}",
                    &acc[had..]
                );
                // Past where it is settled, a cursor can't start.
                assert!(
                    matches!(
                        if c == 0 {
                            DigitalCursor::seek(&fresh, c, reached + 1, &Budget::LARGE, &NEVER)
                                .map(|_| ())
                        } else {
                            AnalogCursor::seek(&fresh, c, reached + 1, &NEVER).map(|_| ())
                        },
                        Err(Error::Unsettled { settled, .. }) if settled == reached
                    ),
                    "{at}, channel {c}"
                );
            }
            // A live recording's channel has no end yet; the finished file's is where it ends.
            let (live_end, end) = (
                DigitalCursor::seek(&fresh, 0, 0, &Budget::LARGE, &NEVER).unwrap(),
                DigitalCursor::seek(&finished, 0, 0, &Budget::LARGE, &NEVER).unwrap(),
            );
            assert_eq!((live_end.end(), end.end()), (None, Some(length)), "{at}");
            // The merge stops where the earlier channel is settled to, and has the finished
            // file's groups before it.
            let (m, acc) = merge.as_mut().unwrap();
            let (got, reached) = merged(u, m, i64::MAX);
            acc.extend(got);
            assert!(
                reached >= reach[2],
                "{at}: the merge settled back to {reached}"
            );
            reach[2] = reached;
            assert_eq!(reached, reach[0].min(reach[1]), "{at}");
            let mut whole = Merge::with_ports((0..2).map(|c| port(&finished, c)).collect());
            let (want, _) = merged(&finished, &mut whole, reached);
            assert_eq!(acc, &want, "{at}: merged");
        }
        // The last snapshot settles the analog channel's samples and D's edges up to its end.
        assert!(reach[0] >= 900 && reach[1] >= 790, "{reach:?}");
    }
    assert!(compared > 2000, "{compared} observations compared");
}

#[test]
fn whole_line_gives_the_known_stretches_and_their_edges() {
    let file = common::fixture("middle-gap.tickwave");
    let r = view(&file);
    let length = r.length();
    for c in 0..r.channels().len() {
        let lines = whole_line(&r, c, length, &NEVER).unwrap();
        let spans = known_spans(&r.known(c).unwrap(), 0, length);
        assert_eq!(
            lines.iter().map(|s| s.from..s.to).collect::<Vec<_>>(),
            spans,
            "channel {c}"
        );
        for s in &lines {
            let (level, edges) = r.edges(c, s.from, s.to, &NEVER).unwrap();
            assert_eq!((s.level, &s.edges), (level, &edges), "channel {c}");
            for t in edges.iter().flat_map(|&e| [e, e + 1]) {
                if (s.from..s.to).contains(&t) {
                    let (level, _) = r.edges(c, t, t + 1, &NEVER).unwrap();
                    assert_eq!(s.before(t), level, "channel {c} at {t}");
                }
            }
        }
    }
}
