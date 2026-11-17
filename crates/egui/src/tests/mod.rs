//! The graph's behaviour, held through egui_kittest: recordings laid out as verge's energy
//! captures are, a host that keeps a `ViewState`, and readers of what a frame painted.

mod ease;
mod graph;
mod lanes;
mod live;
mod roles;
mod strip;
mod viewer;

use std::collections::BTreeMap;
use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use egui::epaint::ClippedShape;
use egui::{Color32, FontId, IdSalt, Pos2, Rect, Shape, Vec2};
use egui_kittest::Harness;
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, IntCoding,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{Gap, Ratio};
use escapement_tickwave_format::writer::{Bundle, Finish, WriteOptions, Writer};
use escapement_tickwave_view::{Answer, Bytes};

use crate::graph::{Shown, shown};
use crate::{Extras, Graph, Palette, Strip, ViewState, Viewer};

/// Ticks a second: a microsecond each.
pub const SECOND: i64 = 1_000_000;
pub const MS: i64 = 1_000;

/// Channel indices in a capture: V, I, under_range, DIO0 to DIO3, then P.
pub const I: usize = 1;
pub const UNDER: usize = 2;
pub const DIO3: usize = 6;
pub const LANES: [usize; 4] = [3, 4, 5, 6];

/// A capture as verge's tests make one: `count` pairs at `rate` a second, each pair's current
/// code (nA) from `code`, the pairs in `lost` never arriving, and time held for `hold` ticks
/// before each pair in `held`, as a debugger connecting does.
pub struct Capture {
    pub rate: i64,
    pub count: u64,
    pub code: fn(u64) -> u64,
    pub lost: Vec<Range<u64>>,
    pub held: Vec<u64>,
    pub hold: i64,
    /// DIO3 is high while the current is above 1 mA; the others stay low.
    pub dio: bool,
    /// Gaps in DIO3, as its edges lost.
    pub dio_gaps: Vec<Range<i64>>,
    /// Pairs read under range.
    pub under: fn(u64) -> bool,
    /// (tick, number, label)
    pub markers: Vec<(i64, u64, String)>,
}

impl Capture {
    pub fn new(rate: i64, count: u64, code: fn(u64) -> u64) -> Self {
        Self {
            rate,
            count,
            code,
            lost: Vec::new(),
            held: Vec::new(),
            hold: 0,
            dio: false,
            dio_gaps: Vec::new(),
            under: |_| false,
            markers: Vec::new(),
        }
    }

    pub fn period(&self) -> i64 {
        SECOND / self.rate
    }

    /// Where pair `i` sits, held time included.
    pub fn tick(&self, i: u64) -> i64 {
        let holds = self.held.iter().filter(|&&at| at <= i).count() as i64;
        i as i64 * self.period() + holds * self.hold
    }

    fn lost_at(&self, i: u64) -> bool {
        self.lost.iter().any(|r| r.contains(&i))
    }

    /// The finished file, its markers bundled.
    pub fn write(&self) -> Vec<u8> {
        let (mut w, mut out) = self.writer();
        self.items(&mut w, 0..self.count);
        out.extend(w.take_output());
        out.extend(self.finish(w));
        out
    }

    pub fn writer(&self) -> (Writer, Vec<u8>) {
        let int = |den| {
            AnalogCoding::Int(IntCoding {
                scale: Exact::new(1, den),
                ..IntCoding::new(32, false)
            })
        };
        let analog = |name: &str, unit: &str, coding| DeclaredChannel {
            name: name.into(),
            source_name: name.to_lowercase(),
            source: 0,
            data: DeclaredKind::Analog {
                unit: unit.into(),
                period: Ratio::integer(self.period() as u64).unwrap(),
                coding,
                significant: Some(4),
                phase: None,
            },
            shaping: Default::default(),
            overview_shift: None,
            codec: None,
            requires: Vec::new(),
        };
        let digital = |name: &str| DeclaredChannel {
            name: name.into(),
            source_name: name.to_lowercase(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
            overview_shift: None,
            codec: None,
            requires: Vec::new(),
        };
        let timebase = Ratio::integer(SECOND as u64).unwrap();
        let declaration = Declaration {
            timebase,
            channels: vec![
                analog("V", "V", int(1000)),
                analog("I", "A", int(1_000_000_000)),
                digital("under_range"),
                digital("DIO0"),
                digital("DIO1"),
                digital("DIO2"),
                digital("DIO3"),
            ],
            derived: vec![Derived {
                name: "P".into(),
                unit: "W".into(),
                op: escapement_tickwave_format::header::Op::Multiply,
                operands: vec!["I".into(), "V".into()],
                params: Default::default(),
                requires: Vec::new(),
                significant: None,
            }],
            sources: vec![SourceHeader::new(
                SourceFormat::Other("verge-energy".into()),
                timebase,
            )],
            title: None,
            meta: BTreeMap::new(),
            // The Recorder's rule: round(log2(1024 × ticks a pair)).
            overview_shift: (1024.0 * self.period() as f64).log2().round() as u8,
            requires: Vec::new(),
        };
        let mut w = Writer::new(
            declaration,
            WriteOptions {
                level: 1,
                block_bytes: 1 << 16,
                promise_with_items: true,
                ..WriteOptions::default()
            },
        )
        .unwrap();
        let out = w.take_output();
        w.run(0, 0, 0).unwrap();
        w.run(1, 0, 0).unwrap();
        (w, out)
    }

    /// Writes pairs `pairs`, with their runs, edges, gaps and markers, flushing at each second.
    /// DIO3 has no edge inside a gap; the gap ends at the level its pairs left.
    pub fn items(&self, w: &mut Writer, pairs: Range<u64>) {
        let high = |i: u64| self.dio && (self.code)(i) > 1_000_000;
        // The levels the last pair written left.
        let before = (0..pairs.start).rev().find(|&j| !self.lost_at(j));
        let mut level = before.is_some_and(high);
        let mut under = before.is_some_and(self.under);
        let mut flushed = self.tick(pairs.start) / SECOND;
        let start = self.tick(pairs.start);
        let mut gaps = self.dio_gaps.iter().filter(|g| g.end > start).peekable();
        let mut markers = self.markers.iter().filter(|m| m.0 >= start).peekable();
        let last = pairs.end == self.count;
        for i in pairs {
            if self.lost_at(i) {
                continue;
            }
            let tick = self.tick(i);
            while (flushed + 1) * SECOND <= tick {
                flushed += 1;
                w.flush(flushed * SECOND).unwrap();
            }
            while let Some(m) = markers.next_if(|m| m.0 <= tick) {
                marker(w, m);
            }
            while let Some(g) = gaps.next_if(|g| g.end <= tick) {
                let gap = Gap {
                    from: g.start,
                    to: g.end,
                    level,
                };
                w.gap(DIO3, gap).unwrap();
            }
            let lost = (0..i).rev().take_while(|&j| self.lost_at(j)).count() as u64;
            if i > 0 && (lost > 0 || self.held.contains(&i)) {
                w.run(0, tick, lost).unwrap();
                w.run(1, tick, lost).unwrap();
            }
            if high(i) != level {
                level = !level;
                if !self.dio_gaps.iter().any(|g| g.contains(&tick)) {
                    w.edge(DIO3, tick).unwrap();
                }
            }
            if (self.under)(i) != under {
                under = !under;
                w.edge(UNDER, tick).unwrap();
            }
            w.sample(0, 3300u64).unwrap();
            w.sample(1, (self.code)(i)).unwrap();
        }
        if last {
            for m in markers {
                marker(w, m);
            }
        }
    }

    pub fn length(&self) -> i64 {
        self.tick(self.count - 1) + self.period()
    }

    pub fn finish(&self, w: Writer) -> Vec<u8> {
        let length = self.length();
        let finish = Finish {
            bundle: (!self.markers.is_empty()).then(|| Bundle::Generated {
                stem: "capture".into(),
            }),
            ..Finish::default()
        };
        w.finish(length, &[length; 5], finish).unwrap().tail
    }
}

/// A marker as verge's Recorder writes one.
fn marker(w: &mut Writer, (tick, number, label): &(i64, u64, String)) {
    let serde_json::Value::Object(marker) = serde_json::json!({
        "id": format!("marker/{number}"),
        "kind": "marker",
        "start": tick,
        "label": label,
        "values": {"number": number.to_string()},
    }) else {
        unreachable!("an object")
    };
    w.annotation("verge", marker).unwrap();
}

/// 20 s at 1,000 pairs a second, with a 30 ms pulse each second: verge's fixture.
pub fn fixture() -> Capture {
    Capture::new(1000, 20_000, |i| {
        if i % 1000 < 30 { 3_000_000 } else { 350_000 }
    })
}

/// 2 ms of every 10, with noise on both levels: verge's `noisy_marker`.
pub fn noisy(i: u64) -> u64 {
    let noise = u64::from((i as u32).wrapping_mul(2_654_435_761) >> 24);
    if i % 10 < 2 {
        2_900_000 + noise * 500
    } else {
        340_000 + noise * 60
    }
}

/// Holds reads while closed, and fails them while failing.
#[derive(Default)]
pub struct Gate {
    /// The reads let through before it closes; None while it is open.
    reads: Mutex<Option<u64>>,
    turn: Condvar,
    fail: AtomicBool,
}

impl Gate {
    pub fn close(&self) {
        self.pass(0);
    }

    pub fn open(&self) {
        *self.reads.lock().unwrap() = None;
        self.turn.notify_all();
    }

    /// Lets `n` more reads through, then holds the rest.
    pub fn pass(&self, n: u64) {
        *self.reads.lock().unwrap() = Some(n);
        self.turn.notify_all();
    }

    pub fn fail(&self, fail: bool) {
        self.fail.store(fail, Ordering::Relaxed);
    }
}

/// Opens the gate when dropped, so a failing test never leaves the query thread waiting on it.
pub struct Opens(pub Arc<Gate>);

impl Drop for Opens {
    fn drop(&mut self) {
        self.0.open();
    }
}

struct Gated {
    bytes: Arc<dyn Bytes>,
    gate: Arc<Gate>,
}

impl Bytes for Gated {
    fn size(&self) -> io::Result<u64> {
        self.bytes.size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut reads = self.gate.reads.lock().unwrap();
        while *reads == Some(0) {
            reads = self.gate.turn.wait(reads).unwrap();
        }
        if let Some(n) = reads.as_mut() {
            *n -= 1;
        }
        drop(reads);
        if self.gate.fail.load(Ordering::Relaxed) {
            return Err(io::Error::other("unreadable"));
        }
        self.bytes.read_at(offset, buf)
    }
}

/// A viewer of `file` whose reads go through `gate`, keeping no decoded data when `keep` is
/// false, so every query reads.
pub fn gated(ctx: &egui::Context, file: Vec<u8>, gate: &Arc<Gate>, keep: bool) -> Viewer {
    let bytes: Arc<dyn Bytes> = Arc::new(Arc::<[u8]>::from(file));
    let gate = Arc::clone(gate);
    Viewer::with_opener(
        ctx,
        Box::new(move || {
            Ok(Arc::new(Gated {
                bytes: Arc::clone(&bytes),
                gate: Arc::clone(&gate),
            }) as Arc<dyn Bytes>)
        }),
        if keep { 64 << 20 } else { 0 },
    )
    .unwrap()
}

/// A live viewer of the file at `path`, its reads through `gate`, keeping no decoded data.
pub fn gated_live(
    ctx: &egui::Context,
    path: &std::path::Path,
    snapshot: escapement_tickwave_format::snapshot::Snapshot,
    gate: &Arc<Gate>,
) -> Viewer {
    let path = path.to_owned();
    let gate = Arc::clone(gate);
    Viewer::live_with_opener(
        ctx,
        Box::new(move || {
            Ok(Arc::new(Gated {
                bytes: Arc::new(std::fs::File::open(&path)?),
                gate: Arc::clone(&gate),
            }) as Arc<dyn Bytes>)
        }),
        snapshot,
        0,
    )
    .unwrap()
}

pub fn palette() -> Palette {
    Palette {
        trace: Some(Color32::from_rgb(200, 30, 30)),
        dark: false,
        roles: BTreeMap::new(),
        surface: Color32::from_rgb(250, 250, 250),
        hair: Color32::from_rgb(220, 220, 220),
        ink: Color32::from_rgb(10, 10, 10),
        ink_2: Color32::from_rgb(90, 90, 90),
        attention_fill: Color32::from_rgb(240, 160, 0),
        attention_ink: Color32::from_rgb(160, 80, 0),
        wash: Color32::from_rgba_unmultiplied(0, 0, 200, 40),
        figures: FontId::monospace(11.0),
        labels: FontId::monospace(11.0),
        pale: 0.35,
        fill: 0.45,
        shade: 0.2,
        // The width the lanes' geometry in these tests is worked for.
        line_weight: 1.0,
    }
}

const SALT: &str = "test graph";
const STRIP_SALT: &str = "test strip";
/// The strip's height in the tests.
pub const STRIP: f32 = 64.0;

/// What a host keeps for its graph.
pub struct Host {
    pub viewer: Option<Viewer>,
    pub state: ViewState,
    pub palette: Palette,
    /// None for a graph of lanes alone.
    pub trace: Option<usize>,
    pub flag: Option<usize>,
    pub lanes: Vec<usize>,
    pub include_zero: bool,
    pub extras: Extras,
    pub id: Option<egui::Id>,
    /// Some frame's response said the window or the selection moved.
    pub changed: bool,
    /// A strip under the graph, and what its response said.
    pub strip: bool,
    pub strip_id: Option<egui::Id>,
    pub strip_changed: bool,
    /// The host's now, given to the graph and the strip.
    pub now: Option<i64>,
    /// Whether a zoom step eases: off but in the tests of the ease.
    pub motion: bool,
}

pub fn harness(state: ViewState) -> Harness<'static, Host> {
    harness_at(state, 1.0)
}

/// A harness at `pixels_per_point` device pixels a point.
pub fn harness_at(state: ViewState, pixels_per_point: f32) -> Harness<'static, Host> {
    let host = Host {
        viewer: None,
        state,
        palette: palette(),
        trace: Some(I),
        flag: Some(UNDER),
        lanes: LANES.to_vec(),
        include_zero: true,
        extras: Extras::default(),
        id: None,
        changed: false,
        strip: false,
        strip_id: None,
        strip_changed: false,
        now: None,
        motion: false,
    };
    Harness::builder()
        .with_size(egui::vec2(1080.0, 700.0))
        .with_pixels_per_point(pixels_per_point)
        // A frame's time, so two clicks a few frames apart make a double click.
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, host: &mut Host| {
                let Some(viewer) = &host.viewer else { return };
                host.id = Some(ui.make_persistent_id(IdSalt::new(SALT)));
                let graph = match host.trace {
                    Some(t) => Graph::new(viewer, &mut host.state, &host.palette, t),
                    None => Graph::lanes_only(viewer, &mut host.state, &host.palette),
                };
                let mut graph = graph
                    .flag(host.flag)
                    .lanes(&host.lanes)
                    .include_zero(host.include_zero)
                    .extras(host.extras)
                    .now(host.now)
                    .motion(host.motion)
                    .id_salt(SALT);
                if host.strip {
                    graph = graph.height(ui.available_height() - STRIP - 8.0);
                }
                let response = ui.add(graph);
                host.changed |= response.changed();
                if host.strip {
                    host.strip_id = Some(ui.make_persistent_id(IdSalt::new(STRIP_SALT)));
                    let response = ui.add(
                        Strip::new(viewer, &mut host.state, &host.palette)
                            .include_zero(host.include_zero)
                            .now(host.now)
                            .height(STRIP)
                            .id_salt(STRIP_SALT),
                    );
                    host.strip_changed |= response.changed();
                }
            },
            host,
        )
}

/// A harness showing `file` through a gate, with `state`.
pub fn showing(file: Vec<u8>, state: ViewState, keep: bool) -> (Harness<'static, Host>, Arc<Gate>) {
    let gate = Arc::new(Gate::default());
    let mut h = harness(state);
    h.state_mut().viewer = Some(gated(&h.ctx, file, &gate, keep));
    (h, gate)
}

pub fn viewer<'a>(h: &'a Harness<'_, Host>) -> &'a Viewer {
    h.state().viewer.as_ref().expect("a viewer")
}

/// What the graph drew last.
pub fn drawn(h: &Harness<'_, Host>) -> Shown {
    shown(&h.ctx, h.state().id.expect("a graph")).expect("a frame drawn")
}

/// Steps until an answer has landed.
pub fn wait(h: &mut Harness<'_, Host>) -> Arc<Answer> {
    for _ in 0..400 {
        h.step();
        if let Some(a) = viewer(h).answer() {
            h.step();
            return a;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("no answer");
}

/// Whether `a` answers a window a second wide that follows a recording being written to `end`:
/// its columns whole ticks, and its window ending at the first of their ends at or after `end`.
pub fn follows_to(a: &Answer, end: i64) -> bool {
    let (x0, per) = (a.grid.x0, a.grid.per);
    let ends = a.window.end as f64 - x0;
    per.fract() == 0.0
        && x0.fract() == 0.0
        && ends % per == 0.0
        && a.window.end - a.window.start == SECOND
        && (end..end + per as i64).contains(&a.window.end)
}

/// Steps until the answer held follows the recording to `end`, as `follows_to` has it, then
/// draws it.
pub fn followed(h: &mut Harness<'_, Host>, end: i64) -> Arc<Answer> {
    for _ in 0..400 {
        h.step();
        if let Some(a) = viewer(h).answer()
            && follows_to(&a, end)
        {
            h.step();
            return a;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!(
        "no answer following to {end}: {:?}",
        viewer(h).answer().map(|a| (a.window.clone(), a.grid))
    );
}

/// Steps until the answer held is for `window`, then draws it.
pub fn answered(h: &mut Harness<'_, Host>, window: Range<i64>) -> Arc<Answer> {
    for _ in 0..400 {
        h.step();
        if let Some(a) = viewer(h).answer()
            && a.window == window
        {
            h.step();
            return a;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("no answer for {window:?}");
}

pub fn button(h: &mut Harness<'_, Host>, pos: Pos2, pressed: bool) {
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    });
    h.step();
}

/// Drags from `from` by `by` in `steps` frames, letting queries run between them, calling
/// `during` before the button is released.
pub fn drag_with(
    h: &mut Harness<'_, Host>,
    from: Pos2,
    by: Vec2,
    steps: u32,
    during: impl FnOnce(&mut Harness<'_, Host>),
) {
    h.event(egui::Event::PointerMoved(from));
    h.step();
    button(h, from, true);
    for step in 1..=steps {
        h.event(egui::Event::PointerMoved(
            from + by * step as f32 / steps as f32,
        ));
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    during(h);
    button(h, from + by, false);
}

pub fn drag(h: &mut Harness<'_, Host>, from: Pos2, by: Vec2, steps: u32) {
    drag_with(h, from, by, steps, |_| {});
}

/// The middle of the plot.
pub fn plot(h: &Harness<'_, Host>) -> Rect {
    drawn(h).layout.plot
}

/// Every shape the last frame painted, with its clip.
pub fn shapes(h: &Harness<'_, Host>) -> Vec<ClippedShape> {
    h.output().shapes.clone()
}

/// The x spans of the triangles all of whose corners are `colour`, in the meshes clipped to
/// `clip`.
pub fn spans_of(h: &Harness<'_, Host>, clip: Rect, colour: Color32) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    for s in shapes(h) {
        let Shape::Mesh(mesh) = &s.shape else {
            continue;
        };
        if s.clip_rect != clip {
            continue;
        }
        for t in mesh.indices.as_chunks::<3>().0 {
            let v = t.map(|i| mesh.vertices[i as usize]);
            if v.iter().all(|v| v.color == colour) {
                let xs = v.map(|v| v.pos.x);
                out.push((
                    xs.iter().copied().fold(f32::INFINITY, f32::min),
                    xs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
                ));
            }
        }
    }
    out
}

/// Whether a triangle with a corner coloured `colour`, in the meshes clipped to `clip`, reaches
/// into the open interval (a, b) of x: a line drawn across it has no corner inside it.
pub fn crossed(h: &Harness<'_, Host>, clip: Rect, colour: Color32, a: f32, b: f32) -> bool {
    shapes(h).iter().any(|s| match &s.shape {
        Shape::Mesh(mesh) if s.clip_rect == clip => {
            mesh.indices.as_chunks::<3>().0.iter().any(|t| {
                let v = t.map(|i| mesh.vertices[i as usize]);
                let lo = v.iter().map(|v| v.pos.x).fold(f32::INFINITY, f32::min);
                let hi = v.iter().map(|v| v.pos.x).fold(f32::NEG_INFINITY, f32::max);
                v.iter().any(|v| v.color == colour) && lo < b && hi > a
            })
        }
        _ => false,
    })
}

/// The points of every vertex coloured `colour` in the meshes clipped to `clip`.
pub fn vertices_of(h: &Harness<'_, Host>, clip: Rect, colour: Color32) -> Vec<Pos2> {
    shapes(h)
        .iter()
        .filter(|s| s.clip_rect == clip)
        .filter_map(|s| match &s.shape {
            Shape::Mesh(mesh) => Some(mesh.vertices.clone()),
            _ => None,
        })
        .flatten()
        .filter(|v| v.color == colour)
        .map(|v| v.pos)
        .collect()
}

/// The filled rectangles of `colour` clipped to `clip`.
pub fn rects_of(h: &Harness<'_, Host>, clip: Rect, colour: Color32) -> Vec<Rect> {
    shapes(h)
        .iter()
        .filter(|s| s.clip_rect == clip)
        .filter_map(|s| match &s.shape {
            Shape::Rect(r) if r.fill == colour => Some(r.rect),
            _ => None,
        })
        .collect()
}

/// Every text painted, and where.
pub fn texts(h: &Harness<'_, Host>) -> Vec<(String, Rect)> {
    shapes(h)
        .iter()
        .filter_map(|s| match &s.shape {
            Shape::Text(t) => Some((
                t.galley.text().to_owned(),
                Rect::from_min_size(t.pos, t.galley.size()),
            )),
            _ => None,
        })
        .collect()
}
