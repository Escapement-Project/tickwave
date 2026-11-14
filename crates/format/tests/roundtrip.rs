use escapement_tickwave_format::decode::{BlockChannel, DecodeError, Footer};
use escapement_tickwave_format::encode::{EncodeError, Options, WriteError, encode};
use escapement_tickwave_format::header::AnalogCoding;
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Ratio, Recording, Sample, Samples, Source, SourceFormat,
};
use escapement_tickwave_format::verify::{VerifyError, verify};

const EDGES_A: [i64; 9] = [1, 2, 3, 100, 101, 200, 5000, 9999, 10000];
/// Ticks 0..=10,000: A's last edge and V's last sample sit on the final tick.
const LENGTH: i64 = 10_001;

/// Sample i of V sits at tick 10 i, so samples share ticks 100 and 200 with A's edges.
fn value(i: u64) -> f32 {
    match i {
        7 => f32::NAN,
        8 => -0.0,
        _ => i as f32 * 0.5 - 3.0,
    }
}

fn recording(edges_a: Vec<i64>, values: Vec<f32>) -> Recording {
    let count = values.len() as u64;
    Recording {
        timebase: Ratio::integer(1_000_000_000).unwrap(),
        length: LENGTH,
        channels: vec![
            Channel {
                name: "A".into(),
                source_name: "a".into(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: true,
                    end: LENGTH,
                    count: edges_a.len() as u64,
                    edges: Box::new(edges_a.into_iter()),
                    gaps: Vec::new(),
                }),
                shaping: Default::default(),
            },
            Channel {
                name: "B".into(),
                source_name: "b".into(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end: 9001,
                    edges: Box::new(std::iter::empty()),
                    count: 0,
                    gaps: Vec::new(),
                }),
                shaping: Default::default(),
            },
            Channel {
                name: "V".into(),
                source_name: "v".into(),
                source: 0,
                data: ChannelData::Analog(Analog {
                    significant: None,
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::integer(10).unwrap(),
                    count,
                    breaks: Vec::new(),
                    samples: Samples::F32(Box::new(values.into_iter())),
                }),
                shaping: Default::default(),
            },
        ],
        derived: Vec::new(),
        sources: vec![Source::new(
            SourceFormat::Other("test".into()),
            Ratio::integer(1_000_000_000).unwrap(),
        )],
    }
}

fn values() -> Vec<f32> {
    (0..1000).map(value).collect()
}

fn small_blocks() -> Options {
    Options {
        block_bytes: 64,
        ..Options::default()
    }
}

#[test]
fn round_trips_through_many_blocks() {
    let file = encode(recording(EDGES_A.to_vec(), values()), &small_blocks()).unwrap();
    verify(&file[..], recording(EDGES_A.to_vec(), values())).unwrap();
    let head = Footer::parse_file(&file).unwrap();
    assert!(head.rows.len() > 20, "{} blocks", head.rows.len());
    let mut edges: Vec<i64> = Vec::new();
    let mut levels = Vec::new();
    let mut samples = 0;
    for i in 0..head.rows.len() {
        let (offset, len) = head.block_location(i);
        let block = head
            .decode_block(i, &file[offset as usize..][..len as usize])
            .unwrap();
        for (c, channel) in block.channels.iter().enumerate() {
            match channel {
                BlockChannel::Digital {
                    initial, edges: e, ..
                } if c == 0 => {
                    levels.push((block.start, *initial));
                    for &t in e {
                        assert!(t >= block.start && t < block.end, "{t} in block {i}");
                    }
                    edges.extend(e);
                }
                BlockChannel::Analog {
                    first_index,
                    values: v,
                } => {
                    assert_eq!(*first_index, samples);
                    for (k, x) in v.iter().enumerate() {
                        let tick = 10 * (first_index + k as u64) as i64;
                        assert!(tick >= block.start && tick < block.end);
                        let Sample::F32(x) = x else {
                            panic!("V stores f32 values");
                        };
                        assert_eq!(x.to_bits(), value(first_index + k as u64).to_bits());
                    }
                    samples += v.len() as u64;
                }
                _ => {}
            }
        }
    }
    assert_eq!(edges, EDGES_A);
    assert_eq!(samples, 1000);
    // A starts high and flips at each edge: the level at a block's start is high exactly when
    // an even number of edges came before it.
    for (start, level) in levels {
        let before = EDGES_A.iter().filter(|&&t| t < start).count();
        assert_eq!(level, before % 2 == 0, "block at {start}");
    }
}

#[test]
fn same_tick_items_share_a_block() {
    let file = encode(recording(EDGES_A.to_vec(), values()), &small_blocks()).unwrap();
    let head = Footer::parse_file(&file).unwrap();
    // A's edge at tick 100 and V's sample 10 (tick 100) are in one block.
    let blocks = head.blocks_covering(100, 101);
    assert_eq!(blocks.len(), 1);
    let i = blocks.start;
    let (offset, len) = head.block_location(i);
    let block = head
        .decode_block(i, &file[offset as usize..][..len as usize])
        .unwrap();
    let BlockChannel::Digital { edges, .. } = &block.channels[0] else {
        panic!()
    };
    let BlockChannel::Analog {
        first_index,
        values,
    } = &block.channels[2]
    else {
        panic!()
    };
    assert!(edges.contains(&100));
    assert!((*first_index..*first_index + values.len() as u64).contains(&10));
}

#[test]
fn the_overview_matches_the_edges() {
    let file = encode(recording(EDGES_A.to_vec(), values()), &Options::default()).unwrap();
    let head = Footer::parse_file(&file).unwrap();
    // V, a sample every 10 ticks, is the densest: about 1,024 of them a bucket is
    // round(log2(10,240)) = 13. 10,001 ticks fit one level of 2^13-tick buckets: 0..8191 and
    // 8192..10000.
    assert_eq!(head.header.overview_shift, 13);
    assert_eq!(head.header.overview, [13]);
    let a = &head.overview[0].channels[0];
    // Bucket 0: starts high, and edges 1-3, 100, 101, 200 and 5000 change it. Bucket 1: low after
    // those seven flips; 9999 and 10000 change it.
    assert_eq!(a.digital(0), Some((true, true)));
    assert_eq!(a.digital(1), Some((false, true)));
}

#[test]
fn codes_round_trip_exactly() {
    let coding = AnalogCoding::Codes {
        bits: 12,
        scale: 0.5,
        offset: -3.0,
    };
    // Every value is -3 + 0.5 k: codes reproduce them.
    let exact: Vec<f32> = (0..1000).map(|i| (i % 4000) as f32 * 0.5 - 3.0).collect();
    let options = Options {
        analog: vec![None, None, Some(coding)],
        ..small_blocks()
    };
    let file = encode(recording(EDGES_A.to_vec(), exact.clone()), &options).unwrap();
    verify(&file[..], recording(EDGES_A.to_vec(), exact)).unwrap();
}

#[test]
fn refuses_a_value_codes_cannot_hold() {
    let coding = AnalogCoding::Codes {
        bits: 12,
        scale: 0.5,
        offset: -3.0,
    };
    let mut v: Vec<f32> = (0..1000).map(|i| i as f32 * 0.5 - 3.0).collect();
    v[500] += 0.25;
    let options = Options {
        analog: vec![None, None, Some(coding)],
        ..Options::default()
    };
    let err = encode(recording(EDGES_A.to_vec(), v), &options).unwrap_err();
    assert!(
        matches!(
            err,
            EncodeError::Write(WriteError::Coding { index: 500, .. })
        ),
        "{err}"
    );
}

#[test]
fn refuses_edges_out_of_order() {
    let err = encode(recording(vec![5, 3], values()), &Options::default()).unwrap_err();
    assert!(
        matches!(err, EncodeError::Write(WriteError::Order { tick: 3, .. })),
        "{err}"
    );
    let err = encode(recording(vec![5, 5, 4], values()), &Options::default()).unwrap_err();
    assert!(
        matches!(err, EncodeError::Write(WriteError::Order { tick: 4, .. })),
        "{err}"
    );
}

/// Pulses the source stamped with one time: pairs at the first tick, at a sample's tick, at a block's
/// start (300, where small blocks cut), on the last tick, and three edges at one tick.
const EDGES_SHARED: [i64; 17] = [
    1, 1, 2, 3, 3, 3, 100, 100, 101, 200, 300, 300, 5000, 5000, 9999, 10000, 10000,
];

#[test]
fn round_trips_edges_that_share_a_tick() {
    let file = encode(recording(EDGES_SHARED.to_vec(), values()), &small_blocks()).unwrap();
    verify(&file[..], recording(EDGES_SHARED.to_vec(), values())).unwrap();
    let head = Footer::parse_file(&file).unwrap();
    let mut edges: Vec<i64> = Vec::new();
    let mut pair_starts_a_block = false;
    for i in 0..head.rows.len() {
        let (offset, len) = head.block_location(i);
        let block = head
            .decode_block(i, &file[offset as usize..][..len as usize])
            .unwrap();
        let BlockChannel::Digital {
            initial, edges: e, ..
        } = &block.channels[0]
        else {
            panic!()
        };
        let before = EDGES_SHARED.iter().filter(|&&t| t < block.start).count();
        assert_eq!(*initial, before % 2 == 0, "block at {}", block.start);
        pair_starts_a_block |= e.len() >= 2 && e[0] == block.start && e[1] == block.start;
        edges.extend(e);
    }
    assert_eq!(edges, EDGES_SHARED);
    // The case only a block's first interval could hold before: zero, then zero again.
    assert!(pair_starts_a_block);
}

#[test]
fn refuses_an_edge_past_the_channel_end() {
    let err = encode(recording(vec![10_001], values()), &Options::default()).unwrap_err();
    assert!(
        matches!(err, EncodeError::Write(WriteError::Length { .. })),
        "{err}"
    );
    let mut rec = recording(EDGES_A.to_vec(), values());
    if let ChannelData::Digital(d) = &mut rec.channels[1].data {
        (d.edges, d.count) = (Box::new([9001].into_iter()), 1);
    }
    let err = encode(rec, &Options::default()).unwrap_err();
    assert!(
        matches!(
            err,
            EncodeError::Write(WriteError::EdgePastEnd { tick: 9001, .. })
        ),
        "{err}"
    );
}

#[test]
fn refuses_the_wrong_sample_count() {
    let mut v = values();
    v.pop();
    let mut rec = recording(EDGES_A.to_vec(), v);
    if let ChannelData::Analog(a) = &mut rec.channels[2].data {
        a.count = 1000;
    }
    let err = encode(rec, &Options::default()).unwrap_err();
    assert!(
        matches!(err, EncodeError::Count { delivered: 999, .. }),
        "{err}"
    );
}

#[test]
fn verify_catches_a_missing_edge_and_a_changed_sample() {
    let file = encode(recording(EDGES_A.to_vec(), values()), &small_blocks()).unwrap();
    let mut fewer = EDGES_A.to_vec();
    fewer.remove(4);
    let err = verify(&file[..], recording(fewer, values())).unwrap_err();
    assert!(matches!(err, VerifyError::Mismatch(_)), "{err}");
    let mut changed = values();
    changed[8] = 0.0; // was -0.0: equal as floats, different bits
    let err = verify(&file[..], recording(EDGES_A.to_vec(), changed)).unwrap_err();
    assert!(matches!(err, VerifyError::Mismatch(_)), "{err}");
}

#[test]
fn a_flipped_byte_in_a_block_fails_to_decode() {
    let mut file = encode(recording(EDGES_A.to_vec(), values()), &small_blocks()).unwrap();
    let head = Footer::parse_file(&file).unwrap();
    let (offset, len) = head.block_location(3);
    file[(offset + u64::from(len) / 2) as usize] ^= 0x10;
    let err = verify(&file[..], recording(EDGES_A.to_vec(), values())).unwrap_err();
    assert!(matches!(err, VerifyError::Decode(_)), "{err}");
}

#[test]
fn a_short_tail_asks_for_the_footer() {
    let file = encode(recording(EDGES_A.to_vec(), values()), &Options::default()).unwrap();
    let len = file.len() as u64;
    assert!(matches!(
        Footer::parse(&file[file.len() - 10..], len),
        Err(DecodeError::NeedFrom(n)) if n == len - 24
    ));
    let footer_at = Footer::parse_file(&file).unwrap().trailer.footer_offset;
    assert!(matches!(
        Footer::parse(&file[footer_at as usize + 1..], len),
        Err(DecodeError::NeedFrom(n)) if n == footer_at
    ));
    let footer = Footer::parse(&file[footer_at as usize..], len).unwrap();
    assert_eq!(footer.rows, Footer::parse_file(&file).unwrap().rows);
    let not = b"not a recording at all, not even close to one";
    assert!(matches!(
        Footer::parse(not, not.len() as u64),
        Err(DecodeError::Unfinished)
    ));
    assert!(matches!(
        Footer::parse_file(not),
        Err(DecodeError::NotTickwave)
    ));
    assert!(matches!(
        Footer::parse_file(&file[..file.len() - 1]),
        Err(DecodeError::Unfinished)
    ));
}

#[test]
fn the_content_id_ignores_how_the_file_is_cut() {
    let small = encode(recording(EDGES_A.to_vec(), values()), &small_blocks()).unwrap();
    let large = encode(recording(EDGES_A.to_vec(), values()), &Options::default()).unwrap();
    let small = Footer::parse_file(&small).unwrap();
    let large = Footer::parse_file(&large).unwrap();
    assert_ne!(small.rows.len(), large.rows.len());
    assert_eq!(small.header.content_id, large.header.content_id);
    let other = encode(recording(vec![1, 2], values()), &Options::default()).unwrap();
    assert_ne!(
        Footer::parse_file(&other).unwrap().header.content_id,
        large.header.content_id
    );
}

#[test]
fn a_bundle_keeps_the_content_and_carries_the_files() {
    use escapement_tickwave_format::bundle::bundle;
    let file = encode(recording(EDGES_A.to_vec(), values()), &small_blocks()).unwrap();
    let files: std::collections::BTreeMap<String, String> = [
        (
            "rec.annotations.toml".to_string(),
            "format = \"tickwave-annotations/1\"\n".to_string(),
        ),
        ("rec.gen.jsonl".to_string(), "{}\n".to_string()),
    ]
    .into();
    let bundled = bundle(&file, &Footer::parse_file(&file).unwrap(), files.clone()).unwrap();
    assert!(bundled.len() > file.len());
    verify(&bundled[..], recording(EDGES_A.to_vec(), values())).unwrap();
    let head = Footer::parse_file(&bundled).unwrap();
    assert_eq!(head.header.annotations.as_ref(), Some(&files));
    assert_eq!(
        head.header.content_id,
        Footer::parse_file(&file).unwrap().header.content_id
    );
    // Bundling again replaces the files rather than adding to them.
    let again = bundle(
        &bundled,
        &Footer::parse_file(&bundled).unwrap(),
        [("x.toml".to_string(), String::new())].into(),
    )
    .unwrap();
    let head = Footer::parse_file(&again).unwrap();
    assert_eq!(head.header.annotations.unwrap().len(), 1);
    verify(&again[..], recording(EDGES_A.to_vec(), values())).unwrap();
}

#[test]
fn codes_are_stored_in_byte_planes() {
    use escapement_tickwave_format::decode::inflate;
    let coding = AnalogCoding::Codes {
        bits: 16,
        scale: 1.0,
        offset: 0.0,
    };
    let rec = Recording {
        timebase: Ratio::integer(1_000_000_000).unwrap(),
        length: 3,
        channels: vec![Channel {
            name: "V".into(),
            source_name: "v".into(),
            source: 0,
            data: ChannelData::Analog(Analog {
                significant: None,
                unit: "V".into(),
                first: 0,
                period: Ratio::integer(1).unwrap(),
                count: 3,
                breaks: Vec::new(),
                samples: Samples::F32(Box::new(
                    [0x0102 as f32, 0x0304 as f32, 0x0506 as f32].into_iter(),
                )),
            }),
            shaping: Default::default(),
        }],
        derived: Vec::new(),
        sources: vec![Source::new(
            SourceFormat::Other("test".into()),
            Ratio::integer(1_000_000_000).unwrap(),
        )],
    };
    let options = Options {
        analog: vec![Some(coding)],
        ..Options::default()
    };
    let file = encode(rec, &options).unwrap();
    let head = Footer::parse_file(&file).unwrap();
    let (offset, len) = head.block_location(0);
    let raw = inflate(
        &file[offset as usize..][..len as usize],
        head.rows[0].decoded_length as usize,
    )
    .unwrap();
    // A u32 section length of 6, then the low bytes 02 04 06, then the high bytes 01 03 05.
    assert_eq!(raw, [6, 0, 0, 0, 0x02, 0x04, 0x06, 0x01, 0x03, 0x05]);
}

#[test]
fn a_newer_minor_version_is_not_bundled() {
    use escapement_tickwave_format::bundle::bundle;
    use escapement_tickwave_format::header::MINOR;
    let mut file = encode(recording(EDGES_A.to_vec(), values()), &small_blocks()).unwrap();
    // The trailer's minor version, after the footer's offset and length and the major version.
    let at = file.len() - 24 + 14;
    file[at..at + 2].copy_from_slice(&(MINOR + 1).to_le_bytes());
    let head = Footer::parse_file(&file).unwrap();
    let err = bundle(&file, &head, Default::default()).expect_err("refused");
    assert!(
        err.to_string()
            .contains(&format!("newer than this tool's 1.{MINOR}")),
        "{err}"
    );
}

#[test]
fn the_rates_origin_and_the_trigger_survive_encoding_and_bundling() {
    use escapement_tickwave_format::bundle::bundle;
    use escapement_tickwave_format::model::RateOrigin;
    use std::collections::BTreeMap;
    let mut rec = recording(EDGES_A.to_vec(), values());
    rec.sources[0].rate = Some(RateOrigin::Inferred);
    rec.sources[0].trigger_s = Some(-2.5e-6);
    let file = encode(rec, &Options::default()).unwrap();
    let head = Footer::parse_file(&file).unwrap();
    assert_eq!(head.trailer.minor, 0);
    let json = serde_json::to_string(&head.header).unwrap();
    assert!(
        json.contains(r#""trigger_s":-2.5e-6,"rate":"inferred""#),
        "{json}"
    );
    let bundled = bundle(&file, &head, BTreeMap::new()).unwrap();
    let again = Footer::parse_file(&bundled).unwrap();
    assert_eq!(again.header.sources[0].rate, Some(RateOrigin::Inferred));
    assert_eq!(again.header.sources[0].trigger_s, Some(-2.5e-6));
}
