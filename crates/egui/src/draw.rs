//! The shapes a graph draws each frame, from the latest answer mapped to the window on screen.

use std::ops::Range;

use egui::epaint::{CircleShape, PathShape, Tessellator};
use egui::{Align2, Color32, Mesh, Pos2, Rect, Shape, Stroke, Ui, pos2};
use escapement_tickwave_view::axis::time_axis;
use escapement_tickwave_view::columns::{DigitalColumns, Grid, State};
use escapement_tickwave_view::format::annotations::Role;
use escapement_tickwave_view::format::model::Ratio;
use escapement_tickwave_view::readouts::format_marks;
use escapement_tickwave_view::roles::{self, Dash, LookColour};
use escapement_tickwave_view::scale::{ValueRange, range_marks};
use escapement_tickwave_view::shade::{self, Look};
use escapement_tickwave_view::{AnalogColumns, Drawn, Marker, Paint, Point, StripAnswer};

use crate::graph::{LABEL_TOP, Layout, View, fitted};
use crate::palette::Palette;
use crate::viewer::{Facts, Held};

/// A lane's high level sits this far under its top, its low level `LANE_SWING` under that.
const LANE_PAD: f32 = 3.0;
const LANE_SWING: f32 = 14.0;
const DOT_RADIUS: f32 = 2.5;
/// Single samples get a dot each once they stand this many points apart.
const DOT_SPACING: f32 = 6.0;
const AXIS_GAP: f64 = 16.0;
const NOTE: &str = "More markers: zoom in or use the list";
/// A key's entry: its line this long, then this far to its label, and entries this far apart.
const SWATCH: f32 = 18.0;
const SWATCH_GAP: f32 = 5.0;
const KEY_GAP: f32 = 12.0;

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
                Drawn::Digital { .. } | Drawn::Unavailable => None,
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
    /// The trace's channel; None for a graph of lanes alone.
    pub channel: Option<usize>,
    pub trace: Option<&'a Trace<'a>>,
    pub lanes: &'a [usize],
    pub selection: Option<&'a Range<i64>>,
    /// The trace's colour: the palette's, else the recording's stored one, else ink.
    pub ink: Color32,
    /// The paint roles the sidecar declares, in order, and the paints over the answer's span.
    pub roles: &'a [Role],
    pub paints: &'a [Paint],
    pub pixels_per_point: f32,
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

    fn across(&self, from: f64, to: f64, px: f32) -> Option<(f32, f32)> {
        across(self.view, self.layout.plot, from, to, px)
    }
}

/// From tick `from` to tick `to` across `rect` as `view` maps them, at least `px` wide; None
/// where it lies outside.
fn across(view: &View, rect: Rect, from: f64, to: f64, px: f32) -> Option<(f32, f32)> {
    let a = view.x(from).max(rect.left());
    let b = view.x(to).min(rect.right());
    (b >= a && a < rect.right()).then_some((a, b.max(a + px)))
}

/// Shades each of `lost` over `rect`, at least `px` wide so none vanishes at any zoom; spans
/// less than `px` apart share one band.
fn shade_lost(
    painter: &egui::Painter,
    view: &View,
    rect: Rect,
    lost: impl IntoIterator<Item = Range<i64>>,
    px: f32,
    p: &Palette,
) {
    let mut bands: Vec<(f32, f32)> = Vec::new();
    for r in lost {
        if let Some((a, b)) = across(view, rect, r.start as f64, r.end as f64, px) {
            match bands.last_mut() {
                Some(last) if a - last.1 < px => last.1 = last.1.max(b),
                _ => bands.push((a, b)),
            }
        }
    }
    for (a, b) in bands {
        painter.rect_filled(
            Rect::from_min_max(pos2(a, rect.top()), pos2(b, rect.bottom())),
            0.0,
            p.attention_fill.gamma_multiply(p.shade),
        );
    }
}

/// A colour as a share of the one a row's stretch draws in: that colour `gamma_multiply`'d by
/// `.0`, then by `.1`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Tint(f32, f32);

impl Tint {
    const FULL: Self = Self(1.0, 1.0);

    /// A trace's: pale where it touches a loss or is flagged.
    fn trace(p: &Palette, pale: bool) -> Self {
        Self(if pale { p.pale } else { 1.0 }, 1.0)
    }

    fn of(self, c: Color32) -> Color32 {
        c.gamma_multiply(self.0).gamma_multiply(self.1)
    }
}

/// What a row draws, each part a tint of the colour its stretch draws in: rectangles filled,
/// polylines, and dots. Each polyline is on a chain: a band's top edge, its bottom edge, a line
/// through samples, a lane's levels, each from one run of data to the next gap. A dash runs on
/// along a chain.
#[derive(Default)]
struct Ink {
    fills: Vec<(Rect, Tint)>,
    lines: Vec<(Vec<Pos2>, Tint, u32)>,
    dots: Vec<(Pos2, Tint)>,
}

impl Ink {
    fn line(&mut self, line: &mut Vec<Pos2>, tint: Tint, chain: u32) {
        if line.len() >= 2 {
            self.lines.push((std::mem::take(line), tint, chain));
        } else {
            line.clear();
        }
    }

    /// Each line's length before it along its chain, and its points' x and the chain's length
    /// there, flat: where a dash's phase is (`roles::length_at`).
    fn lengths(&self) -> Vec<(f64, Vec<f64>)> {
        let mut along: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
        self.lines
            .iter()
            .map(|(points, _, chain)| {
                let at = *along.get(chain).unwrap_or(&0.0);
                let mut length = at;
                let mut flat = Vec::with_capacity(points.len() * 2);
                for (i, q) in points.iter().enumerate() {
                    if i > 0 {
                        length += f64::from(points[i - 1].distance(*q));
                    }
                    flat.extend([f64::from(q.x), length]);
                }
                along.insert(*chain, length);
                (at, flat)
            })
            .collect()
    }
}

/// How a stretch of a row draws: its colour, its line's width in points, and its dash's (dash,
/// gap) lengths in points, or None for a solid line.
#[derive(Clone, Debug, PartialEq)]
struct Pen {
    colour: Color32,
    width: f32,
    dash: Option<Vec<f32>>,
}

/// The dashes of `line` whose pattern, (dash, gap) lengths, is `phase` along at its first point:
/// each a polyline.
fn dashes(line: &[Pos2], pattern: &[f32], phase: f32) -> Vec<Vec<Pos2>> {
    let period: f32 = pattern.iter().sum();
    let lengths = pattern.iter().all(|x| x.is_finite() && *x >= 0.0);
    if line.len() < 2 || !lengths || period <= 0.0 {
        return vec![line.to_vec()];
    }
    let mut k = 0;
    let mut into = phase.rem_euclid(period);
    while into >= pattern[k] {
        into -= pattern[k];
        k = (k + 1) % pattern.len();
    }
    // What is left of element `k`; an even one is a dash.
    let mut left = pattern[k] - into;
    let mut out = Vec::new();
    let mut dash = if k % 2 == 0 {
        vec![line[0]]
    } else {
        Vec::new()
    };
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        let length = a.distance(b);
        let mut done = 0.0;
        while length - done > left {
            done += left;
            let q = a + (b - a) * (done / length);
            if k % 2 == 0 {
                dash.push(q);
                out.push(std::mem::take(&mut dash));
            } else {
                dash = vec![q];
            }
            k = (k + 1) % pattern.len();
            left = pattern[k];
        }
        left -= length - done;
        if k % 2 == 0 {
            dash.push(b);
        }
    }
    if dash.len() >= 2 {
        out.push(dash);
    }
    out
}

/// Draws `ink` in `pen`, clipped to `clip`: its fills, then its lines and dots. Where `whole`,
/// `clip` is the row's own and every part is drawn; else only the parts that reach into it.
fn paint(ui: &Ui, ink: &Ink, pen: &Pen, clip: Rect, whole: bool) {
    let reaches = |a: f32, b: f32| whole || (b >= clip.left() && a <= clip.right());
    let mut fills = Mesh::default();
    for (r, t) in &ink.fills {
        if reaches(r.left(), r.right()) {
            fills.add_colored_rect(*r, t.of(pen.colour));
        }
    }
    let options = ui.ctx().tessellation_options(|o| *o);
    let size = ui.ctx().fonts(|f| f.font_image_size());
    let mut t = Tessellator::new(ui.ctx().pixels_per_point(), options, size, Vec::new());
    t.set_clip_rect(clip);
    let mut mesh = Mesh::default();
    let mut stroke = |points: Vec<Pos2>, tint: Tint, mesh: &mut Mesh| {
        t.tessellate_path(
            &PathShape::line(points, Stroke::new(pen.width, tint.of(pen.colour))),
            mesh,
        );
    };
    let span = |points: &[Pos2]| {
        let xs = points.iter().map(|q| q.x);
        let lo = xs.clone().fold(f32::INFINITY, f32::min);
        (lo, xs.fold(f32::NEG_INFINITY, f32::max))
    };
    match &pen.dash {
        None => {
            for (points, tint, _) in &ink.lines {
                let (a, b) = span(points);
                if reaches(a, b) {
                    stroke(points.clone(), *tint, &mut mesh);
                }
            }
        }
        Some(pattern) => {
            // One phase along each chain from where the stretch starts, or the chain does.
            let lengths = ink.lengths();
            let mut origins: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
            for (i, (points, tint, chain)) in ink.lines.iter().enumerate() {
                let (a, b) = span(points);
                if !reaches(a, b) {
                    continue;
                }
                let origin = *origins.entry(*chain).or_insert_with(|| {
                    let paths: Vec<&[f64]> = (ink.lines.iter().zip(&lengths))
                        .filter(|((_, _, c), _)| c == chain)
                        .map(|(_, (_, flat))| flat.as_slice())
                        .collect();
                    roles::length_at(&paths, f64::from(clip.left()))
                });
                let phase = (lengths[i].0 - origin) as f32;
                for d in dashes(points, pattern, phase) {
                    stroke(d, *tint, &mut mesh);
                }
            }
        }
    }
    for (q, tint) in &ink.dots {
        t.tessellate_shape(
            Shape::Circle(CircleShape::filled(*q, DOT_RADIUS, tint.of(pen.colour))),
            &mut mesh,
        );
    }
    let painter = ui.painter().with_clip_rect(clip);
    painter.add(Shape::mesh(fills));
    painter.add(Shape::mesh(mesh));
}

/// Role `name`'s pen: its look (`roles::look`), the host's over the declaration's, in the trace's
/// line weight with the role's added.
fn role_pen(f: &Frame, name: &str) -> Option<Pen> {
    let p = f.palette;
    let host = p.roles.get(name).cloned().unwrap_or_default();
    let look = roles::look(f.roles, name, host, p.dark)?;
    let colour = match look.colour {
        LookColour::Host(c) => c,
        LookColour::Stored([r, g, b]) => Color32::from_rgb(r, g, b),
        LookColour::Trace => f.ink,
        LookColour::Ink => p.ink,
    };
    let width = p.line_weight + look.weight as f32;
    let dash = match look.dash {
        Dash::Solid => None,
        Dash::Pattern(d) => Some(d.iter().map(|&x| x as f32 * width).collect::<Vec<f32>>()),
    };
    // A dash is at least a line width a period, but a host's line weight may be tiny: a period
    // under a device pixel would cut a row into more pieces than it has pixels, and shows as
    // solid anyway.
    let dash = dash.filter(|d| d.iter().sum::<f32>() * f.pixels_per_point >= 1.0);
    Some(Pen {
        colour,
        width,
        dash,
    })
}

/// Row `rect` of channel `c` cut where the role painting it changes (`roles::stretches`), each
/// stretch the part of `rect` it covers and its pen: the latest paint in sidecar order with a
/// declared role draws a column, and the trace's own pen the rest. `rect` alone, in the trace's
/// pen, where nothing paints it.
fn stretches(f: &Frame, c: usize, rect: Rect) -> Vec<(Rect, Pen)> {
    let p = f.palette;
    let plain = Pen {
        colour: f.ink,
        width: p.line_weight,
        dash: None,
    };
    let name = f.facts.channels.get(c).map(|i| i.name.as_str());
    let (Some(name), false) = (name, f.roles.is_empty()) else {
        return vec![(rect, plain)];
    };
    let ppp = f.pixels_per_point;
    let column = |t: i64| ((f.view.x(t as f64) - rect.left()) * ppp).round() as i64;
    let spans: Vec<(i64, i64, &str)> = (f.paints.iter())
        .filter(|q| q.channels.is_empty() || q.channels.iter().any(|n| n == name))
        .map(|q| (column(q.span.start), column(q.span.end), q.role.as_str()))
        .collect();
    let cut = roles::stretches(&spans, f.roles, 0, (rect.width() * ppp).round() as i64);
    if cut.iter().all(|s| s.role.is_none()) {
        return vec![(rect, plain)];
    }
    cut.into_iter()
        .map(|s| {
            let x = |px: i64| rect.left() + px as f32 / ppp;
            let clip = Rect::from_x_y_ranges(x(s.px0)..=x(s.px1), rect.y_range());
            let pen = (s.role)
                .and_then(|r| role_pen(f, &f.roles[r].name))
                .unwrap_or_else(|| plain.clone());
            (clip, pen)
        })
        .collect()
}

/// Each piece's band from its least to its greatest value, filled, with lines along both
/// edges that step from one piece to the next where they join. A piece that doesn't join the
/// one before starts the edges' chains again, from `chain` on.
fn bands(
    pieces: &[Piece],
    view: &View,
    y_at: impl Fn(f64) -> f32,
    p: &Palette,
    ink: &mut Ink,
    chain: &mut u32,
) {
    let (mut top, mut bottom) = (Vec::new(), Vec::new());
    let mut drawing: Option<(Tint, f32, f32)> = None;
    for piece in pieces {
        let (x0, x1) = (view.x(piece.from), view.x(piece.to));
        let (yt, yb) = (y_at(piece.max), y_at(piece.min));
        let t = Tint::trace(p, piece.pale);
        ink.fills.push((
            Rect::from_min_max(pos2(x0, yt), pos2(x1, yb)),
            Tint(t.0, p.fill),
        ));
        match drawing {
            Some((same, _, _)) if piece.joined && same == t => {
                step(&mut top, pos2(x0, yt));
                step(&mut top, pos2(x1, yt));
                step(&mut bottom, pos2(x0, yb));
                step(&mut bottom, pos2(x1, yb));
            }
            _ => {
                if let Some((was, pt, pb)) = drawing {
                    ink.line(&mut top, was, *chain);
                    ink.line(&mut bottom, was, *chain + 1);
                    if piece.joined {
                        top.push(pos2(x0, pt));
                        bottom.push(pos2(x0, pb));
                    }
                }
                if !piece.joined {
                    *chain += 2;
                }
                top.extend([pos2(x0, yt), pos2(x1, yt)]);
                bottom.extend([pos2(x0, yb), pos2(x1, yb)]);
            }
        }
        drawing = Some((t, yt, yb));
    }
    if let Some((t, _, _)) = drawing {
        ink.line(&mut top, t, *chain);
        ink.line(&mut bottom, t, *chain + 1);
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
    if let Some(c) = f.channel {
        let _span = tracing::debug_span!("trace").entered();
        let ink = trace(f, px, &plot);
        let cut = stretches(f, c, l.plot);
        let whole = cut.len() == 1;
        let _span = tracing::debug_span!("tessellate").entered();
        for (clip, pen) in &cut {
            paint(ui, &ink, pen, *clip, whole);
        }
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
    if let Some(rect) = l.key {
        key(ui, f, rect);
    }
}

/// The key: every role the sidecar declares, in its order, right-aligned along `rect`, each a
/// short line drawn as the role's lines are, then its label.
fn key(ui: &Ui, f: &Frame, rect: Rect) {
    let p = f.palette;
    let painter = ui.painter().with_clip_rect(rect);
    let middle = rect.center().y;
    let mut right = rect.right();
    for role in f.roles.iter().rev() {
        let Some(pen) = role_pen(f, &role.name) else {
            continue;
        };
        let galley = painter.layout_no_wrap(role.label.clone(), p.labels.clone(), p.ink_2);
        let left = right - galley.size().x;
        let size = galley.size();
        painter.galley(pos2(left, middle - size.y / 2.0), galley, p.ink_2);
        let line = vec![
            pos2(left - SWATCH_GAP - SWATCH, middle),
            pos2(left - SWATCH_GAP, middle),
        ];
        let stroke = Stroke::new(pen.width, pen.colour);
        match &pen.dash {
            None => {
                painter.add(PathShape::line(line, stroke));
            }
            Some(pattern) => {
                for d in dashes(&line, pattern, 0.0) {
                    painter.add(PathShape::line(d, stroke));
                }
            }
        }
        right = left - SWATCH_GAP - SWATCH - KEY_GAP;
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
    let unit = (f.channel)
        .and_then(|c| f.facts.channels.get(c))
        .map_or("", |c| c.unit.as_str());
    // A graph of lanes alone has no value axis.
    let marks = f.channel.and_then(|_| {
        range_marks(ValueRange {
            lo: f.y.0,
            hi: f.y.1,
        })
    });
    if let Some(m) = marks {
        let right = l.gutter.right() - 6.0;
        let texts = format_marks(m.top, m.bottom, m.place, unit);
        for (value, text, align, nudge) in [
            (m.top, texts.as_ref().map(|t| &t.0), Align2::RIGHT_TOP, 1.0),
            (
                m.bottom,
                texts.as_ref().map(|t| &t.1),
                Align2::RIGHT_BOTTOM,
                -1.0,
            ),
        ] {
            let y = f.y_at(value);
            painter.line_segment([pos2(l.plot.left(), y), pos2(l.plot.right(), y)], hair);
            if let Some(text) = text {
                labels.push((
                    text.clone(),
                    pos2(right, y + nudge),
                    align,
                    p.figures.clone(),
                ));
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

/// The trace's band fills, its lines and its dots, as tints of the colour each stretch draws in,
/// with the lost time shaded under them.
fn trace(f: &Frame, px: f32, plot: &egui::Painter) -> Ink {
    let p = f.palette;
    let mut ink = Ink::default();
    let Some(t) = f.trace else {
        return ink;
    };
    let w = &f.view.window;
    shade_lost(plot, f.view, f.layout.plot, t.lost(w), px, p);
    let mut chain = 0;
    bands(&t.pieces(w), f.view, |v| f.y_at(v), p, &mut ink, &mut chain);
    if let Some(points) = t.samples {
        let mut line = Vec::new();
        let mut drawing: Option<(Tint, i64, f32)> = None;
        chain += 2;
        for s in points {
            if s.value.is_nan() {
                if let Some((was, _, _)) = drawing.take() {
                    ink.line(&mut line, was, chain);
                    chain += 1;
                }
                continue;
            }
            let c = Tint::trace(p, s.flagged);
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
                        ink.line(&mut line, was, chain);
                        if joined {
                            line.push(pos2(x0, before));
                        } else {
                            chain += 1;
                        }
                    }
                    line.extend([pos2(x0, y), pos2(x1, y)]);
                }
            }
            drawing = Some((c, s.end, y));
        }
        if let Some((c, _, _)) = drawing {
            ink.line(&mut line, c, chain);
        }
        let shown = points
            .iter()
            .filter(|s| s.tick >= w.start && s.tick < w.end)
            .count();
        if shown > 0 && f.view.width / shown as f32 >= DOT_SPACING {
            for s in points.iter().filter(|s| !s.value.is_nan()) {
                ink.dots.push((
                    pos2(f.view.x(s.tick as f64), f.y_at(s.value)),
                    Tint::trace(p, s.flagged),
                ));
            }
        }
    }
    ink
}

/// Digital channel `c`'s lane: its gaps shaded, its levels as lines, and where edges crowd, the
/// band between the levels and each level's rail at the opacities `shade` gives them, each
/// stretch in its pen. Inside the answer's `around`, the answer's columns; past it, the channel's
/// columns over the extent, so a window the answer doesn't yet cover draws its levels too.
fn lane(ui: &Ui, f: &Frame, c: usize, rect: Rect, px: f32) {
    let p = f.palette;
    let Some(held) = f.held else { return };
    let Some(at) = held.request.channels.iter().position(|&x| x == c) else {
        return;
    };
    let Some(Drawn::Digital { columns, gaps }) = held.answer.channels.get(at) else {
        return;
    };
    let fallback = (held.request.fallbacks.iter())
        .position(|&(x, _)| x == c)
        .and_then(|i| held.answer.fallbacks.get(i))
        .and_then(|s| match &s.channel {
            Drawn::Digital { columns, gaps } => Some((&s.grid, columns, gaps)),
            _ => None,
        });
    let painter = ui.painter().with_clip_rect(rect);
    let high = rect.top() + LANE_PAD;
    let low = high + LANE_SWING;
    let w = &f.view.window;
    let a = &held.answer.around;
    // The answer's gaps inside `around`, the fallback's past it, so none is shaded twice.
    let mut shaded: Vec<(i64, i64)> = (gaps.iter())
        .map(|g| (g.start.max(a.start), g.end.min(a.end)))
        .collect();
    if let Some((_, _, outside)) = fallback {
        for g in outside {
            shaded.push((g.start, g.end.min(a.start)));
            shaded.push((g.start.max(a.end), g.end));
        }
    }
    for (from, to) in shaded {
        let (from, to) = (from.max(w.start), to.min(w.end));
        if to <= from {
            continue;
        }
        if let Some((x0, x1)) = f.across(from as f64, to as f64, px) {
            painter.rect_filled(
                Rect::from_min_max(pos2(x0, high), pos2(x1, low)),
                0.0,
                p.attention_fill.gamma_multiply(p.shade),
            );
        }
    }
    let g = &held.answer.grid;
    let mut runs = Vec::with_capacity(3);
    if let Some((fg, fc, _)) = fallback {
        runs.push(Run {
            grid: fg,
            columns: fc,
            ks: columns_over(fg, w.start, w.end.min(a.start)),
            from: f64::NEG_INFINITY,
            to: a.start as f64,
        });
    }
    runs.push(Run {
        grid: g,
        columns,
        ks: columns_over(g, w.start.max(a.start), w.end.min(a.end)),
        from: f64::NEG_INFINITY,
        to: f64::INFINITY,
    });
    if let Some((fg, fc, _)) = fallback {
        runs.push(Run {
            grid: fg,
            columns: fc,
            ks: columns_over(fg, w.start.max(a.end), w.end),
            from: a.end as f64,
            to: f64::INFINITY,
        });
    }
    let cut = stretches(f, c, rect);
    let whole = cut.len() == 1;
    // A lane's geometry follows its line's width: one ink a width its stretches draw at.
    let mut inks: Vec<(f32, Ink)> = Vec::new();
    for (clip, pen) in &cut {
        let at = match inks.iter().position(|(width, _)| *width == pen.width) {
            Some(at) => at,
            None => {
                inks.push((pen.width, lane_ink(f, &runs, rect, px, pen.width)));
                inks.len() - 1
            }
        };
        let _span = tracing::debug_span!("tessellate").entered();
        paint(ui, &inks[at].1, pen, *clip, whole);
    }
}

/// A run of a lane's columns: `ks` of `columns` on `grid`, each across no more of the ticks than
/// `from` to `to`.
struct Run<'a> {
    grid: &'a Grid,
    columns: &'a DigitalColumns,
    ks: Range<usize>,
    from: f64,
    to: f64,
}

/// A column as a lane draws it: across `x0` to `x1`, its state, where its edge sits from 0 to 1
/// across it, its look, where a pulse's stroke is, and the run it is in.
struct Col {
    x0: f32,
    x1: f32,
    state: State,
    at: f64,
    look: Look,
    pulse: f32,
    run: usize,
}

/// A lane's levels as lines and its crowded columns as fills, for a line `weight` points wide,
/// from its `runs` in order.
fn lane_ink(f: &Frame, runs: &[Run], rect: Rect, px: f32, weight: f32) -> Ink {
    let high = rect.top() + LANE_PAD;
    let low = high + LANE_SWING;
    let width = f64::from(weight / px);
    let mut cols: Vec<Col> = Vec::new();
    for (i, r) in runs.iter().enumerate() {
        let g = r.grid;
        let looks = shade::looks(r.columns, r.ks.clone(), width, g.per, f.facts.timebase);
        for k in r.ks.clone() {
            let start = g.x0 + k as f64 * g.per;
            let pulse = match looks[k] {
                Look::Pulse { at, .. } => f.view.x(g.x0 + at * g.per),
                _ => 0.0,
            };
            cols.push(Col {
                x0: f.view.x(start.max(r.from)),
                x1: f.view.x((g.x0 + (k + 1) as f64 * g.per).min(r.to)),
                state: r.columns.state[k],
                at: r.columns.at[k],
                look: looks[k],
                pulse,
                run: i,
            });
        }
    }
    let mut ink = Ink::default();
    let mut chain = 0;
    let half = weight / 2.0;
    let mut line = Vec::new();
    let mut pen: Option<f32> = None;
    let mut k = 0;
    while k < cols.len() {
        let Col {
            x0, x1, state, at, ..
        } = cols[k];
        match (state, cols[k].look) {
            (State::None, _) => {
                // A gap: the line's dash starts again after it.
                ink.line(&mut line, Tint::FULL, chain);
                chain += 1;
                pen = None;
            }
            (_, Look::Pulse { alpha, .. }) => {
                // As a lone edge: the line in at the level before its first column to its centre,
                // a stroke across the band, and out at the level after its last column.
                let last = (k..cols.len())
                    .take_while(|&j| cols[j].run == cols[k].run && cols[j].look == cols[k].look)
                    .last()
                    .unwrap_or(k);
                let x = cols[k].pulse;
                let before = match state {
                    State::Rise => Some(low),
                    State::Fall => Some(high),
                    _ => pen,
                };
                if let Some(y) = before {
                    if pen != Some(y) {
                        ink.line(&mut line, Tint::FULL, chain);
                        line.push(pos2(x0, y));
                    }
                    step(&mut line, pos2(x, y));
                }
                ink.line(&mut line, Tint::FULL, chain);
                ink.fills.push((
                    Rect::from_min_max(pos2(x - half, high - half), pos2(x + half, low + half)),
                    Tint(alpha as f32, 1.0),
                ));
                let after = match (cols[last].state, cols.get(last + 1).map(|c| c.state)) {
                    (State::Rise, _) => Some(high),
                    (State::Fall, _) => Some(low),
                    (_, Some(State::High | State::Fall)) => Some(high),
                    (_, Some(State::Low | State::Rise)) => Some(low),
                    _ => before,
                };
                pen = after;
                if let Some(y) = after {
                    line.extend([pos2(x, y), pos2(cols[last].x1, y)]);
                }
                k = last + 1;
                continue;
            }
            (
                _,
                Look::Shade {
                    band,
                    high: h,
                    low: l,
                },
            ) => {
                let (band, h, l) = (band as f32, h as f32, l as f32);
                ink.line(&mut line, Tint::FULL, chain);
                pen = None;
                let x1 = x1.max(x0 + px);
                let mut rect = |top: f32, bottom: f32, alpha: f32| {
                    if alpha > 0.0 {
                        ink.fills.push((
                            Rect::from_min_max(pos2(x0, top), pos2(x1, bottom)),
                            Tint(alpha, 1.0),
                        ));
                    }
                };
                rect(high + half, low - half, band);
                rect(high - half, high + half, h);
                rect(low - half, low + half, l);
            }
            (State::Low | State::High, Look::Held(alpha)) => {
                ink.line(&mut line, Tint::FULL, chain);
                pen = None;
                let y = if state == State::High { high } else { low };
                ink.fills.push((
                    Rect::from_min_max(pos2(x0, y - half), pos2(x1.max(x0 + px), y + half)),
                    Tint(alpha as f32, 1.0),
                ));
            }
            (State::Low | State::High, _) => {
                let y = if state == State::High { high } else { low };
                if pen != Some(y) {
                    ink.line(&mut line, Tint::FULL, chain);
                    line.push(pos2(x0, y));
                }
                step(&mut line, pos2(x1, y));
                pen = Some(y);
            }
            (State::Rise | State::Fall, Look::Stroke(alpha)) => {
                let alpha = alpha as f32;
                let (ya, yb) = if state == State::Rise {
                    (low, high)
                } else {
                    (high, low)
                };
                let x = x0 + at as f32 * (x1 - x0);
                if pen != Some(ya) {
                    ink.line(&mut line, Tint::FULL, chain);
                    line.push(pos2(x0, ya));
                }
                step(&mut line, pos2(x, ya));
                if alpha >= 1.0 {
                    line.push(pos2(x, yb));
                } else {
                    // Its levels at full ink, the edge itself at the opacity the model gives it.
                    ink.line(&mut line, Tint::FULL, chain);
                    ink.fills.push((
                        Rect::from_min_max(pos2(x - half, high - half), pos2(x + half, low + half)),
                        Tint(alpha, 1.0),
                    ));
                    line.push(pos2(x, yb));
                }
                line.push(pos2(x1, yb));
                pen = Some(yb);
            }
            (State::Rise | State::Fall | State::Busy, _) => {
                debug_assert!(false, "a changing column is a stroke, a pulse or a shade");
            }
        }
        k += 1;
    }
    ink.line(&mut line, Tint::FULL, chain);
    ink
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

/// The strip: the trace's columns over the extent, stretched across `rect`, with its lost spans
/// shaded under them and the window washed over them.
pub(crate) fn strip(
    ui: &Ui,
    rect: Rect,
    answer: &StripAnswer,
    window: &Range<i64>,
    include_zero: bool,
    p: &Palette,
    trace: Color32,
) {
    let px = 1.0 / ui.ctx().pixels_per_point();
    let painter = ui.painter().with_clip_rect(rect);
    let view = View::new(answer.extent.clone(), rect);
    if let Drawn::Analog { columns, lost, .. } = &answer.channel {
        let c = Columns {
            grid: &answer.grid,
            columns,
            lost,
        };
        let mut pieces = Vec::new();
        part(
            &c,
            answer.extent.start,
            answer.extent.end,
            false,
            &mut pieces,
        );
        shade_lost(&painter, &view, rect, lost.iter().cloned(), px, p);
        let raw = pieces
            .iter()
            .map(|piece| (piece.min, piece.max))
            .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)));
        if let Some(raw) = raw {
            let (lo, hi) = fitted(raw, include_zero, 0, 0.0, rect.height());
            let y_at = |v: f64| rect.bottom() - ((v - lo) / (hi - lo)) as f32 * rect.height();
            let mut ink = Ink::default();
            bands(&pieces, &view, y_at, p, &mut ink, &mut 0);
            let pen = Pen {
                colour: trace,
                width: p.line_weight,
                dash: None,
            };
            let _span = tracing::debug_span!("tessellate").entered();
            paint(ui, &ink, &pen, rect, true);
        }
    }
    if let Some((a, b)) = across(&view, rect, window.start as f64, window.end as f64, px) {
        painter.rect_filled(
            Rect::from_min_max(pos2(a, rect.top()), pos2(b, rect.bottom())),
            0.0,
            p.wash,
        );
    }
}

/// The strip's time axis along `rect`: ticks at its top and their labels under them, over the
/// extent the strip spans.
pub(crate) fn strip_axis(ui: &Ui, rect: Rect, extent: &Range<i64>, timebase: Ratio, p: &Palette) {
    let painter = ui.painter().with_clip_rect(rect);
    let axis = time_axis(
        extent.start as f64,
        extent.end as f64,
        f64::from(rect.width()),
        timebase,
        |s| {
            painter
                .layout_no_wrap(s.to_owned(), p.figures.clone(), p.ink_2)
                .size()
                .x
        },
        AXIS_GAP,
    );
    let tick = Stroke::new(1.0, p.ink_2);
    for t in &axis.ticks {
        let x = rect.left() + t.x as f32;
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.top() + 3.0)], tick);
        painter.text(
            pos2(x, rect.top() + 3.0),
            Align2::CENTER_TOP,
            &t.label,
            p.figures.clone(),
            p.ink_2,
        );
    }
}
