//! The side panel: the item the reader selected on a track, in full. Its host gives it a fixed
//! width and reserves it while a decode track shows, so selecting an item never narrows the plot
//! under the reader; with nothing selected it holds a note. Its collapse control sits at the top
//! right, where it stays open or collapsed.
//!
//! An item shows its fullest label, its time and duration, its status and its citation; then its
//! unspanned fields flat, its spanned fields as sections that open to their span and bits, the
//! values it could not know as one sentence for each reason, its links, and the items in the
//! analyses stacked on its own that read it ("read as"). Above it, fixed where they are pressed,
//! previous and next step to the items of its kind before and after it. Pointing at a section,
//! the heading or a reader lights its span, which the host passes to its tracks and graph.
//!
//! The List tab shows one analysis's items as rows, in time order: each its start, its label and,
//! where it holds a condition, a mark. Kinds can be left out, or every item but those holding a
//! condition. A row the pointer rests on lights its span, and a click selects it. When the
//! selection changes the list scrolls to it.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Unbounded};

use egui::{
    Align, Align2, Button, CollapsingHeader, Grid, Layout, Rect, Response, RichText, Role,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, UiBuilder, Widget, WidgetInfo, pos2, vec2,
};
use escapement_tickwave_decode::{
    ElementType, Id, Item, KindId, LinkId, Observation, Ref, Schema, Span, Status, Tick, Value,
    citation,
};
use escapement_tickwave_view::format::mapping::Mapping;
use escapement_tickwave_view::format::model::Ratio;
use escapement_tickwave_view::readouts::format_interval;

use crate::palette::Palette;
use crate::track::{Labels, condition, heading, value_text};

/// The most elements a part's section lists; the rest are counted.
const ELEMENTS_AT_MOST: usize = 64;
/// The most items a "read as" list shows; the rest are counted.
const READERS_AT_MOST: usize = 32;
/// The collapse control's size, and the height of the row it heads.
const CONTROL: f32 = 24.0;
/// A list row's height.
const ROW: f32 = 18.0;

/// One analysis as the panel reads it: its name, its schema, the labels its track draws it with,
/// its items as its store keeps them, and the analysis it is stacked on, by its index among the
/// panel's.
pub struct Analysis<'a> {
    pub name: &'a str,
    pub schema: &'a Schema,
    pub labels: &'a Labels,
    pub items: &'a BTreeMap<Id, Item>,
    pub reads: Option<usize>,
}

/// The panel's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    /// The selected item, in full.
    #[default]
    Selected,
    /// One analysis's items as rows.
    List,
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
    /// Collapsed to its control, by the reader.
    pub collapsed: bool,
    /// What the pointer rests on in the panel this frame: its span on the items' timeline.
    pub lit: Option<Span>,
    found: Option<Found>,
    /// The kinds the List tab leaves out, by analysis.
    hidden: BTreeSet<(usize, KindId)>,
    /// The List tab shows only items holding a condition.
    conditions: bool,
    rows: Option<Rows>,
    /// The selection the List tab last scrolled to.
    scrolled: Option<(usize, Id)>,
}

/// The ids a List tab shows, in order, kept while its analysis, its store's size and its filter
/// hold.
#[derive(Clone, Debug)]
struct Rows {
    key: (usize, usize, Vec<KindId>, bool),
    ids: Vec<Id>,
}

/// The selected item's neighbours of its kind and the items that read it, kept while the
/// selection and every store's size hold: a store that grows may hold new ones.
#[derive(Clone, Debug)]
struct Found {
    selected: (usize, Id),
    sizes: Vec<usize>,
    previous: Option<Id>,
    next: Option<Id>,
    /// Each reader's analysis, id, and the link by which it reads the selected item.
    readers: Vec<(usize, Id, LinkId)>,
}

/// The items of `id`'s kind just before and after it. Ids order by anchor first, so these are
/// the nearest in time.
fn neighbours(items: &BTreeMap<Id, Item>, id: Id) -> (Option<Id>, Option<Id>) {
    let same = |k: &&Id| k.kind == id.kind;
    let previous = items.range(..id).map(|(k, _)| k).rev().find(same);
    let next = (items.range((Excluded(id), Unbounded)))
        .map(|(k, _)| k)
        .find(same);
    (previous.copied(), next.copied())
}

/// The side panel, `width` points wide when open.
pub struct Panel<'a> {
    palette: &'a Palette,
    state: &'a mut PanelState,
    analyses: &'a [Analysis<'a>],
    width: f32,
    timebase: Ratio,
    map: Mapping,
}

impl<'a> Panel<'a> {
    /// The width a collapsed panel keeps: its control's.
    pub const COLLAPSED: f32 = CONTROL;

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
            timebase,
            map,
        }
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
        format_interval(self.map.axis_saturating(t), self.timebase, self.map.m())
    }

    fn interval(&self, from: Tick, to: Tick) -> String {
        let ticks = self.map.axis_saturating(to) - self.map.axis_saturating(from);
        format_interval(ticks, self.timebase, self.map.m())
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

    fn figures(&self, t: impl Into<String>) -> RichText {
        RichText::new(t)
            .font(self.palette.figures.clone())
            .color(self.palette.ink)
    }

    /// The neighbours and readers of `selected`, found again only where it or a store changed.
    fn found(&mut self, selected: (usize, Id)) -> Found {
        let sizes: Vec<usize> = self.analyses.iter().map(|a| a.items.len()).collect();
        if let Some(f) = &self.state.found
            && f.selected == selected
            && f.sizes == sizes
        {
            return f.clone();
        }
        let (a, id) = selected;
        let (previous, next) = neighbours(self.analyses[a].items, id);
        let mut readers = Vec::new();
        for (b, reader) in self.analyses.iter().enumerate() {
            if reader.reads != Some(a) {
                continue;
            }
            for item in reader.items.values() {
                for &(link, r) in &item.links {
                    if r == Ref::Upstream(id) {
                        readers.push((b, item.id, link));
                    }
                }
            }
        }
        let f = Found {
            selected,
            sizes,
            previous,
            next,
            readers,
        };
        self.state.found = Some(f.clone());
        f
    }

    /// The ids analysis `a`'s List tab shows, found again only where its store or filter changed.
    fn rows(&mut self, a: usize) -> Rows {
        let items = self.analyses[a].items;
        let hidden: Vec<KindId> = (self.state.hidden.iter())
            .filter(|(b, _)| *b == a)
            .map(|&(_, k)| k)
            .collect();
        let key = (a, items.len(), hidden, self.state.conditions);
        if let Some(r) = self.state.rows.take()
            && r.key == key
        {
            return r;
        }
        let ids = (items.values())
            .filter(|i| !key.2.contains(&i.id.kind))
            .filter(|i| !key.3 || condition(i))
            .map(|i| i.id)
            .collect();
        Rows { key, ids }
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
                    if ui
                        .selectable_label(k == listed, self.text(a.name))
                        .clicked()
                    {
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
                if (ui.selectable_label(shown, self.quiet(decl.name.to_string()))).clicked() {
                    if shown {
                        self.state.hidden.insert(kind);
                    } else {
                        self.state.hidden.remove(&kind);
                    }
                }
            }
            let only = self.state.conditions;
            if (ui.selectable_label(only, self.quiet("conditions only"))).clicked() {
                self.state.conditions = !only;
            }
        });
        ui.separator();
        let rows = self.rows(listed);
        if rows.ids.is_empty() {
            ui.label(self.quiet("No items here."));
            self.state.rows = Some(rows);
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
                && let Ok(k) = rows.ids.binary_search(&id)
            {
                // Within the list's extent, so the area has no overscroll to spring back from.
                let view = ui.available_height();
                let most = (rows.ids.len() as f32 * (ROW + gap) - gap - view).max(0.0);
                let at = k as f32 * (ROW + gap) - view / 2.0;
                scroll = scroll.vertical_scroll_offset(at.clamp(0.0, most));
            }
            self.state.scrolled = self.state.selected;
        }
        // The times' column, as wide as a time with the most figures.
        let widest = ui.painter().layout_no_wrap(
            self.time(Tick::MAX / 2),
            self.palette.figures.clone(),
            self.palette.ink,
        );
        let label_at = widest.size().x + 12.0;
        let selected = self.state.selected;
        scroll.show_rows(ui, ROW, rows.ids.len(), |ui, range| {
            for id in &rows.ids[range] {
                let Some(item) = self.analyses[listed].items.get(id) else {
                    continue;
                };
                let on = selected == Some((listed, *id));
                self.row(ui, listed, item, label_at, on, lit, go);
            }
        });
        self.state.rows = Some(rows);
    }

    /// One list row: the item's start, its label, and a mark where it holds a condition.
    #[expect(clippy::too_many_arguments, reason = "each is the row's own")]
    fn row(
        &self,
        ui: &mut Ui,
        a: usize,
        item: &Item,
        label_at: f32,
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
        let marked = condition(item);
        let named = format!("{time}  {label}");
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
        let at = |x: f32| pos2(x, y);
        let (figures, labels) = (p.figures.clone(), p.labels.clone());
        painter.text(
            at(rect.left() + 4.0),
            Align2::LEFT_CENTER,
            time,
            figures,
            p.ink_2,
        );
        painter.text(
            at(rect.left() + label_at),
            Align2::LEFT_CENTER,
            label,
            labels.clone(),
            p.ink,
        );
        if marked {
            let right = at(rect.right() - 4.0);
            painter.text(right, Align2::RIGHT_CENTER, "!", labels, p.attention_ink);
        }
        if response.clicked() {
            *go = Some((a, item.id));
        }
    }

    fn selected_tab(&mut self, ui: &mut Ui, lit: &mut Option<Span>, go: &mut Option<(usize, Id)>) {
        let found = self.state.selected.and_then(|(a, id)| {
            let item = self.analyses.get(a)?.items.get(&id)?;
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
        let kind = schema.kind(item.id.kind);
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
        let status = match item.status {
            Some(Status::Complete) => "complete".to_owned(),
            Some(Status::Incomplete(c)) => {
                let why = schema.causes.get(usize::from(c.0)).map_or("?", |w| w);
                format!("incomplete: {why}")
            }
            _ => "open".to_owned(),
        };
        // The status on a line of its own, so an open item closing moves nothing after it.
        let at = match item.end {
            Some(e) => format!(
                "{} for {}",
                self.time(item.start),
                self.interval(item.start, e)
            ),
            None => self.time(item.start),
        };
        ui.label(self.figures(at));
        ui.label(self.text(status));
        ui.label(self.quiet(citation(an.name, schema, &item.id)));
        ui.add_space(6.0);

        let name = |f: &escapement_tickwave_decode::Field| {
            kind.and_then(|k| k.fields.get(usize::from(f.field.0)))
                .map_or_else(|| "?".to_owned(), |d| d.name.to_string())
        };
        // Values it could not know, by their reason and need, each named once in a sentence.
        let mut unknown: BTreeMap<(u16, Option<u16>), Vec<String>> = BTreeMap::new();
        for f in &item.fields {
            if let Value::Unknown { reason, needs } = f.value {
                unknown
                    .entry((reason.0, needs.map(|n| n.0)))
                    .or_default()
                    .push(name(f));
            }
        }
        self.flat(ui, schema, item, ("panel fields", item.id));
        for f in item.fields.iter().filter(|f| f.span.is_some()) {
            let field = name(f);
            let value = match f.value {
                Value::Unknown { .. } => "?".to_owned(),
                _ => value_text(schema, item.id.kind, f.field, &f.value),
            };
            let title = match value.as_str() {
                "" => self.figures(field.clone()),
                v => self.figures(format!("{field}  {v}")),
            };
            let section = CollapsingHeader::new(title)
                .id_salt(("panel part", item.id, f.field.0))
                .show(ui, |ui| self.part(ui, schema, item, f, &field));
            if section.header_response.hovered() {
                *lit = f.span;
            }
        }
        for ((reason, needs), fields) in &unknown {
            let why = schema.reasons.get(usize::from(*reason)).map_or("?", |w| w);
            let need = needs
                .and_then(|n| schema.needs.get(usize::from(n)))
                .map_or(String::new(), |n| format!("; needs {n}"));
            ui.label(self.quiet(format!("{} unknown: {why}{need}", fields.join(", "))));
        }
        if !item.links.is_empty() {
            ui.add_space(6.0);
            Grid::new(("panel links", item.id))
                .num_columns(2)
                .show(ui, |ui| {
                    for (l, r) in &item.links {
                        let role = (kind.and_then(|k| k.links.get(usize::from(l.0))))
                            .map_or("?", |d| &d.role);
                        let (to, target) = match (r, an.reads) {
                            (Ref::Own(id), _) => (citation(an.name, schema, id), Some((a, *id))),
                            (Ref::Upstream(id), Some(u)) => {
                                let up = &self.analyses[u];
                                (citation(up.name, up.schema, id), Some((u, *id)))
                            }
                            (Ref::Upstream(id), None) => (format!("upstream {}", id.anchor), None),
                            _ => ("?".to_owned(), None),
                        };
                        ui.label(self.quiet(role.to_string()));
                        // A link to an item the panel holds selects it.
                        let held = target
                            .and_then(|(b, id)| Some((b, self.analyses.get(b)?.items.get(&id)?)));
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
            for &(b, id, link) in found.readers.iter().take(READERS_AT_MOST) {
                let Some(reader) = self.analyses[b].items.get(&id) else {
                    continue;
                };
                let rb = &self.analyses[b];
                let role = (rb.schema.kind(id.kind))
                    .and_then(|k| k.links.get(usize::from(link.0)))
                    .map_or("?", |d| &d.role);
                let title = self.text(format!(
                    "read as {}  ({}, its {role})",
                    heading(rb.schema, rb.labels, reader),
                    rb.name
                ));
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
            if found.readers.len() > READERS_AT_MOST {
                let more = found.readers.len() - READERS_AT_MOST;
                ui.label(self.quiet(format!("and {more} more that read it")));
            }
        }
    }

    /// An item's fields without a span and with a value, as name and value.
    fn flat(&self, ui: &mut Ui, schema: &Schema, item: &Item, salt: (&str, Id)) {
        let kind = schema.kind(item.id.kind);
        Grid::new(salt).num_columns(2).show(ui, |ui| {
            for f in item.fields.iter().filter(|f| f.span.is_none()) {
                if matches!(f.value, Value::Unknown { .. }) {
                    continue;
                }
                let name = (kind.and_then(|k| k.fields.get(usize::from(f.field.0))))
                    .map_or_else(|| "?".to_owned(), |d| d.name.to_string());
                ui.label(self.quiet(name));
                ui.label(self.figures(value_text(schema, item.id.kind, f.field, &f.value)));
                ui.end_row();
            }
        });
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
            let at = format!(
                "{} for {}",
                self.time(span.start),
                self.interval(span.start, span.end)
            );
            ui.label(self.figures(at));
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

impl Widget for Panel<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        let width = Self::reserved(self.state, self.width);
        let (rect, response) =
            ui.allocate_exact_size(vec2(width, ui.available_height()), Sense::hover());
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
                for (tab, name) in [(Tab::Selected, "Selected"), (Tab::List, "List")] {
                    if ui
                        .selectable_label(self.state.tab == tab, self.text(name))
                        .clicked()
                    {
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
