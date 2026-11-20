//! This crate's decoders as a registry that binds recipes (`escapement_tickwave_decode::recipe`),
//! with the stacks in `recipes/` as its presets.

use crate::{adiv5, cjtag, jtag, jtagdp, swd};
use escapement_tickwave_decode::recipe::{Entry, Preset, Recipe, Registry};

/// The presets: each `recipes/` file a whole stack, by the name it is offered under.
const PRESETS: [(&str, &str); 3] = [
    ("SWD, ADIv5, MEM-AP", include_str!("../recipes/swd.toml")),
    (
        "JTAG, JTAG-DP, ADIv5, MEM-AP",
        include_str!("../recipes/jtag-adi.toml"),
    ),
    (
        "cJTAG, JTAG, JTAG-DP, ADIv5, MEM-AP",
        include_str!("../recipes/cjtag-adi.toml"),
    ),
];

/// This crate's decoders, and its presets.
pub fn registry() -> Registry {
    Registry {
        decoders: vec![
            Entry::new(swd::definition(), "SWD", swd::Swd::factory),
            Entry::new(jtag::definition(), "JTAG", jtag::Jtag::factory),
            Entry::new(cjtag::definition(), "cJTAG", cjtag::Cjtag::factory),
            Entry::new(jtagdp::definition(), "JTAG-DP", |_| {
                jtagdp::JtagDp::factory()
            }),
            Entry::new(adiv5::adi_definition(), "ADIv5", |_| adiv5::Adi::factory()),
            Entry::new(adiv5::mem_definition(), "MEM-AP", |_| adiv5::Mem::factory()),
        ],
        presets: (PRESETS.iter())
            .map(|&(name, text)| Preset {
                name: name.to_owned(),
                recipe: Recipe::parse(text).expect("a shipped recipe reads"),
            })
            .collect(),
    }
}
