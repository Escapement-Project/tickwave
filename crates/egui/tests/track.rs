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
use escapement_tickwave_egui::panel::{Analysis, Panel, PanelState};
use escapement_tickwave_egui::track::{
    self, Density, Labels, Track, condition, decoder_roles, driver_paints, value_text,
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
    let density = Density::of(store.items(), map, condition);
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
            jtag::definition(),
            InputSpec::Upstream(c),
            jtag::Jtag::factory(1 << 16),
            &[],
            all(),
        )
        .unwrap();
    let mut until: Tick = 0;
    let format = loop {
        until += 1_000_000;
        h.run_until(|h| h.settled(j) >= until).unwrap();
        let first = (h.store(c).items().values()).find(|i| i.id.kind == cjtag::FORMAT);
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
            InputSpec::Upstream(s),
            adiv5::Adi::factory(),
            &[],
            all(),
        )
        .unwrap();
    let m = mem.then(|| {
        h.add(
            name("MEM-AP"),
            adiv5::mem_definition(),
            InputSpec::Upstream(a),
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
    panel: PanelState,
    timebase: Ratio,
}

/// The analyses as a panel reads them, each stacked on the one before it.
fn analyses(decoded: &[Decoded]) -> Vec<Analysis<'_>> {
    (decoded.iter().enumerate())
        .map(|(k, d)| Analysis {
            name: d.name,
            schema: &d.schema,
            labels: &d.labels,
            store: &d.store,
            reads: k.checked_sub(1),
        })
        .collect()
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
        panel: {
            let mut p = PanelState::default();
            p.selected = selected;
            p
        },
        timebase,
    };
    let mut h = Harness::builder().with_size(size).wgpu().build_ui_state(
        |ui, host: &mut Host2| {
            ui.painter()
                .rect_filled(ui.ctx().content_rect(), 0.0, host.face.page);
            if host.panel.selected.is_none() {
                draw(ui, host);
                return;
            }
            ui.horizontal_top(|ui| {
                let reserved = Panel::reserved(&host.panel, PANEL);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - reserved - ui.spacing().item_spacing.x);
                    draw(ui, host);
                });
                let analyses = analyses(&host.decoded);
                ui.add(Panel::new(
                    &host.face.palette,
                    &mut host.panel,
                    &analyses,
                    PANEL,
                    host.timebase,
                    host.map,
                ));
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
            first.store.items(),
            &first.density,
            &host.map,
            &s.window,
            s.plot.end - s.plot.start,
        )
    });
    let roles = decoder_roles(&first.schema);
    let lit = (host.panel.lit)
        .map(|l| host.map.axis_saturating(l.start)..host.map.axis_saturating(l.end));
    let gutter = (host.decoded.iter())
        .map(|d| track::gutter(ui, &host.face.palette, d.name, &d.schema))
        .fold(0.0, f32::max);
    let response = ui.add(
        Graph::lanes_only(viewer, &mut host.state, &host.face.palette)
            .gutter(gutter)
            .lit(lit)
            .motion(false)
            .lanes(&host.lanes)
            .decoded_paint(&paints, &host.painted, &roles)
            .decoded_provisional(host.provisional),
    );
    host.heights.clear();
    let Some(shown) = Graph::shown(&response) else {
        return;
    };
    host.shown = Some(shown.clone());
    for (k, d) in host.decoded.iter().enumerate() {
        let selected = host
            .panel
            .selected
            .filter(|(s, _)| *s == k)
            .map(|(_, id)| id);
        let r = ui.add(
            Track::new(
                &shown,
                d.name,
                &d.schema,
                d.store.items(),
                &d.density,
                &d.labels,
                &host.face.palette,
                host.map,
            )
            .settled(host.settled)
            .provisional(host.provisional)
            .parts(d.parts)
            .selected(selected)
            .lit(host.panel.lit),
        );
        if let Some(id) = Track::clicked(&r) {
            host.panel.selected = Some((k, id));
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
    let first = case.decoded[0].store.items().values().next().unwrap().start;
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
fn a_value_reads_in_its_fields_notation() {
    use escapement_tickwave_decode::{
        FieldDecl, FieldId, FieldType, KindDecl, KindId, Notation, Schema,
    };
    let schema = Schema::new(vec![KindDecl::new("k").with_fields(vec![
        FieldDecl::new("address", FieldType::Word { bits: 12 }),
        FieldDecl::new("length", FieldType::Word { bits: 32 }).in_notation(Notation::Decimal),
        FieldDecl::new("mask", FieldType::Word { bits: 4 }).in_notation(Notation::Binary),
        FieldDecl::new("bits", FieldType::Bits { max: 16 }).in_notation(Notation::Binary),
        FieldDecl::new("bytes", FieldType::Bits { max: 16 }),
    ])]);
    let text = |f, v: &Value| value_text(&schema, KindId(0), FieldId(f), v);
    let word = |value, bits| Value::Word { value, bits };
    let bits = Value::Bits {
        bytes: vec![0b0000_0110, 0b1].into(),
        bits: 9,
    };
    assert_eq!(text(0, &word(0x2A, 12)), "0x02A");
    assert_eq!(text(1, &word(256, 32)), "256");
    assert_eq!(text(2, &word(0b0101, 4)), "0b0101");
    assert_eq!(
        text(3, &bits),
        "100000110",
        "the last bit first, as hex puts it"
    );
    assert_eq!(text(4, &bits), "0106");
    let length = value_text(&jtag::schema(), jtag::IR_SCAN, jtag::LENGTH, &word(4, 32));
    assert_eq!(length, "4", "a scan's length in decimal");
}

#[test]
#[ignore = "writes pictures where TICKWAVE_SHOTS says"]
fn shots() {
    let Some(dir) = std::env::var_os("TICKWAVE_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let case = swd_case();
    let transfers: Vec<(Tick, Tick)> = (case.decoded[0].store.items().values())
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
    let ids: Vec<Id> = case.decoded[0].store.items().keys().copied().collect();
    let transfer = ids[mid + 2];
    let access = (case.decoded[1].store.items().values())
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
    let items = &case.decoded[0].store.items();
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
    let items = &case.decoded[0].store.items();
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
    let transfers: Vec<(Tick, Tick)> = (decoded[0].store.items().values())
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
                            d.store.items(),
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
            frame().textures_delta.clear();
        }
        let mut times: Vec<f64> = (0..101)
            .map(|_| {
                let t = std::time::Instant::now();
                let mut out = frame();
                out.textures_delta.clear();
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
    let transfers: Vec<(Tick, Tick)> = (decoded[0].store.items().values())
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
                        first.store.items(),
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
                            d.store.items(),
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
            frame(&mut state).textures_delta.clear();
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
            frame(&mut state).textures_delta.clear();
        }
        let mut times: Vec<f64> = (0..101)
            .map(|_| {
                let t = std::time::Instant::now();
                let mut out = frame(&mut state);
                out.textures_delta.clear();
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
    let packets: Vec<(Tick, Tick)> = (case.decoded[0].store.items().values())
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
                            d.store.items(),
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
            frame().textures_delta.clear();
        }
        let mut times: Vec<f64> = (0..101)
            .map(|_| {
                let t = std::time::Instant::now();
                let mut out = frame();
                out.textures_delta.clear();
                std::hint::black_box(ctx.tessellate(out.shapes, out.pixels_per_point));
                t.elapsed().as_secs_f64() * 1e6
            })
            .collect();
        times.sort_by(f64::total_cmp);
        println!("{name}: median {:.0} us a frame", times[50]);
    }
}

/// A panel showing `item` as an item of the analysis `decoded[k]`, each analysis stacked on the
/// one before it. The panel's state is the harness's.
fn panel<'a>(
    decoded: &'a [Decoded],
    k: usize,
    item: &'a escapement_tickwave_decode::Item,
    map: Mapping,
    path: &Path,
) -> Harness<'a, PanelState> {
    placed(decoded, k, item, map, path, None)
}

/// The height of the stand-in for a plot above a panel under it.
const PLOT: f32 = 300.0;

/// The frames a panel test lets the panel read its stores in: a million items each, at
/// `READ_AT_MOST` ids a frame shared between two analyses.
const READ_FRAMES: u64 = 4096;

/// A store holding `items` as given, each replayed through its ops: an open one left open.
fn replayed<'a>(items: impl IntoIterator<Item = &'a escapement_tickwave_decode::Item>) -> Store {
    use escapement_tickwave_decode::{MAX_CHUNK, Op};
    let mut store = Store::with_keep(all());
    for i in items {
        let id = i.id;
        let mut ops = vec![Op::Open {
            id,
            parent: None,
            start: i.start,
        }];
        ops.extend(i.fields.iter().map(|f| Op::Set {
            id,
            field: f.field,
            value: f.value.clone(),
            span: f.span,
            driver: f.driver,
        }));
        for (seq, elements) in &i.seqs {
            for (n, chunk) in elements.chunks(MAX_CHUNK).enumerate() {
                ops.push(Op::Extend {
                    id,
                    seq: *seq,
                    offset: (n * MAX_CHUNK) as u32,
                    elements: chunk.to_vec(),
                });
            }
        }
        ops.extend((i.links.iter()).map(|&(link, other)| Op::Link { id, link, other }));
        if let (Some(end), Some(status)) = (i.end, i.status) {
            ops.push(Op::Close { id, end, status });
        }
        store.apply_batch(&ops, usize::MAX).unwrap();
    }
    store
}

/// As `panel`; with `under`, a 400-point-wide window whose top `PLOT` points stand in for a plot
/// and whose panel lies under it, that many points tall.
fn placed<'a>(
    decoded: &'a [Decoded],
    k: usize,
    item: &'a escapement_tickwave_decode::Item,
    map: Mapping,
    path: &Path,
    under: Option<f32>,
) -> Harness<'a, PanelState> {
    let timebase = open(path).timebase();
    let palette = escapement_tickwave_egui::Palette::from_visuals(&egui::Visuals::default());
    // The item as given, an open one in place of its closed self.
    let held = (decoded[k].store.items().get(&item.id)).is_some_and(|i| std::ptr::eq(i, item));
    let replaced = (!held).then(|| {
        let others = (decoded[k].store.items().values()).filter(|i| i.id != item.id);
        replayed(others.chain([item]))
    });
    let mut state = PanelState::default();
    state.selected = Some((k, item.id));
    let mut h = Harness::builder()
        .with_size(egui::vec2(400.0, 900.0))
        .with_max_steps(READ_FRAMES)
        .build_ui_state(
            move |ui, state: &mut PanelState| {
                let analyses: Vec<Analysis> = (decoded.iter().enumerate())
                    .map(|(j, d)| Analysis {
                        name: d.name,
                        schema: &d.schema,
                        labels: &d.labels,
                        store: replaced.as_ref().filter(|_| j == k).unwrap_or(&d.store),
                        reads: j.checked_sub(1),
                    })
                    .collect();
                let panel = Panel::new(&palette, state, &analyses, 360.0, timebase, map);
                match under {
                    Some(h) => {
                        let plot = egui::vec2(ui.available_width(), PLOT);
                        ui.allocate_exact_size(plot, egui::Sense::hover());
                        ui.add(panel.under(h));
                        ui.label("after the panel");
                    }
                    None => {
                        // Against the right edge, as a host beside its plot puts it.
                        let right = egui::Layout::right_to_left(egui::Align::Min);
                        ui.with_layout(right, |ui| ui.add(panel));
                    }
                }
            },
            state,
        );
    h.run();
    h
}

#[test]
fn the_panel_shows_a_selected_transfer_in_full_and_an_access_with_its_links() {
    use egui_kittest::kittest::Queryable;
    let (decoded, map, _) = decode(&fixture());
    let transfer = (decoded[0].store.items().values())
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
    // Each bit at its time from the part's start, which the line above gives in full.
    assert_eq!(h.query_all_by_label_contains("+").count(), 32);
    assert_eq!(
        h.query_all_by_label("31").count(),
        1,
        "32 data bits, numbered"
    );
    assert!(h.query_by_label("32").is_none());
    // ADIv5 over it: an access, what it links to in SWD below, and the values it could not know.
    let access = (decoded[1].store.items().values())
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
    let scan = (jtag.decoded[0].store.items().values())
        .find(|i| (i.fields.iter()).any(|f| matches!(f.value, Value::Unknown { .. })))
        .unwrap();
    let h = panel(&jtag.decoded, 0, scan, jtag.map, &jtag_fixture());
    h.get_by_label_contains(" unknown: ");
    // Its one label is the state it could not know, so it is headed by its kind.
    h.get_by_label("state");
}

/// An access's heading says whether it reads or writes, where the fixture holds one of its kind
/// with every value known.
#[test]
fn an_access_says_whether_it_reads_or_writes() {
    use egui_kittest::kittest::Queryable;
    let (decoded, map, _) = decode_with(&fixture(), true);
    let cases = [
        (1, adiv5::DP_READ, "DP read "),
        (1, adiv5::DP_WRITE, "DP write "),
        (1, adiv5::AP_READ, "AP read "),
        (1, adiv5::AP_WRITE, "AP write "),
        (2, adiv5::MEM_READ, "read 0x"),
        (2, adiv5::MEM_WRITE, "write 0x"),
    ];
    let mut seen = 0;
    for (k, kind, head) in cases {
        let Some(item) = (decoded[k].store.items().values())
            .find(|i| i.id.kind == kind && i.end.is_some() && !condition(i))
        else {
            continue;
        };
        seen += 1;
        // Only its heading holds the words: a link reads "ADIv5/ap-read/..." and a reader "read as".
        panel(&decoded, k, item, map, &fixture()).get_by_label_contains(head);
    }
    assert!(
        seen >= 4,
        "the fixture holds reads and writes of each layer"
    );
}

#[test]
fn an_open_item_closing_moves_nothing_in_the_panel() {
    use egui_kittest::kittest::Queryable;
    let (decoded, map, _) = decode(&fixture());
    let closed = (decoded[0].store.items().values())
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

fn transfers(decoded: &[Decoded]) -> Vec<&escapement_tickwave_decode::Item> {
    (decoded[0].store.items().values())
        .filter(|i| i.id.kind == swd::TRANSFER)
        .collect()
}

#[test]
fn previous_and_next_step_through_its_kind_and_stay_where_pressed() {
    use egui_kittest::kittest::Queryable;
    let (decoded, map, _) = decode(&fixture());
    let t = transfers(&decoded);
    let mut h = panel(&decoded, 0, t[1], map, &fixture());
    let next = "next transfer \u{25b8}";
    let previous = "\u{25c2} previous transfer";
    let at = h.get_by_label(next).rect();
    h.get_by_label(next).click();
    h.run();
    assert_eq!(h.state().selected, Some((0, t[2].id)));
    assert_eq!(h.get_by_label(next).rect(), at, "it stays where pressed");
    for want in [t[1], t[0], t[0]] {
        h.get_by_label(previous).click();
        h.run();
        assert_eq!(
            h.state().selected,
            Some((0, want.id)),
            "none before the first"
        );
    }
    // ADIv5's kinds interleave: next steps over the others to a DP write's next.
    let writes: Vec<Id> = (decoded[1].store.items().keys())
        .filter(|id| id.kind == adiv5::DP_WRITE)
        .copied()
        .collect();
    let between = (decoded[1].store.items().keys())
        .any(|id| id.kind != adiv5::DP_WRITE && writes[0] < *id && *id < writes[1]);
    assert!(between, "another kind lies between the first two DP writes");
    let first = &decoded[1].store.items()[&writes[0]];
    let mut h = panel(&decoded, 1, first, map, &fixture());
    h.get_by_label("next dp-write \u{25b8}").click();
    h.run();
    assert_eq!(h.state().selected, Some((1, writes[1])));
}

#[test]
fn the_collapse_control_stays_where_it_is_pressed() {
    use egui_kittest::kittest::Queryable;
    let (decoded, map, _) = decode(&fixture());
    let t = transfers(&decoded);
    let mut h = panel(&decoded, 0, t[0], map, &fixture());
    let at = h.get_by_label("\u{21e5}").rect();
    h.get_by_label("\u{21e5}").click();
    h.run();
    assert!(h.state().collapsed);
    assert_eq!(Panel::reserved(h.state(), 360.0), Panel::COLLAPSED);
    assert_eq!(h.get_by_label("\u{21e4}").rect(), at);
    assert!(
        h.query_by_label("complete").is_none(),
        "collapsed to its control"
    );
    h.get_by_label("\u{21e4}").click();
    h.run();
    assert!(!h.state().collapsed);
    h.get_by_label("complete");
}

#[test]
fn pointing_at_a_section_or_the_heading_lights_its_span() {
    use egui_kittest::kittest::Queryable;
    let (decoded, map, _) = decode(&fixture());
    let transfer = (decoded[0].store.items().values())
        .find(|i| i.field(swd::DATA).is_some())
        .unwrap();
    let mut h = panel(&decoded, 0, transfer, map, &fixture());
    assert_eq!(h.state().lit, None);
    h.get_by_label_contains("data  0x").hover();
    h.run();
    assert_eq!(h.state().lit, transfer.field(swd::DATA).unwrap().span);
    let fullest = [swd::DIR, swd::PORT, swd::A, swd::ACK, swd::DATA]
        .map(|f| {
            let v = &transfer.field(f).unwrap().value;
            value_text(&decoded[0].schema, transfer.id.kind, f, v)
        })
        .join(" ");
    h.get_by_label(&fullest).hover();
    h.run();
    let whole = Span {
        start: transfer.start,
        end: transfer.end.unwrap(),
    };
    assert_eq!(h.state().lit, Some(whole));
}

#[test]
fn read_as_lists_what_reads_it_and_a_link_selects_what_it_names() {
    use egui_kittest::kittest::Queryable;
    let (decoded, map, _) = decode(&fixture());
    let reads = |t: &escapement_tickwave_decode::Item| -> Vec<Id> {
        (decoded[1].store.items().values())
            .filter(|i| (i.links.iter()).any(|(_, r)| *r == Ref::Upstream(t.id)))
            .map(|i| i.id)
            .collect()
    };
    // A transfer a DP read reads, and nothing else.
    let transfer = (transfers(&decoded).into_iter())
        .find(|t| {
            let r = reads(t);
            r.len() == 1 && r[0].kind == adiv5::DP_READ
        })
        .unwrap();
    let reader = reads(transfer)[0];
    let mut h = panel(&decoded, 0, transfer, map, &fixture());
    assert_eq!(h.query_all_by_label_contains("read as ").count(), 1);
    h.get_by_label_contains("read as ").click();
    h.run();
    h.get_by_label("select it").click();
    h.run();
    assert_eq!(h.state().selected, Some((1, reader)));
    // The DP read's link to its transfer, in SWD below, selects the transfer again.
    h.get_by_label(&format!("SWD/transfer/{}", transfer.id.anchor))
        .click();
    h.run();
    assert_eq!(h.state().selected, Some((0, transfer.id)));
}

#[test]
fn the_list_shows_an_analysis_as_rows_filters_them_and_selects_one() {
    use egui_kittest::kittest::Queryable;
    use escapement_tickwave_egui::panel::Tab;
    let (decoded, map, _) = decode(&fixture());
    let timebase = open(&fixture()).timebase();
    let time = |i: &escapement_tickwave_decode::Item| {
        format!(
            "{}  ",
            format_interval(map.axis_saturating(i.start), timebase, map.m())
        )
    };
    let t = transfers(&decoded);
    let mut h = panel(&decoded, 0, t[0], map, &fixture());
    h.get_by_label("List").click();
    h.run();
    assert_eq!(h.state().tab, Tab::List);
    h.get_by_label_contains(&time(t[1]));
    // SWD's one kind goes unsaid.
    assert!(
        h.query_by_label_contains(&format!("{}transfer  ", time(t[1])))
            .is_none()
    );
    h.get_by_label_contains(&time(t[3])).click();
    h.run();
    assert_eq!(h.state().selected, Some((0, t[3].id)));
    h.get_by_label_contains(&time(t[2])).hover();
    h.run();
    let span = Span {
        start: t[2].start,
        end: t[2].end.unwrap(),
    };
    assert_eq!(h.state().lit, Some(span));
    // A selection far down scrolls the list to it.
    let last = *t.last().unwrap();
    assert!(h.query_by_label_contains(&time(last)).is_none());
    h.state_mut().selected = Some((0, last.id));
    h.run();
    let row = h.get_by_label_contains(&time(last)).rect();
    // Once there, the list scrolls as the reader scrolls it.
    h.event(egui::Event::PointerMoved(row.center()));
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, 2000.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Default::default(),
    });
    h.run_steps(60);
    assert!(h.query_by_label_contains(&time(last)).is_none());

    // ADIv5's rows, then without its DP writes, then only those holding a condition.
    h.get_by_label(decoded[1].name).click();
    h.run();
    assert_eq!(h.state().listed, Some(1));
    let items: Vec<_> = decoded[1].store.items().values().collect();
    let write = *(items.iter())
        .find(|i| i.id.kind == adiv5::DP_WRITE)
        .unwrap();
    h.get_by_label_contains(&time(write));
    h.get_by_label("dp-write").click();
    h.run();
    assert!(h.query_by_label_contains(&time(write)).is_none());
    let marked = (items.iter()).find(|i| escapement_tickwave_egui::track::condition(i));
    let plain = (items.iter())
        .find(|i| i.id.kind != adiv5::DP_WRITE && !escapement_tickwave_egui::track::condition(i))
        .unwrap();
    h.get_by_label_contains(&time(plain));
    h.get_by_label("conditions only").click();
    h.run();
    assert!(h.query_by_label_contains(&time(plain)).is_none());
    let marked = marked.expect("an ADIv5 item holding a condition");
    h.get_by_label_contains(&time(marked));
    h.get_by_label("Selected").click();
    h.run();
    assert_eq!(h.state().tab, Tab::Selected);

    // With nothing chosen, the list is the selected item's analysis, with its kinds to filter.
    let mut h = panel(&decoded, 1, write, map, &fixture());
    h.state_mut().tab = Tab::List;
    h.run();
    h.get_by_label("dp-write");
    // Each row says its kind while more than one is shown.
    h.get_by_label_contains(&format!("{}dp-write  ", time(write)));
    for other in ["context", "dp-read", "ap-write", "ap-read"] {
        h.get_by_label(other).click();
        h.run();
    }
    h.get_by_label_contains(&time(write));
    assert!(
        h.query_by_label_contains(&format!("{}dp-write  ", time(write)))
            .is_none()
    );
}

/// An analysis of one byte an item, its kind's text the byte, `None` a byte it could not know:
/// each byte 1000 ticks after the one before.
fn bytes_decoded(bytes: &[Option<u8>]) -> Decoded {
    let values = (bytes.iter())
        .map(|b| match b {
            Some(b) => Value::Word {
                value: u64::from(*b),
                bits: 8,
            },
            None => Value::Unknown {
                reason: escapement_tickwave_decode::Reason(0),
                needs: None,
            },
        })
        .collect();
    text_decoded(
        "bytes",
        escapement_tickwave_decode::FieldType::Word { bits: 8 },
        values,
    )
}

/// An analysis named `name` of one item a value, its kind's text a field of type `ty`: each
/// item 1000 ticks after the one before.
fn text_decoded(
    name: &'static str,
    ty: escapement_tickwave_decode::FieldType,
    values: Vec<Value>,
) -> Decoded {
    use escapement_tickwave_decode::{FieldId, KindDecl, KindId, Op, Status, field};
    let schema = Schema::new(vec![
        KindDecl::new("byte")
            .with_fields(vec![field("data", ty)])
            .with_text(FieldId(0)),
    ])
    .with_reasons(["a framing error"]);
    let mut store = Store::with_keep(all());
    for (k, value) in values.into_iter().enumerate() {
        let start = 1000 * (k as Tick + 1);
        let id = Id::new(KindId(0), start);
        let ops = [
            Op::Open {
                id,
                parent: None,
                start,
            },
            Op::Set {
                id,
                field: FieldId(0),
                value,
                span: None,
                driver: None,
            },
            Op::Close {
                id,
                end: start + 800,
                status: Status::Complete,
            },
        ];
        store.apply_batch(&ops, 8).unwrap();
    }
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(store.items(), &map, condition);
    Decoded {
        name,
        schema,
        store,
        density,
        labels: Labels::default(),
        parts: false,
    }
}

#[test]
fn the_text_tab_joins_an_analysis_bytes_into_lines_each_character_from_its_item() {
    use egui_kittest::kittest::Queryable;
    use escapement_tickwave_egui::panel::Tab;
    let text: Vec<Option<u8>> = (b"hi\r\nok\n".iter().map(|&b| Some(b)))
        .chain([None, Some(b'z'), Some(0x15)])
        .collect();
    let decoded = vec![bytes_decoded(&text)];
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let items: Vec<_> = decoded[0].store.items().values().cloned().collect();
    let mut h = panel(&decoded, 0, &items[0], map, &fixture());
    h.get_by_label("Text").click();
    h.run();
    assert_eq!(h.state().tab, Tab::Text);
    h.get_by_label_contains("  hi\\n");
    let ok = h.get_by_label_contains("  ok\\n").rect();
    h.get_by_label_contains("  \u{b7}z\\x15");
    // Across "ok\n", the pointer lights each character's byte in turn; a click selects one.
    let span = |i: &escapement_tickwave_decode::Item| Span {
        start: i.start,
        end: i.end.unwrap(),
    };
    let mut seen = vec![];
    let mut x = ok.left();
    while x < ok.right() {
        h.event(egui::Event::PointerMoved(egui::pos2(x, ok.center().y)));
        h.run();
        if let Some(l) = h.state().lit
            && seen.last() != Some(&l)
        {
            seen.push(l);
        }
        x += 1.0;
    }
    assert_eq!(seen, [span(&items[4]), span(&items[5]), span(&items[6])]);
    // Back onto the "k", then a click.
    let mut x = ok.left();
    while h.state().lit != Some(span(&items[5])) {
        h.event(egui::Event::PointerMoved(egui::pos2(x, ok.center().y)));
        h.run();
        x += 1.0;
    }
    let at = egui::pos2(x - 1.0, ok.center().y);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        });
    }
    h.run();
    assert_eq!(h.state().selected, Some((0, items[5].id)));

    // A second analysis, of frames whose text is two bytes each, chosen by name: a lone carriage
    // return escaped, and a line without a newline wrapping.
    let frames = |b: &[u8]| Value::Bits {
        bytes: b.to_vec().into(),
        bits: 8 * b.len() as u32,
    };
    let long = [b'a'; escapement_tickwave_egui::panel::LINE_AT_MOST + 2];
    let mut values = vec![frames(b"x\r"), frames(b"y\n")];
    values.extend(long.chunks(2).map(frames));
    let ty = escapement_tickwave_decode::FieldType::Bits { max: 16 };
    let decoded = vec![bytes_decoded(&text), text_decoded("frames", ty, values)];
    let mut h = panel(&decoded, 0, &items[0], map, &fixture());
    h.state_mut().tab = Tab::Text;
    h.run();
    h.get_by_label_contains("  hi\\n");
    h.get_by_label("frames").click();
    h.run();
    assert_eq!(h.state().texted, Some(1));
    h.get_by_label_contains("  x\\ry\\n");
    let wrapped = "a".repeat(escapement_tickwave_egui::panel::LINE_AT_MOST);
    assert!(
        h.query_by_label_contains(&format!("  {wrapped}a"))
            .is_none()
    );
    h.get_by_label_contains(&format!("  {wrapped}"));
    assert!(h.query_by_label_contains("  hi\\n").is_none());

    // With no analysis declaring text, a note.
    let (swd, map, _) = decode(&fixture());
    let t = transfers(&swd);
    let mut h = panel(&swd, 0, t[0], map, &fixture());
    h.state_mut().tab = Tab::Text;
    h.run();
    h.get_by_label("No analysis here reads as text.");
}

#[test]
fn a_narrow_panel_sits_under_the_plot_and_scrolls_inside() {
    use egui_kittest::kittest::Queryable;
    use escapement_tickwave_egui::panel::Tab;
    assert!(Panel::fits_beside(1280.0, 360.0));
    assert!(!Panel::fits_beside(600.0, 360.0));
    let (decoded, map, _) = decode(&fixture());
    let t = transfers(&decoded);
    let height = 200.0;
    let mut h = placed(&decoded, 0, t[0], map, &fixture(), Some(height));
    // The whole width under the plot, its control at the top right.
    let at = h.get_by_label("\u{21e5}").rect();
    // The window's right edge, inside the harness's 8-point margin.
    assert_eq!(at.right(), 400.0 - 8.0);
    assert!(
        at.top() >= PLOT && at.top() < PLOT + 20.0,
        "just under the plot: {at:?}"
    );
    // The list scrolls inside the height: its rows end above the panel's foot.
    h.state_mut().tab = Tab::List;
    h.run();
    let foot = at.top() + height;
    let rows: Vec<egui::Rect> = (t.iter())
        .filter_map(|i| {
            let time = format_interval(
                map.axis_saturating(i.start),
                open(&fixture()).timebase(),
                map.m(),
            );
            h.query_by_label_contains(&format!("{time}  "))
                .map(|n| n.rect())
        })
        .collect();
    assert!(
        !rows.is_empty() && rows.len() < t.len(),
        "{} rows",
        rows.len()
    );
    // What comes after the panel starts at its foot, however long the list.
    let after = h.get_by_label("after the panel").rect().top();
    assert!(
        after >= foot && after < foot + 10.0,
        "{after} after a foot at {foot}"
    );
    // Collapsed, it keeps its control's row, the control where it was.
    h.get_by_label("\u{21e5}").click();
    h.run();
    assert_eq!(Panel::reserved_height(h.state(), height), Panel::COLLAPSED);
    assert_eq!(h.get_by_label("\u{21e4}").rect(), at);
    let after = h.get_by_label("after the panel").rect().top();
    let row = at.top() + Panel::COLLAPSED;
    assert!(
        after >= row && after < row + 10.0,
        "{after} after the control's row"
    );
}

/// What the side panel costs over a million items in each of two analyses. A fresh panel reads
/// its stores a slice a frame. After that a new selection, a new analysis listed or texted, a
/// filter, and stores gaining a thousand items a frame each cost what changed. Run with
/// `--ignored --nocapture`.
#[test]
#[ignore = "a measurement"]
fn panel_frame_times() {
    use egui_kittest::kittest::Queryable;
    use escapement_tickwave_egui::panel::Tab;
    const N: usize = 1_000_000;
    let values: Vec<Option<u8>> = (0..N).map(|k| Some(b'a' + (k % 26) as u8)).collect();
    let mut a = bytes_decoded(&values);
    a.name = "upstream";
    let mut b = bytes_decoded(&values);
    b.name = "downstream";
    let decoded = vec![a, b];
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let ids: Vec<Id> = decoded[0].store.items().keys().copied().collect();
    let middle = &decoded[0].store.items()[&ids[N / 2]];
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let timed = |step: &mut dyn FnMut()| {
        let t = std::time::Instant::now();
        step();
        t.elapsed().as_secs_f64() * 1e3
    };
    for tab in [Tab::Selected, Tab::List, Tab::Text] {
        let mut h = panel(&decoded, 0, middle, map, &fixture());
        h.state_mut().tab = tab;
        h.run();
        // A new selection, and in the List and Text tabs the other analysis chosen.
        let mut chosen = vec![];
        for k in 0..20 {
            h.state_mut().selected = Some((0, ids[N / 2 + k + 1]));
            if tab != Tab::Selected {
                h.state_mut().listed = Some(k % 2);
                h.state_mut().texted = Some(k % 2);
            }
            chosen.push(timed(&mut || h.step()));
        }
        let mut kept = vec![];
        for _ in 0..50 {
            kept.push(timed(&mut || h.step()));
        }
        println!(
            "{tab:?}: a new selection {:.2} ms (median of 20), a frame after {:.2} ms (median of 50)",
            median(chosen),
            median(kept)
        );
        if tab == Tab::List {
            // A kind left out and put back, and conditions only and all again.
            let mut filtered = vec![];
            for _ in 0..10 {
                for label in ["byte", "conditions only"] {
                    h.get_by_label(label).click();
                    filtered.push(timed(&mut || h.step()));
                    h.step();
                }
            }
            println!(
                "List: a new filter {:.2} ms (median of 20)",
                median(filtered)
            );
        }
    }
    // A fresh panel reading both stores.
    let mut state = PanelState::default();
    state.selected = Some((0, middle.id));
    let timebase = open(&fixture()).timebase();
    let palette = escapement_tickwave_egui::Palette::from_visuals(&egui::Visuals::default());
    // Built, the harness runs two frames, timed together.
    let t = std::time::Instant::now();
    let mut h = Harness::builder()
        .with_size(egui::vec2(400.0, 900.0))
        .with_max_steps(1)
        .build_ui_state(
            |ui, state: &mut PanelState| {
                let analyses = analyses(&decoded);
                ui.add(Panel::new(&palette, state, &analyses, 360.0, timebase, map));
            },
            state,
        );
    let built = t.elapsed().as_secs_f64() * 1e3;
    let mut reading = vec![];
    while reading.is_empty() || h.query_by_label_contains("Still reading").is_some() {
        reading.push(timed(&mut || h.step()));
    }
    let frames = reading.len() + 2;
    let slowest = reading.iter().copied().fold(0.0, f64::max);
    println!(
        "Reading two stores of a million items: {frames} frames; past the first two ({built:.2} ms together), {:.2} ms a frame (median), {slowest:.2} ms the slowest, {:.0} ms in all",
        median(reading.clone()),
        built + reading.iter().sum::<f64>()
    );
    drop(h);
    // Stores that grow a thousand items a frame, as a live decode's do.
    for tab in [Tab::Selected, Tab::List, Tab::Text] {
        let decoded = vec![bytes_decoded(&values), bytes_decoded(&values)];
        let last = *decoded[0].store.items().keys().next_back().unwrap();
        let mut h = growing(decoded, (0, last), tab);
        let mut at = 1000 * (N as Tick + 1);
        let mut grown = vec![];
        for _ in 0..30 {
            for _ in 0..1000 {
                at += 1000;
                push(&mut h.state_mut().decoded[0], at, byte(b'q'), None);
                push(&mut h.state_mut().decoded[1], at, byte(b'q'), None);
            }
            grown.push(timed(&mut || h.step()));
        }
        println!(
            "{tab:?}: a frame after a thousand items appended {:.2} ms (median of 30)",
            median(grown)
        );
    }
}

/// Analyses a test grows between frames, and the panel over them.
struct Grown {
    decoded: Vec<Decoded>,
    panel: PanelState,
}

fn growing(
    decoded: Vec<Decoded>,
    selected: (usize, Id),
    tab: escapement_tickwave_egui::panel::Tab,
) -> Harness<'static, Grown> {
    let timebase = open(&fixture()).timebase();
    let palette = escapement_tickwave_egui::Palette::from_visuals(&egui::Visuals::default());
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let mut panel = PanelState::default();
    panel.selected = Some(selected);
    panel.tab = tab;
    let mut h = Harness::builder()
        .with_size(egui::vec2(400.0, 900.0))
        .with_max_steps(READ_FRAMES)
        .build_ui_state(
            move |ui, g: &mut Grown| {
                let Grown { decoded, panel } = g;
                let analyses = analyses(decoded);
                let right = egui::Layout::right_to_left(egui::Align::Min);
                ui.with_layout(right, |ui| {
                    ui.add(Panel::new(&palette, panel, &analyses, 360.0, timebase, map));
                });
            },
            Grown { decoded, panel },
        );
    h.run();
    h
}

/// Adds an item of kind 0 at `start` to `d`'s store: its field 0 `value`, its link 0 to `of`
/// upstream.
fn push(d: &mut Decoded, start: Tick, value: Option<Value>, of: Option<Id>) -> Id {
    use escapement_tickwave_decode::{FieldId, KindId, LinkId, Op, Status};
    let id = Id::new(KindId(0), start);
    let mut ops = vec![Op::Open {
        id,
        parent: None,
        start,
    }];
    if let Some(value) = value {
        ops.push(Op::Set {
            id,
            field: FieldId(0),
            value,
            span: None,
            driver: None,
        });
    }
    if let Some(o) = of {
        ops.push(Op::Link {
            id,
            link: LinkId(0),
            other: Ref::Upstream(o),
        });
    }
    ops.push(Op::Close {
        id,
        end: start + 800,
        status: Status::Complete,
    });
    d.store.apply_batch(&ops, 8).unwrap();
    id
}

fn byte(b: u8) -> Option<Value> {
    Some(Value::Word {
        value: u64::from(b),
        bits: 8,
    })
}

#[test]
fn the_panel_takes_in_what_a_growing_store_gains() {
    use egui_kittest::kittest::Queryable;
    use escapement_tickwave_decode::{KindDecl, LinkDecl, LinkTo};
    use escapement_tickwave_egui::panel::Tab;
    // Text: a carriage return waits for what follows it.
    let decoded = vec![bytes_decoded(&[Some(b'x'), Some(b'\r')])];
    let first = *decoded[0].store.items().keys().next().unwrap();
    let mut h = growing(decoded, (0, first), Tab::Text);
    h.get_by_label_contains("  x\\r");
    push(&mut h.state_mut().decoded[0], 3000, byte(b'\n'), None);
    h.run();
    h.get_by_label_contains("  x\\n");
    assert!(h.query_by_label_contains("  x\\r").is_none());
    push(&mut h.state_mut().decoded[0], 4000, byte(b'y'), None);
    h.run();
    h.get_by_label_contains("  y");
    // An item before the others takes a full pass.
    push(&mut h.state_mut().decoded[0], 500, byte(b'w'), None);
    h.run();
    h.get_by_label_contains("  wx\\n");
    // The list adds a row for an item appended.
    h.state_mut().panel.tab = Tab::List;
    h.run();
    let rows = |h: &Harness<'_, Grown>| h.query_all_by_label_contains("  byte").count();
    let before = rows(&h);
    push(&mut h.state_mut().decoded[0], 5000, byte(b'z'), None);
    h.run();
    assert_eq!(rows(&h), before + 1);

    // A line that starts inside a frame of several bytes is joined again from that byte.
    let frame = |b: &[u8]| Value::Bits {
        bytes: b.to_vec().into(),
        bits: 8 * b.len() as u32,
    };
    let ty = escapement_tickwave_decode::FieldType::Bits { max: 16 };
    let decoded = vec![text_decoded("frames", ty, vec![frame(b"ab\ncd")])];
    let first = *decoded[0].store.items().keys().next().unwrap();
    let mut h = growing(decoded, (0, first), Tab::Text);
    h.get_by_label_contains("  cd");
    push(&mut h.state_mut().decoded[0], 3000, Some(frame(b"e")), None);
    h.run();
    h.get_by_label_contains("  ab\\n");
    h.get_by_label_contains("  cde");
    assert_eq!(h.query_all_by_label_contains("ab\\n").count(), 1);

    // A reader appended downstream, and a next of the selected item's kind.
    let readers = Schema::new(vec![KindDecl::new("reader").with_links(vec![
        LinkDecl::new("of", vec![LinkTo::Upstream(name("byte"))]),
    ])]);
    let store = Store::with_keep(all());
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let reader = Decoded {
        name: "readers",
        schema: readers,
        density: Density::of(store.items(), &map, condition),
        store,
        labels: Labels::default(),
        parts: false,
    };
    let decoded = vec![bytes_decoded(&[Some(b'a'), Some(b'b')]), reader];
    let last = *decoded[0].store.items().keys().next_back().unwrap();
    let mut h = growing(decoded, (0, last), Tab::Selected);
    assert!(h.query_by_label_contains("read as ").is_none());
    h.get_by_label("next byte \u{25b8}").click();
    h.run();
    assert_eq!(
        h.state().panel.selected,
        Some((0, last)),
        "none after the last"
    );
    push(&mut h.state_mut().decoded[1], 2500, None, Some(last));
    let next = push(&mut h.state_mut().decoded[0], 3000, byte(b'c'), None);
    h.run();
    h.get_by_label_contains("read as ");
    h.get_by_label("next byte \u{25b8}").click();
    h.run();
    assert_eq!(h.state().panel.selected, Some((0, next)));
}

#[test]
fn a_reader_linked_in_a_later_batch_reads_the_selected_item() {
    use egui_kittest::kittest::Queryable;
    use escapement_tickwave_decode::{KindDecl, KindId, LinkDecl, LinkId, LinkTo, Op, Status};
    use escapement_tickwave_egui::panel::Tab;
    let readers = Schema::new(vec![KindDecl::new("reader").with_links(vec![
        LinkDecl::new("of", vec![LinkTo::Upstream(name("byte"))]),
    ])]);
    let store = Store::with_keep(all());
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let reader = Decoded {
        name: "readers",
        schema: readers,
        density: Density::of(store.items(), &map, condition),
        store,
        labels: Labels::default(),
        parts: false,
    };
    let mut decoded = vec![bytes_decoded(&[Some(b'a'), Some(b'b')]), reader];
    let last = *decoded[0].store.items().keys().next_back().unwrap();
    let rid = Id::new(KindId(0), 2500);
    let open = Op::Open {
        id: rid,
        parent: None,
        start: 2500,
    };
    decoded[1].store.apply_batch(&[open], 8).unwrap();
    let mut h = growing(decoded, (0, last), Tab::Selected);
    assert!(h.query_by_label_contains("read as ").is_none());
    let later = [
        Op::Link {
            id: rid,
            link: LinkId(0),
            other: Ref::Upstream(last),
        },
        Op::Close {
            id: rid,
            end: 3300,
            status: Status::Complete,
        },
    ];
    h.state_mut().decoded[1]
        .store
        .apply_batch(&later, 8)
        .unwrap();
    h.run();
    h.get_by_label_contains("read as ");
}

#[test]
fn the_panel_lets_go_of_what_its_store_drops() {
    use egui_kittest::kittest::Queryable;
    use escapement_tickwave_decode::{FieldId, KindId, Op, Span, Status};
    use escapement_tickwave_egui::panel::Tab;
    let mut d = bytes_decoded(&[Some(b'a'), Some(b'b')]);
    d.store.keep = vec![Span {
        start: 0,
        end: 2500,
    }];
    let first = *d.store.items().keys().next().unwrap();
    let mut h = growing(vec![d], (0, first), Tab::Text);
    h.get_by_label_contains("  ab");
    // Open past the kept span, the store holds it; closed there, it drops it.
    let id = Id::new(KindId(0), 3000);
    let open = [
        Op::Open {
            id,
            parent: None,
            start: 3000,
        },
        Op::Set {
            id,
            field: FieldId(0),
            value: byte(b'z').unwrap(),
            span: None,
            driver: None,
        },
    ];
    h.state_mut().decoded[0]
        .store
        .apply_batch(&open, 8)
        .unwrap();
    h.run();
    h.get_by_label_contains("  abz");
    h.state_mut().panel.tab = Tab::List;
    h.run();
    let rows = |h: &Harness<'_, Grown>| h.query_all_by_label_contains("  byte").count();
    assert_eq!(rows(&h), 3);
    let close = Op::Close {
        id,
        end: 3800,
        status: Status::Complete,
    };
    h.state_mut().decoded[0]
        .store
        .apply_batch(&[close], 8)
        .unwrap();
    assert!(!h.state().decoded[0].store.items().contains_key(&id));
    h.run();
    assert_eq!(rows(&h), 2);
    h.state_mut().panel.tab = Tab::Text;
    h.run();
    h.get_by_label_contains("  ab");
    assert!(h.query_by_label_contains("abz").is_none());
}

#[test]
fn a_panel_reads_a_large_store_a_slice_a_frame() {
    use egui_kittest::kittest::Queryable;
    use escapement_tickwave_egui::panel::READ_AT_MOST;
    let n = 10 * READ_AT_MOST;
    let values: Vec<Option<u8>> = (0..n).map(|k| Some(b'a' + (k % 26) as u8)).collect();
    let decoded = vec![bytes_decoded(&values)];
    let ids: Vec<Id> = decoded[0].store.items().keys().copied().collect();
    let last = *ids.last().unwrap();
    let mut state = PanelState::default();
    state.selected = Some((0, last));
    let timebase = open(&fixture()).timebase();
    let palette = escapement_tickwave_egui::Palette::from_visuals(&egui::Visuals::default());
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    // Built, the harness runs three frames: the first, and two more as it waits to settle.
    let mut h = Harness::builder()
        .with_size(egui::vec2(400.0, 900.0))
        .with_max_steps(1)
        .build_ui_state(
            |ui, state: &mut PanelState| {
                let analyses = analyses(&decoded);
                ui.add(Panel::new(&palette, state, &analyses, 360.0, timebase, map));
            },
            state,
        );
    h.get_by_label_contains("Still reading the decode");
    // The last item's neighbour is not read yet, so there is none to step to; a click takes two
    // frames.
    h.get_by_label("\u{25c2} previous byte").click();
    h.step();
    h.step();
    assert_eq!(h.state().selected, Some((0, last)));
    // The List shows no rows until its store is read through.
    h.state_mut().tab = escapement_tickwave_egui::panel::Tab::List;
    h.step();
    h.get_by_label("Still reading the decode.");
    assert_eq!(h.query_all_by_label_contains("  byte").count(), 0);
    while h.query_by_label_contains("Still reading").is_some() {
        h.step();
    }
    assert_eq!(h.ctx.cumulative_frame_nr(), 10, "a slice a frame");
    assert!(h.query_all_by_label_contains("  byte").count() > 10);
    h.state_mut().tab = escapement_tickwave_egui::panel::Tab::Selected;
    h.step();
    h.get_by_label("\u{25c2} previous byte").click();
    h.run_steps(3);
    assert_eq!(h.state().selected, Some((0, ids[n - 2])));
}
