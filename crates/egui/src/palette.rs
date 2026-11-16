//! The colours and type the graph draws with, all from the host.

use egui::{Color32, FontId};

/// Every colour, font and opacity the graph uses; it has none of its own.
#[derive(Clone, Debug, PartialEq)]
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
}
