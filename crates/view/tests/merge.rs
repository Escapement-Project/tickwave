//! A merge of several channels' cursors: one exact order whatever the sources' mappings, groups
//! of observations at one exact time, timer queries at exact times, and calls that fail or ask
//! for less than the last leaving the merge as it was.

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::model::{
    self, Channel, ChannelData, Digital, Gap, Ratio, Shaping, Source, SourceFormat,
};
use escapement_tickwave_view::cursor::{
    AnalogCursor, Budget, DigitalCursor, Exact, Group, Groups, Merge, PortCursor, Seen, Until,
};
use escapement_tickwave_view::{Cache, ChannelKind, Error, Recording};

static NEVER: AtomicBool = AtomicBool::new(false);

fn budget(items: usize) -> Budget {
    Budget {
        items,
        block_bytes: u32::MAX,
    }
}

fn view(file: &[u8]) -> Recording {
    Recording::open(Arc::new(file.to_vec()), Cache::new(64 << 20)).unwrap()
}

fn source() -> Source {
    Source::new(
        SourceFormat::Other("merge-test".into()),
        Ratio::integer(1000).unwrap(),
    )
}

fn digital(source: u32, edges: Vec<i64>, end: i64, gaps: Vec<Gap>) -> Channel {
    Channel {
        name: format!("D{source}"),
        source_name: format!("D{source}"),
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

fn encoded(
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

fn two_sources() -> Vec<u8> {
    two_sources_in(128)
}

/// Port 0 on the axis's ticks; port 1 from a source two thirds as long a tick, starting five
/// ticks in, with a gap. Port 1's source tick 9 lands exactly on port 0's edge at 11. Blocks of
/// `block_bytes`.
fn two_sources_in(block_bytes: usize) -> Vec<u8> {
    let mut b = source();
    b.stretch = Ratio::new(2, 3);
    b.origin = 5;
    encoded(
        vec![
            digital(0, vec![6, 11, 20, 31, 40], 50, vec![]),
            digital(
                1,
                vec![3, 9, 10, 14, 30],
                40,
                vec![Gap {
                    from: 16,
                    to: 25,
                    level: true,
                }],
            ),
        ],
        vec![source(), b],
        50,
        block_bytes,
    )
}

fn cursors(r: &Recording) -> Vec<DigitalCursor> {
    (0..2)
        .map(|c| DigitalCursor::seek(r, c, 0, &Budget::LARGE, &NEVER).unwrap())
        .collect()
}

/// Every group before 50, in windows of `step` ticks, a batch of `budget` at a time.
fn merged(r: &Recording, budget: &Budget, step: i64) -> Groups {
    let mut m = Merge::new(cursors(r));
    let mut out = Groups::new();
    let mut until = 0;
    while until < 50 {
        until = (until + step).min(50);
        while m.next(r, until, budget, &NEVER, &mut out).unwrap().reached < until {}
    }
    out
}

fn flat(groups: &Groups) -> Vec<(Exact, usize, Seen)> {
    groups
        .iter()
        .flat_map(|g| g.seen.iter().map(move |&(p, s)| (g.at, p, s)))
        .collect()
}

#[test]
fn sources_with_different_mappings_merge_in_exact_order() {
    let r = view(&two_sources());
    // Each port alone, with its exact times; then every observation sorted by exact time.
    let mut all: Vec<(Exact, usize, Seen)> = vec![];
    for c in 0..2 {
        let mut cur = DigitalCursor::seek(&r, c, 0, &Budget::LARGE, &NEVER).unwrap();
        let mut seen = vec![];
        cur.next(&r, 50, &Budget::LARGE, &NEVER, &mut seen).unwrap();
        for s in seen {
            all.push((cur.exact_tick(s.source_tick()), c, s));
        }
    }
    all.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let whole = merged(&r, &Budget::LARGE, 50);
    assert_eq!(flat(&whole), all);
    // Exactly one coincidence: port 1's source tick 9 at exactly 11, with port 0's edge there.
    let both: Vec<Group> = whole.iter().filter(|g| g.coincident()).collect();
    assert_eq!(both.len(), 1);
    assert_eq!(both[0].at, Exact::whole(11));
    // Port 1's tick 10 also floors to 11, but is later: its own group.
    assert!(
        whole
            .iter()
            .any(|g| g.axis() == 11 && !g.coincident() && g.at > Exact::whole(11))
    );
    for (items, step) in [(1, 50), (2, 7), (3, 1)] {
        assert_eq!(merged(&r, &budget(items), step), whole, "{items} {step}");
    }
}

#[test]
fn timer_queries_sample_before_or_at_an_exact_time() {
    let r = view(&two_sources());
    let cursors = cursors(&r);
    let b = cursors[1].clone();
    let mut m = Merge::new(cursors);
    let at = |m: &mut Merge, t: Exact, at: bool| {
        let mut groups = Groups::new();
        assert!(
            m.sample(&r, t, at, &Budget::LARGE, &NEVER, &mut groups)
                .unwrap()
        );
        (groups.len(), m.level(1))
    };
    // Before and at port 1's edge at its source tick 9: the level either side of it.
    let t = b.exact_tick(9);
    assert_eq!(at(&mut m, t, false).1, Some(true));
    assert_eq!(at(&mut m, t, true), (1, Some(false)));
    // Asking again at the same time returns nothing new.
    assert_eq!(at(&mut m, t, true), (0, Some(false)));
    // Inside the gap the level is not known; past its end it is the gap's.
    assert_eq!(at(&mut m, b.exact_tick(20), true).1, None);
    assert_eq!(at(&mut m, b.exact_tick(26), true).1, Some(true));
    // A place between source ticks: 29.5 is before the edge at 30.
    assert_eq!(
        at(&mut m, b.exact_place(59, 2).unwrap(), true).1,
        Some(true)
    );
    assert_eq!(
        at(&mut m, b.exact_place(61, 2).unwrap(), true).1,
        Some(false)
    );
    // Past the channel's end nothing is known.
    assert_eq!(at(&mut m, b.exact_tick(41), true).1, None);
}

/// 8N1 frames at 115200 baud on a 1 MHz source stretched by 1001/1000: a bit lasts 625/72
/// source ticks. The line changes at the first source tick at or after each bit's boundary.
fn uart(bytes: &[u8]) -> Vec<u8> {
    let bit = (625i128, 72i128);
    let mut edges = vec![];
    let mut level = true;
    let mut k = 0i128;
    let start = 100i128;
    for &byte in bytes {
        let mut bits = vec![false];
        bits.extend((0..8).map(|i| byte >> i & 1 == 1));
        bits.push(true);
        for b in bits {
            if b != level {
                let num = start * bit.1 + k * bit.0;
                edges.push(((num + bit.1 - 1) / bit.1) as i64);
                level = b;
            }
            k += 1;
        }
    }
    let end = edges.last().unwrap() + 100;
    let mut s = source();
    s.timebase = Ratio::integer(1_000_000).unwrap();
    s.stretch = Ratio::new(1001, 1000);
    let mut ch = digital(0, edges, end, vec![]);
    if let ChannelData::Digital(d) = &mut ch.data {
        d.initial = true;
    }
    let mut options = Options::default();
    options.level = 1;
    encode(
        model::Recording {
            timebase: Ratio::integer(1_000_000).unwrap(),
            length: end * 2,
            channels: vec![ch],
            derived: vec![],
            sources: vec![s],
        },
        &options,
    )
    .unwrap()
}

/// Decodes 8N1 frames: from each start edge, samples each bit's centre either at an exact
/// offset from the edge, or by adding a whole-tick bit time to the last sample.
fn decode_uart(r: &Recording, exact: bool) -> Vec<u8> {
    let cur = DigitalCursor::seek(r, 0, 0, &Budget::LARGE, &NEVER).unwrap();
    let mut m = Merge::new(vec![cur.clone()]);
    let length = r.length();
    let mut out = vec![];
    let mut groups = Groups::new();
    loop {
        // The next edge, a group at a time.
        groups.clear();
        m.next(r, length, &budget(1), &NEVER, &mut groups).unwrap();
        let Some(g) = groups.get(0) else {
            return out;
        };
        let (_, Seen::Edge(e)) = g.seen[0] else {
            continue;
        };
        if e.level {
            continue;
        }
        let mut byte = 0u8;
        for i in 0..10i128 {
            // The centre of bit i: (i + 1/2) × 625/72 source ticks after the edge.
            let t = if exact {
                cur.exact_place(i128::from(e.source_tick) * 144 + (2 * i + 1) * 625, 144)
                    .unwrap()
            } else {
                cur.exact_tick(e.source_tick + 4 + 8 * i as i64)
            };
            let mut skipped = Groups::new();
            m.sample(r, t, true, &Budget::LARGE, &NEVER, &mut skipped)
                .unwrap();
            let level = m.level(0).unwrap();
            if (1..9).contains(&i) && level {
                byte |= 1 << (i - 1);
            }
        }
        out.push(byte);
    }
}

#[test]
fn a_timer_at_exact_offsets_decodes_what_whole_tick_steps_lose() {
    let sent: Vec<u8> = (0..=255).collect();
    let r = view(&uart(&sent));
    assert!(!r.mapping(0).unwrap().is_identity());
    assert_eq!(decode_uart(&r, true), sent);
    let stepped = decode_uart(&r, false);
    let wrong = sent.iter().zip(&stepped).filter(|(a, b)| a != b).count();
    assert!(wrong > 0, "whole-tick steps decoded every byte");
}

#[test]
fn analog_and_digital_ports_merge_in_exact_order() {
    for (name, ports) in [
        ("phased.tickwave", vec!["A", "B", "D"]),
        (
            "energy.tickwave",
            vec!["V", "I", "range", "under_range", "DIO0"],
        ),
    ] {
        let file = common::fixture(name);
        let r = view(&file);
        let length = r.length();
        let mut cursors = vec![];
        let mut all: Vec<(Exact, usize, Seen)> = vec![];
        for (p, port) in ports.iter().enumerate() {
            let c = r.channel(port).unwrap();
            let cursor = match r.channels()[c].kind {
                ChannelKind::Digital => PortCursor::Digital(
                    DigitalCursor::seek(&r, c, 0, &Budget::LARGE, &NEVER).unwrap(),
                ),
                _ => PortCursor::Analog(AnalogCursor::seek(&r, c, 0, &NEVER).unwrap()),
            };
            let mut items = vec![];
            cursor
                .clone()
                .next_in(&r, Until::Axis(length), &Budget::LARGE, &NEVER, &mut items)
                .unwrap();
            for s in items {
                all.push((cursor.exact(&s), p, s));
            }
            cursors.push(cursor);
        }
        all.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        for (items, step) in [(usize::MAX, length), (3, length / 7 + 1)] {
            let mut m = Merge::with_ports(cursors.clone());
            let mut out = Groups::new();
            let mut until = 0;
            while until < length {
                until = (until + step).min(length);
                while m
                    .next(&r, until, &budget(items), &NEVER, &mut out)
                    .unwrap()
                    .reached
                    < until
                {}
            }
            assert_eq!(flat(&out).len(), all.len(), "{name}");
            assert!(flat(&out) == all, "{name}: merged order differs");
        }
        let samples = all
            .iter()
            .filter(|x| matches!(x.2, Seen::Sample(_)))
            .count();
        let edges = all.iter().filter(|x| matches!(x.2, Seen::Edge(_))).count();
        assert!(samples > 0 && edges > 0, "{name}");
    }
}

#[test]
fn a_merge_asked_for_less_than_it_read_returns_nothing_past_it() {
    let r = view(&two_sources());
    let mut m = Merge::new(cursors(&r));
    let mut out = Groups::new();
    // One group to 50: the ports read ahead to 50.
    m.next(&r, 50, &budget(1), &NEVER, &mut out).unwrap();
    assert_eq!(out.len(), 1);
    // To 10: nothing at or past it, though the ports hold observations there.
    let b = m.next(&r, 10, &Budget::LARGE, &NEVER, &mut out).unwrap();
    assert_eq!(b.reached, 10);
    assert!(out.iter().all(|g| g.at < Exact::whole(10)), "{out:?}");
    // The rest comes out in order after.
    while m
        .next(&r, 50, &Budget::LARGE, &NEVER, &mut out)
        .unwrap()
        .reached
        < 50
    {}
    assert_eq!(out, merged(&r, &Budget::LARGE, 50));
}

#[test]
fn a_timeline_bound_needs_every_port_on_one_source() {
    let r = view(&two_sources());
    let mut m = Merge::new(cursors(&r));
    let mut out = Groups::new();
    assert!(matches!(
        m.next_in(&r, Until::Source(20), &Budget::LARGE, &NEVER, &mut out),
        Err(Error::Timeline)
    ));
    assert!(out.is_empty());
    // One port alone shares its own source: its timeline groups end before the bound.
    let mut one = Merge::new(vec![cursors(&r).remove(1)]);
    let b = one
        .next_in(&r, Until::Source(20), &Budget::LARGE, &NEVER, &mut out)
        .unwrap();
    assert_eq!(b.reached, 20);
    assert!(out.iter().all(|g| one.timeline(&g).unwrap() < 20));
    assert_eq!(out.len(), 6);
}

#[test]
fn a_cancelled_or_refused_merge_stays_where_it_was() {
    // An edge a block, so a call past the first has blocks to decode.
    let r = view(&two_sources_in(1));
    let mut m = Merge::new(cursors(&r));
    let mut out = Groups::new();
    m.next(&r, 12, &Budget::LARGE, &NEVER, &mut out).unwrap();
    let before = out.clone();
    let cancel = AtomicBool::new(true);
    assert!(matches!(
        m.next(&r, 50, &Budget::LARGE, &cancel, &mut out),
        Err(Error::Cancelled)
    ));
    let small = Budget {
        items: usize::MAX,
        block_bytes: 0,
    };
    assert!(matches!(
        m.next(&r, 50, &small, &NEVER, &mut out),
        Err(Error::NeedsBudget { .. })
    ));
    assert_eq!(out, before);
    while m
        .next(&r, 50, &Budget::LARGE, &NEVER, &mut out)
        .unwrap()
        .reached
        < 50
    {}
    assert_eq!(out, merged(&r, &Budget::LARGE, 50));
}

/// Raises `cancel` once `after` reads have completed.
struct Cancelling {
    bytes: Vec<u8>,
    after: std::sync::atomic::AtomicU64,
    cancel: Arc<AtomicBool>,
}

impl escapement_tickwave_view::Bytes for Cancelling {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.bytes.len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        use std::sync::atomic::Ordering;
        if self.after.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.cancel.store(true, Ordering::Relaxed);
        }
        let at = offset as usize;
        buf.copy_from_slice(&self.bytes[at..at + buf.len()]);
        Ok(())
    }
}

#[test]
fn a_merge_cancelled_after_groups_went_out_takes_them_back() {
    use std::sync::atomic::Ordering;
    // An edge a block, more than a port reads at once: a call reads again after its first
    // groups.
    let edges: Vec<i64> = (0..10_000).map(|k| 10 + 3 * k).collect();
    let file = encoded(
        vec![
            digital(0, edges, 30_100, vec![]),
            Channel {
                name: "E".into(),
                source_name: "E".into(),
                ..digital(0, vec![5, 15_000], 30_100, vec![])
            },
        ],
        vec![source()],
        30_100,
        1,
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let bytes = Arc::new(Cancelling {
        bytes: file.clone(),
        after: u64::MAX.into(),
        cancel: cancel.clone(),
    });
    let r = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    let ports = cursors(&r);
    let mut m = Merge::new(ports.clone());
    let mut out = Groups::new();
    bytes.after.store(6000, Ordering::Relaxed);
    assert!(matches!(
        m.next(&r, 30_100, &Budget::LARGE, &cancel, &mut out),
        Err(Error::Cancelled)
    ));
    assert!(out.is_empty(), "{} groups kept", out.len());
    cancel.store(false, Ordering::Relaxed);
    bytes.after.store(u64::MAX, Ordering::Relaxed);
    while m
        .next(&r, 30_100, &Budget::LARGE, &cancel, &mut out)
        .unwrap()
        .reached
        < 30_100
    {}
    let mut fresh = Merge::new(ports);
    let mut want = Groups::new();
    while fresh
        .next(&r, 30_100, &Budget::LARGE, &NEVER, &mut want)
        .unwrap()
        .reached
        < 30_100
    {}
    assert_eq!(out, want);
    assert!(out.len() > 10_000);
}
