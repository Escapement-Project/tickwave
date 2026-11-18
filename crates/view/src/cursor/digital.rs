//! A digital channel's cursor.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use escapement_tickwave_format::decode::BlockChannel;
use escapement_tickwave_format::header::ChannelKind as HeaderKind;
use escapement_tickwave_format::mapping::Mapping;
use escapement_tickwave_format::model::Gap;

use super::lists::{self, More};
use super::{
    Batch, Budget, Edge, Exact, Held, Seen, Settled, Until, block_at, corrupt, hold, items_of,
    last_block, place_of, promise_of, source_of, walk,
};
use crate::read::check;
use crate::recording::{Error, Place, Recording, Revision, Store};

#[derive(Clone, Copy, Debug)]
struct Boundary {
    source_tick: i64,
    axis: i64,
    level: Option<bool>,
}

/// What the recording states of a digital channel, apart from its gaps: its start and a finished
/// file's end in source ticks, and a finished file's count of edges.
struct Facts {
    initial: bool,
    start: i64,
    map: Mapping,
    end: Option<i64>,
    edges: Option<u64>,
}

fn facts(rec: &Recording, c: usize) -> Result<Facts, Error> {
    let Place::Digital {
        initial,
        source_start,
        map,
        ..
    } = place_of(rec, c)?
    else {
        return Err(Error::NoChannel(c));
    };
    let (end, edges) = match &rec.store {
        Store::Finished(f) => match f.footer().header.channels.get(c).map(|ch| &ch.data) {
            Some(HeaderKind::Digital { end, edges, .. }) => (Some(*end), Some(*edges)),
            _ => return Err(Error::NoChannel(c)),
        },
        #[cfg(feature = "live")]
        Store::Live(_) => (None, None),
    };
    Ok(Facts {
        initial: *initial,
        start: *source_start,
        map: *map,
        end,
        edges,
    })
}

/// Where digital channel `c`, starting at source tick `start`, is settled to; None for a
/// finished file's. A writer takes an edge at its promise or later and at the last edge's source
/// tick or later, past the last gap, and a gap from past the last edge and the last gap's end,
/// or from the channel's start before either.
#[cfg_attr(
    not(feature = "live"),
    expect(
        unused_variables,
        reason = "only a live recording's blocks are read for it"
    )
)]
fn settled(
    rec: &Recording,
    c: usize,
    start: i64,
    map: &Mapping,
    cancel: &AtomicBool,
) -> Result<Option<Settled>, Error> {
    let Some(promise) = promise_of(rec, c)? else {
        return Ok(None);
    };
    let (edge, gap): (Option<i64>, Option<i64>) = match &rec.store {
        Store::Finished(_) => (None, None),
        #[cfg(feature = "live")]
        Store::Live(live) => (
            last_edge(rec, c, cancel)?,
            lists::live_last_gap(rec, live, c)?.map(|g| g.to),
        ),
    };
    let edges_from = map
        .first_at(promise)
        .max(edge.unwrap_or(start))
        .max(gap.map_or(i64::MIN, |to| to.saturating_add(1)));
    let gaps_from = edge.max(gap).map_or(start, |t| t.saturating_add(1));
    let source = edges_from.min(gaps_from);
    Ok(Some(Settled {
        source,
        axis: map.axis_saturating(source),
    }))
}

/// Live channel `c`'s last edge's source tick, from the last of its blocks that holds any.
#[cfg(feature = "live")]
fn last_edge(rec: &Recording, c: usize, cancel: &AtomicBool) -> Result<Option<i64>, Error> {
    for at in rec.blocks.get(c).into_iter().flatten().rev() {
        let items = items_of(rec, c, at, true, cancel)?;
        if items.1 > 0 {
            let h = hold(rec, c, at, items, true, &Budget::LARGE, cancel)?;
            return Ok(h.block.data.source_ticks().and_then(|s| s.last().copied()));
        }
    }
    Ok(None)
}

#[derive(Clone)]
struct DigitalPos {
    next: u64,
    /// The boundaries read so far and not yet passed; `boundary` is the next.
    bounds: Arc<Vec<Boundary>>,
    boundary: usize,
    /// Where the gaps after those read come from.
    more: More,
    level: Option<bool>,
    /// The block holding edge `next`, where the cursor holds it.
    held: Option<Held>,
    /// A tick at or before the block holding edge `next`, where a walk to it starts.
    from: i64,
    /// The recording's revision the held block is of: a live recording's open block grows.
    revision: Revision,
}

/// A digital channel's observations from a place on, resumable by ordinal. Its gaps come a list
/// page at a time.
#[derive(Clone)]
pub struct DigitalCursor {
    channel: usize,
    source: u32,
    map: Mapping,
    /// The channel's start, in source ticks.
    start: i64,
    /// A finished file's channel's end.
    end: Option<Boundary>,
    pos: DigitalPos,
}

fn gap_bounds(gaps: &[Gap], map: &Mapping, out: &mut Vec<Boundary>) {
    for g in gaps {
        out.push(Boundary {
            source_tick: g.from,
            axis: map.axis_saturating(g.from),
            level: None,
        });
        out.push(Boundary {
            source_tick: g.to,
            axis: map.axis_saturating(g.to),
            level: Some(g.level),
        });
    }
}

impl DigitalCursor {
    /// A cursor at the first observation at or after axis tick `t`, and the level there. On a
    /// live recording `t` must be settled, else `Error::Unsettled`.
    pub fn seek(
        rec: &Recording,
        channel: usize,
        t: i64,
        budget: &Budget,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        check(cancel)?;
        let c = channel;
        let f = facts(rec, c)?;
        let t = t.max(0);
        if let Some(s) = settled(rec, c, f.start, &f.map, cancel)?
            && t > s.axis
        {
            return Err(Error::Unsettled {
                channel: c,
                at: t,
                settled: s.axis,
            });
        }
        let map = f.map;
        let window = lists::gaps_at(rec, c, t, &map, cancel)?;
        let end = f.end.map(|e| Boundary {
            source_tick: e,
            axis: map.axis_saturating(e),
            level: None,
        });
        // The channel's start comes before any gap. Where a gap starts there, the level is
        // unknown from the start.
        let mut bounds = Vec::with_capacity(2 * window.list.len() + 2);
        if window.first && !window.list.first().is_some_and(|g| g.from == f.start) {
            bounds.push(Boundary {
                source_tick: f.start,
                axis: map.axis_saturating(f.start),
                level: Some(f.initial),
            });
        }
        gap_bounds(&window.list, &map, &mut bounds);
        if window.more == More::Done
            && let Some(end) = end
        {
            bounds.push(end);
        }
        let passed = bounds.partition_point(|b| b.axis < t);
        let last = passed.checked_sub(1).and_then(|i| bounds.get(i)).copied();
        let mut pos = DigitalPos {
            next: f.edges.unwrap_or(0),
            bounds: Arc::new(bounds),
            boundary: passed,
            more: window.more,
            level: last.and_then(|b| b.level),
            held: None,
            from: t,
            revision: rec.revision,
        };
        // The block holding `t`; on a live recording past its blocks, its last.
        if let Some(at) = block_at(rec, c, t, cancel)?.or_else(|| last_block(rec, c)) {
            let items = items_of(rec, c, &at, true, cancel)?;
            let held = hold(rec, c, &at, items, true, budget, cancel)?;
            let BlockChannel::Digital {
                initial: level_at_start,
                edges: axes,
                source,
            } = &held.block.data
            else {
                return Err(Error::NoChannel(c));
            };
            let sources = source.as_deref().unwrap_or(axes);
            let lo = axes.partition_point(|&a| a < t);
            let before = sources.get(..lo).unwrap_or_default();
            pos.next = held.first.saturating_add(lo as u64);
            // A block's level bit carries the level from before a gap to the gap's end, which
            // sets it: a boundary in the block, before `t`, overrides it. Edges at a boundary's
            // tick come after it: the channel's start, where edges can lie, comes first.
            pos.level = match last {
                None | Some(Boundary { level: None, .. }) => None,
                Some(Boundary {
                    level: Some(l),
                    source_tick,
                    axis,
                }) if axis >= at.start => {
                    let after = before.iter().filter(|&&s| s >= source_tick).count();
                    Some(l ^ (after % 2 == 1))
                }
                Some(_) => Some(*level_at_start ^ (lo % 2 == 1)),
            };
            pos.from = held.start;
            pos.held = Some(held);
        }
        Ok(Self {
            channel: c,
            source: source_of(rec, c),
            map,
            start: f.start,
            end,
            pos,
        })
    }

    /// A cursor at the first observation at or after `u` on the source's timeline. On a live
    /// recording `u` must be settled, else `Error::Unsettled`.
    pub fn seek_timeline(
        rec: &Recording,
        channel: usize,
        u: i64,
        budget: &Budget,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let f = facts(rec, channel)?;
        let k = u.saturating_sub(f.map.offset());
        let t = f.map.axis_saturating(k);
        if let Some(s) = settled(rec, channel, f.start, &f.map, cancel)?
            && k > s.source
        {
            return Err(Error::Unsettled {
                channel,
                at: t,
                settled: s.axis,
            });
        }
        let mut cur = Self::seek(rec, channel, t, budget, cancel)?;
        // Seeking by axis tick can land on an earlier source tick that shares it.
        let mut skipped = Vec::new();
        loop {
            skipped.clear();
            let b = cur.next_in(rec, Until::Source(u), budget, cancel, &mut skipped)?;
            if b.reached >= u || b.taken == 0 {
                return Ok(cur);
            }
        }
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

    /// Where a finished file's channel ends, on the source's timeline; None on a live
    /// recording, whose writer states it only when it finishes.
    pub fn end(&self) -> Option<i64> {
        self.end.map(|b| self.timeline(b.source_tick))
    }

    /// The level after the last observation reported; None where it is not known.
    pub fn level(&self) -> Option<bool> {
        self.pos.level
    }

    /// The ordinal of the next edge.
    pub fn next_ordinal(&self) -> u64 {
        self.pos.next
    }

    /// Edge `e`'s exact axis time.
    pub fn exact(&self, e: &Edge) -> Exact {
        self.exact_tick(e.source_tick)
    }

    /// Source tick `k`'s exact axis time.
    pub fn exact_tick(&self, k: i64) -> Exact {
        let den = u128::from(self.map.m().den());
        // Any source tick's fits 128 bits under a mapping a header allows.
        let num = self.map.axis_time(i128::from(k), 1).unwrap_or(if k < 0 {
            i128::MIN
        } else {
            i128::MAX
        });
        Exact::new(num, den).unwrap_or(Exact::whole(self.map.axis_saturating(k)))
    }

    /// The exact axis time of a place `units / d` of the source's ticks; None for `d` 0, or a
    /// time past 128 bits.
    pub fn exact_place(&self, units: i128, d: u64) -> Option<Exact> {
        let num = self.map.axis_time(units, d)?;
        Exact::new(num, u128::from(d) * u128::from(self.map.m().den()))
    }

    /// Source tick `k` on the source's timeline: moved by the channel's offset.
    pub fn timeline(&self, k: i64) -> i64 {
        k.saturating_add(self.map.offset())
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

    /// Observations before `until`, at most the budget's, appended to `out`; `reached` is in
    /// `until`'s terms. Cancelled or refused, the cursor stays where it was and `out` as it was.
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
        let settled = settled(rec, c, self.start, &self.map, cancel)?;
        let offset = self.map.offset();
        let limit = until.cap(settled, offset);
        let mut p = self.pos.clone();
        if p.revision != rec.revision {
            if let Some(h) = p.held.take() {
                p.from = h.start;
            }
            p.revision = rec.revision;
        }
        // Once a call finds no next edge, or no gap past those read, it doesn't look again.
        let (mut edges_done, mut gaps_done) = (false, false);
        let mut taken = 0;
        let reached = loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let edge = match edges_done {
                true => None,
                false => self.peek(rec, &mut p, budget, cancel)?,
            };
            edges_done = edge.is_none();
            let bound = self.bound(rec, &mut p, &mut gaps_done, cancel)?;
            // The channel's start at an edge's tick comes first: the edge changes the level the
            // start states. No edge lies at a gap's ticks (FORMAT.md, "Gaps").
            let (boundary, axis, s) = match (edge, bound) {
                (Some((s, _)), Some(b)) if b.source_tick <= s => (Some(b), b.axis, b.source_tick),
                (Some((s, a)), _) => (None, a, s),
                (None, Some(b)) => (Some(b), b.axis, b.source_tick),
                (None, None) => break limit,
            };
            let at = until.key(axis, s, offset);
            if at >= limit {
                break limit;
            }
            if taken >= budget.items {
                break at;
            }
            match boundary {
                Some(b) => {
                    out.push(match b.level {
                        Some(level) => Seen::Known {
                            source_tick: s,
                            axis,
                            level,
                        },
                        None => Seen::Unknown {
                            source_tick: s,
                            axis,
                        },
                    });
                    p.level = b.level;
                    p.boundary += 1;
                }
                None => {
                    let level = !p.level.ok_or_else(|| {
                        corrupt(format!(
                            "channel {c}: an edge at source tick {s} where the level is not known"
                        ))
                    })?;
                    out.push(Seen::Edge(Edge {
                        ordinal: p.next,
                        source_tick: s,
                        axis,
                        level,
                    }));
                    p.level = Some(level);
                    p.next += 1;
                }
            }
            taken += 1;
        };
        self.pos = p;
        Ok(Batch { taken, reached })
    }

    /// Edge `p.next`'s source tick and axis tick, moving `p` to its block; None past the last.
    fn peek(
        &self,
        rec: &Recording,
        p: &mut DigitalPos,
        budget: &Budget,
        cancel: &AtomicBool,
    ) -> Result<Option<(i64, i64)>, Error> {
        loop {
            if let Some(h) = &p.held {
                if let Some(e) = edge_in(h, p.next) {
                    return Ok(Some(e));
                }
                let (start, end) = (h.start, h.end);
                p.held = walk(rec, self.channel, true, end, p.next, budget, cancel)?;
                // With none past it, a live recording's walk starts again at this block, which
                // can grow.
                p.from = p.held.as_ref().map_or(start, |h| h.start);
            } else {
                p.held = walk(rec, self.channel, true, p.from, p.next, budget, cancel)?;
                if let Some(h) = &p.held {
                    p.from = h.start;
                }
            }
            if p.held.is_none() {
                return Ok(None);
            }
        }
    }

    /// The next boundary, reading the next window of gaps once those read are passed.
    fn bound(
        &self,
        rec: &Recording,
        p: &mut DigitalPos,
        gaps_done: &mut bool,
        cancel: &AtomicBool,
    ) -> Result<Option<Boundary>, Error> {
        while p.boundary >= p.bounds.len() {
            if *gaps_done || p.more == More::Done {
                return Ok(None);
            }
            let w = lists::gaps_after(rec, self.channel, p.more, cancel)?;
            if w.list.is_empty() && w.more == p.more {
                // A live recording's list holds no gap past those read yet.
                *gaps_done = true;
                return Ok(None);
            }
            let bounds = Arc::make_mut(&mut p.bounds);
            bounds.clear();
            gap_bounds(&w.list, &self.map, bounds);
            if w.more == More::Done
                && let Some(end) = self.end
            {
                bounds.push(end);
            }
            p.boundary = 0;
            p.more = w.more;
        }
        Ok(p.bounds.get(p.boundary).copied())
    }
}

/// Edge `next`'s source tick and axis tick, where `h` holds it.
fn edge_in(h: &Held, next: u64) -> Option<(i64, i64)> {
    let k = usize::try_from(next.checked_sub(h.first)?).ok()?;
    let BlockChannel::Digital { edges, source, .. } = &h.block.data else {
        return None;
    };
    let a = *edges.get(k)?;
    let s = match source {
        Some(s) => *s.get(k)?,
        None => a,
    };
    Some((s, a))
}

/// A stretch of a digital channel where its level is known, on its source's timeline: from, to,
/// the level at `from`, and the edges inside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stretch {
    pub from: i64,
    pub to: i64,
    pub level: bool,
    pub edges: Vec<i64>,
}

impl Stretch {
    /// The level just before `t`, inside the stretch: an edge at `t` does not count.
    pub fn before(&self, t: i64) -> bool {
        self.level ^ (self.edges.partition_point(|&e| e < t) % 2 == 1)
    }
}

/// Channel `c` before `until` on its source's timeline, as known stretches: for a reader that
/// holds a whole capture, or its prefix. On a live recording, the last stretch ends where the
/// channel is settled to, where that comes first.
pub fn whole_line(
    rec: &Recording,
    c: usize,
    until: i64,
    cancel: &AtomicBool,
) -> Result<Vec<Stretch>, Error> {
    let budget = Budget {
        items: 4096,
        block_bytes: u32::MAX,
    };
    let mut cur = DigitalCursor::seek(rec, c, 0, &budget, cancel)?;
    let mut out: Vec<Stretch> = Vec::new();
    let mut open: Option<Stretch> = None;
    let mut seen = Vec::new();
    let reached = loop {
        seen.clear();
        let b = cur.next_in(rec, Until::Source(until), &budget, cancel, &mut seen)?;
        for s in &seen {
            let u = cur.timeline(s.source_tick());
            match s {
                Seen::Known { level, .. } => {
                    open = Some(Stretch {
                        from: u,
                        to: u,
                        level: *level,
                        edges: Vec::new(),
                    })
                }
                Seen::Unknown { .. } => {
                    if let Some(mut st) = open.take() {
                        st.to = u;
                        out.push(st);
                    }
                }
                Seen::Edge(_) => {
                    if let Some(st) = &mut open {
                        st.edges.push(u);
                    }
                }
                Seen::Sample(_) | Seen::Break { .. } | Seen::Lost { .. } => {}
            }
        }
        if b.reached >= until || b.taken == 0 {
            break b.reached;
        }
    };
    if let Some(mut st) = open {
        st.to = reached.min(until);
        out.push(st);
    }
    Ok(out)
}

/// The mapping from channel `c`'s source's timeline to the axis: its own, without its offset.
pub fn timeline_map(rec: &Recording, c: usize) -> Result<Mapping, Error> {
    let m = match place_of(rec, c)? {
        Place::Digital { map, .. } | Place::Analog { map, .. } => *map,
        Place::Unavailable(_) => return Err(Error::NoChannel(c)),
    };
    Ok(Mapping::new(m.origin(), 0, m.m()))
}
