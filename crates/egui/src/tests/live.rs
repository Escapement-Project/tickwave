//! Following a recording as it is written, at frame rate: the host's now, nothing drawn past
//! the data, a held answer scrolling with now, and asking again as answers land.

use std::fs::File;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use egui::{Pos2, Rect};
use escapement_tickwave_format::snapshot::Snapshot;
use escapement_tickwave_format::writer::Writer;

use super::*;
use crate::Window;

/// A recording a writer is writing, pair by pair, to a file.
struct Live {
    _dir: tempfile::TempDir,
    path: PathBuf,
    cap: Capture,
    writer: Writer,
    file: File,
    pairs: u64,
}

impl Live {
    fn new(cap: Capture) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.tickwave");
        let (writer, head) = cap.writer();
        let mut file = File::create(&path).unwrap();
        file.write_all(&head).unwrap();
        Self {
            _dir: dir,
            path,
            cap,
            writer,
            file,
            pairs: 0,
        }
    }

    /// Writes pairs up to `to`, flushed and in the file, and the snapshot that covers them.
    fn write(&mut self, to: u64) -> Snapshot {
        let end = self.cap.tick(to);
        self.cap.items(&mut self.writer, self.pairs..to);
        self.writer.flush(end).unwrap();
        self.file.write_all(&self.writer.take_output()).unwrap();
        self.file.flush().unwrap();
        self.pairs = to;
        self.writer.snapshot()
    }
}

fn follow(span: i64) -> ViewState {
    ViewState {
        window: Window::Follow { span },
        ..ViewState::default()
    }
}

/// The repaint the last frame asked for.
fn repaint(h: &Harness<'_, Host>) -> Duration {
    h.output().viewport_output[&egui::ViewportId::ROOT].repaint_delay
}

/// The right end of the trace drawn: the greatest x of its lines' and dots' corners.
fn trace_end(h: &Harness<'_, Host>) -> f32 {
    let plot = plot(h);
    let p = palette();
    vertices_of(h, plot, p.trace)
        .iter()
        .map(|v| v.x)
        .fold(f32::NEG_INFINITY, f32::max)
}

#[test]
fn a_followed_live_window_ends_at_now_once_now_is_past_the_data() {
    let mut live = Live::new(Capture::new(1000, 10_000, noisy));
    let snapshot = live.write(4000);
    let mut h = harness(follow(SECOND));
    h.state_mut().strip = true;
    h.state_mut().viewer = Some(Viewer::live(&h.ctx, &live.path, snapshot).unwrap());
    // No now: the window follows the extent.
    let a = answered(&mut h, 3 * SECOND..4 * SECOND);
    assert_eq!(a.extent, Some(0..4 * SECOND));
    // Following a recording as it is written, it asks for every frame, with no answer landing.
    for _ in 0..5 {
        h.step();
    }
    assert_eq!(repaint(&h), Duration::ZERO);
    // Now past the data: the window ends there, drawn from the answer held, which covers it.
    let now = 4 * SECOND + 300 * MS;
    h.state_mut().now = Some(now);
    h.step();
    assert_eq!(drawn(&h).window, now - SECOND..now);
    // The strip washes the same window, cut at the extent's end.
    let band = crate::strip::shown(&h.ctx, h.state().strip_id.unwrap())
        .unwrap()
        .1;
    let wash = rects_of(&h, band, palette().wash);
    assert_eq!(wash.len(), 1, "{wash:?}");
    let x = |t: i64| band.left() + (t as f64 / (4 * SECOND) as f64) as f32 * band.width();
    assert!((wash[0].left() - x(now - SECOND)).abs() < 0.5, "{wash:?}");
    assert!((wash[0].right() - band.right()).abs() < 0.5, "{wash:?}");
    // Now behind the data: the window ends at the data's end.
    h.state_mut().now = Some(3 * SECOND + 500 * MS);
    answered(&mut h, 3 * SECOND..4 * SECOND);
    assert_eq!(drawn(&h).window, 3 * SECOND..4 * SECOND);
    // A window of its own doesn't move with now, and asks nothing as now moves.
    h.state_mut().state.window = Window::At(SECOND..2 * SECOND);
    answered(&mut h, SECOND..2 * SECOND);
    let issued = viewer(&h).issued();
    for k in 1..=10 {
        h.state_mut().now = Some(4 * SECOND + k * 16 * MS);
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(viewer(&h).issued(), issued);
    assert_eq!(drawn(&h).window, SECOND..2 * SECOND);
}

#[test]
fn nothing_is_drawn_between_the_data_end_and_now() {
    let mut cap = Capture::new(1000, 10_000, noisy);
    cap.dio = true;
    cap.under = |i| i % 10 == 3;
    // Pairs lost just before the end, so a shade and pale samples sit near it.
    cap.lost = std::iter::once(3980..3983).collect();
    let mut live = Live::new(cap);
    let snapshot = live.write(4000);
    let mut h = harness(follow(SECOND));
    h.state_mut().viewer = Some(Viewer::live(&h.ctx, &live.path, snapshot).unwrap());
    let now = 4 * SECOND + 500 * MS;
    h.state_mut().now = Some(now);
    answered(&mut h, now - SECOND..now);
    let s = drawn(&h);
    let plot = s.layout.plot;
    let end = plot.left() + 0.5 * plot.width();
    let p = palette();
    let past =
        |points: Vec<Pos2>, slack: f32| points.into_iter().filter(|v| v.x > end + slack).count();
    // A dot at the last sample reaches 2.5 points and its feathering past the sample's tick,
    // which lies a period before the end: nothing reaches past the end.
    assert_eq!(
        past(vertices_of(&h, plot, p.trace), 1.0),
        0,
        "the trace past the data"
    );
    let pale = p.trace.gamma_multiply(p.pale);
    assert!(
        !vertices_of(&h, plot, pale).is_empty(),
        "pale samples before the end"
    );
    assert_eq!(
        past(vertices_of(&h, plot, pale), 1.0),
        0,
        "pale samples past the data"
    );
    let shade = p.attention_fill.gamma_multiply(p.shade);
    let shades = rects_of(&h, plot, shade);
    assert_eq!(shades.len(), 1, "{shades:?}");
    assert!(
        shades.iter().all(|r: &Rect| r.right() <= end + 1.0),
        "{shades:?}"
    );
    for lane in &s.layout.lanes {
        assert!(!vertices_of(&h, *lane, p.trace).is_empty());
        assert_eq!(
            past(vertices_of(&h, *lane, p.trace), 1.0),
            0,
            "a lane past the data"
        );
        assert!(
            rects_of(&h, *lane, shade)
                .iter()
                .all(|r| r.right() <= end + 1.0)
        );
    }
}

#[test]
fn a_held_answer_scrolls_with_now_while_a_query_is_held_back() {
    let mut live = Live::new(Capture::new(1000, 10_000, noisy));
    let snapshot = live.write(4000);
    let gate = Arc::new(Gate::default());
    let mut h = harness(follow(SECOND));
    h.state_mut().viewer = Some(gated_live(&h.ctx, &live.path, snapshot, &gate));
    let _opens = Opens(Arc::clone(&gate));
    h.state_mut().now = Some(4 * SECOND);
    let a = answered(&mut h, 3 * SECOND..4 * SECOND);
    // A snapshot asks, and its query is held back at its first read.
    gate.close();
    let issued = viewer(&h).issued();
    viewer(&h).update(live.write(5000));
    for _ in 0..400 {
        if viewer(&h).snapshots() == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    h.step();
    assert_eq!(viewer(&h).issued(), issued + 1);
    let per = SECOND as f64 / f64::from(plot(&h).width());
    let mut before = trace_end(&h);
    for k in 1..=5 {
        let now = 4 * SECOND + k * 10 * MS;
        h.state_mut().now = Some(now);
        h.step();
        assert_eq!(drawn(&h).window, now - SECOND..now, "frame {k}");
        assert!(
            Arc::ptr_eq(&viewer(&h).answer().unwrap(), &a),
            "frame {k}: the answer held"
        );
        // The data's end moves left by the 10 ms now moved.
        let end = trace_end(&h);
        let moved = f64::from(before - end) * per;
        assert!(
            (moved - (10 * MS) as f64).abs() < per,
            "frame {k}: {moved} ticks"
        );
        before = end;
    }
    // Nothing more asked while it was held.
    assert_eq!(viewer(&h).issued(), issued + 1);
    gate.open();
    answered(&mut h, 4 * SECOND..5 * SECOND);
}

#[test]
fn a_moving_now_asks_only_once_the_window_leaves_what_the_held_answer_covers() {
    let mut live = Live::new(Capture::new(1000, 10_000, noisy));
    let snapshot = live.write(4000);
    let mut h = harness(follow(SECOND));
    h.state_mut().viewer = Some(Viewer::live(&h.ctx, &live.path, snapshot).unwrap());
    h.state_mut().now = Some(4 * SECOND);
    let a = answered(&mut h, 3 * SECOND..4 * SECOND);
    // The answer covers 2 s to 5 s: now moving 30 frames inside it, with no snapshot, asks
    // nothing, and the window still ends at now.
    let issued = viewer(&h).issued();
    for k in 1..=30 {
        let now = 4 * SECOND + k * 16 * MS;
        h.state_mut().now = Some(now);
        h.step();
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(drawn(&h).window, now - SECOND..now, "frame {k}");
    }
    assert_eq!(viewer(&h).issued(), issued);
    assert!(Arc::ptr_eq(&viewer(&h).answer().unwrap(), &a));
    // A snapshot asks once.
    viewer(&h).update(live.write(4500));
    for _ in 0..400 {
        h.step();
        if viewer(&h).issued() > issued {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(viewer(&h).issued(), issued + 1);
    // Now carried past what the latest answer covers, with no snapshot: asked once, at that now.
    let around = viewer(&h).answer().unwrap().around.clone();
    let now = around.end + 200 * MS;
    h.state_mut().now = Some(now);
    let landed = answered(&mut h, now - SECOND..now);
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(viewer(&h).issued(), issued + 2);
    assert!(landed.around.contains(&now));
}

#[test]
fn a_live_follow_asks_again_once_an_answer_lands_and_never_on_a_timer() {
    let mut live = Live::new(Capture::new(1000, 10_000, noisy));
    let snapshot = live.write(4000);
    let gate = Arc::new(Gate::default());
    let mut h = harness(follow(SECOND));
    h.state_mut().viewer = Some(gated_live(&h.ctx, &live.path, snapshot, &gate));
    let _opens = Opens(Arc::clone(&gate));
    answered(&mut h, 3 * SECOND..4 * SECOND);
    let issued = viewer(&h).issued();
    // Half a second of frames with no new snapshot and no now: nothing asked.
    for _ in 0..30 {
        h.step();
    }
    assert_eq!(viewer(&h).issued(), issued);
    // A snapshot taken: asked at once, and the query held back at its first read.
    gate.close();
    viewer(&h).update(live.write(5000));
    let snapshots = |h: &Harness<'_, Host>| viewer(h).snapshots();
    for _ in 0..400 {
        if snapshots(&h) == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(snapshots(&h), 1);
    h.step();
    assert_eq!(viewer(&h).issued(), issued + 1);
    assert!(viewer(&h).busy());
    // Another snapshot, and now moving every frame, while that one is in flight: nothing more
    // asked.
    viewer(&h).update(live.write(6000));
    for k in 1..=10 {
        h.state_mut().now = Some(5 * SECOND + k * 16 * MS);
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(viewer(&h).issued(), issued + 1);
    // It lands, the newer snapshot is taken, and the graph asks again, once.
    h.state_mut().now = None;
    gate.open();
    let a = answered(&mut h, 5 * SECOND..6 * SECOND);
    assert_eq!(a.extent, Some(0..6 * SECOND));
    assert_eq!(viewer(&h).issued(), issued + 2);
}

#[test]
fn the_strip_labels_its_time_axis_at_the_times_they_name() {
    let (mut h, _gate) = {
        let gate = Arc::new(Gate::default());
        let mut h = harness(ViewState::default());
        h.state_mut().strip = true;
        h.state_mut().viewer = Some(gated(&h.ctx, fixture().write(), &gate, true));
        (h, gate)
    };
    let extent = wait(&mut h).extent.clone().unwrap();
    let (rect, band) = crate::strip::shown(&h.ctx, h.state().strip_id.unwrap()).unwrap();
    let below = Rect::from_min_max(Pos2::new(rect.left(), band.bottom()), rect.max);
    let labels: Vec<(String, Rect)> = texts(&h)
        .into_iter()
        .filter(|(_, r)| below.contains_rect(*r))
        .collect();
    assert!(labels.len() >= 3, "{labels:?}");
    for (text, r) in &labels {
        // "<seconds> s", its space a no-break one.
        let seconds: f64 = text
            .strip_suffix("\u{a0}s")
            .unwrap_or_else(|| panic!("{text:?}"))
            .parse()
            .unwrap();
        let t = (seconds * SECOND as f64).round() as i64;
        let x = rect.left()
            + ((t - extent.start) as f64 / (extent.end - extent.start) as f64) as f32
                * rect.width();
        assert!(
            (r.center().x - x).abs() < 1.0,
            "{text} at {}, its tick at {x}",
            r.center().x
        );
    }
}
