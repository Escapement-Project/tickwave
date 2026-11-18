//! Decode tracks under a graph of SWCLK and SWDIO, on tickwave's own SWD capture: SWD and ADIv5
//! decoded through the wire source, drawn zoomed out, at the transfers' scale and at the bits'.
//! The shots add JTAG on tickwave's own JTAG capture, and cJTAG with JTAG on it where
//! `TICKWAVE_SHOTS_CJTAG` names a recording of TCKC and TMSC.
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
    AnalysisId, Host, Id, InputSpec, Ref, Schema, Span, Store, Tick, Value, name, wire::WireSource,
};
use escapement_tickwave_decoders::{adiv5, cjtag, jtag, swd};
use escapement_tickwave_egui::panel::{Panel, Selected};
use escapement_tickwave_egui::track::{
    Density, Labels, Track, condition, decoder_roles, driver_paints, value_text,
};
use escapement_tickwave_egui::{Graph, ViewState, Viewer, Window};
use escapement_tickwave_view::cursor::timeline_map;
use escapement_tickwave_view::format::mapping::Mapping;
use escapement_tickwave_view::format::model::Ratio;
use escapement_tickwave_view::readouts::format_interval;
use escapement_tickwave_view::{Cache, Recording};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/fixtures/identify-swd.tickwave")
}

fn jtag_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/fixtures/identify-jtag.tickwave")
}

fn all() -> Vec<Span> {
    vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]
}

fn open(path: &Path) -> Arc<Recording> {
    let file = std::fs::File::open(path).unwrap();
    Arc::new(Recording::open(Arc::new(file), Cache::new(16 << 20)).unwrap())
}

/// A recording, the lanes its graph shows, and the analyses drawn under it.
struct Case {
    path: PathBuf,
    lanes: Vec<usize>,
    decoded: Vec<Decoded>,
    map: Mapping,
    /// The channels the first analysis's parts paint by who drives them.
    painted: Vec<String>,
    size: egui::Vec2,
    /// Drawn as a provisional window's.
    provisional: bool,
    /// The item selected, by the index of its analysis, and shown in a side panel.
    selected: Option<(usize, Id)>,
}

fn swd_case() -> Case {
    let (decoded, map, lanes) = decode(&fixture());
    Case {
        path: fixture(),
        lanes: lanes.to_vec(),
        decoded,
        map,
        painted: vec!["SWDIO".into()],
        size: SIZE,
        provisional: false,
        selected: None,
    }
}

/// One analysis's store as a track draws it.
fn decoded(
    h: &Host,
    a: AnalysisId,
    name: &'static str,
    labels: Labels,
    map: &Mapping,
    parts: bool,
) -> Decoded {
    let store = h.store(a).clone();
    let density = Density::of(store.items.values(), map, condition);
    Decoded {
        name,
        schema: h.definition(a).schema.clone(),
        store,
        density,
        labels,
        parts,
    }
}

/// JTAG over TCK, TMS, TDI and, where recorded, TDO.
fn jtag_case(path: &Path) -> Case {
    let rec = open(path);
    let mut lanes: Vec<usize> = ["TCK", "TMS", "TDI"]
        .map(|n| rec.channel(n).unwrap())
        .to_vec();
    let tdo = rec.channel("TDO");
    lanes.extend(tdo);
    let map = timeline_map(&rec, lanes[0]).unwrap();
    let ports = (lanes[..3].iter().copied().map(Some)).chain([tdo]);
    let source = WireSource::with_ports(rec.clone(), ports.collect()).unwrap();
    let mut h = Host::default();
    let j = h
        .add(
            name("JTAG"),
            jtag::definition(),
            InputSpec::Wire {
                source: Box::new(source),
                local: None,
            },
            jtag::Jtag::factory(50_000),
            &[],
            all(),
        )
        .unwrap();
    h.run_until(|_| false).unwrap();
    Case {
        path: path.to_path_buf(),
        lanes,
        decoded: vec![decoded(&h, j, "JTAG", Labels::default(), &map, false)],
        map,
        painted: vec![],
        size: SIZE,
        provisional: false,
        selected: None,
    }
}

/// cJTAG over TCKC and TMSC with JTAG on it, decoded until `ahead` ticks past the first scan
/// format, so a long recording keeps its first activation's items only.
fn cjtag_case(path: &Path, ahead: Tick) -> Case {
    let rec = open(path);
    let lanes = vec![rec.channel("TCKC").unwrap(), rec.channel("TMSC").unwrap()];
    let map = timeline_map(&rec, lanes[0]).unwrap();
    let source = WireSource::new(rec.clone(), lanes.clone()).unwrap();
    let mut h = Host::default();
    let c = h
        .add(
            name("cJTAG"),
            cjtag::definition(),
            InputSpec::Wire {
                source: Box::new(source),
                local: None,
            },
            cjtag::Cjtag::factory(1_000_000),
            &[cjtag::assume_qualified(true)],
            all(),
        )
        .unwrap();
    let j = h
        .add(
            name("JTAG"),
            cjtag::jtag_definition(),
            InputSpec::Upstream(cjtag::jtag_subscription(c)),
            cjtag::JtagOnCjtag::factory(),
            &[],
            all(),
        )
        .unwrap();
    let mut until: Tick = 0;
    let format = loop {
        until += 1_000_000;
        h.run_until(|h| h.settled(j) >= until).unwrap();
        let first = (h.store(c).items.values()).find(|i| i.id.kind == cjtag::FORMAT);
        if let Some(i) = first {
            break i.start;
        }
        assert!(h.finished(c).is_none(), "no scan format in the recording");
    };
    h.run_until(|h| h.settled(j) >= format + ahead).unwrap();
    let labels = Labels::default();
    Case {
        path: path.to_path_buf(),
        lanes,
        decoded: vec![
            decoded(&h, c, "cJTAG", labels, &map, true),
            decoded(&h, j, "JTAG", Labels::default(), &map, false),
        ],
        map,
        painted: vec!["TMSC".into()],
        size: egui::vec2(1280.0, 480.0),
        provisional: false,
        selected: None,
    }
}

/// One analysis as a track draws it.
struct Decoded {
    name: &'static str,
    schema: Schema,
    store: Store,
    density: Density,
    labels: Labels,
    parts: bool,
}

/// SWD and ADIv5 over the recording's SWCLK and SWDIO, every item kept.
fn decode(path: &Path) -> (Vec<Decoded>, Mapping, [usize; 2]) {
    decode_with(path, false)
}

/// `decode`, and MEM-AP over ADIv5 where `mem`.
fn decode_with(path: &Path, mem: bool) -> (Vec<Decoded>, Mapping, [usize; 2]) {
    let file = std::fs::File::open(path).unwrap();
    let rec = Arc::new(Recording::open(Arc::new(file), Cache::new(16 << 20)).unwrap());
    let lanes = [rec.channel("SWCLK").unwrap(), rec.channel("SWDIO").unwrap()];
    let map = timeline_map(&rec, lanes[0]).unwrap();
    let source = WireSource::new(rec.clone(), lanes.to_vec()).unwrap();
    let mut h = Host::default();
    let s = h
        .add(
            name("SWD"),
            swd::definition(),
            InputSpec::Wire {
                source: Box::new(source),
                local: None,
            },
            swd::Swd::factory(1 << 16),
            &[],
            all(),
        )
        .unwrap();
    let a = h
        .add(
            name("ADIv5"),
            adiv5::adi_definition(),
            InputSpec::Upstream(adiv5::adi_subscription(s)),
            adiv5::Adi::factory(),
            &[],
            all(),
        )
        .unwrap();
    let m = mem.then(|| {
        h.add(
            name("MEM-AP"),
            adiv5::mem_definition(),
            InputSpec::Upstream(adiv5::mem_subscription(a)),
            adiv5::Mem::factory(),
            &[],
            all(),
        )
        .unwrap()
    });
    h.run_until(|_| false).unwrap();
    // The lanes each level draws in come from its schema: ADIv5's AP reads may overlap.
    let mut out = vec![
        decoded(&h, s, "SWD", Labels::default(), &map, true),
        decoded(&h, a, "ADIv5", Labels::default(), &map, false),
    ];
    if let Some(m) = m {
        out.push(decoded(&h, m, "MEM-AP", Labels::default(), &map, false));
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
    painted: Vec<String>,
    provisional: bool,
    /// Where decoding has reached, for drawing a run under way.
    settled: Tick,
    heights: Vec<f32>,
    /// The window the graph drew last, for painting SWDIO by who drives it.
    shown: Option<escapement_tickwave_egui::Shown>,
    selected: Option<(usize, Id)>,
    timebase: Ratio,
}

/// A face whose levels are in `--series-verge`, as a page naming the series draws them.
fn series_face(night: bool) -> tokens::Face {
    let mut face = tokens::face(night).unwrap();
    face.palette.trace = Some(face.series);
    face
}

const SIZE: egui::Vec2 = egui::vec2(1280.0, 360.0);

fn harness(window: Window, case: Case) -> Harness<'static, Host2> {
    let Case {
        path,
        lanes,
        decoded,
        map,
        painted,
        size,
        provisional,
        selected,
    } = case;
    let face = series_face(false);
    let timebase = open(&path).timebase();
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
        painted,
        provisional,
        settled: Tick::MAX,
        heights: vec![],
        shown: None,
        selected,
        timebase,
    };
    let mut h = Harness::builder().with_size(size).wgpu().build_ui_state(
        |ui, host: &mut Host2| {
            ui.painter()
                .rect_filled(ui.ctx().content_rect(), 0.0, host.face.page);
            if host.selected.is_none() {
                draw(ui, host);
                return;
            }
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - PANEL - ui.spacing().item_spacing.x);
                    draw(ui, host);
                });
                let selected = host.selected.and_then(|(k, id)| {
                    let d = &host.decoded[k];
                    let item = d.store.items.get(&id)?;
                    let upstream = k
                        .checked_sub(1)
                        .map(|u| (host.decoded[u].name, &host.decoded[u].schema));
                    Some(Selected {
                        analysis: d.name,
                        schema: &d.schema,
                        labels: &d.labels,
                        item,
                        upstream,
                    })
                });
                ui.add(
                    Panel::new(&host.face.palette, PANEL, host.timebase, host.map)
                        .selected(selected),
                );
            });
        },
        host,
    );
    let viewer = Viewer::open(&h.ctx, &path).unwrap();
    h.state_mut().viewer = Some(viewer);
    h
}

/// The width of the side panel.
const PANEL: f32 = 360.0;

/// The graph and the tracks under it; a click on a track selects the item under it.
fn draw(ui: &mut egui::Ui, host: &mut Host2) {
    let Some(viewer) = &host.viewer else { return };
    // The data line painted by who drives each part of the items on screen.
    let first = &host.decoded[0];
    let paints = host.shown.as_ref().map_or(vec![], |s| {
        driver_paints(
            &first.schema,
            &first.store.items,
            &first.density,
            &host.map,
            &s.window,
            s.plot.end - s.plot.start,
        )
    });
    let roles = decoder_roles(&first.schema);
    let response = ui.add(
        Graph::lanes_only(viewer, &mut host.state, &host.face.palette)
            .motion(false)
            .lanes(&host.lanes)
            .decoded_paint(&paints, &host.painted, &roles),
    );
    host.heights.clear();
    let Some(shown) = Graph::shown(&response) else {
        return;
    };
    host.shown = Some(shown.clone());
    for (k, d) in host.decoded.iter().enumerate() {
        let selected = host.selected.filter(|(s, _)| *s == k).map(|(_, id)| id);
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
            .provisional(host.provisional)
            .parts(d.parts)
            .selected(selected),
        );
        if let Some(id) = Track::clicked(&r) {
            host.selected = Some((k, id));
        }
        host.heights.push(r.rect.height());
    }
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
    let case = swd_case();
    let first = case.decoded[0].store.items.values().next().unwrap().start;
    let mut h = harness(ViewState::default().window, case);
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
fn a_bool_reads_as_a_bit_and_a_check_by_its_name() {
    let tms = value_text(
        &cjtag::schema(),
        cjtag::PACKET,
        cjtag::PACKET_TMS,
        &Value::Bool(true),
    );
    let tdo = value_text(
        &cjtag::schema(),
        cjtag::PACKET,
        cjtag::PACKET_TDO,
        &Value::Bool(false),
    );
    assert_eq!((tms.as_str(), tdo.as_str()), ("1", "0"));
    let parity = |n| value_text(&swd::schema(), swd::TRANSFER, swd::PARITY, &Value::Name(n));
    assert_eq!(parity(swd::PARITY_OK), "ok");
    assert_eq!(parity(swd::PARITY_BAD), "bad");
}

#[test]
#[ignore = "writes pictures where TICKWAVE_SHOTS says"]
fn shots() {
    let Some(dir) = std::env::var_os("TICKWAVE_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let case = swd_case();
    let transfers: Vec<(Tick, Tick)> = (case.decoded[0].store.items.values())
        .map(|i| (i.start, i.end.unwrap()))
        .collect();
    let axis = |t: Tick| case.map.axis_saturating(t);
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
    shoot(&dir, swd_case, &views);
    // The same as a provisional window's: decoded from a local start without earlier context.
    let provisional = || Case {
        provisional: true,
        ..swd_case()
    };
    shoot(
        &dir,
        provisional,
        &[
            ("track-provisional-whole", views[0].1.clone()),
            ("track-provisional-transfers", views[1].1.clone()),
        ],
    );

    // A transfer selected, beside the graph, with its acknowledgement opened; then the ADIv5
    // access it carries.
    let case = swd_case();
    let ids: Vec<Id> = case.decoded[0].store.items.keys().copied().collect();
    let transfer = ids[mid + 2];
    let access = (case.decoded[1].store.items.values())
        .find(|i| (i.links.iter()).any(|(_, r)| *r == Ref::Upstream(transfer)))
        .map(|i| i.id);
    let panel = |selected| {
        move || Case {
            selected: Some(selected),
            size: egui::vec2(1280.0, 420.0),
            ..swd_case()
        }
    };
    shoot_opened(
        &dir,
        panel((0, transfer)),
        &[("panel-transfer", views[1].1.clone())],
        &["ack  OK"],
    );
    shoot(
        &dir,
        panel((1, access.unwrap())),
        &[("panel-access", views[1].1.clone())],
    );

    // JTAG: an IR scan and the DR scan after it, with the states they shift in.
    let case = jtag_case(&jtag_fixture());
    let axis = |t: Tick| case.map.axis_saturating(t);
    let items = &case.decoded[0].store.items;
    let ir = (items.values())
        .find(|i| i.id.kind == jtag::IR_SCAN)
        .unwrap();
    let dr = (items.values())
        .find(|i| i.id.kind == jtag::DR_SCAN && i.start > ir.start)
        .unwrap();
    let (a, b) = (ir.start, dr.end.unwrap());
    let views = [
        ("jtag-whole", ViewState::default().window),
        (
            "jtag-scans",
            Window::At(axis(a - (b - a) / 8)..axis(b + (b - a) / 8)),
        ),
        (
            "jtag-ir",
            Window::At(axis(ir.start - 2_000)..axis(ir.end.unwrap() + 2_000)),
        ),
    ];
    shoot(&dir, || jtag_case(&jtag_fixture()), &views);

    // cJTAG: the first activation, its control level and the Scan Packets after it.
    let Some(rec) = std::env::var_os("TICKWAVE_SHOTS_CJTAG").map(PathBuf::from) else {
        return;
    };
    const AHEAD: Tick = 2_000_000;
    let case = cjtag_case(&rec, AHEAD);
    let axis = |t: Tick| case.map.axis_saturating(t);
    let items = &case.decoded[0].store.items;
    let first = |k| (items.values()).find(|i| i.id.kind == k).unwrap();
    let (escape, level, format) = (
        first(cjtag::ESCAPE),
        first(cjtag::LEVEL),
        first(cjtag::FORMAT),
    );
    let packets: Vec<_> = (items.values())
        .filter(|i| i.id.kind == cjtag::PACKET && i.start >= format.start)
        .take(12)
        .collect();
    let views = [
        (
            "cjtag-activation",
            Window::At(axis(escape.start)..axis(packets[0].start)),
        ),
        (
            "cjtag-level",
            Window::At(axis(level.start)..axis(level.end.unwrap())),
        ),
        (
            "cjtag-packets",
            Window::At(axis(packets[0].start)..axis(packets[11].end.unwrap())),
        ),
    ];
    shoot(&dir, || cjtag_case(&rec, AHEAD), &views);
}

/// Each view of the case `make` builds, in the day and the night face.
fn shoot(dir: &Path, make: impl Fn() -> Case, views: &[(&str, Window)]) {
    shoot_opened(dir, make, views, &[]);
}

/// `shoot`, with the panel's sections titled `opened` open.
fn shoot_opened(dir: &Path, make: impl Fn() -> Case, views: &[(&str, Window)], opened: &[&str]) {
    use egui_kittest::kittest::Queryable;
    for (name, window) in views {
        let mut h = harness(window.clone(), make());
        settle(&mut h);
        for title in opened {
            h.get_by_label(title).click();
            h.step();
            h.step();
        }
        h.event(egui::Event::PointerGone);
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
}

/// One frame of the SWD and ADIv5 tracks at each zoom, its shapes built and tessellated, timed on
/// this machine: the median of 101 frames, printed. A calibration for the instruction counts in
/// crates/bench, not a gate.
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
        let frame = || {
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
                        .parts(d.parts),
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
                let out = frame();
                std::hint::black_box(ctx.tessellate(out.shapes, out.pixels_per_point));
                t.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        times.sort_by(f64::total_cmp);
        println!("{name}: median {:.0} us a frame", times[50]);
    }
}

/// One whole frame as a host draws it: the graph of SWCLK and SWDIO, SWDIO painted by who drives
/// it, and the SWD, ADIv5 and MEM-AP tracks under it, its shapes built and tessellated, at each
/// zoom, its answer landed: the median of 101 frames, printed. A frame's time on this machine,
/// against the ×4 target of 16.7 ms and the unrelaxed 4.17 ms.
#[test]
#[ignore = "prints wall-clock frame times"]
fn graph_frame_times() {
    let (decoded, map, lanes) = decode_with(&fixture(), true);
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
    let face = series_face(false);
    let painted = vec!["SWDIO".to_owned()];
    let roles = decoder_roles(&decoded[0].schema);
    for (name, window) in zooms {
        let ctx = egui::Context::default();
        let viewer = Viewer::open(&ctx, &fixture()).unwrap();
        let mut state = ViewState::default();
        state.window = Window::At(window.clone());
        let mut shown: Option<escapement_tickwave_egui::Shown> = None;
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SIZE)),
            ..egui::RawInput::default()
        };
        let mut frame = |state: &mut ViewState| {
            ctx.run_ui(input(), |ui| {
                let first = &decoded[0];
                let paints = shown.as_ref().map_or(vec![], |s| {
                    driver_paints(
                        &first.schema,
                        &first.store.items,
                        &first.density,
                        &map,
                        &s.window,
                        s.plot.end - s.plot.start,
                    )
                });
                let response = ui.add(
                    Graph::lanes_only(&viewer, state, &face.palette)
                        .motion(false)
                        .lanes(&lanes)
                        .decoded_paint(&paints, &painted, &roles),
                );
                let Some(s) = Graph::shown(&response) else {
                    return;
                };
                for d in &decoded {
                    ui.add(
                        Track::new(
                            &s,
                            d.name,
                            &d.schema,
                            &d.store.items,
                            &d.density,
                            &d.labels,
                            &face.palette,
                            map,
                        )
                        .settled(Tick::MAX)
                        .parts(d.parts),
                    );
                }
                shown = Some(s.clone());
            })
        };
        let mut landed = false;
        for _ in 0..2000 {
            frame(&mut state);
            if let Some(a) = viewer.answer()
                && a.window == state.window.resolve(a.extent.as_ref(), None)
                && !viewer.busy()
            {
                landed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(landed, "no answer for {name}");
        for _ in 0..5 {
            frame(&mut state);
        }
        let mut times: Vec<f64> = (0..101)
            .map(|_| {
                let t = std::time::Instant::now();
                let out = frame(&mut state);
                std::hint::black_box(ctx.tessellate(out.shapes, out.pixels_per_point));
                t.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        times.sort_by(f64::total_cmp);
        println!("{name}: median {:.0} us a frame", times[50]);
    }
}

/// One frame of the cJTAG and JTAG tracks over a real recording, zoomed in on six Scan Packets
/// and on one, inside the control level open across them: the median of 101 frames, printed.
/// `TICKWAVE_SHOTS_CJTAG` names the recording, decoded 10^9 ticks past its first scan format.
#[test]
#[ignore = "prints wall-clock frame times"]
fn cjtag_frame_times() {
    use escapement_tickwave_egui::{Palette, Shown};
    let Some(rec) = std::env::var_os("TICKWAVE_SHOTS_CJTAG").map(PathBuf::from) else {
        return;
    };
    let case = cjtag_case(&rec, 1_000_000_000);
    let axis = |t: Tick| case.map.axis_saturating(t);
    let packets: Vec<(Tick, Tick)> = (case.decoded[0].store.items.values())
        .filter(|i| i.id.kind == cjtag::PACKET)
        .filter_map(|i| Some((i.start, i.end?)))
        .collect();
    let n = packets.len();
    let held: u64 = case.decoded.iter().map(|d| d.density.items()).sum();
    println!("{n} packets, {held} items in the stores");
    let zooms = [
        (
            "six packets",
            axis(packets[n - 7].0)..axis(packets[n - 2].1),
        ),
        ("one packet", axis(packets[n - 2].0)..axis(packets[n - 2].1)),
    ];
    let palette = Palette::from_visuals(&egui::Visuals::default());
    for (name, window) in zooms {
        let ctx = egui::Context::default();
        let shown = Shown::new(window, 100.0..2000.0);
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(2000.0, 300.0),
            )),
            ..egui::RawInput::default()
        };
        let frame = || {
            ctx.run_ui(input(), |ui| {
                for d in &case.decoded {
                    ui.add(
                        Track::new(
                            &shown,
                            d.name,
                            &d.schema,
                            &d.store.items,
                            &d.density,
                            &d.labels,
                            &palette,
                            case.map,
                        )
                        .settled(Tick::MAX)
                        .parts(d.parts),
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
                let out = frame();
                std::hint::black_box(ctx.tessellate(out.shapes, out.pixels_per_point));
                t.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        times.sort_by(f64::total_cmp);
        println!("{name}: median {:.0} us a frame", times[50]);
    }
}

/// A panel showing `item`, of the analysis `decoded[k]`, stacked on `decoded[k - 1]`.
fn panel<'a>(
    decoded: &'a [Decoded],
    k: usize,
    item: &'a escapement_tickwave_decode::Item,
    map: Mapping,
    path: &Path,
) -> Harness<'a> {
    let timebase = open(path).timebase();
    let palette = escapement_tickwave_egui::Palette::from_visuals(&egui::Visuals::default());
    let d = &decoded[k];
    let upstream = k
        .checked_sub(1)
        .map(|u| (decoded[u].name, &decoded[u].schema));
    let mut h = Harness::builder()
        .with_size(egui::vec2(400.0, 900.0))
        .build_ui(move |ui| {
            let selected = Selected {
                analysis: d.name,
                schema: &d.schema,
                labels: &d.labels,
                item,
                upstream,
            };
            ui.add(Panel::new(&palette, 360.0, timebase, map).selected(Some(selected)));
        });
    h.run();
    h
}

#[test]
fn the_panel_shows_a_selected_transfer_in_full_and_an_access_with_its_links() {
    use egui_kittest::kittest::Queryable;
    let (decoded, map, _) = decode(&fixture());
    let transfer = (decoded[0].store.items.values())
        .find(|i| i.field(swd::DATA).is_some())
        .unwrap();
    let mut h = panel(&decoded, 0, transfer, map, &fixture());
    // Its fullest label: direction, port, address, acknowledgement and data.
    let text = |f| {
        let v = &transfer.field(f).unwrap().value;
        value_text(&decoded[0].schema, transfer.id.kind, f, v)
    };
    let fullest = [swd::DIR, swd::PORT, swd::A, swd::ACK, swd::DATA]
        .map(text)
        .join(" ");
    h.get_by_label(&fullest);
    h.get_by_label(&format!("SWD/transfer/{}", transfer.id.anchor));
    h.get_by_label_contains(" for ");
    h.get_by_label("complete");
    h.get_by_label("port");
    h.get_by_label("dir");
    h.get_by_label_contains("request  0x");
    h.get_by_label("trn");
    // A part opens to its span, who drives it and its bits, each at its time.
    assert!(h.query_by_label_contains("driven by").is_none());
    h.get_by_label_contains("data  0x").click();
    h.run();
    assert_eq!(h.query_all_by_label_contains(" for ").count(), 2);
    h.get_by_label("driven by target");
    assert_eq!(
        h.query_all_by_label("31").count(),
        1,
        "32 data bits, numbered"
    );
    assert!(h.query_by_label("32").is_none());
    // ADIv5 over it: an access, what it links to in SWD below, and the values it could not know.
    let access = (decoded[1].store.items.values())
        .find(|i| !i.links.is_empty())
        .unwrap();
    let h = panel(&decoded, 1, access, map, &fixture());
    h.get_by_label(&format!(
        "ADIv5/{}",
        escapement_tickwave_decode::citation(decoded[1].name, &decoded[1].schema, &access.id)
            .split_once('/')
            .unwrap()
            .1
    ));
    h.get_by_label_contains("SWD/transfer/");
    // JTAG on its own capture: the values a scan could not know, one sentence for each reason.
    let jtag = jtag_case(&jtag_fixture());
    let scan = (jtag.decoded[0].store.items.values())
        .find(|i| (i.fields.iter()).any(|f| matches!(f.value, Value::Unknown { .. })))
        .unwrap();
    let h = panel(&jtag.decoded, 0, scan, jtag.map, &jtag_fixture());
    h.get_by_label_contains(" unknown: ");
    // Its one label is the state it could not know, so it is headed by its kind.
    h.get_by_label("state");
}

#[test]
fn an_open_item_closing_moves_nothing_in_the_panel() {
    use egui_kittest::kittest::Queryable;
    let (decoded, map, _) = decode(&fixture());
    let closed = (decoded[0].store.items.values())
        .find(|i| i.field(swd::DATA).is_some())
        .unwrap();
    let mut still_open = closed.clone();
    (still_open.end, still_open.status) = (None, None);
    let cite = format!("SWD/transfer/{}", closed.id.anchor);
    let timebase = open(&fixture()).timebase();
    let start = format_interval(map.axis_saturating(closed.start), timebase, map.m());
    // The status a label of its own, and the start, the citation and the fields where they were.
    let places = |item, status: &str| {
        let h = panel(&decoded, 0, item, map, &fixture());
        let mut at = vec![h.get_by_label(status).rect().min];
        at.extend(
            [&start[..], &cite, "port", "data  0x"].map(|l| h.get_by_label_contains(l).rect().min),
        );
        at
    };
    assert_eq!(places(&still_open, "open"), places(closed, "complete"));
}
