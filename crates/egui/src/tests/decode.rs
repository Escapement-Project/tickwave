//! A recording decoded on a worker: binding, the tally a zoomed-out track draws, the window chain
//! a zoomed-in one draws, letting go of the file and taking it again, and the widget over a
//! graph, on tickwave's own SWD capture.

use super::palette;
use crate::decode::{
    Decode, DecodeOptions, DecodeState, Decoding, Phase, Place, Progress, Tracked,
};
use crate::{Graph, ViewState, Viewer, Window};
use egui_kittest::Harness;
use escapement_tickwave_decode::{Host, HostOptions, Span, Tick};
use escapement_tickwave_decoders::recipe::{self, Recipe};
use escapement_tickwave_view::cursor::timeline_map;
use escapement_tickwave_view::{Cache, Recording};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/fixtures/identify-swd.tickwave")
}

const SWD: &str = r#"
[[decode]]
id = "swd"
decoder = "swd"
version = 1
label = "SWD"
inputs = { SWCLK = "SWCLK", SWDIO = "SWDIO" }

[[decode]]
id = "adi"
decoder = "adiv5"
version = 1
inputs = { transfers = { analysis = "swd" } }
"#;

fn open() -> Arc<Recording> {
    let file = std::fs::File::open(fixture()).unwrap();
    Arc::new(Recording::open(Arc::new(file), Cache::new(16 << 20)).unwrap())
}

/// Binds `recipe` over the fixture, as a host's bind function does.
fn bind(recipe: &'static str) -> impl Fn() -> Result<Decoding, crate::BindFailure> + Send + Sync {
    binding(recipe, &[])
}

/// Every item, kept.
const ALL: &[Span] = &[Span {
    start: Tick::MIN,
    end: Tick::MAX,
}];

/// As `bind`, each analysis bound to keep `keep`.
fn binding(
    recipe: &'static str,
    keep: &'static [Span],
) -> impl Fn() -> Result<Decoding, crate::BindFailure> + Send + Sync {
    move || {
        let rec = open();
        let b = Recipe::parse(recipe)?.bind(
            &rec,
            recipe::window(&rec),
            keep,
            HostOptions::default(),
        )?;
        let mut analyses = vec![];
        for a in b.analyses {
            let map = timeline_map(&rec, a.timeline)?;
            analyses.push(Tracked {
                id: a.id,
                name: a.label,
                map,
            });
        }
        Ok(Decoding {
            host: b.host,
            analyses,
        })
    }
}

fn until(what: &str, done: impl Fn() -> bool) {
    let t = Instant::now();
    while !done() {
        assert!(t.elapsed() < Duration::from_secs(20), "{what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn finished(d: &Decode) -> bool {
    matches!(d.progress(), Progress::Running(r) if r.iter().all(|x| x.finished.is_some()))
}

/// The same recipe's analyses, every item kept, run to the end.
fn kept() -> Host {
    let rec = open();
    let mut h = (Recipe::parse(SWD).unwrap())
        .bind(&rec, recipe::window(&rec), ALL, HostOptions::default())
        .unwrap()
        .host;
    h.run_until(|_| false).unwrap();
    h
}

#[test]
fn a_decode_binds_on_its_worker_and_tallies_what_its_pass_keeps_none_of() {
    let ctx = egui::Context::default();
    // Bound to keep everything: the decode's forward pass keeps nothing whatever the binding says.
    let d = Decode::start(&ctx, DecodeOptions::default(), binding(SWD, ALL));
    until("the pass finishes", || finished(&d));
    let all = kept();
    let g = d.lock();
    let Phase::Running(r) = &g.phase else {
        panic!("running");
    };
    for l in &r.lanes {
        let want = all
            .store(all.index(r.host.name(l.id)).unwrap())
            .items()
            .len();
        assert!(want > 0);
        assert_eq!(l.tally.items(), want as u64, "{}", l.name);
        assert!(
            r.host.store(l.id).items().is_empty(),
            "the pass keeps nothing"
        );
    }
    let kept = r.host.kept(r.lanes[0].id).map(|k| k.bytes);
    drop(g);
    assert!(kept.is_some_and(|b| b > 0), "the pass keeps checkpoints");
    assert_eq!(Some(d.checkpoint_bytes()), kept);
}

#[test]
fn a_window_few_enough_items_to_draw_apart_is_derived_and_a_crowded_one_is_not() {
    let ctx = egui::Context::default();
    let d = Decode::start(&ctx, DecodeOptions::default(), bind(SWD));
    until("the pass finishes", || finished(&d));
    let all = kept();
    let swd = all.index("swd").unwrap();
    // A window around the first transfer, as the axis gives it: one tick a nanosecond.
    let first = all.store(swd).items().values().next().unwrap().start;
    let window = first - 20_000..first + 80_000;
    d.show(window.clone(), 800.0);
    until("the window is drawn", || {
        let g = d.lock();
        matches!(&g.phase, Phase::Running(r) if r.chains[0].drawn.is_some())
    });
    let g = d.lock();
    let Phase::Running(r) = &g.phase else {
        panic!("running");
    };
    let chain = r.chains[0].drawn.as_ref().unwrap();
    assert!(chain.axis.start <= window.start && window.end <= chain.axis.end);
    let ours = r.host.window_run(chain.id, r.lanes[0].id).unwrap();
    let meets = |i: &&escapement_tickwave_decode::Item| {
        i.start < window.end && i.end.is_none_or(|e| e > window.start)
    };
    let got: Vec<_> = ours
        .store()
        .items()
        .values()
        .filter(meets)
        .map(|i| i.id)
        .collect();
    let want: Vec<_> = all
        .store(swd)
        .items()
        .values()
        .filter(meets)
        .map(|i| i.id)
        .collect();
    assert!(!want.is_empty());
    assert_eq!(
        got, want,
        "the window's items, as a pass keeping all of them holds them"
    );
    let density = chain.densities[0].as_ref().unwrap();
    assert_eq!(
        density.items(),
        ours.store().items().len() as u64,
        "the chain's density counts its store"
    );
    drop(g);
    // The whole recording at 80 points is too crowded to draw apart: no chain for it.
    d.show(0..i64::MAX / 4, 80.0);
    std::thread::sleep(Duration::from_millis(50));
    let g = d.lock();
    let Phase::Running(r) = &g.phase else {
        panic!("running");
    };
    assert!(
        r.chains[0].pending.is_none(),
        "no chain for a crowded window"
    );
}

#[test]
fn a_released_decode_lets_go_and_a_reopened_one_starts_again() {
    let ctx = egui::Context::default();
    let d = Decode::start(&ctx, DecodeOptions::default(), bind(SWD));
    until("the pass finishes", || finished(&d));
    d.release();
    assert_eq!(d.progress(), Progress::Released);
    d.reopen().unwrap();
    until("the pass finishes again", || finished(&d));
}

#[test]
fn a_bind_that_fails_shows_in_progress() {
    let ctx = egui::Context::default();
    let d = Decode::start(
        &ctx,
        DecodeOptions::default(),
        bind("[[decode]]\nid = \"x\"\ndecoder = \"none\"\nversion = 1\n"),
    );
    until("refused", || matches!(d.progress(), Progress::Refused(_)));
    let Progress::Refused(why) = d.progress() else {
        unreachable!()
    };
    assert!(why.contains("no decoder none"), "{why}");
}

struct View {
    viewer: Viewer,
    state: ViewState,
    decode: Decode,
    decoded: DecodeState,
    lanes: Vec<usize>,
    place: Place,
    /// `Graph::shown` read the decoded graph's response last frame.
    shown: bool,
}

/// The decoded graph draws the tracks under the graph, answers `Graph::shown`, and lays out the
/// height `tracks_height` says, with the panel under it or nowhere.
#[test]
fn the_decoded_graph_draws_its_tracks_under_the_graph() {
    let rec = open();
    let lanes = vec![rec.channel("SWCLK").unwrap(), rec.channel("SWDIO").unwrap()];
    let all = kept();
    let first = all
        .store(all.index("swd").unwrap())
        .items()
        .values()
        .next()
        .unwrap()
        .start;
    for place in [Place::Nowhere, Place::Under(200.0)] {
        let mut h = Harness::builder()
            .with_size(egui::vec2(1000.0, 600.0))
            .build_ui_state(
                |ui, v: &mut View| {
                    let palette = palette();
                    let place = v.place;
                    let r = ui.add(
                        Graph::lanes_only(&v.viewer, &mut v.state, &palette)
                            .lanes(&v.lanes)
                            .decoded(&v.decode, &mut v.decoded)
                            .panel(place),
                    );
                    v.shown = Graph::shown(&r).is_some();
                },
                {
                    let ctx = egui::Context::default();
                    View {
                        viewer: Viewer::open(&ctx, &fixture()).unwrap(),
                        state: ViewState {
                            window: Window::At(first - 20_000..first + 80_000),
                            ..ViewState::default()
                        },
                        decode: Decode::start(&ctx, DecodeOptions::default(), bind(SWD)),
                        decoded: DecodeState::default(),
                        lanes: lanes.clone(),
                        place,
                        shown: false,
                    }
                },
            );
        let t = Instant::now();
        loop {
            h.step();
            let drawn = {
                let g = h.state().decode.lock();
                matches!(&g.phase, Phase::Running(r) if r.chains[0].drawn.is_some())
            };
            if drawn && finished(&h.state().decode) {
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(20), "drawn");
            std::thread::sleep(Duration::from_millis(2));
        }
        h.run_steps(3);
        assert!(
            h.state().shown,
            "Graph::shown reads the decoded graph's response"
        );
        assert!(h.state().decode.tracks_height(&h.state().decoded) > 0.0);
    }
}

/// Pictures of the decoded graph: SWD and ADIv5 under SWCLK and SWDIO, zoomed in to the first
/// transfer, the panel beside them showing it selected.
///
///     TICKWAVE_SHOTS=DIR cargo test -p escapement-tickwave-egui --features decode --lib \
///         tests::decode::shots -- --ignored
#[test]
#[ignore = "writes pictures where TICKWAVE_SHOTS says"]
fn shots() {
    let Some(dir) = std::env::var_os("TICKWAVE_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let rec = open();
    let lanes = vec![rec.channel("SWCLK").unwrap(), rec.channel("SWDIO").unwrap()];
    let all = kept();
    let swd = all.store(all.index("swd").unwrap());
    let first = swd.items().values().next().unwrap();
    let (start, id) = (first.start, first.id);
    let ctx = egui::Context::default();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1400.0, 520.0))
        .wgpu()
        .build_ui_state(
            |ui, v: &mut View| {
                let palette = palette();
                let place = v.place;
                let r = ui.add(
                    Graph::lanes_only(&v.viewer, &mut v.state, &palette)
                        .lanes(&v.lanes)
                        .decoded(&v.decode, &mut v.decoded)
                        .panel(place),
                );
                v.shown = Graph::shown(&r).is_some();
            },
            View {
                viewer: Viewer::open(&ctx, &fixture()).unwrap(),
                state: ViewState {
                    window: Window::At(start - 5_000..start + 45_000),
                    ..ViewState::default()
                },
                decode: Decode::start(&ctx, DecodeOptions::default(), bind(SWD)),
                decoded: DecodeState::default(),
                lanes,
                place: Place::Beside(360.0),
                shown: false,
            },
        );
    let t = Instant::now();
    loop {
        h.step();
        let drawn = {
            let g = h.state().decode.lock();
            matches!(&g.phase, Phase::Running(r) if r.chains[0].drawn.is_some())
        };
        if drawn && finished(&h.state().decode) {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "drawn");
        std::thread::sleep(Duration::from_millis(2));
    }
    // Asked for at the first frame, ahead of the pass: derived again exact once the pass is past.
    let t = Instant::now();
    loop {
        h.step();
        let exact = {
            let g = h.state().decode.lock();
            matches!(&g.phase, Phase::Running(r) if r.chains[0].drawn.as_ref().is_some_and(|c| !c.provisional))
        };
        if exact {
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "exact");
        std::thread::sleep(Duration::from_millis(2));
    }
    h.state_mut().decoded.panel.selected = Some((0, id));
    h.run_steps(4);
    h.render()
        .unwrap()
        .save(dir.join("decoded-swd.png"))
        .unwrap();
}
