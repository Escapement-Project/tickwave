//! The side panel: the item the reader selected on a track, in full. Its host gives it a fixed
//! width and reserves it while a decode track shows, so selecting an item never narrows the plot
//! under the reader; with nothing selected it holds a note. Where the plot would be left
//! narrower than `Panel::PLOT_AT_LEAST`, the host puts the panel under the plot instead, at a
//! height it reserves, and the panel scrolls inside it. Its collapse control sits at the top
//! right, where it stays open or collapsed.
//!
//! An item shows its fullest label, its time and duration, its status and its citation, with
//! controls that copy the citation, or what the tab shows as text; then its unspanned fields flat,
//! its spanned fields as sections that open to their span and bits, the values it could not know as
//! one sentence for each reason, its links, and the items in the analyses stacked on its own that
//! read it ("read as"). Above it, fixed where they are pressed, previous and next step to the items
//! of its kind before and after it. Pointing at a section, the heading or a reader lights its span,
//! which the host passes to its tracks and graph.
//!
//! The List tab shows one analysis's items as rows, in time order: each its start, its kind while
//! more than one kind is shown, its label and, where it holds a condition, a mark. Kinds can be
//! left out, or every item but those holding a condition. A row the pointer rests on lights its
//! span, and a click selects it. When the selection changes the list scrolls to it.
//!
//! The Text tab joins the bytes of an analysis whose schema declares a kind's text
//! into lines, each after its start: a line ends at a newline, shown as `\n` with a carriage
//! return before it dropped, or wraps at `LINE_AT_MOST` characters. Any other byte that is not
//! printable shows as an escape, `\r` or `\x15`, and a value the decoder could not know as ·:
//! characters a host's fonts all hold. The character under the pointer lights its item's span,
//! and a click selects that item.
//!
//! The panel keeps its own reading of each analysis's store, and its tabs draw from that, so a
//! selection, a filter or another analysis chosen costs a few lookups, however many items the
//! store holds. Each frame it takes in what the store's change log says changed. A store it has
//! not read, or one whose log no longer reaches back to where it read, it reads `READ_AT_MOST`
//! ids a frame, shared between the analyses. Until it has read a store through, its List and
//! Text tabs say so in place of their rows, an item's previous waits until the reading has
//! passed it, and the Selected tab says what it shows may be missing.

use std::collections::{BTreeMap, BTreeSet};

use egui::{
    Align, Align2, Button, CollapsingHeader, Grid, Layout, Rect, Response, RichText, Role,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, UiBuilder, Widget, WidgetInfo, pos2, vec2,
};
use escapement_tickwave_decode::{
    ElementType, Id, Item, KindId, LinkId, Observation, Ref, Schema, Span, Status, Store, Tick,
    Value, citation,
};
use escapement_tickwave_view::format::mapping::Mapping;
use escapement_tickwave_view::format::model::Ratio;
use escapement_tickwave_view::readouts::format_interval;

use crate::palette::Palette;
use crate::track::{self, Labels, condition, heading, value_text};

mod index;

pub use index::READ_AT_MOST;
use index::{Index, Line};

/// The most elements a part's section lists; the rest are counted.
const ELEMENTS_AT_MOST: usize = 64;
/// The most items a "read as" list shows; the rest are counted.
const READERS_AT_MOST: usize = 32;
/// The collapse control's size, and the height of the row it heads.
const CONTROL: f32 = 24.0;
/// A list row's height.
const ROW: f32 = 18.0;
/// The most characters a line of text holds before it wraps.
pub const LINE_AT_MOST: usize = 120;

/// One analysis as the panel reads it: its name, its schema, the labels its track draws it with,
/// its store, and the analysis it is stacked on, by its index among the panel's. An analysis keeps
/// its place among them: the panel's reading of the store at a place is kept between frames.
pub struct Analysis<'a> {
    pub name: &'a str,
    pub schema: &'a Schema,
    pub labels: &'a Labels,
    pub store: &'a Store,
    pub reads: Option<usize>,
}

/// Where a list row's columns start, from its left: the kind's where it has one, and the label's.
#[derive(Clone, Copy)]
struct Columns {
    kind: Option<f32>,
    label: f32,
}

/// The panel's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    /// The selected item, in full.
    #[default]
    Selected,
    /// One analysis's items as rows.
    List,
    /// One analysis's text, joined into lines.
    Text,
}

/// What the panel shows and keeps between frames. Its host owns it, as it owns a graph's
/// `ViewState`: it passes the selection to its tracks, sets it from `Track::clicked`, and passes
/// `lit` to its tracks and graph.
#[derive(Clone, Debug, Default)]
pub struct PanelState {
    /// The item selected, by the index of its analysis. The panel's previous, next, link,
    /// "read as" and list controls set it too.
    pub selected: Option<(usize, Id)>,
    pub tab: Tab,
    /// The analysis the List tab shows, by index; None for the selected item's, else the first.
    pub listed: Option<usize>,
    /// The analysis the Text tab shows, by index; None for the selected item's where it reads as
    /// text, else the first that does.
    pub texted: Option<usize>,
    /// Collapsed to its control, by the reader.
    pub collapsed: bool,
    /// What the pointer rests on in the panel this frame: its span on the items' timeline.
    pub lit: Option<Span>,
    /// The kinds the List tab leaves out, by analysis.
    hidden: BTreeSet<(usize, KindId)>,
    /// The List tab shows only items holding a condition.
    conditions: bool,
    /// The selection the List tab last scrolled to.
    scrolled: Option<(usize, Id)>,
    /// Each analysis's store as the panel has read it, by its place.
    indexes: Vec<Index>,
    /// The frame it last read them in: a frame of several passes reads once.
    read_at: Option<u64>,
}

/// What the Selected tab shows around the selected item, found in the indexes each frame.
struct Found {
    previous: Option<Id>,
    next: Option<Id>,
    /// The first `READERS_AT_MOST` items that read it: each one's analysis, id, and the link by
    /// which it reads it.
    readers: Vec<(usize, Id, LinkId)>,
    /// How many read it in all.
    read_by: usize,
    /// An index these come from is still reading its store: some may be missing.
    reading: bool,
}

/// The side panel, `width` points wide when open.
pub struct Panel<'a> {
    palette: &'a Palette,
    state: &'a mut PanelState,
    analyses: &'a [Analysis<'a>],
    width: f32,
    /// Under the plot, this many points tall; else beside it.
    under: Option<f32>,
    timebase: Ratio,
    map: Mapping,
}

impl<'a> Panel<'a> {
    /// The width a collapsed panel keeps beside the plot, and the height it keeps under it: its
    /// control's.
    pub const COLLAPSED: f32 = CONTROL;
    /// The least width a plot keeps beside an open panel, in points.
    pub const PLOT_AT_LEAST: f32 = 480.0;

    /// `map` places the analyses' ticks on the recording's axis, whose ticks are `timebase`'s.
    pub fn new(
        palette: &'a Palette,
        state: &'a mut PanelState,
        analyses: &'a [Analysis<'a>],
        width: f32,
        timebase: Ratio,
        map: Mapping,
    ) -> Self {
        Self {
            palette,
            state,
            analyses,
            width,
            under: None,
            timebase,
            map,
        }
    }

    /// Whether a panel `width` wide fits beside a plot in `available` points; if not, a host puts
    /// it under the plot with `under`.
    pub fn fits_beside(available: f32, width: f32) -> bool {
        available >= width + Self::PLOT_AT_LEAST
    }

    /// The panel under the plot: the whole width, `height` points tall when open and its control's
    /// row collapsed. Its tabs scroll inside it.
    pub fn under(mut self, height: f32) -> Self {
        self.under = Some(height);
        self
    }

    /// The height a host reserves under its plot for a panel `height` tall when open, as `state`
    /// has it.
    pub fn reserved_height(state: &PanelState, height: f32) -> f32 {
        Self::reserved(state, height)
    }

    /// The width a host reserves for a panel `width` wide when open, as `state` has it.
    pub fn reserved(state: &PanelState, width: f32) -> f32 {
        if state.collapsed {
            Self::COLLAPSED
        } else {
            width
        }
    }

    /// A tick of the item's timeline as a time on the recording's axis, to the axis's resolution.
    fn time(&self, t: Tick) -> String {
        track::time(&self.map, self.timebase, t)
    }

    /// A span's start and length, or its start alone while it is open.
    fn when(&self, start: Tick, end: Option<Tick>) -> String {
        track::when(&self.map, self.timebase, start, end)
    }

    /// `to` against `from`, signed: "+40 ns".
    fn offset(&self, from: Tick, to: Tick) -> String {
        let ticks = self.map.axis_saturating(to) - self.map.axis_saturating(from);
        let sign = if ticks < 0 { "" } else { "+" };
        format!(
            "{sign}{}",
            format_interval(ticks, self.timebase, self.map.m())
        )
    }

    fn text(&self, t: impl Into<String>) -> RichText {
        RichText::new(t)
            .font(self.palette.labels.clone())
            .color(self.palette.ink)
    }

    fn quiet(&self, t: impl Into<String>) -> RichText {
        RichText::new(t)
            .font(self.palette.labels.clone())
            .color(self.palette.ink_2)
    }

    /// A choice that is on or off, without hue as the project's interaction is: on, its text in
    /// ink and underlined; off, in the quieter ink.
    fn toggle(&self, ui: &mut Ui, on: bool, text: &str) -> Response {
        let words = if on {
            self.text(text)
        } else {
            self.quiet(text)
        };
        let r = ui.add(Button::new(words).frame(false));
        if on {
            let under = [r.rect.left_bottom(), r.rect.right_bottom()];
            ui.painter()
                .line_segment(under, Stroke::new(1.5, self.palette.ink));
        }
        r
    }

    fn figures(&self, t: impl Into<String>) -> RichText {
        RichText::new(t)
            .font(self.palette.figures.clone())
            .color(self.palette.ink)
    }

    /// Takes in what each analysis's store changed, then reads on where an index is behind, all
    /// of them `READ_AT_MOST` ids a frame at most. While one is behind, the panel asks for another
    /// frame.
    fn read(&mut self, ctx: &egui::Context) {
        let frame = ctx.cumulative_frame_nr();
        if self.state.read_at == Some(frame) {
            return;
        }
        self.state.read_at = Some(frame);
        let n = self.analyses.len();
        self.state.indexes.resize_with(n, Index::default);
        // An equal share each, then what those with less to read left to those still behind.
        let mut left = READ_AT_MOST;
        let share = (left / n.max(1)).max(1);
        for (index, an) in self.state.indexes.iter_mut().zip(self.analyses) {
            left -= index.step(an.store, an.schema, an.reads.is_some(), share.min(left));
        }
        for (index, an) in self.state.indexes.iter_mut().zip(self.analyses) {
            if left == 0 {
                break;
            }
            if !index.current() {
                left -= index.step(an.store, an.schema, an.reads.is_some(), left);
            }
        }
        if self.state.indexes.iter().any(|i| !i.current()) {
            ctx.request_repaint();
        }
    }

    /// The neighbours of `selected` and the items that read it.
    fn found(&self, (a, id): (usize, Id)) -> Found {
        let indexes = &self.state.indexes;
        let (previous, next) = indexes[a].neighbours(id);
        let mut found = Found {
            previous,
            next,
            readers: Vec::new(),
            read_by: 0,
            reading: !indexes[a].current(),
        };
        for (b, reader) in self.analyses.iter().enumerate() {
            if reader.reads != Some(a) {
                continue;
            }
            found.reading |= !indexes[b].current();
            for (r, link) in indexes[b].readers_of(id) {
                found.read_by += 1;
                if found.readers.len() < READERS_AT_MOST {
                    found.readers.push((b, r, link));
                }
            }
        }
        found
    }

    fn list_tab(&mut self, ui: &mut Ui, lit: &mut Option<Span>, go: &mut Option<(usize, Id)>) {
        let Some(last) = self.analyses.len().checked_sub(1) else {
            ui.label(self.quiet("No analysis to list."));
            return;
        };
        let listed = (self.state.listed)
            .or(self.state.selected.map(|(a, _)| a))
            .unwrap_or(0)
            .min(last);
        if self.analyses.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                for (k, a) in self.analyses.iter().enumerate() {
                    if self.toggle(ui, k == listed, a.name).clicked() {
                        self.state.listed = Some(k);
                    }
                }
            });
        }
        let schema = self.analyses[listed].schema;
        ui.horizontal_wrapped(|ui| {
            for (k, decl) in schema.kinds.iter().enumerate() {
                let kind = (listed, KindId(k as u16));
                let shown = !self.state.hidden.contains(&kind);
                if self.toggle(ui, shown, &decl.name).clicked() {
                    if shown {
                        self.state.hidden.insert(kind);
                    } else {
                        self.state.hidden.remove(&kind);
                    }
                }
            }
            let only = self.state.conditions;
            if self.toggle(ui, only, "conditions only").clicked() {
                self.state.conditions = !only;
            }
        });
        ui.separator();
        let hidden: Vec<KindId> = (self.state.hidden.iter())
            .filter(|(b, _)| *b == listed)
            .map(|&(_, k)| k)
            .collect();
        let index = &self.state.indexes[listed];
        // Rows only once its store is read through, so none come in above those shown.
        if !index.read() {
            ui.label(self.quiet("Still reading the decode."));
            return;
        }
        let shown = index.shown(&hidden, self.state.conditions);
        let total = shown.len();
        if total == 0 {
            ui.label(self.quiet("No items here."));
            return;
        }
        let gap = ui.spacing().item_spacing.y;
        let mut scroll = (ScrollArea::vertical())
            .auto_shrink(false)
            .id_salt(("panel list", listed));
        // A new selection scrolls the list to it, near the middle.
        if self.state.selected != self.state.scrolled {
            if let Some((a, id)) = self.state.selected
                && a == listed
                && let Some(k) = shown.row(id)
            {
                // Within the list's extent, so the area has no overscroll to spring back from.
                let view = ui.available_height();
                let most = (total as f32 * (ROW + gap) - gap - view).max(0.0);
                let at = k as f32 * (ROW + gap) - view / 2.0;
                scroll = scroll.vertical_scroll_offset(at.clamp(0.0, most));
            }
            self.state.scrolled = self.state.selected;
        }
        // With more than one kind shown, a column of kind names between the time and the label, as
        // wide as the schema's widest so it stays put as kinds are shown and left out.
        let mut at = Columns {
            kind: None,
            label: self.label_at(ui),
        };
        if schema.kinds.len().saturating_sub(hidden.len()) > 1 {
            let widest = (schema.kinds.iter())
                .map(|k| self.width(ui, k.name.to_string()))
                .fold(0.0, f32::max);
            at.kind = Some(at.label);
            at.label += widest + 12.0;
        }
        let selected = self.state.selected;
        let store = self.analyses[listed].store;
        scroll.show_rows(ui, ROW, total, |ui, range| {
            for id in shown.rows(range) {
                let Some(item) = store.items().get(&id) else {
                    continue;
                };
                let on = selected == Some((listed, id));
                self.row(ui, listed, item, at, on, lit, go);
            }
        });
    }

    /// One list row: the item's start, its kind where `at` has a column for it, its label, and a
    /// mark where it holds a condition.
    #[expect(clippy::too_many_arguments, reason = "each is the row's own")]
    fn row(
        &self,
        ui: &mut Ui,
        a: usize,
        item: &Item,
        at: Columns,
        selected: bool,
        lit: &mut Option<Span>,
        go: &mut Option<(usize, Id)>,
    ) {
        let p = self.palette;
        let an = &self.analyses[a];
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
        let time = self.time(item.start);
        let label = heading(an.schema, an.labels, item);
        let kind = (at.kind)
            .and_then(|_| an.schema.kind(item.id.kind))
            .map(|k| k.name.to_string());
        let marked = condition(item);
        let named = match &kind {
            Some(k) => format!("{time}  {k}  {label}"),
            None => format!("{time}  {label}"),
        };
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &named));
        let painter = ui.painter_at(rect);
        if response.hovered() {
            painter.rect_filled(rect, 0.0, p.wash);
            *lit = Some(Span {
                start: item.start,
                end: item.end.unwrap_or(item.start),
            });
        }
        if selected {
            let ring = Stroke::new(1.0, p.ink);
            painter.rect_stroke(rect, 2.0, ring, StrokeKind::Inside);
        }
        let y = rect.center().y;
        let x = |dx: f32| pos2(rect.left() + dx, y);
        let (figures, labels) = (p.figures.clone(), p.labels.clone());
        painter.text(x(4.0), Align2::LEFT_CENTER, time, figures, p.ink_2);
        if let (Some(k), Some(dx)) = (kind, at.kind) {
            painter.text(x(dx), Align2::LEFT_CENTER, k, labels.clone(), p.ink_2);
        }
        painter.text(
            x(at.label),
            Align2::LEFT_CENTER,
            label,
            labels.clone(),
            p.ink,
        );
        if marked {
            let right = pos2(rect.right() - 4.0, y);
            painter.text(right, Align2::RIGHT_CENTER, "!", labels, p.attention_ink);
        }
        if response.clicked() {
            *go = Some((a, item.id));
        }
    }

    fn width(&self, ui: &Ui, text: String) -> f32 {
        let galley =
            (ui.painter()).layout_no_wrap(text, self.palette.labels.clone(), self.palette.ink);
        galley.size().x
    }

    /// Where a row's text starts: past a time with the most figures.
    fn label_at(&self, ui: &Ui) -> f32 {
        let widest = ui.painter().layout_no_wrap(
            self.time(Tick::MAX / 2),
            self.palette.figures.clone(),
            self.palette.ink,
        );
        widest.size().x + 12.0
    }

    fn text_tab(&mut self, ui: &mut Ui, lit: &mut Option<Span>, go: &mut Option<(usize, Id)>) {
        let texted: Vec<usize> = (self.analyses.iter().enumerate())
            .filter(|(_, a)| a.schema.kinds.iter().any(|k| k.text.is_some()))
            .map(|(k, _)| k)
            .collect();
        let Some(&first) = texted.first() else {
            ui.label(self.quiet("No analysis here reads as text."));
            return;
        };
        let held = |k: &usize| texted.contains(k);
        let chosen = (self.state.texted.filter(held))
            .or(self.state.selected.map(|(a, _)| a).filter(held))
            .unwrap_or(first);
        if texted.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                for &k in &texted {
                    if self
                        .toggle(ui, k == chosen, self.analyses[k].name)
                        .clicked()
                    {
                        self.state.texted = Some(k);
                    }
                }
            });
            ui.separator();
        }
        let index = &self.state.indexes[chosen];
        if !index.read() {
            ui.label(self.quiet("Still reading the decode."));
            return;
        }
        if index.lines.is_empty() {
            ui.label(self.quiet("No text decoded here yet."));
            return;
        }
        let label_at = self.label_at(ui);
        (ScrollArea::vertical())
            .auto_shrink(false)
            .id_salt(("panel text", chosen))
            .show_rows(ui, ROW, index.lines.len(), |ui, range| {
                for line in &index.lines[range] {
                    self.text_row(ui, chosen, line, label_at, lit, go);
                }
            });
    }

    /// One line of text after its start: the character under the pointer lights its item's
    /// span, and a click selects that item.
    fn text_row(
        &self,
        ui: &mut Ui,
        a: usize,
        line: &Line,
        label_at: f32,
        lit: &mut Option<Span>,
        go: &mut Option<(usize, Id)>,
    ) {
        let p = self.palette;
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
        let time = self.time(line.start);
        let named = format!("{time}  {}", line.text);
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &named));
        let painter = ui.painter_at(rect);
        let y = rect.center().y;
        painter.text(
            pos2(rect.left() + 4.0, y),
            Align2::LEFT_CENTER,
            time,
            p.figures.clone(),
            p.ink_2,
        );
        let galley = painter.layout_no_wrap(line.text.clone(), p.figures.clone(), p.ink);
        let origin = pos2(rect.left() + label_at, y - galley.size().y / 2.0);
        let under = response.hover_pos().and_then(|q| {
            let placed = galley.rows.first()?;
            let x = q.x - origin.x - placed.pos.x;
            let k = (placed.row.glyphs.iter()).position(|g| {
                let r = g.logical_rect();
                r.left() <= x && x < r.right()
            })?;
            let cell = placed.row.glyphs[k]
                .logical_rect()
                .translate(origin.to_vec2() + placed.pos.to_vec2());
            Some((k, cell))
        });
        if let Some((k, cell)) = under
            && let Some(item) = line
                .from
                .get(k)
                .and_then(|id| self.analyses[a].store.items().get(id))
        {
            painter.rect_filled(cell, 0.0, p.wash);
            *lit = Some(Span {
                start: item.start,
                end: item.end.unwrap_or(item.start),
            });
            if response.clicked() {
                *go = Some((a, item.id));
            }
        }
        painter.galley(origin, galley, p.ink);
    }

    fn selected_tab(&mut self, ui: &mut Ui, lit: &mut Option<Span>, go: &mut Option<(usize, Id)>) {
        let found = self.state.selected.and_then(|(a, id)| {
            let item = self.analyses.get(a)?.store.items().get(&id)?;
            Some((a, item))
        });
        let Some((a, item)) = found else {
            let note = match self.state.selected {
                None => "Select an item on a track to see it here.",
                Some(_) => "The item selected is not decoded here.",
            };
            ui.label(self.quiet(note));
            return;
        };
        let f = self.found((a, item.id));
        let an = &self.analyses[a];
        let kind = an.schema.kind(item.id.kind).map_or("item", |k| &k.name);
        // Fixed above the item, so stepping through a run keeps the pointer on the control.
        ui.horizontal(|ui| {
            let previous = Button::new(self.text(format!("◂ previous {kind}"))).frame(false);
            if ui.add_enabled(f.previous.is_some(), previous).clicked() {
                *go = f.previous.map(|id| (a, id));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let next = Button::new(self.text(format!("next {kind} ▸"))).frame(false);
                if ui.add_enabled(f.next.is_some(), next).clicked() {
                    *go = f.next.map(|id| (a, id));
                }
            });
        });
        ui.separator();
        ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("panel selected")
            .show(ui, |ui| self.show(ui, a, item, &f, lit, go));
    }

    fn show(
        &self,
        ui: &mut Ui,
        a: usize,
        item: &Item,
        found: &Found,
        lit: &mut Option<Span>,
        go: &mut Option<(usize, Id)>,
    ) {
        let an = &self.analyses[a];
        let schema = an.schema;
        let span = |i: &Item| Span {
            start: i.start,
            end: i.end.unwrap_or(i.start),
        };
        let head = ui.add(
            egui::Label::new(self.text(heading(schema, an.labels, item)).strong())
                .sense(Sense::hover()),
        );
        if head.hovered() {
            *lit = Some(span(item));
        }
        // The status on a line of its own, so an open item closing moves nothing after it.
        ui.label(self.figures(self.when(item.start, item.end)));
        ui.label(self.text(status(schema, item)));
        let cite = citation(an.name, schema, &item.id);
        ui.horizontal_wrapped(|ui| {
            ui.label(self.quiet(cite.clone()));
            let copy = ui.add(Button::new(self.quiet("copy")).frame(false));
            if copy.on_hover_text("Copy the citation").clicked() {
                ui.ctx().copy_text(cite.clone());
            }
            let all = ui.add(Button::new(self.quiet("copy item")).frame(false));
            if all.on_hover_text("Copy this tab as text").clicked() {
                ui.ctx().copy_text(self.as_text(a, item, found));
            }
        });
        ui.add_space(6.0);

        self.flat(ui, schema, item, ("panel fields", item.id));
        for f in item.fields.iter().filter(|f| f.span.is_some()) {
            let field = field_name(schema, item, f);
            let section = CollapsingHeader::new(self.figures(part_title(schema, item, f)))
                .id_salt(("panel part", item.id, f.field.0))
                .show(ui, |ui| self.part(ui, schema, item, f, &field));
            if section.header_response.hovered() {
                *lit = f.span;
            }
        }
        for sentence in unknowns(schema, item) {
            ui.label(self.quiet(sentence));
        }
        if !item.links.is_empty() {
            ui.add_space(6.0);
            Grid::new(("panel links", item.id))
                .num_columns(2)
                .show(ui, |ui| {
                    for (l, r) in &item.links {
                        let role = link_role(schema, item, *l);
                        let (to, target) = self.link_to(a, r);
                        ui.label(self.quiet(role.to_string()));
                        // A link to an item the panel holds selects it.
                        let held = target.and_then(|(b, id)| {
                            Some((b, self.analyses.get(b)?.store.items().get(&id)?))
                        });
                        let link =
                            ui.add(egui::Label::new(self.figures(to)).sense(if held.is_some() {
                                Sense::click()
                            } else {
                                Sense::hover()
                            }));
                        if let Some((b, linked)) = held {
                            if link.hovered() {
                                *lit = Some(span(linked));
                            }
                            if link.clicked() {
                                *go = Some((b, linked.id));
                            }
                        }
                        ui.end_row();
                    }
                });
        }
        if !found.readers.is_empty() {
            ui.add_space(6.0);
            for &(b, id, link) in &found.readers {
                let Some(reader) = self.analyses[b].store.items().get(&id) else {
                    continue;
                };
                let rb = &self.analyses[b];
                let title = self.text(self.reader_title(b, reader, link));
                let section = CollapsingHeader::new(title)
                    .id_salt(("panel reader", item.id, b, id))
                    .show(ui, |ui| {
                        self.flat(ui, rb.schema, reader, ("panel reader fields", id));
                        if ui.button(self.text("select it")).clicked() {
                            *go = Some((b, id));
                        }
                    });
                if section.header_response.hovered() {
                    *lit = Some(span(reader));
                }
            }
            if let Some(more) = more_readers(found) {
                ui.label(self.quiet(more));
            }
        }
        // Last, so it moves nothing when it goes.
        if found.reading {
            ui.add_space(6.0);
            ui.label(
                self.quiet("Still reading the decode: its neighbours and readers may be missing."),
            );
        }
    }

    /// An item's fields without a span and with a value, as name and value.
    fn flat(&self, ui: &mut Ui, schema: &Schema, item: &Item, salt: (&str, Id)) {
        Grid::new(salt).num_columns(2).show(ui, |ui| {
            for (name, value) in flat_fields(schema, item) {
                ui.label(self.quiet(name));
                ui.label(self.figures(value));
                ui.end_row();
            }
        });
    }

    /// Where a link of an item of analysis `a` goes: its citation, and the item it names by its
    /// analysis where the panel has that analysis.
    fn link_to(&self, a: usize, r: &Ref) -> (String, Option<(usize, Id)>) {
        let an = &self.analyses[a];
        match (r, an.reads) {
            (Ref::Own(id), _) => (citation(an.name, an.schema, id), Some((a, *id))),
            (Ref::Upstream(id), Some(u)) => {
                let up = &self.analyses[u];
                (citation(up.name, up.schema, id), Some((u, *id)))
            }
            (Ref::Upstream(id), None) => (format!("upstream {}", id.anchor), None),
            _ => ("?".to_owned(), None),
        }
    }

    /// An item of analysis `b` that reads the selected one by its link `link`: "read as AP read
    /// DRW 0x...  (ADIv5, its request)".
    fn reader_title(&self, b: usize, reader: &Item, link: LinkId) -> String {
        let rb = &self.analyses[b];
        format!(
            "read as {}  ({}, its {})",
            heading(rb.schema, rb.labels, reader),
            rb.name,
            link_role(rb.schema, reader, link)
        )
    }

    /// What the Selected tab shows of `item`, an item of analysis `a`, as lines of text: what its
    /// sections hold when closed, and none of their bits.
    fn as_text(&self, a: usize, item: &Item, found: &Found) -> String {
        let an = &self.analyses[a];
        let schema = an.schema;
        let mut lines = vec![
            heading(schema, an.labels, item),
            self.when(item.start, item.end),
            status(schema, item),
            citation(an.name, schema, &item.id),
        ];
        lines.extend((flat_fields(schema, item).into_iter()).map(|(n, v)| format!("{n}  {v}")));
        let parts = item.fields.iter().filter(|f| f.span.is_some());
        lines.extend(parts.map(|f| part_title(schema, item, f)));
        lines.extend(unknowns(schema, item));
        for (l, r) in &item.links {
            lines.push(format!(
                "{}  {}",
                link_role(schema, item, *l),
                self.link_to(a, r).0
            ));
        }
        for &(b, id, link) in &found.readers {
            if let Some(reader) = self.analyses[b].store.items().get(&id) {
                lines.push(self.reader_title(b, reader, link));
            }
        }
        lines.extend(more_readers(found));
        lines.join("\n")
    }

    /// A spanned field's section: its span, who drives it, and its sequence's elements where it
    /// has one of its name.
    fn part(
        &self,
        ui: &mut Ui,
        schema: &Schema,
        item: &Item,
        f: &escapement_tickwave_decode::Field,
        field: &str,
    ) {
        if let Some(span) = f.span {
            ui.label(self.figures(self.when(span.start, Some(span.end))));
        }
        if let Some(role) = f.driver.and_then(|d| schema.roles.get(usize::from(d.0))) {
            ui.label(self.quiet(format!("driven by {role}")));
        }
        let kind = schema.kind(item.id.kind);
        let Some((k, decl)) =
            kind.and_then(|k| (k.sequences.iter().enumerate()).find(|(_, s)| s.name == field))
        else {
            return;
        };
        let elements = item.seq(escapement_tickwave_decode::SeqId(k as u16));
        Grid::new(("panel elements", item.id, f.field.0))
            .num_columns(3)
            .show(ui, |ui| {
                for (n, e) in elements.iter().enumerate().take(ELEMENTS_AT_MOST) {
                    let value = match &decl.element {
                        ElementType::Name(names) => names
                            .get(usize::from(e.value))
                            .map_or_else(|| "?".to_owned(), |v| v.to_string()),
                        _ => e.value.to_string(),
                    };
                    // From the part's start, which the line above gives in full.
                    let at = match (e.at, f.span) {
                        (Observation::Wire(t), Some(span)) => self.offset(span.start, t),
                        (Observation::Wire(t), None) => self.time(t),
                        _ => "from upstream".to_owned(),
                    };
                    ui.label(self.quiet(n.to_string()));
                    ui.label(self.figures(value));
                    ui.label(self.quiet(at));
                    ui.end_row();
                }
            });
        if elements.len() > ELEMENTS_AT_MOST {
            ui.label(self.quiet(format!("and {} more", elements.len() - ELEMENTS_AT_MOST)));
        }
    }
}

/// An item's status: complete, incomplete and why, or open.
fn status(schema: &Schema, item: &Item) -> String {
    match item.status {
        Some(Status::Complete) => "complete".to_owned(),
        Some(Status::Incomplete(c)) => {
            let why = schema.causes.get(usize::from(c.0)).map_or("?", |w| w);
            format!("incomplete: {why}")
        }
        _ => "open".to_owned(),
    }
}

fn field_name(schema: &Schema, item: &Item, f: &escapement_tickwave_decode::Field) -> String {
    (schema.kind(item.id.kind))
        .and_then(|k| k.fields.get(usize::from(f.field.0)))
        .map_or_else(|| "?".to_owned(), |d| d.name.to_string())
}

/// An item's fields without a span and with a value known, as name and value.
fn flat_fields(schema: &Schema, item: &Item) -> Vec<(String, String)> {
    (item.fields.iter())
        .filter(|f| f.span.is_none() && !matches!(f.value, Value::Unknown { .. }))
        .map(|f| {
            let value = value_text(schema, item.id.kind, f.field, &f.value);
            (field_name(schema, item, f), value)
        })
        .collect()
}

/// A spanned field's section heading: its name and its value, "?" where it is not known.
fn part_title(schema: &Schema, item: &Item, f: &escapement_tickwave_decode::Field) -> String {
    let field = field_name(schema, item, f);
    let value = match f.value {
        Value::Unknown { .. } => "?".to_owned(),
        _ => value_text(schema, item.id.kind, f.field, &f.value),
    };
    match value.as_str() {
        "" => field,
        v => format!("{field}  {v}"),
    }
}

/// The values an item could not know, one sentence for each reason and need, naming the fields
/// it holds for.
fn unknowns(schema: &Schema, item: &Item) -> Vec<String> {
    let mut by: BTreeMap<(u16, Option<u16>), Vec<String>> = BTreeMap::new();
    for f in &item.fields {
        if let Value::Unknown { reason, needs } = f.value {
            let key = (reason.0, needs.map(|n| n.0));
            by.entry(key).or_default().push(field_name(schema, item, f));
        }
    }
    (by.into_iter())
        .map(|((reason, needs), fields)| {
            let why = schema.reasons.get(usize::from(reason)).map_or("?", |w| w);
            let need = needs
                .and_then(|n| schema.needs.get(usize::from(n)))
                .map_or(String::new(), |n| format!("; needs {n}"));
            format!("{} unknown: {why}{need}", fields.join(", "))
        })
        .collect()
}

fn link_role<'s>(schema: &'s Schema, item: &Item, link: LinkId) -> &'s str {
    (schema.kind(item.id.kind))
        .and_then(|k| k.links.get(usize::from(link.0)))
        .map_or("?", |d| &d.role)
}

/// How many more items read the selected one than the tab lists, where it lists fewer.
fn more_readers(found: &Found) -> Option<String> {
    let more = found.read_by.checked_sub(found.readers.len())?;
    (more > 0).then(|| format!("and {more} more that read it"))
}

impl Widget for Panel<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        self.read(ui.ctx());
        let size = match self.under {
            Some(h) => vec2(ui.available_width(), Self::reserved_height(self.state, h)),
            None => vec2(
                Self::reserved(self.state, self.width),
                ui.available_height(),
            ),
        };
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        // The control at the top right, where it stays open or collapsed.
        let control = Rect::from_min_size(
            pos2(rect.right() - CONTROL, rect.top()),
            vec2(CONTROL, CONTROL),
        );
        let (sign, hint) = if self.state.collapsed {
            ("⇤", "Open the panel")
        } else {
            ("⇥", "Collapse the panel")
        };
        let toggle = ui.put(control, Button::new(self.text(sign)).frame(false));
        if toggle.on_hover_text(hint).clicked() {
            self.state.collapsed = !self.state.collapsed;
        }
        let mut lit = None;
        let mut go = None;
        if !self.state.collapsed {
            // The tabs, left of the control.
            let tabs = Rect::from_min_max(
                rect.left_top(),
                pos2(control.left() - 4.0, control.bottom()),
            );
            let row = UiBuilder::new()
                .max_rect(tabs)
                .layout(Layout::left_to_right(Align::Center));
            ui.scope_builder(row, |ui| {
                let tabs = [
                    (Tab::Selected, "Selected"),
                    (Tab::List, "List"),
                    (Tab::Text, "Text"),
                ];
                for (tab, name) in tabs {
                    if self.toggle(ui, self.state.tab == tab, name).clicked() {
                        self.state.tab = tab;
                    }
                }
            });
            let body = Rect::from_min_max(pos2(rect.left(), control.bottom()), rect.right_bottom());
            let builder = UiBuilder::new()
                .max_rect(body)
                .layout(Layout::top_down(Align::Min));
            ui.scope_builder(builder, |ui| {
                ui.set_width(body.width());
                match self.state.tab {
                    Tab::Selected => self.selected_tab(ui, &mut lit, &mut go),
                    Tab::List => self.list_tab(ui, &mut lit, &mut go),
                    Tab::Text => self.text_tab(ui, &mut lit, &mut go),
                }
            });
        }
        self.state.lit = lit;
        if let Some(to) = go {
            self.state.selected = Some(to);
            self.state.lit = None;
        }
        response
    }
}
