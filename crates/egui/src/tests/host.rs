//! What a host reads of the graph and the viewer, and what it can ask of them (round 3, for
//! verge-gui): the window and value range drawn, whether a query is under way.

use std::time::Duration;

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
