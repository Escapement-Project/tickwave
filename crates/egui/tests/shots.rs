//! Screenshots of a graph with its strip, in the day and the night face, rendered through
//! egui_kittest's wgpu renderer:
//!
//!     TICKWAVE_SHOTS=DIR TICKWAVE_HOUR=hour-100k.tickwave \
//!         cargo test -p escapement-tickwave-egui --test shots -- --ignored
//!
//! `TICKWAVE_HOUR` is the synthetic hour crates/view/examples/synth.rs writes at 100,000 pairs a
//! second; without it the hour's pictures are left out. The adapter that rendered them is written
//! to `adapter.txt` beside them.

#[path = "../examples/demo/tokens.rs"]
mod tokens;

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use egui_kittest::Harness;
use escapement_tickwave_egui::{Graph, Strip, ViewState, Viewer, Window};

struct Host {
    viewer: Option<Viewer>,
    state: ViewState,
    face: tokens::Face,
    trace: usize,
    flag: Option<usize>,
    lanes: Vec<usize>,
}

const SIZE: egui::Vec2 = egui::vec2(1280.0, 720.0);
const STRIP: f32 = 64.0;
const SPACE: f32 = 8.0;

/// A harness showing `path` over `window`, its channels picked by name as the demo picks them.
fn harness(path: &Path, window: Window) -> Harness<'static, Host> {
    let face = tokens::face(false).unwrap();
    let host = Host {
        viewer: None,
        state: {
            let mut state = ViewState::default();
            state.window = window;
            state
        },
        face,
        trace: 0,
        flag: None,
        lanes: Vec::new(),
    };
    let mut h = Harness::builder().with_size(SIZE).wgpu().build_ui_state(
        |ui, host: &mut Host| {
            let Some(viewer) = &host.viewer else { return };
            ui.painter()
                .rect_filled(ui.ctx().content_rect(), 0.0, host.face.page);
            let graph = Graph::new(viewer, &mut host.state, &host.face.palette, host.trace)
                .motion(false)
                .flag(host.flag)
                .lanes(&host.lanes)
                .include_zero(true)
                .height(ui.available_height() - STRIP - SPACE);
            ui.add(graph);
            ui.add_space(SPACE);
            ui.add(
                Strip::new(viewer, &mut host.state, &host.face.palette)
                    .include_zero(true)
                    .height(STRIP),
            );
        },
        host,
    );
    let viewer = Viewer::open(&h.ctx, path).unwrap();
    let (trace, flag, lanes) = viewer.recording(|r| {
        let lanes = (0..r.channels().len())
            .filter(|&i| r.channels()[i].name.starts_with("DIO"))
            .collect();
        (r.channel("I").unwrap(), r.channel("under_range"), lanes)
    });
    let host = h.state_mut();
    (host.trace, host.flag, host.lanes) = (trace, flag, lanes);
    host.viewer = Some(viewer);
    h
}

/// Steps until the answer is for the window on screen, then draws it.
fn settle(h: &mut Harness<'_, Host>) {
    for _ in 0..2000 {
        h.step();
        let host = h.state();
        if let Some(a) = host.viewer.as_ref().unwrap().answer() {
            let window = host.state.window.resolve(a.extent.as_ref(), None);
            if a.window == window {
                h.step();
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no answer");
}

/// Renders the frame to `<name>-day.png` and `<name>-night.png` in `dir`.
fn shoot(h: &mut Harness<'_, Host>, dir: &Path, name: &str) {
    for night in [false, true] {
        h.state_mut().face = tokens::face(night).unwrap();
        h.ctx.set_visuals(tokens::visuals(&h.state().face));
        h.step();
        h.step();
        let face = if night { "night" } else { "day" };
        h.render()
            .unwrap()
            .save(dir.join(format!("{name}-{face}.png")))
            .unwrap();
    }
}

/// The plot's width in device pixels, from a frame drawn at the window's size: the columns a
/// query asks for.
fn plot_columns(path: &Path) -> i64 {
    let mut h = harness(path, ViewState::default().window);
    settle(&mut h);
    let columns = h
        .state()
        .viewer
        .as_ref()
        .unwrap()
        .answer()
        .unwrap()
        .grid
        .count
        / 3;
    columns as i64
}

#[test]
#[ignore = "renders screenshots with wgpu into TICKWAVE_SHOTS"]
fn shots() {
    let dir = PathBuf::from(std::env::var("TICKWAVE_SHOTS").expect("TICKWAVE_SHOTS"));
    std::fs::create_dir_all(&dir).unwrap();
    // The adapter the harness's renderer takes: the same choice, made again to name it.
    let state = egui_kittest::wgpu::create_render_state(
        egui_kittest::wgpu::default_wgpu_setup(),
        Default::default(),
    );
    let info = state.adapter.get_info();
    std::fs::write(dir.join("adapter.txt"), format!("{info:#?}\n")).unwrap();

    let energy =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/fixtures/energy.tickwave");
    let mut h = harness(&energy, ViewState::default().window);
    settle(&mut h);
    shoot(&mut h, &dir, "energy-whole");

    if let Ok(hour) = std::env::var("TICKWAVE_HOUR") {
        let hour = PathBuf::from(hour);
        let columns = plot_columns(&hour);
        // The finest level's buckets are 2^20 ticks: a column each at its edge, two a bucket
        // one step in. Both windows sit in the middle of the hour.
        for (name, per) in [
            ("hour-finest-edge", 1i64 << 20),
            ("hour-one-step-in", 1 << 19),
        ] {
            let width = columns * per;
            let middle = 1800 * 96_000_000;
            let window: Range<i64> = middle - width / 2..middle - width / 2 + width;
            let mut h = harness(&hour, Window::At(window));
            settle(&mut h);
            shoot(&mut h, &dir, name);
        }
    }
}

/// Pictures of a list of views, for a question a person settles by looking, as how a dense digital
/// channel reads at each zoom:
///
///     TICKWAVE_SHOTS=DIR TICKWAVE_VIEWS=LIST \
///         cargo test -p escapement-tickwave-egui --test shots -- --ignored views
///
/// LIST's lines, tab-separated: a name, a recording's path, the trace's channel (`-` for none,
/// where every channel is digital), the window in ticks as `from..to` (`whole` for the whole
/// recording), and the lanes' channels, comma-separated. Each is rendered at two device pixels a
/// point, as a page at a DPR of 2 draws, to `DIR/<name>.png`, with `DIR/<name>.txt` saying the
/// window, its ticks a device column and the face. Lines starting with `#` are skipped.
#[test]
#[ignore = "renders the listed views with wgpu into TICKWAVE_SHOTS"]
fn views() {
    let dir = PathBuf::from(std::env::var("TICKWAVE_SHOTS").expect("TICKWAVE_SHOTS"));
    let list = std::env::var("TICKWAVE_VIEWS").expect("TICKWAVE_VIEWS");
    std::fs::create_dir_all(&dir).unwrap();
    for line in std::fs::read_to_string(list).unwrap().lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let [name, path, trace, window, lanes] = line.split('\t').collect::<Vec<_>>()[..] else {
            panic!("not five fields: {line:?}");
        };
        let window = match window {
            "whole" => ViewState::default().window,
            w => {
                let (a, b) = w.split_once("..").expect("from..to");
                Window::At(a.parse().unwrap()..b.parse().unwrap())
            }
        };
        let lanes: Vec<&str> = lanes.split(',').filter(|s| !s.is_empty()).collect();
        let height = 64.0 + 24.0 * lanes.len() as f32 + if trace == "-" { 0.0 } else { 160.0 };
        let face = tokens::face(false).unwrap();
        let host = Host {
            viewer: None,
            state: {
                let mut state = ViewState::default();
                state.window = window;
                state
            },
            face,
            trace: 0,
            flag: None,
            lanes: Vec::new(),
        };
        let mut h = Harness::builder()
            .with_size(egui::vec2(SIZE.x, height))
            .with_pixels_per_point(2.0)
            .wgpu()
            .build_ui_state(
                |ui, host: &mut Host| {
                    let Some(viewer) = &host.viewer else { return };
                    ui.painter()
                        .rect_filled(ui.ctx().content_rect(), 0.0, host.face.page);
                    let graph = Graph::new(viewer, &mut host.state, &host.face.palette, host.trace)
                        .motion(false)
                        .flag(host.flag)
                        .lanes(&host.lanes)
                        .include_zero(true)
                        .height(ui.available_height());
                    ui.add(graph);
                },
                host,
            );
        let viewer = Viewer::open(&h.ctx, Path::new(path)).unwrap();
        let (trace, flag, lanes) = viewer.recording(|r| {
            let by = |n: &str| {
                r.channel(n)
                    .unwrap_or_else(|| panic!("{name}: no channel {n}"))
            };
            let lanes: Vec<usize> = lanes.iter().map(|n| by(n)).collect();
            let trace = if trace == "-" { lanes[0] } else { by(trace) };
            (trace, r.channel("under_range"), lanes)
        });
        let host = h.state_mut();
        (host.trace, host.flag, host.lanes) = (trace, flag, lanes);
        host.viewer = Some(viewer);
        h.ctx.set_visuals(tokens::visuals(&h.state().face));
        settle(&mut h);
        let answer = h.state().viewer.as_ref().unwrap().answer().unwrap();
        let w = &answer.window;
        std::fs::write(
            dir.join(format!("{name}.txt")),
            format!(
                "{path}\nwindow {}..{} ({} ticks), {} ticks a device column, day face\n",
                w.start,
                w.end,
                w.end - w.start,
                answer.grid.per
            ),
        )
        .unwrap();
        h.render()
            .unwrap()
            .save(dir.join(format!("{name}.png")))
            .unwrap();
    }
}

/// The faces as `site/tokens.css` states them, read by hand: the day's surface and ink, the
/// night's, and `--wash`, `color-mix(in srgb, var(--ink) 12%, transparent)`: the face's own ink at
/// 12 % alpha, premultiplied as egui keeps colours.
#[test]
fn the_demo_reads_each_face_from_the_site_tokens() {
    let (day, night) = (tokens::face(false).unwrap(), tokens::face(true).unwrap());
    assert_eq!(
        day.palette.surface,
        egui::Color32::from_rgb(0xee, 0xf4, 0xf6)
    );
    assert_eq!(day.ink, egui::Color32::from_rgb(0x11, 0x17, 0x1d));
    assert_eq!(
        night.palette.surface,
        egui::Color32::from_rgb(0x15, 0x1b, 0x21)
    );
    assert_eq!(
        night.palette.trace,
        egui::Color32::from_rgb(0x1a, 0xa3, 0x84)
    );
    let wash = |ink: [u8; 3]| {
        let [r, g, b] = ink.map(|c| (f32::from(c) * 0.12).round() as u8);
        egui::Color32::from_rgba_premultiplied(r, g, b, (255.0f32 * 0.12).round() as u8)
    };
    assert_eq!(day.palette.wash, wash([0x11, 0x17, 0x1d]));
    assert_eq!(night.palette.wash, wash([0xe6, 0xeb, 0xee]));
}
