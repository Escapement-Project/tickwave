//! The live tail as a host drives it: snapshots coming faster than the query thread answers
//! them, what `busy` promises, a snapshot given after a reopen, a reopen before the writer
//! finishes, the now a snapshot's follow-up ends at, and the slot's gates on what it runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use escapement_tickwave_view::{Error, MarkerQuery};

use super::live::Live;
use super::*;
use crate::Window;
use crate::viewer::Request;

fn request(window: Window) -> Request {
    Request {
        window,
        now: None,
        columns: 640,
        channels: vec![I],
        flags: Vec::new(),
        strip: Some((I, 640)),
        fallbacks: Vec::new(),
        selection: None,
        markers: MarkerQuery::default(),
    }
}

fn follow(span: i64) -> Request {
    request(Window::Follow { span })
}

/// Waits, ten seconds at most, until `done`.
fn until(what: &str, done: impl Fn() -> bool) {
    let t = Instant::now();
    while !done() {
        assert!(t.elapsed() < Duration::from_secs(10), "{what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn span(v: &Viewer) -> Option<i64> {
    v.answer().map(|a| a.window.end - a.window.start)
}

fn extent_end(v: &Viewer) -> Option<i64> {
    v.answer().and_then(|a| a.extent.as_ref().map(|e| e.end))
}

#[test]
fn a_users_change_is_asked_at_once_while_snapshots_come_faster_than_their_follow_ups() {
    let mut live = Live::new(Capture::new(1000, 100_000, noisy));
    let snapshot = live.write(4000);
    let gate = Arc::new(Gate::default());
    let mut h = harness(ViewState {
        window: Window::Follow { span: SECOND },
        ..ViewState::default()
    });
    h.state_mut().viewer = Some(gated_live(&h.ctx, &live.path, snapshot, &gate));
    let _opens = Opens(Arc::clone(&gate));
    followed(&mut h, 4 * SECOND);
    // A snapshot every frame, and each read 2 ms: each follow-up runs for several frames, and
    // the next snapshot waits for it.
    gate.slow(2000);
    let mut pairs = 4000;
    let mut frame = |h: &mut Harness<'_, Host>| {
        pairs += 5;
        viewer(h).update(live.write(pairs));
        h.step();
    };
    for _ in 0..200 {
        frame(&mut h);
        if viewer(&h).started() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        viewer(&h).started() && viewer(&h).busy(),
        "a follow-up runs"
    );
    let issued = viewer(&h).issued();
    h.state_mut().state.window = Window::Follow { span: 2 * SECOND };
    frame(&mut h);
    assert_eq!(
        viewer(&h).issued(),
        issued + 1,
        "asked in the frame it changed"
    );
    // Another change while that request runs waits for it, so a pan never cancels its own
    // queries; its answer lands while the snapshots go on, and then the next is asked.
    h.state_mut().state.window = Window::Follow { span: 3 * SECOND };
    let lands = |h: &Harness<'_, Host>, span: i64| {
        (viewer(h).held()).is_some_and(|x| x.request.window == Window::Follow { span })
    };
    for _ in 0..1000 {
        if lands(&h, 2 * SECOND) {
            break;
        }
        assert_eq!(
            viewer(&h).issued(),
            issued + 1,
            "asked over a request running"
        );
        frame(&mut h);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(lands(&h, 2 * SECOND));
    for _ in 0..1000 {
        frame(&mut h);
        if lands(&h, 3 * SECOND) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(lands(&h, 3 * SECOND));
    assert_eq!(viewer(&h).issued(), issued + 2);
}

#[test]
fn busy_holds_from_a_snapshot_given_until_its_answer_lands() {
    let mut live = Live::new(Capture::new(1000, 10_000, noisy));
    let ctx = egui::Context::default();
    let snapshot = live.write(3000);
    let v = Viewer::live(&ctx, &live.path, snapshot).unwrap();
    v.request(follow(SECOND));
    until("the first answer", || !v.busy() && v.answer().is_some());
    for k in 1..=20u64 {
        let to = 3000 + 50 * k;
        v.update(live.write(to));
        let end = live.cap.tick(to);
        // Read in this order, an answer read after `busy` is no older than the one then.
        until("the snapshot's answer", || {
            let busy = v.busy();
            let covered = extent_end(&v) == Some(end);
            assert!(
                busy || covered,
                "snapshot {k}: not busy, answer to {:?}",
                extent_end(&v)
            );
            covered && !busy
        });
    }
    assert_eq!(v.failure(), None);
}

#[test]
fn busy_holds_while_a_snapshot_is_taken() {
    let mut live = Live::new(Capture::new(1000, 10_000, noisy));
    let ctx = egui::Context::default();
    let snapshot = live.write(3000);
    let v = Viewer::live(&ctx, &live.path, snapshot).unwrap();
    v.request(follow(SECOND));
    until("the first answer", || !v.busy() && v.answer().is_some());
    let snapshots = v.snapshots();
    // A read of the recording holds the query thread at the snapshot it took, since taking it
    // writes to the recording, until the read is let go.
    let (held, holding) = std::sync::mpsc::channel();
    let (go, going) = std::sync::mpsc::channel::<()>();
    let reader = &v;
    std::thread::scope(|s| {
        // Dropped as an assertion here fails, so the read ends and the scope can join it.
        let go = go;
        s.spawn(move || {
            reader.recording(|_| {
                held.send(()).unwrap();
                let _ = going.recv();
            })
        });
        holding.recv().unwrap();
        v.update(live.write(3500));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(v.snapshots(), snapshots, "still being taken");
        assert!(v.busy(), "busy while it is taken");
        go.send(()).unwrap();
    });
    until("its answer", || {
        !v.busy() && extent_end(&v) == Some(3500 * MS)
    });
}

#[test]
fn a_snapshot_given_after_a_reopen_is_dropped() {
    let mut live = Live::new(Capture::new(1000, 4000, noisy));
    let ctx = egui::Context::default();
    let snapshot = live.write(3000);
    let v = Viewer::live(&ctx, &live.path, snapshot).unwrap();
    v.request(follow(SECOND));
    until("the first answer", || !v.busy() && v.answer().is_some());
    // One the host took before the finish, and gives after the reopen.
    let late = live.write(3500);
    live.finish();
    v.reopen().unwrap();
    v.request(follow(SECOND));
    until("the finished file's answer", || {
        !v.busy() && extent_end(&v) == Some(4 * SECOND)
    });
    v.update(late);
    assert!(!v.busy(), "dropped, so nothing waits");
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(v.failure(), None);
    v.request(follow(2 * SECOND));
    until("a later answer", || {
        !v.busy() && span(&v) == Some(2 * SECOND)
    });
    assert_eq!((v.failure(), extent_end(&v)), (None, Some(4 * SECOND)));
}

#[test]
fn a_reopen_after_release_of_a_file_still_being_written_carries_on_live() {
    let mut live = Live::new(Capture::new(1000, 10_000, noisy));
    let ctx = egui::Context::default();
    let snapshot = live.write(3000);
    let v = Viewer::live(&ctx, &live.path, snapshot).unwrap();
    v.request(follow(SECOND));
    until("the first answer", || !v.busy() && v.answer().is_some());
    v.release();
    v.update(live.write(3500));
    std::thread::sleep(Duration::from_millis(50));
    assert!(v.busy(), "the snapshot waits while the file is let go of");
    assert!(matches!(v.reopen(), Err(Error::Unfinished)));
    // The file read again: the snapshot is taken and its answer lands.
    until("the snapshot's answer", || {
        !v.busy() && extent_end(&v) == Some(3500 * MS)
    });
    assert!(v.facts().live);
    v.request(follow(2 * SECOND));
    until("a later answer", || {
        !v.busy() && span(&v) == Some(2 * SECOND)
    });
    v.update(live.write(4000));
    until("the next snapshot's answer", || {
        !v.busy() && extent_end(&v) == Some(4 * SECOND)
    });
    assert_eq!(v.failure(), None);
}

#[test]
fn a_snapshots_follow_up_ends_its_window_at_the_latest_now() {
    let mut live = Live::new(Capture::new(1000, 10_000, noisy));
    let snapshot = live.write(4000);
    let mut h = harness(ViewState {
        window: Window::Follow { span: SECOND },
        ..ViewState::default()
    });
    h.state_mut().viewer = Some(Viewer::live(&h.ctx, &live.path, snapshot).unwrap());
    followed(&mut h, 4 * SECOND);
    let issued = viewer(&h).issued();
    // Now past the data, inside what the held answer covers: nothing asked.
    let now = 4 * SECOND + 600 * MS;
    h.state_mut().now = Some(now);
    h.step();
    assert_eq!(viewer(&h).issued(), issued);
    viewer(&h).update(live.write(4200));
    for _ in 0..400 {
        h.step();
        if !viewer(&h).busy() && extent_end(viewer(&h)) == Some(4200 * MS) {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let a = viewer(&h).answer().unwrap();
    assert_eq!(a.extent, Some(0..4200 * MS));
    assert_eq!(viewer(&h).issued(), issued, "the viewer's own follow-up");
    // The window's statistics are those of the window drawn.
    assert_eq!(drawn(&h).window, now - SECOND..now);
    assert_eq!(a.statistics[0].interval, now - SECOND..now);
}

#[test]
fn a_request_waiting_when_a_snapshot_is_taken_runs_once() {
    let mut live = Live::new(Capture::new(1000, 10_000, noisy));
    let snapshot = live.write(4000);
    let gate = Arc::new(Gate::default());
    let ctx = egui::Context::default();
    let v = gated_live(&ctx, &live.path, snapshot, &gate);
    let _opens = Opens(Arc::clone(&gate));
    v.request(follow(SECOND));
    until("the first answer", || !v.busy() && v.answer().is_some());
    let runs = v.runs();
    // One request held at its first read, a newer one waiting, then a snapshot.
    gate.close();
    v.request(follow(2 * SECOND));
    until("it runs", || v.started());
    v.request(follow(3 * SECOND));
    v.update(live.write(5000));
    gate.open();
    until("the waiting request's answer", || {
        !v.busy() && span(&v) == Some(3 * SECOND) && extent_end(&v) == Some(5 * SECOND)
    });
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        v.runs(),
        runs + 2,
        "the one cancelled, and the one waiting once"
    );
}

#[test]
fn a_window_of_its_own_is_not_asked_again_after_a_snapshot() {
    let mut live = Live::new(Capture::new(1000, 10_000, noisy));
    let ctx = egui::Context::default();
    let snapshot = live.write(4000);
    let v = Viewer::live(&ctx, &live.path, snapshot).unwrap();
    v.request(request(Window::At(SECOND..2 * SECOND)));
    until("the first answer", || !v.busy() && v.answer().is_some());
    let (runs, snapshots) = (v.runs(), v.snapshots());
    v.update(live.write(5000));
    until("the snapshot taken", || {
        !v.busy() && v.snapshots() == snapshots + 1
    });
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(v.runs(), runs);
}

#[test]
fn a_query_release_cancels_lands_nothing() {
    let gate = Arc::new(Gate::default());
    let ctx = egui::Context::default();
    let v = gated(&ctx, fixture().write(), &gate, false);
    let _opens = Opens(Arc::clone(&gate));
    v.request(request(Window::At(0..SECOND)));
    until("the first answer", || !v.busy() && span(&v) == Some(SECOND));
    let held = v.answer().unwrap();
    gate.close();
    v.request(request(Window::At(SECOND..3 * SECOND)));
    until("it runs", || v.started());
    // `release` waits for the read under way, so the gate opens from here, once the query is
    // cancelled.
    std::thread::scope(|s| {
        let released = s.spawn(|| v.release());
        until("let go of", || v.released());
        gate.open();
        released.join().unwrap();
    });
    until("it stops", || !v.started());
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(v.failure(), None);
    assert!(Arc::ptr_eq(&v.answer().unwrap(), &held));
}
