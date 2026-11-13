//! The writer and the readers as a live recorder and a viewer use them: items pushed as they
//! come, blocks flushed on a clock, a file cut off anywhere, and repair.

use std::collections::BTreeMap;

use escapement_tickwave_format::decode::{BlockChannel, Footer};
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::{
    AnalogCoding, ChannelKind, Declaration, DeclaredChannel, DeclaredKind, LEVEL_BIT, SourceFormat,
    SourceHeader,
};
use escapement_tickwave_format::model::{
    Analog, Break, Channel, ChannelData, Digital, Gap, Ratio, Recording, Samples, Source,
};
use escapement_tickwave_format::reader::{Ending, Reader};
use escapement_tickwave_format::record::{self, Kind};
use escapement_tickwave_format::repair::repair;
use escapement_tickwave_format::verify::verify;
use escapement_tickwave_format::writer::{
    Finish, WriteError, WriteOptions, Writer, generator_file,
};

mod common;

/// A digital channel A, low at tick 0, and an analog channel V sampled every 10 ticks, with
/// overview buckets of 16 ticks.
fn declaration() -> Declaration {
    Declaration {
        timebase: Ratio::integer(96_000_000).unwrap(),
        channels: vec![
            DeclaredChannel {
                name: "A".into(),
                source_name: "dio0".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Default::default(),
            },
            DeclaredChannel {
                name: "V".into(),
                source_name: "v".into(),
                source: 0,
                data: DeclaredKind::Analog {
                    significant: None,
                    unit: "V".into(),
                    period: Ratio::integer(10).unwrap(),
                    coding: AnalogCoding::F32,
                },
                shaping: Default::default(),
            },
        ],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("test".into()),
            Ratio::integer(96_000_000).unwrap(),
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
    }
}

fn options() -> WriteOptions {
    WriteOptions {
        level: 3,
        block_bytes: 32,
        batch: 1,
    }
}

#[derive(Clone, Debug)]
enum Op {
    Edge(i64),
    Run(i64),
    Sample(f32),
    Gap(Gap),
    Flush(i64),
    Note(i64, Option<i64>),
}

fn apply(w: &mut Writer, op: &Op) -> Result<(), WriteError> {
    match op {
        Op::Edge(t) => w.edge(0, *t),
        Op::Run(t) => w.run(1, *t, 0),
        Op::Sample(v) => w.sample(1, *v),
        Op::Gap(g) => w.gap(0, *g),
        Op::Flush(t) => w.flush(*t),
        Op::Note(start, end) => {
            let mut a = serde_json::Map::new();
            a.insert("id".into(), format!("marker/{start}").into());
            a.insert("start".into(), (*start).into());
            if let Some(end) = end {
                a.insert("end".into(), (*end).into());
            }
            w.annotation("verge", a)
        }
    }
}

/// A recorder's session: samples every 10 ticks from 0, edges, a gap learned after a flush inside
/// it, a pause, markers, and flushes on its clock. Ends at 400.
fn session() -> Vec<Op> {
    let mut ops = vec![Op::Run(0)];
    let sample = |ops: &mut Vec<Op>, i: u64| ops.push(Op::Sample(i as f32 * 0.25 - 1.0));
    for i in 0..5 {
        sample(&mut ops, i);
    }
    ops.push(Op::Edge(5 * 10 - 3));
    ops.push(Op::Note(47, None));
    for i in 5..8 {
        sample(&mut ops, i);
    }
    // The flush falls inside the gap the recorder has not heard of yet.
    ops.push(Op::Flush(80));
    ops.push(Op::Gap(Gap {
        from: 48,
        to: 120,
        level: true,
    }));
    for i in 8..12 {
        sample(&mut ops, i);
    }
    ops.push(Op::Flush(115));
    for i in 12..15 {
        sample(&mut ops, i);
    }
    ops.push(Op::Edge(145));
    ops.push(Op::Edge(146));
    for i in 15..20 {
        sample(&mut ops, i);
    }
    ops.push(Op::Flush(200));
    // A pause: the samples start again at 260, at the same period.
    ops.push(Op::Run(260));
    for i in 20..25 {
        sample(&mut ops, i);
    }
    ops.push(Op::Edge(301));
    ops.push(Op::Note(310, Some(1000)));
    ops.push(Op::Note(500, None));
    for i in 25..34 {
        sample(&mut ops, i);
    }
    ops
}

const LENGTH: i64 = 400;

/// The bytes the session writes, finished at `LENGTH`, drained in pieces of `piece` bytes as they
/// come, and the file's length after each op.
fn written(
    ops: &[Op],
    piece: usize,
) -> (
    Vec<u8>,
    Vec<u64>,
    escapement_tickwave_format::writer::Finished,
) {
    let mut w = Writer::new(declaration(), options()).unwrap();
    let mut out = Vec::new();
    let mut at = Vec::new();
    for op in ops {
        apply(&mut w, op).unwrap_or_else(|e| panic!("{op:?}: {e}"));
        while !w.output().is_empty() {
            let n = piece.min(w.output().len());
            out.extend_from_slice(&w.output()[..n]);
            w.consume(n);
        }
        at.push(out.len() as u64);
    }
    let finished = w.finish(LENGTH, &[LENGTH], Finish::default()).unwrap();
    out.extend_from_slice(&finished.tail);
    (out, at, finished)
}

fn events(file: &[u8], piece: usize) -> (Vec<String>, Ending) {
    let mut r = Reader::default();
    let mut got = Vec::new();
    for chunk in file.chunks(piece.max(1)) {
        r.feed(chunk);
        while let Some(e) = r.next_event().unwrap() {
            got.push(format!("{e:?}"));
        }
    }
    (got, r.end().unwrap())
}

#[test]
fn a_gap_learned_after_a_flush_inside_it_keeps_the_level_from_before() {
    let (file, _, finished) = written(&session(), usize::MAX);
    let footer = Footer::parse_file(&file).unwrap();
    let ChannelKind::Digital {
        gaps, end, edges, ..
    } = &footer.header.channels[0].data
    else {
        panic!()
    };
    assert_eq!(
        (gaps.as_slice(), *end, *edges),
        (
            &[Gap {
                from: 48,
                to: 120,
                level: true
            }][..],
            LENGTH,
            4
        )
    );
    assert_eq!(finished.header, footer.header);
    // Every block decodes, and each checks the next one's starting level.
    let mut starts = Vec::new();
    for i in 0..footer.rows.len() {
        let (offset, len) = footer.block_location(i);
        let block = footer
            .decode_block(i, &file[offset as usize..][..len as usize])
            .unwrap();
        let BlockChannel::Digital { initial, .. } = block.channels[0] else {
            panic!()
        };
        starts.push((block.start, initial));
        assert_eq!(initial, footer.rows[i].channels[0] & LEVEL_BIT != 0);
    }
    // High after the edge at 47, through the gap; the block holding 120 starts high too, since
    // the gap's end is inside it. From 120 the level is the gap's, high: the edges at 145 and 146
    // take it low and high again, and 301 low.
    let level_at = |tick: i64| starts.iter().rev().find(|(s, _)| *s <= tick).unwrap().1;
    assert!(!level_at(0));
    assert!(level_at(80));
    assert!(level_at(115));
    let after = starts.iter().find(|(s, _)| *s > 146 && *s <= 301).unwrap();
    assert!(after.1, "{starts:?}");

    // The overview's buckets of 16 ticks: bucket 2 (32..48) changes at 47; buckets 3 to 6 carry
    // high through the gap; bucket 7 (112..128) holds the gap's end at 120 and starts high.
    let a = &footer.overview[0].channels[0];
    assert_eq!(a.digital(2), Some((false, true)));
    for b in 3..7 {
        assert_eq!(a.digital(b), Some((true, false)), "bucket {b}");
    }
    assert_eq!(a.digital(7), Some((true, true)));
    assert_eq!(a.digital(8), Some((true, false)));
    assert_eq!(a.digital(9), Some((true, true)));
}

#[test]
fn a_late_gap_or_an_edge_in_a_gap_is_refused() {
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.edge(0, 5).unwrap();
    w.flush(100).unwrap();
    let gap = |from, to| Gap {
        from,
        to,
        level: false,
    };
    assert!(matches!(
        w.gap(0, gap(10, 90)),
        Err(WriteError::GapLate {
            to: 90,
            closed: 100,
            ..
        })
    ));
    assert!(matches!(
        w.gap(0, gap(5, 150)),
        Err(WriteError::GapOrder { .. })
    ));
    w.gap(0, gap(10, 150)).unwrap();
    assert!(matches!(
        w.edge(0, 150),
        Err(WriteError::EdgeInGap {
            tick: 150,
            to: 150,
            ..
        })
    ));
    assert!(matches!(
        w.gap(0, gap(140, 160)),
        Err(WriteError::GapOrder { .. })
    ));
    w.edge(0, 151).unwrap();
    assert!(matches!(w.flush(151), Err(WriteError::Flush { .. })));
    w.flush(152).unwrap();
    // A flush at the open block's start does nothing.
    w.flush(152).unwrap();
    assert!(matches!(w.sample(1, 1.0), Err(WriteError::NoRun { .. })));
    // A gap from tick 0 on a channel declared high.
    let mut d = declaration();
    d.channels[0].data = DeclaredKind::Digital { initial: true };
    let mut w = Writer::new(d, options()).unwrap();
    assert!(matches!(
        w.gap(0, gap(0, 9)),
        Err(WriteError::GapFromZero { .. })
    ));
}

#[test]
fn every_cut_reads_up_to_its_last_whole_record() {
    let (file, _, _) = written(&session(), usize::MAX);
    let (whole, ending) = events(&file, usize::MAX);
    assert_eq!(ending, Ending::Complete);
    assert!(whole.last().unwrap().starts_with("Footer"));
    for piece in [1, 7, 64] {
        assert_eq!(events(&file, piece), (whole.clone(), Ending::Complete));
    }
    let mut last = 0;
    for n in 0..file.len() {
        let (got, ending) = events(&file[..n], usize::MAX);
        assert_eq!(got[..], whole[..got.len()], "cut at {n}");
        assert!(got.len() >= last, "cut at {n}");
        last = got.len();
        let Ending::CutOff { at } = ending else {
            panic!("cut at {n} read as complete")
        };
        assert!(at <= n as u64, "cut at {n}");
    }
    // A power cut's zeros after the last whole record stop the reader there too.
    let (_, at, _) = written(&session(), usize::MAX);
    let cut = at[at.len() / 2] as usize;
    let zeroed = [&file[..cut], &[0; 300]].concat();
    let (got, ending) = events(&zeroed, 13);
    assert_eq!(ending, Ending::CutOff { at: cut as u64 });
    assert_eq!(got[..], whole[..got.len()]);
}

#[test]
fn repair_writes_what_finishing_at_the_same_block_would() {
    let ops = session();
    let (file, at, _) = written(&ops, usize::MAX);
    let mut repaired_any = 0;
    for (k, op) in ops.iter().enumerate() {
        let Op::Flush(end) = *op else { continue };
        // The file as a crash left it: every record up to this flush, and half a record after.
        let cut = at[k] as usize;
        let crashed = [&file[..cut], &file[cut..][..9.min(file.len() - cut)]].concat();
        let r = repair(&crashed, 3).unwrap();
        assert_eq!(r.keep, cut as u64);
        let mut fixed = crashed[..cut].to_vec();
        fixed.extend_from_slice(&r.tail);

        let mut w = Writer::new(declaration(), options()).unwrap();
        for op in &ops[..=k] {
            apply(&mut w, op).unwrap();
        }
        let mut finished = w.take_output();
        let f = w.finish(end, &[end], Finish::default()).unwrap();
        finished.extend_from_slice(&f.tail);
        assert_eq!(fixed, finished, "repaired after the flush at {end}");
        assert_eq!(r.header, f.header);
        assert_eq!(r.generated, f.generated);
        // A gap still open at the cut: the level is not known from its start.
        if end < 120 {
            let ChannelKind::Digital { end: e, gaps, .. } = &r.header.channels[0].data else {
                panic!()
            };
            assert!(end <= 80 || (*e == 48 && gaps.is_empty()), "{e} {gaps:?}");
        }
        Footer::parse_file(&fixed).unwrap();
        repaired_any += 1;
    }
    assert_eq!(repaired_any, 3);
    assert!(matches!(
        repair(&file, 3),
        Err(escapement_tickwave_format::repair::RepairError::Finished)
    ));
}

#[test]
fn a_file_cut_in_its_footer_or_trailer_is_repaired_to_the_file_it_was() {
    // Without the mark past the length: repair would take the length one past it.
    let ops: Vec<Op> = session()
        .into_iter()
        .filter(|op| !matches!(op, Op::Note(500, _)))
        .collect();
    let (file, _, _) = written(&ops, usize::MAX);
    let footer_at = Footer::parse_file(&file).unwrap().trailer.footer_offset as usize;
    for cut in footer_at..file.len() {
        let r = repair(&file[..cut], 3).unwrap_or_else(|e| panic!("cut at {cut}: {e}"));
        assert_eq!(r.keep as usize, footer_at, "cut at {cut}");
        let fixed = [&file[..footer_at], &r.tail[..]].concat();
        assert!(fixed == file, "cut at {cut}: the repaired file differs");
    }
}

#[test]
fn a_record_past_what_the_reader_holds_is_refused_rather_than_read_as_cut_off() {
    let (file, _, _) = written(&session(), usize::MAX);
    let mut r = Reader::new(64);
    r.feed(&file);
    let err = loop {
        match r.next_event() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("read as cut off: {:?}", r.end()),
            Err(e) => break e,
        }
    };
    assert!(
        err.to_string().contains("more than this reader holds (64)"),
        "{err}"
    );
}

#[test]
fn runs_place_the_samples_and_come_back_as_breaks() {
    let (file, _, finished) = written(&session(), usize::MAX);
    let footer = Footer::parse_file(&file).unwrap();
    let ChannelKind::Analog {
        first,
        count,
        breaks,
        ..
    } = &footer.header.channels[1].data
    else {
        panic!()
    };
    assert_eq!(
        (*first, *count, breaks.as_slice()),
        (
            0,
            34,
            &[Break {
                lost: 0,
                index: 20,
                tick: 260
            }][..]
        )
    );
    assert_eq!(footer.sample_tick(1, 19), Some(190));
    assert_eq!(footer.sample_tick(1, 20), Some(260));
    assert_eq!(footer.sample_tick(1, 33), Some(390));

    // The same content through the encoder, from a model with the break, has the same content id.
    let values: Vec<f32> = (0..34).map(|i| i as f32 * 0.25 - 1.0).collect();
    let model = || Recording {
        timebase: Ratio::integer(96_000_000).unwrap(),
        length: LENGTH,
        channels: vec![
            Channel {
                name: "A".into(),
                source_name: "dio0".into(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end: LENGTH,
                    edges: Box::new([47, 145, 146, 301].into_iter()),
                    gaps: vec![Gap {
                        from: 48,
                        to: 120,
                        level: true,
                    }],
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
                    count: 34,
                    breaks: vec![Break {
                        lost: 0,
                        index: 20,
                        tick: 260,
                    }],
                    samples: Samples::F32(Box::new(values.clone().into_iter())),
                }),
                shaping: Default::default(),
            },
        ],
        derived: Vec::new(),
        sources: vec![Source::new(
            SourceFormat::Other("test".into()),
            Ratio::integer(96_000_000).unwrap(),
        )],
    };
    let options = Options {
        overview_shift: Some(4),
        block_bytes: 64,
        level: 3,
        ..Options::default()
    };
    let encoded = encode(model(), &options).unwrap();
    let header = verify(&encoded[..], model()).unwrap();
    assert_eq!(header.content_id, finished.header.content_id);
    // And the writer's file verifies against the model as well.
    verify(&file[..], model()).unwrap();
}

#[test]
fn annotations_past_the_length_are_dropped_or_cut_and_come_back_as_a_generator_file() {
    let (_, _, finished) = written(&session(), usize::MAX);
    let lines = &finished.generated["verge"];
    assert_eq!(
        lines,
        &[
            r#"{"id":"marker/47","start":47}"#.to_string(),
            // An annotation's keys come back in order.
            r#"{"end":400,"id":"marker/310","start":310}"#.to_string(),
        ]
    );
    let text = generator_file("verge", Some("abc"), finished.content_id(), lines);
    let first = text.lines().next().unwrap();
    assert_eq!(
        first,
        format!(
            r#"{{"format":"tickwave-annotations/1","source":"verge","version":"abc","recording_id":"{}"}}"#,
            finished.content_id()
        )
    );
    assert_eq!(text.lines().count(), 3);
}

#[test]
fn finish_takes_what_the_source_states_at_its_end() {
    let ops = session();
    let mut w = Writer::new(declaration(), options()).unwrap();
    for op in &ops {
        apply(&mut w, op).unwrap();
    }
    let mut out = w.take_output();
    let extra: BTreeMap<String, String> = [("pairs_lost".to_string(), "0".to_string())].into();
    let f = w
        .finish(
            LENGTH,
            &[LENGTH],
            Finish {
                extra: vec![extra.clone()],
                bundle: None,
                drop_gaps_past_end: false,
            },
        )
        .unwrap();
    out.extend_from_slice(&f.tail);
    let footer = Footer::parse_file(&out).unwrap();
    assert_eq!(footer.header.sources[0].extra, extra);
    // The in-order reader accepts a footer that adds to source.extra, but nothing else.
    let (_, ending) = events(&out, 100);
    assert_eq!(ending, Ending::Complete);
}

#[test]
fn a_finish_that_contradicts_the_items_is_refused() {
    let gapped = || {
        let mut w = Writer::new(declaration(), options()).unwrap();
        w.edge(0, 30).unwrap();
        w.gap(
            0,
            Gap {
                from: 40,
                to: 60,
                level: false,
            },
        )
        .unwrap();
        w
    };
    // An end before a gap's end, which the last block has passed.
    assert!(matches!(
        gapped().finish(100, &[50], Finish::default()),
        Err(WriteError::GapPastEnd {
            to: 60,
            end: 50,
            ..
        })
    ));
    assert!(matches!(
        gapped().finish(100, &[], Finish::default()),
        Err(WriteError::Ends {
            given: 0,
            expected: 1
        })
    ));
    // A recording that ends inside the gap: the channel's level is known up to the gap's start.
    let f = gapped().finish(50, &[50], Finish::default()).unwrap();
    assert!(matches!(
        &f.header.channels[0].data,
        ChannelKind::Digital { end: 40, gaps, .. } if gaps.is_empty()
    ));
}

/// A gap from 30 to 100, learned before the flush at 100 that its end starts: A is high from 10,
/// low from the gap's end, then high and low again at 150 and 160, high at 170, and the
/// recording is finished at `end` or repaired there.
fn gap_at_the_boundary(level: bool) -> (Vec<u8>, Vec<u64>) {
    let mut w = Writer::new(declaration(), options()).unwrap();
    let mut at = Vec::new();
    let mut out = Vec::new();
    let mut step = |w: &mut Writer, out: &mut Vec<u8>| {
        out.extend(w.take_output());
        at.push(out.len() as u64);
    };
    w.edge(0, 10).unwrap();
    w.gap(
        0,
        Gap {
            from: 30,
            to: 100,
            level,
        },
    )
    .unwrap();
    w.flush(100).unwrap();
    step(&mut w, &mut out);
    for t in [150, 160, 170] {
        w.edge(0, t).unwrap();
    }
    w.flush(200).unwrap();
    step(&mut w, &mut out);
    // A quiet time-limit flush: an empty block still says the level held.
    w.flush(300).unwrap();
    step(&mut w, &mut out);
    let f = w.finish(400, &[400], Finish::default()).unwrap();
    out.extend(f.tail);
    (out, at)
}

#[test]
fn a_gap_ending_where_a_block_starts_takes_effect_in_that_block() {
    let (file, at) = gap_at_the_boundary(false);
    let footer = Footer::parse_file(&file).unwrap();
    let starts: Vec<(i64, bool)> = footer
        .rows
        .iter()
        .map(|r| (r.start, r.channels[0] & LEVEL_BIT != 0))
        .collect();
    // The block from 100 starts at the level from before the gap, high: the gap's end is in it.
    // Its three edges after the gap's low take A high, so the block from 200 starts high, and the
    // empty one from 300 too.
    assert_eq!(starts, [(0, false), (100, true), (200, true), (300, true)]);
    for i in 0..footer.rows.len() {
        let (o, n) = footer.block_location(i);
        footer
            .decode_block(i, &file[o as usize..][..n as usize])
            .unwrap();
    }
    let (_, ending) = events(&file, 17);
    assert_eq!(ending, Ending::Complete);
    // Repaired where the gap's end is the recording's: the gap is dropped, and A is known up to
    // its start, as finishing there would have it.
    let r = repair(&file[..at[0] as usize], 3).unwrap();
    let ChannelKind::Digital { end, gaps, .. } = &r.header.channels[0].data else {
        panic!()
    };
    assert_eq!((*end, gaps.len()), (30, 0));
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.edge(0, 10).unwrap();
    w.gap(
        0,
        Gap {
            from: 30,
            to: 100,
            level: false,
        },
    )
    .unwrap();
    w.flush(100).unwrap();
    let f = w.finish(100, &[100], Finish::default()).unwrap();
    assert_eq!(f.header, r.header);
}

#[test]
fn a_gap_s_level_is_part_of_the_content() {
    let id = |level| {
        Footer::parse_file(&gap_at_the_boundary(level).0)
            .unwrap()
            .header
            .content_id
    };
    assert_ne!(id(false), id(true));
}

#[test]
fn a_block_that_starts_at_the_wrong_level_after_a_gap_ends_in_the_one_before_is_refused() {
    let (file, _) = gap_at_the_boundary(false);
    // The blocks from 200 and 300 start high; say low. Without decoding the block from 100, which
    // holds the gap's end, the footer cannot tell: from 200 on the levels agree with each other.
    let bad = common::refooted(&file, |footer| {
        for i in [2, 3] {
            let at = common::word_at(footer, 2, i, 0);
            let word = u32::from_le_bytes(footer[at..at + 4].try_into().unwrap()) ^ LEVEL_BIT;
            footer[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
    });
    let footer = Footer::parse_file(&bad).unwrap();
    let (o, n) = footer.block_location(1);
    let err = footer
        .decode_block(1, &bad[o as usize..][..n as usize])
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("the next block starts at the wrong level"),
        "{err}"
    );
}

#[test]
fn a_footer_whose_index_is_not_the_blocks_read_is_refused_in_order() {
    let (file, _) = gap_at_the_boundary(false);
    // Block 0's frame said to start a byte later: still inside the file, so the footer alone
    // takes it, but the block record says otherwise.
    let bad = common::refooted(&file, |footer| {
        let at = common::word_at(footer, 2, 0, 0) - 24 + 8;
        let offset = u64::from_le_bytes(footer[at..at + 8].try_into().unwrap()) + 1;
        footer[at..at + 8].copy_from_slice(&offset.to_le_bytes());
    });
    Footer::parse_file(&bad).unwrap();
    let mut r = Reader::default();
    r.feed(&bad);
    let err = loop {
        match r.next_event() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("read whole"),
            Err(e) => break e,
        }
    };
    assert!(
        err.to_string().contains("index differs from the blocks"),
        "{err}"
    );
}

#[test]
fn a_gap_after_the_block_holding_its_end_is_refused_in_order() {
    let (file, at) = gap_at_the_boundary(false);
    // The records to the end of the empty block from 200, then a gap after A's last edge, at 170,
    // that ends inside that block.
    let mut bad = file[..at[2] as usize].to_vec();
    let gap = Gap {
        from: 180,
        to: 250,
        level: true,
    };
    record::write(&mut bad, Kind::Gap, &[&record::gap_bytes(0, gap)]);
    let mut r = Reader::default();
    r.feed(&bad);
    let err = loop {
        match r.next_event() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("read as cut off"),
            Err(e) => break e,
        }
    };
    assert!(err.to_string().contains("out of place"), "{err}");
}
