//! Ports of verge-gui's graph tests (verge 08108de, gui/src/bench.rs from line 2137) as the
//! graph's behaviour, and the properties the brief adds.

use std::sync::Arc;
use std::time::Duration;

use egui::{Color32, Pos2, Shape, vec2};
use escapement_tickwave_view::{Answer, Drawn, MarkerCursor};

use super::*;
use crate::graph::{fitted, headroom};
use crate::{Window, Y};

fn at(window: std::ops::Range<i64>) -> ViewState {
    ViewState {
        window: Window::At(window),
        ..ViewState::default()
    }
}

fn fill(p: &Palette) -> Color32 {
    p.trace.gamma_multiply(p.fill)
}

fn pale_fill(p: &Palette) -> Color32 {
    p.trace.gamma_multiply(p.pale).gamma_multiply(p.fill)
}

fn shade(p: &Palette) -> Color32 {
    p.attention_fill.gamma_multiply(p.shade)
}

/// The vertices of `clip`'s meshes in the trace's colour at an opacity between none and full:
/// a lane's shaded columns, which `shade` gives their opacities.
fn shaded(h: &Harness<'_, Host>, clip: Rect, p: &Palette) -> Vec<Pos2> {
    shapes(h)
        .iter()
        .filter(|s| s.clip_rect == clip)
        .filter_map(|s| match &s.shape {
            Shape::Mesh(mesh) => Some(mesh.vertices.clone()),
            _ => None,
        })
        .flatten()
        .filter(|v| {
            let a = v.color.a();
            a > 0 && a < 255 && v.color == p.trace.gamma_multiply(f32::from(a) / 255.0)
        })
        .map(|v| v.pos)
        .collect()
}

/// Where tick `t` sits across the plot as last drawn.
fn x_of(h: &Harness<'_, Host>, t: i64) -> f32 {
    let s = drawn(h);
    let plot = s.layout.plot;
    let w = s.window;
    plot.left() + ((t - w.start) as f64 / (w.end - w.start) as f64) as f32 * plot.width()
}

/// The plot's device pixel columns inside `extent` that no band of the trace covers.
fn blank(h: &Harness<'_, Host>, extent: &std::ops::Range<i64>) -> Vec<f32> {
    let s = drawn(h);
    let plot = s.layout.plot;
    let p = palette();
    let mut spans = spans_of(h, plot, fill(&p));
    spans.extend(spans_of(h, plot, pale_fill(&p)));
    let per = (s.window.end - s.window.start) as f64 / f64::from(plot.width());
    // The middle of each device pixel across the plot.
    let ppp = h.ctx.pixels_per_point();
    let pixels = (plot.width() * ppp).round() as i32;
    (0..pixels)
        .map(|k| plot.left() + (k as f32 + 0.5) / ppp)
        .filter(|&x| {
            let tick = s.window.start as f64 + f64::from(x - plot.left()) * per;
            tick >= extent.start as f64 && tick < extent.end as f64
        })
        .filter(|&x| !spans.iter().any(|&(a, b)| a <= x && x <= b))
        .collect()
}

#[test]
fn fitted_range_leaves_the_label_rows_above_the_trace() {
    assert_eq!(headroom((0.0, 16.0), 0, 16.0, 200.0), (0.0, 16.0));
    assert_eq!(headroom((0.0, 16.0), 3, 16.0, 80.0), (0.0, 16.0));
    let (lo, hi) = headroom((-1.0, 16.0), 3, 16.0, 208.0);
    let band = f64::from(crate::graph::LABEL_TOP + 3.0 * 16.0);
    let below_top = (hi - 16.0) / (hi - lo) * 208.0;
    assert!((below_top - band).abs() < 1e-9, "{below_top}");
}

/// The highest point of the trace drawn, from the top of the plot.
fn trace_top(h: &Harness<'_, Host>) -> f32 {
    let plot = plot(h);
    let p = palette();
    let mut ys: Vec<f32> = vertices_of(h, plot, p.trace).iter().map(|v| v.y).collect();
    ys.extend(vertices_of(h, plot, fill(&p)).iter().map(|v| v.y));
    ys.into_iter().fold(f32::INFINITY, f32::min) - plot.top()
}

#[test]
fn marker_labels_keep_clear_of_the_fitted_trace() {
    // Labels long enough that four markers 50 ms apart fill all three rows.
    let markers: Vec<_> = (0..4)
        .map(|n| {
            (
                5 * SECOND + n * 50 * MS,
                n as u64,
                format!("a label of some length {n}"),
            )
        })
        .collect();
    let mut cap = Capture::new(1000, 20_000, noisy);
    cap.markers = markers;
    let window = 4 * SECOND..6 * SECOND;
    let (mut h, _gate) = showing(cap.write(), at(window.clone()), true);
    wait(&mut h);
    let plot = plot(&h);
    let labels: Vec<Rect> = texts(&h)
        .into_iter()
        .filter(|(t, _)| t.contains("a label of some length"))
        .map(|(_, r)| r)
        .collect();
    let rows: std::collections::BTreeSet<i32> = labels.iter().map(|r| r.top() as i32).collect();
    assert_eq!(rows.len(), 3, "{labels:?}");
    let under_labels = labels.iter().map(|r| r.bottom()).fold(0.0, f32::max) - plot.top();
    let top = trace_top(&h);
    assert!(
        top >= under_labels,
        "the trace's top {top} under the labels' {under_labels}"
    );

    // The same recording with its markers out of view keeps the same room for them.
    let fitted = drawn(&h).y;
    h.state_mut().state.window = Window::At(14 * SECOND..16 * SECOND);
    answered(&mut h, 14 * SECOND..16 * SECOND);
    h.state_mut().state.window = Window::At(window.clone());
    answered(&mut h, window.clone());
    assert_eq!(drawn(&h).y, fitted);

    // Without markers the trace would have reached into the labels' rows.
    let mut bare = Capture::new(1000, 20_000, noisy);
    bare.markers.clear();
    let (mut h, _gate) = showing(bare.write(), at(window), true);
    wait(&mut h);
    let top = trace_top(&h);
    assert!(top < under_labels, "the trace's top {top} with no markers");
}

#[test]
fn zooming_the_plot_queries_the_range_it_shows() {
    let (mut h, _gate) = showing(fixture().write(), ViewState::default(), true);
    let before = wait(&mut h).window.clone();
    // In select mode, where only dragging is the selection's, the wheel still zooms.
    h.state_mut().state.select_mode = true;
    let p = plot(&h).center() + vec2(100.0, 0.0);
    let under = drawn(&h).window.start as f64
        + f64::from(p.x - plot(&h).left()) / f64::from(plot(&h).width())
            * (before.end - before.start) as f64;
    h.event(egui::Event::PointerMoved(p));
    h.step();
    h.event(egui::Event::Zoom(2.0));
    h.step();
    let Window::At(wanted) = h.state().state.window.clone() else {
        panic!("a window of its own");
    };
    assert!(h.state().changed);
    assert!(
        (wanted.end - wanted.start) < (before.end - before.start) * 3 / 4,
        "{wanted:?} from {before:?}"
    );
    // The tick under the pointer stays there.
    let now = wanted.start as f64
        + f64::from(p.x - plot(&h).left()) / f64::from(plot(&h).width())
            * (wanted.end - wanted.start) as f64;
    assert!((now - under).abs() < 1.0, "{now} from {under}");
    answered(&mut h, wanted);
}

#[test]
fn moving_the_plot_up_keeps_it_there() {
    let (mut h, _gate) = showing(fixture().write(), ViewState::default(), true);
    wait(&mut h);
    let before = drawn(&h).y;
    assert_eq!(h.state().state.y, Y::Fit);
    let p = plot(&h).center();
    h.event(egui::Event::PointerMoved(p));
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, 80.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Default::default(),
    });
    for _ in 0..60 {
        h.step();
    }
    let Y::Fixed { low, high } = h.state().state.y else {
        panic!("Y off the fit");
    };
    assert!(low > before.0, "{low} from {before:?}");
    let moved = drawn(&h).y;
    assert_eq!(moved, (low, high));
    // A new answer leaves the range where the wheel left it.
    let earlier = viewer(&h).answer().unwrap();
    h.state_mut().state.selection = Some(SECOND..2 * SECOND);
    for _ in 0..400 {
        h.step();
        if !Arc::ptr_eq(&viewer(&h).answer().unwrap(), &earlier) {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!Arc::ptr_eq(&viewer(&h).answer().unwrap(), &earlier));
    h.step();
    assert_eq!(drawn(&h).y, moved);
    // A double click fits it again.
    h.event(egui::Event::PointerMoved(p));
    for _ in 0..2 {
        button(&mut h, p, true);
        button(&mut h, p, false);
    }
    h.step();
    assert_eq!(h.state().state.y, Y::Fit);
    assert_eq!(drawn(&h).y, before);
}

#[test]
fn a_drag_along_time_leaves_y_fitted_and_a_vertical_one_moves_it_from_its_start() {
    let (mut h, _gate) = showing(fixture().write(), ViewState::default(), true);
    wait(&mut h);
    let before = drawn(&h);
    let p = plot(&h).center();
    // A pan that wanders past the detent, but along time more than across it each frame.
    drag(&mut h, p, vec2(-200.0, 30.0), 10);
    assert_eq!(h.state().state.y, Y::Fit);
    assert!(matches!(h.state().state.window, Window::At(_)));
    let panned = drawn(&h);
    // The window moves by the points dragged.
    let per = (before.window.end - before.window.start) as f64 / f64::from(plot(&h).width());
    let points = (panned.window.start - before.window.start) as f64 / per;
    assert!((points - 200.0).abs() < 1.0, "{points}");
    drag(&mut h, p, vec2(0.0, 60.0), 6);
    h.step();
    let Y::Fixed { low, .. } = h.state().state.y else {
        panic!("Y off the fit");
    };
    assert_eq!(drawn(&h).window, panned.window);
    // All 60 points, the detent's included.
    let per = (panned.y.1 - panned.y.0) / f64::from(plot(&h).height());
    let points = (low - panned.y.0) / per;
    assert!((points - 60.0).abs() < 1.0, "{points}");
}

#[test]
fn a_selection_started_straight_after_a_zoom_keeps_the_zoom() {
    let (mut h, _gate) = showing(fixture().write(), ViewState::default(), true);
    wait(&mut h);
    h.state_mut().state.select_mode = true;
    let p = plot(&h).center();
    h.event(egui::Event::PointerMoved(p));
    h.step();
    h.event(egui::Event::Zoom(2.0));
    h.step();
    let Window::At(zoomed) = h.state().state.window.clone() else {
        panic!("zoomed");
    };
    drag(&mut h, p, vec2(60.0, 0.0), 3);
    h.step();
    assert_eq!(h.state().state.window, Window::At(zoomed.clone()));
    let selection = h.state().state.selection.clone().expect("a selection");
    assert!(selection.start >= zoomed.start && selection.end <= zoomed.end);
    let per = (zoomed.end - zoomed.start) as f64 / f64::from(plot(&h).width());
    let points = (selection.end - selection.start) as f64 / per;
    assert!((points - 60.0).abs() < 1.0, "{points}");
    // Washed from where the drag started to where it ended.
    let field = drawn(&h).layout.rect;
    let wash = rects_of(&h, field, palette().wash);
    assert_eq!(wash.len(), 1, "{wash:?}");
    assert!((wash[0].left() - p.x).abs() < 1.0 && (wash[0].right() - (p.x + 60.0)).abs() < 1.0);
}

#[test]
fn answers_arrive_while_the_plot_is_still_moving() {
    let (mut h, _gate) = showing(fixture().write(), ViewState::default(), true);
    wait(&mut h);
    let p = plot(&h).center();
    h.event(egui::Event::PointerMoved(p));
    h.step();
    button(&mut h, p, true);
    let mut answers = std::collections::HashSet::new();
    for step in 1..=40 {
        h.event(egui::Event::PointerMoved(p - vec2(5.0 * step as f32, 0.0)));
        h.step();
        std::thread::sleep(Duration::from_millis(5));
        let w = viewer(&h).answer().unwrap().window.clone();
        answers.insert((w.start, w.end));
    }
    button(&mut h, p - vec2(200.0, 0.0), false);
    assert!(
        answers.len() > 3,
        "{} answers during the drag",
        answers.len()
    );
}

#[test]
fn zoomed_in_the_answer_holds_samples_and_the_strip_covers_the_extent() {
    let mut cap = fixture();
    cap.dio = true;
    let (mut h, _gate) = showing(cap.write(), ViewState::default(), true);
    let whole = wait(&mut h);
    let Drawn::Analog { samples, .. } = &whole.channels[0] else {
        panic!("analog")
    };
    assert!(samples.is_none());
    let visible = 5 * SECOND..5 * SECOND + 50 * MS;
    h.state_mut().state.window = Window::At(visible.clone());
    let a = answered(&mut h, visible);
    let Drawn::Analog {
        samples: Some(samples),
        ..
    } = &a.channels[0]
    else {
        panic!("single samples");
    };
    // 50 ms shown, 50 ms either side, and the sample past each end.
    assert_eq!(samples.len(), 152);
    let strip = a.strip.as_ref().expect("the strip");
    let extent = a.extent.clone().unwrap();
    assert_eq!(strip.extent, extent);
    assert_eq!(strip.grid.x0, extent.start as f64);
    let end = strip.grid.x0 + strip.grid.per * strip.grid.count as f64;
    assert!((end - extent.end as f64).abs() < 1.0, "{end}");
    assert!(
        a.channels[1..]
            .iter()
            .all(|c| matches!(c, Drawn::Digital { .. }))
    );
    let window = &a.statistics[0];
    assert_eq!(window.channels[I].as_ref().unwrap().samples, 50);
    // Drawn as samples, not as bands.
    let p = palette();
    assert!(spans_of(&h, plot(&h), fill(&p)).is_empty());
    assert!(!vertices_of(&h, plot(&h), p.trace).is_empty());
}

#[test]
fn a_pause_leaves_a_gap_and_only_lost_time_is_shaded() {
    // Held for 4 ms before pairs 10,120 and 15,000; pairs lost from 17,000 (17.008 s to 17.011
    // s, after both holds) and at 18,000 (18.008 s to 18.009 s).
    let mut cap = Capture::new(1000, 20_000, |_| 350_000);
    cap.lost = vec![17_000..17_003, 18_000..18_001];
    cap.held = vec![10_120, 15_000];
    cap.hold = 4 * MS;
    let (mut h, _gate) = showing(cap.write(), ViewState::default(), true);
    wait(&mut h);
    let p = palette();
    let lost = [17_008 * MS..17_011 * MS, 18_008 * MS..18_009 * MS];
    let near = |h: &Harness<'_, Host>, a: f32, b: f32, r: &std::ops::Range<i64>| {
        a <= x_of(h, r.start) + 1.0 && b >= x_of(h, r.end) - 1.0
    };
    let bands = rects_of(&h, plot(&h), shade(&p));
    assert_eq!(bands.len(), 2, "{bands:?}");
    for (band, r) in bands.iter().zip(&lost) {
        assert!(near(&h, band.left(), band.right(), r), "{band:?} for {r:?}");
        assert!(band.width() <= x_of(&h, r.end) - x_of(&h, r.start) + 2.0);
    }
    let pale = spans_of(&h, plot(&h), pale_fill(&p));
    assert!(!pale.is_empty());
    for (a, b) in &pale {
        assert!(
            lost.iter()
                .any(|r| *b >= x_of(&h, r.start) - 1.0 && *a <= x_of(&h, r.end) + 1.0),
            "pale at {a}..{b}"
        );
    }

    // Zoomed in on the first pause: a gap in the trace, and no shade.
    let pause = 10_120 * MS..10_124 * MS;
    let window = 10_110 * MS..10_135 * MS;
    h.state_mut().state.window = Window::At(window.clone());
    answered(&mut h, window);
    assert!(rects_of(&h, plot(&h), shade(&p)).is_empty());
    let (a, b) = (x_of(&h, pause.start), x_of(&h, pause.end));
    let line = vertices_of(&h, plot(&h), p.trace);
    assert!(line.iter().any(|v| v.x < a) && line.iter().any(|v| v.x > b));
    // Clear of the dots at either end, 2.5 points and their feathering.
    assert!(
        !crossed(&h, plot(&h), p.trace, a + 4.0, b - 4.0),
        "the trace crosses the pause at {a}..{b}"
    );

    // Zoomed in on the first loss: a gap, shaded.
    let window = 17_000 * MS..17_020 * MS;
    h.state_mut().state.window = Window::At(window.clone());
    answered(&mut h, window);
    let bands = rects_of(&h, plot(&h), shade(&p));
    assert_eq!(bands.len(), 1, "{bands:?}");
    let (a, b) = (x_of(&h, lost[0].start), x_of(&h, lost[0].end));
    assert!((bands[0].left() - a).abs() < 1.0 && (bands[0].right() - b).abs() < 1.0);
    assert!(
        !crossed(&h, plot(&h), p.trace, a + 4.0, b - 4.0),
        "the trace crosses the loss at {a}..{b}"
    );
}

#[test]
fn a_window_past_the_capture_counts_only_the_capture_as_unmeasured() {
    // 2 s of pairs, five of them lost before 1.1 s.
    let mut cap = Capture::new(1000, 2000, |_| 350_000);
    cap.lost = vec![500..502, 1000..1003];
    let (mut h, _gate) = showing(cap.write(), ViewState::default(), true);
    wait(&mut h);
    for (window, unmeasured) in [(1500 * MS..2300 * MS, 0), (0..2300 * MS, 5 * MS)] {
        h.state_mut().state.window = Window::At(window.clone());
        let a = answered(&mut h, window.clone());
        let extent = a.extent.clone().unwrap();
        let s = a.statistics[0].channels[I].as_ref().unwrap();
        assert_eq!(
            s.unmeasured(&window, &extent),
            escapement_tickwave_view::Ticks::whole(unmeasured),
            "{window:?}"
        );
    }
}

#[test]
fn no_column_goes_blank_while_a_finer_answer_is_pending() {
    let mut cap = Capture::new(1000, 20_000, noisy);
    cap.dio = true;
    // `around` is 6 s to 12 s.
    let window = 8 * SECOND..10 * SECOND;
    let (mut h, gate) = showing(cap.write(), at(window.clone()), false);
    let _opens = Opens(Arc::clone(&gate));
    let a = answered(&mut h, window);
    let extent = a.extent.clone().unwrap();
    assert!(blank(&h, &extent).is_empty());
    gate.close();
    let p = plot(&h).center();
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
        let blank = blank(h, &extent);
        assert!(blank.is_empty(), "{what}: blank at {blank:?}");
    };
    // A pan inside `around`. Its first frame asks; the rest wait for that query, which never
    // stops the drag.
    let issued = viewer(&h).issued();
    drag(&mut h, p, vec2(-300.0, 0.0), 5);
    check(&mut h, "a pan");
    assert_eq!(viewer(&h).issued(), issued + 1);
    let Window::At(panned) = h.state().state.window.clone() else {
        panic!("panned");
    };
    assert_eq!(drawn(&h).window, panned);
    // A zoom in: the held columns stretched.
    h.event(egui::Event::PointerMoved(p));
    h.step();
    h.event(egui::Event::Zoom(4.0));
    check(&mut h, "a zoom in");
    // Past `around`, after it and before it: the strip's columns.
    h.state_mut().state.window = Window::At(14 * SECOND..16 * SECOND);
    check(&mut h, "after around");
    h.state_mut().state.window = Window::At(2 * SECOND..4 * SECOND);
    check(&mut h, "before around");
    // A zoom out past `around` on both sides: the answer's columns, and the strip's either side.
    h.state_mut().state.window = Window::At(0..20 * SECOND);
    check(&mut h, "a zoom out");
    gate.open();
    let landed = answered(&mut h, 0..20 * SECOND);
    assert!(!Arc::ptr_eq(&landed, &a));
    assert!(blank(&h, &extent).is_empty());
}

#[test]
fn a_new_trace_is_asked_for_at_once_and_drawn_only_from_its_own_answer() {
    let window = 2 * SECOND..4 * SECOND;
    let (mut h, gate) = showing(fixture().write(), at(window.clone()), false);
    let _opens = Opens(Arc::clone(&gate));
    answered(&mut h, window);
    let p = palette();
    assert!(!spans_of(&h, plot(&h), fill(&p)).is_empty());
    gate.close();
    // A moved window holds a query back.
    h.state_mut().state.window = Window::At(3 * SECOND..5 * SECOND);
    h.step();
    h.step();
    assert!(viewer(&h).busy());
    let issued = viewer(&h).issued();
    // V in place of I: asked for though a query is under way.
    h.state_mut().trace = Some(0);
    h.step();
    assert_eq!(viewer(&h).issued(), issued + 1);
    for _ in 0..3 {
        h.step();
    }
    assert!(spans_of(&h, plot(&h), fill(&p)).is_empty());
    assert!(vertices_of(&h, plot(&h), p.trace).is_empty());
    gate.open();
    for _ in 0..400 {
        h.step();
        if viewer(&h)
            .held()
            .is_some_and(|held| held.request.channels[0] == 0)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    h.step();
    assert_eq!(viewer(&h).held().unwrap().request.channels[0], 0);
    assert!(!spans_of(&h, plot(&h), fill(&p)).is_empty());
}

#[test]
fn the_fit_follows_the_window_on_screen_while_its_answer_is_pending() {
    // The current rises by 100 nA a pair from 1 mA.
    let cap = Capture::new(1000, 20_000, |i| 1_000_000 + i * 100);
    let amps = |t: i64| (1_000_000 + t / MS * 100) as f64 * 1e-9;
    let window = 2 * SECOND..4 * SECOND;
    let (mut h, gate) = showing(cap.write(), at(window.clone()), false);
    let _opens = Opens(Arc::clone(&gate));
    let a = answered(&mut h, window);
    let layout = drawn(&h).layout;
    let fit = |raw: (f64, f64)| fitted(raw, true, 0, layout.label_row, layout.plot.height());
    let own = |a: &Answer| match &a.channels[0] {
        Drawn::Analog { fit: Some(f), .. } => *f,
        _ => panic!("a fit"),
    };
    assert_eq!(drawn(&h).y, fit(own(&a)));
    gate.close();
    let moved = 3 * SECOND..5 * SECOND;
    h.state_mut().state.window = Window::At(moved.clone());
    for _ in 0..3 {
        h.step();
    }
    assert!(Arc::ptr_eq(&viewer(&h).answer().unwrap(), &a));
    // The answer held is for 2 s to 4 s; the range drawn is the moved window's, from the
    // columns held.
    let pending = drawn(&h).y;
    assert_ne!(pending, fit(own(&a)));
    let want = fit((amps(moved.start), amps(moved.end)));
    assert!(
        (pending.1 - want.1).abs() < 1e-6,
        "{pending:?} for {want:?}"
    );
    gate.open();
    let b = answered(&mut h, moved);
    assert_eq!(drawn(&h).y, fit(own(&b)));
}

#[test]
fn selection_statistics_stay_fixed_when_the_window_changes() {
    let state = ViewState {
        selection: Some(12 * SECOND..14 * SECOND),
        ..ViewState::default()
    };
    let (mut h, _gate) = showing(fixture().write(), state, true);
    let a = wait(&mut h);
    let expected = a.statistics[1].clone();
    assert_eq!(expected.interval, 12 * SECOND..14 * SECOND);
    assert_eq!(expected.channels[I].as_ref().unwrap().samples, 2000);
    h.state_mut().state.window = Window::At(0..1000 * SECOND);
    let a = answered(&mut h, 0..1000 * SECOND);
    assert_eq!(a.statistics[1], expected);
    h.state_mut().state.selection = None;
    for _ in 0..400 {
        h.step();
        if viewer(&h).answer().unwrap().statistics.len() == 1 {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("the selection's statistics stayed");
}

#[test]
fn marker_pages_and_the_successor_come_with_the_answer() {
    let mut cap = Capture::new(1000, 40_000, |_| 350_000);
    cap.markers = (0..30)
        .map(|n| ((n + 1) * SECOND, n as u64, format!("step {n}")))
        .collect();
    let (mut h, _gate) = showing(cap.write(), ViewState::default(), true);
    h.state_mut().extras.marker_list = Some((None, 24));
    let a = wait(&mut h);
    let list = a.markers.list.as_ref().unwrap();
    assert_eq!(list.entries.len(), 24);
    assert_eq!(list.total, 30);
    let number = |m: &escapement_tickwave_view::Marker| {
        m.annotation.values.as_ref().unwrap()["number"].clone()
    };
    let c23: MarkerCursor = list.entries[23].cursor;
    assert_eq!(number(&list.entries[23]), "23");
    // Every marker in the window has its line.
    let ink = palette().ink;
    let lines = shapes(&h)
        .iter()
        .filter(|s| matches!(&s.shape, Shape::LineSegment { stroke, .. } if stroke.color == ink))
        .count();
    assert_eq!(lines, 30);

    h.state_mut().extras = Extras {
        marker_list: Some((Some(c23), 24)),
        successor_of: Some(c23),
    };
    let mut a = viewer(&h).answer().unwrap();
    for _ in 0..400 {
        h.step();
        a = viewer(&h).answer().unwrap();
        if a.markers.successor.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let list = a.markers.list.as_ref().unwrap();
    assert_eq!(list.entries.len(), 6);
    assert_eq!(list.total, 30);
    assert_eq!(number(&list.entries[0]), "24");
    let (asked, next) = a.markers.successor.clone().unwrap();
    assert_eq!(asked, c23);
    let next = next.unwrap();
    assert_eq!(number(&next), "24");
    // From the marker to the one after it.
    h.state_mut().state.selection = Some(c23.tick..next.cursor.tick);
    for _ in 0..400 {
        h.step();
        let a = viewer(&h).answer().unwrap();
        if let Some(s) = a.statistics.get(1) {
            assert_eq!(s.interval, 24 * SECOND..25 * SECOND);
            assert_eq!(s.channels[I].as_ref().unwrap().samples, 1000);
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("no selection");
}

#[test]
fn more_markers_than_a_page_says_so() {
    let note = "More markers: zoom in or use the list";
    let mut cap = Capture::new(1000, 10_000, |_| 350_000);
    cap.markers = (0..600)
        .map(|n| (n * 10 * MS, n as u64, String::new()))
        .collect();
    let (mut h, _gate) = showing(cap.write(), ViewState::default(), true);
    wait(&mut h);
    assert!(texts(&h).iter().any(|(t, _)| t == note));
    let window = 0..2 * SECOND;
    h.state_mut().state.window = Window::At(window.clone());
    answered(&mut h, window);
    assert!(!texts(&h).iter().any(|(t, _)| t == note));
}

#[test]
fn a_failed_query_is_asked_once() {
    let (mut h, gate) = showing(fixture().write(), at(2 * SECOND..4 * SECOND), false);
    let _opens = Opens(Arc::clone(&gate));
    wait(&mut h);
    gate.fail(true);
    h.state_mut().state.window = Window::At(3 * SECOND..5 * SECOND);
    for _ in 0..200 {
        h.step();
        if viewer(&h).failure().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(viewer(&h).failure().as_deref(), Some("unreadable"));
    let runs = viewer(&h).runs();
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(viewer(&h).runs(), runs);
    gate.fail(false);
    h.state_mut().state.window = Window::At(4 * SECOND..6 * SECOND);
    answered(&mut h, 4 * SECOND..6 * SECOND);
    assert_eq!(viewer(&h).failure(), None);
}

#[test]
fn a_drag_on_the_value_axis_zooms_y_about_where_it_started() {
    let (mut h, _gate) = showing(fixture().write(), ViewState::default(), true);
    wait(&mut h);
    let before = drawn(&h);
    let gutter = before.layout.gutter;
    let from = Pos2::new(gutter.center().x, before.layout.plot.center().y + 50.0);
    let plot = before.layout.plot;
    let value = |y: (f64, f64), at: f32| {
        y.0 + f64::from(plot.bottom() - at) / f64::from(plot.height()) * (y.1 - y.0)
    };
    drag(&mut h, from, vec2(0.0, 30.0), 3);
    h.step();
    assert!(matches!(h.state().state.y, Y::Fixed { .. }));
    let after = drawn(&h).y;
    assert!(
        after.1 - after.0 < before.y.1 - before.y.0,
        "{after:?} from {:?}",
        before.y
    );
    // The value where the drag started holds its place.
    let (a, b) = (value(before.y, from.y), value(after, from.y));
    assert!(
        (a - b).abs() < (before.y.1 - before.y.0) * 1e-6,
        "{b} from {a}"
    );
    assert_eq!(drawn(&h).window, before.window);
}

#[test]
fn a_lane_shades_its_gaps_and_what_it_cannot_resolve() {
    // DIO3 high for 2 ms of every 10, with its edges lost from 5.0013 s to 5.0057 s.
    let mut cap = Capture::new(1000, 20_000, noisy);
    cap.dio = true;
    cap.dio_gaps = std::iter::once(5_001_300..5_005_700).collect();
    let (mut h, _gate) = showing(cap.write(), ViewState::default(), true);
    wait(&mut h);
    let p = palette();
    let lane = drawn(&h).layout.lanes[3];
    // Zoomed out, a column holds several pulses: shaded.
    assert!(!shaded(&h, lane, &p).is_empty());
    let window = 4_990 * MS..5_020 * MS;
    h.state_mut().state.window = Window::At(window.clone());
    answered(&mut h, window);
    let lane = drawn(&h).layout.lanes[3];
    let gaps = rects_of(&h, lane, shade(&p));
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    let (a, b) = (x_of(&h, 5_001_300), x_of(&h, 5_005_700));
    assert!((gaps[0].left() - a).abs() < 1.0 && (gaps[0].right() - b).abs() < 1.0);
    // Zoomed in, every pulse resolves: levels as lines, nothing shaded.
    assert!(shaded(&h, lane, &p).is_empty());
    let levels = vertices_of(&h, lane, p.trace);
    assert!(levels.iter().any(|v| v.x < a) && levels.iter().any(|v| v.x > b));
    // The other lanes are flat low lines, with nothing shaded.
    let lane0 = drawn(&h).layout.lanes[0];
    assert!(rects_of(&h, lane0, shade(&p)).is_empty());
}

#[test]
fn a_pulse_across_a_column_boundary_draws_as_one_full_ink_stroke_at_its_middle() {
    // DIO3 high for the one pair at 10 s: a 1 ms pulse alone. Its edges fall in different columns,
    // `apart` columns apart, under the join distance, a line's width and a pixel: one stroke a line
    // wide at its middle. A line 1 wide joins them 1.25 apart; a line 3 wide joins them 2.5 apart,
    // which a line 1 wide leaves apart.
    let cap = Capture {
        dio: true,
        ..Capture::new(
            1000,
            20_000,
            |i| {
                if i == 10_000 { 3_000_000 } else { 350_000 }
            },
        )
    };
    let file = cap.write();
    for (weight, half_span, (lo, hi)) in [(1.0, 400 * MS, (1.0, 2.0)), (3.0, 200 * MS, (2.0, 4.0))]
    {
        let window = 10_000 * MS - half_span..10_000 * MS + half_span;
        let (mut h, _gate) = showing(file.clone(), at(window.clone()), true);
        h.state_mut().palette.line_weight = weight;
        answered(&mut h, window);
        let p = h.state().palette.clone();
        let lane = drawn(&h).layout.lanes[3];
        let apart = 1000.0 * drawn(&h).layout.plot.width() / (2 * half_span) as f32;
        assert!((lo..hi).contains(&apart), "edges {apart} columns apart");
        assert!(shaded(&h, lane, &p).is_empty(), "weight {weight}");
        let mid = x_of(&h, 10_000_500);
        let strokes: Vec<_> = spans_of(&h, lane, p.trace)
            .into_iter()
            .filter(|&(a, b)| (b - a - weight).abs() < 0.01 && ((a + b) / 2.0 - mid).abs() < 0.6)
            .collect();
        assert!(
            !strokes.is_empty(),
            "weight {weight}: no stroke a line wide at {mid}"
        );
    }
}

#[test]
fn a_short_pause_between_two_runs_keeps_its_gap_and_its_line_takes_the_larger_band() {
    // DIO3 toggles each millisecond for 60 ms, rests low, then toggles every 2 ms for 60 ms: at a
    // column a millisecond and a line 3 wide, two runs, the first's band the darker. At 10 s the rest
    // is 5 ms, last edge to first, a bare 2 between their paint, under the join distance of 4; at
    // 12 s it is 9 ms, a bare 6.
    let cap = Capture {
        dio: true,
        ..Capture::new(1000, 20_000, |i| {
            let high = match i {
                10_000..10_060 | 12_000..12_060 => i % 2 == 0,
                10_064..10_124 => (i - 10_064) / 2 % 2 == 0,
                12_068..12_128 => (i - 12_068) / 2 % 2 == 0,
                _ => false,
            };
            if high { 3_000_000 } else { 350_000 }
        })
    };
    let file = cap.write();
    let width = {
        let (mut h, _gate) = showing(file.clone(), at(0..1000 * MS), true);
        answered(&mut h, 0..1000 * MS);
        plot(&h).width()
    };
    let span = (f64::from(width) * MS as f64) as i64;
    for (start, rest, held) in [(10_000, 5, true), (12_000, 9, false)] {
        let (last, first) = ((start + 59) * MS, (start + 59 + rest) * MS);
        let window = (last + first) / 2 - span / 2..(last + first) / 2 + span / 2;
        let (mut h, _gate) = showing(file.clone(), at(window.clone()), true);
        h.state_mut().palette.line_weight = 3.0;
        answered(&mut h, window);
        let p = h.state().palette.clone();
        let lane = drawn(&h).layout.lanes[3];
        // The low level: draw.rs's LANE_PAD and LANE_SWING under the lane's top.
        let low = lane.top() + 3.0 + 14.0;
        let (a, b) = (x_of(&h, last), x_of(&h, first));
        let inside = |x: f32| x > a + 1.5 && x < b - 1.5;
        // The lane's vertices drawn short of full ink and not clear: its fills, not the line.
        let toned: Vec<(Pos2, u8)> = shapes(&h)
            .iter()
            .filter(|s| s.clip_rect == lane)
            .filter_map(|s| match &s.shape {
                Shape::Mesh(mesh) => Some(mesh.vertices.clone()),
                _ => None,
            })
            .flatten()
            .filter(|v| v.color.a() > 0 && v.color.a() < 255)
            .map(|v| (v.pos, v.color.a()))
            .collect();
        let darkest = |from: i64, to: i64| {
            let (x0, x1) = (x_of(&h, from), x_of(&h, to));
            toned
                .iter()
                .filter(|(v, _)| v.x > x0 && v.x < x1)
                .map(|&(_, a)| a)
                .max()
                .unwrap_or(0)
        };
        let (first_band, second_band) = (
            darkest((start + 2) * MS, (start + 57) * MS),
            darkest((start + 62 + rest) * MS, (start + 117 + rest) * MS),
        );
        assert!(first_band > second_band, "{first_band} {second_band}");
        let rest_toned: Vec<(Pos2, u8)> =
            toned.iter().copied().filter(|(v, _)| inside(v.x)).collect();
        let ink = spans_of(&h, lane, p.trace)
            .into_iter()
            .any(|(x0, x1)| x0 <= (a + b) / 2.0 && x1 >= (a + b) / 2.0);
        match held {
            true => {
                assert!(!rest_toned.is_empty(), "no line in the {rest} ms rest");
                assert!(
                    rest_toned
                        .iter()
                        .all(|(v, _)| (v.y - low).abs() <= 1.5 + 0.01),
                    "band in the rest: {rest_toned:?}"
                );
                assert!(
                    rest_toned.iter().all(|&(_, a)| a == first_band),
                    "{rest_toned:?} against {first_band}"
                );
                assert!(!ink, "the rest's line at full ink");
            }
            false => {
                assert!(
                    rest_toned.is_empty(),
                    "the {rest} ms rest toned: {rest_toned:?}"
                );
                assert!(ink, "no line at full ink in the {rest} ms rest");
            }
        }
    }
}

#[test]
fn ctrl_and_the_wheel_zoom_time_about_the_pointer() {
    let (mut h, _gate) = showing(fixture().write(), ViewState::default(), true);
    let before = wait(&mut h).window.clone();
    assert!(!h.state().changed);
    let p = plot(&h).center() - vec2(150.0, 0.0);
    let tick = |w: &std::ops::Range<i64>, h: &Harness<'_, Host>| {
        w.start as f64
            + f64::from(p.x - plot(h).left()) / f64::from(plot(h).width())
                * (w.end - w.start) as f64
    };
    let under = tick(&before, &h);
    h.event(egui::Event::PointerMoved(p));
    h.step();
    h.event_modifiers(
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, 40.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::COMMAND,
        },
        egui::Modifiers::COMMAND,
    );
    for _ in 0..30 {
        h.step();
    }
    let Window::At(zoomed) = h.state().state.window.clone() else {
        panic!("a window of its own");
    };
    assert!(h.state().changed);
    assert!(zoomed.end - zoomed.start < before.end - before.start);
    assert!((tick(&zoomed, &h) - under).abs() < 2.0);
    // The wheel zoomed time and left Y fitted.
    assert_eq!(h.state().state.y, Y::Fit);
}

#[test]
fn a_flick_up_the_value_axis_zooms_out_by_its_points_and_stays_finite() {
    let (mut h, _gate) = showing(fixture().write(), ViewState::default(), true);
    wait(&mut h);
    let before = drawn(&h);
    let from = Pos2::new(
        before.layout.gutter.center().x,
        before.layout.plot.center().y,
    );
    // 60 points up in one frame.
    drag(&mut h, from, vec2(0.0, -60.0), 1);
    h.step();
    let Y::Fixed { low, high } = h.state().state.y else {
        panic!("Y off the fit");
    };
    assert!(low.is_finite() && high.is_finite(), "{low}..{high}");
    // e^(0.02 × 60): no more, and no less, so all 60 points counted.
    let wider = (high - low) / (before.y.1 - before.y.0);
    assert!(
        (wider / 1.2f64.exp() - 1.0).abs() < 1e-9,
        "{wider} wider, for e^1.2 = {}",
        1.2f64.exp()
    );
}

#[test]
fn the_value_axis_shows_a_vertical_resize_cursor() {
    let (mut h, _gate) = showing(fixture().write(), ViewState::default(), true);
    wait(&mut h);
    let layout = drawn(&h).layout;
    let cursor = |h: &Harness<'_, Host>| h.output().platform_output.cursor_icon;
    h.event(egui::Event::PointerMoved(layout.gutter.center()));
    h.step();
    assert_eq!(cursor(&h), egui::CursorIcon::ResizeVertical);
    h.event(egui::Event::PointerMoved(layout.plot.center()));
    h.step();
    assert_eq!(cursor(&h), egui::CursorIcon::Default);
}

#[test]
fn at_two_pixels_a_point_a_query_asks_a_column_a_pixel() {
    let window = 2 * SECOND..4 * SECOND;
    let gate = Arc::new(Gate::default());
    let mut h = harness_at(at(window.clone()), 2.0);
    h.state_mut().viewer = Some(gated(
        &h.ctx,
        Capture::new(1000, 20_000, noisy).write(),
        &gate,
        true,
    ));
    let a = answered(&mut h, window);
    let plot = plot(&h);
    let columns = viewer(&h).sent().unwrap().columns;
    assert_eq!(columns, (plot.width() * 2.0).round() as u32, "{plot:?}");
    assert_eq!(a.grid.count, 3 * columns as usize);
    let extent = a.extent.clone().unwrap();
    let blank = blank(&h, &extent);
    assert!(blank.is_empty(), "blank at {blank:?}");
}

/// A first draw: before the first answer lands, the graph draws the sketch, from the coarsest
/// levels the footer holds, over the whole extent, while the query waits on what it reads; then
/// the answer replaces it.
#[test]
fn a_first_frame_draws_the_sketch_while_the_query_reads() {
    let cap = fixture();
    let gate = Arc::new(Gate::default());
    // Opening reads the magic and the last 32 KiB, which hold the footer; nothing after.
    gate.pass(2);
    let mut h = harness(ViewState::default());
    h.state_mut().viewer = Some(gated(&h.ctx, cap.write(), &gate, true));
    // Dropped before the harness, so a failure lets the query thread's read through.
    let _opens = Opens(Arc::clone(&gate));
    for _ in 0..400 {
        h.step();
        if viewer(&h).sketches() > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    h.step();
    assert_eq!(viewer(&h).sketches(), 1);
    assert!(
        viewer(&h).answer().is_none(),
        "the query waits on its reads"
    );
    let extent = 0..cap.length();
    assert_eq!(drawn(&h).window, extent);
    assert_eq!(
        blank(&h, &extent),
        Vec::<f32>::new(),
        "the sketch covers the extent"
    );
    gate.open();
    let a = wait(&mut h);
    assert_eq!(a.window, extent);
    assert!(a.statistics.len() == 1, "the answer, not the sketch");
    assert_eq!(blank(&h, &extent), Vec::<f32>::new());
}

#[test]
fn the_trace_and_the_lanes_draw_at_the_palettes_line_weight() {
    // A flat stretch of the trace, its samples under a point apart so no dot draws, and DIO0's
    // lane, flat low: each the depth of its line's solid core, at two weights.
    let window = 2_100 * MS..2_900 * MS;
    let depths = |weight: f32| {
        let (mut h, _gate) = showing(fixture().write(), at(window.clone()), true);
        h.state_mut().palette.line_weight = weight;
        answered(&mut h, window.clone());
        let p = h.state().palette.clone();
        let depth = |clip: Rect| {
            let ys: Vec<f32> = vertices_of(&h, clip, p.trace).iter().map(|v| v.y).collect();
            ys.iter().copied().fold(f32::NEG_INFINITY, f32::max)
                - ys.iter().copied().fold(f32::INFINITY, f32::min)
        };
        (depth(plot(&h)), depth(drawn(&h).layout.lanes[0]))
    };
    let (thin, thick) = (depths(1.0), depths(3.0));
    assert!(
        (thick.0 - thin.0 - 2.0).abs() < 0.01,
        "trace {thin:?} {thick:?}"
    );
    assert!(
        (thick.1 - thin.1 - 2.0).abs() < 0.01,
        "lane {thin:?} {thick:?}"
    );
}

#[test]
fn a_palette_from_the_hosts_theme_takes_its_colours_and_the_webs_opacities_and_weight() {
    for v in [egui::Visuals::light(), egui::Visuals::dark()] {
        let p = Palette::from_visuals(&v);
        let ink = v.text_color();
        assert_eq!((p.trace, p.ink, p.ink_2), (ink, ink, v.weak_text_color()));
        assert_eq!(p.surface, v.panel_fill);
        assert_eq!(p.wash, ink.gamma_multiply(0.12));
        assert_eq!(p.attention_ink, v.warn_fg_color);
        // The hair 22 % ink, the rest the panel: between the two, nearer the panel.
        for (h, (i, s)) in [p.hair.r(), p.hair.g(), p.hair.b()].into_iter().zip([
            (ink.r(), v.panel_fill.r()),
            (ink.g(), v.panel_fill.g()),
            (ink.b(), v.panel_fill.b()),
        ]) {
            let want = f32::from(i) * 0.22 + f32::from(s) * 0.78;
            assert!((f32::from(h) - want).abs() <= 0.5, "{h} against {want}");
        }
        assert_eq!(
            (p.pale, p.fill, p.shade, p.line_weight),
            (0.35, 1.0, 0.12, 1.5)
        );
    }
}
