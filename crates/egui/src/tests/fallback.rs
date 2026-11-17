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

/// verge's fixture with DIO3 high through each pulse, its answer for 8 s to 10 s held (`around`
/// 6 s to 12 s), then shown at 5.5 s to 6.5 s while the next answer is held back: the strip's and
/// the lanes' columns before 6 s, the answer's after. The columns over the extent are about 20.4
/// ms wide (20 s in the plot's pixels); the one `around` cuts runs from 5.9857 s to 6.0061 s, and
/// its part before 6 s holds no pulse, which starts at 6 s.
fn cut_at_six(dio: bool) -> (Harness<'static, Host>, Arc<Gate>) {
    let mut cap = fixture();
    cap.dio = dio;
    let (mut h, gate) = showing(cap.write(), at(8 * SECOND..10 * SECOND), false);
    let a = answered(&mut h, 8 * SECOND..10 * SECOND);
    assert_eq!(a.around, 6 * SECOND..12 * SECOND);
    gate.close();
    h.state_mut().state.window = Window::At(5 * SECOND + 500 * MS..6 * SECOND + 500 * MS);
    for _ in 0..5 {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(viewer(&h).busy());
    assert!(Arc::ptr_eq(&viewer(&h).answer().unwrap(), &a));
    (h, gate)
}

#[test]
fn a_strip_column_around_cuts_draws_the_levels_of_its_part_outside_around() {
    let (h, gate) = cut_at_six(false);
    let _opens = Opens(gate);
    let s = viewer(&h).answer().unwrap();
    let cut = &s.strip.as_ref().unwrap().cut;
    assert_eq!(cut[0].span.end, 6 * SECOND);
    assert!(
        cut[0].span.start > 5 * SECOND + 980 * MS,
        "{:?}",
        cut[0].span
    );
    let plot = drawn(&h).layout.plot;
    let top = |from: i64, to: i64| {
        let (a, b) = (x_of(&h, from), x_of(&h, to));
        ink_in(&h, plot)
            .iter()
            .filter(|r| r.right() >= a && r.left() <= b)
            .map(|r| r.top())
            .fold(f32::INFINITY, f32::min)
    };
    // The quiet level, from the strip's columns well before the cut one.
    let quiet = top(5 * SECOND + 800 * MS, 5 * SECOND + 900 * MS);
    let pulse = top(6 * SECOND, 6 * SECOND + 30 * MS);
    assert!(
        pulse < quiet - 100.0,
        "the pulse at {pulse}, the quiet level at {quiet}"
    );
    // The cut column's part before 6 s: quiet, not the pulse its whole column holds.
    let cut = top(cut[0].span.start + MS, 6 * SECOND - MS);
    assert!(
        (cut - quiet).abs() < 2.0,
        "at {cut}, the quiet level at {quiet}"
    );
}

#[test]
fn a_lane_column_around_cuts_draws_the_levels_of_its_part_outside_around() {
    let (h, gate) = cut_at_six(true);
    let _opens = Opens(gate);
    let lane = drawn(&h).layout.lanes[3];
    let high = lane.top() + 3.0;
    let six = x_of(&h, 6 * SECOND);
    let highs: Vec<Rect> = (ink_in(&h, lane).into_iter())
        .filter(|r| (r.top() - high).abs() < 1.0)
        .collect();
    // High from the rise at 6 s, and nowhere before it.
    assert!(highs.iter().any(|r| r.right() > six + 5.0), "{highs:?}");
    let early: Vec<&Rect> = highs.iter().filter(|r| r.left() < six - 1.5).collect();
    assert!(
        early.is_empty(),
        "high before 6 s at {early:?}, 6 s at {six}"
    );
}
