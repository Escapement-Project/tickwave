//! Several channels' observations in one exact order, digital and analog, whatever their
//! sources' mappings. Observations from different channels at one exact time come out as one
//! group, in no order: a decoder that needs an order states its own rule. Timer queries sample
//! the channels at an exact time, before or at it.

use std::sync::atomic::AtomicBool;

use escapement_tickwave_format::mapping::Mapping;

use super::{AnalogCursor, Batch, Budget, DigitalCursor, Exact, Sampled, Seen, Until};
use crate::recording::{Error, Recording};

/// How many observations a port reads at a time.
const PORT_BATCH: usize = 4096;

/// A port's cursor: a digital channel's or an analog one's.
#[derive(Clone)]
pub enum PortCursor {
    Digital(DigitalCursor),
    Analog(AnalogCursor),
}

impl PortCursor {
    pub fn next_in(
        &mut self,
        rec: &Recording,
        until: Until,
        budget: &Budget,
        cancel: &AtomicBool,
        out: &mut Vec<Seen>,
    ) -> Result<Batch, Error> {
        match self {
            PortCursor::Digital(c) => c.next_in(rec, until, budget, cancel, out),
            PortCursor::Analog(c) => c.next_in(rec, until, budget, cancel, out),
        }
    }

    pub fn map(&self) -> Mapping {
        match self {
            PortCursor::Digital(c) => c.map(),
            PortCursor::Analog(c) => c.map(),
        }
    }

    pub fn source(&self) -> u32 {
        match self {
            PortCursor::Digital(c) => c.source(),
            PortCursor::Analog(c) => c.source(),
        }
    }

    /// An observation's exact axis time: an analog one's at its place.
    pub fn exact(&self, s: &Seen) -> Exact {
        match (self, *s) {
            (PortCursor::Analog(c), Seen::Sample(x)) => c.exact_place(x.place),
            (PortCursor::Analog(c), Seen::Break { place, .. }) => c.exact_place(place),
            (PortCursor::Analog(c), Seen::Lost { from, .. }) => c.exact_place(from),
            _ => self.exact_tick(s.source_tick()),
        }
    }

    /// Source tick `k`'s exact axis time.
    pub fn exact_tick(&self, k: i64) -> Exact {
        match self {
            PortCursor::Digital(c) => c.exact_tick(k),
            PortCursor::Analog(c) => {
                c.exact_place(i128::from(k).saturating_mul(i128::from(c.grid().d())))
            }
        }
    }

    /// Source tick `k` on the source's timeline.
    pub fn timeline(&self, k: i64) -> i64 {
        k.saturating_add(self.map().offset())
    }

    fn level(&self) -> Option<bool> {
        match self {
            PortCursor::Digital(c) => c.level(),
            PortCursor::Analog(_) => None,
        }
    }
}

/// Observations at one exact time, each with its port.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Group<'a> {
    pub at: Exact,
    pub seen: &'a [(usize, Seen)],
}

impl Group<'_> {
    pub fn axis(&self) -> i64 {
        self.at.floor()
    }

    /// Whether observations from more than one port share the time.
    pub fn coincident(&self) -> bool {
        self.seen.windows(2).any(|w| w[0].0 != w[1].0)
    }
}

/// Groups as a merge appends them, flat: every group's observations with their ports in one
/// list, and where each group ends in it. Clear and reuse one across windows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Groups {
    seen: Vec<(usize, Seen)>,
    /// Each group's exact time, and the end of its observations in `seen`.
    ends: Vec<(Exact, usize)>,
}

impl Groups {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.seen.clear();
        self.ends.clear();
    }

    /// How many groups it holds.
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Every group's observations, in order.
    pub fn seen(&self) -> &[(usize, Seen)] {
        &self.seen
    }

    pub fn get(&self, i: usize) -> Option<Group<'_>> {
        let &(at, end) = self.ends.get(i)?;
        let start = match i.checked_sub(1) {
            Some(j) => self.ends.get(j)?.1,
            None => 0,
        };
        Some(Group {
            at,
            seen: self.seen.get(start..end)?,
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = Group<'_>> + '_ {
        (0..self.len()).filter_map(|i| self.get(i))
    }

    fn mark(&self) -> (usize, usize) {
        (self.seen.len(), self.ends.len())
    }

    fn truncate(&mut self, (seen, ends): (usize, usize)) {
        self.seen.truncate(seen);
        self.ends.truncate(ends);
    }
}

/// A port as a call left it, for a call that fails to put back.
#[derive(Clone)]
struct Saved {
    cursor: Option<PortCursor>,
    pending: usize,
    reached: i64,
    level: Option<bool>,
    last: Option<Sampled>,
}

#[derive(Clone)]
struct Port {
    cursor: PortCursor,
    /// Its observations read and in no group yet, each with its exact time: those before `taken`
    /// went in a group during the call under way.
    pending: Vec<(Exact, Seen)>,
    taken: usize,
    /// Where its cursor reached, in the terms of the merge's last call.
    reached: i64,
    /// Whether the call under way found its live recording settled no further.
    stalled: bool,
    level: Option<bool>,
    last: Option<Sampled>,
    /// Its cursor's batch, before it goes into `pending`.
    batch: Vec<Seen>,
    saved: Saved,
}

impl Port {
    fn waiting(&self) -> bool {
        self.taken >= self.pending.len()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Terms {
    Axis,
    Source,
}

/// Why a run of the merge stopped.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// Every group before the bound, and to the limit, is out.
    Done,
    Budget,
    /// A live port is settled no further.
    Stalled,
}

#[derive(Clone)]
pub struct Merge {
    ports: Vec<Port>,
    terms: Option<Terms>,
}

impl Merge {
    /// Digital ports in the order given; each cursor where its decoder starts.
    pub fn new(cursors: Vec<DigitalCursor>) -> Self {
        Self::with_ports(cursors.into_iter().map(PortCursor::Digital).collect())
    }

    /// Digital and analog ports in the order given.
    pub fn with_ports(cursors: Vec<PortCursor>) -> Self {
        Self {
            ports: cursors
                .into_iter()
                .map(|cursor| Port {
                    level: cursor.level(),
                    reached: i64::MIN,
                    pending: Vec::new(),
                    taken: 0,
                    stalled: false,
                    last: None,
                    batch: Vec::new(),
                    saved: Saved {
                        cursor: None,
                        pending: 0,
                        reached: i64::MIN,
                        level: None,
                        last: None,
                    },
                    cursor,
                })
                .collect(),
            terms: None,
        }
    }

    pub fn ports(&self) -> usize {
        self.ports.len()
    }

    /// Digital port `p`'s level after the last group returned; None where it is not known.
    pub fn level(&self, p: usize) -> Option<bool> {
        self.ports.get(p)?.level
    }

    /// Analog port `p`'s last sample returned, None after a break or a loss: a value holds
    /// only inside its run, and nothing is interpolated.
    pub fn last_sample(&self, p: usize) -> Option<Sampled> {
        self.ports.get(p)?.last
    }

    pub fn cursor(&self, p: usize) -> Option<&PortCursor> {
        Some(&self.ports.get(p)?.cursor)
    }

    /// Group `g`'s tick on the source's timeline: every port's, where they share one source.
    pub fn timeline(&self, g: &Group) -> Option<i64> {
        let (p, s) = g.seen.first()?;
        Some(self.ports.get(*p)?.cursor.timeline(s.source_tick()))
    }

    /// Groups before axis tick `until`, at most `budget.items` of them, appended to `out`.
    pub fn next(
        &mut self,
        rec: &Recording,
        until: i64,
        budget: &Budget,
        cancel: &AtomicBool,
        out: &mut Groups,
    ) -> Result<Batch, Error> {
        self.next_in(rec, Until::Axis(until), budget, cancel, out)
    }

    /// As `next`, bounded by axis ticks or by the source's timeline, which needs every port on
    /// one source. Cancelled or refused, the merge stays where it was and `out` as it was.
    pub fn next_in(
        &mut self,
        rec: &Recording,
        until: Until,
        budget: &Budget,
        cancel: &AtomicBool,
        out: &mut Groups,
    ) -> Result<Batch, Error> {
        if matches!(until, Until::Source(_)) && !self.shared() {
            return Err(Error::Timeline);
        }
        let mark = out.mark();
        self.begin(until);
        match self.run(rec, until, None, budget, cancel, out) {
            Ok((_, reached)) => {
                self.commit();
                Ok(Batch {
                    taken: out.len() - mark.1,
                    reached,
                })
            }
            Err(e) => {
                self.rollback(out, mark);
                Err(e)
            }
        }
    }

    /// Every group up to exact time `t`, before it or, with `at`, at it too, appended to `out`;
    /// then `level` gives each port's level there. Nothing is skipped: a decoder sees each
    /// observation on the way. False where a live recording is settled short of `t`: the
    /// groups before where it is are out, and the levels are there.
    pub fn sample(
        &mut self,
        rec: &Recording,
        t: Exact,
        at: bool,
        budget: &Budget,
        cancel: &AtomicBool,
        out: &mut Groups,
    ) -> Result<bool, Error> {
        let mark = out.mark();
        let until = Until::Axis(t.floor().saturating_add(1));
        self.begin(until);
        let r = loop {
            match self.run(rec, until, Some((t, at)), budget, cancel, out) {
                Ok((Stop::Budget, _)) => continue,
                Ok((stop, _)) => break Ok(stop == Stop::Done),
                Err(e) => break Err(e),
            }
        };
        match &r {
            Ok(_) => self.commit(),
            Err(_) => self.rollback(out, mark),
        }
        r
    }

    /// Whether every port is on one source, mapped alike.
    fn shared(&self) -> bool {
        let Some(first) = self.ports.first() else {
            return true;
        };
        let (s, m) = (first.cursor.source(), first.cursor.map());
        self.ports.iter().all(|p| {
            let n = p.cursor.map();
            p.cursor.source() == s && n.origin() == m.origin() && n.m() == m.m()
        })
    }

    fn begin(&mut self, until: Until) {
        let terms = match until {
            Until::Axis(_) => Terms::Axis,
            Until::Source(_) => Terms::Source,
        };
        for p in &mut self.ports {
            p.saved = Saved {
                cursor: Some(p.cursor.clone()),
                pending: p.pending.len(),
                reached: p.reached,
                level: p.level,
                last: p.last,
            };
            p.taken = 0;
            p.stalled = false;
            if self.terms != Some(terms) {
                p.reached = i64::MIN;
            }
        }
        self.terms = Some(terms);
    }

    fn commit(&mut self) {
        for p in &mut self.ports {
            p.pending.drain(..p.taken.min(p.pending.len()));
            p.taken = 0;
            p.saved.cursor = None;
        }
    }

    fn rollback(&mut self, out: &mut Groups, mark: (usize, usize)) {
        for p in &mut self.ports {
            if let Some(cursor) = p.saved.cursor.take() {
                p.cursor = cursor;
            }
            p.pending.truncate(p.saved.pending);
            p.taken = 0;
            p.reached = p.saved.reached;
            p.level = p.saved.level;
            p.last = p.saved.last;
        }
        out.truncate(mark);
        // Each port's reach is back in the last call's terms, which this one may have changed:
        // the next call reads every port again.
        self.terms = None;
    }

    /// Groups before `bound`, and up to `limit` where given, at most `budget.items` of them.
    fn run(
        &mut self,
        rec: &Recording,
        bound: Until,
        limit: Option<(Exact, bool)>,
        budget: &Budget,
        cancel: &AtomicBool,
        out: &mut Groups,
    ) -> Result<(Stop, i64), Error> {
        let port_budget = Budget {
            items: PORT_BATCH,
            block_bytes: budget.block_bytes,
        };
        let until = bound.limit();
        let past = |x: Exact| limit.is_some_and(|(l, at)| x > l || (x == l && !at));
        // Where `until` lies in exact time: a port can hold observations past it from a call
        // with a later bound.
        let end = match bound {
            Until::Axis(_) => Some(Exact::whole(until)),
            Until::Source(_) => (self.ports.first()).map(|p| {
                p.cursor
                    .exact_tick(until.saturating_sub(p.cursor.map().offset()))
            }),
        };
        let mut groups = 0;
        loop {
            for p in &mut self.ports {
                while p.waiting() && p.reached < until && !p.stalled {
                    p.batch.clear();
                    let b = p
                        .cursor
                        .next_in(rec, bound, &port_budget, cancel, &mut p.batch)?;
                    p.reached = b.reached;
                    p.stalled = b.taken == 0 && b.reached < until;
                    for s in &p.batch {
                        p.pending.push((p.cursor.exact(s), *s));
                    }
                }
            }
            // A port with nothing pending has reported everything before where it reached, so
            // groups go out before the earliest such port's bound; one that reached `until`
            // bounds nothing.
            let mut wall: Option<(Exact, i64)> = None;
            for p in self
                .ports
                .iter()
                .filter(|p| p.waiting() && p.reached < until)
            {
                let x = match bound {
                    Until::Axis(_) => Exact::whole(p.reached),
                    Until::Source(_) => p
                        .cursor
                        .exact_tick(p.reached.saturating_sub(p.cursor.map().offset())),
                };
                if wall.is_none_or(|w| x < w.0) {
                    wall = Some((x, p.reached));
                }
            }
            let next = (self.ports.iter())
                .filter_map(|p| p.pending.get(p.taken).map(|x| x.0))
                .min();
            let open =
                |t: Exact| !past(t) && end.is_none_or(|e| t < e) && wall.is_none_or(|w| t < w.0);
            let Some(t) = next.filter(|&t| open(t)) else {
                return Ok(match wall {
                    Some(w) if !past(w.0) => (Stop::Stalled, w.1),
                    _ => (Stop::Done, until),
                });
            };
            if groups >= budget.items {
                let reached = match bound {
                    Until::Axis(_) => t.floor(),
                    Until::Source(_) => (self.ports.iter())
                        .find_map(|p| {
                            let &(x, s) = p.pending.get(p.taken)?;
                            (x == t).then(|| p.cursor.timeline(s.source_tick()))
                        })
                        .unwrap_or(until),
                };
                return Ok((Stop::Budget, reached));
            }
            for (k, p) in self.ports.iter_mut().enumerate() {
                while let Some(&(x, s)) = p.pending.get(p.taken)
                    && x == t
                {
                    match s {
                        Seen::Edge(e) => p.level = Some(e.level),
                        Seen::Known { level, .. } => p.level = Some(level),
                        Seen::Unknown { .. } => p.level = None,
                        Seen::Sample(x) => p.last = Some(x),
                        Seen::Break { .. } | Seen::Lost { .. } => p.last = None,
                    }
                    out.seen.push((k, s));
                    p.taken += 1;
                }
            }
            out.ends.push((t, out.seen.len()));
            groups += 1;
        }
    }
}
