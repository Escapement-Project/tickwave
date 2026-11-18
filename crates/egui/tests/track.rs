//! Decode tracks under a graph of SWCLK and SWDIO, on tickwave's own SWD capture: SWD and ADIv5
//! decoded through the wire source, drawn zoomed out, at the transfers' scale and at the bits'.
//!
//!     TICKWAVE_SHOTS=DIR cargo test -p escapement-tickwave-egui --features decode --test track \
//!         -- --ignored
//!
//! writes the pictures to DIR, in the day and the night face.

#![cfg(feature = "decode")]

#[path = "../examples/demo/tokens.rs"]
mod tokens;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui_kittest::Harness;
use escapement_tickwave_decode::{
    Host, InputSpec, Schema, Span, Store, Tick, name, wire::WireSource,
};
use escapement_tickwave_decoders::{adiv5, swd};
use escapement_tickwave_egui::track::{
    Density, Labels, Template, Track, condition, decoder_roles, driver_paints,
};
use escapement_tickwave_egui::{Graph, ViewState, Viewer, Window};
use escapement_tickwave_view::cursor::timeline_map;
use escapement_tickwave_view::format::mapping::Mapping;
use escapement_tickwave_view::{Cache, Recording};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/fixtures/identify-swd.tickwave")
}

/// One analysis as a track draws it.
struct Decoded {
    name: &'static str,
    schema: Schema,
    store: Store,
    density: Density,
    labels: Labels,
    parts: bool,
    lanes: usize,
}

/// SWD and ADIv5 over the recording's SWCLK and SWDIO, every item kept.
fn decode(path: &Path) -> (Vec<Decoded>, Mapping, [usize; 2]) {
    let file = std::fs::File::open(path).unwrap();
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(16 << 20)).unwrap());
    let lanes = [rec.channel("SWCLK").unwrap(), rec.channel("SWDIO").unwrap()];
    let map = timeline_map(&rec, lanes[0]).unwrap();
    let source = WireSource::new(rec.clone(), lanes.to_vec()).unwrap();
    let all = vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }];
    let mut h = Host::default();
    let s = h
        .add(
            name("SWD"),
            swd::definition(),
            InputSpec::Wire {
                source: Box::new(source),
                local: None,
            },
            swd::Swd::factory(1 << 16, 1).unwrap(),
            all.clone(),
        )
        .unwrap();
    let a = h
        .add(
            name("ADIv5"),
            adiv5::adi_definition(),
            InputSpec::Upstream(adiv5::adi_subscription(s)),
            adiv5::Adi::factory(),
            all,
        )
        .unwrap();
    h.run_until(|_| false).unwrap();
    let t = |pieces: &[Result<&'static str, escapement_tickwave_decode::FieldId>]| {
        pieces.iter().fold(Template::default(), |t, p| match p {
            Ok(text) => t.text(*text),
            Err(f) => t.field(*f),
        })
    };
    let swd_labels = Labels::default().kind(
        swd::TRANSFER,
        vec![
            t(&[
                Err(swd::DIR),
                Ok(" "),
                Err(swd::PORT),
                Ok(" "),
                Err(swd::A),
                Ok(" "),
                Err(swd::ACK),
                Ok(" "),
                Err(swd::DATA),
            ]),
            t(&[
                Err(swd::DIR),
                Ok(" "),
                Err(swd::PORT),
                Ok(" "),
                Err(swd::A),
                Ok(" "),
                Err(swd::ACK),
            ]),
            t(&[Err(swd::PORT)]),
        ],
    );
    let dp = vec![
        t(&[Err(adiv5::DP_REG), Ok(" "), Err(adiv5::DP_VALUE)]),
        t(&[Err(adiv5::DP_REG)]),
    ];
    let ap = vec![
        t(&[
            Ok("AP "),
            Err(adiv5::AP_NAME),
            Ok(" "),
            Err(adiv5::AP_VALUE),
        ]),
        t(&[Err(adiv5::AP_NAME)]),
    ];
    let adi_labels = Labels::default()
        .kind(adiv5::DP_READ, dp.clone())
        .kind(adiv5::DP_WRITE, dp)
        .kind(adiv5::AP_READ, ap.clone())
        .kind(adiv5::AP_WRITE, ap);
    let mut out = vec![];
    // An AP read stays open until its data comes back, in the next read: two may overlap.
    for (x, n, labels, parts, lanes) in [
        (s, "SWD", swd_labels, true, 1),
        (a, "ADIv5", adi_labels, false, 2),
    ] {
        let store = h.store(x).clone();
        let density = Density::of(store.items.values(), &map, condition);
        out.push(Decoded {
            name: n,
            schema: h.definition(x).schema.clone(),
            store,
            density,
            labels,
            parts,
            lanes,
        });
    }
    (out, map, lanes)
}

struct Host2 {
    viewer: Option<Viewer>,
    state: ViewState,
    face: tokens::Face,
    lanes: Vec<usize>,
    decoded: Vec<Decoded>,
    map: Mapping,
    /// Where decoding has reached, for drawing a run under way.
    settled: Tick,
    heights: Vec<f32>,
    /// The window the graph drew last, for painting SWDIO by who drives it.
    shown: Option<escapement_tickwave_egui::Shown>,
}

/// A face whose levels are in `--series-verge`, as a page naming the series draws them.
fn series_face(night: bool) -> tokens::Face {
    let mut face = tokens::face(night).unwrap();
    face.palette.trace = Some(face.series);
    face
}

const SIZE: egui::Vec2 = egui::vec2(1280.0, 360.0);

fn harness(
    window: Window,
    decoded: Vec<Decoded>,
    map: Mapping,
    lanes: Vec<usize>,
) -> Harness<'static, Host2> {
    let face = series_face(false);
    let host = Host2 {
        viewer: None,
        state: {
            let mut s = ViewState::default();
            s.window = window;
            s
        },
        face,
        lanes,
        decoded,
        map,
        settled: Tick::MAX,
        heights: vec![],
        shown: None,
    };
    let mut h = Harness::builder().with_size(SIZE).wgpu().build_ui_state(
        |ui, host: &mut Host2| {
            let Some(viewer) = &host.viewer else { return };
            ui.painter()
                .rect_filled(ui.ctx().content_rect(), 0.0, host.face.page);
            // SWDIO painted by who drives each part of the transfers on screen.
            let swd = &host.decoded[0];
            let paints = host.shown.as_ref().map_or(vec![], |s| {
                driver_paints(
                    &swd.schema,
                    &swd.store.items,
                    &swd.density,
                    &host.map,
                    &s.window,
                    &["SWDIO".to_string()],
                )
            });
            let roles = decoder_roles(&swd.schema);
            let response = ui.add(
                Graph::lanes_only(viewer, &mut host.state, &host.face.palette)
                    .motion(false)
                    .lanes(&host.lanes)
                    .decoded_paint(&paints, &roles),
            );
            host.heights.clear();
            let Some(shown) = Graph::shown(&response) else {
                return;
            };
            host.shown = Some(shown.clone());
            for d in &host.decoded {
                let r = ui.add(
                    Track::new(
                        &shown,
                        d.name,
                        &d.schema,
                        &d.store.items,
                        &d.density,
                        &d.labels,
                        &host.face.palette,
                        host.map,
                    )
                    .settled(host.settled)
                    .parts(d.parts)
                    .lanes(d.lanes),
                );
                host.heights.push(r.rect.height());
            }
        },
        host,
    );
    let viewer = Viewer::open(&h.ctx, &fixture()).unwrap();
    h.state_mut().viewer = Some(viewer);
    h
}

fn settle(h: &mut Harness<'_, Host2>) {
    for _ in 0..2000 {
        h.step();
        let host = h.state();
        if let Some(a) = host.viewer.as_ref().unwrap().answer() {
            let window = host.state.window.resolve(a.extent.as_ref(), None);
            if a.window == window {
                h.step();
                h.step();
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no answer");
}

#[test]
fn a_tracks_rows_are_reserved_before_anything_is_decoded() {
    let (decoded, map, lanes) = decode(&fixture());
    let first = decoded[0].store.items.values().next().unwrap().start;
    let mut h = harness(ViewState::default().window, decoded, map, lanes.to_vec());
    settle(&mut h);
    let full = h.state().heights.clone();
    // The same tracks with nothing decoded yet past the first transfer's start.
    h.state_mut().settled = first;
    h.step();
    h.step();
    assert_eq!(h.state().heights, full);
    assert_eq!(full.len(), 2);
    assert_eq!(
        full[0], full[1],
        "SWD's parts row and ADIv5's second lane are reserved"
    );
}

#[test]
#[ignore = "writes pictures where TICKWAVE_SHOTS says"]
fn shots() {
    let Some(dir) = std::env::var_os("TICKWAVE_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let (decoded, map, lanes) = decode(&fixture());
    let transfers: Vec<(Tick, Tick)> = (decoded[0].store.items.values())
        .map(|i| (i.start, i.end.unwrap()))
        .collect();
    let axis = |t: Tick| map.axis_saturating(t);
    let mid = transfers.len() / 2;
    let views = [
        ("track-whole", ViewState::default().window),
        (
            "track-transfers",
            Window::At(axis(transfers[mid].0)..axis(transfers[mid + 6].1)),
        ),
        (
            "track-bits",
            Window::At(
                axis(transfers[mid].0)
                    ..axis(transfers[mid].0 + (transfers[mid].1 - transfers[mid].0) / 2),
            ),
        ),
    ];
    for (name, window) in views {
        let mut h = harness(window, decode(&fixture()).0, map, lanes.to_vec());
        settle(&mut h);
        for night in [false, true] {
            h.state_mut().face = series_face(night);
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
    let _ = decoded;
}

/// One frame of the SWD and ADIv5 tracks at each zoom, timed on this machine: the median of 101
/// frames, printed. A calibration for the instruction counts in crates/bench, not a gate.
#[test]
#[ignore = "prints wall-clock frame times"]
fn frame_times() {
    use escapement_tickwave_egui::{Palette, Shown};
    let (decoded, map, _) = decode(&fixture());
    let transfers: Vec<(Tick, Tick)> = (decoded[0].store.items.values())
        .map(|i| (i.start, i.end.unwrap()))
        .collect();
    let axis = |t: Tick| map.axis_saturating(t);
    let mid = transfers.len() / 2;
    let (first, last) = (transfers[0].0, transfers[transfers.len() - 1].1);
    let half = (transfers[mid].1 - transfers[mid].0) / 2;
    let zooms = [
        ("whole", axis(first)..axis(last)),
        (
            "transfers",
            axis(transfers[mid].0)..axis(transfers[mid + 6].1),
        ),
        (
            "bits",
            axis(transfers[mid].0)..axis(transfers[mid].0 + half),
        ),
    ];
    let palette = Palette::from_visuals(&egui::Visuals::default());
    for (name, window) in zooms {
        let ctx = egui::Context::default();
        let shown = Shown::new(window, 100.0..2000.0);
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(2000.0, 200.0),
            )),
            ..egui::RawInput::default()
        };
        let mut frame = || {
            ctx.run_ui(input(), |ui| {
                for d in &decoded {
                    ui.add(
                        Track::new(
                            &shown,
                            d.name,
                            &d.schema,
                            &d.store.items,
                            &d.density,
                            &d.labels,
                            &palette,
                            map,
                        )
                        .parts(d.parts)
                        .lanes(d.lanes),
                    );
                }
            })
        };
        for _ in 0..5 {
            frame();
        }
        let mut times: Vec<f64> = (0..101)
            .map(|_| {
                let t = std::time::Instant::now();
                std::hint::black_box(frame());
                t.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        times.sort_by(f64::total_cmp);
        println!("{name}: median {:.0} us a frame", times[50]);
    }
}
