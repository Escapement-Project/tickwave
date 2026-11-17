//! What a graph draws past the answer's `around` while a newer answer is on its way (round 3):
//! each lane from its columns over the extent, as the trace from the strip's.

use std::time::Duration;

use egui::{Color32, Shape};

use super::*;
use crate::Window;

fn at(window: std::ops::Range<i64>) -> ViewState {
    ViewState {
        window: Window::At(window),
        ..ViewState::default()
    }
}

/// The trace's colour at any opacity: a lane's levels, edges and shades.
fn traced(c: Color32) -> bool {
    c.a() > 0 && u16::from(c.r()) > 2 * u16::from(c.g().max(c.b()))
}

/// What shows of the trace's colour in `lane`: each triangle's corners of it, cut to its clip.
fn ink_in(h: &Harness<'_, Host>, lane: Rect) -> Vec<Rect> {
    let mut out = Vec::new();
    for s in shapes(h) {
        let Shape::Mesh(mesh) = &s.shape else {
            continue;
        };
        for t in mesh.indices.as_chunks::<3>().0 {
            let corners: Vec<Pos2> = t
                .iter()
                .map(|&i| mesh.vertices[i as usize])
                .filter(|v| traced(v.color))
                .map(|v| v.pos)
                .collect();
            if corners.is_empty() {
                continue;
            }
            let r = Rect::from_points(&corners).intersect(s.clip_rect);
            if r.min.x <= r.max.x && r.min.y <= r.max.y && lane.contains(r.center()) {
                out.push(r);
            }
        }
    }
    out
}

/// The middle of each device pixel across the plot that no ink of `lane` reaches.
fn lane_blank(h: &Harness<'_, Host>, lane: Rect) -> Vec<f32> {
    let ink = ink_in(h, lane);
    let ppp = h.ctx.pixels_per_point();
    let pixels = (lane.width() * ppp).round() as i32;
    (0..pixels)
        .map(|k| lane.left() + (k as f32 + 0.5) / ppp)
        .filter(|&x| {
            !ink.iter()
                .any(|r| r.left() - 0.5 <= x && x <= r.right() + 0.5)
        })
        .collect()
}

/// Where tick `t` sits across the plot as last drawn.
fn x_of(h: &Harness<'_, Host>, t: i64) -> f32 {
    let s = drawn(h);
    let (plot, w) = (s.layout.plot, s.window);
    plot.left() + ((t - w.start) as f64 / (w.end - w.start) as f64) as f32 * plot.width()
}

#[test]
fn no_lane_column_goes_blank_while_an_answer_is_pending_and_past_around_it_draws_its_levels() {
    // DIO3 high while the current's pulse is: 30 ms at each second.
    let mut cap = Capture::new(1000, 20_000, noisy);
    cap.dio = true;
    // `around` is 6 s to 12 s.
    let window = 8 * SECOND..10 * SECOND;
    let (mut h, gate) = showing(cap.write(), at(window.clone()), false);
    let _opens = Opens(Arc::clone(&gate));
    let a = answered(&mut h, window);
    assert_eq!(a.around, 6 * SECOND..12 * SECOND);
    assert_eq!(a.fallbacks.len(), LANES.len());
    let lane = drawn(&h).layout.lanes[3];
    assert!(lane_blank(&h, lane).is_empty());
    gate.close();
    let check = |h: &mut Harness<'_, Host>, what: &str| {
        for _ in 0..5 {
            h.step();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(viewer(h).busy(), "{what}: a query held back");
        assert!(
            Arc::ptr_eq(&viewer(h).answer().unwrap(), &a),
            "{what}: the answer held"
        );
        for (k, &lane) in drawn(h).layout.lanes.iter().enumerate() {
            let blank = lane_blank(h, lane);
            assert!(
                blank.is_empty(),
                "{what}: lane {k} blank at {:?}",
                &blank[..blank.len().min(6)]
            );
        }
    };
    // Past `around`, after it and before it, and over it and either side.
    h.state_mut().state.window = Window::At(14 * SECOND..16 * SECOND);
    check(&mut h, "after around");
    // The pulses at 14 s and 15 s, high, from the columns over the extent.
    let high = lane.top() + 3.0;
    for s in [14, 15] {
        let x = x_of(&h, s * SECOND);
        let up = ink_in(&h, lane)
            .iter()
            .any(|r| (r.top() - high).abs() < 1.0 && r.right() >= x - 2.0 && r.left() <= x + 25.0);
        assert!(up, "no high level at {s} s");
    }
    h.state_mut().state.window = Window::At(2 * SECOND..4 * SECOND);
    check(&mut h, "before around");
    h.state_mut().state.window = Window::At(0..20 * SECOND);
    check(&mut h, "over around");
    gate.open();
    let landed = answered(&mut h, 0..20 * SECOND);
    assert!(!Arc::ptr_eq(&landed, &a));
    assert!(lane_blank(&h, drawn(&h).layout.lanes[3]).is_empty());
}
