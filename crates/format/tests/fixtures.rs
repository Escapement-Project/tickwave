//! The committed recordings the format's own examples write: the crafted ones each refused for
//! what it states, and the energy capture holding what its example says.

use std::path::PathBuf;

use escapement_tickwave_format::annotations::{Included, resolve};
use escapement_tickwave_format::decode::{
    BlockChannel, DecodeError, Footer, Index, derived_values, from_memory,
};
use escapement_tickwave_format::header::{AnalogCoding, ChannelKind, Exact, IntCoding};
use escapement_tickwave_format::model::{Break, Gap, Ratio};
use escapement_tickwave_format::samples::Values;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../web/fixtures")
            .join(name),
    )
    .unwrap()
}

/// Channel 0's first block of `file`, decoded through its footer and its index page.
fn first_block(file: &[u8]) -> Result<escapement_tickwave_format::decode::Block, DecodeError> {
    let head = Footer::parse_file(file)?;
    let blocks = head.block_refs(0, from_memory(file))?;
    let b = &blocks[0];
    head.decode_block(
        0,
        b,
        &file[b.row.offset as usize..][..b.row.length as usize],
    )
}

#[test]
fn each_crafted_recording_is_refused_for_what_it_states() {
    for (name, why) in [
        ("header-wrap", "the footer's lengths do not fit it"),
        (
            "levels",
            "channel A: the overview levels are not the ones the format sets for this length",
        ),
        (
            "big-decoded",
            "it decodes to 2147483648 bytes, more than this reader holds (67108864)",
        ),
        (
            "huge-count",
            "channel A, the block at tick 0: the section is too short for its edges",
        ),
        (
            "page-count",
            "channel 0: page 0 at depth 0 of its blocks: its head differs from where the table's \
             shape puts it",
        ),
    ] {
        let file = fixture(&format!("crafted/{name}.tickwave"));
        let err = first_block(&file).expect_err(name);
        assert_eq!(err.to_string(), why, "{name}");
    }
}

#[test]
fn the_energy_fixture_holds_what_its_example_says() {
    let file = fixture("energy.tickwave");
    assert!(file.len() < 64 << 10);
    let head = Footer::parse_file(&file).unwrap();
    let h = &head.header;
    assert_eq!(h.timebase, Ratio::integer(96_000_000).unwrap());
    let [source] = h.sources.as_slice() else {
        panic!("one source")
    };
    assert_eq!(
        (
            source.format.as_str(),
            source.files.len(),
            source.timebase,
            source.origin
        ),
        ("verge-energy", 0, h.timebase, 0)
    );
    assert_eq!(source.extra["end_reason"], "stopped");
    let int = |den| {
        AnalogCoding::Int(IntCoding {
            scale: Exact::new(1, den),
            ..IntCoding::new(32, false)
        })
    };
    let lost = [
        Break {
            index: 1000,
            tick: 1_008_000,
            lost: 50,
        },
        Break {
            index: 1800,
            tick: 2_000_000,
            lost: 0,
        },
    ];
    for (c, unit, coding) in [(0, "V", int(1000)), (1, "A", int(1_000_000_000))] {
        let ChannelKind::Analog {
            unit: u,
            period,
            count,
            breaks,
            coding: got,
            significant,
            ..
        } = &h.channels[c].data
        else {
            panic!()
        };
        assert_eq!(
            (
                u.as_str(),
                *period,
                *count,
                breaks.as_slice(),
                *got,
                *significant
            ),
            (
                unit,
                Ratio::integer(960).unwrap(),
                2600,
                &lost[..],
                coding,
                Some(4)
            )
        );
    }
    // A loss: one period after the pair at 959,040, for 50 periods, ends at the run's tick.
    assert_eq!(999 * 960 + 960 + 50 * 960, lost[0].tick);
    let names: Vec<&str> = h.channels.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["V", "I", "range", "under_range", "DIO0"]);
    assert_eq!(h.derived[0].product, ["I".to_owned(), "V".to_owned()]);
    let ChannelKind::Digital { gaps, edges, .. } = &h.channels[4].data else {
        panic!()
    };
    assert_eq!(
        (gaps.as_slice(), *edges),
        (
            &[Gap {
                from: 960_000,
                to: 1_008_000,
                level: true
            }][..],
            8
        )
    );
    // A flush every 480,000 ticks: six blocks of V and of I, one a flush.
    assert_eq!((h.channels[0].blocks, h.channels[1].blocks), (6, 6));
    let index = Index::of_file(&file).unwrap();
    let dio = index.block_in(&file, 4, 0).unwrap();
    let BlockChannel::Digital { edges, .. } = &dio.data else {
        panic!()
    };
    assert_eq!(edges[..2], [0, 0], "the pulse at tick 0");
    // P's first sample: I's 1,000,000 nA times V's 3,300 mV, 3,300,000,000 pW.
    let (v, i) = (
        index.block_in(&file, 0, 0).unwrap(),
        index.block_in(&file, 1, 0).unwrap(),
    );
    let Values::U64(p) = derived_values(&i.data, &v.data, head.products[0].2).unwrap() else {
        panic!("an integer product")
    };
    assert_eq!(p[0], 3_300_000_000);
    // The bundled sidecar checks against the recording.
    let files = head.read_bundle(from_memory(&file)).unwrap().unwrap();
    let resolved = resolve(
        &files["energy.annotations.toml"],
        &[Included {
            name: "energy.verge.jsonl",
            text: &files["energy.verge.jsonl"],
        }],
        h,
        Some("energy.tickwave"),
    )
    .unwrap();
    let kinds: Vec<_> = resolved
        .iter()
        .map(|a| a.kind.as_deref().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["marker", "marker", "marker-after-loss", "marker", "marker"]
    );
    assert_eq!(resolved[2].values.as_ref().unwrap()["number"], "3");
}
