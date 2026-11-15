//! The strip under the graph: verge's overview (bench.rs 1488-1560 at verge 08108de) as
//! behaviour. verge's tests have none of their own for it.

use std::ops::Range;
use std::sync::Arc;

use egui::{Pos2, Rect, vec2};

use super::*;
use crate::Window;

/// A harness with a strip under the graph, showing `file` through a gate.
fn with_strip(file: Vec<u8>, state: ViewState) -> (Harness<'static, Host>, Arc<Gate>) {
    let gate = Arc::new(Gate::default());
    let mut h = harness(state);
    h.state_mut().strip = true;
    h.state_mut().viewer = Some(gated(&h.ctx, file, &gate, true));
    (h, gate)
}

/// The strip's band the trace draws in, above its time axis.
fn strip(h: &Harness<'_, Host>) -> Rect {
    shown_strip(h).1
}

/// The whole strip, and its trace's band.
fn shown_strip(h: &Harness<'_, Host>) -> (Rect, Rect) {
    crate::strip::shown(&h.ctx, h.state().strip_id.expect("a strip")).expect("a strip laid out")
}

/// Where tick `t` sits across the strip, which spans `extent`.
fn x_at(h: &Harness<'_, Host>, extent: &Range<i64>, t: i64) -> f32 {
    let r = strip(h);
    r.left() + ((t - extent.start) as f64 / (extent.end - extent.start) as f64) as f32 * r.width()
}

/// The ticks a point of the strip spans.
fn per(h: &Harness<'_, Host>, extent: &Range<i64>) -> i64 {
    ((extent.end - extent.start) as f64 / f64::from(strip(h).width())).ceil() as i64
}

fn at(window: Range<i64>) -> ViewState {
    ViewState {
        window: Window::At(window),
        ..ViewState::default()
    }
}

fn window(h: &Harness<'_, Host>) -> Range<i64> {
    match &h.state().state.window {
        Window::At(w) => w.clone(),
        other => panic!("a window of its own, not {other:?}"),
    }
}

fn click(h: &mut Harness<'_, Host>, p: Pos2) {
    h.event(egui::Event::PointerMoved(p));
    h.step();
    button(h, p, true);
    button(h, p, false);
    h.step();
}

#[test]
fn a_click_on_the_strip_centres_the_window_under_it() {
    let (mut h, _gate) = with_strip(fixture().write(), at(2 * SECOND..4 * SECOND));
    let extent = answered(&mut h, 2 * SECOND..4 * SECOND)
        .extent
        .clone()
        .unwrap();
    assert!(!h.state().strip_changed);
    let p = Pos2::new(x_at(&h, &extent, 12 * SECOND), strip(&h).center().y);
    click(&mut h, p);
    let w = window(&h);
    assert_eq!(w.end - w.start, 2 * SECOND);
    assert!((w.start - 11 * SECOND).abs() <= per(&h, &extent), "{w:?}");
    assert!(h.state().strip_changed);
    // The graph asks for it.
    answered(&mut h, w);
}

#[test]
fn a_drag_along_the_strip_keeps_the_window_centred_under_the_pointer() {
    let (mut h, _gate) = with_strip(fixture().write(), at(2 * SECOND..4 * SECOND));
    let extent = answered(&mut h, 2 * SECOND..4 * SECOND)
        .extent
        .clone()
        .unwrap();
    let y = strip(&h).center().y;
    let from = Pos2::new(x_at(&h, &extent, 12 * SECOND), y);
    let to = x_at(&h, &extent, 15 * SECOND);
    drag(&mut h, from, vec2(to - from.x, 0.0), 6);
    h.step();
    let w = window(&h);
    assert_eq!(w.end - w.start, 2 * SECOND);
    assert!((w.start - 14 * SECOND).abs() <= per(&h, &extent), "{w:?}");
}

#[test]
fn centring_near_the_start_keeps_a_followed_window_inside_the_extent() {
    let state = ViewState {
        window: Window::Follow { span: 2 * SECOND },
        ..ViewState::default()
    };
    let (mut h, _gate) = with_strip(fixture().write(), state);
    let extent = answered(&mut h, 18 * SECOND..20 * SECOND)
        .extent
        .clone()
        .unwrap();
    let p = Pos2::new(x_at(&h, &extent, SECOND / 5), strip(&h).center().y);
    click(&mut h, p);
    assert_eq!(window(&h), 0..2 * SECOND);
}

#[test]
fn the_start_handle_drags_the_start_alone() {
    let (mut h, _gate) = with_strip(fixture().write(), at(2 * SECOND..4 * SECOND));
    let extent = answered(&mut h, 2 * SECOND..4 * SECOND)
        .extent
        .clone()
        .unwrap();
    let y = strip(&h).center().y;
    let from = Pos2::new(x_at(&h, &extent, 2 * SECOND), y);
    let to = x_at(&h, &extent, SECOND);
    drag(&mut h, from, vec2(to - from.x, 0.0), 4);
    h.step();
    let w = window(&h);
    assert_eq!(w.end, 4 * SECOND);
    assert!((w.start - SECOND).abs() <= per(&h, &extent), "{w:?}");
    assert!(h.state().strip_changed);
}

#[test]
fn the_end_handle_drags_the_end_alone() {
    let (mut h, _gate) = with_strip(fixture().write(), at(2 * SECOND..4 * SECOND));
    let extent = answered(&mut h, 2 * SECOND..4 * SECOND)
        .extent
        .clone()
        .unwrap();
    let y = strip(&h).center().y;
    let from = Pos2::new(x_at(&h, &extent, 4 * SECOND), y);
    let to = x_at(&h, &extent, 6 * SECOND);
    drag(&mut h, from, vec2(to - from.x, 0.0), 4);
    h.step();
    let w = window(&h);
    assert_eq!(w.start, 2 * SECOND);
    assert!((w.end - 6 * SECOND).abs() <= per(&h, &extent), "{w:?}");
}

#[test]
fn a_handle_stops_a_tick_short_of_the_other_end() {
    let (mut h, _gate) = with_strip(fixture().write(), at(2 * SECOND..4 * SECOND));
    let extent = answered(&mut h, 2 * SECOND..4 * SECOND)
        .extent
        .clone()
        .unwrap();
    let y = strip(&h).center().y;
    // The start dragged past the end.
    let from = Pos2::new(x_at(&h, &extent, 2 * SECOND), y);
    let to = x_at(&h, &extent, 10 * SECOND);
    drag(&mut h, from, vec2(to - from.x, 0.0), 4);
    h.step();
    assert_eq!(window(&h), 4 * SECOND - 1..4 * SECOND);
    // The end dragged past the start.
    h.state_mut().state.window = Window::At(2 * SECOND..4 * SECOND);
    h.step();
    let from = Pos2::new(x_at(&h, &extent, 4 * SECOND), y);
    let to = x_at(&h, &extent, SECOND / 2);
    drag(&mut h, from, vec2(to - from.x, 0.0), 4);
    h.step();
    assert_eq!(window(&h), 2 * SECOND..2 * SECOND + 1);
}

#[test]
fn a_strip_before_any_answer_draws_its_ground_and_moves_nothing() {
    let gate = Arc::new(Gate::default());
    let mut h = harness(at(2 * SECOND..4 * SECOND));
    h.state_mut().strip = true;
    h.state_mut().viewer = Some(gated(&h.ctx, fixture().write(), &gate, false));
    // Nothing to draw: no sketch either.
    viewer(&h).no_sketch();
    // After the harness, so it opens the gate before the viewer waits for its thread.
    let _opens = Opens(Arc::clone(&gate));
    // Opened, and held back from its first read.
    gate.close();
    for _ in 0..5 {
        h.step();
    }
    assert!(viewer(&h).answer().is_none());
    let (rect, _) = shown_strip(&h);
    assert_eq!(rect.height(), STRIP);
    let p = palette();
    assert_eq!(rects_of(&h, rect, p.surface), vec![rect]);
    assert!(rects_of(&h, rect, p.wash).is_empty());
    click(&mut h, rect.center());
    assert_eq!(window(&h), 2 * SECOND..4 * SECOND);
    assert!(!h.state().strip_changed);
}

#[test]
fn the_strip_draws_the_graph_answer_across_its_own_width_and_asks_nothing() {
    let (mut h, _gate) = with_strip(fixture().write(), at(2 * SECOND..4 * SECOND));
    let a = answered(&mut h, 2 * SECOND..4 * SECOND);
    let extent = a.extent.clone().unwrap();
    // The strip's columns are the graph's plot's, over the whole extent.
    let plot = drawn(&h).layout.plot;
    let strip_answer = a.strip.as_ref().unwrap();
    assert_eq!(strip_answer.grid.count, plot.width().round() as usize);
    let rect = strip(&h);
    let p = palette();
    let fill = p.trace.gamma_multiply(p.fill);
    let spans = spans_of(&h, rect, fill);
    let (a0, b0) = (
        x_at(&h, &extent, extent.start),
        x_at(&h, &extent, extent.end),
    );
    let blank: Vec<f32> = (rect.left() as i32..rect.right() as i32)
        .map(|x| x as f32 + 0.5)
        .filter(|&x| x > a0 && x < b0)
        .filter(|&x| !spans.iter().any(|&(a, b)| a <= x && x <= b))
        .collect();
    assert!(blank.is_empty(), "blank at {blank:?}");
    let wash = rects_of(&h, rect, p.wash);
    assert_eq!(wash.len(), 1);
    let (x2, x4) = (x_at(&h, &extent, 2 * SECOND), x_at(&h, &extent, 4 * SECOND));
    assert!((wash[0].left() - x2).abs() < 0.5 && (wash[0].right() - x4).abs() < 0.5);
    // A handle's line at each end.
    let ends: Vec<f32> = shapes(&h)
        .iter()
        .filter(|s| s.clip_rect == rect)
        .filter_map(|s| match &s.shape {
            egui::Shape::LineSegment { points, stroke } if stroke.color == p.ink => {
                Some(points[0].x)
            }
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 2);
    assert!(
        (ends[0] - x2).abs() < 0.5 && (ends[1] - x4).abs() < 0.5,
        "{ends:?}"
    );
    // Frames go by and the strip adds no question of its own.
    let issued = viewer(&h).issued();
    for _ in 0..10 {
        h.step();
    }
    assert_eq!(viewer(&h).issued(), issued);
}

#[test]
fn a_loss_far_narrower_than_a_strip_column_stays_visible() {
    // One pair lost at 10 s: a millisecond, where a column of the strip is about 20.
    let mut cap = fixture();
    cap.lost = std::iter::once(10_000..10_001).collect();
    let (mut h, _gate) = with_strip(cap.write(), ViewState::default());
    let extent = wait(&mut h).extent.clone().unwrap();
    let rect = strip(&h);
    let p = palette();
    let shade = p.attention_fill.gamma_multiply(p.shade);
    let bands = rects_of(&h, rect, shade);
    assert_eq!(bands.len(), 1, "{bands:?}");
    let px = 1.0 / h.ctx.pixels_per_point();
    assert!(bands[0].width() >= px, "{:?}", bands[0]);
    let x = x_at(&h, &extent, 10 * SECOND);
    assert!(
        bands[0].left() <= x + px && bands[0].right() >= x,
        "{:?} at {x}",
        bands[0]
    );
}
