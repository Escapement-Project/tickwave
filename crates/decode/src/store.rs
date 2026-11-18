//! One analysis's output as the host admits it: the open items, checked op by op against their
//! lifecycle, and whole items kept only where they meet a span a view asked for.

use crate::output::{
    Element, FieldId, Id, LinkId, MAX_BITS, MAX_CHUNK, MAX_FIELDS, MAX_LINKS, MAX_TEXT, Op, Ref,
    Role, SeqId, Span, Status, Tick, Value,
};
use std::collections::{BTreeMap, HashMap};

/// Why an operation breaks its item's lifecycle.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
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
    /// Each sequence's next offset.
    next: Vec<(SeqId, u32)>,
    links: usize,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub field: FieldId,
    pub value: Value,
    pub span: Option<Span>,
    pub driver: Option<Role>,
}

#[derive(Clone, Debug)]
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

/// FNV-1a, 64 bits: a digest that stays the same across builds and platforms.
struct Fnv(u64);

impl Fnv {
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x100_0000_01b3);
        }
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.bytes(&v.to_le_bytes());
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

/// The open items a batch has changed so far, over the store's: a batch is checked against them
/// before any op is recorded, without copying every open item.
struct Staged<'a> {
    base: &'a HashMap<Id, Open>,
    /// An item the batch changed, or None where it closed one.
    changed: HashMap<Id, Option<Open>>,
    open: usize,
}

impl Staged<'_> {
    fn contains(&self, id: &Id) -> bool {
        match self.changed.get(id) {
            Some(o) => o.is_some(),
            None => self.base.contains_key(id),
        }
    }

    fn get_mut(&mut self, id: &Id) -> Option<&mut Open> {
        if !self.changed.contains_key(id) {
            let o = self.base.get(id)?.clone();
            self.changed.insert(*id, Some(o));
        }
        self.changed.get_mut(id)?.as_mut()
    }

    fn insert(&mut self, id: Id, o: Open) {
        self.changed.insert(id, Some(o));
        self.open += 1;
    }

    fn remove(&mut self, id: &Id) -> Option<Open> {
        let o = self.get_mut(id)?.clone();
        self.changed.insert(*id, None);
        self.open -= 1;
        Some(o)
    }
}

/// Checks ops and keeps whole items only where they meet a retained span.
#[derive(Clone, Debug, Default)]
pub struct Store {
    open: HashMap<Id, Open>,
    pub items: BTreeMap<Id, Item>,
    pub keep: Vec<Span>,
    pub counts: Counts,
}

impl Store {
    pub fn with_keep(keep: Vec<Span>) -> Self {
        Self {
            keep,
            ..Self::default()
        }
    }

    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    /// Where the earliest open item starts: nothing before it can still change.
    pub fn earliest_open(&self) -> Option<Tick> {
        self.open.values().map(|o| o.start).min()
    }

    /// Applies a batch whole or not at all. Every op is checked against the open items as the
    /// batch leaves them so far before any is recorded. The whole batch is refused, at the op that
    /// does it, if it ever holds more than `at_most` items open. Returns the index of the first
    /// refused op.
    pub fn apply_batch(&mut self, ops: &[Op], at_most: usize) -> Result<(), (usize, Refused)> {
        let mut staged = Staged {
            base: &self.open,
            changed: HashMap::new(),
            open: self.open.len(),
        };
        let mut peak = staged.open;
        for (k, op) in ops.iter().enumerate() {
            Self::check(&mut staged, &self.items, op).map_err(|why| (k, why))?;
            if staged.open > at_most {
                let open = staged.open;
                return Err((k, Refused::TooManyOpen { open, at_most }));
            }
            peak = peak.max(staged.open);
        }
        for (id, o) in staged.changed {
            match o {
                Some(o) => {
                    self.open.insert(id, o);
                }
                None => {
                    self.open.remove(&id);
                }
            }
        }
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
                        next: vec![],
                        links: 0,
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
                let at = match o.next.iter().position(|(s, _)| s == seq) {
                    Some(k) => k,
                    None => {
                        o.next.push((*seq, 0));
                        o.next.len() - 1
                    }
                };
                let next = o.next[at].1;
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
                o.next[at].1 = next
                    .checked_add(elements.len() as u32)
                    .ok_or(Refused::SequenceTooLong)?;
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

    /// Records a checked op: counts it, and keeps it where its item is retained.
    fn record(&mut self, op: &Op) {
        let c = &mut self.counts;
        c.ops += 1;
        let mut h = Fnv(c.digest ^ 0xcbf2_9ce4_8422_2325);
        h.op(op);
        c.digest = h.0;
        match op {
            Op::Open { id, parent, start } => {
                c.opens += 1;
                if self
                    .keep
                    .iter()
                    .any(|s| s.start <= *start && *start < s.end)
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
                if let Some(item) = self.items.get_mut(id) {
                    match item.seqs.iter_mut().find(|(s, _)| s == seq) {
                        Some((_, v)) => v.extend_from_slice(elements),
                        None => item.seqs.push((*seq, elements.clone())),
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
                }
            }
        }
    }
}
