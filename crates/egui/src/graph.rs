//! The graph: one analog trace, or none, DIO lanes under it on the same time axis, markers, the
//! selection, and the gestures that move them.

use std::ops::Range;

use egui::{
    AsIdSalt, CursorIcon, IdSalt, PointerButton, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    Vec2, Widget, WidgetInfo, pos2, vec2,
};
use escapement_tickwave_view::format::model::Ratio;
use escapement_tickwave_view::readouts::{MINUS, NBSP, format_interval};
use escapement_tickwave_view::scale::{ValueRange, move_range, scale_limits, zoom_range};
use escapement_tickwave_view::{MarkerCursor, MarkerQuery, Resolution};

use crate::draw::{self, Trace};
use crate::palette::Palette;
use crate::state::{ViewState, Window, Y};
use crate::viewer::{Facts, Held, Request, Viewer};

/// The window's markers drawn at most; past it the graph says there are more.
const MARKER_LIMIT: usize = 512;
/// Vertical travel, in points, before a drag or the wheel takes Y off the fit, so a pan along
/// time that wanders a little leaves Y fitted.
const DETENT: f32 = 12.0;
/// The closest zoom: this many points a tick.
const MAX_POINTS_PER_TICK: f64 = 40.0;
const MAX_TICKS: f64 = (1u64 << 62) as f64;
pub(crate) const LANE_HEIGHT: f32 = 24.0;
pub(crate) const AXIS_HEIGHT: f32 = 24.0;
const LABEL_ROWS: usize = 3;
pub(crate) const LABEL_TOP: f32 = 4.0;
/// Room above and below the key's labels.
const KEY_PAD: f32 = 3.0;
const MIN_TRACE: f32 = 40.0;
/// A zoom step's ease, in seconds.
const EASE: f64 = 0.15;
/// How near a marker's line, in points, the pointer reads it.
const MARKER_REACH: f32 = 3.0;
/// The Live toggle's word, and the room around it.
const LIVE: &str = "Live";
const LIVE_PAD: Vec2 = vec2(7.0, 2.0);

/// What a host adds to the graph's query for its own panel. A host builds one with `default` and
/// sets its fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Extras {
    /// A page of the list of every marker: from after a cursor, at most so many.
    pub marker_list: Option<(Option<MarkerCursor>, usize)>,
    /// The marker after this one, wherever it starts.
    pub successor_of: Option<MarkerCursor>,
}

/// An analog or derived channel's trace, with digital channels in lanes under it, or digital
/// channels alone (`lanes_only`). It queries through its `Viewer` and draws the latest answer,
/// mapped to the window on screen while a newer one is on the way.
///
/// Drag or scroll to move along time, ctrl-scroll or pinch to zoom it. A vertical drag or
/// scroll past a 12-point detent takes Y off the fit and moves it, a drag on the value axis
/// zooms it, and a double click fits it again. With `select_mode` a drag selects, and stops a
/// followed window where it is drawn; a zoom keeps following. The response is `changed()` when
/// the window or the selection moved.
///
/// A zoom step sets the host's window to where it goes at once, and the graph eases the window it
/// draws there over 150 ms, unless `motion` is off. A step during an ease starts again from the
/// window drawn; a pan, or a window the host sets, jumps.
pub struct Graph<'a> {
    viewer: &'a Viewer,
    state: &'a mut ViewState,
    palette: &'a Palette,
    trace: Option<usize>,
    flag: Option<usize>,
    lanes: &'a [usize],
    include_zero: bool,
    extras: Extras,
    height: Option<f32>,
    now: Option<i64>,
    id_salt: IdSalt,
    motion: bool,
    /// A gesture this frame zoomed the window.
    zoomed: bool,
}

impl<'a> Graph<'a> {
    /// `trace` is the analog or derived channel drawn, by index.
    pub fn new(
        viewer: &'a Viewer,
        state: &'a mut ViewState,
        palette: &'a Palette,
        trace: usize,
    ) -> Self {
        Self {
            viewer,
            state,
            palette,
            trace: Some(trace),
            flag: None,
            lanes: &[],
            include_zero: false,
            extras: Extras::default(),
            height: None,
            now: None,
            id_salt: IdSalt::new("tickwave graph"),
            motion: true,
            zoomed: false,
        }
    }

    /// Digital channels alone, in the lanes `lanes` gives, with no trace: the graph is its lanes,
    /// a band over them for the markers' labels where the recording has markers, and the time
    /// axis, and its height is what those take unless `height` gives one. A flag, with no samples
    /// to draw, does nothing; nor does a vertical drag or the value axis's, which it hasn't.
    pub fn lanes_only(viewer: &'a Viewer, state: &'a mut ViewState, palette: &'a Palette) -> Self {
        Self {
            trace: None,
            ..Self::new(viewer, state, palette, 0)
        }
    }

    /// A digital channel: where it is high at a single sample's tick, the sample draws pale.
    pub fn flag(mut self, flag: Option<usize>) -> Self {
        self.flag = flag;
        self
    }

    /// Digital channels, a lane each under the trace, in this order.
    pub fn lanes(mut self, lanes: &'a [usize]) -> Self {
        self.lanes = lanes;
        self
    }

    /// The fitted value range reaches zero.
    pub fn include_zero(mut self, include: bool) -> Self {
        self.include_zero = include;
        self
    }

    pub fn extras(mut self, extras: Extras) -> Self {
        self.extras = extras;
        self
    }

    /// The whole graph's height, lanes and time axis included; the space left in the `Ui` by
    /// default.
    pub fn height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }

    /// The host's now, in ticks, while the recording is being written: a followed window ends
    /// there where it is past the data. None once the recording is finished, and for a finished
    /// file.
    pub fn now(mut self, now: Option<i64>) -> Self {
        self.now = now;
        self
    }

    /// Tells two graphs in one `Ui` apart.
    pub fn id_salt(mut self, salt: impl AsIdSalt) -> Self {
        self.id_salt = IdSalt::new(salt);
        self
    }

    /// Whether a zoom step eases the window drawn to where it goes; on by default. Off, it jumps,
    /// for a host whose reader asked for reduced motion.
    pub fn motion(mut self, on: bool) -> Self {
        self.motion = on;
        self
    }
}

/// Where the graph's parts sit, in points.
#[derive(Clone, Debug)]
pub(crate) struct Layout {
    pub rect: Rect,
    /// Left of the plot: the value axis beside the trace, the lane names beside the lanes.
    pub gutter: Rect,
    pub plot: Rect,
    pub lanes: Vec<Rect>,
    pub axis: Rect,
    /// A marker label's row.
    pub label_row: f32,
    /// The key of the recording's paint roles, under the time axis: kept whenever the recording
    /// declares a role, so its height never changes as the reader moves.
    pub key: Option<Rect>,
}

/// The window on screen, along the plot.
#[derive(Clone, Debug)]
pub(crate) struct View {
    pub window: Range<i64>,
    pub left: f32,
    pub width: f32,
    /// Ticks a point.
    pub per: f64,
}

impl View {
    pub(crate) fn new(window: Range<i64>, plot: Rect) -> Self {
        let width = plot.width().max(1.0);
        Self {
            per: (window.end - window.start) as f64 / f64::from(width),
            window,
            left: plot.left(),
            width,
        }
    }

    pub fn x(&self, tick: f64) -> f32 {
        self.left + ((tick - self.window.start as f64) / self.per) as f32
    }

    pub fn tick(&self, x: f32) -> f64 {
        self.window.start as f64 + f64::from(x - self.left) * self.per
    }
}

/// What a graph drew in its last frame: the window across its plot, and the trace's value range.
/// A host reads it with `Graph::shown`, to fix Y where it is on screen, say. A host builds none.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Shown {
    pub window: Range<i64>,
    /// Low, then high, in the trace's unit. A graph of lanes alone draws no value range: this is
    /// the one a trace would start from.
    pub y: (f64, f64),
    #[cfg(test)]
    pub(crate) layout: Layout,
}

/// A zoom step under way: the window drawn eases from `from` to the window the graph set.
#[derive(Clone, Debug)]
struct Ease {
    /// The window drawn when the step was taken.
    from: Range<i64>,
    /// The window the graph set: the host's, until the host sets another.
    to: Window,
    /// When the step was taken, in egui's input time.
    since: f64,
}

#[derive(Clone, Debug, Default)]
struct Memory {
    ease: Option<Ease>,
    /// Vertical travel while Y fits, toward the detent.
    travel: f32,
    /// A pan's part of a tick not yet moved.
    pan: f64,
    /// Where a selection drag started.
    anchor: Option<i64>,
    shown: Option<Shown>,
}

/// Ranges with low below high: a fixed range with none gets one.
fn ordered((lo, hi): (f64, f64)) -> (f64, f64) {
    if hi > lo {
        (lo, hi)
    } else {
        (lo - 1.0, lo + 1.0)
    }
}

/// The fitted range from the values shown: zero where the graph includes it, a twentieth either
/// side, and the top raised so `rows` of marker labels sit above the values.
pub(crate) fn fitted(
    (lo, hi): (f64, f64),
    include_zero: bool,
    rows: usize,
    row: f32,
    height: f32,
) -> (f64, f64) {
    let (lo, hi) = if include_zero {
        (lo.min(0.0), hi.max(0.0))
    } else {
        (lo, hi)
    };
    let margin = if hi > lo { (hi - lo) * 0.05 } else { 1.0 };
    headroom((lo - margin, hi + margin), rows, row, height)
}

/// `range` raised so that its top lies below `rows` of marker labels in a plot `height` tall.
pub(crate) fn headroom((lo, hi): (f64, f64), rows: usize, row: f32, height: f32) -> (f64, f64) {
    let band = f64::from(LABEL_TOP + rows as f32 * row);
    let height = f64::from(height);
    if rows == 0 || height <= 2.0 * band {
        return (lo, hi);
    }
    let share = band / height;
    (lo, (hi - lo * share) / (1.0 - share))
}

impl Graph<'_> {
    fn layout(&self, ui: &mut Ui, facts: &Facts) -> Layout {
        let _span = tracing::debug_span!("layout").entered();
        let p = self.palette;
        let unit = (self.trace)
            .and_then(|t| facts.channels.get(t))
            .map_or("", |c| c.unit.as_str());
        let measure = |text: String, font: &egui::FontId| {
            ui.painter()
                .layout_no_wrap(text, font.clone(), p.ink_2)
                .size()
                .x
        };
        // As wide as the widest value label a unit can take, so the plot stays where it is as
        // the labels change; with no trace, as wide as the lanes' names.
        let mut gutter = match self.trace {
            Some(_) => measure(format!("{MINUS}888.888{NBSP}M{unit}"), &p.figures),
            None => 0.0,
        };
        for &c in self.lanes {
            if let Some(info) = facts.channels.get(c) {
                gutter = gutter.max(measure(info.name.clone(), &p.labels));
            }
        }
        let gutter = (gutter + 12.0).ceil().max(40.0);
        let label_row = (ui.fonts_mut(|f| f.row_height(&p.labels)) + 2.0).ceil();
        let lanes = self.lanes.len() as f32 * LANE_HEIGHT;
        let key = match facts.roles.is_empty() {
            true => 0.0,
            false => label_row + 2.0 * KEY_PAD,
        };
        let height = match self.trace {
            Some(_) => self
                .height
                .unwrap_or_else(|| ui.available_height())
                .max(MIN_TRACE + lanes + AXIS_HEIGHT + key),
            // The markers' labels in their rows above the lanes, where there are markers.
            None => {
                let band = match facts.markers {
                    true => LABEL_TOP + LABEL_ROWS as f32 * label_row,
                    false => 0.0,
                };
                let least = band + lanes + AXIS_HEIGHT + key;
                self.height.unwrap_or(least).max(least)
            }
        };
        let width = ui.available_width().max(gutter + 40.0);
        let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let plot = Rect::from_min_max(
            pos2(rect.left() + gutter, rect.top()),
            pos2(rect.right(), rect.bottom() - lanes - AXIS_HEIGHT - key),
        );
        let lanes = (0..self.lanes.len())
            .map(|i| {
                let top = plot.bottom() + i as f32 * LANE_HEIGHT;
                Rect::from_min_max(
                    pos2(plot.left(), top),
                    pos2(plot.right(), top + LANE_HEIGHT),
                )
            })
            .collect();
        Layout {
            gutter: Rect::from_min_max(rect.left_top(), pos2(plot.left(), plot.bottom())),
            axis: Rect::from_min_max(
                pos2(plot.left(), rect.bottom() - key - AXIS_HEIGHT),
                pos2(rect.right(), rect.bottom() - key),
            ),
            key: (key > 0.0).then(|| {
                Rect::from_min_max(pos2(plot.left(), rect.bottom() - key), rect.right_bottom())
            }),
            rect,
            plot,
            lanes,
            label_row,
        }
    }

    /// The trace's value range as drawn over `view`, and the fit it would have.
    fn y(
        &self,
        trace: Option<&Trace>,
        view: &View,
        layout: &Layout,
        facts: &Facts,
        before: Option<&Shown>,
    ) -> ((f64, f64), Option<(f64, f64)>) {
        let rows = if facts.markers { LABEL_ROWS } else { 0 };
        let fit = trace.and_then(|t| t.fit(view)).map(|raw| {
            fitted(
                raw,
                self.include_zero,
                rows,
                layout.label_row,
                layout.plot.height(),
            )
        });
        let y = match self.state.y {
            Y::Fixed { low, high } => (low, high),
            // With nothing in view, the range stays where it was.
            Y::Fit => fit.or(before.map(|s| s.y)).unwrap_or(if self.include_zero {
                (0.0, 1.0)
            } else {
                (-1.0, 1.0)
            }),
        };
        (ordered(y), fit)
    }

    fn limits(
        &self,
        facts: &Facts,
        whole: (f64, f64),
    ) -> escapement_tickwave_view::scale::ScaleLimits {
        let channel = self.trace.and_then(|t| facts.channels.get(t));
        let resolution = match channel.and_then(|c| c.resolution) {
            Some(Resolution::Code { scale, .. }) => {
                Some((scale.num() as f64 / scale.den() as f64).abs())
            }
            Some(Resolution::Step(step)) => Some(step.abs()),
            None => None,
        };
        let (lo, hi) = ordered(whole);
        scale_limits(ValueRange { lo, hi }, resolution)
    }

    /// The window `view` shows, as one the user has moved.
    fn moved(&mut self, window: Range<i64>) {
        self.state.window = Window::At(window);
    }

    fn pan(&mut self, view: &View, by: f32, memory: &mut Memory) {
        let ticks = memory.pan - f64::from(by) * view.per;
        let whole = ticks.round();
        memory.pan = ticks - whole;
        if whole != 0.0 {
            let by = whole as i64;
            let w = &view.window;
            self.moved(w.start.saturating_add(by)..w.end.saturating_add(by));
        }
    }

    /// Zooms the window the view goes to, `target`, by `factor` about the tick under `at` there. A
    /// followed window of a recording being written keeps following, its span zoomed about its
    /// end.
    fn zoom(&mut self, target: &View, at: f32, factor: f32, live: bool) {
        let w = &target.window;
        let width = (w.end - w.start) as f64;
        let min = (f64::from(target.width) / MAX_POINTS_PER_TICK)
            .ceil()
            .max(1.0);
        let new = (width / f64::from(factor)).clamp(min, MAX_TICKS).round();
        self.zoomed = true;
        if live && let Window::Follow { .. } = self.state.window {
            self.state.window = Window::Follow { span: new as i64 };
            return;
        }
        let tick = target.tick(at);
        let start = (tick - (tick - w.start as f64) / width * new).round();
        self.moved(start as i64..(start + new) as i64);
    }

    /// The window drawn: the ease's at time `now`, or the target where none is under way. An ease
    /// that has run its time ends.
    fn drawn(&self, memory: &mut Memory, extent: Option<&Range<i64>>, now: f64) -> Range<i64> {
        let target = self.state.window.resolve(extent, self.now);
        let Some(e) = &memory.ease else {
            return target;
        };
        let k = ((now - e.since) / EASE).clamp(0.0, 1.0);
        if k >= 1.0 {
            memory.ease = None;
            return target;
        }
        // Ease-out: quick at first, settling into place.
        let f = 1.0 - (1.0 - k).powi(3);
        let along = |a: i64, b: i64| (a as f64 + (b as f64 - a as f64) * f).round() as i64;
        match &e.to {
            Window::At(_) => along(e.from.start, target.start)..along(e.from.end, target.end),
            // The span eases, its end where the followed window's end is now.
            Window::Follow { span } => Window::Follow {
                span: along(e.from.end - e.from.start, *span),
            }
            .resolve(extent, self.now),
        }
    }

    /// The value range moved by `by` points down, its content following.
    fn y_moved(
        &mut self,
        y: (f64, f64),
        by: f32,
        layout: &Layout,
        facts: &Facts,
        whole: (f64, f64),
    ) {
        let per = (y.1 - y.0) / f64::from(layout.plot.height().max(1.0));
        let r = move_range(
            ValueRange { lo: y.0, hi: y.1 },
            f64::from(by) * per,
            &self.limits(facts, whole),
        );
        self.state.y = Y::Fixed {
            low: r.lo,
            high: r.hi,
        };
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a frame's gestures read all of it"
    )]
    fn gestures(
        &mut self,
        ui: &Ui,
        response: &Response,
        axis: &Response,
        layout: &Layout,
        (view, target): (&View, &View),
        y: (f64, f64),
        fit: Option<(f64, f64)>,
        facts: &Facts,
        memory: &mut Memory,
    ) {
        let whole = fit.unwrap_or(y);
        if response.double_clicked() {
            self.state.y = Y::Fit;
            memory.travel = 0.0;
        }
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let over = response.contains_pointer();
        if over && let Some(p) = pointer {
            let factor = ui.input(|i| i.zoom_delta_2d().x);
            if factor != 1.0 {
                self.zoom(target, p.x, factor, facts.live);
            }
        }
        let scroll = if over {
            ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta))
        } else {
            Vec2::ZERO
        };
        let panning = response.dragged_by(PointerButton::Primary) && !self.state.select_mode;
        if self.state.select_mode && response.dragged_by(PointerButton::Primary) {
            let clamp = |x: f32| x.clamp(layout.plot.left(), layout.plot.right());
            let tick = |x: f32| view.tick(clamp(x)).round() as i64;
            if response.drag_started() || memory.anchor.is_none() {
                memory.anchor = ui.input(|i| i.pointer.press_origin()).map(|p| tick(p.x));
            }
            if let (Some(a), Some(p)) = (memory.anchor, response.interact_pointer_pos()) {
                let b = tick(p.x);
                if a != b {
                    self.state.selection = Some(a.min(b)..a.max(b));
                    // The span marked stays where it was drawn: a followed window stops there.
                    if let Window::Follow { .. } = self.state.window {
                        self.moved(view.window.clone());
                    }
                }
            }
        }
        if !response.dragged() {
            memory.anchor = None;
        }
        let drag = if panning {
            response.drag_delta()
        } else {
            Vec2::ZERO
        };
        let delta = scroll + drag;
        // A pan during an ease moves on from where the ease was going.
        if delta.x != 0.0 {
            self.pan(target, delta.x, memory);
        }
        if response.double_clicked() || self.trace.is_none() {
            return;
        }
        match self.state.y {
            Y::Fit => {
                if !panning && scroll == Vec2::ZERO {
                    memory.travel = 0.0;
                } else if delta.y.abs() > delta.x.abs() {
                    memory.travel += delta.y;
                    if memory.travel.abs() > DETENT {
                        let travel = std::mem::take(&mut memory.travel);
                        self.y_moved(y, travel, layout, facts, whole);
                    }
                }
            }
            Y::Fixed { .. } => {
                memory.travel = 0.0;
                if delta.y != 0.0 {
                    self.y_moved(y, delta.y, layout, facts, whole);
                }
            }
        }
        if axis.dragged_by(PointerButton::Primary)
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            let dy = axis.drag_delta().y;
            if dy != 0.0 {
                // Down zooms in, about where the drag started, as egui_plot's axis drag does for a
                // slow drag; finite however far one frame moves.
                let factor = (-0.02 * f64::from(dy)).exp();
                let now = match self.state.y {
                    Y::Fixed { low, high } => (low, high),
                    Y::Fit => y,
                };
                let at = now.0
                    + f64::from(layout.plot.bottom() - origin.y) / f64::from(layout.plot.height())
                        * (now.1 - now.0);
                let r = zoom_range(
                    ValueRange {
                        lo: now.0,
                        hi: now.1,
                    },
                    at,
                    factor,
                    &self.limits(facts, whole),
                );
                self.state.y = Y::Fixed {
                    low: r.lo,
                    high: r.hi,
                };
            }
        }
    }

    fn request(&self, columns: u32, facts: &Facts) -> Request {
        let mut channels: Vec<usize> = self.trace.into_iter().collect();
        channels.extend_from_slice(self.lanes);
        let following = matches!(self.state.window, Window::Follow { .. });
        let mut markers = MarkerQuery::default();
        markers.limit = MARKER_LIMIT;
        markers.list = self.extras.marker_list;
        markers.successor_of = self.extras.successor_of;
        // Every paint over the answer's span, however many markers the window holds, where the
        // recording declares a role to draw it in.
        markers.paints = !facts.roles.is_empty();
        Request {
            window: self.state.window.clone(),
            // A window of its own doesn't move with now, so a moving now asks nothing.
            now: self.now.filter(|_| following),
            columns,
            channels,
            flags: (self.trace.zip(self.flag)).into_iter().collect(),
            strip: self.trace.map(|t| (t, columns)),
            fallbacks: self.lanes.iter().map(|&l| (l, columns)).collect(),
            selection: self.state.selection.clone(),
            markers,
        }
    }

    /// Asks for `wanted` when it differs from what was asked, now aside. A followed window also
    /// asks when the window on screen has left what the held answer covers; a moving now alone
    /// asks nothing, since the held answer covers a width either side and is drawn moved with
    /// it, and the viewer answers a followed request again after each snapshot itself. The
    /// request carries the latest now, so the query ends its window there. A change of the
    /// channels drawn asks at once; anything else waits for the query under way, so a pan draws
    /// its answers as they come rather than cancelling each one.
    fn ask(&self, wanted: Request, window: &Range<i64>, held: Option<&Held>) {
        let sent = self.viewer.sent();
        let structural = sent.as_ref().is_none_or(|s| {
            s.channels != wanted.channels
                || s.flags != wanted.flags
                || s.strip.map(|s| s.0) != wanted.strip.map(|s| s.0)
        });
        let changed = sent.as_deref().is_none_or(|s| {
            *s != Request {
                now: s.now,
                ..wanted.clone()
            }
        });
        let following = matches!(wanted.window, Window::Follow { .. });
        let left = following
            && held.is_some_and(|h| {
                window.start < h.answer.around.start || window.end > h.answer.around.end
            });
        if structural || (!self.viewer.busy() && (changed || left)) {
            self.viewer.request(wanted);
        }
    }
}

impl Widget for Graph<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        let _span = tracing::debug_span!("graph").entered();
        let id = ui.make_persistent_id(self.id_salt);
        let facts = self.viewer.facts();
        let held: Option<Held> = self.viewer.held();
        let layout = self.layout(ui, &facts);
        let mut response = ui.interact(layout.rect, id, Sense::click_and_drag());
        let value_axis = match self.trace {
            Some(_) => ui
                .interact(layout.gutter, id.with("value axis"), Sense::drag())
                .on_hover_and_drag_cursor(CursorIcon::ResizeVertical),
            None => ui.interact(layout.gutter, id.with("value axis"), Sense::hover()),
        };
        let mut memory: Memory = ui.data(|d| d.get_temp(id)).unwrap_or_default();
        let before = (self.state.window.clone(), self.state.selection.clone());
        let extent = held.as_ref().and_then(|h| h.answer.extent.clone());
        let trace = (held.as_ref().zip(self.trace)).and_then(|(h, t)| Trace::of(h, t));
        let time = ui.input(|i| i.time);
        // The host set a window other than the one the ease is going to: it jumps there.
        if memory
            .ease
            .as_ref()
            .is_some_and(|e| e.to != self.state.window)
        {
            memory.ease = None;
        }

        let view = View::new(self.drawn(&mut memory, extent.as_ref(), time), layout.plot);
        let target = View::new(
            self.state.window.resolve(extent.as_ref(), self.now),
            layout.plot,
        );
        let (y, fit) = self.y(
            trace.as_ref(),
            &view,
            &layout,
            &facts,
            memory.shown.as_ref(),
        );
        self.gestures(
            ui,
            &response,
            &value_axis,
            &layout,
            (&view, &target),
            y,
            fit,
            &facts,
            &mut memory,
        );
        if self.state.window != before.0 {
            memory.ease = (self.zoomed && self.motion).then(|| Ease {
                from: view.window.clone(),
                to: self.state.window.clone(),
                since: time,
            });
        }

        let view = View::new(self.drawn(&mut memory, extent.as_ref(), time), layout.plot);
        let (y, _) = self.y(
            trace.as_ref(),
            &view,
            &layout,
            &facts,
            memory.shown.as_ref(),
        );
        let live = facts.live.then(|| self.live_rect(ui, &layout));
        if let Some(rect) = live {
            self.live_toggle(ui, id, rect, &view);
        }
        {
            let _span = tracing::debug_span!("build query").entered();
            let columns = (layout.plot.width() * ui.pixels_per_point())
                .round()
                .max(1.0) as u32;
            let wanted = self.request(columns, &facts);
            self.ask(wanted, &view.window, held.as_ref());
        }
        draw::graph(
            ui,
            &draw::Frame {
                layout: &layout,
                view: &view,
                y,
                palette: self.palette,
                facts: &facts,
                held: held.as_ref(),
                channel: self.trace,
                trace: trace.as_ref(),
                lanes: self.lanes,
                selection: self.state.selection.as_ref(),
                ink: self.palette.trace_colour(facts.stored.as_ref()),
                roles: &facts.roles,
                paints: held.as_ref().map_or(&[][..], |h| &h.answer.markers.paints),
                pixels_per_point: ui.pixels_per_point(),
                live,
            },
        );
        if let Some(rect) = live {
            self.paint_live(ui, id.with("live"), rect);
        }
        if let Some(h) = &held {
            hover_markers(ui, id, h, &view, &layout, facts.timebase);
        }
        memory.shown = Some(Shown {
            y,
            window: view.window.clone(),
            #[cfg(test)]
            layout,
        });
        let easing = memory.ease.is_some();
        ui.data_mut(|d| d.insert_temp(id, memory));
        // Following a recording as it is written, the window moves every frame; so does an ease.
        if easing || (facts.live && matches!(self.state.window, Window::Follow { .. })) {
            ui.request_repaint();
        }
        if (self.state.window.clone(), self.state.selection.clone()) != before {
            response.mark_changed();
        }
        response
    }
}

impl Graph<'_> {
    /// Where the Live toggle sits: at the time axis's right end, one size whichever its state.
    fn live_rect(&self, ui: &Ui, layout: &Layout) -> Rect {
        let p = self.palette;
        let word = ui
            .painter()
            .layout_no_wrap(LIVE.into(), p.labels.clone(), p.ink);
        let size = word.size() + 2.0 * LIVE_PAD;
        let axis = layout.axis;
        Rect::from_min_size(
            pos2(axis.right() - size.x - 1.0, axis.center().y - size.y / 2.0),
            size,
        )
    }

    /// The Live toggle, while the recording is being written: a click, or Space or Enter while it
    /// has the focus, stops following at the window drawn, or follows again at its span.
    fn live_toggle(&mut self, ui: &Ui, id: egui::Id, rect: Rect, view: &View) {
        let following = matches!(self.state.window, Window::Follow { .. });
        let response = ui.interact(rect, id.with("live"), Sense::click());
        response.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, following, LIVE));
        if response.clicked() {
            let w = &view.window;
            self.state.window = match following {
                true => Window::At(w.clone()),
                false => Window::Follow {
                    span: w.end - w.start,
                },
            };
        }
    }

    /// The toggle as the window is now: filled in ink while it follows, outlined while not, with
    /// an ink ring while it has the focus.
    fn paint_live(&self, ui: &Ui, id: egui::Id, rect: Rect) {
        let p = self.palette;
        let following = matches!(self.state.window, Window::Follow { .. });
        let (fill, text) = match following {
            true => (p.ink, p.surface),
            false => (p.surface, p.ink),
        };
        let painter = ui.painter();
        painter.rect(rect, 3.0, fill, Stroke::new(1.0, p.ink), StrokeKind::Inside);
        let word = painter.layout_no_wrap(LIVE.into(), p.labels.clone(), text);
        painter.galley(rect.min + LIVE_PAD, word, text);
        if ui.memory(|m| m.has_focus(id)) {
            painter.rect_stroke(
                rect.expand(2.0),
                4.0,
                Stroke::new(2.0, p.ink),
                StrokeKind::Outside,
            );
        }
    }
}

/// Each marker in the window answers the pointer near its line, over the plot and the lanes, with
/// its time and its label.
fn hover_markers(
    ui: &Ui,
    id: egui::Id,
    held: &Held,
    view: &View,
    layout: &Layout,
    timebase: Ratio,
) {
    let plot = layout.plot;
    let bottom = layout.lanes.last().map_or(plot.bottom(), |r| r.bottom());
    for (i, m) in held.answer.markers.window.entries.iter().enumerate() {
        let x = view.x(m.cursor.tick as f64);
        if x < plot.left() || x > plot.right() {
            continue;
        }
        let near = Rect::from_min_max(
            pos2(x - MARKER_REACH, plot.top()),
            pos2(x + MARKER_REACH, bottom),
        );
        let time = format_interval(m.cursor.tick, timebase, Ratio::ONE);
        let text = match draw::marker_text(m) {
            label if label.is_empty() => time,
            label => format!("{time} · {label}"),
        };
        ui.interact(near, id.with(("marker", i)), Sense::hover())
            .on_hover_text(text);
    }
}

impl Graph<'_> {
    /// What the graph whose response `response` is drew in its last frame; None before its first.
    pub fn shown(response: &Response) -> Option<Shown> {
        shown(&response.ctx, response.id)
    }
}

/// What the graph with id `id` drew last.
pub(crate) fn shown(ctx: &egui::Context, id: egui::Id) -> Option<Shown> {
    ctx.data(|d| d.get_temp::<Memory>(id)).and_then(|m| m.shown)
}
