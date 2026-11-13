//! A frame's work in `tracing` spans. A binary of its own: tracing caches whether a span is
//! wanted for the whole process, and a test running beside this one, with no subscriber, would
//! settle it for spans this one has not reached yet.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui_kittest::Harness;
use escapement_tickwave_egui::{Graph, Palette, ViewState, Viewer};
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Exact, IntCoding, SourceFormat,
    SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};

/// Records the name and level of every span opened.
#[derive(Default)]
struct Spans(Mutex<Vec<(&'static str, tracing::Level)>>);

impl tracing::Subscriber for Spans {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut seen = self.0.lock().unwrap();
        seen.push((span.metadata().name(), *span.metadata().level()));
        tracing::span::Id::from_u64(seen.len() as u64)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, _: &tracing::Event<'_>) {}

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// 2 s of a current at 1,000 samples a second, and two digital channels, one toggling every
/// 10 ms.
fn recording() -> Vec<u8> {
    let timebase = Ratio::integer(1_000_000).unwrap();
    let digital = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Digital { initial: false },
        shaping: Default::default(),
    };
    let declaration = Declaration {
        timebase,
        channels: vec![
            DeclaredChannel {
                name: "I".into(),
                source_name: "i".into(),
                source: 0,
                data: DeclaredKind::Analog {
                    unit: "A".into(),
                    period: Ratio::integer(1000).unwrap(),
                    coding: AnalogCoding::Int(IntCoding {
                        scale: Exact::new(1, 1_000_000_000),
                        ..IntCoding::new(32, false)
                    }),
                    significant: Some(4),
                },
                shaping: Default::default(),
            },
            digital("DIO0"),
            digital("DIO1"),
        ],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("verge-energy".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 20,
    };
    let mut w = Writer::new(declaration, WriteOptions::default()).unwrap();
    w.run(0, 0, 0).unwrap();
    for i in 0..2000u64 {
        if i % 10 == 0 {
            w.edge(1, i as i64 * 1000).unwrap();
        }
        w.sample(0, 350_000 + (i % 7) * 1000).unwrap();
    }
    let mut out = w.take_output();
    out.extend(
        w.finish(2_000_000, &[2_000_000; 2], Finish::default())
            .unwrap()
            .tail,
    );
    out
}

fn palette() -> Palette {
    use egui::{Color32, FontId};
    Palette {
        trace: Color32::from_rgb(200, 30, 30),
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
        busy: 0.6,
    }
}

struct Host {
    viewer: Option<Viewer>,
    state: ViewState,
    palette: Palette,
}

#[test]
fn a_frame_works_in_spans_at_debug_level() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spans.tickwave");
    std::fs::write(&path, recording()).unwrap();
    let host = Host {
        viewer: None,
        state: ViewState::default(),
        palette: palette(),
    };
    let mut h = Harness::builder()
        .with_size(egui::vec2(800.0, 400.0))
        .build_ui_state(
            |ui, host: &mut Host| {
                if let Some(viewer) = &host.viewer {
                    ui.add(Graph::new(viewer, &mut host.state, &host.palette, 0).lanes(&[1, 2]));
                }
            },
            host,
        );
    h.state_mut().viewer = Some(Viewer::open(&h.ctx, &path).unwrap());
    for _ in 0..400 {
        h.step();
        if h.state().viewer.as_ref().unwrap().answer().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(h.state().viewer.as_ref().unwrap().answer().is_some());
    let spans = Arc::new(Spans::default());
    tracing::subscriber::with_default(Arc::clone(&spans), || h.step());
    let seen = spans.0.lock().unwrap().clone();
    for name in [
        "graph",
        "layout",
        "build query",
        "axes",
        "trace",
        "tessellate",
        "lane",
        "markers",
    ] {
        assert!(
            seen.contains(&(name, tracing::Level::DEBUG)),
            "{name} in {seen:?}"
        );
    }
    assert_eq!(seen.iter().filter(|s| s.0 == "lane").count(), 2);
    assert!(
        seen.iter().all(|s| s.1 == tracing::Level::DEBUG),
        "{seen:?}"
    );
}
