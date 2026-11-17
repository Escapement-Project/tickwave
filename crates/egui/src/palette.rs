//! The colours, type and line weight the graph draws with, all from the host.

use egui::{Color32, FontId, Visuals};

/// Every colour, font, opacity and line weight the graph uses; it has none of its own. A host
/// starts from `from_visuals` and sets what its own tokens give.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Palette {
    /// The trace, and the DIO lanes' levels.
    pub trace: Color32,
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
    /// The trace's line and the DIO lanes' levels and edges, in points.
    pub line_weight: f32,
}

impl Palette {
    /// A palette from the host's egui theme, as the web's elements fall back on the system's
    /// colours on a page without tokens: the trace and marker lines in the text colour, axis
    /// labels and lane names in the weak text colour, rules mixed from the text colour into the
    /// panel's, the selection and lost time washed with the text colour, the attention colour from
    /// the theme's warning colour; figures and labels in the host's monospace font at 11 points;
    /// the web's opacities and its line weight, 1.5 points.
    pub fn from_visuals(v: &Visuals) -> Self {
        let ink = v.text_color();
        Self {
            trace: ink,
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
            line_weight: 1.5,
        }
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
