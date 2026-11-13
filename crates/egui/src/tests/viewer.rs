//! The viewer: its latest-only slot, its repaints, a live recording and a reopen.

use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use escapement_tickwave_format::recut::Recutter;
use escapement_tickwave_format::writer::WriteOptions;
use escapement_tickwave_view::{Error, MarkerQuery};

use super::*;
use crate::Window;
use crate::viewer::Request;

fn request(window: Range<i64>, selection: Option<Range<i64>>) -> Request {
    Request {
        window: Window::At(window),
        columns: 64,
        channels: vec![I],
        flags: Vec::new(),
        strip: None,
        selection,
        markers: MarkerQuery::default(),
    }
}

/// Waits for the query thread to have started a query.
fn started(v: &Viewer) {
    for _ in 0..400 {
        if v.started() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("no query started");
}

#[test]
fn superseded_queries_cannot_replace_a_new_selection() {
    let ctx = egui::Context::default();
    let gate = Arc::new(Gate::default());
    let v = gated(&ctx, fixture().write(), &gate, false);
    // After the viewer, so it opens the gate before the viewer waits for its thread.
    let _opens = Opens(Arc::clone(&gate));
    let selection = |g: i64| Some(g * SECOND / 10..(g + 1) * SECOND / 10);
    gate.close();
    // The first is under way, held at its first read; the rest arrive while it waits.
    v.request(request(12 * SECOND..14 * SECOND, selection(1)));
    started(&v);
    for g in 2..=20 {
        v.request(request(12 * SECOND..14 * SECOND, selection(g)));
    }
    // The first's read goes through, and it stops at its next block, cancelled; the last starts
    // and is held at its first read.
    gate.pass(1);
    for _ in 0..400 {
        if v.runs() == 1 && v.started() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!((v.runs(), v.started()), (1, true));
    // A cancelled query reports nothing: no answer, no failure.
    assert!(v.held().is_none());
    assert_eq!(v.failure(), None);
    gate.open();
    let mut seen = Vec::new();
    for _ in 0..400 {
        if let Some(held) = v.held() {
            seen.push(held.request.selection.clone());
            if held.request.selection == selection(20) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!seen.is_empty(), "no answer");
    assert!(seen.iter().all(|s| *s == selection(20)), "{seen:?}");
    let s = &v.answer().unwrap().statistics[1];
    assert_eq!(s.interval, selection(20).unwrap());
    // The first, cancelled, and the last: the ones between were replaced before they started.
    assert_eq!(v.runs(), 2);
}

#[test]
fn an_answer_asks_egui_for_a_frame() {
    let ctx = egui::Context::default();
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked);
    ctx.set_request_repaint_callback(move |_| {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
    let gate = Arc::new(Gate::default());
    let v = gated(&ctx, fixture().write(), &gate, true);
    let before = asked.load(std::sync::atomic::Ordering::Relaxed);
    v.request(request(0..SECOND, None));
    // The answer is in the slot a moment before the thread asks for the frame.
    let asked_for = || asked.load(std::sync::atomic::Ordering::Relaxed) > before;
    for _ in 0..400 {
        if v.answer().is_some() && asked_for() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(v.answer().is_some());
    assert!(asked_for());
}

#[test]
fn a_file_that_is_not_a_recording_is_refused_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.tickwave");
    std::fs::write(&path, b"").unwrap();
    let ctx = egui::Context::default();
    assert!(Viewer::open(&ctx, &path).is_err());
}

/// The repaint the last frame asked for.
fn repaint(h: &Harness<'_, Host>) -> Duration {
    h.output().viewport_output[&egui::ViewportId::ROOT].repaint_delay
}

#[test]
fn a_live_recording_is_followed_and_a_reopen_keeps_the_view() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("live.tickwave");
    let cap = Capture::new(1000, 10_000, noisy);
    let (mut w, head) = cap.writer();
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(&head).unwrap();
    let mut write = |w: &mut escapement_tickwave_format::writer::Writer, pairs: Range<u64>| {
        let end = cap.tick(pairs.end);
        cap.items(w, pairs);
        w.flush(end).unwrap();
        file.write_all(&w.take_output()).unwrap();
        file.flush().unwrap();
        w.snapshot()
    };
    let snapshot = write(&mut w, 0..2000);
    let state = ViewState {
        window: Window::Follow { span: SECOND },
        ..ViewState::default()
    };
    let mut h = harness(state.clone());
    h.state_mut().viewer = Some(Viewer::live(&h.ctx, &path, snapshot).unwrap());
    let a = answered(&mut h, SECOND..2 * SECOND);
    assert_eq!(a.extent, Some(0..2 * SECOND));

    // A newer snapshot: within a quarter of a second of egui's time the window ends with it.
    let snapshot = write(&mut w, 2000..5000);
    viewer(&h).update(snapshot);
    let a = answered(&mut h, 4 * SECOND..5 * SECOND);
    assert_eq!(a.extent, Some(0..5 * SECOND));
    assert_eq!(h.state().state, state);

    // Not finished yet: the viewer keeps what it has.
    assert!(matches!(viewer(&h).reopen(), Err(Error::Unfinished)));
    assert!(viewer(&h).facts().live);

    let snapshot = write(&mut w, 5000..cap.count);
    viewer(&h).update(snapshot);
    let live = answered(&mut h, 9 * SECOND..10 * SECOND);
    file.write_all(&cap.finish(w)).unwrap();
    file.flush().unwrap();
    viewer(&h).reopen().unwrap();
    // The same view asked again of the finished file.
    for _ in 0..400 {
        h.step();
        if !Arc::ptr_eq(&viewer(&h).answer().unwrap(), &live) {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let a = viewer(&h).answer().unwrap();
    assert!(!Arc::ptr_eq(&a, &live), "no answer from the finished file");
    assert_eq!(a.window, 9 * SECOND..10 * SECOND);
    assert_eq!(a.extent, Some(0..10 * SECOND));
    assert!(!viewer(&h).facts().live);
    assert_eq!(h.state().state, state);
    // Finished, it asks for no frames of its own once its answer has landed.
    for _ in 0..5 {
        h.step();
    }
    assert!(repaint(&h) > Duration::from_secs(1), "{:?}", repaint(&h));
}

#[test]
fn a_live_graph_asks_for_frames_and_a_finished_one_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("live.tickwave");
    let cap = Capture::new(1000, 2000, noisy);
    let (mut w, head) = cap.writer();
    cap.items(&mut w, 0..cap.count);
    w.flush(cap.length()).unwrap();
    let mut file = head;
    file.extend(w.take_output());
    std::fs::write(&path, &file).unwrap();
    // A window of its own, so no answer lands after the first to ask for a frame.
    let window = 0..SECOND;
    let mut h = harness(ViewState {
        window: Window::At(window.clone()),
        ..ViewState::default()
    });
    h.state_mut().viewer = Some(Viewer::live(&h.ctx, &path, w.snapshot()).unwrap());
    answered(&mut h, window.clone());
    for _ in 0..5 {
        h.step();
    }
    assert!(
        repaint(&h) <= Duration::from_millis(50),
        "{:?}",
        repaint(&h)
    );

    let finished = dir.path().join("finished.tickwave");
    std::fs::write(&finished, cap.write()).unwrap();
    let mut h = harness(ViewState {
        window: Window::At(window.clone()),
        ..ViewState::default()
    });
    h.state_mut().viewer = Some(Viewer::open(&h.ctx, &finished).unwrap());
    answered(&mut h, window);
    for _ in 0..5 {
        h.step();
    }
    assert!(repaint(&h) > Duration::from_secs(1), "{:?}", repaint(&h));
}

#[test]
fn a_snapshot_given_a_finished_recording_is_its_failure() {
    let ctx = egui::Context::default();
    let gate = Arc::new(Gate::default());
    let mut cap = fixture();
    cap.lost = std::iter::once(500..502).collect();
    let v = gated(&ctx, cap.write(), &gate, true);
    // What a host reads of the whole capture for its notes.
    assert_eq!(v.recording(|r| r.lost(I)), 2);
    let (w, _) = cap.writer();
    v.update(w.snapshot());
    for _ in 0..400 {
        if v.failure().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        v.failure().as_deref(),
        Some("the recording is finished: it takes no snapshot")
    );
}

#[test]
fn a_reopen_after_compaction_reads_the_file_renamed_over_the_live_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("live.tickwave");
    let cap = Capture::new(1000, 10_000, noisy);
    let (mut w, head) = cap.writer();
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(&head).unwrap();
    let mut write = |w: &mut escapement_tickwave_format::writer::Writer, pairs: Range<u64>| {
        let end = cap.tick(pairs.end);
        cap.items(w, pairs);
        w.flush(end).unwrap();
        file.write_all(&w.take_output()).unwrap();
        file.flush().unwrap();
        w.snapshot()
    };
    let snapshot = write(&mut w, 0..4000);
    let state = ViewState {
        window: Window::Follow { span: SECOND },
        ..ViewState::default()
    };
    let mut h = harness(state.clone());
    h.state_mut().viewer = Some(Viewer::live(&h.ctx, &path, snapshot).unwrap());
    answered(&mut h, 3 * SECOND..4 * SECOND);

    // Before the writer finishes, a reopen finds no footer and the live recording stays.
    assert!(matches!(viewer(&h).reopen(), Err(Error::Unfinished)));
    let snapshot = write(&mut w, 4000..cap.count);
    viewer(&h).update(snapshot);
    let live = answered(&mut h, 9 * SECOND..10 * SECOND);
    assert!(viewer(&h).facts().live);

    // The writer finishes in place; a compacted copy is written beside it and renamed over it.
    file.write_all(&cap.finish(w)).unwrap();
    file.flush().unwrap();
    let finished = std::fs::read(&path).unwrap();
    let mut recut = Recutter::new(WriteOptions {
        level: 1,
        block_bytes: 1 << 20,
        batch: 1,
    });
    recut.feed(&finished).unwrap();
    let mut compacted = recut.take_output();
    compacted.extend(recut.finish().unwrap().tail);
    let beside = dir.path().join("live.tickwave.compacting");
    std::fs::write(&beside, &compacted).unwrap();
    std::fs::rename(&beside, &path).unwrap();
    // The file the writer wrote, now unlinked, empties: nothing can be read from it any more,
    // so an answer that lands is the compacted file's.
    file.set_len(0).unwrap();

    viewer(&h).reopen().unwrap();
    for _ in 0..400 {
        h.step();
        if !Arc::ptr_eq(&viewer(&h).answer().unwrap(), &live) {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let a = viewer(&h).answer().unwrap();
    assert!(!Arc::ptr_eq(&a, &live), "no answer from the compacted file");
    assert_eq!(a.window, 9 * SECOND..10 * SECOND);
    assert_eq!(a.extent, Some(0..10 * SECOND));
    assert!(!viewer(&h).facts().live);
    assert_eq!(h.state().state, state);
    h.step();
    assert_eq!(viewer(&h).failure(), None);
}
