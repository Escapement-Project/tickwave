//! The graph: one analog trace, DIO lanes under it on the same time axis, markers, the selection,
//! and the gestures that move them.

use std::ops::Range;

use egui::{
    AsIdSalt, CursorIcon, IdSalt, PointerButton, Rect, Response, Sense, Ui, Vec2, Widget, pos2,
    vec2,
};
use escapement_tickwave_view::readouts::{MINUS, NBSP};
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
const MIN_TRACE: f32 = 40.0;

/// What a host adds to the graph's query for its own panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extras {
    /// A page of the list of every marker: from after a cursor, at most so many.
    pub marker_list: Option<(Option<MarkerCursor>, usize)>,
    /// The marker after this one, wherever it starts.
    pub successor_of: Option<MarkerCursor>,
}

/// An analog or derived channel's trace, with digital channels in lanes under it. It queries
/// through its `Viewer` and draws the latest answer, mapped to the window on screen while a
/// newer one is on the way.
///
/// Drag or scroll to move along time, ctrl-scroll or pinch to zoom it. A vertical drag or
/// scroll past a 12-point detent takes Y off the fit and moves it, a drag on the value axis
/// zooms it, and a double click fits it again. With `select_mode` a drag selects. The response
/// is `changed()` when the window or the selection moved.
pub struct Graph<'a> {
    viewer: &'a Viewer,
    state: &'a mut ViewState,
    palette: &'a Palette,
    trace: usize,
    flag: Option<usize>,
    lanes: &'a [usize],
    include_zero: bool,
    extras: Extras,
    height: Option<f32>,
    now: Option<i64>,
    id_salt: IdSalt,
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
            trace,
            flag: None,
            lanes: &[],
            include_zero: false,
            extras: Extras::default(),
            height: None,
            now: None,
            id_salt: IdSalt::new("tickwave graph"),
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

/// What the graph drew last.
#[derive(Clone, Debug)]
pub(crate) struct Shown {
    /// Kept while nothing is in view to fit.
    pub y: (f64, f64),
    #[cfg(test)]
    pub window: Range<i64>,
    #[cfg(test)]
    pub layout: Layout,
}

#[derive(Clone, Debug, Default)]
struct Memory {
    /// Vertical travel while Y fits, toward the detent.
    travel: f32,
    /// A pan's part of a tick not yet moved.
    pan: f64,
    /// Where a selection drag started.
    anchor: Option<i64>,
    /// The viewer's snapshots when the graph last asked.
    asked_snapshots: u64,
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
        let unit = facts
            .channels
            .get(self.trace)
            .map_or("", |c| c.unit.as_str());
        let measure = |text: String, font: &egui::FontId| {
            ui.painter()
                .layout_no_wrap(text, font.clone(), p.ink_2)
                .size()
                .x
        };
        // As wide as the widest value label a unit can take, so the plot stays where it is as
        // the labels change.
        let mut gutter = measure(format!("{MINUS}888.888{NBSP}M{unit}"), &p.figures);
        for &c in self.lanes {
            if let Some(info) = facts.channels.get(c) {
                gutter = gutter.max(measure(info.name.clone(), &p.labels));
            }
        }
        let gutter = (gutter + 12.0).ceil().max(40.0);
        let label_row = (ui.fonts_mut(|f| f.row_height(&p.labels)) + 2.0).ceil();
        let lanes = self.lanes.len() as f32 * LANE_HEIGHT;
        let height = self
            .height
            .unwrap_or_else(|| ui.available_height())
            .max(MIN_TRACE + lanes + AXIS_HEIGHT);
        let width = ui.available_width().max(gutter + 40.0);
        let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let plot = Rect::from_min_max(
            pos2(rect.left() + gutter, rect.top()),
            pos2(rect.right(), rect.bottom() - lanes - AXIS_HEIGHT),
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
                pos2(plot.left(), rect.bottom() - AXIS_HEIGHT),
                rect.right_bottom(),
            ),
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
        let rows = if facts.marker_count > 0 {
            LABEL_ROWS
        } else {
            0
        };
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
        let resolution = match facts.channels.get(self.trace).and_then(|c| c.resolution) {
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

    fn zoom(&mut self, view: &View, at: f32, factor: f32) {
        let w = &view.window;
        let width = (w.end - w.start) as f64;
        let min = (f64::from(view.width) / MAX_POINTS_PER_TICK)
            .ceil()
            .max(1.0);
        let new = (width / f64::from(factor)).clamp(min, MAX_TICKS).round();
        let tick = view.tick(at);
        let start = (tick - (tick - w.start as f64) / width * new).round();
        self.moved(start as i64..(start + new) as i64);
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
        view: &View,
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
                self.zoom(view, p.x, factor);
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
        if delta.x != 0.0 {
            self.pan(view, delta.x, memory);
        }
        if response.double_clicked() {
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

    fn request(&self, columns: u32) -> Request {
        let mut channels = vec![self.trace];
        channels.extend_from_slice(self.lanes);
        let following = matches!(self.state.window, Window::Follow { .. });
        Request {
            window: self.state.window.clone(),
            // A window of its own doesn't move with now, so a moving now asks nothing.
            now: self.now.filter(|_| following),
            columns,
            channels,
            flags: self.flag.map(|f| (self.trace, f)).into_iter().collect(),
            strip: Some((self.trace, columns)),
            selection: self.state.selection.clone(),
            markers: MarkerQuery {
                limit: MARKER_LIMIT,
                list: self.extras.marker_list,
                successor_of: self.extras.successor_of,
            },
        }
    }

    /// Asks for `wanted` when it differs from what was asked, now aside. A followed window also
    /// asks when the recording has taken a snapshot since, or when the window on screen has left
    /// what the held answer covers; a moving now alone asks nothing, since the held answer covers
    /// a width either side and is drawn moved with it. The request carries the latest now, so the
    /// query ends its window there. A change of the channels drawn asks at once; anything else
    /// waits for the query under way, so a pan or a followed recording draws its answers as they
    /// come rather than cancelling each one.
    fn ask(
        &self,
        wanted: Request,
        window: &Range<i64>,
        held: Option<&Held>,
        facts: &Facts,
        memory: &mut Memory,
    ) {
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
        let snapshots = self.viewer.snapshots();
        let grown = facts.live && following && snapshots != memory.asked_snapshots;
        let left = following
            && held.is_some_and(|h| {
                window.start < h.answer.around.start || window.end > h.answer.around.end
            });
        if structural || (!self.viewer.busy() && (changed || grown || left)) {
            self.viewer.request(wanted);
            memory.asked_snapshots = snapshots;
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
        let value_axis = ui
            .interact(layout.gutter, id.with("value axis"), Sense::drag())
            .on_hover_and_drag_cursor(CursorIcon::ResizeVertical);
        let mut memory: Memory = ui.data(|d| d.get_temp(id)).unwrap_or_default();
        let before = (self.state.window.clone(), self.state.selection.clone());
        let extent = held.as_ref().and_then(|h| h.answer.extent.clone());
        let trace = held.as_ref().and_then(|h| Trace::of(h, self.trace));

        let view = View::new(
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
            &view,
            y,
            fit,
            &facts,
            &mut memory,
        );

        let view = View::new(
            self.state.window.resolve(extent.as_ref(), self.now),
            layout.plot,
        );
        let (y, _) = self.y(
            trace.as_ref(),
            &view,
            &layout,
            &facts,
            memory.shown.as_ref(),
        );
        {
            let _span = tracing::debug_span!("build query").entered();
            let columns = (layout.plot.width() * ui.pixels_per_point())
                .round()
                .max(1.0) as u32;
            let wanted = self.request(columns);
            self.ask(wanted, &view.window, held.as_ref(), &facts, &mut memory);
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
            },
        );
        memory.shown = Some(Shown {
            y,
            #[cfg(test)]
            window: view.window.clone(),
            #[cfg(test)]
            layout,
        });
        ui.data_mut(|d| d.insert_temp(id, memory));
        // Following a recording as it is written, the window moves every frame.
        if facts.live && matches!(self.state.window, Window::Follow { .. }) {
            ui.request_repaint();
        }
        if (self.state.window.clone(), self.state.selection.clone()) != before {
            response.mark_changed();
        }
        response
    }
}

/// What the graph with id `id` drew last.
#[cfg(test)]
pub(crate) fn shown(ctx: &egui::Context, id: egui::Id) -> Option<Shown> {
    ctx.data(|d| d.get_temp::<Memory>(id)).and_then(|m| m.shown)
}
