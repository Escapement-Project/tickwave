//! Each channel's overview (FORMAT.md, "Overview"): its own ladder of levels, those before its
//! strip in overview blocks, a run of alike blocks one record, a level's last block a record of
//! its own, and the strip in the footer.

use escapement_tickwave_format::bundle::bundle;
use escapement_tickwave_format::decode::{Footer, Index};
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::{
    Declaration, DeclaredChannel, DeclaredKind, OverviewLevel, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::overview::{self, AnalogOverview, coarsen, density, duty};
use escapement_tickwave_format::record::{self, Kind};
use escapement_tickwave_format::verify::verify;
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};

mod common;
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Ratio, Recording, Samples, Source,
};

/// At 2^12 ticks a bucket this has 2^18 + 1 buckets, so the levels at 2^12, 2^16 and 2^20 go in
/// overview blocks and 2^24, of 65 buckets, is the strip. At 2^12, 65 blocks of 4,096, the last
/// holding only the bucket that starts at the final tick. The finest shift is given: the
/// density rule would pick a coarser one for these few items.
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
                    lost_before: 0,
                    lost_after: 0,
                    phase: None,
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
    let mut o = Options::default();
    o.overview_shift = Some(12);
    o
}

fn file() -> Vec<u8> {
    encode(recording(EDGES.to_vec()), &options()).unwrap()
}

/// The overview block records in `file`, in order.
fn overview_records(file: &[u8]) -> usize {
    let mut at = escapement_tickwave_format::header::PREAMBLE_LEN;
    let mut n = 0;
    while let record::Next::Record { kind, len, .. } = record::next(&file[at..], usize::MAX) {
        n += usize::from(kind == Kind::OverviewBlock as u8);
        at += len;
    }
    n
}

/// C's digital level of `shift` by FORMAT.md's definition, from its edges alone: each bucket's
/// edges, and its high ticks against its ticks below the length.
fn by_definition(shift: u8) -> Vec<(u8, u8)> {
    let n = ((LENGTH - 1) >> shift) as usize + 1;
    let (mut edges, mut high) = (vec![0u64; n], vec![0u64; n]);
    let (mut level, mut at) = (false, 0i64);
    let add = |a: i64, b: i64, high: &mut Vec<u64>| {
        let mut t = a;
        while t < b {
            let k = (t >> shift) as usize;
            let end = ((k as i64 + 1) << shift).min(b);
            high[k] += (end - t) as u64;
            t = end;
        }
    };
    for t in EDGES {
        if level {
            add(at, t, &mut high);
        }
        level = !level;
        edges[(t >> shift) as usize] += 1;
        at = t;
    }
    if level {
        add(at, LENGTH, &mut high);
    }
    (0..n)
        .map(|k| {
            let ticks = (((k as i64 + 1) << shift).min(LENGTH) - ((k as i64) << shift)) as u64;
            (density(edges[k]), duty(high[k], ticks))
        })
        .collect()
}

#[test]
fn a_long_recording_keeps_each_channels_levels_before_its_strip_in_blocks() {
    let file = file();
    verify(&file[..], recording(EDGES.to_vec())).unwrap();
    let index = Index::of_file(&file).unwrap();
    let h = index.header();
    for c in &h.channels {
        let levels: Vec<(u8, u64)> = c
            .overview_blocks
            .iter()
            .map(|l| (l.shift, l.blocks))
            .collect();
        assert_eq!(levels, [(12, 65), (16, 5), (20, 1)], "{}", c.name);
    }
    assert_eq!(index.footer.channels[0].strip_shift, 24);

    // C's level of 2^12: block 0 holds the edges at 5,000 and 5,001; blocks 1 to 16 are quiet
    // and low, one row; 17 holds 300,000,000; 18 to 63 are quiet and high, one row; and 64, the
    // last, holds the bucket at the final tick, a row of its own.
    let rows: Vec<(u64, u32)> = index.levels[0][0].iter().map(|r| (r.k, r.row.n)).collect();
    assert_eq!(rows, [(0, 1), (1, 16), (17, 1), (18, 46), (64, 1)]);
    assert_eq!(h.channels[0].overview_blocks[0].rows, 5);
    // V's samples, one every 256 buckets and each a value of its own, make every block differ.
    assert_eq!(index.levels[1][0].len(), 65);

    // Expected buckets from the edges themselves, not from the builder that wrote them.
    let level = index.level_in(&file, 0, 0).unwrap();
    assert_eq!(level.buckets, (1 << 18) + 1);
    let digital = |i: u64| {
        let b = level.data.digital(i).unwrap();
        (b.density, b.duty)
    };
    // 5000 and 5001 fall in bucket 1, high for one of its 4,096 ticks: two edges, density 5,
    // duty floor(4,124 / 8,192) = 0. Bucket 0 is low and quiet.
    assert_eq!(digital(0), (0, 0));
    assert_eq!(digital(1), (5, 0));
    assert_eq!(digital(2), (0, 0));
    // 300,000,000 is in bucket 73,242, which ends at 300,003,328: high its last 3,328 ticks,
    // duty floor(97,280 / 8,192) = 11.
    assert_eq!(digital(73_242), (1, 11));
    assert_eq!(digital(73_243), (0, 14));
    // The last edge is at the final tick, the only tick of the last bucket the length holds: it
    // reads low there.
    assert_eq!(digital(1 << 18), (1, 0));

    let v = index.level_in(&file, 1, 0).unwrap();
    let Some(AnalogOverview::F32(v)) = v.data.analog() else {
        panic!("V is f32");
    };
    assert_eq!((v.min[0], v.max[0], v.sum[0]), (0.0, 0.0, 0.0));
    assert_eq!(
        (v.min[1], v.max[1], v.sum[1]),
        (f32::INFINITY, f32::NEG_INFINITY, 0.0)
    );
    assert_eq!((v.min[256], v.max[256], v.sum[256]), (1.0, 1.0, 1.0));
    assert_eq!(v.min[1 << 18], (SAMPLES - 1) as f32);

    // Every coarser level, and the strip: V's folds its finest, and C's each follows from the
    // edges by FORMAT.md's definition.
    let ch = &h.channels[1];
    let finest = index.level_in(&file, 1, 0).unwrap();
    for (li, level) in ch.overview_blocks.iter().enumerate().take(3).skip(1) {
        let shift = level.shift;
        assert_eq!(
            Some(index.level_in(&file, 1, li).unwrap()),
            coarsen(&finest, shift, LENGTH),
            "V, level 2^{shift}"
        );
    }
    assert_eq!(
        Some(index.footer.strip(1).unwrap()),
        coarsen(&finest, 24, LENGTH),
        "V's strip"
    );
    let mut levels: Vec<_> = (0..3)
        .map(|li| index.level_in(&file, 0, li).unwrap())
        .collect();
    levels.push(index.footer.strip(0).unwrap());
    for level in &levels {
        let got: Vec<(u8, u8)> = (0..level.buckets)
            .map(|i| level.data.digital(i).unwrap())
            .map(|b| (b.density, b.duty))
            .collect();
        assert_eq!(
            got,
            by_definition(level.shift),
            "C, level 2^{}",
            level.shift
        );
    }

    // The rows that hold any tick of a span: a block covers 4,096 buckets of 4,096 ticks.
    let refs = &index.levels[0][0];
    assert_eq!(Footer::rows_covering(refs, 12, 0, 1), 0..1);
    assert_eq!(
        Footer::rows_covering(refs, 12, 1 << 24, (1 << 24) + 1),
        1..2
    );
    assert_eq!(
        Footer::rows_covering(refs, 12, (17 << 24) - 1, (17 << 24) + 1),
        1..3
    );
    assert_eq!(Footer::rows_covering(refs, 12, LAST, LENGTH), 4..5);
    assert_eq!(Footer::rows_covering(refs, 12, 0, LENGTH), 0..5);
}

/// One digital channel at 1 GHz with a clock of `period` ticks from 0 to `length`, written with
/// its finest level at `shift`.
fn clock(period: i64, length: i64, shift: u8) -> Vec<u8> {
    let timebase = Ratio::integer(1_000_000_000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "CLK".into(),
            source_name: "clk".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
            overview_shift: None,
            codec: None,
            requires: Vec::new(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
        title: None,
        meta: Default::default(),
        overview_shift: shift,
        requires: Vec::new(),
    };
    let mut w = Writer::new(
        d,
        WriteOptions {
            level: 3,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let mut out = w.take_output();
    for k in 0..length / period {
        w.edge(0, k * period).unwrap();
        out.extend(w.take_output());
    }
    out.extend(w.finish(length, &[length], Finish::default()).unwrap().tail);
    out
}

/// A run of alike blocks is one record and one row, from a signal that repeats or a span with
/// nothing in it; a level's last block is a record of its own even where it is whole and alike.
#[test]
fn runs_collapse_alike_blocks_and_never_take_the_last_one() {
    // A clock of 32 ticks at buckets of 1: every block of 4,096 buckets the same 128 cycles. The
    // length is 64 whole blocks, so the last is whole and alike.
    let file = clock(32, 64 * 4096, 0);
    let index = Index::of_file(&file).unwrap();
    let rows: Vec<(u64, u32)> = index.levels[0][0].iter().map(|r| (r.k, r.row.n)).collect();
    assert_eq!(rows, [(0, 63), (63, 1)]);
    // 2^4: four blocks alike, the last on its own. 2^8, of 1,024 buckets, is the strip.
    let h = index.header();
    let levels: Vec<(u8, u64, u64)> = h.channels[0]
        .overview_blocks
        .iter()
        .map(|l| (l.shift, l.blocks, l.rows))
        .collect();
    assert_eq!(levels, [(0, 64, 2), (4, 4, 2)]);
    assert_eq!(overview_records(&file), 4);

    // An edge at the start and nothing for 2^40 ticks, at buckets of 2^4: 2^24 blocks at the
    // finest level, which take one row for the run of quiet ones and one for the last.
    let timebase = Ratio::integer(1_000_000_000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "D".into(),
            source_name: "d".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
            overview_shift: None,
            codec: None,
            requires: Vec::new(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
        title: None,
        meta: Default::default(),
        overview_shift: 4,
        requires: Vec::new(),
    };
    let started = std::time::Instant::now();
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    w.edge(0, 3).unwrap();
    let mut file = w.take_output();
    let length = (1i64 << 40) + 5;
    file.extend(w.finish(length, &[length], Finish::default()).unwrap().tail);
    let index = Index::of_file(&file).unwrap();
    let rows: Vec<Vec<(u64, u32)>> = index.levels[0]
        .iter()
        .map(|l| l.iter().map(|r| (r.k, r.row.n)).collect())
        .collect();
    // At each level block 0 holds the edge, the quiet blocks after it are a run, and the last,
    // partial, is a row of its own.
    let blocks = |shift: u8| overview::block_count(length, shift);
    for (li, r) in rows.iter().enumerate() {
        let n = blocks(4 + 4 * li as u8);
        let want: Vec<(u64, u32)> = match n {
            1 => vec![(0, 1)],
            2 => vec![(0, 1), (1, 1)],
            n => vec![(0, 1), (1, (n - 2) as u32), (n - 1, 1)],
        };
        assert_eq!(*r, want, "level {li}");
    }
    assert_eq!(rows[0][1].1, (1 << 24) - 1);
    assert_eq!(
        overview_records(&file),
        rows.iter().map(Vec::len).sum::<usize>()
    );
    // A few records, whatever the span: no time goes on the quiet blocks one by one.
    assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
}

/// Each channel's own ladder: its finest level puts about 64 of its items in a bucket, and its
/// strip is its own coarsest level, in the footer, a frame of its own.
#[test]
fn each_channel_has_a_ladder_of_its_own_and_a_strip_in_the_footer() {
    // C's 4 edges over 2^30 ticks: 2^28 ticks an edge, log2(64 x 2^28) = 34; V's sample every
    // 2^20: log2(64 x 2^20) = 26.
    let file = encode(recording(EDGES.to_vec()), &Options::default()).unwrap();
    let footer = Footer::parse_file(&file).unwrap();
    let h = &footer.header;
    assert_eq!((h.shift(0), h.shift(1)), (Some(34), Some(26)));
    // C: 2^30 + 1 ticks at 2^34 a bucket is one, the strip. V: 2^30 / 2^26 = 16 buckets, and the
    // one at the final tick.
    assert!(h.channels[0].overview_blocks.is_empty());
    assert!(h.channels[1].overview_blocks.is_empty());
    assert_eq!(footer.strip(0).unwrap().buckets, 1);
    assert_eq!(footer.strip(1).unwrap().buckets, 17);
    // The strips are the footer's last bytes, one zstd frame each.
    let at = footer.trailer.footer_offset as usize;
    let footer_bytes = &file[at..file.len() - 24];
    let frames = footer_bytes
        .windows(4)
        .filter(|w| *w == 0xFD2F_B528u32.to_le_bytes())
        .count();
    assert_eq!(frames, 2);
}

#[test]
fn a_bundle_leaves_the_overview_blocks_where_they_are() {
    let file = file();
    let bundled = bundle(
        &file,
        &Footer::parse_file(&file).unwrap(),
        [("rec.annotations.toml".into(), "x".repeat(1000))].into(),
        3,
    )
    .unwrap();
    verify(&bundled[..], recording(EDGES.to_vec())).unwrap();
    let (a, b) = (
        Index::of_file(&file).unwrap(),
        Index::of_file(&bundled).unwrap(),
    );
    assert!(bundled.len() > file.len());
    assert_eq!(a.levels, b.levels);
    assert_eq!(a.blocks, b.blocks);
    assert_eq!(
        bundled[..a.footer.trailer.footer_offset as usize],
        file[..a.footer.trailer.footer_offset as usize]
    );
}

#[test]
fn verify_refuses_overview_blocks_that_differ_from_the_content() {
    let file = file();
    let index = Index::of_file(&file).unwrap();
    // V's level at 2^12: a row's frame replaced by a later row's of the same length and other
    // bytes, so every place the pages give stays right, its record's check made again.
    let rows = &index.levels[1][0];
    let (k, r, other) = (0..rows.len())
        .find_map(|k| {
            let frame = |r: &escapement_tickwave_format::decode::LevelRef| {
                &file[r.row.offset as usize..][..r.row.length as usize]
            };
            rows[k + 1..]
                .iter()
                .find(|o| o.row.length == rows[k].row.length && frame(o) != frame(&rows[k]))
                .map(|o| (k, rows[k], frame(o).to_vec()))
        })
        .expect("two rows' frames of one length");
    let at = r.row.offset as usize;
    let record_at = at - record::PAYLOAD_AT - record::OverviewHead::LEN;
    let payload_end = at + other.len();
    let mut spliced = file.clone();
    spliced[at..payload_end].copy_from_slice(&other);
    let mut h = blake3::Hasher::new();
    h.update(&[Kind::OverviewBlock as u8]);
    let n = (payload_end - record_at - record::PAYLOAD_AT) as u32;
    h.update(&n.to_le_bytes());
    h.update(&spliced[record_at + record::PAYLOAD_AT..payload_end]);
    spliced[payload_end..payload_end + 8].copy_from_slice(&h.finalize().as_bytes()[..8]);
    // The random-access reader takes it: its frame checks.
    Index::of_file(&spliced)
        .unwrap()
        .overview_in(&spliced, 1, 0, k)
        .unwrap();
    let err = verify(&spliced[..], recording(EDGES.to_vec())).unwrap_err();
    assert!(
        err.to_string()
            .contains(&format!("block {}: differs from the content", r.k)),
        "row {k}: {err}"
    );
    // Repair holds the overview blocks it keeps to the content too.
    let cut = Footer::parse_file(&spliced).unwrap().trailer.footer_offset as usize;
    assert!(matches!(
        escapement_tickwave_format::repair::repair(&spliced[..cut], 1),
        Err(escapement_tickwave_format::repair::RepairError::Overview)
    ));
}

#[test]
fn a_header_listing_other_overview_levels_is_refused() {
    let file = file();
    let head = Footer::parse_file(&file).unwrap();
    type Edit = Box<dyn Fn(&mut Vec<OverviewLevel>)>;
    let edits: [(&str, Edit); 5] = [
        (
            "a level the ladder lacks",
            Box::new(|levels| levels[0].shift = 13),
        ),
        (
            "one level fewer",
            Box::new(|levels| {
                levels.pop();
            }),
        ),
        ("one block more", Box::new(|levels| levels[0].blocks += 1)),
        (
            "more rows than blocks",
            Box::new(|levels| levels[2].rows = 2),
        ),
        (
            "one row of several blocks",
            Box::new(|levels| levels[0].rows = 1),
        ),
    ];
    for (what, edit) in edits {
        let mut h = head.header.clone();
        edit(&mut h.channels[0].overview_blocks);
        let err = Footer::parse_file(&common::with_header(&file, &h)).expect_err(what);
        assert!(
            err.to_string()
                .contains("the overview levels are not the ones the format sets"),
            "{what}: {err}"
        );
    }
    // A row more than the level's one page holds: the top index lists one page either way, but
    // the page says how many it holds.
    let mut h = head.header.clone();
    h.channels[0].overview_blocks[0].rows = 6;
    let bad = common::with_header(&file, &h);
    Footer::parse_file(&bad).unwrap();
    let err = Index::of_file(&bad).expect_err("refused");
    assert!(err.to_string().contains("its head differs"), "{err}");
    // A row more than one page holds, which takes a second page: the top index lists one.
    h.channels[0].overview_blocks[0].rows = 4097;
    h.channels[0].overview_blocks[0].blocks = 4097;
    let err = Footer::parse_file(&common::with_header(&file, &h)).expect_err("refused");
    assert!(err.to_string().contains("the overview levels"), "{err}");
}
