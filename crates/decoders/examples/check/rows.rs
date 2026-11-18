//! Whether an analysis's items fit the rows and lanes its schema declares, for the decoders' tests
//! and the checks on recordings.

use escapement_tickwave_decode::{KindId, Schema, Tick};

/// For each of the schema's rows, top first: the most of its items that overlap at once, and the
/// lanes it has. `spans` are items as (kind, start, end); two that only touch do not overlap.
pub fn most_at_once(
    schema: &Schema,
    spans: impl IntoIterator<Item = (KindId, Tick, Tick)>,
) -> Vec<(usize, usize)> {
    let (_, rows) = schema.layout();
    let mut firsts: Vec<usize> = rows.iter().map(|r| r.start).collect();
    firsts.sort_unstable();
    firsts.dedup();
    let mut events: Vec<Vec<(Tick, i8)>> = vec![vec![]; firsts.len()];
    for (kind, start, end) in spans {
        let Some(r) = rows.get(usize::from(kind.0)) else {
            continue;
        };
        let Ok(i) = firsts.binary_search(&r.start) else {
            continue;
        };
        events[i].push((start, 1));
        events[i].push((end.max(start), -1));
    }
    let lanes = |first: usize| {
        rows.iter()
            .find(|r| r.start == first)
            .map_or(1, |r| r.len())
    };
    (firsts.iter().zip(events))
        .map(|(&first, mut ev)| {
            // An end before a start at the same tick.
            ev.sort_unstable();
            let (mut now, mut most) = (0i64, 0i64);
            for (_, d) in ev {
                now += i64::from(d);
                most = most.max(now);
            }
            (most as usize, lanes(first))
        })
        .collect()
}
