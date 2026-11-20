//! The Decoders tab: each analysis as bound, stacked under the one it reads, with its inputs, its
//! settings, each a control of its type, and how far its pass has come; and below them what can
//! be added: a wire decoder, with a channel for each port, a decoder that reads an analysis
//! already there, or a preset stack. A setting changed decodes the analysis's chain again.

use std::collections::BTreeMap;

use egui::{Align, Button, ComboBox, Grid, Layout, ScrollArea, TextEdit, Ui};
use escapement_tickwave_decode::recipe::{Analysis as Planned, Input, Recipe, Registry};
use escapement_tickwave_decode::{
    Bound, Definition, InputDecl, Notation, Setting, SettingDecl, SettingId, SettingType,
};

use super::Panel;

/// One analysis as the Decoders tab shows it.
pub(crate) struct Configured<'a> {
    /// Its id in the recipe, and what its recipe table binds each port to.
    pub id: &'a str,
    pub inputs: &'a [(String, Input)],
    /// Its track's name.
    pub label: &'a str,
    pub definition: &'a Definition,
    pub bound: &'a Bound,
    /// The analysis it reads, by its place among the tab's.
    pub reads: Option<usize>,
    /// How far its pass has come: the share of the recording behind it, until it finishes.
    pub done: Option<f64>,
    /// The items its pass has closed.
    pub items: u64,
}

/// What the Decoders tab shows and changes.
pub(crate) struct Stack<'a> {
    pub analyses: Vec<Configured<'a>>,
    pub registry: &'a Registry,
    /// The recording's channels a wire port can read, by name.
    pub channels: &'a [String],
    /// Why the last change asked for was refused.
    pub refused: Option<&'a str>,
    pub change: &'a mut Option<Change>,
}

/// A change the reader asked for in the Decoders tab.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Change {
    /// The analysis at this place bound with these settings, every one it takes.
    Settings(usize, Vec<(String, Setting)>),
    /// These analyses added; their ids may need renaming beside those there.
    Add(Recipe),
    /// The analysis at this place, and every one that reads it, removed.
    Remove(usize),
}

/// What the tab keeps between frames.
#[derive(Clone, Debug, Default)]
pub(crate) struct StackState {
    /// What is being added, its ports given channels in the form; and the control that opened it.
    adding: Option<(Opened, Recipe)>,
    /// A list setting's rows as typed, by analysis id, setting and row, until taken in.
    typed: BTreeMap<(String, usize, usize), String>,
}

/// The control that opened the form: a wire decoder or a preset, by its place in the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Opened {
    Decoder(usize),
    Preset(usize),
}

/// How far an analysis stacked on another sits in from it, in points.
const INDENT: f32 = 12.0;
/// A list setting's row as typed, wide enough for a 32-bit word in hex.
const TYPED: f32 = 96.0;

impl Panel<'_> {
    /// The Decoders tab: each analysis, then what can be added.
    pub(super) fn decoders_tab(&mut self, ui: &mut Ui) {
        let Some(stack) = self.stack.take() else {
            ui.label(self.quiet("Nothing here decodes."));
            return;
        };
        let mut change = None;
        ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("panel decoders")
            .show(ui, |ui| {
                if stack.analyses.is_empty() {
                    ui.label(self.quiet("Nothing decoded yet: add a decoder below."));
                }
                for k in 0..stack.analyses.len() {
                    let depth = depth(&stack.analyses, k);
                    ui.horizontal_top(|ui| {
                        ui.add_space(depth as f32 * INDENT);
                        ui.vertical(|ui| self.configured(ui, &stack, k, &mut change));
                    });
                    ui.add_space(6.0);
                }
                ui.separator();
                self.add_section(ui, &stack, &mut change);
            });
        if change.is_some() {
            *stack.change = change;
        }
    }

    /// One analysis: its name, what it is and reads, its pass, and its settings.
    fn configured(
        &mut self,
        ui: &mut Ui,
        stack: &Stack<'_>,
        k: usize,
        change: &mut Option<Change>,
    ) {
        let a = &stack.analyses[k];
        let def = a.definition;
        ui.horizontal(|ui| {
            ui.label(self.text(a.label).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let remove = ui.add(Button::new(self.quiet("remove")).frame(false));
                if remove
                    .on_hover_text("Remove it, and every decoder that reads it")
                    .clicked()
                {
                    *change = Some(Change::Remove(k));
                }
            });
        });
        let input = def.inputs.get(usize::from(a.bound.input().0));
        match input {
            Some(InputDecl::Upstream(u)) => {
                let from = a.reads.map_or("an analysis", |r| stack.analyses[r].label);
                let line =
                    ui.label(self.quiet(format!("{} {}, reading {from}", def.name, def.version)));
                if let Some(about) = &u.about {
                    line.on_hover_text(about.to_string());
                }
            }
            _ => {
                ui.label(self.quiet(format!("{} {}", def.name, def.version)));
                Grid::new(("panel ports", a.id))
                    .num_columns(2)
                    .show(ui, |ui| {
                        for p in &def.ports {
                            let port = ui.label(self.quiet(p.name.to_string()));
                            if let Some(about) = &p.about {
                                port.on_hover_text(about.to_string());
                            }
                            let channel = (a.inputs.iter())
                                .find(|(n, _)| p.name == n.as_str())
                                .and_then(|(_, i)| match i {
                                    Input::Channel(c) => Some(c.as_str()),
                                    Input::Analysis(_) => None,
                                });
                            ui.label(match channel {
                                Some(c) => self.figures(c),
                                None => self.quiet("none"),
                            });
                            ui.end_row();
                        }
                    });
            }
        }
        let items = grouped(a.items);
        let pass = match a.done {
            None => format!("decoded: {items} items"),
            Some(done) => format!(
                "decoding, {:.0} %: {items} items so far",
                (done * 100.0).floor()
            ),
        };
        ui.label(self.figures(pass));
        let applies: Vec<_> = (def.settings.iter().enumerate())
            .filter(|(_, d)| d.applies_to(a.bound.input()))
            .collect();
        if applies.is_empty() {
            return;
        }
        Grid::new(("panel settings", a.id))
            .num_columns(2)
            .show(ui, |ui| {
                for (s, decl) in applies {
                    let name = ui.label(self.quiet(decl.name.to_string()));
                    if let Some(about) = &decl.about {
                        name.on_hover_text(about.to_string());
                    }
                    ui.vertical(|ui| self.setting(ui, stack, k, s, decl, change));
                    ui.end_row();
                }
            });
    }
}

impl Panel<'_> {
    /// Setting `s` of the analysis at place `k`, as a control of its type.
    fn setting(
        &mut self,
        ui: &mut Ui,
        stack: &Stack<'_>,
        k: usize,
        s: usize,
        decl: &SettingDecl,
        change: &mut Option<Change>,
    ) {
        let a = &stack.analyses[k];
        let Ok(id) = u16::try_from(s).map(SettingId) else {
            return;
        };
        let set = |value: Setting| {
            let mut settings = held(a.definition, a.bound);
            if let Some(slot) = settings.iter_mut().find(|(n, _)| decl.name == n.as_str()) {
                slot.1 = value;
            }
            Change::Settings(k, settings)
        };
        match &decl.ty {
            SettingType::Flag { .. } => {
                let on = a.bound.flag(id) == Some(true);
                ui.horizontal(|ui| {
                    for (v, word) in [(false, "off"), (true, "on")] {
                        if self.toggle(ui, on == v, word).clicked() && on != v {
                            *change = Some(set(Setting::Flag(v)));
                        }
                    }
                });
            }
            &SettingType::Int { min, max, .. } => {
                let v = a.bound.int(id).unwrap_or(min);
                // As wide as its widest value, so the controls hold their places.
                let wide = [min, max]
                    .map(|x| self.width(ui, number(x, decl.notation, max)))
                    .into_iter()
                    .fold(0.0, f32::max);
                ui.horizontal(|ui| {
                    let less = Button::new(self.text("−")).frame(false);
                    if ui.add_enabled(v > min, less).clicked() {
                        *change = Some(set(Setting::Int(v - 1)));
                    }
                    let text = egui::Label::new(self.figures(number(v, decl.notation, max)));
                    ui.add_sized([wide, ui.spacing().interact_size.y], text);
                    let more = Button::new(self.text("+")).frame(false);
                    if ui.add_enabled(v < max, more).clicked() {
                        *change = Some(set(Setting::Int(v + 1)));
                    }
                });
            }
            SettingType::Choice { names, .. } => {
                let on = a.bound.choice(id);
                ui.horizontal_wrapped(|ui| {
                    for (c, name) in names.iter().enumerate() {
                        if self.toggle(ui, on == Some(c), name).clicked() && on != Some(c) {
                            *change = Some(set(Setting::Name(name.clone())));
                        }
                    }
                });
            }
            &SettingType::List {
                min, max, at_most, ..
            } => {
                let list = a.bound.list(id).unwrap_or(&[]).to_vec();
                let entry = decl
                    .entries
                    .as_ref()
                    .map_or("#".to_owned(), |e| e.to_string());
                let analysis = a.id.to_owned();
                // Each row, then one more to type a value into, while there is room.
                for row in 0..(list.len() + usize::from(list.len() < at_most)) {
                    let key = (analysis.clone(), s, row);
                    let was = list.get(row).map(|&v| number(v, decl.notation, max));
                    let mut text = (self.state.stack.typed.get(&key).cloned())
                        .or_else(|| was.clone())
                        .unwrap_or_default();
                    let mut invalid = false;
                    ui.horizontal(|ui| {
                        ui.label(self.quiet(format!("{entry} {row}")));
                        let edit = TextEdit::singleline(&mut text)
                            .font(self.palette.figures.clone())
                            .desired_width(TYPED)
                            .hint_text("new");
                        let r = ui.add(edit);
                        if r.changed() {
                            self.state.stack.typed.insert(key.clone(), text.clone());
                        }
                        let typed = self.state.stack.typed.contains_key(&key);
                        if r.lost_focus() && typed {
                            match parse(&text).filter(|n| (min..=max).contains(n)) {
                                Some(n) => {
                                    let mut l = list.clone();
                                    match l.get_mut(row) {
                                        Some(x) => *x = n,
                                        None => l.push(n),
                                    }
                                    *change = Some(set(Setting::List(l)));
                                    self.state.stack.typed.remove(&key);
                                }
                                None if text.trim().is_empty() && was.is_none() => {
                                    self.state.stack.typed.remove(&key);
                                }
                                None => {}
                            }
                        }
                        invalid = typed
                            && !r.has_focus()
                            && parse(&text).filter(|n| (min..=max).contains(n)).is_none();
                        if row < list.len() {
                            let remove = ui.add(Button::new(self.quiet("×")).frame(false));
                            if remove.on_hover_text("Remove this row").clicked() {
                                let mut l = list.clone();
                                l.remove(row);
                                *change = Some(set(Setting::List(l)));
                                self.state
                                    .stack
                                    .typed
                                    .retain(|(b, t, _), _| (b, *t) != (&analysis, s));
                            }
                        }
                    });
                    if invalid {
                        let range = format!(
                            "A whole number from {} to {}.",
                            number(min, decl.notation, max),
                            number(max, decl.notation, max)
                        );
                        ui.label(self.quiet(range));
                    }
                }
            }
            _ => {
                ui.label(self.quiet("not shown here"));
            }
        }
    }
}

/// Every setting an analysis takes, at the value it is bound with.
fn held(def: &Definition, bound: &Bound) -> Vec<(String, Setting)> {
    (def.settings.iter().enumerate())
        .filter(|(_, d)| d.applies_to(bound.input()))
        .filter_map(|(s, d)| {
            let id = SettingId(u16::try_from(s).ok()?);
            let value = match &d.ty {
                SettingType::Flag { .. } => Setting::Flag(bound.flag(id)?),
                SettingType::Int { .. } => Setting::Int(bound.int(id)?),
                SettingType::Choice { names, .. } => {
                    Setting::Name(names.get(bound.choice(id)?)?.clone())
                }
                SettingType::List { .. } => Setting::List(bound.list(id)?.to_vec()),
                _ => return None,
            };
            Some((d.name.to_string(), value))
        })
        .collect()
}

/// A setting's whole number as its notation reads, decimal where it gives none: in hex, as
/// many digits as `max` takes. A negative one reads in decimal whatever the notation.
fn number(v: i64, notation: Option<Notation>, max: i64) -> String {
    let bits = 64 - max.max(1).leading_zeros() as usize;
    match notation {
        _ if v < 0 => v.to_string(),
        Some(Notation::Hex) => format!("0x{v:0w$X}", w = bits.div_ceil(4)),
        Some(Notation::Binary) => format!("0b{v:0bits$b}"),
        _ => v.to_string(),
    }
}

/// A whole number typed in decimal, or in hex or binary with `0x` or `0b`, `_` between digits.
fn parse(text: &str) -> Option<i64> {
    let t: String = text.trim().chars().filter(|&c| c != '_').collect();
    let (negative, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest.to_owned()),
        None => (false, t),
    };
    let lower = t.to_ascii_lowercase();
    let (radix, digits) = match (lower.strip_prefix("0x"), lower.strip_prefix("0b")) {
        (Some(h), _) => (16, h),
        (_, Some(b)) => (2, b),
        _ => (10, lower.as_str()),
    };
    // A second sign, or one after `0x`, is not a whole number written out.
    if !digits.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        return None;
    }
    let v = i64::from_str_radix(digits, radix).ok()?;
    Some(if negative { v.checked_neg()? } else { v })
}

/// A count with its thousands grouped: 12,345.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (k, c) in digits.chars().enumerate() {
        if k > 0 && (digits.len() - k).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// How many analyses the one at `k` is stacked above.
fn depth(analyses: &[Configured<'_>], k: usize) -> usize {
    let mut n = 0;
    let mut at = analyses[k].reads;
    while let Some(up) = at.filter(|_| n < analyses.len()) {
        n += 1;
        at = analyses[up].reads;
    }
    n
}

impl Panel<'_> {
    /// What can be added: a wire decoder or a preset, each opening a form that gives its ports
    /// channels, or a decoder that reads an analysis already here, added at once.
    fn add_section(&mut self, ui: &mut Ui, stack: &Stack<'_>, change: &mut Option<Change>) {
        ui.label(self.text("Add").strong());
        if let Some(why) = stack.refused {
            ui.label(self.quiet(format!("Not done: {why}")));
        }
        let registry = stack.registry;
        let opened = self.state.stack.adding.as_ref().map(|(o, _)| *o);
        let open = |this: &mut Self, o: Opened, recipe: Recipe| {
            this.state.stack.adding = match opened == Some(o) {
                true => None,
                false => Some((o, recipe.on_channels(registry, stack.channels))),
            };
        };
        ui.label(self.quiet("reading the recording's channels"));
        ui.horizontal_wrapped(|ui| {
            for (e, entry) in registry.decoders.iter().enumerate() {
                if entry.definition.wire_input().is_none() {
                    continue;
                }
                let on = opened == Some(Opened::Decoder(e));
                if self.toggle(ui, on, &entry.label).clicked() {
                    let d = &entry.definition;
                    let one = Planned {
                        id: d.name.to_string(),
                        decoder: d.name.to_string(),
                        version: d.version,
                        label: None,
                        inputs: vec![],
                        settings: vec![],
                    };
                    open(
                        self,
                        Opened::Decoder(e),
                        Recipe {
                            analyses: vec![one],
                        },
                    );
                }
            }
        });
        let fits: Vec<(usize, usize, String)> = (stack.analyses.iter().enumerate())
            .flat_map(|(k, a)| {
                (registry.decoders.iter().enumerate()).filter_map(move |(e, entry)| {
                    Some((k, e, reads(&entry.definition, a.definition)?))
                })
            })
            .collect();
        if !fits.is_empty() {
            ui.label(self.quiet("reading an analysis here"));
            // One a line: their names hold spaces, and a preset's commas.
            ui.vertical(|ui| {
                for (k, e, port) in fits {
                    let (a, entry) = (&stack.analyses[k], &registry.decoders[e]);
                    let text = format!("{} on {}", entry.label, a.label);
                    if ui.add(Button::new(self.text(text)).frame(false)).clicked() {
                        let d = &entry.definition;
                        let one = Planned {
                            id: d.name.to_string(),
                            decoder: d.name.to_string(),
                            version: d.version,
                            label: None,
                            inputs: vec![(port, Input::Analysis(a.id.to_owned()))],
                            settings: vec![],
                        };
                        *change = Some(Change::Add(Recipe {
                            analyses: vec![one],
                        }));
                        self.state.stack.adding = None;
                    }
                }
            });
        }
        if !registry.presets.is_empty() {
            ui.label(self.quiet("a whole stack"));
            ui.vertical(|ui| {
                for (p, preset) in registry.presets.iter().enumerate() {
                    let on = opened == Some(Opened::Preset(p));
                    if self.toggle(ui, on, &preset.name).clicked() {
                        open(self, Opened::Preset(p), preset.recipe.clone());
                    }
                }
            });
        }
        if let Some((opened, mut recipe)) = self.state.stack.adding.take() {
            ui.add_space(6.0);
            let (add, keep) = self.form(ui, &mut recipe, registry, stack.channels);
            if add {
                *change = Some(Change::Add(recipe));
            } else if keep {
                self.state.stack.adding = Some((opened, recipe));
            }
        }
    }

    /// The form that gives each wire port of what is being added a channel: a chooser for each,
    /// "none" for an optional one. Whether it was added, and whether it stays open.
    fn form(
        &self,
        ui: &mut Ui,
        recipe: &mut Recipe,
        registry: &Registry,
        channels: &[String],
    ) -> (bool, bool) {
        let mut ready = true;
        for a in &mut recipe.analyses {
            let Some(entry) = registry.find(&a.decoder, a.version) else {
                continue;
            };
            let def = &entry.definition;
            let stacked = a
                .inputs
                .iter()
                .any(|(_, i)| matches!(i, Input::Analysis(_)));
            if stacked || def.wire_input().is_none() {
                continue;
            }
            ui.label(self.text(a.label.clone().unwrap_or_else(|| entry.label.clone())));
            Grid::new(("panel form", a.id.clone()))
                .num_columns(2)
                .show(ui, |ui| {
                    for p in &def.ports {
                        let port = ui.label(self.quiet(p.name.to_string()));
                        if let Some(about) = &p.about {
                            port.on_hover_text(about.to_string());
                        }
                        let at = a.inputs.iter().position(|(n, _)| p.name == n.as_str());
                        let chosen = at.and_then(|k| match &a.inputs[k].1 {
                            Input::Channel(c) => Some(c.clone()),
                            Input::Analysis(_) => None,
                        });
                        let mut pick = chosen.clone();
                        ComboBox::from_id_salt(("panel port", a.id.clone(), p.name.to_string()))
                            .selected_text(pick.clone().unwrap_or_else(|| "none".to_owned()))
                            .show_ui(ui, |ui| {
                                if p.optional {
                                    ui.selectable_value(&mut pick, None, "none");
                                }
                                for c in channels {
                                    ui.selectable_value(&mut pick, Some(c.clone()), c);
                                }
                            });
                        if pick != chosen {
                            if let Some(k) = at {
                                a.inputs.remove(k);
                            }
                            if let Some(c) = pick.clone() {
                                a.inputs.push((p.name.to_string(), Input::Channel(c)));
                            }
                        }
                        ready &= pick.is_some() || p.optional;
                        ui.end_row();
                    }
                });
        }
        let mut result = (false, true);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ready, Button::new(self.text("add")))
                .clicked()
            {
                result = (true, false);
            }
            if ui
                .add(Button::new(self.quiet("cancel")).frame(false))
                .clicked()
            {
                result = (false, false);
            }
        });
        result
    }
}

/// The port `reader`'s input takes an analysis of definition `read` by, where it reads one: an
/// input that names the definition, or any, and every kind it reads in its schema.
fn reads(reader: &Definition, read: &Definition) -> Option<String> {
    let (_, input) = reader.upstream_input(&read.name)?;
    let held = |k: &escapement_tickwave_decode::Name| read.schema.kind_id(k).is_some();
    input.kinds.iter().all(held).then(|| input.port.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_number_reads_in_its_notation_and_is_typed_back() {
        let word = 0xFFFF_FFFF;
        assert_eq!(number(0x2477_0011, Some(Notation::Hex), word), "0x24770011");
        assert_eq!(number(1, Some(Notation::Hex), word), "0x00000001");
        assert_eq!(number(-1, Some(Notation::Hex), word), "-1");
        assert_eq!(number(5, Some(Notation::Binary), 7), "0b101");
        assert_eq!(number(4, None, 32), "4");
        for typed in ["0x24770011", "0X2477_0011", " 611778577 "] {
            assert_eq!(parse(typed), Some(0x2477_0011), "{typed}");
        }
        assert_eq!(parse("-1"), Some(-1));
        assert_eq!(parse("0b101"), Some(5));
        for typed in ["", "x", "0x", "1.5", "--1", "-+1", "0x-1"] {
            assert_eq!(parse(typed), None, "{typed}");
        }
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }
}
