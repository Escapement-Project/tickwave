//! A decode track: one analysis's items drawn under a graph, on the graph's time axis.
//!
//! Its rows are reserved from the first frame and keep their height: the rows its schema's kinds
//! declare, each with its lanes, and where asked for, a row of their parts and bits. Zoomed out,
//! where items would crowd, it draws how many start in each pixel column from a `Density` ladder
//! instead, so a frame's work follows the columns on screen, not the capture. Conditions (an item
//! closed incomplete, a value unknown, anything the host marks) show at every zoom. Lost input is
//! shaded as the waveform's lost time is, input not decoded yet is hatched, and decoded input with
//! nothing in it is left bare, so the three differ without hue.

use std::borrow::Cow;

use egui::{Align2, Rect, Response, Sense, Shape, Stroke, Ui, Widget, pos2, vec2};
use escapement_tickwave_decode::{
    Element, FieldId, FieldType, Id, Item, KindId, Schema, Span, Status, Tick, Value,
};
use escapement_tickwave_view::Paint;
use escapement_tickwave_view::format::annotations::Role;
use escapement_tickwave_view::format::mapping::Mapping;

use crate::graph::Shown;
use crate::palette::Palette;

/// A row's height, as a lane's.
pub const ROW: f32 = 24.0;
/// Items narrower than this, on average, are drawn as density.
const CROWDED: f32 = 3.0;
/// A sequence's elements get their values once a cell is this wide.
const BIT: f32 = 9.0;
const PAD: f32 = 3.0;
const HATCH: f32 = 6.0;

/// One piece of a label: text, or a field's value as the item holds it.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Piece {
    Text(Cow<'static, str>),
    Field(FieldId),
}

/// A label over an item's typed values. A piece whose field the item does not hold is left out.
#[derive(Clone, Debug, Default)]
pub struct Template(pub Vec<Piece>);

impl Template {
    pub fn text(mut self, t: impl Into<Cow<'static, str>>) -> Self {
        self.0.push(Piece::Text(t.into()));
        self
    }

    pub fn field(mut self, f: FieldId) -> Self {
        self.0.push(Piece::Field(f));
        self
    }
}

/// Each kind's labels, fullest first: a track draws the first that fits its item, else none.
#[derive(Clone, Debug, Default)]
pub struct Labels {
    by_kind: Vec<(KindId, Vec<Template>)>,
}

impl Labels {
    pub fn kind(mut self, kind: KindId, templates: Vec<Template>) -> Self {
        self.by_kind.push((kind, templates));
        self
    }

    fn of(&self, kind: KindId) -> &[Template] {
        (self.by_kind.iter())
            .find(|(k, _)| *k == kind)
            .map_or(&[], |(_, t)| t.as_slice())
    }
}

/// A field's value as a label shows it: a word in hex as wide as its bits, a name by its
/// declared name, a bool as 1 or 0, an unknown one as a question mark.
pub fn value_text(schema: &Schema, kind: KindId, field: FieldId, value: &Value) -> String {
    let decl = schema
        .kind(kind)
        .and_then(|k| k.fields.get(usize::from(field.0)));
    match value {
        Value::Word { value, bits } => {
            let digits = usize::from(bits.div_ceil(4)).max(1);
            format!("0x{value:0digits$X}")
        }
        Value::Name(n) => match decl.map(|d| &d.ty) {
            Some(FieldType::Name(list)) => list
                .get(usize::from(*n))
                .map_or_else(|| "?".into(), |s| s.to_string()),
            _ => "?".into(),
        },
        Value::Bool(b) => u8::from(*b).to_string(),
        Value::Text(t) => t.clone(),
        Value::Bits { bytes, .. } => bytes.iter().rev().map(|b| format!("{b:02X}")).collect(),
        Value::Phase => String::new(),
        _ => "?".into(),
    }
}

fn label(schema: &Schema, item: &Item, t: &Template) -> String {
    let mut out = String::new();
    for p in &t.0 {
        match p {
            Piece::Text(s) => out.push_str(s),
            Piece::Field(f) => {
                if let Some(x) = item.field(*f) {
                    out.push_str(&value_text(schema, item.id.kind, *f, &x.value));
                }
            }
        }
    }
    out.trim().to_string()
}

/// Whether an item is a condition: closed incomplete, or holding a value it could not know.
pub fn condition(item: &Item) -> bool {
    matches!(item.status, Some(Status::Incomplete(_)))
        || (item.fields.iter()).any(|f| matches!(f.value, Value::Unknown { .. }))
}

/// The most items whose parts a window paints a wire with: past it, at a zoom where parts are
/// under a pixel, the wire keeps its own look.
const PAINTED_AT_MOST: u64 = 4096;

/// Paint for `channels` from the parts of the items in `window` (axis ticks), by the role that
/// drives each: what a graph's `decoded_paint` takes to draw a wire by who drives it. Empty
/// where the window holds more items than `PAINTED_AT_MOST`.
pub fn driver_paints(
    schema: &Schema,
    items: &std::collections::BTreeMap<Id, Item>,
    density: &Density,
    map: &Mapping,
    window: &std::ops::Range<i64>,
    channels: &[String],
) -> Vec<Paint> {
    let mut out = Vec::new();
    if density.count(window.start, window.end, 0.0).0 > PAINTED_AT_MOST {
        return out;
    }
    let axis = |t: Tick| map.axis_saturating(t);
    for item in density.meeting(items, map, window, Tick::MAX) {
        for f in &item.fields {
            let (Some(span), Some(d)) = (f.span, f.driver) else {
                continue;
            };
            let Some(role) = schema.roles.get(usize::from(d.0)) else {
                continue;
            };
            let span = axis(span.start)..axis(span.end);
            if span.end <= window.start || span.start >= window.end {
                continue;
            }
            out.push(Paint {
                arrival: 0,
                role: role.to_string(),
                span,
                channels: channels.to_vec(),
            });
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

/// How many items start where, on the axis, at a ladder of bucket sizes: what a zoomed-out
/// track draws. Built once over a store, it costs nothing per frame past the columns drawn.
#[derive(Clone, Debug, Default)]
pub struct Density {
    levels: Vec<Level>,
    /// The longest closed item, in axis ticks.
    longest: i64,
    items: u64,
    /// Where a window finds its items: those shorter than `2^(long - 1)` ticks start at most
    /// `reach` before it, and the rest, open ones among them, are held in `held` by id with
    /// their spans, so a few long items do not make every window walk back as far as they reach.
    long: u32,
    reach: i64,
    held: Vec<(Id, i64, Option<i64>)>,
}

/// The most buckets the finest level holds; coarser levels halve it.
const FINEST: usize = 1 << 20;

/// The most items a density holds apart as long.
const LONG_AT_MOST: u64 = 1 << 16;

/// An item's length class: lengths from `2^(k-1)` up to `2^k` are class `k`.
fn class(len: i64) -> u32 {
    64 - len.max(0).leading_zeros()
}

impl Density {
    /// The ladder of `items`, placed on the axis by `map`, with `marked` saying which are
    /// conditions.
    pub fn of<'a>(
        items: impl Iterator<Item = &'a Item> + Clone,
        map: &Mapping,
        marked: impl Fn(&Item) -> bool,
    ) -> Self {
        let axis = |t: Tick| map.axis_saturating(t);
        let (mut lo, mut hi, mut longest, mut n) = (i64::MAX, i64::MIN, 0, 0u64);
        let (mut classes, mut open) = ([0u64; 65], 0u64);
        for i in items.clone() {
            let s = axis(i.start);
            lo = lo.min(s);
            hi = hi.max(s);
            match i.end {
                Some(e) => {
                    let len = axis(e).saturating_sub(s);
                    longest = longest.max(len);
                    classes[class(len) as usize] += 1;
                }
                None => open += 1,
            }
            n += 1;
        }
        if n == 0 {
            return Self::default();
        }
        let span = (hi.saturating_sub(lo) as u128).max(1);
        // The class from which items are held apart: the one that makes a window's walk least,
        // about the items starting within reach of it plus every long one.
        let per_tick = n as f64 / span as f64;
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
        let mut shift = 0u8;
        while (span >> shift) as usize >= FINEST {
            shift += 1;
        }
        let first = lo >> shift;
        let len = ((hi >> shift) - first + 1) as usize;
        let mut level = Level {
            shift,
            first,
            counts: vec![0; len],
            marked: vec![0; len],
        };
        let (mut reach, mut held) = (0, vec![]);
        for i in items {
            let start = axis(i.start);
            let end = i.end.map(axis);
            match end.map(|e| e.saturating_sub(start)) {
                Some(len) if class(len) < long => reach = reach.max(len),
                _ => held.push((i.id, start, end)),
            }
            let k = ((start >> shift) - first) as usize;
            if let Some(c) = level.counts.get_mut(k) {
                *c = c.saturating_add(1);
            }
            if marked(i)
                && let Some(c) = level.marked.get_mut(k)
            {
                *c = c.saturating_add(1);
            }
        }
        let mut levels = vec![level];
        while let Some(l) = levels.last().filter(|l| l.counts.len() > 1) {
            let first = l.first >> 1;
            let len = ((l.first + l.counts.len() as i64 - 1) >> 1) - first + 1;
            // The buckets either side of zero never join under an arithmetic shift.
            if len as usize >= l.counts.len() {
                break;
            }
            let mut next = Level {
                shift: l.shift + 1,
                first,
                counts: vec![0; len as usize],
                marked: vec![0; len as usize],
            };
            for (k, (&c, &m)) in l.counts.iter().zip(&l.marked).enumerate() {
                let j = (((l.first + k as i64) >> 1) - first) as usize;
                next.counts[j] = next.counts[j].saturating_add(c);
                next.marked[j] = next.marked[j].saturating_add(m);
            }
            levels.push(next);
        }
        held.sort_unstable_by_key(|h| h.0);
        Self {
            levels,
            longest,
            items: n,
            long,
            reach,
            held,
        }
    }

    /// How many items it counts.
    pub fn items(&self) -> u64 {
        self.items
    }

    /// The items of `items` (the store it was built over) that meet `window` (axis ticks), in id
    /// order, an open one reaching `settled`.
    fn meeting<'a>(
        &'a self,
        items: &'a std::collections::BTreeMap<Id, Item>,
        map: &'a Mapping,
        window: &'a std::ops::Range<i64>,
        settled: Tick,
    ) -> impl Iterator<Item = &'a Item> + 'a {
        let axis = |t: Tick| map.axis_saturating(t);
        let meets = move |start: i64, end: Option<i64>| {
            start < window.end && end.unwrap_or(axis(settled)) >= window.start
        };
        // An item's anchor lies at or after its start and within it.
        let lo = Id::new(
            KindId(0),
            window.start.saturating_sub(self.reach.saturating_add(1)),
        );
        let short = (items.range(lo..).map(|(_, i)| i))
            .take_while(move |i| axis(i.start) < window.end.saturating_add(self.reach))
            .filter(move |i| {
                let start = axis(i.start);
                let end = i.end.map(axis);
                let short = end.is_some_and(|e| class(e.saturating_sub(start)) < self.long);
                short && meets(start, end)
            });
        let long = (self.held.iter())
            .filter(move |&&(_, start, end)| meets(start, end))
            .filter_map(|(id, _, _)| items.get(id));
        Merge {
            a: short.peekable(),
            b: long.peekable(),
        }
    }

    /// Items starting in `[from, to)` and how many are conditions, at the coarsest level whose
    /// buckets are no wider than `per`; with `per` 0, the finest.
    fn count(&self, from: i64, to: i64, per: f64) -> (u64, u64) {
        let Some(l) = (self.levels.iter())
            .rev()
            .find(|l| ((1u128 << l.shift) as f64) <= per)
            .or(self.levels.first())
        else {
            return (0, 0);
        };
        let a = ((from >> l.shift) - l.first).max(0) as usize;
        let b = (((to - 1) >> l.shift) - l.first + 1).max(0) as usize;
        let b = b.min(l.counts.len());
        if a >= b {
            return (0, 0);
        }
        let c = l.counts[a..b].iter().map(|&x| u64::from(x)).sum();
        let m = l.marked[a..b].iter().map(|&x| u64::from(x)).sum();
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
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
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
        let (count, _) = self.density.count(w.start, w.end, 0.0);
        let longest = self.density.longest as f64 / s.per;
        let crowded = longest < f64::from(CROWDED) || count as f32 * CROWDED > plot.width();
        let lanes: Vec<Rect> = (0..self.lanes).map(row).collect();
        if crowded {
            self.density_bins(&s, lanes[0], &mut shapes);
        } else {
            let parts = self.parts.then(|| row(self.lanes));
            self.items(ui, &s, &lanes, parts, &mut shapes);
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
        painter.extend(shapes);
        response
    }
}

impl Track<'_> {
    /// Each pixel column's count of items starting in it, as a bar whose height grows with the
    /// count's logarithm, and a mark at the top where any is a condition.
    fn density_bins(&self, s: &Scale, row: Rect, shapes: &mut Vec<Shape>) {
        let p = self.palette;
        let columns = (s.right - s.left).ceil() as usize;
        let per = s.per;
        let mut bins = Vec::with_capacity(columns);
        let mut most = 1u64;
        for k in 0..columns {
            let a = (s.from + k as f64 * per).floor() as i64;
            let b = ((s.from + (k + 1) as f64 * per).floor() as i64).max(a + 1);
            let (c, m) = self.density.count(a, b, per);
            most = most.max(c);
            bins.push((a, b, c, m));
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
        for (k, &(a, b, c, m)) in bins.iter().enumerate() {
            if c == 0 {
                continue;
            }
            let x = s.left + k as f32;
            let h = ((c as f32 + 1.0).log2() / scale * (row.height() - 2.0 * PAD)).max(1.0);
            let ink = match lost(a, b) {
                true => p.ink_2.gamma_multiply(p.pale),
                false => p.ink_2,
            };
            shapes.push(Shape::rect_filled(
                Rect::from_min_max(
                    pos2(x, row.bottom() - PAD - h),
                    pos2(x + 1.0, row.bottom() - PAD),
                ),
                0.0,
                ink,
            ));
            if m > 0 {
                shapes.push(Shape::rect_filled(
                    Rect::from_min_max(pos2(x, row.top()), pos2(x + 1.0, row.top() + PAD)),
                    0.0,
                    p.attention_ink,
                ));
            }
        }
    }

    /// The items the window meets: boxes with the fullest label that fits, conditions outlined
    /// in the attention ink, and in the parts row their spanned fields and elements.
    fn items(
        &self,
        ui: &Ui,
        s: &Scale,
        lanes: &[Rect],
        parts: Option<Rect>,
        shapes: &mut Vec<Shape>,
    ) {
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
            let marked = condition(item);
            let stroke = Stroke::new(1.0, if marked { p.attention_ink } else { p.ink_2 });
            shapes.push(Shape::rect_stroke(r, 2.0, stroke, egui::StrokeKind::Inside));
            let room = r.width() - 2.0 * PAD;
            for t in self.labels.of(item.id.kind) {
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
                shapes.push(Shape::rect_filled(r, 0.0, c.gamma_multiply(0.18)));
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
            // Its value where that fits, else its field's name, else the name's first three
            // letters, else nothing.
            let value = value_text(self.schema, item.id.kind, f.field, &f.value);
            let field = name.map_or(String::new(), |n| n.to_string());
            let short: String = field.chars().take(3).collect();
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
