use escapement_tickwave_format::bundle::bundle;
use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::OverviewLevel;
use escapement_tickwave_format::overview::AnalogOverview;
use escapement_tickwave_format::record::{self, Kind};
use escapement_tickwave_format::verify::verify;

mod common;
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Ratio, Recording, Samples, Source, SourceFormat,
};

/// At 2^12 ticks a bucket this has 2^18 + 1 buckets: too many for the footer, so five overview
/// blocks of 2^16, the last holding only the bucket that starts at the final tick.
const LAST: i64 = 1 << 30;
const LENGTH: i64 = LAST + 1;
const EDGES: [i64; 4] = [5000, 5001, 300_000_000, LAST];
/// One sample every 2^20 ticks: in every 256th bucket at 2^12.
const PERIOD: u64 = 1 << 20;
const SAMPLES: u64 = (LAST as u64 / PERIOD) + 1;

fn recording(edges: Vec<i64>) -> Recording {
    Recording {
        timebase: Ratio::integer(1_000_000_000).unwrap(),
        length: LENGTH,
        channels: vec![
            Channel {
                name: "C".into(),
                source_name: "c".into(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end: LENGTH,
                    edges: Box::new(edges.into_iter()),
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
                    period: Ratio::integer(PERIOD).unwrap(),
                    count: SAMPLES,
                    breaks: Vec::new(),
                    samples: Samples::F32(Box::new((0..SAMPLES).map(|i| i as f32))),
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

fn file() -> Vec<u8> {
    encode(recording(EDGES.to_vec()), &Options::default()).unwrap()
}

fn bit(bytes: &[u8], i: u64) -> bool {
    bytes[(i / 8) as usize] & (1 << (i % 8)) != 0
}

#[test]
fn a_long_recording_keeps_its_finest_level_in_blocks() {
    let file = file();
    verify(&file[..], recording(EDGES.to_vec())).unwrap();
    let head = Footer::parse_file(&file).unwrap();
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
    let Some(AnalogOverview::F32(v)) = block.level.channels[1].analog() else {
        panic!("V is f32");
    };
    let (min, max, sum) = (&v.min, &v.max, &v.sum);
    assert_eq!((min[0], max[0], sum[0]), (0.0, 0.0, 0.0));
    assert_eq!(
        (min[1], max[1], sum[1]),
        (f32::INFINITY, f32::NEG_INFINITY, 0.0)
    );
    assert_eq!((min[256], max[256], sum[256]), (1.0, 1.0, 1.0));
    let last = decode(4);
    let Some(AnalogOverview::F32(v)) = last.level.channels[1].analog() else {
        panic!("V is f32");
    };
    assert_eq!(v.min[0], (SAMPLES - 1) as f32);

    assert_eq!(head.overview_blocks_covering(0, 0, 1), 0..1);
    assert_eq!(
        head.overview_blocks_covering(0, 4096 * 65536 - 1, 4096 * 65536 + 1),
        0..2
    );
    assert_eq!(head.overview_blocks_covering(0, LAST, LENGTH), 4..5);
    assert_eq!(head.overview_blocks_covering(0, i64::MIN, i64::MAX), 0..5);
    assert_eq!(head.overview_blocks_covering(0, 10, 10), 0..1);
}

#[test]
fn a_bundle_leaves_the_overview_blocks_where_they_are() {
    let file = file();
    let bundled = bundle(
        &file,
        &Footer::parse_file(&file).unwrap(),
        [("rec.annotations.toml".into(), "x".repeat(1000))].into(),
    )
    .unwrap();
    verify(&bundled[..], recording(EDGES.to_vec())).unwrap();
    let (a, b) = (
        Footer::parse_file(&file).unwrap(),
        Footer::parse_file(&bundled).unwrap(),
    );
    assert!(bundled.len() >= file.len() + 1000);
    assert_eq!(a.overview_rows, b.overview_rows);
    assert_eq!(a.rows, b.rows);
    assert_eq!(
        bundled[..a.trailer.footer_offset as usize],
        file[..a.trailer.footer_offset as usize]
    );
}

/// `base` with overview block `k` of its finest level taken from `donor`, whose levels must be the
/// same. The records after it move, and the footer is written again for where they now sit.
fn splice_overview_block(base: &[u8], donor: &[u8], k: usize) -> Vec<u8> {
    let head = Footer::parse_file(base).unwrap();
    let donor_head = Footer::parse_file(donor).unwrap();
    let (doffset, dlen) = donor_head.overview_block_location(0, k);
    let frame = &donor[doffset as usize..][..dlen as usize];
    let (offset, len) = head.overview_block_location(0, k);
    let start = offset - (record::PAYLOAD_AT + record::OVERVIEW_HEAD_LEN) as u64;
    let end = offset + u64::from(len) + 8;
    let shift = head.header.overview_blocks[0].shift;
    let mut spliced = Vec::new();
    record::write(
        &mut spliced,
        Kind::OverviewBlock,
        &[&record::overview_head(shift, k as u64), frame],
    );
    let delta = spliced.len() as i64 - (end - start) as i64;
    let moved = |o: u64| {
        if o > start {
            o.checked_add_signed(delta).unwrap()
        } else {
            o
        }
    };
    let mut out = base[..start as usize].to_vec();
    out.extend_from_slice(&spliced);
    out.extend_from_slice(&base[end as usize..head.trailer.footer_offset as usize]);
    let rows: Vec<_> = head
        .rows
        .iter()
        .map(|r| escapement_tickwave_format::header::BlockRow {
            offset: moved(r.offset),
            ..*r
        })
        .collect();
    let mut overview_rows = Vec::new();
    for (l, level) in head.overview_rows.iter().enumerate() {
        for (j, row) in level.iter().enumerate() {
            let mut row = *row;
            if l == 0 && j == k {
                row.length = dlen;
            } else {
                row.offset = moved(row.offset);
            }
            overview_rows.push(row);
        }
    }
    let footer = record::footer_bytes(
        &head.header,
        &rows,
        &head.words,
        &overview_rows,
        &head.overview_frame,
    )
    .unwrap();
    let at = out.len() as u64;
    let mut tail = Vec::new();
    record::write_footer(&mut tail, at, &footer);
    out.extend_from_slice(&tail);
    out
}

#[test]
fn verify_refuses_overview_blocks_that_differ_from_the_content() {
    let file = file();
    // An unchanged block spliced back in: the splice itself must not break anything.
    let same = splice_overview_block(&file, &file, 2);
    assert_eq!(same, file);
    // Two more edges inside one bucket of block 2: only the finest level's block 2 differs, and
    // the footer's coarser levels and the data blocks stay this file's own.
    let mut edges = EDGES.to_vec();
    edges.splice(3..3, [600_000_000, 600_000_001]);
    let donor = encode(recording(edges), &Options::default()).unwrap();
    let mixed = splice_overview_block(&file, &donor, 2);
    let head = Footer::parse_file(&mixed).unwrap();
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
    let err = verify(&mixed[..], recording(EDGES.to_vec())).unwrap_err();
    assert!(err.to_string().contains("2^12, block 2"), "{err}");
}

#[test]
fn a_header_listing_other_overview_levels_is_refused() {
    let file = file();
    // Same byte length, so every offset stays right: only the levels change. Too fine for the
    // footer, past what a shift can be, a level twice (each copy adds its size to what the reader
    // allocates), and out of order.
    for listed in [
        r#""overview":[ 0,20,24]"#,
        r#""overview":[99,20,24]"#,
        r#""overview":[16,16,24]"#,
        r#""overview":[20,16,24]"#,
    ] {
        let bad = common::patched(&file, r#""overview":[16,20,24]"#, listed);
        let err = Footer::parse_file(&bad).expect_err("refused");
        assert!(
            err.to_string().contains("not the ones the format sets"),
            "{err}"
        );
    }
}
