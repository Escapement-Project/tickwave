//! What a host reads of the graph and the viewer, and what it can ask of them (round 3, for
//! verge-gui): the window and value range drawn, whether a query is under way.

use std::time::Duration;

use egui::pos2;

use super::*;
use crate::{Window, Y};

fn at(window: std::ops::Range<i64>) -> ViewState {
    ViewState {
        window: Window::At(window),
        ..ViewState::default()
    }
}

#[test]
fn the_host_reads_the_window_and_value_range_drawn_and_fixes_y_where_it_is() {
    let (mut h, _gate) = showing(fixture().write(), at(2 * SECOND..4 * SECOND), true);
    wait(&mut h);
    h.step();
    let read = h.state().shown.clone().expect("a frame drawn");
    let own = drawn(&h);
    assert_eq!((read.window.clone(), read.y), (own.window.clone(), own.y));
    assert_eq!(read.window, 2 * SECOND..4 * SECOND);
    assert_eq!(h.state().state.y, Y::Fit);
    // Fit off in place: the range drawn, fixed, draws the same.
    h.state_mut().state.y = Y::Fixed {
        low: read.y.0,
        high: read.y.1,
    };
    // A window with no pulse in it, which fits otherwise: fixed, it keeps the range read.
    let quiet = 2 * SECOND + 100 * MS..2 * SECOND + 500 * MS;
    h.state_mut().state.window = Window::At(quiet.clone());
    wait(&mut h);
    h.step();
    let later = h.state().shown.clone().unwrap();
    assert_eq!(later.window, quiet);
    assert_eq!(later.y, read.y);
    h.state_mut().state.y = Y::Fit;
    h.step();
    h.step();
    assert_ne!(h.state().shown.clone().unwrap().y, read.y);
}

#[test]
fn busy_while_a_query_waits_or_runs_and_not_once_its_answer_lands() {
    let (mut h, gate) = showing(fixture().write(), at(2 * SECOND..4 * SECOND), true);
    let _opens = Opens(Arc::clone(&gate));
    gate.close();
    h.step();
    assert!(viewer(&h).busy());
    gate.open();
    wait(&mut h);
    for _ in 0..100 {
        if !viewer(&h).busy() {
            break;
        }
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!viewer(&h).busy());
}

/// Moves the pointer to `pos` and holds it there past the tooltip's delay.
fn hover(h: &mut Harness<'_, Host>, pos: Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    for _ in 0..60 {
        h.step();
    }
}

/// The texts painted that hold `part`.
fn said(h: &Harness<'_, Host>, part: &str) -> Vec<String> {
    texts(h)
        .into_iter()
        .map(|(t, _)| t)
        .filter(|t| t.contains(part))
        .collect()
}

#[test]
fn a_marker_under_the_pointer_says_its_time_and_label() {
    let mut cap = fixture();
    cap.markers = vec![(5 * SECOND + 250 * MS, 7, "seven".into())];
    let (mut h, _gate) = showing(cap.write(), at(4 * SECOND..6 * SECOND), true);
    wait(&mut h);
    h.step();
    let l = drawn(&h).layout;
    let s = drawn(&h).window;
    let x = l.plot.left()
        + ((5 * SECOND + 250 * MS - s.start) as f32 / (s.end - s.start) as f32) * l.plot.width();
    // Over the plot, a point from its line: its time at the recording's tick, then its label.
    hover(&mut h, pos2(x + 1.0, l.plot.center().y));
    let reading = "5.250000\u{a0}s · 7 seven";
    assert_eq!(said(&h, " · "), [reading]);
    // Over a lane too.
    hover(&mut h, pos2(x - 2.0, l.lanes[1].center().y));
    assert_eq!(said(&h, " · "), [reading]);
    // Further off, nothing.
    hover(&mut h, pos2(x + 10.0, l.plot.center().y));
    assert!(said(&h, " · ").is_empty(), "{:?}", said(&h, " · "));
    // A drag from the line pans, as from anywhere on the plot.
    drag(
        &mut h,
        pos2(x, l.plot.center().y),
        egui::vec2(-100.0, 0.0),
        4,
    );
    assert_ne!(h.state().state.window, Window::At(4 * SECOND..6 * SECOND));
}
