//! The view's `Recording::open` on any file, and on a file that opens: queries, an extend, the
//! statistics, the markers, and the readers' helpers, at spans and columns the input picks.
#![no_main]

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use arbitrary::{Arbitrary, Unstructured};
use escapement_tickwave_view::{
    Cache, MarkerCursor, MarkerQuery, Query, Recording, probe, readouts, runs, spans,
};
use libfuzzer_sys::fuzz_target;

/// Where the input puts a tick: a fraction of the recording, or any tick.
#[derive(Arbitrary, Debug, Clone, Copy)]
enum At {
    Part(u16),
    Any(i64),
}

impl At {
    fn tick(self, length: i64) -> i64 {
        match self {
            At::Part(p) => ((i128::from(length) * i128::from(p)) >> 15) as i64 - length / 4,
            At::Any(t) => t,
        }
    }
}

#[derive(Arbitrary, Debug)]
struct Asks {
    from: At,
    to: At,
    columns: u16,
    aligned: bool,
    strip: Option<(u8, u16)>,
    flag: Option<(u8, u8)>,
    limit: u8,
    moved: u16,
    stats: (At, At),
    index: u64,
}

fuzz_target!(|data: &[u8]| {
    let (asks, file) = data.split_at(data.len().min(48));
    let file = escapement_tickwave_fuzz::file(file);
    let Ok(rec) = Recording::open(Arc::new(file), Cache::new(1 << 20)) else {
        return;
    };
    let Ok(a) = Asks::arbitrary(&mut Unstructured::new(asks)) else {
        return;
    };
    let cancel = AtomicBool::new(false);
    let length = rec.length();
    let n = rec.channels().len();
    let (from, to) = (a.from.tick(length), a.to.tick(length));
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "one interval to measure, not its ticks"
    )]
    let statistics = vec![a.stats.0.tick(length)..a.stats.1.tick(length)];
    let q = Query {
        window: from..to,
        columns: u32::from(a.columns % 2048),
        channels: (0..n).collect(),
        flags: a
            .flag
            .map(|(x, y)| vec![(usize::from(x) % n.max(1), usize::from(y) % n.max(1))])
            .unwrap_or_default(),
        strip: a
            .strip
            .map(|(c, w)| (usize::from(c) % n.max(1), u32::from(w % 1024))),
        statistics,
        markers: MarkerQuery {
            limit: usize::from(a.limit),
            list: Some((None, usize::from(a.limit))),
            successor_of: Some(MarkerCursor {
                tick: from,
                arrival: 0,
            }),
        },
        aligned: a.aligned,
    };
    if let Ok(answer) = rec.query(&q, &cancel) {
        let width = i128::from(to.saturating_sub(from).max(1));
        let shift = (width * i128::from(a.moved) / 65536) as i64;
        let later = Query {
            window: from.saturating_add(shift)..to.saturating_add(shift),
            ..q.clone()
        };
        let _ = rec.extend(&answer, &q, &later, &cancel);
    }
    let _ = rec.statistics(i64::MIN..i64::MAX, &cancel);
    let _ = rec.markers(i64::MIN..i64::MAX, None, usize::MAX);
    let step = rec.mapping(0).map(|m| m.m()).unwrap_or(rec.timebase());
    let _ = readouts::format_interval(to.saturating_sub(from), rec.timebase(), step);
    let _ = readouts::format_brief(to.saturating_sub(from), rec.timebase(), step);
    let _ = readouts::format_rate(3, to.saturating_sub(from), rec.timebase(), 1, step);
    for c in 0..=n {
        let _ = rec.edges(c, from, to, &cancel);
        let _ = rec.each_sample(
            c,
            a.index,
            a.index.saturating_add(4096),
            &cancel,
            |_, _, _| Ok(()),
        );
        let _ = rec.index_at(c, from);
        let _ = probe::find_edge(&rec, c, from, to, true, &cancel);
        let _ = probe::find_edge(&rec, c, from, to, false, &cancel);
        if let Ok(known) = rec.known(c) {
            let _ = spans::known_spans(&known, from, to);
        }
        if let Ok(r) = rec.runs(c) {
            let _ = runs::lost_spans(&r);
            let _ = runs::sample_at_or_before(&r, length, from as f64);
            let _ = runs::index_at_or_after(&r, to as f64);
            let _ = r.support(a.index, length);
        }
        let _ = (rec.lost(c), rec.gaps(c), rec.mapping(c));
    }
});
