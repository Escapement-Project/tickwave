use escapement_tickwave_format::bundle::bundle;
use escapement_tickwave_format::decode::{Head, verify};
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::{
    BlockRow, OverviewLevel, OverviewRow, PREAMBLE_LEN, Preamble,
};
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Ratio, Recording, Samples, Source,
};

/// At 2^12 ticks a bucket this has 2^18 + 1 buckets: too many for the head, so five overview
/// blocks of 2^16, the last holding only the bucket that starts at the final tick.
const LENGTH: i64 = 1 << 30;
const EDGES: [i64; 4] = [5000, 5001, 300_000_000, LENGTH];
/// One sample every 2^20 ticks: in every 256th bucket at 2^12.
const PERIOD: u64 = 1 << 20;
const SAMPLES: u64 = (LENGTH as u64 / PERIOD) + 1;

fn recording(edges: Vec<i64>) -> Recording {
    Recording {
        timebase: Ratio::integer(1_000_000_000),
        length: LENGTH,
        channels: vec![
            Channel {
                name: "C".into(),
                source_name: "c".into(),
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end: LENGTH,
                    edges: Box::new(edges.into_iter()),
                }),
            },
            Channel {
                name: "V".into(),
                source_name: "v".into(),
                data: ChannelData::Analog(Analog {
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::integer(PERIOD),
                    count: SAMPLES,
                    samples: Samples::F32(Box::new((0..SAMPLES).map(|i| i as f32))),
                }),
            },
        ],
        source: Source {
            format: "test".into(),
            ..Source::default()
        },
    }
}

fn file() -> Vec<u8> {
    encode(recording(EDGES.to_vec()), &Options::default()).unwrap()
}

fn bit(bytes: &[u8], i: u64) -> bool {
    bytes[(i / 8) as usize] & (1 << (i % 8)) != 0
}

#[test]
fn a_long_recording_keeps_its_finest_level_in_blocks() {
    let file = file();
    verify(&file, recording(EDGES.to_vec())).unwrap();
    let head = Head::parse(&file).unwrap();
    assert_eq!(head.header.overview, [16, 20, 24]);
    assert_eq!(
        head.header.overview_blocks,
        [OverviewLevel {
            shift: 12,
            blocks: 5
        }]
    );
    let decode = |k: usize| {
        let (offset, len) = head.overview_block_location(0, k);
        head.decode_overview_block(0, k, &file[offset as usize..][..len as usize])
            .unwrap()
    };
    let firsts: Vec<_> = (0..5)
        .map(|k| (decode(k).first, decode(k).level.buckets))
        .collect();
    assert_eq!(
        firsts,
        [
            (0, 65536),
            (65536, 65536),
            (131072, 65536),
            (196608, 65536),
            (262144, 1)
        ]
    );

    // Expected bits from the edges themselves, not from the builder that wrote them.
    let digital = |k: usize, i: u64| decode(k).level.channels[0].digital(i).unwrap();
    // 5000 and 5001 fall in bucket 1, which starts low; bucket 0 is low and quiet.
    assert_eq!(digital(0, 0), (false, false));
    assert_eq!(digital(0, 1), (false, true));
    assert_eq!(digital(0, 2), (false, false));
    // 300,000,000 is in bucket 73,242 (block 1, bucket 7,706), which starts low.
    assert_eq!(digital(1, 73_242 - 65_536), (false, true));
    assert_eq!(digital(1, 73_243 - 65_536), (true, false));
    // The last edge is at the final tick, the first tick of the last bucket: the bucket starts
    // at the level before it, high, and changes.
    assert_eq!(digital(4, 0), (true, true));

    let block = decode(0);
    let escapement_tickwave_format::overview::ChannelOverview::Analog { min, max } =
        &block.level.channels[1]
    else {
        panic!("V is analog");
    };
    assert_eq!((min[0], max[0]), (0.0, 0.0));
    assert_eq!((min[1], max[1]), (f32::INFINITY, f32::NEG_INFINITY));
    assert_eq!((min[256], max[256]), (1.0, 1.0));
    let last = decode(4);
    let escapement_tickwave_format::overview::ChannelOverview::Analog { min, .. } =
        &last.level.channels[1]
    else {
        panic!("V is analog");
    };
    assert_eq!(min[0], (SAMPLES - 1) as f32);

    assert_eq!(head.overview_blocks_covering(0, 0, 1), 0..1);
    assert_eq!(
        head.overview_blocks_covering(0, 4096 * 65536 - 1, 4096 * 65536 + 1),
        0..2
    );
    assert_eq!(head.overview_blocks_covering(0, LENGTH, LENGTH + 1), 4..5);
    assert_eq!(head.overview_blocks_covering(0, i64::MIN, i64::MAX), 0..5);
    assert_eq!(head.overview_blocks_covering(0, 10, 10), 0..1);
}

#[test]
fn a_bundle_moves_the_overview_blocks_with_the_rest() {
    let file = file();
    let bundled = bundle(
        &file,
        &Head::parse_file(&file).unwrap(),
        [("rec.annotations.toml".into(), "x".repeat(1000))].into(),
    )
    .unwrap();
    verify(&bundled, recording(EDGES.to_vec())).unwrap();
    let (a, b) = (Head::parse(&file).unwrap(), Head::parse(&bundled).unwrap());
    let shift = b.preamble.head_len - a.preamble.head_len;
    assert!(shift >= 1000);
    for k in 0..5 {
        let (offset, len) = a.overview_block_location(0, k);
        assert_eq!(
            b.overview_block_location(0, k),
            (offset + u64::from(shift), len)
        );
    }
}

/// `base` with `donor`'s overview blocks in place of its own. Both must have the same levels.
fn splice_overview_blocks(base: &[u8], donor: &[u8]) -> Vec<u8> {
    let head = Head::parse(base).unwrap();
    let donor_head = Head::parse(donor).unwrap();
    let rows_at = PREAMBLE_LEN
        + head.preamble.header_len as usize
        + head.rows.len() * BlockRow::size(head.header.channels.len());
    let mut offset = head.overview_rows[0][0].offset;
    let mut out = base[..offset as usize].to_vec();
    for (i, row) in donor_head.overview_rows.iter().flatten().enumerate() {
        let mut bytes = Vec::new();
        OverviewRow { offset, ..*row }.write(&mut bytes);
        out[rows_at + i * OverviewRow::SIZE..][..OverviewRow::SIZE].copy_from_slice(&bytes);
        out.extend_from_slice(&donor[row.offset as usize..][..row.length as usize]);
        offset += u64::from(row.length);
    }
    let preamble = Preamble {
        file_len: out.len() as u64,
        ..head.preamble
    };
    out[..PREAMBLE_LEN].copy_from_slice(&preamble.to_bytes());
    out
}

#[test]
fn verify_refuses_overview_blocks_that_differ_from_the_content() {
    let file = file();
    // Unchanged blocks spliced back in: the splice itself must not break anything.
    let same = splice_overview_blocks(&file, &file);
    assert_eq!(same, file);
    // Two more edges inside one bucket of block 2: only the finest level's block 2 differs, and
    // the head's coarser levels and the data blocks stay this file's own.
    let mut edges = EDGES.to_vec();
    edges.splice(3..3, [600_000_000, 600_000_001]);
    let donor = encode(recording(edges), &Options::default()).unwrap();
    let mixed = splice_overview_blocks(&file, &donor);
    let head = Head::parse(&mixed).unwrap();
    let (offset, len) = head.overview_block_location(0, 2);
    let block = head
        .decode_overview_block(0, 2, &mixed[offset as usize..][..len as usize])
        .unwrap();
    assert!(bit(
        match &block.level.channels[0] {
            escapement_tickwave_format::overview::ChannelOverview::Digital { change, .. } => change,
            _ => unreachable!(),
        },
        (600_000_000 >> 12) - block.first
    ));
    let err = verify(&mixed, recording(EDGES.to_vec())).unwrap_err();
    assert!(err.to_string().contains("2^12, block 2"), "{err}");
}

#[test]
fn a_header_listing_other_overview_levels_is_refused() {
    let file = file();
    let needle = br#""overview":[16,20,24]"#;
    let at = file
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap();
    // Same byte length, so every offset stays right: only the levels change.
    // Too fine for the head, past what a shift can be, a level twice (each copy adds its size to
    // what the reader allocates), and out of order.
    for listed in [
        &br#""overview":[ 0,20,24]"#[..],
        br#""overview":[99,20,24]"#,
        br#""overview":[16,16,24]"#,
        br#""overview":[20,16,24]"#,
    ] {
        let mut bad = file.clone();
        bad[at..at + needle.len()].copy_from_slice(listed);
        let err = Head::parse(&bad).err().expect("refused");
        assert!(
            err.to_string().contains("not the ones the format sets"),
            "{err}"
        );
    }
}
