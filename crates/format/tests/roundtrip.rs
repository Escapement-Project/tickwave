use chrono_format::decode::{BlockChannel, DecodeError, Head, VerifyError, verify};
use chrono_format::encode::{EncodeError, Options, encode};
use chrono_format::header::{AnalogCoding, PREAMBLE_LEN};
use chrono_format::model::{
    Analog, Channel, ChannelData, Digital, Ratio, Recording, Samples, Source,
};

const EDGES_A: [i64; 9] = [1, 2, 3, 100, 101, 200, 5000, 9999, 10000];
const LENGTH: i64 = 10_000;

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
        timebase: Ratio::integer(1_000_000_000),
        length: LENGTH,
        channels: vec![
            Channel {
                name: "A".into(),
                source_name: "a".into(),
                data: ChannelData::Digital(Digital {
                    initial: true,
                    end: LENGTH,
                    edges: Box::new(edges_a.into_iter()),
                }),
            },
            Channel {
                name: "B".into(),
                source_name: "b".into(),
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end: 9000,
                    edges: Box::new(std::iter::empty()),
                }),
            },
            Channel {
                name: "V".into(),
                source_name: "v".into(),
                data: ChannelData::Analog(Analog {
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::integer(10),
                    count,
                    samples: Samples::F32(Box::new(values.into_iter())),
                }),
            },
        ],
        source: Source {
            format: "test".into(),
            ..Source::default()
        },
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
    verify(&file, recording(EDGES_A.to_vec(), values())).unwrap();
    let head = Head::parse(&file).unwrap();
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
                BlockChannel::Digital { initial, edges: e } if c == 0 => {
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
    let head = Head::parse(&file).unwrap();
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
    let head = Head::parse(&file).unwrap();
    // 10,000 ticks fit one level of 2^12-tick buckets: 0..4095, 4096..8191, 8192..10000.
    assert_eq!(head.header.overview, [12]);
    let a = &head.overview[0].channels[0];
    // Bucket 0: starts high, changes. Bucket 1: edges 1-3, 100, 101 and 200 are six flips
    // before it, so it starts high; 5000 changes it. Bucket 2: low after 5000; 9999 and 10000
    // change it.
    assert_eq!(a.digital(0), Some((true, true)));
    assert_eq!(a.digital(1), Some((true, true)));
    assert_eq!(a.digital(2), Some((false, true)));
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
    verify(&file, recording(EDGES_A.to_vec(), exact)).unwrap();
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
        matches!(err, EncodeError::Coding { index: 500, .. }),
        "{err}"
    );
}

#[test]
fn refuses_edges_out_of_order() {
    let err = encode(recording(vec![5, 3], values()), &Options::default()).unwrap_err();
    assert!(matches!(err, EncodeError::Order { tick: 3, .. }), "{err}");
    let err = encode(recording(vec![5, 5], values()), &Options::default()).unwrap_err();
    assert!(matches!(err, EncodeError::Order { tick: 5, .. }), "{err}");
}

#[test]
fn refuses_an_edge_past_the_channel_end() {
    let err = encode(recording(vec![10_001], values()), &Options::default()).unwrap_err();
    assert!(matches!(err, EncodeError::EdgeRange { .. }), "{err}");
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
    let err = verify(&file, recording(fewer, values())).unwrap_err();
    assert!(matches!(err, VerifyError::Mismatch(_)), "{err}");
    let mut changed = values();
    changed[8] = 0.0; // was -0.0: equal as floats, different bits
    let err = verify(&file, recording(EDGES_A.to_vec(), changed)).unwrap_err();
    assert!(matches!(err, VerifyError::Mismatch(_)), "{err}");
}

#[test]
fn a_flipped_byte_in_a_block_fails_to_decode() {
    let mut file = encode(recording(EDGES_A.to_vec(), values()), &small_blocks()).unwrap();
    let head = Head::parse(&file).unwrap();
    let (offset, len) = head.block_location(3);
    file[(offset + u64::from(len) / 2) as usize] ^= 0x10;
    let err = verify(&file, recording(EDGES_A.to_vec(), values())).unwrap_err();
    assert!(matches!(err, VerifyError::Decode(_)), "{err}");
}

#[test]
fn a_short_head_asks_for_more() {
    let file = encode(recording(EDGES_A.to_vec(), values()), &Options::default()).unwrap();
    assert!(matches!(
        Head::parse(&file[..10]),
        Err(DecodeError::NeedMore(PREAMBLE_LEN))
    ));
    let head_len = Head::parse(&file).unwrap().preamble.head_len as usize;
    assert!(matches!(
        Head::parse(&file[..head_len - 1]),
        Err(DecodeError::NeedMore(n)) if n == head_len
    ));
    assert!(matches!(
        Head::parse(b"not a recording at all, no"),
        Err(DecodeError::NotChrono)
    ));
}

#[test]
fn the_content_id_ignores_how_the_file_is_cut() {
    let small = encode(recording(EDGES_A.to_vec(), values()), &small_blocks()).unwrap();
    let large = encode(recording(EDGES_A.to_vec(), values()), &Options::default()).unwrap();
    let small = Head::parse(&small).unwrap();
    let large = Head::parse(&large).unwrap();
    assert_ne!(small.rows.len(), large.rows.len());
    assert_eq!(small.header.content_id, large.header.content_id);
    let other = encode(recording(vec![1, 2], values()), &Options::default()).unwrap();
    assert_ne!(
        Head::parse(&other).unwrap().header.content_id,
        large.header.content_id
    );
}

#[test]
fn a_bundle_keeps_the_content_and_carries_the_files() {
    use chrono_format::bundle::bundle;
    let file = encode(recording(EDGES_A.to_vec(), values()), &small_blocks()).unwrap();
    let files: std::collections::BTreeMap<String, String> = [
        (
            "rec.annotations.toml".to_string(),
            "format = \"chrono-annotations/1\"\n".to_string(),
        ),
        ("rec.gen.jsonl".to_string(), "{}\n".to_string()),
    ]
    .into();
    let bundled = bundle(&file, files.clone()).unwrap();
    assert!(bundled.len() > file.len());
    verify(&bundled, recording(EDGES_A.to_vec(), values())).unwrap();
    let head = Head::parse(&bundled).unwrap();
    assert_eq!(head.header.annotations.as_ref(), Some(&files));
    assert_eq!(
        head.header.content_id,
        Head::parse(&file).unwrap().header.content_id
    );
    // Bundling again replaces the files rather than adding to them.
    let again = bundle(&bundled, [("x.toml".to_string(), String::new())].into()).unwrap();
    let head = Head::parse(&again).unwrap();
    assert_eq!(head.header.annotations.unwrap().len(), 1);
    verify(&again, recording(EDGES_A.to_vec(), values())).unwrap();
}

#[test]
fn codes_are_stored_in_byte_planes() {
    use chrono_format::decode::inflate;
    let coding = AnalogCoding::Codes {
        bits: 16,
        scale: 1.0,
        offset: 0.0,
    };
    let rec = Recording {
        timebase: Ratio::integer(1_000_000_000),
        length: 3,
        channels: vec![Channel {
            name: "V".into(),
            source_name: "v".into(),
            data: ChannelData::Analog(Analog {
                unit: "V".into(),
                first: 0,
                period: Ratio::integer(1),
                count: 3,
                samples: Samples::F32(Box::new(
                    [0x0102 as f32, 0x0304 as f32, 0x0506 as f32].into_iter(),
                )),
            }),
        }],
        source: Source::default(),
    };
    let options = Options {
        analog: vec![Some(coding)],
        ..Options::default()
    };
    let file = encode(rec, &options).unwrap();
    let head = Head::parse(&file).unwrap();
    let (offset, len) = head.block_location(0);
    let raw = inflate(
        &file[offset as usize..][..len as usize],
        head.rows[0].decoded_length as usize,
    )
    .unwrap();
    // A u32 section length of 6, then the low bytes 02 04 06, then the high bytes 01 03 05.
    assert_eq!(raw, [6, 0, 0, 0, 0x02, 0x04, 0x06, 0x01, 0x03, 0x05]);
}
