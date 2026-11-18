//! An analog channel's cursor.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use escapement_tickwave_format::decode::{BlockChannel, lost_after_end, lost_before_start};
use escapement_tickwave_format::mapping::Mapping;
use escapement_tickwave_format::model::{Break, Grid, Sample};

use super::lists::{self, BreaksAt, More};
use super::{
    Batch, Budget, Exact, Held, Sampled, Seen, Settled, Until, corrupt, place_of, promise_of,
    source_of, walk,
};
use crate::read::check;
use crate::recording::{Error, Place, Recording, Revision, Store};

/// What the recording states of an analog channel now, apart from its breaks. A live
/// recording's count grows, and its first run comes with its first samples.
#[derive(Clone, Copy)]
struct Facts {
    first: i64,
    grid: Grid,
    count: u64,
    lost_before: u64,
    lost_after: u64,
    map: Mapping,
    /// Whether the channel has its first run: a live recording's can be still to come.
    started: bool,
}

fn facts(rec: &Recording, c: usize) -> Result<Facts, Error> {
    let Place::Analog {
        first,
        grid,
        count,
        lost_before,
        lost_after,
        map,
        ..
    } = place_of(rec, c)?
    else {
        return Err(Error::NoChannel(c));
    };
    let started = match &rec.store {
        Store::Finished(_) => true,
        #[cfg(feature = "live")]
        Store::Live(live) => live
            .snapshot
            .channel(c)
            .is_some_and(|ch| ch.first_run.is_some()),
    };
    Ok(Facts {
        first: *first,
        grid: *grid,
        count: *count,
        lost_before: *lost_before,
        lost_after: *lost_after,
        map: *map,
        started,
    })
}

/// A place of `units / d` source ticks on the axis, saturating where it passes an i64.
fn axis_of(map: &Mapping, grid: Grid, units: i128) -> i64 {
    map.axis_place(units, grid.d())
        .unwrap_or(if units < 0 { i64::MIN } else { i64::MAX })
}

/// Where live analog channel `c` is settled to. A writer takes a sample, and the run it starts,
/// at its promise or later, and samples lost before a run from where the next sample would sit
/// on the last run's grid; before the first run, samples lost before it anywhere on the axis.
fn settled(rec: &Recording, c: usize, f: &Facts) -> Result<Option<Settled>, Error> {
    let Some(promise) = promise_of(rec, c)? else {
        return Ok(None);
    };
    if !f.started {
        return Ok(Some(Settled { axis: 0, source: 0 }));
    }
    let last = match rec.places.get(c) {
        Some(Place::Analog { breaks, .. }) => breaks.last().copied(),
        _ => None,
    }
    .unwrap_or(f.grid.first_run(f.first, f.lost_before));
    let lost_from = f.grid.place(last, f.count);
    Ok(Some(Settled {
        axis: axis_of(&f.map, f.grid, lost_from).min(promise),
        source: f
            .grid
            .floor(lost_from)
            .min(f.map.first_at(promise).saturating_sub(1)),
    }))
}

/// The first sample at or after axis tick `t` over `breaks`: the list's from its first where
/// `whole`, else a slice whose first run starts before `t` and, where `more` follows, whose last
/// starts past it (see `List`). Its index, and the run it lies in; None past every sample.
fn search(breaks: &[Break], whole: bool, more: bool, f: &Facts, t: i64) -> (u64, Option<Break>) {
    let first_run = f.grid.first_run(f.first, f.lost_before);
    let axis = |run: Break, i: u64| axis_of(&f.map, f.grid, f.grid.place(run, i));
    // The runs: the first, where the list starts here, then each break's.
    let lead = usize::from(whole);
    let run = |k: usize| match k.checked_sub(lead) {
        None => Some(first_run),
        Some(i) => breaks.get(i).copied(),
    };
    // A run whose next starts before `t` lies wholly before it.
    let q = breaks.partition_point(|b| axis(*b, b.index) < t);
    let mut k = (q + lead).saturating_sub(1);
    while let Some(r) = run(k) {
        let end = match run(k + 1) {
            Some(n) => n.index,
            None if !more => f.count,
            // The slice's last run starts past `t`: its first sample is the one.
            None => r.index.saturating_add(1),
        }
        .min(f.count);
        let (mut lo, mut hi) = (r.index, end);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if axis(r, mid) < t {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo < end {
            return (lo, Some(r));
        }
        k += 1;
    }
    (f.count, None)
}

#[derive(Clone)]
struct AnalogPos {
    next: u64,
    /// The breaks read so far and not yet passed; `brk` is the next.
    breaks: Arc<Vec<Break>>,
    brk: usize,
    /// Where the breaks after those read come from.
    more: More,
    /// The run sample `next` belongs to, unless a break starts at `next`. None on a live
    /// recording whose first run is still to come.
    run: Option<Break>,
    /// Whether the samples lost before the first are still to be reported, where there are
    /// any; those at the next break, where it has any; and those after the last, where there
    /// are any.
    before_first: bool,
    at_break: bool,
    after_last: bool,
    /// The block holding sample `next`, where the cursor holds it.
    held: Option<Held>,
    /// A tick at or before the block holding sample `next`, where a walk to it starts.
    from: i64,
    revision: Revision,
}

/// An analog channel's samples, run starts and losses from a place on, resumable by index. Its
/// breaks come a list page at a time.
#[derive(Clone)]
pub struct AnalogCursor {
    channel: usize,
    source: u32,
    map: Mapping,
    grid: Grid,
    pos: AnalogPos,
}

impl AnalogCursor {
    /// A cursor at the first observation at or after axis tick `t`. On a live recording `t` must
    /// be settled, else `Error::Unsettled`.
    pub fn seek(
        rec: &Recording,
        channel: usize,
        t: i64,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        check(cancel)?;
        let c = channel;
        let f = facts(rec, c)?;
        let t = t.max(0);
        if let Some(s) = settled(rec, c, &f)?
            && t > s.axis
        {
            return Err(Error::Unsettled {
                channel: c,
                at: t,
                settled: s.axis,
            });
        }
        let first_run = f.grid.first_run(f.first, f.lost_before);
        // The sample, its run, and the breaks from the one at or after it.
        let (next, run, breaks, brk, more, whole) = if !f.started {
            (0, None, Vec::new(), 0, More::Entry(0), true)
        } else {
            match lists::breaks_at(rec, c, t, cancel)? {
                BreaksAt::Slice(w) => {
                    let (next, found) = search(&w.list, w.first, w.more != More::Done, &f, t);
                    let brk = w.list.partition_point(|b| b.index < next);
                    let run = match brk.checked_sub(1).and_then(|i| w.list.get(i)) {
                        Some(b) => *b,
                        None if w.first => first_run,
                        // A break starts at `next`, and sets the run before any sample.
                        None => found.unwrap_or(first_run),
                    };
                    (next, Some(run), w.list, brk, w.more, w.first)
                }
                BreaksAt::Whole => lists::whole_breaks(rec, c, |list, live| {
                    let (next, _) = search(list, true, false, &f, t);
                    let brk = list.partition_point(|b| b.index < next);
                    let run = brk
                        .checked_sub(1)
                        .and_then(|i| list.get(i))
                        .copied()
                        .unwrap_or(first_run);
                    let end = brk.saturating_add(lists::WINDOW).min(list.len());
                    let window = list.get(brk..end).unwrap_or_default().to_vec();
                    let more = match live || end < list.len() {
                        true => More::Entry(end as u64),
                        false => More::Done,
                    };
                    (next, Some(run), window, 0, more, true)
                })?,
            }
        };
        let axis = |u: i128| axis_of(&f.map, f.grid, u);
        let before_first = match run {
            None => true,
            Some(_) if next == 0 && whole && f.lost_before > 0 => {
                lost_before_start(f.grid, f.first, f.lost_before).is_some_and(|u| axis(u) >= t)
            }
            Some(_) => false,
        };
        // A seek reports the losses at or after `t`: at the break starting at `next`, and after
        // the last sample.
        let at_break = match (run, breaks.get(brk)) {
            (Some(r), Some(b)) if b.index == next && b.lost > 0 => {
                axis(f.grid.place(r, b.index).min(f.grid.start(*b))) >= t
            }
            _ => true,
        };
        let after_last = match run {
            Some(r) if next >= f.count => axis(f.grid.place(r, f.count)) >= t,
            _ => true,
        };
        let from = match run {
            Some(r) if next < f.count => axis(f.grid.place(r, next)),
            _ => t,
        };
        Ok(Self {
            channel: c,
            source: source_of(rec, c),
            map: f.map,
            grid: f.grid,
            pos: AnalogPos {
                next,
                breaks: Arc::new(breaks),
                brk,
                more,
                run,
                before_first,
                at_break,
                after_last,
                held: None,
                from,
                revision: rec.revision,
            },
        })
    }

    pub fn channel(&self) -> usize {
        self.channel
    }

    /// The channel's source, by its index among the recording's sources.
    pub fn source(&self) -> u32 {
        self.source
    }

    pub fn map(&self) -> Mapping {
        self.map
    }

    pub fn grid(&self) -> Grid {
        self.grid
    }

    /// The index of the next sample.
    pub fn next_index(&self) -> u64 {
        self.pos.next
    }

    /// Sample `s`'s exact axis time.
    pub fn exact(&self, s: &Sampled) -> Exact {
        self.exact_place(s.place)
    }

    /// The exact axis time of a place in `1/d` source ticks, saturated for one no header allows.
    pub fn exact_place(&self, place: i128) -> Exact {
        let d = self.grid.d();
        let den = u128::from(d) * u128::from(self.map.m().den());
        let num =
            self.map
                .axis_time(place, d)
                .unwrap_or(if place < 0 { i128::MIN } else { i128::MAX });
        Exact::new(num, den).unwrap_or(Exact::whole(axis_of(&self.map, self.grid, place)))
    }

    /// Source tick `k` on the source's timeline: moved by the channel's offset.
    pub fn timeline(&self, k: i64) -> i64 {
        k.saturating_add(self.map.offset())
    }

    fn lost(&self, from: i128, to: i128, count: u64) -> Seen {
        Seen::Lost {
            from,
            to,
            source_tick: self.grid.floor(from),
            axis: axis_of(&self.map, self.grid, from),
            to_axis: axis_of(&self.map, self.grid, to),
            count,
        }
    }

    /// Observations before axis tick `until`, at most the budget's, appended to `out`.
    pub fn next(
        &mut self,
        rec: &Recording,
        until: i64,
        budget: &Budget,
        cancel: &AtomicBool,
        out: &mut Vec<Seen>,
    ) -> Result<Batch, Error> {
        self.next_in(rec, Until::Axis(until), budget, cancel, out)
    }

    /// Samples, run starts and losses before `until`, at most the budget's, appended to `out`.
    /// Cancelled or refused, the cursor stays where it was and `out` as it was.
    pub fn next_in(
        &mut self,
        rec: &Recording,
        until: Until,
        budget: &Budget,
        cancel: &AtomicBool,
        out: &mut Vec<Seen>,
    ) -> Result<Batch, Error> {
        let mark = out.len();
        let r = self.run(rec, until, budget, cancel, out);
        if r.is_err() {
            out.truncate(mark);
        }
        r
    }

    fn run(
        &mut self,
        rec: &Recording,
        until: Until,
        budget: &Budget,
        cancel: &AtomicBool,
        out: &mut Vec<Seen>,
    ) -> Result<Batch, Error> {
        let c = self.channel;
        let f = facts(rec, c)?;
        let offset = self.map.offset();
        let limit = until.cap(settled(rec, c, &f)?, offset);
        let grid = self.grid;
        let mut p = self.pos.clone();
        if p.revision != rec.revision {
            if let Some(h) = p.held.take() {
                p.from = h.start;
            }
            p.revision = rec.revision;
        }
        if p.run.is_none() && f.started {
            p.run = Some(grid.first_run(f.first, f.lost_before));
        }
        let mut breaks_done = false;
        let mut taken = 0;
        // Takes `seen` unless it is past `until` or the budget is spent.
        macro_rules! offer {
            ($seen:expr) => {{
                let seen: Seen = $seen;
                let at = until.key(seen.axis(), seen.source_tick(), offset);
                if at >= limit {
                    break limit;
                }
                if taken >= budget.items {
                    break at;
                }
                out.push(seen);
                taken += 1;
            }};
        }
        let reached = loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let Some(run) = p.run else {
                break limit;
            };
            if p.before_first {
                if f.lost_before > 0 {
                    let start = grid.start(run);
                    let from = lost_before_start(grid, f.first, f.lost_before).unwrap_or(start);
                    offer!(self.lost(from, start, f.lost_before));
                }
                p.before_first = false;
                continue;
            }
            if p.brk >= p.breaks.len() && !breaks_done && p.more != More::Done {
                let w = lists::breaks_after(rec, c, p.more, cancel)?;
                if w.list.is_empty() && w.more == p.more {
                    breaks_done = true;
                } else {
                    p.breaks = Arc::new(w.list);
                    p.brk = 0;
                    p.more = w.more;
                }
                continue;
            }
            if let Some(&b) = p.breaks.get(p.brk).filter(|b| b.index == p.next) {
                if b.lost > 0 && p.at_break {
                    // On the grid of the run before, from place `index`, ending at the break's
                    // start if that comes sooner.
                    let end = grid.start(b);
                    let from = grid.place(run, b.index).min(end);
                    let to = grid.place(run, b.index.saturating_add(b.lost)).min(end);
                    offer!(self.lost(from, to, b.lost));
                    p.at_break = false;
                    continue;
                }
                let place = grid.start(b);
                offer!(Seen::Break {
                    index: b.index,
                    place,
                    source_tick: grid.floor(place),
                    axis: axis_of(&self.map, grid, place),
                    lost: b.lost,
                });
                p.run = Some(b);
                p.brk += 1;
                p.at_break = true;
                continue;
            }
            if p.next >= f.count {
                if p.after_last && f.lost_after > 0 {
                    let from = grid.place(run, f.count);
                    let to = lost_after_end(grid, run, f.count, f.lost_after).unwrap_or(from);
                    offer!(self.lost(from, to, f.lost_after));
                    p.after_last = false;
                }
                break limit;
            }
            let place = grid.place(run, p.next);
            let axis = axis_of(&self.map, grid, place);
            let source_tick = grid.floor(place);
            let at = until.key(axis, source_tick, offset);
            if at >= limit {
                break limit;
            }
            if taken >= budget.items {
                break at;
            }
            let value = self.value(rec, &mut p, axis, budget, cancel)?;
            out.push(Seen::Sample(Sampled {
                index: p.next,
                place,
                source_tick,
                axis,
                value,
            }));
            taken += 1;
            p.next += 1;
        };
        self.pos = p;
        Ok(Batch { taken, reached })
    }

    /// Sample `p.next`'s value, moving `p` to its block; `axis` is its axis tick.
    fn value(
        &self,
        rec: &Recording,
        p: &mut AnalogPos,
        axis: i64,
        budget: &Budget,
        cancel: &AtomicBool,
    ) -> Result<Sample, Error> {
        if let Some(v) = p.held.as_ref().and_then(|h| value_in(h, p.next)) {
            return Ok(v);
        }
        // The block holding a sample's axis tick holds the sample.
        let from = p
            .held
            .as_ref()
            .map_or(p.from.min(axis), |h| h.end.min(axis));
        p.held = walk(rec, self.channel, false, from, p.next, budget, cancel)?;
        let h = p.held.as_ref().ok_or_else(|| {
            corrupt(format!(
                "channel {}: no block holds sample {}",
                self.channel, p.next
            ))
        })?;
        p.from = h.start;
        value_in(h, p.next).ok_or_else(|| {
            corrupt(format!(
                "channel {}: a block holds fewer samples than its row",
                self.channel
            ))
        })
    }
}

fn value_in(h: &Held, next: u64) -> Option<Sample> {
    let BlockChannel::Analog { values, .. } = &h.block.data else {
        return None;
    };
    values.get(usize::try_from(next.checked_sub(h.first)?).ok()?)
}
