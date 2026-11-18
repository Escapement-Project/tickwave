//! A track's rows follow the rows and lanes its schema declares.
//!
//!     cargo test -p escapement-tickwave-egui --features decode --test lanes

#![cfg(feature = "decode")]

use std::collections::BTreeMap;

use escapement_tickwave_decode::{Id, KindDecl, KindId, Op, Schema, Span, Status, Store, Tick};
use escapement_tickwave_decoders::{adiv5, cjtag, jtag, swd};
use escapement_tickwave_egui::track::{Density, Labels, Track};
use escapement_tickwave_egui::{Palette, Shown};
use escapement_tickwave_view::format::mapping::Mapping;
use escapement_tickwave_view::format::model::Ratio;

#[test]
fn a_tracks_height_follows_the_rows_and_lanes_its_schema_declares() {
    let shown = Shown::new(0..1000, 0.0..500.0);
    let items = BTreeMap::new();
    let map = Mapping::new(0, 0, Ratio::integer(1).unwrap());
    let density = Density::of(items.values(), &map, |_| false);
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
        2.0 * one,
        "scans, then states"
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
    let density = Density::of(store.items.values(), &map, |_| false);
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
            &store.items,
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
