//! A track's rows follow the lanes its schema declares.
//!
//!     cargo test -p escapement-tickwave-egui --features decode --test lanes

#![cfg(feature = "decode")]

use std::collections::BTreeMap;

use escapement_tickwave_decode::{KindDecl, Schema};
use escapement_tickwave_decoders::{adiv5, swd};
use escapement_tickwave_egui::track::{Density, Labels, Track};
use escapement_tickwave_egui::{Palette, Shown};
use escapement_tickwave_view::format::mapping::Mapping;
use escapement_tickwave_view::format::model::Ratio;

#[test]
fn a_tracks_rows_follow_the_lanes_its_schema_declares() {
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
        "the most any kind declares"
    );
}
