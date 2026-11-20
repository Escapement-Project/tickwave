//! A decode track: one analysis's items drawn under a graph, on the graph's time axis.
//!
//! Its rows are reserved from the first frame and keep their height: the rows its schema's kinds
//! declare, each with its lanes, and where asked for, a row of their parts and bits. Zoomed out,
//! where items would crowd, it draws how many start in each pixel column from a `Density` ladder
//! instead, so a frame's work follows the columns on screen, not the capture. Conditions (an item
//! closed incomplete, a value unknown, anything the host marks) show at every zoom. Lost input is
//! shaded as the waveform's lost time is, input not decoded yet is hatched, and decoded input with
//! nothing in it is left bare, so the three differ without hue.

use egui::{Align2, Rect, Response, Sense, Shape, Stroke, Ui, Widget, pos2, vec2};
use escapement_tickwave_decode::{
    Closed, Element, FieldId, FieldType, Id, Item, KindId, Label, LabelPiece, Notation, Schema,
    Span, Status, Tick, Value,
};
use escapement_tickwave_view::columns::Grid;
use escapement_tickwave_view::format::annotations::Role;
use escapement_tickwave_view::format::mapping::Mapping;
use std::collections::BTreeMap;

use crate::graph::{Driven, Shown};
use crate::palette::Palette;

/// A row's height, as a lane's.
pub const ROW: f32 = 24.0;
/// A provisional item's dashed outline: its dashes and the gaps between them, in points.
const DASH: f32 = crate::draw::PROVISIONAL_DASH[0];
const GAP: f32 = crate::draw::PROVISIONAL_DASH[1];
/// Items narrower than this, on average, are drawn as density.
const CROWDED: f32 = 3.0;
/// A sequence's elements get their values once a cell is this wide.
const BIT: f32 = 9.0;
const PAD: f32 = 3.0;
const HATCH: f32 = 6.0;

/// Labels a host gives over those the schema declares, by kind, fullest first: a track draws the
/// first that fits its item, else none.
#[derive(Clone, Debug, Default)]
pub struct Labels {
    by_kind: Vec<(KindId, Vec<Label>)>,
}

impl Labels {
    /// `kind`'s labels in place of its schema's.
    pub fn kind(mut self, kind: KindId, labels: Vec<Label>) -> Self {
        self.by_kind.push((kind, labels));
        self
    }

    fn of(&self, kind: KindId) -> Option<&[Label]> {
        (self.by_kind.iter())
            .find(|(k, _)| *k == kind)
            .map(|(_, l)| l.as_slice())
    }
}

/// A field's value as a label shows it: a word in hex as wide as its bits, a name by its
/// declared name, a bool as 1 or 0, an unknown one as a question mark.
pub fn value_text(schema: &Schema, kind: KindId, field: FieldId, value: &Value) -> String {
    let decl = schema
        .kind(kind)
        .and_then(|k| k.fields.get(usize::from(field.0)));
    let notation = decl.and_then(|d| d.notation).unwrap_or(Notation::Hex);
    match value {
        Value::Word { value, bits } => match notation {
            Notation::Decimal => value.to_string(),
            Notation::Binary => format!("0b{value:0width$b}", width = usize::from(*bits).max(1)),
            _ => {
                let digits = usize::from(bits.div_ceil(4)).max(1);
                format!("0x{value:0digits$X}")
            }
        },
        Value::Name(n) => match decl.map(|d| &d.ty) {
            Some(FieldType::Name(list)) => list
                .get(usize::from(*n))
                .map_or_else(|| "?".into(), |s| s.to_string()),
            _ => "?".into(),
        },
        Value::Bool(b) => u8::from(*b).to_string(),
        Value::Text(t) => t.clone(),
        Value::Bits { bytes, bits } if notation == Notation::Binary => (0..*bits as usize)
            .rev()
            .map(|k| bytes.get(k / 8).is_some_and(|b| b >> (k % 8) & 1 == 1))
            .map(|bit| if bit { '1' } else { '0' })
            .collect(),
        Value::Bits { bytes, .. } => bytes.iter().rev().map(|b| format!("{b:02X}")).collect(),
        Value::Phase => String::new(),
        _ => "?".into(),
    }
}

fn label(schema: &Schema, item: &Item, t: &Label) -> String {
    let mut out = String::new();
    for p in &t.0 {
        match p {
            LabelPiece::Text(s) => out.push_str(s),
            LabelPiece::Field(f) => {
                if let Some(x) = item.field(*f) {
                    out.push_str(&value_text(schema, item.id.kind, *f, &x.value));
                }
            }
            // A piece newer than this view: left out.
            _ => {}
        }
    }
    out.trim().to_string()
}

/// The fullest label, from `labels` else the item's kind, that is not empty and holds no value the
/// item could not know; else its kind's name.
pub(crate) fn heading(schema: &Schema, labels: &Labels, item: &Item) -> String {
    let kind = schema.kind(item.id.kind);
    let declared = kind.map_or(&[][..], |k| &k.labels[..]);
    let known = |t: &&Label| {
        !(t.0.iter()).any(|p| match p {
            LabelPiece::Field(f) => item
                .field(*f)
                .is_some_and(|x| matches!(x.value, Value::Unknown { .. })),
            _ => false,
        })
    };
    (labels.of(item.id.kind).unwrap_or(declared).iter())
        .filter(known)
        .map(|t| label(schema, item, t))
        .find(|t| !t.is_empty())
        .unwrap_or_else(|| kind.map_or_else(|| "?".into(), |k| k.name.to_string()))
}

/// Whether an item is a condition: closed incomplete, or holding a value it could not know.
pub fn condition(item: &Item) -> bool {
    matches!(item.status, Some(Status::Incomplete(_)))
        || (item.fields.iter()).any(|f| matches!(f.value, Value::Unknown { .. }))
}

/// The height a track of `schema` takes, its parts row with `parts`: `Track::height` before there
/// is a track.
pub(crate) fn track_height(schema: &Schema, parts: bool) -> f32 {
    (schema.layout().0 + usize::from(parts)) as f32 * ROW
}

/// `condition`, of an item as it closed.
pub fn closed_condition(c: &Closed) -> bool {
    matches!(c.status, Status::Incomplete(_)) || c.unknown
}

/// The parts of the items in `window` (axis ticks), each by the role that drives it, its index
/// in the schema's roles as `decoder_roles` gives them: what a graph's `decoded_paint` takes to
/// draw a wire by who drives it. Empty where a track `width` points wide draws the window as
/// density, since its parts are then under a pixel.
pub fn driver_paints(
    schema: &Schema,
    items: &std::collections::BTreeMap<Id, Item>,
    density: &Density,
    map: &Mapping,
    window: &std::ops::Range<i64>,
    width: f32,
) -> Vec<Driven> {
    let mut out = Vec::new();
    if density.crowded(window, width) {
        return out;
    }
    let axis = |t: Tick| map.axis_saturating(t);
    for item in density.meeting(items, map, window, Tick::MAX) {
        for f in &item.fields {
            let (Some(span), Some(d)) = (f.span, f.driver) else {
                continue;
            };
            if usize::from(d.0) >= schema.roles.len() {
                continue;
            }
            let span = axis(span.start)..axis(span.end);
            if span.end <= window.start || span.start >= window.end {
                continue;
            }
            out.push(Driven { role: d.0, span });
        }
    }
    out
}

/// The roles a decoder declares, as a graph takes them: named, labelled with their names, with
/// no look of their own, so the host's or the sidecar's look for the name draws them.
pub fn decoder_roles(schema: &Schema) -> Vec<Role> {
    (schema.roles.iter())
        .map(|r| Role {
            name: r.to_string(),
            label: r.to_string(),
            colour: None,
            weight: 0.0,
            dash: None,
        })
        .collect()
}

/// One level of a density ladder: counts of items starting in each bucket of `2^shift` axis
/// ticks, from bucket `first`, and how many of them are conditions.
#[derive(Clone, Debug)]
struct Level {
    shift: u8,
    first: i64,
    counts: Vec<u32>,
    marked: Vec<u32>,
}

impl Level {
    /// Widens the level to hold buckets `lo..=hi`.
    fn cover(&mut self, lo: i64, hi: i64) {
        if lo < self.first {
            let n = (self.first - lo) as usize;
            self.counts.splice(0..0, std::iter::repeat_n(0, n));
            self.marked.splice(0..0, std::iter::repeat_n(0, n));
            self.first = lo;
        }
        let len = (hi - self.first + 1) as usize;
        if len > self.counts.len() {
            self.counts.resize(len, 0);
            self.marked.resize(len, 0);
        }
    }

    /// The level above it: each bucket the sum of two of its own.
    fn above(&self) -> Self {
        let first = self.first >> 1;
        let len = ((self.first + self.counts.len() as i64 - 1) >> 1) - first + 1;
        let mut next = Self {
            shift: self.shift + 1,
            first,
            counts: vec![0; len as usize],
            marked: vec![0; len as usize],
        };
        for (k, (&c, &m)) in self.counts.iter().zip(&self.marked).enumerate() {
            let j = (((self.first + k as i64) >> 1) - first) as usize;
            next.counts[j] = next.counts[j].saturating_add(c);
            next.marked[j] = next.marked[j].saturating_add(m);
        }
        next
    }
}

/// An item a density holds apart, with its span on the axis: a long one, or one open when it was
/// counted. `marked` says whether it was counted as a condition.
#[derive(Clone, Copy, Debug)]
struct Held {
    id: Id,
    start: i64,
    end: Option<i64>,
    marked: bool,
}

/// Which items, by anchor, a density has counted.
#[derive(Clone, Copy, Debug, Default)]
enum Counted {
    #[default]
    None,
    Before(Tick),
    All,
}

/// How many items start where, on the axis, at a ladder of bucket sizes: what a zoomed-out
/// track draws. It grows with its store (`grow`), counting each item once, so a frame costs
/// nothing past the columns drawn and a snapshot only what it added.
#[derive(Clone, Debug)]
pub struct Density {
    levels: Vec<Level>,
    /// The first and last start counted, on the axis.
    lo: i64,
    hi: i64,
    /// The longest closed item, in axis ticks.
    longest: i64,
    items: u64,
    /// Where a window finds its items: closed ones shorter than `2^(long - 1)` ticks start at
    /// most `reach` before it, and the rest are held in `held`, in id order, so a few long items
    /// do not make every window walk back as far as they reach. `long` only rises: lowering it
    /// would need the spans of short items, which are not kept.
    long: u32,
    reach: i64,
    held: Vec<Held>,
    /// Closed items counted, by length class: what `long` is chosen from.
    classes: [u64; 65],
    counted: Counted,
}

/// The most buckets the finest level holds.
const FINEST: usize = 1 << 20;

/// The most items a density holds apart as long.
const LONG_AT_MOST: u64 = 1 << 16;

/// An item's length class: lengths from `2^(k-1)` up to `2^k` are class `k`.
fn class(len: i64) -> u32 {
    64 - len.max(0).leading_zeros()
}

/// The class from which items are held apart: the one that makes a window's walk least, about the
/// items starting within reach of it plus every held one, with `classes` closed and `open` open
/// over `span` axis ticks.
fn pick_long(classes: &[u64; 65], open: u64, span: u128) -> u32 {
    let n = classes.iter().sum::<u64>() + open;
    let per_tick = n as f64 / span.max(1) as f64;
    let (mut long, mut least, mut above) = (65u32, f64::INFINITY, open);
    for k in (0..=65u32).rev() {
        if k < 65 {
            above += classes[k as usize];
        }
        if above > LONG_AT_MOST {
            break;
        }
        let reach = if k == 0 { 0.0 } else { 2f64.powi(k as i32 - 1) };
        let cost = per_tick * 2.0 * reach + above as f64;
        if cost < least {
            (long, least) = (k, cost);
        }
    }
    long
}

impl Default for Density {
    fn default() -> Self {
        Self {
            levels: vec![],
            lo: 0,
            hi: 0,
            longest: 0,
            items: 0,
            long: 0,
            reach: 0,
            held: vec![],
            classes: [0; 65],
            counted: Counted::None,
        }
    }
}

impl Density {
    /// The ladder of `items`, placed on the axis by `map`, with `marked` saying which are
    /// conditions. It counts every item there, so `grow` only settles those still open.
    pub fn of(items: &BTreeMap<Id, Item>, map: &Mapping, marked: impl Fn(&Item) -> bool) -> Self {
        let mut d = Self::default();
        d.take(items, map, &marked, std::ops::Bound::Unbounded);
        d
    }

    /// Counts the items of `items`, the store it grows with, that it has not counted and whose
    /// anchors lie before `frontier`, and settles those it counted open that have closed since.
    /// `frontier` is the host's start frontier for the analysis: no item starts before it from
    /// then on, and an item's anchor lies at or after its start, so every item anchored before it
    /// is in the store already. One that has left the store uncounted, as an unkept closed item
    /// does, is not counted.
    pub fn grow(
        &mut self,
        items: &BTreeMap<Id, Item>,
        map: &Mapping,
        marked: impl Fn(&Item) -> bool,
        frontier: Tick,
    ) {
        self.take(items, map, &marked, std::ops::Bound::Excluded(frontier));
    }

    fn take(
        &mut self,
        items: &BTreeMap<Id, Item>,
        map: &Mapping,
        marked: &dyn Fn(&Item) -> bool,
        upto: std::ops::Bound<Tick>,
    ) {
        use std::ops::Bound;
        let axis = |t: Tick| map.axis_saturating(t);
        self.settle(items, &axis, marked);
        let from = match self.counted {
            Counted::None => Bound::Unbounded,
            Counted::Before(t) => Bound::Included(Id::new(KindId(0), t)),
            Counted::All => return,
        };
        let to = match upto {
            Bound::Excluded(t) => Bound::Excluded(Id::new(KindId(0), t)),
            _ => Bound::Unbounded,
        };
        if let (Bound::Included(a), Bound::Excluded(b)) = (from, to)
            && b <= a
        {
            return;
        }
        let new = || items.range((from, to)).map(|(_, i)| i);
        // The cut between short and long, over what is counted and what comes now.
        let (mut classes, mut open) = (self.classes, self.open());
        let (mut lo, mut hi) = (self.lo, self.hi);
        let mut any = self.items > 0;
        for i in new() {
            let start = axis(i.start);
            (lo, hi) = if any {
                (lo.min(start), hi.max(start))
            } else {
                (start, start)
            };
            any = true;
            match i.end {
                Some(e) => classes[class(axis(e).saturating_sub(start)) as usize] += 1,
                None => open += 1,
            }
        }
        if any {
            let span = hi.saturating_sub(lo) as u128;
            self.raise_long(pick_long(&classes, open, span));
            (self.lo, self.hi) = (lo, hi);
            self.widen();
            // Counted at the finest level, then carried up over the buckets they touched.
            let (mut a, mut b) = (i64::MAX, i64::MIN);
            for i in new() {
                let at = self.add(i, &axis, marked);
                (a, b) = (a.min(at), b.max(at));
            }
            if a <= b {
                self.carry(a, b);
            }
        }
        self.counted = match upto {
            Bound::Excluded(t) => Counted::Before(t),
            _ => Counted::All,
        };
    }

    /// Counts the items a forward run tallied as they closed (`Host::tally`), each once: the
    /// density of a pass that keeps no items, which a zoomed-out track draws. It finds no items in
    /// a store, so a zoomed-in track takes the density of the store it draws.
    pub fn tally(&mut self, closed: &[Closed], map: &Mapping) {
        if closed.is_empty() {
            return;
        }
        let axis = |t: Tick| map.axis_saturating(t);
        let mut classes = self.classes;
        let (mut lo, mut hi) = (self.lo, self.hi);
        let mut any = self.items > 0;
        for c in closed {
            let start = axis(c.start);
            (lo, hi) = if any {
                (lo.min(start), hi.max(start))
            } else {
                (start, start)
            };
            any = true;
            classes[class(axis(c.end).saturating_sub(start)) as usize] += 1;
        }
        let span = hi.saturating_sub(lo) as u128;
        self.raise_long(pick_long(&classes, self.open(), span));
        (self.lo, self.hi) = (lo, hi);
        self.widen();
        let (mut a, mut b) = (i64::MAX, i64::MIN);
        for c in closed {
            let (start, end) = (axis(c.start), axis(c.end));
            let at = self.add_at(c.id, start, Some(end), closed_condition(c));
            (a, b) = (a.min(at), b.max(at));
        }
        self.carry(a, b);
        self.held.sort_unstable_by_key(|h| h.id);
        self.counted = Counted::All;
    }

    /// How many held items are open.
    fn open(&self) -> u64 {
        self.held.iter().filter(|h| h.end.is_none()).count() as u64
    }

    /// Holds items apart from class `long` up, where that is higher than now.
    fn raise_long(&mut self, long: u32) {
        if long <= self.long {
            return;
        }
        self.long = long;
        let mut reach = self.reach;
        self.held.retain(|h| match h.end {
            Some(e) if class(e.saturating_sub(h.start)) < long => {
                reach = reach.max(e.saturating_sub(h.start));
                false
            }
            _ => true,
        });
        self.reach = reach;
    }

    /// Counts item `i` at the finest level, which `widen` has made hold it, and returns its
    /// bucket there; the levels above owe a `carry`.
    fn add(
        &mut self,
        i: &Item,
        axis: &impl Fn(Tick) -> i64,
        marked: &dyn Fn(&Item) -> bool,
    ) -> i64 {
        self.add_at(i.id, axis(i.start), i.end.map(axis), marked(i))
    }

    /// Counts an item starting at `start` (on the axis), closed at `end` or open, as `add` does.
    fn add_at(&mut self, id: Id, start: i64, end: Option<i64>, m: bool) -> i64 {
        self.items += 1;
        let l = &mut self.levels[0];
        let at = start >> l.shift;
        let k = (at - l.first) as usize;
        if let (Some(c), Some(n)) = (l.counts.get_mut(k), l.marked.get_mut(k)) {
            *c = c.saturating_add(1);
            *n = n.saturating_add(u32::from(m));
        }
        let held = Held {
            id,
            start,
            end: None,
            marked: m,
        };
        match end {
            Some(e) => self.closed(held, e),
            None => self.held.push(held),
        }
        at
    }

    /// Item `h`, counted, closed at `end`: held if long, else within reach.
    fn closed(&mut self, h: Held, end: i64) {
        let len = end.saturating_sub(h.start);
        self.longest = self.longest.max(len);
        self.classes[class(len) as usize] += 1;
        if class(len) < self.long {
            self.reach = self.reach.max(len);
        } else {
            self.held.push(Held {
                end: Some(end),
                ..h
            });
        }
    }

    /// Settles the items it counted open that have closed since: their lengths, and whether they
    /// closed as conditions. One gone from the store closed unkept, its end unknown.
    fn settle(
        &mut self,
        items: &BTreeMap<Id, Item>,
        axis: &impl Fn(Tick) -> i64,
        marked: &dyn Fn(&Item) -> bool,
    ) {
        let open: Vec<Held> = (self.held.iter())
            .filter(|h| h.end.is_none())
            .copied()
            .collect();
        for h in open {
            let item = items.get(&h.id);
            if item.is_some_and(|i| i.end.is_none()) {
                continue;
            }
            self.held.retain(|x| x.id != h.id);
            let Some(i) = item else { continue };
            if !h.marked && marked(i) {
                self.mark(h.start, 1);
            }
            if let Some(e) = i.end {
                self.closed(h, axis(e));
            }
        }
        self.held.sort_unstable_by_key(|h| h.id);
    }

    /// Widens the ladder to the starts counted, `lo..=hi`: coarser levels on top until one holds
    /// them in two buckets, and the finest dropped while it would hold more than `FINEST`.
    fn widen(&mut self) {
        let (lo, hi) = (self.lo, self.hi);
        let need = |l: &Level| (hi >> l.shift) - (lo >> l.shift) + 1;
        if self.levels.is_empty() {
            self.levels.push(Level {
                shift: 0,
                first: lo,
                counts: vec![0],
                marked: vec![0],
            });
        }
        while let Some(top) = self.levels.last().filter(|l| need(l) > 2) {
            let next = top.above();
            self.levels.push(next);
        }
        while self.levels.len() > 1 && need(&self.levels[0]) > FINEST as i64 {
            self.levels.remove(0);
        }
        for l in &mut self.levels {
            l.cover(lo >> l.shift, hi >> l.shift);
        }
    }

    /// Sums the finest level's buckets `a..=b`, changed since, into the levels above.
    fn carry(&mut self, mut a: i64, mut b: i64) {
        for k in 1..self.levels.len() {
            (a, b) = (a >> 1, b >> 1);
            let (below, above) = self.levels.split_at_mut(k);
            let (from, to) = (&below[k - 1], &mut above[0]);
            let at = |j: i64| {
                let i = usize::try_from(j - from.first).ok()?;
                Some((*from.counts.get(i)?, *from.marked.get(i)?))
            };
            for j in a..=b {
                let (c, m) = [at(2 * j), at(2 * j + 1)]
                    .into_iter()
                    .flatten()
                    .fold((0u32, 0u32), |(c, m), (x, y)| {
                        (c.saturating_add(x), m.saturating_add(y))
                    });
                let i = (j - to.first) as usize;
                if let (Some(x), Some(y)) = (to.counts.get_mut(i), to.marked.get_mut(i)) {
                    (*x, *y) = (c, m);
                }
            }
        }
    }

    /// Adds `marked` conditions to every level at `at`, a start already counted.
    fn mark(&mut self, at: i64, marked: u32) {
        for l in &mut self.levels {
            let k = ((at >> l.shift) - l.first) as usize;
            if let Some(m) = l.marked.get_mut(k) {
                *m = m.saturating_add(marked);
            }
        }
    }

    /// How many items it counts.
    pub fn items(&self) -> u64 {
        self.items
    }

    /// Whether `window` (axis ticks) over `width` points is too crowded to draw items apart: the
    /// longest a few points wide, or more of them than the width shows apart.
    pub(crate) fn crowded(&self, window: &std::ops::Range<i64>, width: f32) -> bool {
        let per = (window.end - window.start) as f64 / f64::from(width);
        let (count, _) = self.count(window.start, window.end, 0.0);
        (self.longest as f64 / per) < f64::from(CROWDED) || count as f32 * CROWDED > width
    }

    /// The items of `items` (the store it grows with) that meet `window` (axis ticks), in id
    /// order, an open one reaching `settled`.
    fn meeting<'a>(
        &'a self,
        items: &'a BTreeMap<Id, Item>,
        map: &'a Mapping,
        window: &'a std::ops::Range<i64>,
        settled: Tick,
    ) -> impl Iterator<Item = &'a Item> + 'a {
        let axis = |t: Tick| map.axis_saturating(t);
        let meets = move |start: i64, end: Option<i64>| {
            start < window.end && end.unwrap_or(axis(settled)) >= window.start
        };
        let counted = move |id: &Id| match self.counted {
            Counted::None => false,
            Counted::Before(t) => id.anchor < t,
            Counted::All => true,
        };
        // An item's anchor lies at or after its start and within it.
        let lo = Id::new(
            KindId(0),
            window.start.saturating_sub(self.reach.saturating_add(1)),
        );
        let short = (items.range(lo..).map(|(_, i)| i))
            .take_while(move |i| counted(&i.id))
            .take_while(move |i| axis(i.start) < window.end.saturating_add(self.reach))
            .filter(move |i| {
                let start = axis(i.start);
                let end = i.end.map(axis);
                let short = end.is_some_and(|e| class(e.saturating_sub(start)) < self.long);
                short && meets(start, end)
            });
        // Items not yet counted, past where it has grown to: few, so each is tried.
        let tail = match self.counted {
            Counted::None => Some(items.range(..)),
            Counted::Before(t) => Some(items.range(Id::new(KindId(0), t)..)),
            Counted::All => None,
        };
        let tail = (tail.into_iter().flatten().map(|(_, i)| i))
            .filter(move |i| meets(axis(i.start), i.end.map(axis)));
        let long = (self.held.iter())
            .filter(move |h| meets(h.start, h.end))
            .filter_map(|h| items.get(&h.id));
        Merge {
            a: short.chain(tail).peekable(),
            b: long.peekable(),
        }
    }

    /// Items starting in `[from, to)` and how many are conditions, at the coarsest level whose
    /// buckets are no wider than `per`; with `per` 0, the finest.
    pub(crate) fn count(&self, from: i64, to: i64, per: f64) -> (u64, u64) {
        self.level(per)
            .map_or((0, 0), |at| self.count_at(at, from, to))
    }

    /// The coarsest level whose buckets are no wider than `per`, else the finest; none before a
    /// level is built. It depends on the scale alone, so a frame chooses it once.
    fn level(&self, per: f64) -> Option<usize> {
        (0..self.levels.len())
            .rev()
            .find(|&k| ((1u128 << self.levels[k].shift) as f64) <= per)
            .or((!self.levels.is_empty()).then_some(0))
    }

    /// As `count`, at level `at`.
    fn count_at(&self, at: usize, from: i64, to: i64) -> (u64, u64) {
        let shift = self.levels[at].shift;
        // Buckets `a..b` of that level, by their index from zero. A level's bucket is the sum of
        // two of the level below's, so each level takes the odd one at either end, and the rest
        // the level above holds.
        let (mut a, mut b) = (from >> shift, ((to - 1) >> shift) + 1);
        let (mut c, mut m) = (0u64, 0u64);
        for (k, l) in self.levels[at..].iter().enumerate() {
            (a, b) = (a.max(l.first), b.min(l.first + l.counts.len() as i64));
            if a >= b {
                break;
            }
            let i = |j: i64| (j - l.first) as usize;
            if at + k + 1 == self.levels.len() {
                c += l.counts[i(a)..i(b)]
                    .iter()
                    .map(|&x| u64::from(x))
                    .sum::<u64>();
                m += l.marked[i(a)..i(b)]
                    .iter()
                    .map(|&x| u64::from(x))
                    .sum::<u64>();
                break;
            }
            if a & 1 == 1 {
                c += u64::from(l.counts[i(a)]);
                m += u64::from(l.marked[i(a)]);
                a += 1;
            }
            if b & 1 == 1 {
                b -= 1;
                c += u64::from(l.counts[i(b)]);
                m += u64::from(l.marked[i(b)]);
            }
            (a, b) = (a >> 1, b >> 1);
        }
        (c, m)
    }
}

/// Two iterators of items in id order, merged in id order.
struct Merge<A: Iterator, B: Iterator> {
    a: std::iter::Peekable<A>,
    b: std::iter::Peekable<B>,
}

impl<'a, A: Iterator<Item = &'a Item>, B: Iterator<Item = &'a Item>> Iterator for Merge<A, B> {
    type Item = &'a Item;

    fn next(&mut self) -> Option<&'a Item> {
        match (self.a.peek(), self.b.peek()) {
            (Some(x), Some(y)) if y.id < x.id => self.b.next(),
            (Some(_), _) => self.a.next(),
            (None, _) => self.b.next(),
        }
    }
}

/// An analysis's items under a graph, on the window and plot the graph drew last.
pub struct Track<'a> {
    shown: &'a Shown,
    name: &'a str,
    schema: &'a Schema,
    items: &'a std::collections::BTreeMap<Id, Item>,
    density: &'a Density,
    labels: &'a Labels,
    palette: &'a Palette,
    map: Mapping,
    settled: Tick,
    lost: &'a [Span],
    parts: bool,
    provisional: bool,
    selected: Option<Id>,
    lit: Option<Span>,
    /// The lanes its rows take together, and each kind's range of them.
    lanes: usize,
    rows: Vec<std::ops::Range<usize>>,
}

impl<'a> Track<'a> {
    /// `items` are an analysis's, kept by its store, placed on the axis by `map` (its source's
    /// timeline mapping). `settled` is where decoding has reached; past it the track hatches.
    #[expect(clippy::too_many_arguments, reason = "each is the track's own")]
    pub fn new(
        shown: &'a Shown,
        name: &'a str,
        schema: &'a Schema,
        items: &'a std::collections::BTreeMap<Id, Item>,
        density: &'a Density,
        labels: &'a Labels,
        palette: &'a Palette,
        map: Mapping,
    ) -> Self {
        let (lanes, rows) = schema.layout();
        Self {
            shown,
            name,
            schema,
            items,
            density,
            labels,
            palette,
            map,
            settled: Tick::MAX,
            lost: &[],
            parts: false,
            provisional: false,
            selected: None,
            lit: None,
            lanes,
            rows,
        }
    }

    /// Where decoding has reached, on the analysis's timeline.
    pub fn settled(mut self, t: Tick) -> Self {
        self.settled = t;
        self
    }

    /// Where the analysis's input was lost, on its timeline, in order and apart, as a host holds
    /// them.
    pub fn lost(mut self, lost: &'a [Span]) -> Self {
        self.lost = lost;
        self
    }

    /// Whether a second row draws each item's spanned fields and their elements.
    pub fn parts(mut self, on: bool) -> Self {
        self.parts = on;
        self
    }

    /// The item the reader selected, outlined in ink as the focus is. The host keeps it, from
    /// `Track::clicked`.
    pub fn selected(mut self, id: Option<Id>) -> Self {
        self.selected = id;
        self
    }

    /// A span the reader points at elsewhere, on the analysis's timeline (`PanelState::lit`):
    /// washed across every row as the pointer's wash is, edged with hairlines.
    pub fn lit(mut self, span: Option<Span>) -> Self {
        self.lit = span;
        self
    }

    /// The item a click on the track with `response` landed on this frame, if one did.
    pub fn clicked(response: &Response) -> Option<Id> {
        response
            .ctx
            .data(|d| d.get_temp::<Option<Id>>(response.id))
            .flatten()
    }

    /// Whether the items are a provisional window's, decoded from a local start without earlier
    /// context (`Derived::provisional`). They draw at the palette's `pale` opacity, outlines
    /// dashed, and density over a dashed baseline: unlike input not decoded yet, which hatches.
    pub fn provisional(mut self, on: bool) -> Self {
        self.provisional = on;
        self
    }

    /// The height it takes: a lane's for each lane its schema's rows take, and a row for the parts
    /// where they are drawn. An item past its row's lanes at once draws as an overflow mark.
    pub fn height(&self) -> f32 {
        self.strips() as f32 * ROW
    }

    /// Its lanes, and the parts row where it has one.
    fn strips(&self) -> usize {
        self.lanes + usize::from(self.parts)
    }
}

/// The window's axis ticks across the plot's x range.
struct Scale {
    from: f64,
    per: f64,
    left: f32,
    right: f32,
}

impl Scale {
    fn x(&self, tick: f64) -> f32 {
        self.left + ((tick - self.from) / self.per) as f32
    }
}

impl Widget for Track<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let _span = tracing::debug_span!("track", name = self.name).entered();
        let p = self.palette;
        let height = self.height();
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
        let click = response
            .clicked()
            .then(|| response.interact_pointer_pos())
            .flatten();
        let mut clicked = None;
        let plot = Rect::from_min_max(
            pos2(self.shown.plot.start.max(rect.left()), rect.top()),
            pos2(self.shown.plot.end.min(rect.right()), rect.bottom()),
        );
        let painter = ui.painter_at(rect);
        let row = |k: usize| {
            Rect::from_min_size(
                pos2(plot.left(), rect.top() + k as f32 * ROW),
                vec2(plot.width(), ROW),
            )
        };
        // The name in the gutter, as a lane's.
        painter.text(
            pos2(plot.left() - 6.0, row(0).center().y),
            Align2::RIGHT_CENTER,
            self.name,
            p.labels.clone(),
            p.ink_2,
        );
        let w = &self.shown.window;
        if plot.width() < 1.0 || w.end <= w.start {
            return response;
        }
        let s = Scale {
            from: w.start as f64,
            per: (w.end - w.start) as f64 / f64::from(plot.width()),
            left: plot.left(),
            right: plot.right(),
        };
        let axis = |t: Tick| self.map.axis_saturating(t);
        let painter = ui.painter_at(plot);
        let mut shapes = Vec::new();
        // Not decoded yet: hatched, so it differs from decoded and quiet without hue.
        let settled = axis(self.settled);
        if settled < w.end {
            let x0 = s.x(settled as f64).max(plot.left());
            let wash = p.ink_2.gamma_multiply(p.pale * 0.25);
            shapes.push(Shape::rect_filled(
                Rect::from_min_max(pos2(x0, plot.top()), plot.right_bottom()),
                0.0,
                wash,
            ));
            let stroke = Stroke::new(1.0, p.hair);
            let mut x = x0 - plot.height();
            while x < plot.right() {
                shapes.push(Shape::line_segment(
                    [pos2(x, plot.bottom()), pos2(x + plot.height(), plot.top())],
                    stroke,
                ));
                x += HATCH;
            }
        }
        // Crowded where the longest item is a few points wide, or where the window holds more
        // items than its columns could show apart.
        let crowded = self.density.crowded(w, plot.width());
        let lanes: Vec<Rect> = (0..self.lanes).map(row).collect();
        let drawn = shapes.len();
        if crowded {
            let grid = bin_columns(self.shown, &s, ui.pixels_per_point());
            self.density_bins(&s, grid, lanes[0], &mut shapes);
        } else {
            let parts = self.parts.then(|| row(self.lanes));
            clicked = self.items(ui, &s, &lanes, parts, click, &mut shapes);
        }
        if self.provisional {
            let decoded = shapes.split_off(drawn);
            provisional(decoded, p.pale, &mut shapes);
            if crowded {
                let y = lanes[0].bottom() - PAD / 2.0;
                let line = [pos2(plot.left(), y), pos2(plot.right(), y)];
                let stroke = Stroke::new(1.0, p.ink_2.gamma_multiply(p.pale));
                shapes.extend(Shape::dashed_line(&line, stroke, DASH, GAP));
            }
        }
        // Lost input, across every row, as the waveform shades its lost time: only the spans the
        // window meets.
        let first = self.lost.partition_point(|l| axis(l.end) <= w.start);
        for l in &self.lost[first..] {
            let (a, b) = (axis(l.start), axis(l.end));
            if a >= w.end {
                break;
            }
            let x0 = s.x(a as f64).max(plot.left());
            let x1 = s.x(b as f64).min(plot.right()).max(x0 + 1.0);
            shapes.push(Shape::rect_filled(
                Rect::from_min_max(pos2(x0, plot.top()), pos2(x1, plot.bottom())),
                0.0,
                p.attention_fill.gamma_multiply(p.shade),
            ));
        }
        if let Some(l) = self.lit {
            let (a, b) = (axis(l.start), axis(l.end));
            if a < w.end && b >= w.start {
                lit(&s, plot, a, b, p, &mut shapes);
            }
        }
        painter.extend(shapes);
        ui.data_mut(|d| d.insert_temp(response.id, clicked));
        response
    }
}

/// A lit span from axis tick `a` to `b`: the pointer's wash, edged with hairlines so it reads
/// apart from a selection's.
fn lit(s: &Scale, plot: Rect, a: i64, b: i64, p: &Palette, shapes: &mut Vec<Shape>) {
    let x0 = s.x(a as f64).clamp(plot.left(), plot.right());
    let x1 = s.x(b as f64).clamp(plot.left(), plot.right()).max(x0 + 1.0);
    let band = Rect::from_min_max(pos2(x0, plot.top()), pos2(x1, plot.bottom()));
    shapes.push(Shape::rect_filled(band, 0.0, p.wash));
    let edge = Stroke::new(1.0, p.ink_2);
    for x in [x0, x1] {
        shapes.push(Shape::line_segment(
            [pos2(x, plot.top()), pos2(x, plot.bottom())],
            edge,
        ));
    }
}

/// The columns a density bins in, those meeting `shown`'s window: the lanes' where the graph drew
/// them at this scale, so a bin lies under the column it counts; else a point wide from the
/// window's start. An answer for another scale is drawn stretched until the next comes, and its
/// columns would bin too coarsely or too finely.
fn bin_columns(shown: &Shown, s: &Scale, pixels_per_point: f32) -> Grid {
    let w = &shown.window;
    let pixel = s.per / f64::from(pixels_per_point);
    let (x0, per) = match shown.grid {
        Some(g) if (0.5..=2.0).contains(&(g.per / pixel)) => (g.x0, g.per),
        _ => (s.from, s.per),
    };
    let first = ((w.start as f64 - x0) / per).floor();
    let last = ((w.end as f64 - x0) / per).ceil();
    Grid {
        x0: x0 + first * per,
        per,
        count: (last - first).max(0.0) as usize,
    }
}

impl Track<'_> {
    /// Each of `grid`'s columns' count of items starting in it, as a bar whose height grows with
    /// the count's logarithm, and a mark at the top where any is a condition.
    fn density_bins(&self, s: &Scale, grid: Grid, row: Rect, shapes: &mut Vec<Shape>) {
        let p = self.palette;
        let Some(level) = self.density.level(grid.per) else {
            return;
        };
        let mut bins = Vec::with_capacity(grid.count);
        let mut most = 1u64;
        for k in 0..grid.count {
            let (x0, x1) = (
                grid.x0 + k as f64 * grid.per,
                grid.x0 + (k + 1) as f64 * grid.per,
            );
            let a = x0.floor() as i64;
            let b = (x1.floor() as i64).max(a + 1);
            let (c, m) = self.density.count_at(level, a, b);
            most = most.max(c);
            bins.push((x0, x1, a, b, c, m));
        }
        let scale = (most as f32 + 1.0).log2();
        let lost = |a: i64, b: i64| {
            (self.lost.iter()).any(|l| {
                let (la, lb) = (
                    self.map.axis_saturating(l.start),
                    self.map.axis_saturating(l.end),
                );
                la < b && a < lb
            })
        };
        for &(x0, x1, a, b, c, m) in &bins {
            if c == 0 {
                continue;
            }
            let x = s.x(x0).max(s.left);
            let x1 = s.x(x1).min(s.right).max(x);
            let h = ((c as f32 + 1.0).log2() / scale * (row.height() - 2.0 * PAD)).max(1.0);
            let ink = match lost(a, b) {
                true => p.ink_2.gamma_multiply(p.pale),
                false => p.ink_2,
            };
            shapes.push(Shape::rect_filled(
                Rect::from_min_max(
                    pos2(x, row.bottom() - PAD - h),
                    pos2(x1, row.bottom() - PAD),
                ),
                0.0,
                ink,
            ));
            if m > 0 {
                shapes.push(Shape::rect_filled(
                    Rect::from_min_max(pos2(x, row.top()), pos2(x1, row.top() + PAD)),
                    0.0,
                    p.attention_ink,
                ));
            }
        }
    }

    /// The items the window meets: boxes with the fullest label that fits, conditions outlined
    /// in the attention ink, and in the parts row their spanned fields and elements.
    /// The item drawn under `click`, a point clicked this frame, if any.
    fn items(
        &self,
        ui: &Ui,
        s: &Scale,
        lanes: &[Rect],
        parts: Option<Rect>,
        click: Option<egui::Pos2>,
        shapes: &mut Vec<Shape>,
    ) -> Option<Id> {
        let mut clicked = None;
        let p = self.palette;
        let w = &self.shown.window;
        let axis = |t: Tick| self.map.axis_saturating(t) as f64;
        let pointer = ui.input(|i| i.pointer.hover_pos());
        // Where each lane's last item ends, in points.
        let mut ends = vec![f32::NEG_INFINITY; lanes.len()];
        let meeting = self.density.meeting(self.items, &self.map, w, self.settled);
        for item in meeting {
            let start = axis(item.start);
            let end = axis(item.end.unwrap_or(self.settled));
            let (x0, x1) = (s.x(start).max(s.left), s.x(end).min(s.right));
            let x1 = x1.max(x0 + 1.0);
            let row = (self.rows.get(usize::from(item.id.kind.0))).map_or(0..1, Clone::clone);
            let Some(lane) = row.clone().find(|&l| ends[l] <= x0) else {
                // Every lane of its row busy: an overflow mark over the first, never a box over
                // another.
                let top = lanes[row.start].top();
                shapes.push(Shape::line_segment(
                    [pos2(x0, top), pos2(x0, top + PAD)],
                    Stroke::new(1.0, p.attention_ink),
                ));
                continue;
            };
            ends[lane] = x1;
            let row = lanes[lane];
            let r = Rect::from_min_max(pos2(x0, row.top() + PAD), pos2(x1, row.bottom() - PAD));
            let hovered = pointer.is_some_and(|q| r.contains(q));
            if hovered {
                shapes.push(Shape::rect_filled(r, 2.0, p.wash));
            }
            if click.is_some_and(|q| r.contains(q)) {
                clicked = Some(item.id);
            }
            let marked = condition(item);
            let stroke = match (self.selected == Some(item.id), marked) {
                (true, _) => Stroke::new(2.0, p.ink),
                (false, true) => Stroke::new(1.0, p.attention_ink),
                (false, false) => Stroke::new(1.0, p.ink_2),
            };
            shapes.push(Shape::rect_stroke(r, 2.0, stroke, egui::StrokeKind::Inside));
            let room = r.width() - 2.0 * PAD;
            let declared = (self.schema.kind(item.id.kind)).map_or(&[][..], |k| &k.labels[..]);
            for t in self.labels.of(item.id.kind).unwrap_or(declared) {
                let text = label(self.schema, item, t);
                if text.is_empty() {
                    continue;
                }
                let galley = ui.painter().layout_no_wrap(text, p.labels.clone(), p.ink);
                if galley.size().x <= room {
                    let at = pos2(r.left() + PAD, r.center().y - galley.size().y / 2.0);
                    shapes.push(Shape::galley(at, galley, p.ink));
                    break;
                }
            }
            if let Some(prow) = parts {
                self.parts_of(ui, s, item, prow, shapes);
            }
        }
        clicked
    }

    /// An item's spanned fields, each its stretch with its value where it fits; where its
    /// sequence's cells are wide enough, their elements instead.
    fn parts_of(&self, ui: &Ui, s: &Scale, item: &Item, row: Rect, shapes: &mut Vec<Shape>) {
        let p = self.palette;
        let axis = |t: Tick| self.map.axis_saturating(t) as f64;
        let kind = self.schema.kind(item.id.kind);
        let rule = Stroke::new(1.0, p.hair);
        for f in &item.fields {
            let Some(span) = f.span else { continue };
            let (x0, x1) = (s.x(axis(span.start)), s.x(axis(span.end)));
            if x1 < s.left || x0 > s.right {
                continue;
            }
            let r = Rect::from_min_max(
                pos2(x0.max(s.left), row.top() + PAD),
                pos2(x1.min(s.right), row.bottom() - PAD),
            );
            let role = f
                .driver
                .and_then(|d| self.schema.roles.get(usize::from(d.0)))
                .and_then(|name| p.roles.get(name.as_ref()))
                .and_then(|look| look.colour);
            if let Some(c) = role {
                shapes.push(Shape::rect_filled(r, 0.0, c.gamma_multiply(p.part)));
            }
            shapes.push(Shape::rect_stroke(r, 1.0, rule, egui::StrokeKind::Inside));
            // The sequence named as the field holds its bits.
            let name = kind.and_then(|k| k.fields.get(usize::from(f.field.0)).map(|d| &d.name));
            let seq = kind.and_then(|k| {
                (k.sequences.iter())
                    .position(|q| Some(&q.name) == name)
                    .map(|i| escapement_tickwave_decode::SeqId(i as u16))
            });
            let elements: &[Element] = seq.map_or(&[], |q| item.seq(q));
            let cell = elements
                .first()
                .map_or(0.0, |e| s.x(axis(e.cell.end)) - s.x(axis(e.cell.start)));
            if cell >= BIT {
                for e in elements {
                    let (a, b) = (s.x(axis(e.cell.start)), s.x(axis(e.cell.end)));
                    if b < s.left || a > s.right {
                        continue;
                    }
                    shapes.push(Shape::line_segment(
                        [
                            pos2(a, row.bottom() - PAD - 4.0),
                            pos2(a, row.bottom() - PAD),
                        ],
                        rule,
                    ));
                    let galley =
                        ui.painter()
                            .layout_no_wrap(e.value.to_string(), p.figures.clone(), p.ink);
                    let at = pos2(
                        (a + b) / 2.0 - galley.size().x / 2.0,
                        row.center().y - galley.size().y / 2.0,
                    );
                    shapes.push(Shape::galley(at, galley, p.ink));
                }
                continue;
            }
            // Its value where that fits, else its field's name, else the short name its field
            // declares or the name's first three letters, else nothing.
            let value = value_text(self.schema, item.id.kind, f.field, &f.value);
            let field = name.map_or(String::new(), |n| n.to_string());
            let decl = kind.and_then(|k| k.fields.get(usize::from(f.field.0)));
            let short = (decl.and_then(|d| d.short.as_ref()))
                .map_or_else(|| field.chars().take(3).collect(), |s| s.to_string());
            for text in [value, field, short] {
                if text.is_empty() {
                    continue;
                }
                let galley = ui
                    .painter()
                    .layout_no_wrap(text, p.figures.clone(), p.ink_2);
                if galley.size().x <= r.width() - 2.0 * PAD {
                    let at = pos2(r.left() + PAD, r.center().y - galley.size().y / 2.0);
                    shapes.push(Shape::galley(at, galley, p.ink_2));
                    break;
                }
            }
        }
    }
}

/// `decoded`, a track's items or density, into `out` as a provisional window's: each outline
/// dashed, and every colour at opacity `pale`.
fn provisional(decoded: Vec<Shape>, pale: f32, out: &mut Vec<Shape>) {
    for shape in decoded {
        match shape {
            Shape::Rect(r) => {
                let fill = r.fill.gamma_multiply(pale);
                if fill.a() > 0 {
                    out.push(Shape::rect_filled(r.rect, r.corner_radius, fill));
                }
                if r.stroke.width > 0.0 {
                    let q = r.rect;
                    let path = [
                        q.left_top(),
                        q.right_top(),
                        q.right_bottom(),
                        q.left_bottom(),
                        q.left_top(),
                    ];
                    let stroke = Stroke::new(r.stroke.width, r.stroke.color.gamma_multiply(pale));
                    out.extend(Shape::dashed_line(&path, stroke, DASH, GAP));
                }
            }
            Shape::LineSegment { points, stroke } => {
                let stroke = Stroke::new(stroke.width, stroke.color.gamma_multiply(pale));
                out.push(Shape::line_segment(points, stroke));
            }
            Shape::Text(mut t) => {
                t.opacity_factor *= pale;
                out.push(Shape::Text(t));
            }
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A density bins in the lanes' columns where the graph drew them at this scale, across the
    /// whole window though the answer held covers less; at another scale, or with none, in the
    /// plot's points from the window's start.
    #[test]
    fn a_density_bins_in_the_lanes_columns_drawn_at_its_scale_else_in_points() {
        let s = Scale {
            from: 1000.0,
            per: 10.0,
            left: 0.0,
            right: 100.0,
        };
        let mut shown = Shown::new(1000..2000, 0.0..100.0);
        let grid = |x0: f64, per: f64, count: usize| Grid { x0, per, count };
        let points = grid(1000.0, 10.0, 100);
        assert_eq!(bin_columns(&shown, &s, 1.0), points);
        // The lanes' columns, 5 ticks off the window's start and drawn over part of it.
        let lanes = |per: f64| Some(grid(1005.0, per, 20));
        shown.grid = lanes(10.0);
        assert_eq!(bin_columns(&shown, &s, 1.0), grid(995.0, 10.0, 101));
        // At two pixels a point, columns half a point wide.
        shown.grid = lanes(5.0);
        assert_eq!(bin_columns(&shown, &s, 2.0), grid(1000.0, 5.0, 200));
        // Half and twice a pixel, a scale's rounding, still the lanes'.
        assert_eq!(bin_columns(&shown, &s, 1.0), grid(1000.0, 5.0, 200));
        shown.grid = lanes(20.0);
        assert_eq!(bin_columns(&shown, &s, 1.0), grid(985.0, 20.0, 51));
        // An answer for a scale further off: points.
        for per in [4.9, 20.5, 80.0] {
            shown.grid = lanes(per);
            assert_eq!(bin_columns(&shown, &s, 1.0), points, "{per}");
        }
    }

    /// `count` summed bucket by bucket at the level it picks.
    fn flat(d: &Density, from: i64, to: i64, per: f64) -> (u64, u64) {
        let Some(l) = (d.levels.iter())
            .rev()
            .find(|l| ((1u128 << l.shift) as f64) <= per)
            .or(d.levels.first())
        else {
            return (0, 0);
        };
        let a = ((from >> l.shift) - l.first).max(0) as usize;
        let b = ((((to - 1) >> l.shift) - l.first + 1).max(0) as usize).min(l.counts.len());
        if a >= b {
            return (0, 0);
        }
        let c = l.counts[a..b].iter().map(|&x| u64::from(x)).sum();
        let m = l.marked[a..b].iter().map(|&x| u64::from(x)).sum();
        (c, m)
    }

    /// Items starting in time order, some long and some closing incomplete, opened and closed in
    /// batches as a forward pass would, with a density grown after each to the start frontier.
    /// The store keeps `keep`. Each snapshot is the density, the store's items and the frontier.
    #[expect(clippy::type_complexity, reason = "a test's tuple")]
    fn grown(
        seed: u64,
        keep: Span,
    ) -> (
        escapement_tickwave_decode::Store,
        Density,
        Vec<(Density, BTreeMap<Id, Item>, Tick)>,
    ) {
        use escapement_tickwave_decode::{Cause, Op, Store};
        let mut rng = seed;
        let mut next = |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        let map = Mapping::new(
            0,
            0,
            escapement_tickwave_view::format::model::Ratio::integer(1).unwrap(),
        );
        // (start, end, incomplete, anchor), by start; one in five anchored inside it.
        let mut items = vec![];
        let mut t = -2_000_000i64;
        for _ in 0..4_000 {
            t += 1 + next(1_000) as i64;
            let len = match next(50) {
                0 => 200_000 + next(2_000_000) as i64,
                _ => 1 + next(800) as i64,
            };
            let anchor = if next(5) == 0 {
                t + next(len as u64) as i64
            } else {
                t
            };
            items.push((t, t + len, next(7) == 0, anchor));
        }
        // The first long and anchored near its end, so it is counted after items starting later.
        if let Some(first) = items.first_mut() {
            *first = (first.0, first.0 + 1_000_000, false, first.0 + 999_000);
        }
        let mut store = Store::with_keep(vec![keep]);
        let mut d = Density::default();
        let mut snapshots = vec![];
        let (mut opened, mut closed) = (0, vec![false; items.len()]);
        while opened < items.len() || closed.iter().any(|c| !c) {
            // Open up to the next few starts, then close everything ending before the last.
            let upto = (opened + 1 + next(60) as usize).min(items.len());
            let now = items.get(upto).map_or(i64::MAX, |i| i.0);
            let mut ops = vec![];
            for (k, &(start, _, _, anchor)) in items.iter().enumerate().take(upto).skip(opened) {
                let id = Id::new(KindId((k % 3) as u16), anchor);
                ops.push(Op::Open {
                    id,
                    parent: None,
                    start,
                });
            }
            opened = upto;
            for (k, &(_, end, bad, anchor)) in items.iter().enumerate().take(opened) {
                if !closed[k] && end <= now {
                    let id = Id::new(KindId((k % 3) as u16), anchor);
                    let status = if bad {
                        Status::Incomplete(Cause(0))
                    } else {
                        Status::Complete
                    };
                    ops.push(Op::Close { id, end, status });
                    closed[k] = true;
                }
            }
            store.apply_batch(&ops, 1 << 20).unwrap();
            // A host's frontier can lag where the items are: no new one starts before it. At no
            // lag, the next item is anchored at the frontier itself.
            let lag = if next(3) == 0 { 0 } else { next(5_000) as i64 };
            let frontier = now.saturating_sub(lag);
            d.grow(store.items(), &map, condition, frontier);
            snapshots.push((d.clone(), store.items().clone(), frontier));
        }
        (store, d, snapshots)
    }

    fn all() -> Span {
        Span {
            start: Tick::MIN,
            end: Tick::MAX,
        }
    }

    /// A density tallied from a pass that keeps nothing, batch by batch in the order items close,
    /// counts as one built from a store that kept them all.
    #[test]
    fn a_tallied_density_counts_as_one_built_from_the_kept_items() {
        let map = Mapping::new(
            0,
            0,
            escapement_tickwave_view::format::model::Ratio::integer(1).unwrap(),
        );
        let (store, _, _) = grown(0x7a11, all());
        let built = Density::of(store.items(), &map, condition);
        let mut closed: Vec<Closed> = store.items().values().filter_map(Closed::of).collect();
        closed.sort_by_key(|c| (c.end, c.id));
        let mut tallied = Density::default();
        let mut rng = 0x9e37_u64;
        let mut next = |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        let mut rest = &closed[..];
        while !rest.is_empty() {
            let (batch, more) = rest.split_at((1 + next(90) as usize).min(rest.len()));
            tallied.tally(batch, &map);
            rest = more;
        }
        assert_eq!(tallied.items(), built.items());
        assert_eq!(tallied.longest, built.longest);
        for _ in 0..400 {
            let from = -2_100_000 + next(4_500_000) as i64;
            let to = from + 1 + next(3_000_000) as i64;
            let per = [0.0, 1.0, 37.0, 4096.0, 1e9][next(5) as usize];
            assert_eq!(tallied.count(from, to, per), built.count(from, to, per));
            let w = from..to;
            assert_eq!(tallied.crowded(&w, 800.0), built.crowded(&w, 800.0));
        }
        let marked = closed.iter().filter(|c| closed_condition(c)).count() as u64;
        assert!(marked > 0);
        assert_eq!(tallied.count(i64::MIN / 2, i64::MAX / 2, 1e18).1, marked);
    }

    #[test]
    fn a_density_grown_with_its_store_counts_as_one_built_at_the_end() {
        let map = Mapping::new(
            0,
            0,
            escapement_tickwave_view::format::model::Ratio::integer(1).unwrap(),
        );
        let (store, grown, _) = grown(0x5eed, all());
        let built = Density::of(store.items(), &map, condition);
        assert_eq!(grown.items(), built.items());
        assert_eq!(grown.items(), store.items().len() as u64);
        assert_eq!(grown.longest, built.longest);
        // The long items, 200,000 ticks and more, held apart; the short ones within reach.
        assert!(grown.reach <= 800, "{}", grown.reach);
        let long = (store.items().values())
            .filter(|i| i.end.unwrap() - i.start >= 200_000)
            .count();
        assert_eq!(grown.held.len(), long);
        let mut rng = 0x1234_u64;
        let mut next = |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        for _ in 0..400 {
            let from = -2_100_000 + next(4_500_000) as i64;
            let to = from + 1 + next(3_000_000) as i64;
            let per = [0.0, 1.0, 37.0, 4096.0, 1e9][next(5) as usize];
            assert_eq!(grown.count(from, to, per), built.count(from, to, per));
            let w = from..to;
            let ids = |d: &Density| -> Vec<Id> {
                d.meeting(store.items(), &map, &w, Tick::MAX)
                    .map(|i| i.id)
                    .collect()
            };
            assert_eq!(ids(&grown), ids(&built), "{w:?}");
        }
    }

    #[test]
    fn a_growing_density_finds_every_item_and_counts_every_counted_one_at_each_snapshot() {
        let map = Mapping::new(
            0,
            0,
            escapement_tickwave_view::format::model::Ratio::integer(1).unwrap(),
        );
        let (_, _, snapshots) = grown(0xfeed, all());
        let mut rng = 0x77_u64;
        let mut next = |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        assert!(snapshots.len() > 50);
        for (d, items, frontier) in snapshots.iter().step_by(5) {
            let levels = &d.levels;
            assert!(levels[0].counts.len() <= FINEST && levels.last().unwrap().counts.len() <= 2);
            assert!(levels.windows(2).all(|l| l[1].shift == l[0].shift + 1));
            // Counted: the items anchored before the frontier, as a density built over them.
            let counted: BTreeMap<Id, Item> = (items.iter())
                .filter(|(id, _)| id.anchor < *frontier)
                .map(|(id, i)| (*id, i.clone()))
                .collect();
            let built = Density::of(&counted, &map, condition);
            assert_eq!(d.items(), built.items());
            for _ in 0..40 {
                let from = -2_100_000 + next(4_500_000) as i64;
                let w = from..from + 1 + next(400_000) as i64;
                let per = [0.0, 1.0, 37.0, 4096.0, 1e9][next(5) as usize];
                assert_eq!(
                    d.count(w.start, w.end, per),
                    built.count(w.start, w.end, per)
                );
                // And at the finest level, against the counted items' starts bucket by bucket.
                let s = d.levels[0].shift;
                let (a, b) = (w.start >> s, (w.end - 1) >> s);
                let inside = (counted.values()).filter(|i| (a..=b).contains(&(i.start >> s)));
                let marked = inside.clone().filter(|i| condition(i)).count() as u64;
                assert_eq!(
                    d.count(w.start, w.end, 0.0),
                    (inside.count() as u64, marked)
                );
                // Found: every item the window meets, an open one reaching past it.
                let want: Vec<Id> = (items.values())
                    .filter(|i| i.start < w.end && i.end.unwrap_or(Tick::MAX) >= w.start)
                    .map(|i| i.id)
                    .collect();
                let got: Vec<Id> = (d.meeting(items, &map, &w, Tick::MAX))
                    .map(|i| i.id)
                    .collect();
                assert_eq!(got, want, "{w:?} at {frontier}");
            }
        }
    }

    #[test]
    fn a_growing_density_over_a_store_that_keeps_a_span_finds_only_what_it_keeps() {
        let map = Mapping::new(
            0,
            0,
            escapement_tickwave_view::format::model::Ratio::integer(1).unwrap(),
        );
        let keep = Span {
            start: -500_000,
            end: 500_000,
        };
        let (store, d, _) = grown(0xabcd, keep);
        let w = -3_000_000..3_000_000;
        let found: Vec<Id> = (d.meeting(store.items(), &map, &w, Tick::MAX))
            .map(|i| i.id)
            .collect();
        let kept: Vec<Id> = store.items().keys().copied().collect();
        assert_eq!(found, kept);
    }

    #[test]
    fn the_cut_between_short_and_long_rises_as_long_items_become_common() {
        use escapement_tickwave_decode::{Op, Store};
        let map = Mapping::new(
            0,
            0,
            escapement_tickwave_view::format::model::Ratio::integer(1).unwrap(),
        );
        let mut store = Store::with_keep(vec![all()]);
        let put = |store: &mut Store, k: i64, len: i64| {
            let (id, start) = (Id::new(KindId(0), k * 100), k * 100);
            let ops = [
                Op::Open {
                    id,
                    parent: None,
                    start,
                },
                Op::Close {
                    id,
                    end: start + len,
                    status: Status::Complete,
                },
            ];
            store.apply_batch(&ops, 8).unwrap();
        };
        // A few items 3,000 ticks long among short ones: held apart.
        for k in 0..25 {
            put(&mut store, k, if k % 5 == 0 { 3_000 } else { 1 + k % 10 });
        }
        let mut d = Density::default();
        d.grow(store.items(), &map, condition, 2_500);
        let long = d.long;
        assert_eq!(d.held.len(), 5);
        // Then two thousand more of them: holding them all apart would cost more than reaching.
        for k in 25..2_025 {
            put(&mut store, k, 3_000);
        }
        d.grow(store.items(), &map, condition, Tick::MAX);
        assert!(d.long > long && d.held.is_empty() && d.reach >= 3_000);
        for from in (-5_000..210_000).step_by(1_700) {
            let w = from..from + 900;
            let want: Vec<Id> = (store.items().values())
                .filter(|i| i.start < w.end && i.end.unwrap() >= w.start)
                .map(|i| i.id)
                .collect();
            let got: Vec<Id> = (d.meeting(store.items(), &map, &w, Tick::MAX))
                .map(|i| i.id)
                .collect();
            assert_eq!(got, want, "{w:?}");
        }
    }

    #[test]
    fn a_count_through_the_levels_is_the_sum_of_its_buckets() {
        let mut rng = 0x5eed_u64;
        let mut next = |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        let map = Mapping::new(
            0,
            0,
            escapement_tickwave_view::format::model::Ratio::integer(1).unwrap(),
        );
        for origin in [0i64, -3_000_000, -1_500_017] {
            let all = Span {
                start: Tick::MIN,
                end: Tick::MAX,
            };
            let mut store = escapement_tickwave_decode::Store::with_keep(vec![all]);
            for _ in 0..5_000 {
                let start = origin + next(4_000_000) as i64;
                let id = Id::new(KindId(next(2) as u16), start);
                if store.items().contains_key(&id) {
                    continue;
                }
                let status = match next(5) {
                    0 => Status::Incomplete(escapement_tickwave_decode::Cause(0)),
                    _ => Status::Complete,
                };
                let end = start + 1 + next(1000) as i64;
                let ops = [
                    escapement_tickwave_decode::Op::Open {
                        id,
                        parent: None,
                        start,
                    },
                    escapement_tickwave_decode::Op::Close { id, end, status },
                ];
                store.apply_batch(&ops, 8).unwrap();
            }
            let d = Density::of(store.items(), &map, condition);
            for _ in 0..400 {
                let from = origin - 100_000 + next(4_200_000) as i64;
                let to = from + 1 + next(4_000_000) as i64;
                let per = [0.0, 1.0, 37.0, 4096.0, 1e9][next(5) as usize];
                assert_eq!(
                    d.count(from, to, per),
                    flat(&d, from, to, per),
                    "{from}..{to} at {per}"
                );
            }
        }
    }
}
