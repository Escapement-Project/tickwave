//! The strip: the whole recording small, under the graph, with the window on screen washed over
//! it and its ends draggable.

use std::ops::Range;

use egui::{
    AsIdSalt, CursorIcon, IdSalt, PointerButton, Rect, Response, Sense, Stroke, Ui, Widget, pos2,
    vec2,
};

use crate::draw;
use crate::graph::View;
use crate::palette::Palette;
use crate::state::{ViewState, Window};
use crate::viewer::Viewer;

/// The strip's height when the host gives none, in points.
const HEIGHT: f32 = 64.0;
/// A handle's width, in points, centred on the window's end it drags.
const HANDLE: f32 = 10.0;

/// The trace over the recording's whole extent, from the strip the graph's answer holds,
/// stretched to the strip's own width; it asks for nothing itself. A click or a drag centres the
/// window under the pointer; a handle at each end of the window drags that end, at least a tick
/// short of the other. The response is `changed()` when the window moved.
pub struct Strip<'a> {
    viewer: &'a Viewer,
    state: &'a mut ViewState,
    palette: &'a Palette,
    include_zero: bool,
    height: f32,
    id_salt: IdSalt,
}

impl<'a> Strip<'a> {
    pub fn new(viewer: &'a Viewer, state: &'a mut ViewState, palette: &'a Palette) -> Self {
        Self {
            viewer,
            state,
            palette,
            include_zero: false,
            height: HEIGHT,
            id_salt: IdSalt::new("tickwave strip"),
        }
    }

    /// The trace's range reaches zero.
    pub fn include_zero(mut self, include: bool) -> Self {
        self.include_zero = include;
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    /// Tells two strips in one `Ui` apart.
    pub fn id_salt(mut self, salt: impl AsIdSalt) -> Self {
        self.id_salt = IdSalt::new(salt);
        self
    }
}

/// The tick at `x` across the strip, within the extent.
fn tick_at(view: &View, extent: &Range<i64>, x: f32) -> i64 {
    (view.tick(x).round() as i64).clamp(extent.start, extent.end)
}

impl Widget for Strip<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let _span = tracing::debug_span!("strip").entered();
        let id = ui.make_persistent_id(self.id_salt);
        let p = self.palette;
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), self.height), Sense::hover());
        let mut response = ui.interact(rect, id, Sense::click_and_drag());
        ui.painter()
            .with_clip_rect(rect)
            .rect_filled(rect, 0.0, p.surface);
        #[cfg(test)]
        ui.data_mut(|d| d.insert_temp(id, rect));
        let held = self.viewer.held();
        let Some(answer) = held.as_ref().and_then(|h| h.answer.strip.as_ref()) else {
            return response;
        };
        let extent = answer.extent.clone();
        let view = View::new(extent.clone(), rect);
        let before = self.state.window.clone();
        let window = self.state.window.resolve(Some(&extent));
        let handle =
            |x: f32| Rect::from_center_size(pos2(x, rect.center().y), vec2(HANDLE, rect.height()));
        let ends = [(true, window.start), (false, window.end)].map(|(start, t)| {
            let response = ui
                .interact(
                    handle(view.x(t as f64)),
                    id.with(("handle", start)),
                    Sense::drag(),
                )
                .on_hover_and_drag_cursor(CursorIcon::ResizeHorizontal);
            (start, response)
        });
        if (response.clicked() || response.dragged_by(PointerButton::Primary))
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let width = window.end - window.start;
            let start = (tick_at(&view, &extent, pointer.x) - width / 2).max(extent.start);
            self.state.window = Window::At(start..start.saturating_add(width));
        }
        for (start, handle) in &ends {
            if handle.dragged_by(PointerButton::Primary)
                && let Some(pointer) = handle.interact_pointer_pos()
            {
                let t = tick_at(&view, &extent, pointer.x);
                self.state.window = Window::At(if *start {
                    t.min(window.end - 1)..window.end
                } else {
                    window.start..t.max(window.start + 1)
                });
            }
        }
        let window = self.state.window.resolve(Some(&extent));
        draw::strip(ui, rect, answer, &window, self.include_zero, p);
        let painter = ui.painter().with_clip_rect(rect);
        for t in [window.start, window.end] {
            let x = view.x(t as f64);
            painter.line_segment(
                [pos2(x, rect.top()), pos2(x, rect.bottom())],
                Stroke::new(2.0, p.ink),
            );
        }
        if self.state.window != before {
            response.mark_changed();
        }
        response
    }
}

/// Where the strip with id `id` was laid out last.
#[cfg(test)]
pub(crate) fn shown(ctx: &egui::Context, id: egui::Id) -> Option<Rect> {
    ctx.data(|d| d.get_temp::<Rect>(id))
}
