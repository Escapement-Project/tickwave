//! A recording decoded on a worker: binding, the tally a zoomed-out track draws, the window chain
//! a zoomed-in one draws, letting go of the file and taking it again, and the widget over a
//! graph, on tickwave's own SWD capture.

use super::palette;
use crate::decode::{Asked, Decode, DecodeOptions, DecodeState, Phase, Place, Progress};
use crate::{Graph, ViewState, Viewer, Window};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use escapement_tickwave_decode::recipe::{self, Recipe};
use escapement_tickwave_decode::{Host, HostOptions, Span, Tick};
use escapement_tickwave_decode::{Setting, SettingId};
use escapement_tickwave_decoders::recipe::registry;
use escapement_tickwave_view::{Cache, Recording};
use std::ops::Range;
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

/// `SWD` with a name wider than the graph's own gutter.
const LONG: &str = r#"
[[decode]]
id = "swd"
decoder = "swd"
version = 1
label = "SWD on the debug port"
inputs = { SWCLK = "SWCLK", SWDIO = "SWDIO" }
"#;

fn open() -> Arc<Recording> {
    let file = std::fs::File::open(fixture()).unwrap();
    Arc::new(Recording::open(Arc::new(file), Cache::new(16 << 20)).unwrap())
}

/// Every item, kept.
const ALL: &[Span] = &[Span {
    start: Tick::MIN,
    end: Tick::MAX,
}];

/// Opens the recording at `path`, as a host's open function does.
fn opening(
    path: PathBuf,
) -> impl Fn() -> Result<Arc<Recording>, crate::BindFailure> + Send + Sync + 'static {
    move || {
        let file = std::fs::File::open(&path)?;
        Ok(Arc::new(Recording::open(
            Arc::new(file),
            Cache::new(64 << 20),
        )?))
    }
}

/// A decode of `recipe` over the fixture, through this crate's tests' registry.
fn decoding(ctx: &egui::Context, recipe: &str) -> Decode {
    let recipe = Recipe::parse(recipe).unwrap();
    Decode::start(
        ctx,
        DecodeOptions::default(),
        registry(),
        recipe,
        opening(fixture()),
    )
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
        .bind(
            &registry(),
            &rec,
            recipe::window(&rec),
            ALL,
            HostOptions::default(),
        )
        .unwrap()
        .host;
    h.run_until(|_| false).unwrap();
    h
}

#[test]
fn a_decode_binds_on_its_worker_and_tallies_what_its_pass_keeps_none_of() {
    let ctx = egui::Context::default();
    let d = decoding(&ctx, SWD);
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
    let d = decoding(&ctx, SWD);
    until("the pass finishes", || finished(&d));
    let all = kept();
    let swd = all.index("swd").unwrap();
    // A window around the first transfer, as the axis gives it: one tick a nanosecond.
    let first = all.store(swd).items().values().next().unwrap().start;
    let window = first - 20_000..first + 80_000;
    d.show(window.clone(), 800.0, 1e9);
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
    d.show(0..i64::MAX / 4, 80.0, 1e9);
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
    let d = decoding(&ctx, SWD);
    until("the pass finishes", || finished(&d));
    d.release();
    assert_eq!(d.progress(), Progress::Released);
    d.reopen().unwrap();
    until("the pass finishes again", || finished(&d));
}

#[test]
fn a_bind_that_fails_shows_in_progress() {
    let ctx = egui::Context::default();
    let d = decoding(
        &ctx,
        "[[decode]]\nid = \"x\"\ndecoder = \"none\"\nversion = 1\n",
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
    /// What `Graph::shown` read from the decoded graph's response last frame: the gutter it drew,
    /// and the widest its tracks' names take.
    shown: Option<(f32, f32)>,
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
                    let names = match &v.decode.lock().phase {
                        Phase::Running(run) => (run.lanes.iter())
                            .map(|l| crate::track::gutter(ui, &palette, &l.name, &l.schema))
                            .fold(0.0, f32::max),
                        _ => 0.0,
                    };
                    v.shown = Graph::shown(&r).map(|s| (s.plot.start - r.rect.left(), names));
                },
                {
                    let ctx = egui::Context::default();
                    View {
                        viewer: Viewer::open(&ctx, &fixture()).unwrap(),
                        state: ViewState {
                            window: Window::At(first - 20_000..first + 80_000),
                            ..ViewState::default()
                        },
                        decode: decoding(&ctx, LONG),
                        decoded: DecodeState::default(),
                        lanes: lanes.clone(),
                        place,
                        shown: None,
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
        let (gutter, names) = (h.state().shown).expect("Graph::shown reads its response");
        assert!(
            names > 60.0 && gutter >= names,
            "the gutter holds the tracks' names: {gutter} for {names}"
        );
        assert!(h.state().decode.tracks_height(&h.state().decoded) > 0.0);
    }
}

/// Pictures of the decoded graph: SWD and ADIv5 under SWCLK and SWDIO, zoomed in to the first
/// transfer, the panel beside them showing it selected, then its Decoders tab.
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
                v.shown = Graph::shown(&r).map(|s| (s.plot.start - r.rect.left(), 0.0));
            },
            View {
                viewer: Viewer::open(&ctx, &fixture()).unwrap(),
                state: ViewState {
                    window: Window::At(start - 5_000..start + 45_000),
                    ..ViewState::default()
                },
                decode: decoding(&ctx, SWD),
                decoded: DecodeState::default(),
                lanes,
                place: Place::Beside(360.0),
                shown: None,
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
    h.state_mut().decoded.panel.tab = crate::panel::Tab::Decoders;
    h.run_steps(4);
    h.render()
        .unwrap()
        .save(dir.join("decoders-tab.png"))
        .unwrap();
}

/// The recording and recipe `TICKWAVE_DECODE_REC` and `TICKWAVE_DECODE_RECIPE` name, else the
/// SWD fixture: its path, the first analysis's channels, its length, its ticks a microsecond, and
/// a bind function over it.
struct Real {
    path: PathBuf,
    lanes: Vec<usize>,
    length: i64,
    per_us: f64,
    recipe: &'static str,
}

impl Real {
    fn open() -> Self {
        let path = std::env::var_os("TICKWAVE_DECODE_REC").map_or_else(fixture, PathBuf::from);
        let recipe: &'static str = match std::env::var_os("TICKWAVE_DECODE_RECIPE") {
            Some(f) => Box::leak(std::fs::read_to_string(f).unwrap().into_boxed_str()),
            None => SWD,
        };
        let file = std::fs::File::open(&path).unwrap();
        let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(64 << 20)).unwrap());
        let r = Recipe::parse(recipe).unwrap();
        let lanes: Vec<usize> = (r.analyses[0].inputs.iter())
            .filter_map(|(_, i)| match i {
                recipe::Input::Channel(c) => rec.channel(c),
                recipe::Input::Analysis(_) => None,
            })
            .collect();
        Self {
            path,
            lanes,
            length: rec.length(),
            per_us: rec.timebase().num() as f64 / rec.timebase().den() as f64 / 1e6,
            recipe,
        }
    }

    fn start(&self, ctx: &egui::Context) -> Decode {
        let recipe = Recipe::parse(self.recipe).unwrap();
        let open = opening(self.path.clone());
        Decode::start(ctx, DecodeOptions::default(), registry(), recipe, open)
    }

    fn us(&self, n: f64) -> i64 {
        (n * self.per_us) as i64
    }
}

fn cpu_cap() {
    let cap = std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_max_freq");
    println!(
        "CPU clock cap: {} kHz",
        cap.as_deref().unwrap_or("unknown").trim()
    );
}

fn quantiles(v: &mut [Duration]) -> String {
    v.sort_unstable();
    let at = |q: f64| v[((v.len() - 1) as f64 * q) as usize].as_secs_f64() * 1e3;
    format!(
        "n {}  p50 {:.2}  p99 {:.2}  max {:.2} ms",
        v.len(),
        at(0.5),
        at(0.99),
        at(1.0)
    )
}

/// Where the first item `tally` counted at or after axis tick `from`, before `to`, starts.
fn first_after(tally: &crate::track::Density, from: i64, to: i64) -> Option<i64> {
    if tally.count(from, to, 0.0).0 == 0 {
        return None;
    }
    let (mut lo, mut hi) = (from + 1, to);
    while lo < hi {
        let m = lo + (hi - lo) / 2;
        if tally.count(from, m, 0.0).0 > 0 {
            hi = m;
        } else {
            lo = m + 1;
        }
    }
    Some(lo - 1)
}

/// Sleeps to the next 60 Hz frame after `next`, or starts the count again where a frame overran.
fn pace(next: &mut Instant) {
    *next += Duration::from_nanos(16_666_667);
    match next.checked_duration_since(Instant::now()) {
        Some(w) => std::thread::sleep(w),
        None => *next = Instant::now(),
    }
}

/// Frames of the decoded graph (the graph, its tracks and the panel beside them, laid out and
/// tessellated) timed at 60 Hz while the forward pass runs and once it has finished, at four
/// zooms: the whole recording, a few transfers, a transfer's bits, and the widest window from the
/// middle whose first track still draws boxes. That last is timed again with the pointer resting
/// on a box, its hover text shown, and a few transfers with the panel at its Decoders tab. Run it
/// under `taskset -c 0` for one core, the worker and the query thread then sharing it with the
/// frames. With `TICKWAVE_DECODE_REC` and `TICKWAVE_DECODE_RECIPE` set, on that recording and
/// recipe file; else on the SWD fixture.
///
///     TICKWAVE_DECODE_REC=REC TICKWAVE_DECODE_RECIPE=crates/decoders/recipes/swd.toml \
///         taskset -c 0 cargo test --release -p escapement-tickwave-egui --features decode --lib \
///         tests::decode::frame_times -- --ignored --nocapture
#[test]
#[ignore = "prints wall-clock frame times"]
fn frame_times() {
    use crate::Palette;
    let real = Real::open();
    let (length, mid) = (real.length, real.length / 2);
    let mut zooms = vec![
        ("whole", 0..length),
        ("transfers", mid..mid + real.us(400.0)),
        ("bits", mid..mid + real.us(20.0)),
    ];
    let palette = Palette::from_visuals(&egui::Visuals::dark());
    cpu_cap();
    let mut k = 0;
    while k < zooms.len() {
        let (name, window) = zooms[k].clone();
        k += 1;
        let ctx = egui::Context::default();
        let viewer = Viewer::open(&ctx, &real.path).unwrap();
        let decode = real.start(&ctx);
        let mut state = ViewState {
            window: Window::At(window.clone()),
            ..ViewState::default()
        };
        let mut decoded = DecodeState::default();
        let input = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1920.0, 600.0),
            )),
            events,
            ..egui::RawInput::default()
        };
        // Where the plot lies across the graph, and the top of its first track.
        let drawn = std::cell::Cell::new((0.0f32, 0.0f32, 0.0f32));
        let frame = |events: Vec<egui::Event>, state: &mut ViewState, decoded: &mut DecodeState| {
            let t = Instant::now();
            let out = ctx.run_ui(input(events), |ui| {
                let r = ui.add(
                    Graph::lanes_only(&viewer, state, &palette)
                        .lanes(&real.lanes)
                        .decoded(&decode, decoded)
                        .panel(Place::Beside(360.0)),
                );
                if let Some(s) = Graph::shown(&r) {
                    let top = r.rect.bottom() + ui.spacing().item_spacing.y;
                    drawn.set((s.plot.start, s.plot.end, top));
                }
            });
            let hover = (out.shapes.iter()).any(|c| match &c.shape {
                egui::Shape::Text(t) => t.galley.text().contains(" for "),
                _ => false,
            });
            let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
            (t.elapsed(), hover)
        };
        let (mut running, mut done) = (vec![], vec![]);
        let started = Instant::now();
        let mut next = Instant::now();
        let mut after = 0;
        while after < 120 && started.elapsed() < Duration::from_secs(300) {
            let (took, _) = frame(vec![], &mut state, &mut decoded);
            if finished(&decode) {
                done.push(took);
                after += 1;
            } else {
                running.push(took);
            }
            pace(&mut next);
        }
        let pass = started.elapsed().as_secs_f64() - after as f64 / 60.0;
        // From the whole recording's tally, the widest window from the first item after the
        // middle that still draws boxes on the first track.
        if name == "whole" {
            let (left, right, _) = drawn.get();
            let width = right - left;
            let g = decode.lock();
            if let Phase::Running(r) = &g.phase
                && let Some(from) = first_after(&r.lanes[0].tally, mid, length)
            {
                let tally = &r.lanes[0].tally;
                let (mut lo, mut hi) = (from + 1, length);
                while lo < hi {
                    let m = lo + (hi - lo + 1) / 2;
                    if tally.crowded(&(from..m), width) {
                        hi = m - 1;
                    } else {
                        lo = m;
                    }
                }
                let (count, _) = tally.count(from, lo, 0.0);
                println!("crowded: {count} items over {width:.0} points");
                zooms.push(("crowded", from..lo));
            }
        }
        // The pointer resting on a box of the first track, after the pass: the hover's cost.
        let mut pointed = vec![];
        let mut shown = 0;
        if name == "crowded" {
            let at = {
                let g = decode.lock();
                let Phase::Running(r) = &g.phase else {
                    unreachable!("decoding");
                };
                let src = r.source(0, &window);
                let map = r.lanes[0].map;
                // The longest item starting in the window (not in the decoded wings) in a row of
                // one lane, whose lane is then known.
                let (_, rows) = r.lanes[0].schema.layout();
                let lane = |i: &escapement_tickwave_decode::Item| {
                    let l = rows.get(usize::from(i.id.kind.0))?;
                    (l.len() == 1).then_some(l.start)
                };
                let item = (src.items.values())
                    .filter(|i| lane(i).is_some() && i.end.is_some())
                    .filter(|i| window.contains(&map.axis_saturating(i.start)))
                    .max_by_key(|i| i.end.unwrap() - i.start)
                    .expect("an item in the window");
                // The middle of the part the window shows.
                let a = map.axis_saturating(item.start).max(window.start);
                let b = map.axis_saturating(item.end.unwrap()).min(window.end);
                let middle = a + (b - a) / 2;
                let (left, right, top) = drawn.get();
                let per = (window.end - window.start) as f32 / (right - left);
                let row = lane(item).unwrap() as f32;
                egui::pos2(
                    left + (middle - window.start) as f32 / per,
                    top + (row + 0.5) * crate::track::ROW,
                )
            };
            let mut events = vec![egui::Event::PointerMoved(at)];
            let mut next = Instant::now();
            for _ in 0..120 {
                let (took, hover) = frame(std::mem::take(&mut events), &mut state, &mut decoded);
                pointed.push(took);
                shown += usize::from(hover);
                pace(&mut next);
            }
        }
        // The panel at its Decoders tab, after the pass: its forms' share of a frame.
        let mut tabbed = vec![];
        if name == "transfers" {
            decoded.panel.tab = crate::panel::Tab::Decoders;
            let mut next = Instant::now();
            for _ in 0..120 {
                tabbed.push(frame(vec![], &mut state, &mut decoded).0);
                pace(&mut next);
            }
            decoded.panel.tab = crate::panel::Tab::Selected;
        }
        drop(decode);
        // The same graph alone, nothing decoded: the decode's own share of a frame.
        let mut plain = vec![];
        let mut next = Instant::now();
        for _ in 0..120 {
            let t = Instant::now();
            let out = ctx.run_ui(input(vec![]), |ui| {
                ui.add(Graph::lanes_only(&viewer, &mut state, &palette).lanes(&real.lanes));
            });
            let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
            plain.push(t.elapsed());
            pace(&mut next);
        }
        println!("{name}: pass about {pass:.1} s");
        if !running.is_empty() {
            println!("  while the pass runs: {}", quantiles(&mut running));
        }
        println!("  after it:           {}", quantiles(&mut done));
        if !pointed.is_empty() {
            println!(
                "  pointer on a box:   {} (hover text in {shown} frames)",
                quantiles(&mut pointed)
            );
        }
        if !tabbed.is_empty() {
            println!("  Decoders tab open:  {}", quantiles(&mut tabbed));
        }
        println!("  the graph alone:    {}", quantiles(&mut plain));
    }
}

/// Latency from a press of the panel's next to the first frame whose window, graph answer and
/// decoded window are all the new window's, frames paced at 60 Hz once the pass has finished.
/// Each press is on the last item starting in a window of a few transfers, so its next lies past
/// the window's edge. On the recording and recipe `frame_times` takes; run it under
/// `taskset -c 0` too.
#[test]
#[ignore = "prints wall-clock latencies"]
fn step_latency() {
    use egui_kittest::kittest::NodeT as _;
    let real = Real::open();
    let whole = 0..real.length;
    cpu_cap();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1920.0, 600.0))
        .build_ui_state(
            |ui, v: &mut Stepped| {
                let palette = palette();
                v.ready = ready(v);
                let r = ui.add(
                    Graph::lanes_only(&v.viewer, &mut v.state, &palette)
                        .lanes(&v.lanes)
                        .decoded(&v.decode, &mut v.decoded)
                        .panel(Place::Beside(360.0)),
                );
                v.drawn = Graph::shown(&r).map(|s| (s, 0.0));
            },
            {
                let ctx = egui::Context::default();
                Stepped {
                    viewer: Viewer::open(&ctx, &real.path).unwrap(),
                    state: ViewState {
                        window: Window::At(whole.clone()),
                        ..ViewState::default()
                    },
                    decode: real.start(&ctx),
                    decoded: DecodeState::default(),
                    lanes: real.lanes.clone(),
                    drawn: None,
                    ready: false,
                }
            },
        );
    // Whether the last frame drew `window` settled.
    let settled = |h: &Harness<'_, Stepped>, window: &Range<i64>| {
        let v = h.state();
        v.ready && v.drawn.as_ref().is_some_and(|(s, _)| s.window == *window)
    };
    let mut next = Instant::now();
    let t = Instant::now();
    while !finished(&h.state().decode) {
        assert!(t.elapsed() < Duration::from_secs(300), "the pass finished");
        h.step();
        pace(&mut next);
    }
    // A few transfers, from the first item after the middle.
    let from = {
        let g = h.state().decode.lock();
        let Phase::Running(r) = &g.phase else {
            unreachable!("decoding");
        };
        first_after(&r.lanes[0].tally, real.length / 2, real.length).expect("an item")
    };
    let window = from..from + real.us(400.0);
    h.state_mut().state.window = Window::At(window.clone());
    while !settled(&h, &window) {
        assert!(
            t.elapsed() < Duration::from_secs(300),
            "settled at the start"
        );
        h.step();
        pace(&mut next);
    }
    let (mut took, mut frames, mut stuck) = (vec![], vec![], 0);
    for _ in 0..30 {
        // The last item starting in the window: next steps past its edge, into the decoded wing.
        let Window::At(w) = h.state().state.window.clone() else {
            unreachable!("a window set by the test or the panel");
        };
        let last = {
            let g = h.state().decode.lock();
            let Phase::Running(r) = &g.phase else {
                unreachable!("decoding");
            };
            let map = r.lanes[0].map;
            (r.source(0, &w).items.values())
                .filter(|i| (w.start..w.end).contains(&map.axis_saturating(i.start)))
                .max_by_key(|i| i.start)
                .map(|i| {
                    (
                        i.id,
                        r.lanes[0].schema.kind(i.id.kind).unwrap().name.to_string(),
                    )
                })
        };
        let Some((id, kind)) = last else {
            // Nothing decoded starts here: on to the next item after it.
            let from = {
                let g = h.state().decode.lock();
                let Phase::Running(r) = &g.phase else {
                    unreachable!("decoding");
                };
                first_after(&r.lanes[0].tally, w.end, real.length)
            };
            let Some(from) = from else { break };
            let on = from..from + (w.end - w.start);
            h.state_mut().state.window = Window::At(on.clone());
            while !settled(&h, &on) {
                h.step();
                pace(&mut next);
            }
            continue;
        };
        h.state_mut().decoded.panel.selected = Some((0, id));
        h.step();
        pace(&mut next);
        let label = format!("next {kind} \u{25b8}");
        let Some(button) = h
            .query_by_label(&label)
            .filter(|b| !b.accesskit_node().is_disabled())
        else {
            // Its next lies past the decoded wing; step on from there.
            stuck += 1;
            let span = w.end - w.start;
            let on = Window::At(w.end..w.end + span);
            h.state_mut().state.window = on;
            while !matches!(&h.state().state.window, Window::At(x) if settled(&h, x)) {
                h.step();
                pace(&mut next);
            }
            continue;
        };
        button.click();
        let t = Instant::now();
        let mut n = 0;
        loop {
            h.step();
            n += 1;
            let now = h.state().state.window.clone();
            let Window::At(moved) = &now else {
                unreachable!("a window set by the panel");
            };
            if *moved != w && settled(&h, moved) {
                took.push(t.elapsed());
                frames.push(n);
                break;
            }
            assert!(
                t.elapsed() < Duration::from_secs(10),
                "settled after a step"
            );
            pace(&mut next);
        }
        pace(&mut next);
    }
    assert!(!took.is_empty(), "a step that moved the window");
    frames.sort_unstable();
    println!(
        "next, to the picture settled: {} (frames: median {}, most {}); {stuck} items had no next decoded",
        quantiles(&mut took),
        frames[frames.len() / 2],
        frames[frames.len() - 1]
    );
}

/// Whether `v`'s window has its graph answer and its decoded window held, before the window a
/// panel step asked for is set: a frame starting so draws the settled picture.
fn ready(v: &Stepped) -> bool {
    let Window::At(window) = &v.state.window else {
        return false;
    };
    if v.decoded.jumping() {
        return false;
    }
    let answered = (v.viewer.answer()).is_some_and(|a| a.window == *window) && !v.viewer.busy();
    let g = v.decode.lock();
    let decoded = matches!(&g.phase, Phase::Running(r) if r.chains[0].drawn.as_ref()
        .is_some_and(|c| c.axis.start <= window.start && window.end <= c.axis.end));
    answered && decoded
}

/// A decoded graph with its panel beside it, and where it last drew: the graph's `Shown`, and
/// the top of its first track.
struct Stepped {
    viewer: Viewer,
    state: ViewState,
    decode: Decode,
    decoded: DecodeState,
    lanes: Vec<usize>,
    drawn: Option<(crate::Shown, f32)>,
    /// The last frame began with its window's answer and decoded window both held, so drew the
    /// settled picture.
    ready: bool,
}

/// Previous and next in the panel move the window to an item it does not show whole, keeping its
/// span; a click on the track selects an item the window cuts off without moving it. (The
/// fixture's timeline is the axis.)
#[test]
fn a_panel_step_brings_its_item_into_view_and_a_track_click_moves_nothing() {
    use egui_kittest::kittest::Queryable;
    let rec = open();
    let lanes = vec![rec.channel("SWCLK").unwrap(), rec.channel("SWDIO").unwrap()];
    let all = kept();
    let transfers: Vec<_> = (all.store(all.index("swd").unwrap()).items().values())
        .filter(|i| i.id.kind == escapement_tickwave_decoders::swd::TRANSFER)
        .filter_map(|i| Some((i.start, i.end?, i.id)))
        .collect();
    let (start, end, cut) = transfers[3];
    // Ending halfway through the fourth transfer.
    let window = (start + end) / 2 - 100_000..(start + end) / 2;
    let mut h = Harness::builder()
        .with_size(egui::vec2(1400.0, 700.0))
        .build_ui_state(
            |ui, v: &mut Stepped| {
                let palette = palette();
                let r = ui.add(
                    Graph::lanes_only(&v.viewer, &mut v.state, &palette)
                        .lanes(&v.lanes)
                        .motion(false)
                        .decoded(&v.decode, &mut v.decoded)
                        .panel(Place::Beside(360.0)),
                );
                let top = r.rect.bottom() + ui.spacing().item_spacing.y;
                v.drawn = Graph::shown(&r).map(|s| (s, top));
            },
            {
                let ctx = egui::Context::default();
                Stepped {
                    viewer: Viewer::open(&ctx, &fixture()).unwrap(),
                    state: ViewState {
                        window: Window::At(window.clone()),
                        ..ViewState::default()
                    },
                    decode: decoding(&ctx, SWD),
                    decoded: DecodeState::default(),
                    lanes,
                    drawn: None,
                    ready: false,
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
    // A click on the cut-off transfer's box, between its start and the plot's edge.
    let (shown, top) = h.state().drawn.clone().unwrap();
    let per = (window.end - window.start) as f32 / (shown.plot.end - shown.plot.start);
    let x = shown.plot.start + ((start + window.end) / 2 - window.start) as f32 / per;
    h.event(egui::Event::PointerMoved(egui::pos2(x, top + 12.0)));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: egui::pos2(x, top + 12.0),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(3);
    assert_eq!(h.state().decoded.panel.selected, Some((0, cut)));
    assert_eq!(
        h.state().state.window,
        Window::At(window.clone()),
        "a track click"
    );
    // Next: the fifth transfer, past the window, in the middle of one as wide, drawn in the
    // frame that took the press.
    h.get_by_label("next transfer \u{25b8}").click();
    h.step();
    let (start5, end5, fifth) = transfers[4];
    assert_eq!(h.state().decoded.panel.selected, Some((0, fifth)));
    let moved = crate::decode::bring(&window, start5..end5).unwrap();
    assert_eq!(h.state().drawn.as_ref().unwrap().0.window, moved);
    h.run_steps(2);
    assert_eq!(h.state().state.window, Window::At(moved.clone()));
    assert_eq!(moved.end - moved.start, window.end - window.start);
    // Previous: the fourth, wherever that leaves it.
    h.get_by_label("\u{25c2} previous transfer").click();
    h.run_steps(3);
    assert_eq!(h.state().decoded.panel.selected, Some((0, cut)));
    let back = crate::decode::bring(&moved, start..end).unwrap_or(moved);
    assert_eq!(h.state().state.window, Window::At(back));
}

/// A panel's choice moves the window only where it does not show the item whole: keeping its
/// span with the item in the middle, or fitting a longer item with a quarter of it either side.
#[test]
fn an_item_out_of_view_is_brought_into_it() {
    use crate::decode::bring;
    let window = 1000..2000;
    assert_eq!(bring(&window, 1200..1300), None, "in view: nothing moves");
    assert_eq!(bring(&window, 1000..2000), None, "exactly in view");
    assert_eq!(
        bring(&window, 2500..2600),
        Some(2050..3050),
        "past it: centred"
    );
    assert_eq!(
        bring(&window, 100..200),
        Some(-350..650),
        "before it: centred"
    );
    assert_eq!(
        bring(&window, 1900..2100),
        Some(1500..2500),
        "half out: centred"
    );
    assert_eq!(
        bring(&window, 5000..5000),
        Some(4500..5500),
        "an open item, at its start"
    );
    assert_eq!(
        bring(&window, 3000..7000),
        Some(2000..8000),
        "longer than the window: a quarter either side"
    );
    assert_eq!(
        bring(&window, 500..2500),
        Some(0..3000),
        "longer, across it"
    );
}

/// A decode under a graph with the panel beside it, its Decoders tab open: a setting changed
/// there decodes its chain again, and the recipe keeps what changed.
struct Configuring {
    viewer: Viewer,
    state: ViewState,
    decode: Decode,
    decoded: DecodeState,
    lanes: Vec<usize>,
}

fn configuring(recipe: &str, window: Range<i64>) -> Harness<'static, Configuring> {
    let rec = open();
    let lanes = vec![rec.channel("SWCLK").unwrap(), rec.channel("SWDIO").unwrap()];
    let ctx = egui::Context::default();
    Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .build_ui_state(
            |ui, v: &mut Configuring| {
                let palette = palette();
                ui.add(
                    Graph::lanes_only(&v.viewer, &mut v.state, &palette)
                        .lanes(&v.lanes)
                        .decoded(&v.decode, &mut v.decoded)
                        .panel(Place::Beside(480.0)),
                );
            },
            Configuring {
                viewer: Viewer::open(&ctx, &fixture()).unwrap(),
                state: ViewState {
                    window: Window::At(window),
                    ..ViewState::default()
                },
                decode: decoding(&ctx, recipe),
                decoded: DecodeState::default(),
                lanes,
            },
        )
}

/// Steps `h` until `done` holds of its decode, for at most 20 s.
fn stepped(h: &mut Harness<'_, Configuring>, what: &str, done: impl Fn(&Decode) -> bool) {
    let t = Instant::now();
    loop {
        h.step();
        if done(&h.state().decode) {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "{what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Each lane's name, and the items its forward pass has counted.
fn counted(d: &Decode) -> Vec<(String, u64)> {
    match &d.lock().phase {
        Phase::Running(r) => (r.lanes.iter())
            .map(|l| (l.name.clone(), l.tally.items()))
            .collect(),
        _ => vec![],
    }
}

#[test]
fn a_setting_changed_in_the_decoders_tab_decodes_its_chain_again() {
    let all = kept();
    let first = (all.store(all.index("swd").unwrap()).items().values())
        .next()
        .unwrap()
        .start;
    let mut h = configuring(SWD, first - 5_000..first + 45_000);
    stepped(&mut h, "decoded", finished);
    let before = counted(&h.state().decode);
    assert!(before.iter().all(|(_, n)| *n > 0), "{before:?}");
    let runs = |d: &Decode| match &d.lock().phase {
        Phase::Running(r) => r.lanes.iter().map(|l| r.host.run_id(l.id)).collect(),
        _ => vec![],
    };
    let old = runs(&h.state().decode);
    h.get_by_label("Decoders").click();
    h.run_steps(2);
    // SWD's turnaround, a stepper: 1 to 2.
    h.get_by_label("+").click();
    h.step();
    let new = runs(&h.state().decode);
    assert!(
        old.iter().zip(&new).all(|(a, b)| a != b),
        "SWD and ADIv5 decode again"
    );
    let recipe = h.state().decode.recipe();
    assert_eq!(
        recipe.analyses[0].settings,
        [("turnaround".to_owned(), Setting::Int(2))]
    );
    let bound = |d: &Decode| match &d.lock().phase {
        Phase::Running(r) => r.host.settings(r.lanes[0].id).int(SettingId(0)),
        _ => None,
    };
    assert_eq!(bound(&h.state().decode), Some(2));
    stepped(&mut h, "decoded again", finished);
    h.run_steps(2);
    let estimates = |d: &Decode| match &d.lock().phase {
        Phase::Running(r) => r.lanes.iter().filter(|l| l.estimate.is_some()).count(),
        _ => usize::MAX,
    };
    assert_eq!(
        estimates(&h.state().decode),
        0,
        "the count before goes once the pass is done"
    );
    // Back to the default: the recipe leaves it out, and the pass counts what it did.
    h.get_by_label("−").click();
    h.step();
    assert_eq!(bound(&h.state().decode), Some(1));
    assert!(h.state().decode.recipe().analyses[0].settings.is_empty());
    stepped(&mut h, "decoded as at first", finished);
    assert_eq!(counted(&h.state().decode), before);
}

/// From nothing decoded: SWD added with its ports bound by name, ADIv5 stacked on it, the SWD
/// preset beside them under fresh ids, and ADIv5 removed with what reads it.
#[test]
fn decoders_are_added_stacked_and_removed_in_the_decoders_tab() {
    let mut h = configuring("", 0..1_000_000);
    stepped(&mut h, "bound", |d| {
        matches!(d.progress(), Progress::Running(_))
    });
    // With nothing decoded, the panel opens at its Decoders tab.
    h.run_steps(2);
    h.get_by_label("SWD").click();
    h.run_steps(2);
    h.get_by_label("add").click();
    h.run_steps(2);
    let recipe = h.state().decode.recipe();
    assert_eq!(recipe.analyses.len(), 1);
    assert_eq!(
        recipe.analyses[0].inputs,
        [
            (
                "SWCLK".to_owned(),
                recipe::Input::Channel("SWCLK".to_owned())
            ),
            (
                "SWDIO".to_owned(),
                recipe::Input::Channel("SWDIO".to_owned())
            ),
        ],
        "each port bound to the channel of its name"
    );
    h.get_by_label("ADIv5 on SWD").click();
    h.run_steps(2);
    h.get_by_label("MEM-AP on ADIv5").click();
    h.run_steps(2);
    let ids =
        |d: &Decode| -> Vec<String> { d.recipe().analyses.iter().map(|a| a.id.clone()).collect() };
    assert_eq!(ids(&h.state().decode), ["swd", "adiv5", "mem-ap"]);
    stepped(&mut h, "decoded", finished);
    let names: Vec<_> = counted(&h.state().decode)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(names, ["SWD", "ADIv5", "MEM-AP"]);
    assert!(counted(&h.state().decode).iter().all(|(_, n)| *n > 0));
    // The window derived again since, so it draws the analyses added too.
    stepped(&mut h, "the window drawn with MEM-AP", |d| {
        matches!(&d.lock().phase, Phase::Running(r) if r.chains[0].drawn.as_ref()
            .is_some_and(|c| r.host.window_run(c.id, r.lanes[2].id).is_some()))
    });

    h.get_by_label("SWD, ADIv5, MEM-AP").click();
    h.run_steps(2);
    h.get_by_label("add").click();
    h.run_steps(2);
    assert_eq!(
        ids(&h.state().decode),
        ["swd", "adiv5", "mem-ap", "swd-2", "adi", "mem"]
    );
    stepped(&mut h, "the preset decoded", finished);

    // ADIv5's remove control: the first of three, after SWD's.
    let removes = h.get_all_by_label("remove").collect::<Vec<_>>();
    assert_eq!(removes.len(), 6);
    removes[1].click();
    h.run_steps(2);
    stepped(&mut h, "bound again", |d| {
        matches!(d.progress(), Progress::Running(_))
    });
    assert_eq!(ids(&h.state().decode), ["swd", "swd-2", "adi", "mem"]);
}

/// After a setting changes, a window the new pass has not reached derives ahead of it, however
/// long, where the pass before counted few enough items there: provisional until the pass is
/// past it.
#[test]
fn a_window_ahead_of_a_pass_after_a_change_derives_on_the_count_before_it() {
    let ctx = egui::Context::default();
    let d = decoding(&ctx, SWD);
    until("the pass finishes", || finished(&d));
    let all = kept();
    let last = (all.store(all.index("swd").unwrap()).items().values())
        .last()
        .unwrap()
        .start;
    // A narrow window in ticks, which a slow axis makes seconds long: the rule for a window ahead
    // of the pass with nothing counted would wait for the pass.
    let asked = Asked {
        window: last - 20_000..last + 20_000,
        width: 1000.0,
        per_second: 1.0,
    };
    let mut g = d.lock();
    let Phase::Running(r) = &mut g.phase else {
        panic!("running");
    };
    let turnaround = [("turnaround".to_owned(), Setting::Int(1))];
    let estimated = |r: &crate::decode::Running| -> Vec<u64> {
        let count = |l: &crate::decode::Lane| l.estimate.as_ref().map_or(0, |e| e.items());
        r.lanes.iter().map(count).collect()
    };
    r.rebind(0, &turnaround).unwrap();
    assert!(r.lanes.iter().all(|l| l.tally.items() == 0));
    let full = estimated(r);
    assert!(full.iter().all(|&n| n > 0), "{full:?}");
    // Changed again before the new pass finishes: what it counted is cut short.
    r.rebind(0, &turnaround).unwrap();
    assert_eq!(estimated(r), full, "the finished pass's count stays");
    r.ask(&asked).unwrap();
    let pending = r.chains[0]
        .pending
        .as_ref()
        .expect("derived on the count before");
    assert!(pending.provisional, "ahead of the pass, without context");
    let derived = pending.id;
    // The whole recording over a few points, crowded by the count before: not derived, where the
    // new pass's empty count would have it.
    let crowded = Asked {
        window: 0..open().length(),
        width: 50.0,
        per_second: 1.0,
    };
    r.ask(&crowded).unwrap();
    assert_eq!(r.chains[0].pending.as_ref().map(|c| c.id), Some(derived));
    for l in &mut r.lanes {
        l.estimate = None;
    }
    r.rebind(0, &turnaround).unwrap();
    assert!(
        r.lanes.iter().all(|l| l.estimate.is_none()),
        "no finished pass, no estimate"
    );
    r.ask(&asked).unwrap();
    assert!(
        r.chains[0].pending.is_none(),
        "without it, the window waits for the pass"
    );
}

/// A setting changed while a window of a few transfers shows, on a recording decoded through: the
/// time from the change to the window drawn with the new pass's items (derived ahead of the pass,
/// provisional), to it drawn exact once the pass is past it, and to the pass's end. The change
/// rebinds the first analysis with the settings it has, so every round decodes the same. Run as
/// `frame_times` is: TICKWAVE_DECODE_REC and TICKWAVE_DECODE_RECIPE name a recording and its
/// recipe, the fixture and SWD where unset.
#[test]
#[ignore = "timing: run alone, under taskset"]
fn rebind_latency() {
    let real = Real::open();
    let whole = 0..real.length;
    cpu_cap();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1920.0, 600.0))
        .build_ui_state(
            |ui, v: &mut Stepped| {
                let palette = palette();
                v.ready = ready(v);
                let r = ui.add(
                    Graph::lanes_only(&v.viewer, &mut v.state, &palette)
                        .lanes(&v.lanes)
                        .decoded(&v.decode, &mut v.decoded)
                        .panel(Place::Beside(360.0)),
                );
                v.drawn = Graph::shown(&r).map(|s| (s, 0.0));
            },
            {
                let ctx = egui::Context::default();
                Stepped {
                    viewer: Viewer::open(&ctx, &real.path).unwrap(),
                    state: ViewState {
                        window: Window::At(whole.clone()),
                        ..ViewState::default()
                    },
                    decode: real.start(&ctx),
                    decoded: DecodeState::default(),
                    lanes: real.lanes.clone(),
                    drawn: None,
                    ready: false,
                }
            },
        );
    let mut next = Instant::now();
    let t = Instant::now();
    while !finished(&h.state().decode) {
        assert!(t.elapsed() < Duration::from_secs(300), "the pass finished");
        h.step();
        pace(&mut next);
    }
    let from = {
        let g = h.state().decode.lock();
        let Phase::Running(r) = &g.phase else {
            unreachable!("decoding");
        };
        first_after(&r.lanes[0].tally, real.length / 2, real.length).expect("an item")
    };
    let window = from..from + real.us(400.0);
    h.state_mut().state.window = Window::At(window.clone());
    while !h.state().ready {
        assert!(
            t.elapsed() < Duration::from_secs(300),
            "settled at the start"
        );
        h.step();
        pace(&mut next);
    }
    let settings = h.state().decode.recipe().analyses[0].settings.clone();
    let exact = |h: &Harness<'_, Stepped>| {
        let g = h.state().decode.lock();
        matches!(&g.phase, Phase::Running(r) if r.chains[0].drawn.as_ref().is_some_and(|c| !c.provisional))
    };
    let (mut shown, mut settled, mut replayed) = (vec![], vec![], vec![]);
    for _ in 0..3 {
        {
            let mut g = h.state().decode.lock();
            let Phase::Running(r) = &mut g.phase else {
                unreachable!("decoding");
            };
            r.rebind(0, &settings).unwrap();
        }
        h.state().decode.wake();
        let t = Instant::now();
        let (mut first, mut whole) = (None, None);
        loop {
            h.step();
            if first.is_none() && h.state().ready {
                first = Some(t.elapsed());
            }
            if whole.is_none() && h.state().ready && exact(&h) {
                whole = Some(t.elapsed());
            }
            if first.is_some() && whole.is_some() && finished(&h.state().decode) {
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(300), "decoded again");
            pace(&mut next);
        }
        shown.extend(first);
        settled.extend(whole);
        replayed.push(t.elapsed());
    }
    println!("to items in the window: {}", quantiles(&mut shown));
    println!("to the window exact:    {}", quantiles(&mut settled));
    println!("to the pass's end:      {}", quantiles(&mut replayed));
}
