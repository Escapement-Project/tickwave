//! The colours, type and line weight the graph draws with, all from the host.

use std::collections::BTreeMap;

use egui::{Color32, FontId, Visuals};
use escapement_tickwave_view::format::annotations::Colour;
use escapement_tickwave_view::roles::{Host, rgb};

/// Every colour, font, opacity and line weight the graph uses; it has none of its own. A host
/// starts from `from_visuals` and sets what its own tokens give.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Palette {
    /// The trace, and the DIO lanes' levels: None for the colour the recording stores
    /// (ANNOTATIONS.md, "The stored view") on the face `dark` names, else `ink`.
    pub trace: Option<Color32>,
    /// Whether the host's face is its dark one: a stored colour, the trace's or a role's, takes
    /// its night colour.
    pub dark: bool,
    /// The host's own look for each paint role, by the role's name: each field it gives over the
    /// recording's declaration (ANNOTATIONS.md, "Roles"), as a page's `--tickwave-role-<name>`
    /// tokens are. A weight or a dash past a declaration's bounds is left to the declaration.
    pub roles: BTreeMap<String, Host<Color32>>,
    /// What the graph stands on, and the halo of a marker line and the ground under its label.
    pub surface: Color32,
    /// Rules: the value marks and the time axis's ticks across the plot.
    pub hair: Color32,
    /// Marker lines and labels.
    pub ink: Color32,
    /// Axis labels and lane names.
    pub ink_2: Color32,
    /// Where the instrument lost data, at `shade`.
    pub attention_fill: Color32,
    pub attention_ink: Color32,
    /// The selection.
    pub wash: Color32,
    /// The axes' figures.
    pub figures: FontId,
    /// Lane names, marker labels and the graph's one note.
    pub labels: FontId,
    /// The opacity of a column that touches a loss, and of a flagged sample.
    pub pale: f32,
    /// The opacity of a column's band between its least and greatest value.
    pub fill: f32,
    /// The opacity of `attention_fill` over lost time.
    pub shade: f32,
    /// The opacity of a decoded part's fill in the colour of the role driving it, light enough
    /// that its value's text and its bits stay legible over it.
    pub part: f32,
    /// The trace's line and the DIO lanes' levels and edges, in points.
    pub line_weight: f32,
}

impl Palette {
    /// A palette from the host's egui theme, as the web's elements fall back on the system's
    /// colours on a page without tokens: the trace and marker lines in the text colour, axis
    /// labels and lane names in the weak text colour, rules mixed from the text colour into the
    /// panel's, the selection and lost time washed with the text colour, the attention colour from
    /// the theme's warning colour; figures and labels in the host's monospace font at 11 points;
    /// the web's opacities and its line weight, 1.5 points, and a decoded part's fill at 0.18,
    /// which the web does not draw. The trace takes the recording's stored colour where it has
    /// one, on the theme's face, and the host sets no role's look.
    pub fn from_visuals(v: &Visuals) -> Self {
        let ink = v.text_color();
        Self {
            trace: None,
            dark: v.dark_mode,
            roles: BTreeMap::new(),
            surface: v.panel_fill,
            hair: mix(ink, v.panel_fill, 0.22),
            ink,
            ink_2: v.weak_text_color(),
            attention_fill: ink,
            attention_ink: v.warn_fg_color,
            wash: ink.gamma_multiply(0.12),
            figures: FontId::monospace(11.0),
            labels: FontId::monospace(11.0),
            pale: 0.35,
            fill: 1.0,
            shade: 0.12,
            part: 0.18,
            line_weight: 1.5,
        }
    }

    /// The trace's colour: the host's, else `stored` on the face `dark` names, else ink.
    pub(crate) fn trace_colour(&self, stored: Option<&Colour>) -> Color32 {
        let stored = stored.and_then(|c| rgb(if self.dark { &c.night } else { &c.day }));
        self.trace
            .or(stored.map(|[r, g, b]| Color32::from_rgb(r, g, b)))
            .unwrap_or(self.ink)
    }
}

/// `a` mixed into `b`, `a` making up `share` of it, as CSS's `color-mix(in srgb, ...)` does.
fn mix(a: Color32, b: Color32, share: f32) -> Color32 {
    let channel =
        |x: u8, y: u8| (f32::from(x) * share + f32::from(y) * (1.0 - share)).round() as u8;
    Color32::from_rgba_premultiplied(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
        channel(a.a(), b.a()),
    )
}
