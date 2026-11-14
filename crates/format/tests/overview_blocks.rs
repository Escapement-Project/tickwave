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

/// At 2^12 ticks a bucket this has 2^18 + 1 buckets: too many for the footer, so 65 overview
/// blocks of 4,096, the last holding only the bucket that starts at the final tick. The finest
/// shift is given: the density rule would pick a coarser one for these few items.
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
                    count: edges.len() as u64,
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

fn options() -> Options {
    Options {
        overview_shift: Some(12),
        ..Options::default()
    }
}

fn file() -> Vec<u8> {
    encode(recording(EDGES.to_vec()), &options()).unwrap()
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
            blocks: 65
        }]
    );
    let decode = |k: usize| {
        let (offset, len) = head.overview_block_location(0, k);
        head.decode_overview_block(0, k, &file[offset as usize..][..len as usize])
            .unwrap()
    };
    let firsts: Vec<_> = (0..65)
        .map(|k| (decode(k).first, decode(k).level.buckets))
        .collect();
    let mut want: Vec<_> = (0..64).map(|k| (k * 4096, 4096)).collect();
    want.push((262144, 1));
    assert_eq!(firsts, want);

    // Expected bits from the edges themselves, not from the builder that wrote them.
    let digital = |k: usize, i: u64| decode(k).level.channels[0].digital(i).unwrap();
    // 5000 and 5001 fall in bucket 1, which starts low; bucket 0 is low and quiet.
    assert_eq!(digital(0, 0), (false, false));
    assert_eq!(digital(0, 1), (false, true));
    assert_eq!(digital(0, 2), (false, false));
    // 300,000,000 is in bucket 73,242 (block 17, bucket 3,610), which starts low.
    assert_eq!(digital(17, 73_242 - 17 * 4096), (false, true));
    assert_eq!(digital(17, 73_243 - 17 * 4096), (true, false));
    // The last edge is at the final tick, the first tick of the last bucket: the bucket starts
    // at the level before it, high, and changes.
    assert_eq!(digital(64, 0), (true, true));

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
    let last = decode(64);
    let Some(AnalogOverview::F32(v)) = last.level.channels[1].analog() else {
        panic!("V is f32");
    };
    assert_eq!(v.min[0], (SAMPLES - 1) as f32);

    assert_eq!(head.overview_blocks_covering(0, 0, 1), 0..1);
    // A block covers 4,096 buckets of 4,096 ticks: 2^24 ticks.
    assert_eq!(
        head.overview_blocks_covering(0, (1 << 24) - 1, (1 << 24) + 1),
        0..2
    );
    assert_eq!(head.overview_blocks_covering(0, LAST, LENGTH), 64..65);
    assert_eq!(head.overview_blocks_covering(0, i64::MIN, i64::MAX), 0..65);
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
    let same = splice_overview_block(&file, &file, 35);
    assert_eq!(same, file);
    // Two more edges inside one bucket of block 35, 600,000,000 >> 12 = 146,484: only the finest
    // level's block 35 differs, and the footer's coarser levels and the data blocks stay this
    // file's own.
    let mut edges = EDGES.to_vec();
    edges.splice(3..3, [600_000_000, 600_000_001]);
    let donor = encode(recording(edges), &options()).unwrap();
    let mixed = splice_overview_block(&file, &donor, 35);
    let head = Footer::parse_file(&mixed).unwrap();
    let (offset, len) = head.overview_block_location(0, 35);
    let block = head
        .decode_overview_block(0, 35, &mixed[offset as usize..][..len as usize])
        .unwrap();
    assert!(bit(
        match &block.level.channels[0] {
            escapement_tickwave_format::overview::ChannelOverview::Digital { change, .. } => change,
            _ => unreachable!(),
        },
        (600_000_000 >> 12) - block.first
    ));
    let err = verify(&mixed[..], recording(EDGES.to_vec())).unwrap_err();
    assert!(err.to_string().contains("2^12, block 35"), "{err}");
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

/// One digital channel at 1 tick a second, buckets of one tick in its finest level: a span this
/// fine needs an overview block every 4,096 ticks.
fn shift_zero() -> escapement_tickwave_format::header::Declaration {
    use escapement_tickwave_format::header::{
        Declaration, DeclaredChannel, DeclaredKind, SourceHeader,
    };
    Declaration {
        timebase: Ratio::ONE,
        channels: vec![DeclaredChannel {
            name: "D".into(),
            source_name: "d".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("t".into()),
            Ratio::ONE,
        )],
        title: None,
        meta: Default::default(),
        overview_shift: 0,
    }
}

/// At shift 0 a recording to tick 4,026,585,089 needs 1,048,577 overview blocks, one more than
/// tickwave keeps; to the tick before, as many. Each block costs whatever settles the overview, so
/// the span a caller or a file states is refused there, before any of that work, rather than after
/// it.
const PAST: i64 = 4_026_585_089;

#[test]
fn a_span_needing_more_overview_blocks_than_tickwave_keeps_is_refused_before_it_is_settled() {
    use escapement_tickwave_format::writer::{Finish, WriteError, WriteOptions, Writer};
    let options = WriteOptions {
        level: 1,
        ..WriteOptions::default()
    };
    let refused = |e: WriteError| {
        let message = e.to_string();
        assert!(
            matches!(
                e,
                WriteError::Overview {
                    blocks: 1_048_577,
                    shift: 0,
                    ..
                }
            ) && message.ends_with("a coarser overview_shift reaches further"),
            "{message}"
        );
    };
    // To the tick before, as many as tickwave keeps: written.
    let mut w = Writer::new(shift_zero(), options.clone()).unwrap();
    w.edge(0, 1).unwrap();
    w.flush(PAST - 1).unwrap();
    let mut w = Writer::new(shift_zero(), options.clone()).unwrap();
    w.edge(0, 1).unwrap();
    refused(w.flush(PAST).unwrap_err());
    // The flush was refused before it closed anything: the writer goes on.
    w.flush(2).unwrap();
    refused(w.finish(PAST, &[PAST], Finish::default()).unwrap_err());
    // An item past it closes a full block there.
    let mut w = Writer::new(
        shift_zero(),
        WriteOptions {
            block_bytes: 1,
            ..options
        },
    )
    .unwrap();
    w.edge(0, 1).unwrap();
    refused(w.edge(0, PAST).unwrap_err());
}

/// A cut-off file whose last block, or a mark past it, needs more overview blocks than tickwave
/// keeps.
#[test]
fn repair_and_verify_refuse_a_span_needing_more_overview_blocks_than_tickwave_keeps() {
    use escapement_tickwave_format::header::Declaration;
    use escapement_tickwave_format::record::BlockHead;
    use escapement_tickwave_format::repair::{RepairError, Repairer, repair};
    use escapement_tickwave_format::writer::{WriteOptions, Writer};
    let start = |d: Declaration| {
        let mut w = Writer::new(d, WriteOptions::default()).unwrap();
        w.edge(0, 1).unwrap();
        w.flush(2).unwrap();
        w.take_output()
    };
    // A block from 2 to past the bound, holding nothing: a section length of 0 for D.
    let mut blocked = start(shift_zero());
    let head = BlockHead {
        start: 2,
        end: PAST,
        decoded_length: 4,
        words: vec![1 << 31],
    };
    let mut c = zstd::bulk::Compressor::new(1).unwrap();
    c.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))
        .unwrap();
    let frame = c.compress(&[0; 4]).unwrap();
    record::write(&mut blocked, Kind::Block, &[&head.to_bytes(), &frame]);
    // A mark past it.
    let mut marked = start(shift_zero());
    let a = format!(r#"{{"source":"m","annotation":{{"start":{}}}}}"#, PAST - 1);
    record::write(&mut marked, Kind::Annotation, &[a.as_bytes()]);
    // The block is refused as it is read, before its span is settled.
    let mut r = Repairer::new(1);
    let err = r.feed(&blocked).unwrap_err().to_string();
    assert!(err.contains("overview blocks"), "{err}");
    match repair(&marked, 1) {
        Err(RepairError::Decode(e)) if e.to_string().contains("overview blocks") => {}
        other => panic!("{:?}", other.map(|r| r.header.length)),
    }
    let source = Recording {
        timebase: Ratio::ONE,
        length: PAST,
        channels: vec![Channel {
            name: "D".into(),
            source_name: "d".into(),
            source: 0,
            data: ChannelData::Digital(Digital {
                initial: false,
                end: PAST,
                count: 1,
                edges: Box::new([1].into_iter()),
                gaps: Vec::new(),
            }),
            shaping: Default::default(),
        }],
        derived: Vec::new(),
        sources: vec![Source::new(SourceFormat::Other("t".into()), Ratio::ONE)],
    };
    let err = verify(&blocked[..], source).unwrap_err().to_string();
    assert!(err.contains("overview blocks"), "{err}");
}

/// A span with no item settles a run of overview blocks, which the writer writes out once 64
/// wait, the blocks of a batch not yet full with them: the file is the one a batch of 1 writes.
#[test]
fn a_long_span_s_overview_blocks_come_out_as_a_batch_of_one_writes_them() {
    use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
    let write = |batch| {
        let mut w = Writer::new(
            shift_zero(),
            WriteOptions {
                level: 1,
                block_bytes: 16,
                batch,
            },
        )
        .unwrap();
        let mut out = Vec::new();
        for k in 1..40 {
            w.edge(0, k * 7).unwrap();
        }
        w.flush(300).unwrap();
        w.edge(0, 301).unwrap();
        // 2^21 ticks on: past 2^17 the level of one-tick buckets goes in blocks, 512 of them.
        w.flush(1 << 21).unwrap();
        out.extend(w.take_output());
        for k in 0..30 {
            w.edge(0, (1 << 21) + k).unwrap();
        }
        out.extend(w.take_output());
        let end = (1 << 21) + 100;
        out.extend(w.finish(end, &[end], Finish::default()).unwrap().tail);
        out
    };
    let one = write(1);
    assert!(Footer::parse_file(&one).unwrap().header.overview_blocks[0].blocks >= 512);
    for batch in [3, 1000] {
        assert!(write(batch) == one, "batch {batch}");
    }
}

/// A span holding a few items, settled at once: its finest level goes in overview blocks as it
/// settles, and each run of alike buckets between two items is one level all its blocks share,
/// the earlier runs' as much as the last's. A merge of a short dense capture and a long sparse
/// one settles so, and making each block apart held 65 KB a block: 30 GB for 30 minutes at 1 GS/s.
#[test]
fn every_run_of_alike_buckets_one_settle_reaches_shares_one_level_for_its_blocks() {
    use escapement_tickwave_format::overview::{Builder, Settled};
    use std::sync::Arc;
    // Runs of 40 blocks; the level goes in blocks past 32. Each edge lies in its block's third
    // chunk, so the block holding it starts with the run before.
    const RUN: i64 = 40 * 4096;
    let edges: Vec<i64> = (1..=4).map(|k| k * RUN + 2049).collect();
    let d = shift_zero();
    let settle = |each_edge: bool| -> Vec<Settled> {
        let mut b = Builder::new(0, &d.channels, &d.derived);
        let mut settled = Vec::new();
        for &e in &edges {
            b.edge(0, e, e);
            if each_edge {
                b.settle(e).unwrap();
                settled.extend(b.take_settled());
            }
        }
        b.settle(5 * RUN).unwrap();
        settled.extend(b.take_settled());
        settled.retain(|s| s.shift == 0);
        settled
    };
    // Each bucket of one tick from the edges themselves: the level it starts at, and whether
    // an edge lies in it.
    let want = |t: i64| {
        let before = edges.iter().filter(|&&e| e < t).count();
        (before % 2 == 1, edges.contains(&t))
    };
    for each_edge in [false, true] {
        let settled = settle(each_edge);
        assert_eq!(settled.len(), 200);
        for (j, s) in settled.iter().enumerate() {
            assert_eq!(s.k, j as u64);
            for i in 0..4096 {
                let t = (j * 4096 + i) as i64;
                let got = s.level.channels[0].digital(i as u64);
                assert_eq!(
                    got,
                    Some(want(t)),
                    "settled at each edge: {each_edge}, tick {t}"
                );
            }
        }
    }
    let once = settle(false);
    let mut levels: Vec<&Arc<_>> = once.iter().map(|s| &s.level).collect();
    levels.dedup_by(|a, b| Arc::ptr_eq(a, b));
    // The first run, then each edge's block and the run after it.
    assert_eq!(levels.len(), 1 + 4 * 2);
}

/// A burst of 1,000 samples a tick apart, whose density asks for 2^10-tick buckets, in a recording
/// a tick longer than those keep in 2^20 overview blocks: the encoder, which knows the length,
/// takes 2^11-tick buckets instead, and the file verifies. The writer alone refuses it.
#[test]
fn an_encoding_past_the_overview_bound_takes_a_coarser_finest_level() {
    const PAST: i64 = 4_123_223_130_113;
    let recording = || Recording {
        timebase: Ratio::integer(1_000_000_000).unwrap(),
        length: PAST,
        channels: vec![
            Channel {
                name: "V".into(),
                source_name: "v".into(),
                source: 0,
                data: ChannelData::Analog(Analog {
                    significant: None,
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::ONE,
                    count: 1000,
                    breaks: Vec::new(),
                    samples: Samples::F32(Box::new((0..1000).map(|i| i as f32))),
                }),
                shaping: Default::default(),
            },
            Channel {
                name: "D".into(),
                source_name: "d".into(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end: PAST,
                    count: 1,
                    edges: Box::new([PAST / 2].into_iter()),
                    gaps: Vec::new(),
                }),
                shaping: Default::default(),
            },
        ],
        derived: Vec::new(),
        sources: vec![Source::new(
            SourceFormat::Other("test".into()),
            Ratio::integer(1_000_000_000).unwrap(),
        )],
    };
    let options = Options {
        level: 1,
        ..Options::default()
    };
    let file = encode(recording(), &options).unwrap();
    assert_eq!(Footer::parse_file(&file).unwrap().header.overview_shift, 11);
    verify(&file[..], recording()).unwrap();
    let at_ten = Options {
        overview_shift: Some(10),
        ..options
    };
    let err = encode(recording(), &at_ten).unwrap_err().to_string();
    assert!(
        err.contains("a coarser overview_shift reaches further"),
        "{err}"
    );
}
