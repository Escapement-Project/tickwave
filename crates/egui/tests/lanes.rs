//! A track's rows follow the rows and lanes its schema declares.
//!
//!     cargo test -p escapement-tickwave-egui --features decode --test lanes

#![cfg(feature = "decode")]

use std::collections::BTreeMap;

use escapement_tickwave_decode::{Id, KindDecl, KindId, Op, Schema, Span, Status, Store, Tick};
use escapement_tickwave_decoders::{adiv5, cjtag, jtag, swd};
use escapement_tickwave_egui::track::{Density, Labels, Track, driver_paints};
use escapement_tickwave_egui::{Palette, Shown};
use escapement_tickwave_view::format::mapping::Mapping;
use escapement_tickwave_view::format::model::Ratio;

#[test]
fn a_tracks_height_follows_the_rows_and_lanes_its_schema_declares() {
    let shown = Shown::new(0..1000, 0.0..500.0);
    let items = BTreeMap::new();
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(&items, &map, |_| false);
    let labels = Labels::default();
    let palette = Palette::from_visuals(&egui::Visuals::default());
    let height = |schema: &Schema, parts: bool| {
        Track::new(
            &shown, "t", schema, &items, &density, &labels, &palette, map,
        )
        .parts(parts)
        .height()
    };
    let swd = swd::definition().schema.clone();
    let adi = adiv5::adi_definition().schema.clone();
    let one = height(&swd, false);
    assert!(one > 0.0);
    assert_eq!(
        height(&adi, false),
        2.0 * one,
        "ADIv5's AP reads declare two lanes"
    );
    assert_eq!(height(&swd, true), 2.0 * one, "and a row for the parts");
    let three = Schema::new(vec![KindDecl::new("a"), KindDecl::new("b").with_lanes(3)]);
    assert_eq!(
        height(&three, true),
        4.0 * one,
        "a row is the most its kinds declare"
    );
    let rows = Schema::new(vec![
        KindDecl::new("a").with_row(2).with_lanes(2),
        KindDecl::new("b"),
        KindDecl::new("c").with_row(2),
    ]);
    assert_eq!(height(&rows, false), 3.0 * one, "rows add up");
    assert_eq!(
        height(&jtag::schema(), false),
        3.0 * one,
        "scans, then states, then the TAPs' parts of a chain's scans"
    );
    assert_eq!(
        height(&cjtag::schema(), false),
        4.0 * one,
        "the format, the level, what is inside it, and the bits"
    );
}

/// The boxes and overflow marks a track draws, counted from the shapes egui paints. `items` are
/// (kind, start, end).
fn drawn(schema: &Schema, items: &[(u16, Tick, Tick)]) -> (usize, usize, f32) {
    let (boxes, marks, height) = boxes(schema, items);
    (boxes.len(), marks, height)
}

/// The boxes a track draws, the count of its overflow marks, and its height.
fn boxes(schema: &Schema, items: &[(u16, Tick, Tick)]) -> (Vec<egui::Rect>, usize, f32) {
    let mut store = Store::with_keep(vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]);
    for &(kind, start, end) in items {
        let id = Id::new(KindId(kind), start);
        let ops = [
            Op::Open {
                id,
                parent: None,
                start,
            },
            Op::Close {
                id,
                end,
                status: Status::Complete,
            },
        ];
        store.apply_batch(&ops, 8).unwrap();
    }
    let shown = Shown::new(0..1000, 0.0..500.0);
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(store.items(), &map, |_| false);
    let labels = Labels::default();
    let palette = Palette::from_visuals(&egui::Visuals::default());
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 200.0),
        )),
        ..egui::RawInput::default()
    };
    let mut height = 0.0;
    let mut out = ctx.run_ui(input, |ui| {
        let track = Track::new(
            &shown,
            "t",
            schema,
            store.items(),
            &density,
            &labels,
            &palette,
            map,
        );
        height = track.height();
        let r = ui.add(track);
        assert_eq!(r.rect.height(), height, "the rows never grow");
    });
    fn walk(s: &egui::Shape, boxes: &mut Vec<egui::Rect>, marks: &mut usize) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, boxes, marks)),
            egui::Shape::Rect(r) if r.stroke.width > 0.0 => boxes.push(r.rect),
            egui::Shape::LineSegment { .. } => *marks += 1,
            _ => {}
        }
    }
    out.textures_delta.clear();
    let (mut boxes, mut marks) = (vec![], 0);
    for c in &out.shapes {
        walk(&c.shape, &mut boxes, &mut marks);
    }
    (boxes, marks, height)
}

#[test]
fn an_item_past_the_declared_lanes_draws_as_an_overflow_mark() {
    // Three overlap from 300 to 500, then one alone.
    let items = [(0, 100, 500), (0, 200, 600), (0, 300, 700), (0, 800, 900)];
    let one = Schema::new(vec![KindDecl::new("a")]);
    let two = Schema::new(vec![KindDecl::new("a").with_lanes(2)]);
    let three = Schema::new(vec![KindDecl::new("a").with_lanes(3)]);
    let (b1, m1, h1) = drawn(&one, &items);
    let (b2, m2, h2) = drawn(&two, &items);
    let (b3, m3, h3) = drawn(&three, &items);
    assert_eq!(
        (b1, m1),
        (2, 2),
        "one lane: the first and the last, two marks"
    );
    assert_eq!((b2, m2), (3, 1), "two lanes: one mark");
    assert_eq!((b3, m3), (4, 0), "three lanes: room for all");
    assert_eq!((h2, h3), (2.0 * h1, 3.0 * h1));
}

#[test]
fn kinds_in_different_rows_do_not_compete_for_lanes() {
    // A long item of kind 0, and two of kind 1 overlapping it, the first starting before it.
    let items = [(1, 100, 300), (0, 200, 900), (1, 400, 600)];
    let shared = Schema::new(vec![KindDecl::new("a"), KindDecl::new("b")]);
    let rows = Schema::new(vec![KindDecl::new("a"), KindDecl::new("b").with_row(1)]);
    let (b1, m1, h1) = drawn(&shared, &items);
    assert_eq!((b1, m1), (2, 1), "one row: the long one is a mark");
    let (b2, m2, h2) = boxes(&rows, &items);
    assert_eq!(
        (b2.len(), m2, h2),
        (3, 0, 2.0 * h1),
        "a row each: every item a box"
    );
    let long = (b2.iter())
        .max_by(|a, b| a.width().total_cmp(&b.width()))
        .unwrap();
    assert!(
        b2.iter().all(|r| r == long || r.top() >= long.bottom()),
        "kind 1 in the row under kind 0's: {b2:?}"
    );
}

#[test]
fn lost_input_shades_the_spans_the_window_meets_and_no_others() {
    let lost = [
        (-500, -400),
        (-100, 50),
        (200, 300),
        (900, 1100),
        (1200, 1300),
    ]
    .map(|(start, end)| Span { start, end });
    let items = BTreeMap::new();
    let shown = Shown::new(0..1000, 0.0..500.0);
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(&items, &map, |_| false);
    let labels = Labels::default();
    let palette = Palette::from_visuals(&egui::Visuals::default());
    let schema = Schema::new(vec![KindDecl::new("a")]);
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 200.0),
        )),
        ..egui::RawInput::default()
    };
    let mut out = egui::Context::default().run_ui(input, |ui| {
        ui.add(
            Track::new(
                &shown, "t", &schema, &items, &density, &labels, &palette, map,
            )
            .settled(Tick::MAX)
            .lost(&lost),
        );
    });
    out.textures_delta.clear();
    let fill = palette.attention_fill.gamma_multiply(palette.shade);
    let mut shaded = vec![];
    fn walk(s: &egui::Shape, fill: egui::Color32, out: &mut Vec<egui::Rect>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, fill, out)),
            egui::Shape::Rect(r) if r.fill == fill => out.push(r.rect),
            _ => {}
        }
    }
    for c in &out.shapes {
        walk(&c.shape, fill, &mut shaded);
    }
    assert_eq!(shaded.len(), 3, "{shaded:?}");
    assert!(shaded.windows(2).all(|w| w[0].right() < w[1].left()));
}

#[test]
fn a_long_or_open_item_from_far_before_the_window_still_draws_beside_the_short_ones() {
    // A long item and an open one across thousands of short ones, each in its own row.
    let schema = Schema::new(vec![
        KindDecl::new("long"),
        KindDecl::new("open").with_row(1),
        KindDecl::new("short").with_row(2),
    ]);
    let mut store = Store::with_keep(vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]);
    let mut ops = vec![];
    let open = |id, start| Op::Open {
        id,
        parent: None,
        start,
    };
    let close = |id, end| Op::Close {
        id,
        end,
        status: Status::Complete,
    };
    let long = Id::new(KindId(0), 0);
    ops.extend([open(long, 0), close(long, 200_000)]);
    ops.push(open(Id::new(KindId(1), 50_000), 50_000));
    let mut short = 0;
    for start in (10..200_000).step_by(20) {
        let id = Id::new(KindId(2), start);
        ops.extend([open(id, start), close(id, start + 15)]);
        short += usize::from(start < 101_000 && start + 15 >= 100_000);
    }
    for chunk in ops.chunks(1024) {
        store.apply_batch(chunk, 8).unwrap();
    }
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(store.items(), &map, |_| false);
    let labels = Labels::default();
    let palette = Palette::from_visuals(&egui::Visuals::default());
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(5000.0, 200.0),
        )),
        ..egui::RawInput::default()
    };
    let shown = Shown::new(100_000..101_000, 0.0..5000.0);
    let mut out = egui::Context::default().run_ui(input, |ui| {
        ui.add(
            Track::new(
                &shown,
                "t",
                &schema,
                store.items(),
                &density,
                &labels,
                &palette,
                map,
            )
            .settled(Tick::MAX),
        );
    });
    out.textures_delta.clear();
    let mut boxes = vec![];
    fn walk(s: &egui::Shape, out: &mut Vec<egui::Rect>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            egui::Shape::Rect(r) if r.stroke.width > 0.0 => out.push(r.rect),
            _ => {}
        }
    }
    for c in &out.shapes {
        walk(&c.shape, &mut boxes);
    }
    let width = boxes.iter().map(|r| r.width()).fold(0.0, f32::max);
    let whole = boxes.iter().filter(|r| r.width() >= width - 1.0).count();
    assert_eq!(
        boxes.len(),
        short + 2,
        "every short item meeting it, and both"
    );
    assert_eq!(whole, 2, "the long and the open one span the window");
}

#[test]
fn a_density_over_items_either_side_of_zero_is_built_and_drawn() {
    let mut store = Store::with_keep(vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]);
    let mut n = 0;
    for start in (-100_000..100_000).step_by(20) {
        let id = Id::new(KindId(0), start);
        let ops = [
            Op::Open {
                id,
                parent: None,
                start,
            },
            Op::Close {
                id,
                end: start + 10,
                status: Status::Complete,
            },
        ];
        store.apply_batch(&ops, 8).unwrap();
        n += 1;
    }
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(store.items(), &map, |_| false);
    assert_eq!(density.items(), n);
    // Zoomed out across zero, as density bins.
    let shown = Shown::new(-100_000..100_000, 0.0..500.0);
    let (labels, palette) = (
        Labels::default(),
        Palette::from_visuals(&egui::Visuals::default()),
    );
    let schema = Schema::new(vec![KindDecl::new("a")]);
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 200.0),
        )),
        ..egui::RawInput::default()
    };
    let mut out = egui::Context::default().run_ui(input, |ui| {
        ui.add(
            Track::new(
                &shown,
                "t",
                &schema,
                store.items(),
                &density,
                &labels,
                &palette,
                map,
            )
            .settled(Tick::MAX),
        );
    });
    out.textures_delta.clear();
    assert!(!out.shapes.is_empty());
}

#[test]
fn a_wire_is_painted_by_its_parts_only_where_the_track_draws_their_items_apart() {
    // An item every 100 ticks, each with a part driven by the probe over its first half.
    let schema = Schema::new(vec![KindDecl::new("a").with_fields(vec![
        escapement_tickwave_decode::field("p", escapement_tickwave_decode::FieldType::Phase),
    ])])
    .with_roles(["probe"]);
    let mut store = Store::with_keep(vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]);
    for start in (0..100_000).step_by(100) {
        let id = Id::new(KindId(0), start);
        let ops = [
            Op::Open {
                id,
                parent: None,
                start,
            },
            Op::Set {
                id,
                field: escapement_tickwave_decode::FieldId(0),
                value: escapement_tickwave_decode::Value::Phase,
                span: Some(Span {
                    start,
                    end: start + 50,
                }),
                driver: Some(escapement_tickwave_decode::Role(0)),
            },
            Op::Close {
                id,
                end: start + 100,
                status: Status::Complete,
            },
        ];
        store.apply_batch(&ops, 8).unwrap();
    }
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(store.items(), &map, |_| false);
    let paints = |window: std::ops::Range<i64>, width: f32| {
        driver_paints(&schema, store.items(), &density, &map, &window, width)
    };
    let few = paints(10_000..20_000, 1000.0);
    assert_eq!(few.len(), 100, "a hundred items, ten points each");
    assert!(
        few.iter()
            .all(|p| p.role == 0 && p.span.end - p.span.start == 50)
    );
    assert!(
        paints(0..100_000, 1000.0).is_empty(),
        "a point an item: density"
    );
    assert_eq!(paints(0..100_000, 4000.0).len(), 1000, "four points each");
}

/// Every shape a track of `items` (kind, start, end) draws over `window`, provisional or not.
fn shapes_of(
    items: &[(u16, Tick, Tick)],
    window: std::ops::Range<i64>,
    provisional: bool,
) -> Vec<egui::Shape> {
    shapes_on(items, window, provisional, None, None)
}

/// As `shapes_of`, under a graph whose lanes were drawn in `grid`'s columns.
fn shapes_on(
    items: &[(u16, Tick, Tick)],
    window: std::ops::Range<i64>,
    provisional: bool,
    grid: Option<escapement_tickwave_view::columns::Grid>,
    lit: Option<Span>,
) -> Vec<egui::Shape> {
    let schema = Schema::new(vec![KindDecl::new("a")]);
    let mut store = Store::with_keep(vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]);
    for &(kind, start, end) in items {
        let id = Id::new(KindId(kind), start);
        let ops = [
            Op::Open {
                id,
                parent: None,
                start,
            },
            Op::Close {
                id,
                end,
                status: Status::Complete,
            },
        ];
        store.apply_batch(&ops, 8).unwrap();
    }
    let mut shown = Shown::new(window, 0.0..500.0);
    shown.grid = grid;
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(store.items(), &map, |_| false);
    let (labels, palette) = (
        Labels::default(),
        Palette::from_visuals(&egui::Visuals::default()),
    );
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 200.0),
        )),
        ..egui::RawInput::default()
    };
    let mut out = egui::Context::default().run_ui(input, |ui| {
        ui.add(
            Track::new(
                &shown,
                "t",
                &schema,
                store.items(),
                &density,
                &labels,
                &palette,
                map,
            )
            .provisional(provisional)
            .lit(lit),
        );
    });
    out.textures_delta.clear();
    fn flat(s: egui::Shape, out: &mut Vec<egui::Shape>) {
        match s {
            egui::Shape::Vec(v) => v.into_iter().for_each(|s| flat(s, out)),
            s => out.push(s),
        }
    }
    let mut all = vec![];
    for c in out.shapes {
        flat(c.shape, &mut all);
    }
    all
}

/// Zoomed out, the density's bins lie in the lanes' columns where the graph drew them, here a
/// third of a point off the plot's own; without them, in the plot's points.
#[test]
fn a_crowded_tracks_bins_lie_in_the_lanes_columns() {
    use escapement_tickwave_view::columns::Grid;
    let items: Vec<(u16, Tick, Tick)> = (0..5000).map(|k| (0, 2 * k, 2 * k + 1)).collect();
    // Each bin's left edge, past the first, which the plot's edge clips.
    let lefts = |grid: Option<Grid>| -> Vec<f32> {
        let shapes = shapes_on(&items, 0..10_000, false, grid, None);
        let mut xs: Vec<f32> = (shapes.iter())
            .filter_map(|s| match s {
                egui::Shape::Rect(r) if r.stroke.width == 0.0 => Some(r.rect.left()),
                _ => None,
            })
            .collect();
        xs.sort_by(f32::total_cmp);
        xs.split_off(1)
    };
    let own = lefts(None);
    assert!(own.len() > 400, "{}", own.len());
    assert!(own.iter().all(|x| (x - x.round()).abs() < 1e-3), "{own:?}");
    let lanes = lefts(Some(Grid {
        x0: 7.0,
        per: 20.0,
        count: 1500,
    }));
    assert!(lanes.len() > 400, "{}", lanes.len());
    assert!(
        lanes.iter().all(|x| (x - x.floor() - 0.35).abs() < 1e-3),
        "{lanes:?}"
    );
}

#[test]
fn a_provisional_windows_items_draw_pale_with_dashed_outlines() {
    let items = [(0, 100, 300), (0, 400, 600)];
    let outlined = |shapes: &[egui::Shape]| {
        (shapes.iter())
            .filter(|s| matches!(s, egui::Shape::Rect(r) if r.stroke.width > 0.0))
            .count()
    };
    let dashes = |shapes: &[egui::Shape]| -> Vec<egui::Color32> {
        (shapes.iter())
            .filter_map(|s| match s {
                egui::Shape::LineSegment { stroke, .. } => Some(stroke.color),
                _ => None,
            })
            .collect()
    };
    let plain = shapes_of(&items, 0..1000, false);
    assert_eq!((outlined(&plain), dashes(&plain).len()), (2, 0));
    let pale = shapes_of(&items, 0..1000, true);
    let d = dashes(&pale);
    assert_eq!(outlined(&pale), 0, "no solid outline");
    assert!(d.len() > 20, "two outlines in dashes: {}", d.len());
    let ink = Palette::from_visuals(&egui::Visuals::default()).ink_2;
    assert!(d.iter().all(|c| c.a() < ink.a()), "at a lower opacity");

    // Zoomed out to density, a dashed baseline under the bins.
    let many: Vec<_> = (0..2_000).map(|k| (0, k * 10, k * 10 + 5)).collect();
    let dense = |provisional| dashes(&shapes_of(&many, 0..20_000, provisional)).len();
    assert_eq!(dense(false), 0);
    assert!(dense(true) > 20, "{}", dense(true));
}

/// The texts a track over one item of kind `a`, its field `parity` set to `ok` over `part`, draws
/// with `labels`, its parts row shown.
fn texts(labels: Labels, part: (Tick, Tick)) -> Vec<String> {
    use escapement_tickwave_decode::{FieldDecl, FieldId, FieldType, Label, Value};
    let schema = Schema::new(vec![
        KindDecl::new("a")
            .with_fields(vec![
                FieldDecl::new("parity", FieldType::Name(vec!["ok".into(), "bad".into()]))
                    .with_short("P"),
            ])
            .with_labels(vec![Label::default().text("parity ").field(FieldId(0))]),
    ]);
    let mut store = Store::with_keep(vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]);
    let id = Id::new(KindId(0), 100);
    let ops = [
        Op::Open {
            id,
            parent: None,
            start: 100,
        },
        Op::Set {
            id,
            field: FieldId(0),
            value: Value::Name(0),
            span: Some(Span {
                start: part.0,
                end: part.1,
            }),
            driver: None,
        },
        Op::Close {
            id,
            end: 900,
            status: Status::Complete,
        },
    ];
    store.apply_batch(&ops, 8).unwrap();
    let shown = Shown::new(0..1000, 0.0..500.0);
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(store.items(), &map, |_| false);
    let palette = Palette::from_visuals(&egui::Visuals::default());
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 200.0),
        )),
        ..egui::RawInput::default()
    };
    let mut out = egui::Context::default().run_ui(input, |ui| {
        ui.add(
            Track::new(
                &shown,
                "t",
                &schema,
                store.items(),
                &density,
                &labels,
                &palette,
                map,
            )
            .parts(true),
        );
    });
    out.textures_delta.clear();
    fn walk(s: &egui::Shape, out: &mut Vec<String>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
            _ => {}
        }
    }
    let mut all = vec![];
    for c in &out.shapes {
        walk(&c.shape, &mut all);
    }
    all
}

#[test]
fn a_track_shows_its_schemas_labels_unless_the_host_gives_its_own() {
    use escapement_tickwave_decode::Label;
    let wide = (200, 800);
    assert!(texts(Labels::default(), wide).contains(&"parity ok".to_owned()));
    let host = Labels::default().kind(KindId(0), vec![Label::default().text("host's")]);
    let drawn = texts(host, wide);
    assert!(drawn.contains(&"host's".to_owned()), "{drawn:?}");
    assert!(!drawn.contains(&"parity ok".to_owned()));
}

#[test]
fn a_part_too_narrow_for_its_fields_name_shows_the_short_name_it_declares() {
    // 16 points: too narrow for "ok", "parity" or "par", room for "P".
    let drawn = texts(Labels::default(), (200, 232));
    assert!(drawn.contains(&"P".to_owned()), "{drawn:?}");
    assert!(
        !drawn
            .iter()
            .any(|t| t == "par" || t == "parity" || t == "ok")
    );
}

/// What a click at `at` on a track of two items selects, and the outlines it then draws with
/// `selected`: the id clicked, and each outlined box's stroke width.
fn click(at: egui::Pos2, selected: Option<Id>) -> (Option<Id>, Vec<f32>) {
    let items = [(0, 100, 300), (0, 400, 600)];
    let schema = Schema::new(vec![KindDecl::new("a")]);
    let mut store = Store::with_keep(vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]);
    for &(kind, start, end) in &items {
        let id = Id::new(KindId(kind), start);
        let ops = [
            Op::Open {
                id,
                parent: None,
                start,
            },
            Op::Close {
                id,
                end,
                status: Status::Complete,
            },
        ];
        store.apply_batch(&ops, 8).unwrap();
    }
    let shown = Shown::new(0..1000, 0.0..500.0);
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(store.items(), &map, |_| false);
    let (labels, palette) = (
        Labels::default(),
        Palette::from_visuals(&egui::Visuals::default()),
    );
    let ctx = egui::Context::default();
    let mut clicked = None;
    let mut widths = vec![];
    let press = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    for events in [
        vec![egui::Event::PointerMoved(at)],
        vec![press(true)],
        vec![press(false)],
    ] {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(600.0, 200.0),
            )),
            events,
            ..egui::RawInput::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            let r = ui.add(
                Track::new(
                    &shown,
                    "t",
                    &schema,
                    store.items(),
                    &density,
                    &labels,
                    &palette,
                    map,
                )
                .selected(selected),
            );
            clicked = clicked.or(Track::clicked(&r));
        });
        out.textures_delta.clear();
        widths.clear();
        fn walk(s: &egui::Shape, out: &mut Vec<f32>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Rect(r) if r.stroke.width > 0.0 => out.push(r.stroke.width),
                _ => {}
            }
        }
        for c in &out.shapes {
            walk(&c.shape, &mut widths);
        }
    }
    (clicked, widths)
}

#[test]
fn a_click_selects_the_item_under_it_and_the_selected_one_is_outlined_in_ink() {
    // The plot runs from x 0 to 500 over ticks 0 to 1000: the second item is at x 200 to 300.
    let row = 12.0;
    let second = Id::new(KindId(0), 400);
    let (got, plain) = click(egui::pos2(250.0, row), None);
    assert_eq!(got, Some(second));
    assert_eq!(plain, vec![1.0, 1.0]);
    let (none, _) = click(egui::pos2(350.0, row), None);
    assert_eq!(none, None, "a click between items selects nothing");
    let (_, outlined) = click(egui::pos2(350.0, row), Some(second));
    assert_eq!(outlined, vec![1.0, 2.0], "the selected one heavier");
}

/// A span lit from elsewhere washes the track between two hairlines at its ends, over every row.
#[test]
fn a_lit_span_washes_the_track_between_hairlines() {
    let palette = Palette::from_visuals(&egui::Visuals::default());
    let items = [(0, 100, 200), (0, 400, 500)];
    let lit = Span {
        start: 400,
        end: 500,
    };
    let washes = |shapes: &[egui::Shape]| -> Vec<egui::Rect> {
        (shapes.iter())
            .filter_map(|s| match s {
                egui::Shape::Rect(r) if r.fill == palette.wash => Some(r.rect),
                _ => None,
            })
            .collect()
    };
    let lines = |shapes: &[egui::Shape]| -> Vec<f32> {
        (shapes.iter())
            .filter_map(|s| match s {
                egui::Shape::LineSegment { points, stroke } if stroke.color == palette.ink_2 => {
                    Some(points[0].x)
                }
                _ => None,
            })
            .collect()
    };
    let plain = shapes_on(&items, 0..1000, false, None, None);
    let shapes = shapes_on(&items, 0..1000, false, None, Some(lit));
    assert_eq!(washes(&shapes).len(), washes(&plain).len() + 1);
    let band = *washes(&shapes).last().unwrap();
    // The plot is 500 points for 1000 ticks.
    assert_eq!((band.left(), band.right()), (200.0, 250.0));
    let mut edges = lines(&shapes);
    for x in lines(&plain) {
        let k = edges.iter().position(|&e| e == x).unwrap();
        edges.remove(k);
    }
    assert_eq!(edges, [200.0, 250.0]);
    // Off the window, nothing.
    let past = Span {
        start: 2000,
        end: 3000,
    };
    let off = shapes_on(&items, 0..1000, false, None, Some(past));
    assert_eq!(washes(&off).len(), washes(&plain).len());
}

/// The texts a track named `name` of `schema` draws in its gutter, each with where it lies, over
/// a plot starting where `track::gutter` says the gutter ends; and that point.
fn gutter_texts(name: &str, schema: &Schema) -> (f32, Vec<(String, egui::Rect)>) {
    let items = BTreeMap::new();
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(&items, &map, |_| false);
    let (labels, palette) = (
        Labels::default(),
        Palette::from_visuals(&egui::Visuals::default()),
    );
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(700.0, 200.0),
        )),
        ..egui::RawInput::default()
    };
    let mut plot = 0.0;
    let mut out = egui::Context::default().run_ui(input, |ui| {
        let left = ui.max_rect().left();
        plot = left + escapement_tickwave_egui::track::gutter(ui, &palette, name, schema);
        let shown = Shown::new(0..1000, plot..700.0);
        ui.add(Track::new(
            &shown, name, schema, &items, &density, &labels, &palette, map,
        ));
    });
    out.textures_delta.clear();
    fn walk(s: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            egui::Shape::Text(t) => {
                let laid = t.galley.rect.translate(t.pos.to_vec2());
                out.push((t.galley.text().to_owned(), laid));
            }
            _ => {}
        }
    }
    let mut all = vec![];
    for c in &out.shapes {
        walk(&c.shape, &mut all);
    }
    (plot, all)
}

#[test]
fn a_tracks_gutter_holds_its_name_and_each_rows_name_beside_the_row() {
    use escapement_tickwave_egui::track::ROW;
    let (plot, texts) = gutter_texts("JTAG", &jtag::schema());
    let at = |t: &str| {
        (texts.iter())
            .find(|(s, _)| s == t)
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("{t} not drawn: {texts:?}"))
    };
    let name = at("JTAG");
    let top = name.center().y - ROW / 2.0;
    for (lane, row) in ["scans", "states", "TAPs"].into_iter().enumerate() {
        let r = at(row);
        assert!(
            r.right() <= plot,
            "{row} ends before the plot: {r:?}, {plot}"
        );
        let centre = top + (lane as f32 + 0.5) * ROW;
        assert!(
            (r.center().y - centre).abs() < 1.0,
            "{row} beside lane {lane}"
        );
        assert!(name.right() < r.left(), "the name clear of {row}");
    }
    assert_eq!(texts.len(), 4, "{texts:?}");
    // A schema that names no rows: the name alone, and the gutter only as wide as it.
    let (plot, texts) = gutter_texts("SWD", &swd::definition().schema);
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(texts[0].1.right() < plot && plot - texts[0].1.right() < 12.0);
}

#[test]
fn a_label_naming_a_field_the_item_lacks_gives_way_to_the_next() {
    use escapement_tickwave_decode::{FieldDecl, FieldId, FieldType, Label};
    let schema = Schema::new(vec![
        KindDecl::new("a")
            .with_fields(vec![FieldDecl::new("value", FieldType::Word { bits: 8 })])
            .with_labels(vec![
                Label::default()
                    .text("read ")
                    .field(FieldId(0))
                    .text(" done"),
                Label::default().text("read"),
            ]),
    ]);
    let mut store = Store::with_keep(vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]);
    let id = Id::new(KindId(0), 100);
    let ops = [
        Op::Open {
            id,
            parent: None,
            start: 100,
        },
        Op::Close {
            id,
            end: 900,
            status: Status::Complete,
        },
    ];
    store.apply_batch(&ops, 8).unwrap();
    let shown = Shown::new(0..1000, 0.0..500.0);
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(store.items(), &map, |_| false);
    let palette = Palette::from_visuals(&egui::Visuals::default());
    let labels = Labels::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 200.0),
        )),
        ..egui::RawInput::default()
    };
    let mut out = egui::Context::default().run_ui(input, |ui| {
        ui.add(Track::new(
            &shown,
            "t",
            &schema,
            store.items(),
            &density,
            &labels,
            &palette,
            map,
        ));
    });
    out.textures_delta.clear();
    fn walk(s: &egui::Shape, out: &mut Vec<String>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
            _ => {}
        }
    }
    let mut drawn = vec![];
    for c in &out.shapes {
        walk(&c.shape, &mut drawn);
    }
    assert!(drawn.contains(&"read".to_owned()), "{drawn:?}");
    assert!(!drawn.iter().any(|t| t.contains("done")), "{drawn:?}");
}

/// Resting the pointer on a box shows its fullest label, its start and its length, even where the
/// box is too narrow for any label.
#[test]
fn resting_the_pointer_on_a_box_gives_its_label_start_and_length() {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use escapement_tickwave_decode::{FieldDecl, FieldId, FieldType, Label, Value};
    let schema = Schema::new(vec![
        KindDecl::new("a")
            .with_fields(vec![FieldDecl::new("value", FieldType::Word { bits: 8 })])
            .with_labels(vec![
                Label::default().text("value ").field(FieldId(0)),
                Label::default().text("v"),
            ]),
    ]);
    let mut store = Store::with_keep(vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]);
    // 4 points wide at 2 ticks a point: too narrow for either label.
    let id = Id::new(KindId(0), 100);
    let ops = [
        Op::Open {
            id,
            parent: None,
            start: 100,
        },
        Op::Set {
            id,
            field: FieldId(0),
            value: Value::Word {
                value: 0x2A,
                bits: 8,
            },
            span: None,
            driver: None,
        },
        Op::Close {
            id,
            end: 108,
            status: Status::Complete,
        },
    ];
    store.apply_batch(&ops, 8).unwrap();
    let shown = Shown::new(0..1000, 0.0..500.0);
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(store.items(), &map, |_| false);
    let palette = Palette::from_visuals(&egui::Visuals::default());
    let labels = Labels::default();
    // A tick a microsecond.
    let timebase = Ratio::integer(1_000_000).unwrap();
    let corner = std::cell::Cell::new(egui::Pos2::ZERO);
    let mut h = Harness::builder()
        .with_size(egui::vec2(600.0, 200.0))
        .build_ui(|ui| {
            ui.ctx().global_style_mut(|s| {
                s.interaction.tooltip_delay = 0.0;
                s.interaction.show_tooltips_only_when_still = false;
            });
            corner.set(ui.max_rect().left_top());
            ui.add(
                Track::new(
                    &shown,
                    "t",
                    &schema,
                    store.items(),
                    &density,
                    &labels,
                    &palette,
                    map,
                )
                .timebase(timebase),
            );
        });
    h.run();
    // The plot runs from the ui's left to 500, over ticks 0 to 1000; the box's middle is at 104.
    let corner = corner.get();
    let x = corner.x + 104.0 * (500.0 - corner.x) / 1000.0;
    h.event(egui::Event::PointerMoved(egui::pos2(x, corner.y + 12.0)));
    h.run_steps(4);
    // The tooltip and its text both carry it.
    let shows = |h: &Harness<'_>, t: &str| h.query_all_by_label_contains(t).next().is_some();
    // Figures and units joined by a no-break space.
    assert!(shows(&h, "value 0x2A\n100\u{a0}µs for 8\u{a0}µs"));
    // Away from the box, nothing.
    h.event(egui::Event::PointerMoved(egui::pos2(
        300.0,
        corner.y + 12.0,
    )));
    h.run_steps(4);
    assert!(!shows(&h, "value 0x2A"));
}

/// The 2-point line segments a crowded track of 5000 short items draws with `selected`, each its
/// x and colour: 20 ticks a point, so item k, starting at tick 2k, starts at 0.1 k points. Each is
/// anchored at its start but item 2000's, anchored a tick after.
fn selection_ticks(selected: Id, provisional: bool) -> Vec<(f32, egui::Color32)> {
    let schema = Schema::new(vec![KindDecl::new("a")]);
    let mut store = Store::with_keep(vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]);
    for k in 0..5000 {
        let (start, end) = (2 * k, 2 * k + 1);
        let id = Id::new(KindId(0), if k == 2000 { start + 1 } else { start });
        let ops = [
            Op::Open {
                id,
                parent: None,
                start,
            },
            Op::Close {
                id,
                end,
                status: Status::Complete,
            },
        ];
        store.apply_batch(&ops, 8).unwrap();
    }
    let shown = Shown::new(0..10_000, 0.0..500.0);
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(store.items(), &map, |_| false);
    let (labels, palette) = (
        Labels::default(),
        Palette::from_visuals(&egui::Visuals::default()),
    );
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(600.0, 200.0),
        )),
        ..egui::RawInput::default()
    };
    let mut out = egui::Context::default().run_ui(input, |ui| {
        ui.add(
            Track::new(
                &shown,
                "t",
                &schema,
                store.items(),
                &density,
                &labels,
                &palette,
                map,
            )
            .selected(Some(selected))
            .provisional(provisional),
        );
    });
    out.textures_delta.clear();
    fn walk(s: &egui::Shape, out: &mut Vec<(f32, egui::Color32)>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            egui::Shape::LineSegment { points, stroke } if stroke.width == 2.0 => {
                out.push((points[0].x, stroke.color))
            }
            _ => {}
        }
    }
    let mut ticks = vec![];
    for c in &out.shapes {
        walk(&c.shape, &mut ticks);
    }
    ticks
}

#[test]
fn zoomed_out_the_selected_item_keeps_an_ink_tick_at_its_start() {
    let ink = Palette::from_visuals(&egui::Visuals::default()).ink;
    let item = Id::new(KindId(0), 2000);
    assert_eq!(selection_ticks(item, false), [(100.0, ink)]);
    assert_eq!(
        selection_ticks(item, true),
        [(100.0, ink)],
        "in ink over a provisional window's pale density"
    );
    let anchored_after = Id::new(KindId(0), 4001);
    assert_eq!(
        selection_ticks(anchored_after, false),
        [(200.0, ink)],
        "at its start"
    );
    // An item the track does not hold: at its anchor.
    let held_elsewhere = Id::new(KindId(0), 3001);
    assert_eq!(selection_ticks(held_elsewhere, false), [(150.05, ink)]);
    let past = Id::new(KindId(0), 20_000);
    assert!(
        selection_ticks(past, false).is_empty(),
        "none off the window"
    );
}
