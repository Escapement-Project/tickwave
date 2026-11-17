//! A zoom step eased: the host's window set at once, the window drawn easing there, steps and
//! pans during an ease, and what draws while the step's answer is pending.

use std::sync::Arc;
use std::time::Duration;

use egui::vec2;

use super::*;
use crate::Window;

fn at(window: Range<i64>) -> ViewState {
    ViewState {
        window: Window::At(window),
        ..ViewState::default()
    }
}

/// A finished capture shown over `window`, the ease on, its answer landed.
fn eased(window: Range<i64>) -> (Harness<'static, Host>, Arc<Gate>) {
    let (mut h, gate) = showing(fixture().write(), at(window.clone()), true);
    h.state_mut().motion = true;
    answered(&mut h, window);
    (h, gate)
}

/// A zoom step by `factor` about the plot's middle, in one frame.
fn zoom(h: &mut Harness<'_, Host>, factor: f32) {
    let p = plot(h).center();
    h.event(egui::Event::PointerMoved(p));
    h.step();
    h.event(egui::Event::Zoom(factor));
    h.step();
}

fn target(h: &Harness<'_, Host>) -> Range<i64> {
    let Window::At(w) = h.state().state.window.clone() else {
        panic!("a window of its own");
    };
    w
}

fn width(w: &Range<i64>) -> i64 {
    w.end - w.start
}

/// Whether the last frame asked for the next at once.
fn asks_again(h: &Harness<'_, Host>) -> bool {
    h.output().viewport_output[&egui::ViewportId::ROOT].repaint_delay == Duration::ZERO
}

#[test]
fn a_zoom_step_sets_the_hosts_window_at_once_and_eases_the_window_drawn_there() {
    let before = 8 * SECOND..12 * SECOND;
    let (mut h, _gate) = eased(before.clone());
    h.state_mut().changed = false;
    zoom(&mut h, 2.0);
    // The host's window is where the view goes, its middle where it was, and the query asks for
    // it in the same frame.
    let to = target(&h);
    assert_eq!(to, 9 * SECOND..11 * SECOND);
    assert!(h.state().changed);
    assert_eq!(viewer(&h).sent().unwrap().window, Window::At(to.clone()));
    // The frame of the step draws where the view was, and asks for another frame.
    assert_eq!(drawn(&h).window, before);
    assert!(asks_again(&h));
    // Each frame narrower, about the same middle, quick at first, and asking for the next; there
    // by 150 ms.
    let mut widths = vec![width(&before)];
    let mut frames = 0;
    while drawn(&h).window != to {
        assert!(asks_again(&h), "frame {frames} asked for no next");
        h.state_mut().changed = false;
        h.step();
        frames += 1;
        let w = drawn(&h).window;
        assert!((w.start + w.end - 20 * SECOND).abs() <= 1, "{w:?}");
        assert!(
            !h.state().changed,
            "frame {frames}: the host's window moved"
        );
        widths.push(width(&w));
        assert!(frames <= 10, "still easing after {frames} frames: {w:?}");
    }
    assert!(frames >= 8, "{frames} frames: {widths:?}");
    assert!(widths.windows(2).all(|p| p[1] < p[0]), "{widths:?}");
    assert!(
        widths[0] - widths[1] > widths[frames - 1] - widths[frames],
        "{widths:?}"
    );
    // Landed: no more frames asked for.
    h.step();
    assert!(!asks_again(&h));
}

#[test]
fn a_step_during_an_ease_goes_on_from_where_the_last_was_going_and_starts_from_the_window_drawn() {
    let (mut h, _gate) = eased(8 * SECOND..12 * SECOND);
    zoom(&mut h, 2.0);
    h.step();
    h.step();
    let mid = drawn(&h).window;
    assert!(
        width(&mid) < 4 * SECOND && width(&mid) > 2 * SECOND,
        "{mid:?}"
    );
    zoom(&mut h, 2.0);
    // Two steps from where the first started: half of the first's target.
    assert_eq!(target(&h), 9_500 * MS..10_500 * MS);
    // The new ease starts where the window drawn was, not where the first was going.
    let w = drawn(&h).window;
    assert!(
        width(&w) <= width(&mid) && width(&w) > 2 * SECOND,
        "{w:?} after {mid:?}"
    );
    for _ in 0..12 {
        h.step();
    }
    assert_eq!(drawn(&h).window, 9_500 * MS..10_500 * MS);
}

#[test]
fn a_window_the_host_sets_during_an_ease_is_drawn_at_once() {
    let (mut h, _gate) = eased(8 * SECOND..12 * SECOND);
    zoom(&mut h, 2.0);
    h.step();
    h.state_mut().state.window = Window::At(2 * SECOND..3 * SECOND);
    h.step();
    assert_eq!(drawn(&h).window, 2 * SECOND..3 * SECOND);
}

#[test]
fn a_pan_during_an_ease_moves_on_from_where_it_was_going_and_jumps() {
    let (mut h, _gate) = eased(8 * SECOND..12 * SECOND);
    zoom(&mut h, 2.0);
    let to = target(&h);
    // Panned by 100 points of the target's 2 s across the plot.
    let by = (100.0 / plot(&h).width() * width(&to) as f32) as i64;
    let p = plot(&h).center();
    drag(&mut h, p, vec2(-100.0, 0.0), 1);
    let w = target(&h);
    assert!(
        (w.start - (to.start + by)).abs() <= 2 * MS,
        "{w:?} from {to:?}"
    );
    assert_eq!(width(&w), width(&to));
    assert_eq!(drawn(&h).window, w);
}

#[test]
fn with_motion_off_a_zoom_step_jumps() {
    let (mut h, _gate) = eased(8 * SECOND..12 * SECOND);
    h.state_mut().motion = false;
    zoom(&mut h, 2.0);
    assert_eq!(drawn(&h).window, 9 * SECOND..11 * SECOND);
}

#[test]
fn while_an_ease_runs_with_its_answer_pending_no_column_drawn_goes_blank() {
    let mut cap = Capture::new(1000, 20_000, noisy);
    cap.dio = true;
    let window = 8 * SECOND..10 * SECOND;
    let (mut h, gate) = showing(cap.write(), at(window.clone()), false);
    h.state_mut().motion = true;
    let a = answered(&mut h, window);
    let extent = a.extent.clone().unwrap();
    let _opens = Opens(Arc::clone(&gate));
    gate.close();
    let mut frames = 0;
    for factor in [0.5, 2.0, 0.5] {
        zoom(&mut h, factor);
        loop {
            assert!(
                Arc::ptr_eq(&viewer(&h).answer().unwrap(), &a),
                "the answer held"
            );
            let blank = super::graph::blank(&h, &extent);
            assert!(
                blank.is_empty(),
                "zoom {factor}, frame {frames}: blank at {blank:?}"
            );
            if drawn(&h).window == target(&h) {
                break;
            }
            h.step();
            std::thread::sleep(Duration::from_millis(1));
            frames += 1;
            assert!(frames < 60);
        }
    }
}
