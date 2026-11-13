//! The shapes a graph draws each frame, from the latest answer mapped to the window on screen.

use std::ops::Range;

use egui::epaint::{CircleShape, PathShape, Tessellator};
use egui::{Align2, Color32, Mesh, Pos2, Rect, Shape, Stroke, Ui, pos2};
use escapement_tickwave_view::axis::time_axis;
use escapement_tickwave_view::columns::{DigitalColumns, Grid, State};
use escapement_tickwave_view::readouts::format_at;
use escapement_tickwave_view::scale::{ValueRange, range_marks};
use escapement_tickwave_view::{AnalogColumns, Drawn, Marker, Point};

use crate::graph::{LABEL_TOP, Layout, View};
use crate::palette::Palette;
use crate::viewer::{Facts, Held};

/// A trace's line, in points.
const TRACE_WIDTH: f32 = 1.2;
const LANE_WIDTH: f32 = 1.0;
/// A lane's high level sits this far under its top, its low level `LANE_SWING` under that.
const LANE_PAD: f32 = 3.0;
const LANE_SWING: f32 = 14.0;
const DOT_RADIUS: f32 = 2.5;
/// Single samples get a dot each once they stand this many points apart.
const DOT_SPACING: f32 = 6.0;
const AXIS_GAP: f64 = 16.0;
const NOTE: &str = "More markers: zoom in or use the list";

/// The columns of `g` that any tick of [from, to) falls in, as the view core picks a window's.
pub(crate) fn columns_over(g: &Grid, from: i64, to: i64) -> Range<usize> {
    let c0 = ((from as f64 - g.x0) / g.per).floor().max(0.0);
    let c1 = ((to as f64 - g.x0) / g.per).ceil().min(g.count as f64);
    if c1 <= c0 || c1.is_nan() || c0.is_nan() {
        return 0..0;
    }
    c0 as usize..c1 as usize
}

/// One column of a trace, or the part of one, in ticks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Piece {
    pub from: f64,
    pub to: f64,
    pub min: f64,
    pub max: f64,
    /// The trace comes into it from the piece before.
    pub joined: bool,
    pub pale: bool,
}

struct Columns<'a> {
    grid: &'a Grid,
    columns: &'a AnalogColumns,
    lost: &'a [Range<i64>],
}

/// The trace as the latest answer holds it: columns or single samples over `around`, and past
/// `around` the strip's columns over the extent.
pub(crate) struct Trace<'a> {
    around: Range<i64>,
    held: Columns<'a>,
    samples: Option<&'a [Point]>,
    strip: Option<Columns<'a>>,
}

impl<'a> Trace<'a> {
    /// Channel `channel` in `held`, where it was asked for as an analog trace.
    pub fn of(held: &'a Held, channel: usize) -> Option<Self> {
        let answer = &held.answer;
        let at = held.request.channels.iter().position(|&c| c == channel)?;
        let Drawn::Analog {
            columns,
            samples,
            lost,
            ..
        } = answer.channels.get(at)?
        else {
            return None;
        };
        let strip = match (held.request.strip, &answer.strip) {
            (Some((c, _)), Some(s)) if c == channel => match &s.channel {
                Drawn::Analog { columns, lost, .. } => Some(Columns {
                    grid: &s.grid,
                    columns,
                    lost,
                }),
                Drawn::Digital { .. } => None,
            },
            _ => None,
        };
        Some(Self {
            around: answer.around.clone(),
            held: Columns {
                grid: &answer.grid,
                columns,
                lost,
            },
            samples: samples.as_deref(),
            strip,
        })
    }

    /// The trace's columns over `w`, in order: the strip's before `around`, the answer's in it
    /// (none where it holds single samples), the strip's after it.
    pub fn pieces(&self, w: &Range<i64>) -> Vec<Piece> {
        let mut out = Vec::new();
        let a = &self.around;
        if let Some(s) = &self.strip {
            part(s, w.start, w.end.min(a.start), true, &mut out);
        }
        if self.samples.is_none() {
            part(
                &self.held,
                w.start.max(a.start),
                w.end.min(a.end),
                false,
                &mut out,
            );
        }
        if let Some(s) = &self.strip {
            part(s, w.start.max(a.end), w.end, true, &mut out);
        }
        out
    }

    /// The least and greatest value the trace shows over the window: the samples whose time
    /// reaches into it, or the columns any of its ticks fall in.
    pub fn fit(&self, view: &View) -> Option<(f64, f64)> {
        let w = &view.window;
        let mut fit: Option<(f64, f64)> = None;
        let mut widen = |lo: f64, hi: f64| {
            fit = Some(fit.map_or((lo, hi), |(a, b)| (a.min(lo), b.max(hi))));
        };
        for p in self.samples.unwrap_or_default() {
            if p.end > w.start && p.tick < w.end && !p.value.is_nan() {
                widen(p.value, p.value);
            }
        }
        for p in self.pieces(w) {
            widen(p.min, p.max);
        }
        fit
    }

    /// The lost spans over `w`: the answer's in `around`, the strip's past it.
    fn lost(&self, w: &Range<i64>) -> Vec<Range<i64>> {
        let a = &self.around;
        let clip = |r: &Range<i64>, from: i64, to: i64| r.start.max(from)..r.end.min(to);
        let mut out = Vec::new();
        let mut take = |spans: &[Range<i64>], from: i64, to: i64| {
            out.extend(
                spans
                    .iter()
                    .map(|r| clip(r, from, to))
                    .filter(|r| r.end > r.start),
            );
        };
        if let Some(s) = &self.strip {
            take(s.lost, w.start, w.end.min(a.start));
        }
        take(self.held.lost, w.start.max(a.start), w.end.min(a.end));
        if let Some(s) = &self.strip {
            take(s.lost, w.start.max(a.end), w.end);
        }
        out
    }
}

/// The columns of `c` over [from, to) with something in them, as pieces after those in `out`:
/// clipped to it where `clip`, else whole.
fn part(c: &Columns, from: i64, to: i64, clip: bool, out: &mut Vec<Piece>) {
    if to <= from {
        return;
    }
    let (g, cols) = (c.grid, c.columns);
    let ks = columns_over(g, from, to);
    let first = ks.start;
    for k in ks {
        if cols.min[k] > cols.max[k] {
            continue;
        }
        let (mut s, mut e) = (g.x0 + k as f64 * g.per, g.x0 + (k + 1) as f64 * g.per);
        if clip {
            (s, e) = (s.max(from as f64), e.min(to as f64));
            if e <= s {
                continue;
            }
        }
        // Into the first column from the piece before, where it ends where this one starts.
        let joined = if k == first {
            out.last().is_some_and(|p: &Piece| (p.to - s).abs() <= 0.5)
        } else {
            cols.joined[k]
        };
        out.push(Piece {
            from: s,
            to: e,
            min: cols.min[k],
            max: cols.max[k],
            joined,
            pale: cols.pale[k],
        });
    }
}

/// What a frame draws from.
pub(crate) struct Frame<'a> {
    pub layout: &'a Layout,
    pub view: &'a View,
    pub y: (f64, f64),
    pub palette: &'a Palette,
    pub facts: &'a Facts,
    pub held: Option<&'a Held>,
    /// The trace's channel.
    pub channel: usize,
    pub trace: Option<&'a Trace<'a>>,
    pub lanes: &'a [usize],
    pub selection: Option<&'a Range<i64>>,
}

impl Frame<'_> {
    fn y_at(&self, v: f64) -> f32 {
        let plot = self.layout.plot;
        let (lo, hi) = self.y;
        plot.bottom() - ((v - lo) / (hi - lo)) as f32 * plot.height()
    }

    /// The part of the plot and the lanes the graph draws over.
    fn field(&self) -> Rect {
        let l = self.layout;
        let bottom = l.lanes.last().map_or(l.plot.bottom(), |r| r.bottom());
        Rect::from_min_max(l.plot.left_top(), pos2(l.plot.right(), bottom))
    }

    /// From `from` to `to` across the plot, at least `px` wide; None where it lies outside.
    fn across(&self, from: f64, to: f64, px: f32) -> Option<(f32, f32)> {
        let plot = self.layout.plot;
        let a = self.view.x(from).max(plot.left());
        let b = self.view.x(to).min(plot.right());
        (b >= a && a < plot.right()).then_some((a, b.max(a + px)))
    }
}

/// Polylines a frame builds before they are tessellated: each point list with its stroke.
#[derive(Default)]
struct Lines(Vec<(Vec<Pos2>, Stroke)>);

impl Lines {
    fn add(&mut self, line: &mut Vec<Pos2>, stroke: Stroke) {
        if line.len() >= 2 {
            self.0.push((std::mem::take(line), stroke));
        } else {
            line.clear();
        }
    }
}

/// `p` onto `line`, along a level stretch where the two points before it are already on one.
fn step(line: &mut Vec<Pos2>, p: Pos2) {
    let n = line.len();
    if n >= 2 && line[n - 1].y == p.y && line[n - 2].y == p.y {
        line[n - 1].x = p.x;
    } else {
        line.push(p);
    }
}

/// Every line of `lines` and every circle of `circles` as one mesh.
fn tessellate(ui: &Ui, clip: Rect, lines: Lines, circles: Vec<CircleShape>) -> Mesh {
    let options = ui.ctx().tessellation_options(|o| *o);
    let size = ui.ctx().fonts(|f| f.font_image_size());
    let mut t = Tessellator::new(ui.ctx().pixels_per_point(), options, size, Vec::new());
    t.set_clip_rect(clip);
    let mut mesh = Mesh::default();
    for (points, stroke) in lines.0 {
        t.tessellate_path(&PathShape::line(points, stroke), &mut mesh);
    }
    for c in circles {
        t.tessellate_shape(Shape::Circle(c), &mut mesh);
    }
    mesh
}

/// The text of a marker's label: its number, then its label.
fn marker_text(m: &Marker) -> String {
    let a = &m.annotation;
    let number = a.values.as_ref().and_then(|v| v.get("number"));
    [number.map(String::as_str), a.label.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn graph(ui: &Ui, f: &Frame) {
    let l = f.layout;
    let p = f.palette;
    let px = 1.0 / ui.ctx().pixels_per_point();
    let field = f.field();
    let painter = ui.painter().with_clip_rect(l.rect);
    painter.rect_filled(field, 0.0, p.surface);
    let axis = {
        let _span = tracing::debug_span!("axes").entered();
        axes(ui, f, field)
    };
    let plot = ui.painter().with_clip_rect(l.plot);
    {
        let _span = tracing::debug_span!("trace").entered();
        let (fills, lines, circles) = trace(f, px, &plot);
        let mesh = {
            let _span = tracing::debug_span!("tessellate").entered();
            tessellate(ui, l.plot, lines, circles)
        };
        plot.add(Shape::mesh(fills));
        plot.add(Shape::mesh(mesh));
    }
    for (&c, rect) in f.lanes.iter().zip(&l.lanes) {
        let _span = tracing::debug_span!("lane", channel = c).entered();
        lane(ui, f, c, *rect, px);
    }
    if let Some(s) = f.selection
        && let Some((a, b)) = f.across(s.start as f64, s.end as f64, px)
    {
        painter.rect_filled(
            Rect::from_min_max(pos2(a, field.top()), pos2(b, field.bottom())),
            0.0,
            p.wash,
        );
    }
    {
        let _span = tracing::debug_span!("markers").entered();
        markers(f, &plot);
    }
    for (text, pos, align, font) in axis {
        painter.text(pos, align, text, font, p.ink_2);
    }
}

/// The rules across the field, and the labels of both axes and the lanes for drawing last.
fn axes(ui: &Ui, f: &Frame, field: Rect) -> Vec<(String, Pos2, Align2, egui::FontId)> {
    let l = f.layout;
    let p = f.palette;
    let painter = ui.painter().with_clip_rect(l.rect);
    let hair = Stroke::new(1.0, p.hair);
    let mut labels = Vec::new();
    let w = &f.view.window;
    let axis = time_axis(
        w.start as f64,
        w.end as f64,
        f64::from(l.plot.width()),
        f.facts.timebase,
        |s| {
            painter
                .layout_no_wrap(s.to_owned(), p.figures.clone(), p.ink_2)
                .size()
                .x
        },
        AXIS_GAP,
    );
    painter.line_segment([l.axis.left_top(), l.axis.right_top()], hair);
    for t in &axis.ticks {
        let x = l.plot.left() + t.x as f32;
        painter.line_segment([pos2(x, field.top()), pos2(x, field.bottom())], hair);
        painter.line_segment(
            [pos2(x, l.axis.top()), pos2(x, l.axis.top() + 4.0)],
            Stroke::new(1.0, p.ink_2),
        );
        labels.push((
            t.label.clone(),
            pos2(x, l.axis.top() + 5.0),
            Align2::CENTER_TOP,
            p.figures.clone(),
        ));
    }
    let unit = f
        .facts
        .channels
        .get(f.channel)
        .map_or("", |c| c.unit.as_str());
    if let Some(m) = range_marks(ValueRange {
        lo: f.y.0,
        hi: f.y.1,
    }) {
        let right = l.gutter.right() - 6.0;
        for (value, align, nudge) in [
            (m.top, Align2::RIGHT_TOP, 1.0),
            (m.bottom, Align2::RIGHT_BOTTOM, -1.0),
        ] {
            let y = f.y_at(value);
            painter.line_segment([pos2(l.plot.left(), y), pos2(l.plot.right(), y)], hair);
            if let Some(text) = format_at(value, m.place, unit) {
                labels.push((text, pos2(right, y + nudge), align, p.figures.clone()));
            }
        }
    }
    for (&c, rect) in f.lanes.iter().zip(&l.lanes) {
        if let Some(info) = f.facts.channels.get(c) {
            labels.push((
                info.name.clone(),
                pos2(
                    l.gutter.right() - 6.0,
                    rect.top() + LANE_PAD + LANE_SWING / 2.0,
                ),
                Align2::RIGHT_CENTER,
                p.labels.clone(),
            ));
        }
    }
    labels
}

/// The trace's band fills, its lines and its dots, with the lost time shaded under them.
fn trace(f: &Frame, px: f32, plot: &egui::Painter) -> (Mesh, Lines, Vec<CircleShape>) {
    let p = f.palette;
    let mut fills = Mesh::default();
    let mut lines = Lines::default();
    let mut circles = Vec::new();
    let Some(t) = f.trace else {
        return (fills, lines, circles);
    };
    let w = &f.view.window;
    let mut bands: Vec<(f32, f32)> = Vec::new();
    for r in t.lost(w) {
        if let Some((a, b)) = f.across(r.start as f64, r.end as f64, px) {
            match bands.last_mut() {
                Some(last) if a - last.1 < px => last.1 = last.1.max(b),
                _ => bands.push((a, b)),
            }
        }
    }
    let plot_rect = f.layout.plot;
    for (a, b) in bands {
        plot.rect_filled(
            Rect::from_min_max(pos2(a, plot_rect.top()), pos2(b, plot_rect.bottom())),
            0.0,
            p.attention_fill.gamma_multiply(p.shade),
        );
    }
    let colour = |pale: bool| {
        if pale {
            p.trace.gamma_multiply(p.pale)
        } else {
            p.trace
        }
    };
    let stroke = |c: Color32| Stroke::new(TRACE_WIDTH, c);
    let (mut top, mut bottom) = (Vec::new(), Vec::new());
    let mut drawing: Option<(Color32, f32, f32)> = None;
    for piece in t.pieces(w) {
        let (x0, x1) = (f.view.x(piece.from), f.view.x(piece.to));
        let (yt, yb) = (f.y_at(piece.max), f.y_at(piece.min));
        let c = colour(piece.pale);
        fills.add_colored_rect(
            Rect::from_min_max(pos2(x0, yt), pos2(x1, yb)),
            c.gamma_multiply(p.fill),
        );
        match drawing {
            Some((same, _, _)) if piece.joined && same == c => {
                step(&mut top, pos2(x0, yt));
                step(&mut top, pos2(x1, yt));
                step(&mut bottom, pos2(x0, yb));
                step(&mut bottom, pos2(x1, yb));
            }
            _ => {
                if let Some((was, pt, pb)) = drawing {
                    lines.add(&mut top, stroke(was));
                    lines.add(&mut bottom, stroke(was));
                    if piece.joined {
                        top.push(pos2(x0, pt));
                        bottom.push(pos2(x0, pb));
                    }
                }
                top.extend([pos2(x0, yt), pos2(x1, yt)]);
                bottom.extend([pos2(x0, yb), pos2(x1, yb)]);
            }
        }
        drawing = Some((c, yt, yb));
    }
    if let Some((c, _, _)) = drawing {
        lines.add(&mut top, stroke(c));
        lines.add(&mut bottom, stroke(c));
    }
    if let Some(points) = t.samples {
        let mut line = Vec::new();
        let mut drawing: Option<(Color32, i64, f32)> = None;
        for s in points {
            if s.value.is_nan() {
                if let Some((c, _, _)) = drawing.take() {
                    lines.add(&mut line, stroke(c));
                }
                continue;
            }
            let c = colour(s.flagged);
            let (x0, x1, y) = (
                f.view.x(s.tick as f64),
                f.view.x(s.end as f64),
                f.y_at(s.value),
            );
            let joined = s.joined && drawing.is_some_and(|(_, end, _)| end == s.tick);
            match drawing {
                Some((same, _, _)) if joined && same == c => {
                    step(&mut line, pos2(x0, y));
                    step(&mut line, pos2(x1, y));
                }
                _ => {
                    if let Some((was, _, before)) = drawing {
                        lines.add(&mut line, stroke(was));
                        if joined {
                            line.push(pos2(x0, before));
                        }
                    }
                    line.extend([pos2(x0, y), pos2(x1, y)]);
                }
            }
            drawing = Some((c, s.end, y));
        }
        if let Some((c, _, _)) = drawing {
            lines.add(&mut line, stroke(c));
        }
        let shown = points
            .iter()
            .filter(|s| s.tick >= w.start && s.tick < w.end)
            .count();
        if shown > 0 && f.view.width / shown as f32 >= DOT_SPACING {
            for s in points.iter().filter(|s| !s.value.is_nan()) {
                circles.push(CircleShape::filled(
                    pos2(f.view.x(s.tick as f64), f.y_at(s.value)),
                    DOT_RADIUS,
                    colour(s.flagged),
                ));
            }
        }
    }
    (fills, lines, circles)
}

/// Digital channel `c`'s lane: its gaps shaded, its busy columns filled, its levels as lines.
fn lane(ui: &Ui, f: &Frame, c: usize, rect: Rect, px: f32) {
    let p = f.palette;
    let Some(held) = f.held else { return };
    let Some(at) = held.request.channels.iter().position(|&x| x == c) else {
        return;
    };
    let Some(Drawn::Digital { columns, gaps }) = held.answer.channels.get(at) else {
        return;
    };
    let painter = ui.painter().with_clip_rect(rect);
    let high = rect.top() + LANE_PAD;
    let low = high + LANE_SWING;
    let w = &f.view.window;
    let a = &held.answer.around;
    for g in gaps {
        if let Some((x0, x1)) = f.across(g.start.max(w.start) as f64, g.end.min(w.end) as f64, px) {
            painter.rect_filled(
                Rect::from_min_max(pos2(x0, high), pos2(x1, low)),
                0.0,
                p.attention_fill.gamma_multiply(p.shade),
            );
        }
    }
    let mut fills = Mesh::default();
    let mut lines = Lines::default();
    let stroke = Stroke::new(LANE_WIDTH, p.trace);
    let busy = p.trace.gamma_multiply(p.busy);
    let g = &held.answer.grid;
    let DigitalColumns { state, at } = columns;
    let mut line = Vec::new();
    let mut pen: Option<f32> = None;
    for k in columns_over(g, w.start.max(a.start), w.end.min(a.end)) {
        let x0 = f.view.x(g.x0 + k as f64 * g.per);
        let x1 = f.view.x(g.x0 + (k + 1) as f64 * g.per);
        match state[k] {
            State::None => {
                lines.add(&mut line, stroke);
                pen = None;
            }
            State::Low | State::High => {
                let y = if state[k] == State::High { high } else { low };
                if pen != Some(y) {
                    lines.add(&mut line, stroke);
                    line.push(pos2(x0, y));
                }
                step(&mut line, pos2(x1, y));
                pen = Some(y);
            }
            State::Rise | State::Fall => {
                let (ya, yb) = if state[k] == State::Rise {
                    (low, high)
                } else {
                    (high, low)
                };
                let x = x0 + at[k] as f32 * (x1 - x0);
                if pen != Some(ya) {
                    lines.add(&mut line, stroke);
                    line.push(pos2(x0, ya));
                }
                step(&mut line, pos2(x, ya));
                line.extend([pos2(x, yb), pos2(x1, yb)]);
                pen = Some(yb);
            }
            State::Busy => {
                lines.add(&mut line, stroke);
                pen = None;
                fills.add_colored_rect(
                    Rect::from_min_max(pos2(x0, high), pos2(x1.max(x0 + px), low)),
                    busy,
                );
            }
        }
    }
    lines.add(&mut line, stroke);
    let mesh = {
        let _span = tracing::debug_span!("tessellate").entered();
        tessellate(ui, rect, lines, Vec::new())
    };
    painter.add(Shape::mesh(fills));
    painter.add(Shape::mesh(mesh));
}

/// The window's markers: a line each with a halo, and labels in up to three rows across the
/// plot's top, each in the first row with room for it, drawn once every line is.
fn markers(f: &Frame, plot: &egui::Painter) {
    let Some(held) = f.held else { return };
    let p = f.palette;
    let rect = f.layout.plot;
    let page = &held.answer.markers.window;
    let mut occupied = [rect.left() - 1.0; 3];
    let mut labels = Vec::new();
    for m in &page.entries {
        let x = f.view.x(m.cursor.tick as f64);
        if x < rect.left() || x > rect.right() {
            continue;
        }
        let line = [pos2(x, rect.top()), pos2(x, rect.bottom())];
        plot.line_segment(line, Stroke::new(3.0, p.surface));
        plot.line_segment(line, Stroke::new(1.0, p.ink));
        let text = marker_text(m);
        if text.is_empty() {
            continue;
        }
        let galley = plot.layout_no_wrap(text, p.labels.clone(), p.ink);
        let left = (x + 3.0)
            .min(rect.right() - galley.size().x)
            .max(rect.left());
        if let Some((row, right)) = occupied
            .iter_mut()
            .enumerate()
            .find(|(_, right)| **right < left)
        {
            *right = left + galley.size().x + 8.0;
            let top = rect.top() + LABEL_TOP + row as f32 * f.layout.label_row;
            labels.push((pos2(left, top), galley));
        }
    }
    for (pos, galley) in labels {
        plot.rect_filled(Rect::from_min_size(pos, galley.size()), 0.0, p.surface);
        plot.galley(pos, galley, p.ink);
    }
    if page.next.is_some() {
        let galley = plot.layout_no_wrap(NOTE.into(), p.labels.clone(), p.ink_2);
        let pos = pos2(rect.left() + 6.0, rect.bottom() - 6.0 - galley.size().y);
        plot.rect_filled(Rect::from_min_size(pos, galley.size()), 0.0, p.surface);
        plot.galley(pos, galley, p.ink_2);
    }
}
