//! What the side panel reads of one analysis's store, kept as the store changes: its ids in order,
//! by kind and holding a condition, the links its items hold to the analysis it is stacked on, by
//! the item read, and its text joined into lines. Each frame it takes in the ids the store's log
//! says changed. A store it has not read, or one whose log no longer reaches back to where it
//! read, it reads `READ_AT_MOST` ids a frame, so no frame's work grows with the store.

use std::collections::VecDeque;
use std::ops::Bound::{Excluded, Included, Unbounded};
use std::ops::Range;

use escapement_tickwave_decode::{
    FieldId, Id, Item, KindId, LinkId, Ref, Revision, Schema, Store, Tick, Value,
};
use indexset::BTreeSet as Ranked;

use super::LINE_AT_MOST;
use crate::track::condition;

/// The most ids the panel reads from its stores in a frame, taking in changes or reading a store
/// in full, shared between its analyses: a millisecond's work or less where an id costs 100 to
/// 200 ns, as one does here.
pub const READ_AT_MOST: usize = 1 << 12;

/// Before every id.
const FIRST: Id = Id::new(KindId(0), Tick::MIN);

/// A line of text: where it starts, the item and the byte in it its first character came from,
/// and each character with its item.
#[derive(Clone, Debug)]
pub(super) struct Line {
    pub start: Tick,
    pub at: (Id, usize),
    pub text: String,
    pub from: Vec<Id>,
}

/// One analysis's store as the panel reads it.
#[derive(Clone, Debug, Default)]
pub(super) struct Index {
    /// The store's revision taken in; None before the first frame.
    seen: Option<Revision>,
    /// The last id read in full; None before the first.
    read_to: Option<Id>,
    /// Every id the store held has been read. Until then a change past `read_to` waits for it.
    read: bool,
    /// Changed ids already read, to take in again.
    pending: VecDeque<Id>,
    all: Ranked<Id>,
    kinds: Vec<Ranked<Id>>,
    marked: Ranked<Id>,
    marked_kinds: Vec<Ranked<Id>>,
    /// The links its items hold upstream, by the item read: (read, reader, link).
    reads: Ranked<(Id, Id, LinkId)>,
    /// The same by reader, to drop a reader's links when it changes: (reader, link, read).
    readers: Ranked<(Id, LinkId, Id)>,
    /// The text, where its schema declares a kind's.
    pub lines: Vec<Line>,
}

impl Index {
    /// Whether it holds what its store does, as of this frame's step.
    pub fn current(&self) -> bool {
        self.read && self.pending.is_empty()
    }

    /// Whether it has read its store through once, so that only changes are left to take in.
    pub fn read(&self) -> bool {
        self.read
    }

    /// Takes in what `store` changed since the last step, then reads on: `budget` ids at most,
    /// and returns how many it read. `stacked` where the analysis reads another, whose items its
    /// links name.
    pub fn step(&mut self, store: &Store, schema: &Schema, stacked: bool, budget: usize) -> usize {
        let changed = self.seen.and_then(|s| store.changed_since(s));
        match changed {
            Some(ids) => {
                for &id in ids {
                    if self.read || self.read_to.is_some_and(|r| id <= r) {
                        self.pending.push_back(id);
                    }
                }
            }
            // The first step, another store, or one whose log no longer reaches back.
            None => *self = Index::default(),
        }
        self.seen = Some(store.revision());
        // Taking in more changes than the store holds items costs more than reading it again.
        if self.pending.len() > store.items().len().max(budget) {
            *self = Index {
                seen: self.seen,
                ..Index::default()
            };
        }
        let text = texts(schema);
        let mut joined: Option<(Id, Id)> = None;
        let mut widen = |id: Id| {
            if text.iter().any(|(k, _)| *k == id.kind) {
                joined = Some(joined.map_or((id, id), |(a, b)| (a.min(id), b.max(id))));
            }
        };
        let mut left = budget;
        while left > 0
            && let Some(id) = self.pending.pop_front()
        {
            match store.items().get(&id) {
                Some(item) => self.put(item, stacked, true),
                None => self.forget(id, stacked),
            }
            widen(id);
            left -= 1;
        }
        if !self.read {
            let from = self.read_to.map_or(Unbounded, Excluded);
            for (id, item) in store.items().range((from, Unbounded)).take(left) {
                self.put(item, stacked, false);
                self.read_to = Some(*id);
                widen(*id);
                left -= 1;
            }
            let from = self.read_to.map_or(Unbounded, Excluded);
            self.read = store.items().range((from, Unbounded)).next().is_none();
        }
        if let Some((first, last)) = joined {
            self.join(store, &text, first, last);
        }
        budget - left
    }

    /// Takes in `item` as its store holds it now; `again` where it may have been taken in before.
    fn put(&mut self, item: &Item, stacked: bool, again: bool) {
        let id = item.id;
        let k = usize::from(id.kind.0);
        if self.kinds.len() <= k {
            self.kinds.resize_with(k + 1, Ranked::new);
            self.marked_kinds.resize_with(k + 1, Ranked::new);
        }
        self.all.insert(id);
        self.kinds[k].insert(id);
        if condition(item) {
            self.marked.insert(id);
            self.marked_kinds[k].insert(id);
        } else if again {
            self.marked.remove(&id);
            self.marked_kinds[k].remove(&id);
        }
        if stacked {
            if again {
                self.unlink(id);
            }
            for &(link, r) in &item.links {
                if let Ref::Upstream(read) = r {
                    self.reads.insert((read, id, link));
                    self.readers.insert((id, link, read));
                }
            }
        }
    }

    /// Drops an id its store no longer holds.
    fn forget(&mut self, id: Id, stacked: bool) {
        let k = usize::from(id.kind.0);
        self.all.remove(&id);
        self.marked.remove(&id);
        if let Some(set) = self.kinds.get_mut(k) {
            set.remove(&id);
        }
        if let Some(set) = self.marked_kinds.get_mut(k) {
            set.remove(&id);
        }
        if stacked {
            self.unlink(id);
        }
    }

    fn unlink(&mut self, reader: Id) {
        let held: Vec<(Id, LinkId, Id)> = (self.readers.range((reader, LinkId(0), FIRST)..))
            .take_while(|(r, ..)| *r == reader)
            .copied()
            .collect();
        for (r, link, read) in held {
            self.readers.remove(&(r, link, read));
            self.reads.remove(&(read, r, link));
        }
    }

    /// The items of `id`'s kind just before and after it. Ids order by anchor first, so these are
    /// the nearest in time. None for one not read yet: until it has read up to `id`, an item
    /// between the last read and `id` may be its previous.
    pub fn neighbours(&self, id: Id) -> (Option<Id>, Option<Id>) {
        let Some(set) = self.kinds.get(usize::from(id.kind.0)) else {
            return (None, None);
        };
        let known = self.read || self.read_to.is_some_and(|r| r >= id);
        let previous = known
            .then(|| set.range(..id).next_back().copied())
            .flatten();
        let next = set.range((Excluded(id), Unbounded)).next().copied();
        (previous, next)
    }

    /// The items that read `read`, an item of the analysis this one is stacked on, by the link
    /// they read it by.
    pub fn readers_of(&self, read: Id) -> impl Iterator<Item = (Id, LinkId)> + '_ {
        (self.reads.range((read, FIRST, LinkId(0))..))
            .take_while(move |(r, ..)| *r == read)
            .map(|&(_, reader, link)| (reader, link))
    }

    /// The rows of a list of its items: every kind but `hidden`, all of them or only those
    /// holding a condition.
    pub fn shown<'a>(&'a self, hidden: &'a [KindId], marked: bool) -> Shown<'a> {
        Shown {
            index: self,
            hidden,
            marked,
        }
    }

    /// Joins the text again from the last line starting before `first`, which holds the byte
    /// before `first`'s and so a carriage return that may wait on them, until a line starts past
    /// `last` where one started before: from there the lines are as they were.
    fn join(&mut self, store: &Store, text: &[(KindId, FieldId)], first: Id, last: Id) {
        let from = self
            .lines
            .partition_point(|l| l.at.0 < first)
            .saturating_sub(1);
        let (start, skip) = match from {
            0 => (FIRST, 0),
            _ => self.lines[from].at,
        };
        let mut after = self.lines.split_off(from);
        let aligned =
            |at: (Id, usize)| at.0 > last && after.binary_search_by(|l| l.at.cmp(&at)).is_ok();
        let mut j = Joiner {
            text,
            lines: Vec::new(),
            line: None,
            cr: None,
        };
        let to = if self.read { None } else { self.read_to };
        let items = store.items().range((Included(start), Unbounded));
        let mut resumed = None;
        for (id, item) in items {
            if to.is_some_and(|t| *id > t) {
                break;
            }
            let skip = if *id == start { skip } else { 0 };
            if let Some(at) = j.item(item, skip, &aligned) {
                resumed = Some(at);
                break;
            }
        }
        match resumed {
            Some(at) => {
                self.lines.append(&mut j.lines);
                let k = after.partition_point(|l| l.at < at);
                self.lines.extend(after.drain(k..));
            }
            None => {
                j.finish();
                self.lines.append(&mut j.lines);
            }
        }
    }
}

/// Each kind that declares its text, with the field holding it.
fn texts(schema: &Schema) -> Vec<(KindId, FieldId)> {
    (schema.kinds.iter().enumerate())
        .filter_map(|(k, d)| Some((KindId(k as u16), d.text?)))
        .collect()
}

/// Joins items' text into lines: a newline ends a line, shown as `\n` with a carriage return
/// before it dropped, and a line wraps at `LINE_AT_MOST` characters.
struct Joiner<'a> {
    text: &'a [(KindId, FieldId)],
    lines: Vec<Line>,
    line: Option<Line>,
    /// A carriage return waiting on the next byte: dropped before a newline, else shown.
    cr: Option<(Id, usize, Tick)>,
}

impl Joiner<'_> {
    /// Takes `item`'s bytes past the first `skip`. Stops, and returns where, before starting a
    /// line where `aligned` holds.
    fn item(
        &mut self,
        item: &Item,
        skip: usize,
        aligned: &impl Fn((Id, usize)) -> bool,
    ) -> Option<(Id, usize)> {
        let &(_, f) = self.text.iter().find(|(k, _)| *k == item.id.kind)?;
        let (id, start) = (item.id, item.start);
        match item.field(f).map(|x| &x.value) {
            Some(&Value::Word { value, .. }) if skip == 0 => {
                self.byte(Some(value as u8), id, 0, start, aligned)
            }
            Some(Value::Bits { bytes, bits }) => {
                let whole = (*bits as usize / 8).min(bytes.len());
                (bytes[..whole].iter().enumerate().skip(skip))
                    .find_map(|(n, &b)| self.byte(Some(b), id, n, start, aligned))
            }
            Some(Value::Unknown { .. }) if skip == 0 => self.byte(None, id, 0, start, aligned),
            _ => None,
        }
    }

    fn byte(
        &mut self,
        b: Option<u8>,
        id: Id,
        n: usize,
        start: Tick,
        aligned: &impl Fn((Id, usize)) -> bool,
    ) -> Option<(Id, usize)> {
        if let Some((cid, cn, cstart)) = self.cr.take()
            && b != Some(b'\n')
            && let Some(at) = self.put("\\r", false, (cid, cn), cstart, aligned)
        {
            return Some(at);
        }
        let shown = match b {
            Some(b'\r') => {
                self.cr = Some((id, n, start));
                return None;
            }
            Some(b'\n') => "\\n".to_owned(),
            Some(c @ 0x20..=0x7e) => char::from(c).to_string(),
            Some(c) => format!("\\x{c:02x}"),
            None => "\u{b7}".to_owned(),
        };
        self.put(&shown, b == Some(b'\n'), (id, n), start, aligned)
    }

    fn put(
        &mut self,
        shown: &str,
        ends: bool,
        at: (Id, usize),
        start: Tick,
        aligned: &impl Fn((Id, usize)) -> bool,
    ) -> Option<(Id, usize)> {
        if self.line.is_none() && aligned(at) {
            return Some(at);
        }
        let l = self.line.get_or_insert_with(|| Line {
            start,
            at,
            text: String::new(),
            from: Vec::new(),
        });
        for c in shown.chars() {
            l.text.push(c);
            l.from.push(at.0);
        }
        if ends || l.from.len() >= LINE_AT_MOST {
            self.lines.extend(self.line.take());
        }
        None
    }

    /// Shows a carriage return still waiting, and ends the last line.
    fn finish(&mut self) {
        if let Some((id, n, start)) = self.cr.take() {
            self.put("\\r", false, (id, n), start, &|_| false);
        }
        self.lines.extend(self.line.take());
    }
}

/// The rows of a list over an index: its ids in order, but for kinds left out, all of them or
/// only those holding a condition. Rows are found by rank, so a list of any length shows any row
/// in a few lookups.
pub(super) struct Shown<'a> {
    index: &'a Index,
    hidden: &'a [KindId],
    marked: bool,
}

impl Shown<'_> {
    fn base(&self) -> &Ranked<Id> {
        if self.marked {
            &self.index.marked
        } else {
            &self.index.all
        }
    }

    fn kind(&self, k: KindId) -> Option<&Ranked<Id>> {
        let kinds = if self.marked {
            &self.index.marked_kinds
        } else {
            &self.index.kinds
        };
        kinds.get(usize::from(k.0))
    }

    /// The ids of the kinds left out that come before `id`.
    fn hidden_before(&self, id: &Id) -> usize {
        (self.hidden.iter())
            .filter_map(|&k| self.kind(k))
            .map(|s| s.rank(id))
            .sum()
    }

    pub fn len(&self) -> usize {
        let hidden: usize = (self.hidden.iter())
            .filter_map(|&k| self.kind(k))
            .map(|s| s.len())
            .sum();
        self.base().len() - hidden
    }

    /// The row `id` is shown in, if it is.
    pub fn row(&self, id: Id) -> Option<usize> {
        if self.hidden.contains(&id.kind) || !self.base().contains(&id) {
            return None;
        }
        Some(self.base().rank(&id) - self.hidden_before(&id))
    }

    /// The ids in `rows`.
    pub fn rows(&self, rows: Range<usize>) -> Vec<Id> {
        let base = self.base();
        let want = rows.end.min(self.len()).saturating_sub(rows.start);
        if self.hidden.is_empty() {
            return base
                .range_idx(rows.start..rows.start + want)
                .copied()
                .collect();
        }
        if want == 0 {
            return Vec::new();
        }
        // The least place in the base past which `rows.start` ids are shown: the first row's.
        let shown_before = |p: usize| match base.get_index(p) {
            Some(id) => p - self.hidden_before(id),
            None => self.len(),
        };
        let (mut lo, mut hi) = (0, base.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if shown_before(mid + 1) > rows.start {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        let Some(first) = base.get_index(lo) else {
            return Vec::new();
        };
        // The rows after it from each kind shown, merged in order.
        let kinds = self.index.kinds.len().max(self.index.marked_kinds.len());
        let mut runs: Vec<_> = (0..kinds as u16)
            .map(KindId)
            .filter(|k| !self.hidden.contains(k))
            .filter_map(|k| self.kind(k))
            .map(|s| s.range((Included(*first), Unbounded)).peekable())
            .collect();
        let mut out = Vec::with_capacity(want);
        while out.len() < want {
            let next = (runs.iter_mut().enumerate())
                .filter_map(|(r, run)| Some((**run.peek()?, r)))
                .min();
            let Some((id, r)) = next else {
                break;
            };
            runs[r].next();
            out.push(id);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use escapement_tickwave_decode::{FieldType, KindDecl, LinkDecl, LinkTo, Op, Span, Status};
    use std::collections::BTreeSet;

    /// A small generator, so the cases repeat.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    fn schema() -> Schema {
        let text = |name: &'static str| {
            KindDecl::new(name)
                .with_fields(vec![escapement_tickwave_decode::FieldDecl::new(
                    "b",
                    FieldType::Bits { max: 64 },
                )])
                .with_text(FieldId(0))
        };
        Schema::new(vec![
            text("a"),
            KindDecl::new("b").with_links(vec![LinkDecl::new(
                "of",
                vec![LinkTo::Upstream(escapement_tickwave_decode::name("x"))],
            )]),
            KindDecl::new("c"),
        ])
    }

    /// Does one random thing to `store`: opens an item, sets its bytes or a link, or closes it
    /// complete or not, which drops it where it meets no kept span.
    fn churn(
        store: &mut Store,
        rng: &mut Rng,
        open: &mut Vec<Id>,
        at: &mut Tick,
        dropped: &mut Vec<Id>,
    ) {
        let pick = rng.below(10);
        // An id the store dropped, opened again: a decoder may, and its content is new.
        if pick == 3 && !dropped.is_empty() {
            // The latest, often dropped since the index last stepped.
            let Some(id) = dropped.pop() else {
                return;
            };
            let open_again = Op::Open {
                id,
                parent: None,
                start: id.anchor,
            };
            if store.apply_batch(&[open_again], 64).is_ok() {
                open.push(id);
            }
            return;
        }
        if pick < 4 || open.is_empty() {
            *at += 1 + rng.below(3) as Tick;
            // Now and then an anchor behind the others.
            let anchor = if rng.below(8) == 0 { *at - 40 } else { *at };
            let id = Id {
                anchor,
                kind: KindId(rng.below(3) as u16),
                disc: rng.below(4) as u32,
            };
            let start = anchor - rng.below(3) as Tick;
            if store
                .apply_batch(
                    &[Op::Open {
                        id,
                        parent: None,
                        start,
                    }],
                    64,
                )
                .is_ok()
            {
                open.push(id);
            }
            return;
        }
        let k = rng.below(open.len() as u64) as usize;
        let id = open[k];
        let unknown = Value::Unknown {
            reason: escapement_tickwave_decode::Reason(0),
            needs: None,
        };
        let op = match (pick, id.kind.0) {
            // A value not known makes an open item a condition.
            (4..=5, 0 | 2) if rng.below(4) == 0 => Op::Set {
                id,
                field: FieldId(0),
                value: unknown,
                span: None,
                driver: None,
            },
            (4..=5, 0) => {
                let n = rng.below(4) as usize;
                let bytes = (0..n)
                    .map(|_| [b'\r', b'\n', b'q', 0x15][rng.below(4) as usize])
                    .collect();
                Op::Set {
                    id,
                    field: FieldId(0),
                    value: Value::Bits {
                        bits: 8 * n as u32,
                        bytes,
                    },
                    span: None,
                    driver: None,
                }
            }
            (4..=5, 1) => Op::Link {
                id,
                link: LinkId(0),
                other: Ref::Upstream(Id::new(KindId(0), rng.below(200) as Tick)),
            },
            _ => {
                let status = if rng.below(3) == 0 {
                    Status::Incomplete(escapement_tickwave_decode::Cause(0))
                } else {
                    Status::Complete
                };
                Op::Close {
                    id,
                    end: *at + rng.below(5) as Tick,
                    status,
                }
            }
        };
        let close = matches!(op, Op::Close { .. });
        if store.apply_batch(&[op], 64).is_ok() && close {
            open.swap_remove(k);
            if !store.items().contains_key(&id) {
                dropped.push(id);
            }
        }
    }

    fn joined(store: &Store, schema: &Schema) -> Vec<(Tick, (Id, usize), String)> {
        let text = texts(schema);
        let mut j = Joiner {
            text: &text,
            lines: Vec::new(),
            line: None,
            cr: None,
        };
        for item in store.items().values() {
            j.item(item, 0, &|_| false);
        }
        j.finish();
        let lines = j.lines;
        lines.into_iter().map(|l| (l.start, l.at, l.text)).collect()
    }

    /// The index against the store read afresh.
    fn check(index: &Index, store: &Store, schema: &Schema, rng: &mut Rng) {
        let items = store.items();
        let all: Vec<Id> = items.keys().copied().collect();
        assert_eq!(index.all.iter().copied().collect::<Vec<_>>(), all);
        let marked: Vec<Id> = items
            .values()
            .filter(|i| condition(i))
            .map(|i| i.id)
            .collect();
        assert_eq!(index.marked.iter().copied().collect::<Vec<_>>(), marked);
        let reads: BTreeSet<(Id, Id, LinkId)> = (items.values())
            .flat_map(|i| {
                (i.links.iter()).filter_map(move |&(l, r)| match r {
                    Ref::Upstream(read) => Some((read, i.id, l)),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(index.reads.iter().copied().collect::<BTreeSet<_>>(), reads);
        for &id in all.iter().step_by(7) {
            let same: Vec<Id> = all.iter().copied().filter(|i| i.kind == id.kind).collect();
            let k = same.binary_search(&id).unwrap();
            let want = (k.checked_sub(1).map(|p| same[p]), same.get(k + 1).copied());
            assert_eq!(index.neighbours(id), want, "around {id:?}");
        }
        for &(read, ..) in reads.iter().step_by(3) {
            let want: Vec<(Id, LinkId)> = (reads.iter())
                .filter(|r| r.0 == read)
                .map(|r| (r.1, r.2))
                .collect();
            assert_eq!(index.readers_of(read).collect::<Vec<_>>(), want);
        }
        // Rows under a filter, against the filter applied to every id.
        let hidden: Vec<KindId> = (0..3).filter(|_| rng.below(2) == 0).map(KindId).collect();
        let only = rng.below(2) == 0;
        let want: Vec<Id> = (items.values())
            .filter(|i| !hidden.contains(&i.id.kind) && (!only || condition(i)))
            .map(|i| i.id)
            .collect();
        let shown = index.shown(&hidden, only);
        assert_eq!(
            shown.len(),
            want.len(),
            "hidden {hidden:?}, conditions only {only}"
        );
        for _ in 0..4 {
            let a = rng.below(want.len() as u64 + 2) as usize;
            let b = a + rng.below(12) as usize;
            let end = b.min(want.len());
            let rows = want.get(a.min(end)..end).unwrap_or(&[]);
            assert_eq!(shown.rows(a..b), rows, "rows {a}..{b}");
        }
        for (r, &id) in want.iter().enumerate().step_by(5) {
            assert_eq!(shown.row(id), Some(r));
        }
        if let Some(&id) = all.iter().find(|i| !want.contains(i)) {
            assert_eq!(shown.row(id), None);
        }
        assert_eq!(
            index
                .lines
                .iter()
                .map(|l| (l.start, l.at, l.text.clone()))
                .collect::<Vec<_>>(),
            joined(store, schema)
        );
        for l in &index.lines {
            assert_eq!(l.from.len(), l.text.chars().count());
        }
    }

    #[test]
    fn an_index_holds_what_its_store_does_through_every_change() {
        let schema = schema();
        for seed in [0x51e7_u64, 0x7ac4, 0xbeef] {
            let mut rng = Rng(seed);
            let mut store = Store::with_keep(vec![Span { start: 0, end: 300 }]);
            let (mut open, mut at, mut dropped) = (Vec::new(), 0, Vec::new());
            let mut index = Index::default();
            for round in 0..300 {
                for _ in 0..rng.below(12) {
                    churn(&mut store, &mut rng, &mut open, &mut at, &mut dropped);
                }
                // A small budget now and then, so reading in full spans frames between changes.
                let budget = if rng.below(4) == 0 { 3 } else { READ_AT_MOST };
                index.step(&store, &schema, true, budget);
                while !index.current() {
                    for _ in 0..rng.below(3) {
                        churn(&mut store, &mut rng, &mut open, &mut at, &mut dropped);
                    }
                    index.step(&store, &schema, true, budget);
                }
                if round % 3 == 0 {
                    check(&index, &store, &schema, &mut rng);
                }
                // A panel opened late reads the store a few ids a frame while it changes.
                if round % 20 == 19 {
                    let mut late = Index::default();
                    while !late.current() {
                        for _ in 0..rng.below(4) {
                            churn(&mut store, &mut rng, &mut open, &mut at, &mut dropped);
                        }
                        late.step(&store, &schema, true, 5);
                    }
                    check(&late, &store, &schema, &mut rng);
                }
            }
            assert!(
                store.items().len() > 50,
                "the store kept {}",
                store.items().len()
            );
        }
    }

    #[test]
    fn an_index_reads_another_store_afresh() {
        let schema = schema();
        let mut rng = Rng(9);
        let (mut a, mut b) = (Store::with_keep(vec![]), Store::with_keep(vec![]));
        for s in [&mut a, &mut b] {
            s.keep = vec![Span {
                start: 0,
                end: Tick::MAX,
            }];
            let (mut open, mut at, mut dropped) = (Vec::new(), 0, Vec::new());
            for _ in 0..200 {
                churn(s, &mut rng, &mut open, &mut at, &mut dropped);
            }
        }
        let mut index = Index::default();
        index.step(&a, &schema, true, READ_AT_MOST);
        check(&index, &a, &schema, &mut rng);
        index.step(&b, &schema, true, READ_AT_MOST);
        check(&index, &b, &schema, &mut rng);
        // A clone is another store too.
        let c = b.clone();
        index.step(&c, &schema, true, 5);
        assert!(!index.current(), "five ids a frame read the copy anew");
        while !index.current() {
            index.step(&c, &schema, true, 5);
        }
        check(&index, &c, &schema, &mut rng);
    }
}

/// What reading a store in full costs an id, the figure `READ_AT_MOST` is set from. Run with
/// `--release -- --ignored --nocapture`.
#[cfg(test)]
mod cost {
    use super::*;
    use escapement_tickwave_decode::{FieldDecl, FieldType, KindDecl, Op, Span, Status};

    /// `n` items of one kind, each with a byte of text or a link upstream where asked.
    fn store(n: i64, text: bool, link: bool) -> Store {
        let mut s = Store::with_keep(vec![Span {
            start: 0,
            end: Tick::MAX,
        }]);
        for k in 0..n {
            let id = Id::new(KindId(0), k * 1000);
            let mut ops = vec![Op::Open {
                id,
                parent: None,
                start: k * 1000,
            }];
            if text {
                ops.push(Op::Set {
                    id,
                    field: FieldId(0),
                    value: Value::Word {
                        value: u64::from(b'a' + (k % 26) as u8),
                        bits: 8,
                    },
                    span: None,
                    driver: None,
                });
            }
            if link {
                ops.push(Op::Link {
                    id,
                    link: LinkId(0),
                    other: Ref::Upstream(Id::new(KindId(0), k * 1000)),
                });
            }
            ops.push(Op::Close {
                id,
                end: k * 1000 + 800,
                status: Status::Complete,
            });
            s.apply_batch(&ops, 8).unwrap();
        }
        s
    }

    #[test]
    #[ignore = "a measurement"]
    fn reading_cost_per_id() {
        let n = 1 << 20;
        let plain = Schema::new(vec![KindDecl::new("x")]);
        let texted = Schema::new(vec![
            KindDecl::new("x")
                .with_fields(vec![FieldDecl::new("d", FieldType::Word { bits: 8 })])
                .with_text(FieldId(0)),
        ]);
        for (name, schema, text, link) in [
            ("plain", &plain, false, false),
            ("text", &texted, true, false),
            ("stacked, one link", &plain, false, true),
        ] {
            let s = store(n, text, link);
            let mut index = Index::default();
            let t = std::time::Instant::now();
            let mut frames = 0;
            while !index.current() {
                index.step(&s, schema, link, READ_AT_MOST);
                frames += 1;
            }
            let ns = t.elapsed().as_nanos() as f64 / n as f64;
            println!("{name}: {ns:.0} ns an id, {frames} steps");
        }
    }
}
