//! The side panel's Selected section: the item the reader selected on a track, in full. Its host
//! gives it a fixed width and reserves it while a decode track shows, so selecting an item never
//! narrows the plot under the reader; with nothing selected it holds a note.
//!
//! An item shows its fullest label, its time and duration, its status and its citation; then its
//! unspanned fields flat, its spanned fields as sections that open to their span and bits, the
//! values it could not know as one sentence for each reason, and its links.

use std::collections::BTreeMap;

use egui::{Align, CollapsingHeader, Grid, Layout, Response, RichText, Ui, Widget, vec2};
use escapement_tickwave_decode::{
    ElementType, Item, Observation, Ref, Schema, Status, Tick, Value, citation,
};
use escapement_tickwave_view::format::mapping::Mapping;
use escapement_tickwave_view::format::model::Ratio;
use escapement_tickwave_view::readouts::format_interval;

use crate::palette::Palette;
use crate::track::{Labels, heading, value_text};

/// The most elements a part's section lists; the rest are counted.
const ELEMENTS_AT_MOST: usize = 64;

/// The item a panel shows: its analysis's name and schema, the labels its track draws it with,
/// and the analysis it is stacked on, by name and schema, to cite what it links to there.
pub struct Selected<'a> {
    pub analysis: &'a str,
    pub schema: &'a Schema,
    pub labels: &'a Labels,
    pub item: &'a Item,
    pub upstream: Option<(&'a str, &'a Schema)>,
}

/// The side panel, `width` points wide.
pub struct Panel<'a> {
    palette: &'a Palette,
    width: f32,
    timebase: Ratio,
    map: Mapping,
    selected: Option<Selected<'a>>,
}

impl<'a> Panel<'a> {
    /// `map` places the selected item's ticks on the recording's axis, whose ticks are
    /// `timebase`'s.
    pub fn new(palette: &'a Palette, width: f32, timebase: Ratio, map: Mapping) -> Self {
        Self {
            palette,
            width,
            timebase,
            map,
            selected: None,
        }
    }

    pub fn selected(mut self, selected: Option<Selected<'a>>) -> Self {
        self.selected = selected;
        self
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

    fn show(&self, ui: &mut Ui, sel: &Selected) {
        let (schema, item) = (sel.schema, sel.item);
        let kind = schema.kind(item.id.kind);
        ui.label(self.text(heading(schema, sel.labels, item)).strong());
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
        ui.label(self.quiet(citation(sel.analysis, schema, &item.id)));
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
        Grid::new(("panel fields", item.id))
            .num_columns(2)
            .show(ui, |ui| {
                for f in item.fields.iter().filter(|f| f.span.is_none()) {
                    if matches!(f.value, Value::Unknown { .. }) {
                        continue;
                    }
                    ui.label(self.quiet(name(f)));
                    ui.label(self.figures(value_text(schema, item.id.kind, f.field, &f.value)));
                    ui.end_row();
                }
            });
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
            CollapsingHeader::new(title)
                .id_salt(("panel part", item.id, f.field.0))
                .show(ui, |ui| self.part(ui, sel, f, &field));
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
                        let to = match (r, sel.upstream) {
                            (Ref::Own(id), _) => citation(sel.analysis, schema, id),
                            (Ref::Upstream(id), Some((a, s))) => citation(a, s, id),
                            (Ref::Upstream(id), None) => format!("upstream {}", id.anchor),
                            _ => "?".to_owned(),
                        };
                        ui.label(self.quiet(role.to_string()));
                        ui.label(self.figures(to));
                        ui.end_row();
                    }
                });
        }
    }

    /// A spanned field's section: its span, who drives it, and its sequence's elements where it
    /// has one of its name.
    fn part(
        &self,
        ui: &mut Ui,
        sel: &Selected,
        f: &escapement_tickwave_decode::Field,
        field: &str,
    ) {
        let (schema, item) = (sel.schema, sel.item);
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
    fn ui(self, ui: &mut Ui) -> Response {
        let size = vec2(self.width, ui.available_height());
        ui.allocate_ui_with_layout(size, Layout::top_down(Align::Min), |ui| {
            ui.set_width(self.width);
            match &self.selected {
                Some(sel) => self.show(ui, sel),
                None => {
                    ui.label(self.quiet("Select an item on a track to see it here."));
                }
            }
        })
        .response
    }
}
