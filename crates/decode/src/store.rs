//! One analysis's output as the host admits it: the open items, checked op by op against their
//! lifecycle, and whole items kept only where they meet a span a view asked for. A store logs
//! which items each change touched, so a view that keeps its own reading of them (an index, the
//! rows of a list) takes in only what changed.

use crate::output::{
    Element, FieldId, Id, LinkId, MAX_BITS, MAX_CHUNK, MAX_FIELDS, MAX_LINKS, MAX_TEXT, Op, Ref,
    Role, SeqId, Span, Status, Tick, Value,
};
use foldhash::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

/// Why an operation breaks its item's lifecycle.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Refused {
    #[error("the item was opened before")]
    Reopened,
    #[error("the item is not open")]
    NotOpen,
    #[error("the parent is not open")]
    ParentNotOpen,
    #[error("the field was set before")]
    FieldTwice,
    #[error("more than {MAX_FIELDS} fields")]
    TooManyFields,
    #[error("more than {MAX_CHUNK} elements in one extend")]
    ChunkTooLong,
    #[error("the sequence's next element is {expected}, not {got}")]
    OffsetGap { expected: u32, got: u32 },
    #[error("the sequence is past what a count holds")]
    SequenceTooLong,
    #[error("a span or cell starts before its item")]
    BeforeStart,
    #[error("the item ends before what it holds")]
    PastEnd,
    #[error("an end before its start")]
    EndBeforeStart,
    #[error("a value past its bounds")]
    ValueTooLong,
    /// Bits past a bit string's length, in its last byte, are set: equal strings would differ.
    #[error("a bit string's padding is set")]
    Padding,
    #[error("more than {MAX_LINKS} links")]
    TooManyLinks,
    /// A child starting before its parent.
    #[error("a child starts before its parent")]
    OutsideParent,
    /// A parent closing while a child is open.
    #[error("a parent closes while a child is open")]
    ChildOpen,
    /// A batch that leaves more items open than its analysis declares.
    #[error("{open} items open, past the {at_most} declared")]
    TooManyOpen { open: usize, at_most: usize },
}

/// Bookkeeping for an open item: enough to check its remaining ops, nothing more.
#[derive(Clone, Debug)]
struct Open {
    start: Tick,
    parent: Option<Id>,
    /// Children still open; a closed child's end counts towards `latest`.
    children: u32,
    /// The fields set, one bit each.
    fields: u32,
    latest: Tick,
    /// Each sequence's next offset, by its index in the kind's sequences.
    next: [u32; crate::MAX_SEQUENCES],
    links: usize,
    /// The batch that last backed it up, so a batch backs an item up once.
    stamp: u64,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Field {
    pub field: FieldId,
    pub value: Value,
    pub span: Option<Span>,
    pub driver: Option<Role>,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Item {
    pub id: Id,
    pub parent: Option<Id>,
    pub start: Tick,
    pub end: Option<Tick>,
    pub status: Option<Status>,
    pub fields: Vec<Field>,
    pub seqs: Vec<(SeqId, Vec<Element>)>,
    pub links: Vec<(LinkId, Ref)>,
}

impl Item {
    pub fn field(&self, f: FieldId) -> Option<&Field> {
        self.fields.iter().find(|x| x.field == f)
    }

    pub fn seq(&self, s: SeqId) -> &[Element] {
        self.seqs
            .iter()
            .find(|x| x.0 == s)
            .map_or(&[], |x| x.1.as_slice())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Counts {
    pub ops: u64,
    pub opens: u64,
    pub sets: u64,
    pub spanned_sets: u64,
    pub extends: u64,
    pub elements: u64,
    pub links: u64,
    pub closes: u64,
    pub incomplete: u64,
    pub peak_open: usize,
    /// Every op so far, in order, hashed: equal digests mean equal output. Stable across builds.
    pub digest: u64,
}

/// FNV-1a over bytes, and its step over whole words: a digest that stays the same across builds
/// and platforms.
struct Fnv(u64);

impl Fnv {
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x100_0000_01b3);
        }
    }
    /// A whole word in one step, rotated after the multiply so its high bits reach the low ones.
    fn u64(&mut self, v: u64) {
        self.0 = (self.0 ^ v).wrapping_mul(0x100_0000_01b3).rotate_left(29);
    }
    fn i64(&mut self, v: i64) {
        self.u64(v as u64);
    }
    fn id(&mut self, id: &Id) {
        self.i64(id.anchor);
        self.u64(u64::from(id.kind.0));
        self.u64(u64::from(id.disc));
    }
    fn span(&mut self, s: &Option<Span>) {
        match s {
            None => self.bytes(&[0]),
            Some(s) => {
                self.bytes(&[1]);
                self.i64(s.start);
                self.i64(s.end);
            }
        }
    }
    fn value(&mut self, v: &Value) {
        match v {
            Value::Phase => self.bytes(&[0]),
            Value::Bool(b) => self.bytes(&[1, u8::from(*b)]),
            Value::Word { value, bits } => {
                self.bytes(&[2, *bits]);
                self.u64(*value);
            }
            Value::Name(n) => {
                self.bytes(&[3]);
                self.u64(u64::from(*n));
            }
            Value::Text(t) => {
                self.bytes(&[4]);
                self.u64(t.len() as u64);
                self.bytes(t.as_bytes());
            }
            Value::Bits { bytes, bits } => {
                self.bytes(&[5]);
                self.u64(u64::from(*bits));
                self.bytes(bytes);
            }
            Value::Unknown { reason, needs } => {
                self.bytes(&[6]);
                self.u64(u64::from(reason.0));
                self.u64(needs.map_or(u64::MAX, |n| u64::from(n.0)));
            }
        }
    }
    fn op(&mut self, op: &Op) {
        self.bytes(&[op.letter() as u8]);
        self.id(op.id());
        match op {
            Op::Open { parent, start, .. } => {
                match parent {
                    None => self.bytes(&[0]),
                    Some(p) => {
                        self.bytes(&[1]);
                        self.id(p);
                    }
                }
                self.i64(*start);
            }
            Op::Set {
                field,
                value,
                span,
                driver,
                ..
            } => {
                self.u64(u64::from(field.0));
                self.value(value);
                self.span(span);
                self.u64(driver.map_or(u64::MAX, |d| u64::from(d.0)));
            }
            Op::Extend {
                seq,
                offset,
                elements,
                ..
            } => {
                self.u64(u64::from(seq.0));
                self.u64(u64::from(*offset));
                for e in elements {
                    self.bytes(&[e.value]);
                    match e.at {
                        crate::Observation::Wire(t) => {
                            self.bytes(&[0]);
                            self.i64(t);
                        }
                        crate::Observation::Upstream { id, seq, index } => {
                            self.bytes(&[1]);
                            self.id(&id);
                            self.u64(u64::from(seq.0));
                            self.u64(u64::from(index));
                        }
                    }
                    self.span(&Some(e.cell));
                }
            }
            Op::Link { link, other, .. } => {
                self.u64(u64::from(link.0));
                match other {
                    Ref::Own(o) => {
                        self.bytes(&[0]);
                        self.id(o);
                    }
                    Ref::Upstream(o) => {
                        self.bytes(&[1]);
                        self.id(o);
                    }
                }
            }
            Op::Close { end, status, .. } => {
                self.i64(*end);
                match status {
                    Status::Complete => self.bytes(&[0]),
                    Status::Incomplete(c) => {
                        self.bytes(&[1]);
                        self.u64(u64::from(c.0));
                    }
                }
            }
        }
    }
}

/// The open items as a batch changes them, in place: each item it touches is backed up once into
/// `undo` first, so a refused batch is put back without copying every open item.
struct Staged<'a> {
    open: &'a mut HashMap<Id, Open>,
    undo: &'a mut Vec<(Id, Option<Open>)>,
    batch: u64,
}

impl Staged<'_> {
    fn len(&self) -> usize {
        self.open.len()
    }

    fn contains(&self, id: &Id) -> bool {
        self.open.contains_key(id)
    }

    fn get_mut(&mut self, id: &Id) -> Option<&mut Open> {
        let o = self.open.get_mut(id)?;
        if o.stamp != self.batch {
            self.undo.push((*id, Some(o.clone())));
            o.stamp = self.batch;
        }
        Some(o)
    }

    fn insert(&mut self, id: Id, mut o: Open) {
        o.stamp = self.batch;
        self.undo.push((id, None));
        self.open.insert(id, o);
    }

    fn remove(&mut self, id: &Id) -> Option<Open> {
        self.get_mut(id)?;
        self.open.remove(id)
    }

    /// Puts back what the batch changed.
    fn rollback(&mut self) {
        while let Some((id, was)) = self.undo.pop() {
            match was {
                Some(o) => {
                    self.open.insert(id, o);
                }
                None => {
                    self.open.remove(&id);
                }
            }
        }
    }
}

/// The most ids a store's change log holds. A reader further behind than this reads the items
/// again in full.
pub const CHANGES_AT_MOST: usize = 1 << 18;

/// How far a store's changes had gone when a reader last took them in. Only the store it came
/// from answers it: another store, a clone included, has changes of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revision {
    store: u64,
    at: u64,
}

static STORES: AtomicU64 = AtomicU64::new(0);

/// The ids a store's changes touched, oldest first, and the store's identity. A clone starts a
/// log of its own under a new identity: a checkpoint's copy, restored, is a store no reader has
/// read.
#[derive(Debug)]
struct Log {
    store: u64,
    ids: Vec<Id>,
    /// The revision before `ids[0]`.
    from: u64,
    /// Where the batch under way logged its first id, so an item that several of its ops touch
    /// one after another is logged once. No reader looks in during a batch.
    batch: usize,
}

impl Default for Log {
    fn default() -> Self {
        Self {
            store: STORES.fetch_add(1, Ordering::Relaxed),
            ids: Vec::new(),
            from: 0,
            batch: 0,
        }
    }
}

impl Clone for Log {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl Log {
    fn revision(&self) -> Revision {
        Revision {
            store: self.store,
            at: self.from + self.ids.len() as u64,
        }
    }

    fn start_batch(&mut self) {
        self.batch = self.ids.len();
    }

    fn touch(&mut self, id: Id) {
        if self.ids.len() > self.batch && self.ids.last() == Some(&id) {
            return;
        }
        self.ids.push(id);
        if self.ids.len() > CHANGES_AT_MOST {
            let half = CHANGES_AT_MOST / 2;
            self.ids.drain(..half);
            self.from += half as u64;
            self.batch = self.batch.saturating_sub(half);
        }
    }
}

/// Checks ops and keeps items: every open one, its fields and links, so that a checkpoint carries
/// what a later window needs of it, and once closed those that meet a retained span, and in a
/// host's store those that end past where the host has passed it. An item keeps the elements
/// whose cells meet a retained span.
#[derive(Clone, Debug, Default)]
pub struct Store {
    open: HashMap<Id, Open>,
    items: BTreeMap<Id, Item>,
    log: Log,
    pub keep: Vec<Span>,
    pub counts: Counts,
    /// Closed items that meet no retained span, by end, held until `pass` goes past their ends: a
    /// checkpoint taken meanwhile needs an item that ends past the input it had consumed.
    ahead: BTreeSet<(Tick, Id)>,
    /// Where `pass` last went. Until a host passes the store, such an item is dropped at once.
    passed: Option<Tick>,
    /// What the batch under way changed, to put back if it is refused.
    undo: Vec<(Id, Option<Open>)>,
    batch: u64,
}

impl Store {
    pub fn with_keep(keep: Vec<Span>) -> Self {
        Self {
            keep,
            ..Self::default()
        }
    }

    /// Every item held: those open, and those closed that it keeps.
    pub fn items(&self) -> &BTreeMap<Id, Item> {
        &self.items
    }

    /// How far its changes have gone, for a reader to ask `changed_since` later.
    pub fn revision(&self) -> Revision {
        self.log.revision()
    }

    /// The ids of the items changed since `since`, oldest first: opened, given a field, elements
    /// or a link, closed, or dropped. An id comes once for each batch that touched it, or more.
    /// Read each in `items`: one not there was dropped. None where `since` is another store's, or
    /// further back than the log holds (`CHANGES_AT_MOST`): read the items again in full.
    pub fn changed_since(&self, since: Revision) -> Option<&[Id]> {
        if since.store != self.log.store || since.at < self.log.from {
            return None;
        }
        let k = usize::try_from(since.at - self.log.from).ok()?;
        self.log.ids.get(k..)
    }

    /// Keeps only the items `keep` holds to, logging each one dropped.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&Item) -> bool) {
        self.log.start_batch();
        let log = &mut self.log;
        self.items.retain(|id, i| {
            let kept = keep(i);
            if !kept {
                log.touch(*id);
            }
            kept
        });
    }

    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    /// Where the earliest open item starts: nothing before it can still change.
    pub fn earliest_open(&self) -> Option<Tick> {
        self.open.values().map(|o| o.start).min()
    }

    /// About how many bytes the store holds, for a checkpoint budget: its open items, and its
    /// kept items with their fields, elements and links. An estimate, not an exact figure.
    pub fn bytes(&self) -> usize {
        use std::mem::size_of;
        let open = self.open.capacity() * (size_of::<Id>() + size_of::<Open>());
        let items: usize = (self.items.values())
            .map(|i| {
                let seqs: usize = (i.seqs.iter())
                    .map(|(_, e)| {
                        size_of::<(SeqId, Vec<Element>)>() + e.capacity() * size_of::<Element>()
                    })
                    .sum();
                size_of::<(Id, Item)>()
                    + i.fields.capacity() * size_of::<Field>()
                    + i.links.capacity() * size_of::<(LinkId, Ref)>()
                    + seqs
            })
            .sum();
        let log = self.log.ids.capacity() * size_of::<Id>();
        size_of::<Self>() + open + items + log + self.keep.capacity() * size_of::<Span>()
    }

    /// Drops the closed items held past the retained spans whose ends are at or before `to`, and
    /// holds those that close from now on until a pass goes past their ends.
    pub(crate) fn pass(&mut self, to: Tick) {
        self.passed = Some(to);
        self.log.start_batch();
        while let Some(&(end, id)) = self.ahead.first() {
            if end > to {
                break;
            }
            self.ahead.pop_first();
            let keep = &self.keep;
            if (self.items.get(&id)).is_some_and(|i| !keep.iter().any(|s| meets(i.start, end, s))) {
                self.items.remove(&id);
                self.log.touch(id);
            }
        }
    }

    /// Applies a batch whole or not at all. Every op is checked against the open items as the
    /// batch leaves them so far before any is recorded. The whole batch is refused, at the op that
    /// does it, if it ever holds more than `at_most` items open. Returns the index of the first
    /// refused op.
    pub fn apply_batch(&mut self, ops: &[Op], at_most: usize) -> Result<(), (usize, Refused)> {
        self.batch = self.batch.wrapping_add(1);
        self.undo.clear();
        let mut staged = Staged {
            open: &mut self.open,
            undo: &mut self.undo,
            batch: self.batch,
        };
        let mut peak = staged.len();
        for (k, op) in ops.iter().enumerate() {
            let refused = match Self::check(&mut staged, &self.items, op) {
                Err(why) => Some(why),
                Ok(()) if staged.len() > at_most => Some(Refused::TooManyOpen {
                    open: staged.len(),
                    at_most,
                }),
                Ok(()) => None,
            };
            if let Some(why) = refused {
                staged.rollback();
                return Err((k, why));
            }
            peak = peak.max(staged.len());
        }
        self.undo.clear();
        self.log.start_batch();
        for op in ops {
            self.record(op);
        }
        self.counts.peak_open = self.counts.peak_open.max(peak);
        Ok(())
    }

    fn check(staged: &mut Staged, items: &BTreeMap<Id, Item>, op: &Op) -> Result<(), Refused> {
        match op {
            Op::Open { id, parent, start } => {
                if staged.contains(id) || items.contains_key(id) {
                    return Err(Refused::Reopened);
                }
                if let Some(p) = parent {
                    let p = staged.get_mut(p).ok_or(Refused::ParentNotOpen)?;
                    if *start < p.start {
                        return Err(Refused::OutsideParent);
                    }
                    p.children = p.children.saturating_add(1);
                }
                staged.insert(
                    *id,
                    Open {
                        start: *start,
                        parent: *parent,
                        children: 0,
                        fields: 0,
                        latest: *start,
                        next: [0; crate::MAX_SEQUENCES],
                        links: 0,
                        stamp: 0,
                    },
                );
            }
            Op::Set {
                id,
                field,
                value,
                span,
                ..
            } => {
                let o = staged.get_mut(id).ok_or(Refused::NotOpen)?;
                let bit = 1u32
                    .checked_shl(u32::from(field.0))
                    .filter(|_| usize::from(field.0) < MAX_FIELDS)
                    .ok_or(Refused::TooManyFields)?;
                if o.fields & bit != 0 {
                    return Err(Refused::FieldTwice);
                }
                match value {
                    Value::Bits { bytes, bits }
                        if *bits > MAX_BITS || bytes.len() != bits.div_ceil(8) as usize =>
                    {
                        return Err(Refused::ValueTooLong);
                    }
                    // Lowest bit first: the last byte's bits past the length are padding.
                    Value::Bits { bytes, bits }
                        if bits % 8 != 0 && bytes.last().is_some_and(|b| b >> (bits % 8) != 0) =>
                    {
                        return Err(Refused::Padding);
                    }
                    Value::Text(t) if t.len() > MAX_TEXT => return Err(Refused::ValueTooLong),
                    _ => {}
                }
                if let Some(s) = span {
                    if s.start < o.start {
                        return Err(Refused::BeforeStart);
                    }
                    if s.end < s.start {
                        return Err(Refused::EndBeforeStart);
                    }
                    o.latest = o.latest.max(s.end);
                }
                o.fields |= bit;
            }
            Op::Extend {
                id,
                seq,
                offset,
                elements,
            } => {
                let o = staged.get_mut(id).ok_or(Refused::NotOpen)?;
                if elements.len() > MAX_CHUNK {
                    return Err(Refused::ChunkTooLong);
                }
                let slot = o
                    .next
                    .get_mut(usize::from(seq.0))
                    .ok_or(Refused::SequenceTooLong)?;
                let next = *slot;
                if *offset != next {
                    return Err(Refused::OffsetGap {
                        expected: next,
                        got: *offset,
                    });
                }
                for e in elements {
                    if e.cell.start < o.start {
                        return Err(Refused::BeforeStart);
                    }
                    if e.cell.end < e.cell.start {
                        return Err(Refused::EndBeforeStart);
                    }
                    o.latest = o.latest.max(e.cell.end);
                }
                let after = next
                    .checked_add(elements.len() as u32)
                    .ok_or(Refused::SequenceTooLong)?;
                if let Some(slot) = o.next.get_mut(usize::from(seq.0)) {
                    *slot = after;
                }
            }
            Op::Link { id, .. } => {
                let o = staged.get_mut(id).ok_or(Refused::NotOpen)?;
                if o.links >= MAX_LINKS {
                    return Err(Refused::TooManyLinks);
                }
                o.links += 1;
            }
            Op::Close { id, end, .. } => {
                let o = staged.get_mut(id).ok_or(Refused::NotOpen)?;
                if *end < o.start {
                    return Err(Refused::EndBeforeStart);
                }
                if o.latest > *end {
                    return Err(Refused::PastEnd);
                }
                if o.children > 0 {
                    return Err(Refused::ChildOpen);
                }
                if let Some(p) = staged.remove(id).and_then(|o| o.parent)
                    && let Some(p) = staged.get_mut(&p)
                {
                    p.children = p.children.saturating_sub(1);
                    p.latest = p.latest.max(*end);
                }
            }
        }
        Ok(())
    }

    /// Records a checked op: counts it, keeps it where its item is retained, and logs its item.
    fn record(&mut self, op: &Op) {
        self.log.touch(*op.id());
        let c = &mut self.counts;
        c.ops += 1;
        let mut h = Fnv(c.digest ^ 0xcbf2_9ce4_8422_2325);
        h.op(op);
        c.digest = h.0;
        match op {
            Op::Open { id, parent, start } => {
                c.opens += 1;
                // Held while open, whatever the spans: a checkpoint taken now carries it, and a
                // window restored from that checkpoint keeps it if it meets the window.
                {
                    self.items.insert(
                        *id,
                        Item {
                            id: *id,
                            parent: *parent,
                            start: *start,
                            end: None,
                            status: None,
                            fields: vec![],
                            seqs: vec![],
                            links: vec![],
                        },
                    );
                }
            }
            Op::Set {
                id,
                field,
                value,
                span,
                driver,
            } => {
                c.sets += 1;
                if span.is_some() {
                    c.spanned_sets += 1;
                }
                if let Some(item) = self.items.get_mut(id) {
                    item.fields.push(Field {
                        field: *field,
                        value: value.clone(),
                        span: *span,
                        driver: *driver,
                    });
                }
            }
            Op::Extend {
                id, seq, elements, ..
            } => {
                c.extends += 1;
                c.elements += elements.len() as u64;
                let keep = &self.keep;
                if let Some(item) = self.items.get_mut(id) {
                    let kept = (elements.iter())
                        .filter(|e| keep.iter().any(|s| meets(e.cell.start, e.cell.end, s)));
                    match item.seqs.iter_mut().find(|(s, _)| s == seq) {
                        Some((_, v)) => v.extend(kept.copied()),
                        None => {
                            let v: Vec<Element> = kept.copied().collect();
                            if !v.is_empty() {
                                item.seqs.push((*seq, v));
                            }
                        }
                    }
                }
            }
            Op::Link { id, link, other } => {
                c.links += 1;
                if let Some(item) = self.items.get_mut(id) {
                    item.links.push((*link, *other));
                }
            }
            Op::Close { id, end, status } => {
                c.closes += 1;
                if matches!(status, Status::Incomplete(_)) {
                    c.incomplete += 1;
                }
                if let Some(item) = self.items.get_mut(id) {
                    item.end = Some(*end);
                    item.status = Some(*status);
                    if !self.keep.iter().any(|s| meets(item.start, *end, s)) {
                        match self.passed {
                            Some(p) if *end > p => {
                                self.ahead.insert((*end, *id));
                            }
                            _ => {
                                self.items.remove(id);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Whether an item over `start..end` meets span `s`. An item of no length meets a span it lies in.
fn meets(start: Tick, end: Tick, s: &Span) -> bool {
    start < s.end && s.start < end.max(start.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::KindId;

    #[test]
    fn an_item_is_kept_where_it_meets_a_kept_span_wherever_it_starts() {
        let span = Span {
            start: 100,
            end: 200,
        };
        let mut store = Store::with_keep(vec![span]);
        let item = |anchor| Id::new(KindId(0), anchor);
        let ops = |anchor: Tick, end: Tick| {
            [
                Op::Open {
                    id: item(anchor),
                    parent: None,
                    start: anchor,
                },
                Op::Close {
                    id: item(anchor),
                    end,
                    status: Status::Complete,
                },
            ]
        };
        for (start, end) in [
            (50, 120),
            (150, 160),
            (180, 250),
            (20, 300),
            (40, 90),
            (200, 210),
            (170, 170),
        ] {
            store.apply_batch(&ops(start, end), 8).unwrap();
        }
        let kept: Vec<Tick> = store.items.keys().map(|id| id.anchor).collect();
        assert_eq!(kept, [20, 50, 150, 170, 180]);
        let open = Op::Open {
            id: item(60),
            parent: None,
            start: 60,
        };
        store.apply_batch(&[open], 8).unwrap();
        assert!(
            store.items.contains_key(&item(60)),
            "open, it could still meet the span"
        );
    }

    #[test]
    fn a_bit_strings_padding_must_be_clear() {
        let id = Id::new(KindId(0), 0);
        let set = |bytes: Vec<u8>, bits| {
            let mut store = Store::default();
            let ops = [
                Op::Open {
                    id,
                    parent: None,
                    start: 0,
                },
                Op::Set {
                    id,
                    field: crate::FieldId(0),
                    value: Value::Bits { bytes, bits },
                    span: None,
                    driver: None,
                },
            ];
            store.apply_batch(&ops, 8).map_err(|e| e.1)
        };
        assert_eq!(set(vec![0x0F], 4), Ok(()));
        assert_eq!(set(vec![0x1F], 4), Err(Refused::Padding));
        assert_eq!(set(vec![0xFF, 0x01], 9), Ok(()));
        assert_eq!(set(vec![0xFF, 0x03], 9), Err(Refused::Padding));
        assert_eq!(set(vec![0xFF], 8), Ok(()), "a whole byte has none");
    }

    #[test]
    fn an_item_open_at_a_checkpoint_is_kept_by_a_window_restored_from_it() {
        let id = Id::new(KindId(0), 100);
        let mut forward = Store::with_keep(vec![]);
        forward.pass(Tick::MIN);
        forward
            .apply_batch(
                &[Op::Open {
                    id,
                    parent: None,
                    start: 100,
                }],
                8,
            )
            .unwrap();
        let mut window = forward.clone();
        window.keep = vec![Span {
            start: 150,
            end: 200,
        }];
        let set = Op::Set {
            id,
            field: crate::FieldId(0),
            value: Value::Bool(true),
            span: None,
            driver: None,
        };
        let close = Op::Close {
            id,
            end: 160,
            status: Status::Complete,
        };
        window
            .apply_batch(&[set.clone(), close.clone()], 8)
            .unwrap();
        assert!(
            window
                .items
                .get(&id)
                .is_some_and(|i| i.end == Some(160) && i.fields.len() == 1)
        );
        forward.apply_batch(&[set, close], 8).unwrap();
        forward.pass(159);
        assert!(
            forward.items.contains_key(&id),
            "held until the input passes its end"
        );
        forward.pass(160);
        assert!(
            forward.items.is_empty(),
            "closed, it meets no span of the forward pass"
        );
    }

    fn open(id: Id) -> Op {
        Op::Open {
            id,
            parent: None,
            start: id.anchor,
        }
    }

    fn close(id: Id, end: Tick) -> Op {
        Op::Close {
            id,
            end,
            status: Status::Complete,
        }
    }

    fn changed(store: &Store, since: Revision) -> Vec<Tick> {
        let ids = store.changed_since(since).expect("the log reaches back");
        ids.iter().map(|i| i.anchor).collect()
    }

    #[test]
    fn a_store_logs_each_item_a_batch_touches_and_each_it_drops() {
        let id = |anchor| Id::new(KindId(0), anchor);
        let mut store = Store::with_keep(vec![Span {
            start: 100,
            end: 200,
        }]);
        let before = store.revision();
        assert_eq!(changed(&store, before), Vec::<Tick>::new());
        let set = Op::Set {
            id: id(150),
            field: FieldId(0),
            value: Value::Bool(true),
            span: None,
            driver: None,
        };
        // Kept; then closed outside every kept span, and dropped at its close.
        let ops = [
            open(id(150)),
            set,
            close(id(150), 160),
            open(id(300)),
            close(id(300), 310),
        ];
        store.apply_batch(&ops, 8).unwrap();
        assert_eq!(
            changed(&store, before),
            [150, 300],
            "once each, though touched by several ops"
        );
        assert!(!store.items().contains_key(&id(300)));
        // Ops on two items interleaved log each again.
        let r = store.revision();
        let link = |at, other| Op::Link {
            id: id(at),
            link: LinkId(0),
            other: Ref::Own(id(other)),
        };
        store
            .apply_batch(&[open(id(170)), open(id(180)), link(170, 150)], 8)
            .unwrap();
        assert_eq!(changed(&store, r), [170, 180, 170]);
        // A refused batch changes nothing, so it logs nothing.
        let r = store.revision();
        store.apply_batch(&[close(id(150), 400)], 8).unwrap_err();
        assert_eq!(store.revision(), r);
        // Held for a checkpoint past where the host has passed it, then dropped by a later pass.
        store.pass(0);
        let r = store.revision();
        store
            .apply_batch(&[open(id(400)), close(id(400), 410)], 8)
            .unwrap();
        assert!(store.items().contains_key(&id(400)));
        assert_eq!(changed(&store, r), [400]);
        let r = store.revision();
        store.pass(500);
        assert!(!store.items().contains_key(&id(400)));
        assert_eq!(changed(&store, r), [400]);
        // A pass that drops nothing logs nothing; nor does dropping with nothing to drop.
        let r = store.revision();
        store.pass(600);
        store.retain(|_| true);
        assert_eq!(store.revision(), r);
        // What `retain` drops is logged.
        store
            .apply_batch(&[close(id(170), 175), close(id(180), 185)], 8)
            .unwrap();
        let r = store.revision();
        store.retain(|i| i.id != id(170) && i.id != id(150));
        assert_eq!(changed(&store, r), [150, 170]);
    }

    #[test]
    fn a_revision_is_answered_only_by_its_own_store() {
        let id = |anchor| Id::new(KindId(0), anchor);
        let mut store = Store::with_keep(vec![Span {
            start: 0,
            end: Tick::MAX,
        }]);
        store.apply_batch(&[open(id(1))], 8).unwrap();
        let read = store.revision();
        let mut copy = store.clone();
        assert_eq!(copy.items().len(), 1, "a clone holds the same items");
        assert!(
            copy.changed_since(read).is_none(),
            "a clone is another store"
        );
        assert_eq!(copy.changed_since(copy.revision()), Some(&[][..]));
        copy.apply_batch(&[open(id(2))], 8).unwrap();
        assert!(store.changed_since(copy.revision()).is_none());
        assert!(Store::default().changed_since(read).is_none());
        assert_eq!(changed(&store, read), Vec::<Tick>::new());
    }

    #[test]
    fn a_reader_further_back_than_the_log_holds_reads_the_items_again() {
        let id = |anchor| Id::new(KindId(0), anchor);
        let mut store = Store::with_keep(vec![Span {
            start: 0,
            end: Tick::MAX,
        }]);
        let first = store.revision();
        let n = CHANGES_AT_MOST as Tick + 1;
        let ops: Vec<Op> = (0..n)
            .flat_map(|k| [open(id(k)), close(id(k), k)])
            .collect();
        store.apply_batch(&ops, 8).unwrap();
        assert!(store.changed_since(first).is_none());
        let r = store.revision();
        store
            .apply_batch(&[open(id(n)), close(id(n), n)], 8)
            .unwrap();
        assert_eq!(changed(&store, r), [n]);
        let logged = store.log.ids.len();
        assert!(logged <= CHANGES_AT_MOST, "{logged} ids logged");
    }
}
