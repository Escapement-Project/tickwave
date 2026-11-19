//! The writer and the readers as a live recorder and a viewer use them: items pushed as they
//! come, blocks flushed on a clock, a file cut off anywhere, and repair.

use std::collections::BTreeMap;

use escapement_tickwave_format::decode::{BlockChannel, Footer, Index, from_memory};
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::{
    AnalogCoding, ChannelKind, Declaration, DeclaredChannel, DeclaredKind, LEVEL_BIT, SourceFormat,
    SourceHeader,
};
use escapement_tickwave_format::model::{
    Analog, Break, Channel, ChannelData, Digital, Gap, Ratio, Recording, Samples, Source,
};
use escapement_tickwave_format::overview::DigitalBucket;
use escapement_tickwave_format::reader::{Ending, Event, Reader};
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
                overview_shift: None,
                codec: None,
                requires: Vec::new(),
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
                    phase: None,
                },
                shaping: Default::default(),
                overview_shift: None,
                codec: None,
                requires: Vec::new(),
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
        requires: Vec::new(),
    }
}

fn options() -> WriteOptions {
    WriteOptions {
        level: 3,
        block_bytes: 32,
        ..WriteOptions::default()
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
    let index = Index::of_file(&file).unwrap();
    let mut starts = Vec::new();
    for (i, block) in index
        .channel_blocks(&file, 0)
        .unwrap()
        .into_iter()
        .enumerate()
    {
        let BlockChannel::Digital { initial, .. } = block.data else {
            panic!()
        };
        starts.push((block.start, initial));
        assert_eq!(initial, index.blocks[0][i].row.word & LEVEL_BIT != 0);
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

    // The overview's buckets of 16 ticks: bucket 2 (32..48) goes high at 47, one tick of 16,
    // duty floor(44 / 32) = 1; buckets 3 to 6 carry high through the gap; bucket 7 (112..128)
    // holds the gap's end at 120, high on both sides of it and no edge; bucket 9 (144..160) is low
    // at 145 alone, 15 of 16 high, duty floor(436 / 32) = 13, with two edges.
    let strip = footer.strip(0).unwrap();
    let a = &strip.data;
    let bucket = |density, duty| Some(DigitalBucket { density, duty });
    assert_eq!(a.digital(2), bucket(1, 1));
    for b in 3..7 {
        assert_eq!(a.digital(b), bucket(0, 14), "bucket {b}");
    }
    assert_eq!(a.digital(7), bucket(0, 14));
    assert_eq!(a.digital(8), bucket(0, 14));
    assert_eq!(a.digital(9), bucket(5, 13));
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
    // A flush at an item's tick leaves it in the open block; one before the latest is refused.
    w.flush(151).unwrap();
    assert!(matches!(
        w.flush(150),
        Err(WriteError::Flush {
            before: 150,
            last: 151
        })
    ));
    w.flush(152).unwrap();
    // A flush at the latest flush's tick does nothing.
    let at = w.position();
    w.flush(152).unwrap();
    assert_eq!(w.position(), at);
    assert!(matches!(w.sample(1, 1.0), Err(WriteError::NoRun { .. })));
    // A gap from tick 0 on a channel declared high.
    let mut d = declaration();
    d.channels[0].data = DeclaredKind::Digital { initial: true };
    let mut w = Writer::new(d, options()).unwrap();
    assert!(matches!(
        w.gap(0, gap(0, 9)),
        Err(WriteError::GapFromStart { .. })
    ));
}

/// FORMAT.md, "Known-through records": every block record before one ends at or before its tick.
/// A channel's own promise can close a block past the latest promise to all, so the writer
/// refuses a flush behind that block at the call, and takes one at its end: the file then reads
/// in order and repairs.
#[test]
fn a_flush_behind_a_block_a_channels_promise_closed_is_refused() {
    let mut w = Writer::new(
        declaration(),
        WriteOptions {
            block_bytes: 1,
            ..options()
        },
    )
    .unwrap();
    w.edge(0, 10).unwrap();
    w.edge(0, 20).unwrap();
    w.promise_channel(0, 1000).unwrap();
    let err = w.flush(500).unwrap_err();
    assert!(
        matches!(
            err,
            WriteError::FlushBehindBlock {
                before: 500,
                end: 1000,
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(
        err.to_string(),
        "A: a flush at tick 500 comes before the end of the block its promise closed, at 1000"
    );
    w.flush(1000).unwrap();
    let file = w.take_output();
    let (_, ending) = events(&file, file.len());
    assert_eq!(
        ending,
        Ending::CutOff {
            at: file.len() as u64
        }
    );
    let r = repair(&file, 3).unwrap();
    assert_eq!(r.header.length, 1000);
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
        assert_eq!(common::each_generated(&fixed), common::generated(&finished));
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
        // Cut after the known-through record the finish writes at the length, with the last
        // pages and overview blocks after it written again as they were.
        assert!(r.keep as usize <= footer_at, "cut at {cut}");
        let fixed = [&file[..r.keep as usize], &r.tail[..]].concat();
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
                tick: 260,
                phase: 0,
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
                    count: 4,
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
                        phase: 0,
                    }],
                    samples: Samples::F32(Box::new(values.clone().into_iter())),
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
            Ratio::integer(96_000_000).unwrap(),
        )],
    };
    let options = {
        let mut o = Options::default();
        o.overview_shift = Some(4);
        o.block_bytes = 64;
        o.level = 3;
        o
    };
    let encoded = encode(model(), &options).unwrap();
    let header = verify(&encoded[..], model()).unwrap();
    assert_eq!(header.content_id, finished.header.content_id);
    // And the writer's file verifies against the model as well.
    verify(&file[..], model()).unwrap();
}

#[test]
fn annotations_past_the_length_are_dropped_or_cut_and_come_back_as_a_generator_file() {
    let (file, _, finished) = written(&session(), usize::MAX);
    let lines = &common::generated(&file)["verge"];
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
                lost_after: Default::default(),
                view: None,
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
    let index = Index::of_file(&file).unwrap();
    let starts: Vec<(i64, bool)> = index.blocks[0]
        .iter()
        .map(|b| (b.start, b.row.word & LEVEL_BIT != 0))
        .collect();
    // The block from 100 starts at the level from before the gap, high: the gap's end is in it.
    // Its three edges after the gap's low take A high, so the block from 200 starts high. The
    // quiet flush at 300 writes no block of A: its known-through record says A held.
    assert_eq!(starts, [(0, false), (100, true), (200, true)]);
    index.channel_blocks(&file, 0).unwrap();
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
    // The block from 200 starts high; say low. Without decoding the block from 100, which holds
    // the gap's end, the index cannot tell.
    let bad = common::rows_edited(&file, 0, |rows| rows[2].word ^= LEVEL_BIT);
    let index = Index::of_file(&bad).unwrap();
    let err = index.block_in(&bad, 0, 1).unwrap_err();
    assert!(
        err.to_string()
            .contains("the next block starts at the wrong level"),
        "{err}"
    );
}

#[test]
fn a_footer_whose_index_is_not_the_blocks_read_is_refused_in_order() {
    let (file, _) = gap_at_the_boundary(false);
    // Block 0's frame said to start a byte later in its page: still inside the file, so the
    // footer and the page alone take it, but the block record says otherwise.
    let bad = common::rows_edited(&file, 0, |rows| rows[0].offset += 1);
    Index::of_file(&bad).unwrap();
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
        err.to_string()
            .contains("differs from the one the records before it give"),
        "{err}"
    );
}

#[test]
fn a_gap_after_the_block_holding_its_end_is_refused_in_order() {
    let (file, at) = gap_at_the_boundary(false);
    // The records to the flush at 200, then a gap after A's last edge, at 170, that ends inside
    // the block that flush closed.
    let mut bad = file[..at[1] as usize].to_vec();
    let gap = Gap {
        from: 180,
        to: 190,
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

/// A recording cut off after its first level has gone into overview blocks: edges every 1,000
/// ticks to past 2^21, where the level of 16-tick buckets has more than 2^17, flushed as it goes.
fn cut_with_overview_blocks() -> (Vec<u8>, Vec<std::ops::Range<usize>>) {
    let mut w = Writer::new(declaration(), options()).unwrap();
    for k in 1..2_200 {
        w.edge(0, 1_000 * k).unwrap();
        if k % 500 == 0 {
            w.flush(1_000 * k + 1).unwrap();
        }
    }
    w.flush(2_200_000).unwrap();
    let file = w.take_output();
    let mut overview = Vec::new();
    let mut at = escapement_tickwave_format::header::PREAMBLE_LEN;
    while let record::Next::Record { kind, len, .. } = record::next(&file[at..], usize::MAX) {
        if kind == Kind::OverviewBlock as u8 {
            overview.push(at..at + len);
        }
        at += len;
    }
    (file, overview)
}

#[test]
fn a_cut_off_file_holding_an_overview_block_its_content_does_not_give_is_not_repaired() {
    use escapement_tickwave_format::repair::RepairError;
    let (file, overview) = cut_with_overview_blocks();
    assert!(overview.len() > 1, "{} overview blocks", overview.len());
    repair(&file, 3).expect("the file as written repairs");
    // A block past the level's last.
    let mut stray = file.clone();
    let mut payload = record::OverviewHead {
        channel: 0,
        shift: 4,
        k: 9_999,
        n: 1,
    }
    .to_bytes()
    .to_vec();
    payload.extend_from_slice(&file[overview[0].start + 5 + record::OverviewHead::LEN..][..1]);
    record::write(&mut stray, Kind::OverviewBlock, &[&payload]);
    // A block written twice.
    let twice = [&file[..], &file[overview[0].clone()]].concat();
    // The in-order reader refuses either: an overview block's `k` follows its level's last.
    for (what, bytes) in [("a stray block", stray), ("a block twice", twice)] {
        let err = repair(&bytes, 3)
            .err()
            .unwrap_or_else(|| panic!("{what}: repaired"));
        assert!(
            matches!(&err, RepairError::Decode(_)) && err.to_string().contains("is out of place"),
            "{what}: {err}"
        );
    }
}

/// A file whose records go on past the block that ends at its source's length: a gap, then a
/// block. Verify finishes the overview at that block, so what follows is refused rather than
/// taken into an overview no longer there.
#[test]
fn verify_refuses_a_gap_or_a_block_past_the_source_s_length() {
    let source = |gaps: Vec<Gap>| Recording {
        timebase: Ratio::integer(96_000_000).unwrap(),
        length: 10,
        channels: vec![
            Channel {
                name: "A".into(),
                source_name: "dio0".into(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end: 10,
                    count: 0,
                    edges: Box::new(std::iter::empty()),
                    gaps,
                }),
                shaping: Default::default(),
            },
            Channel {
                name: "V".into(),
                source_name: "v".into(),
                source: 0,
                data: ChannelData::Analog(Analog {
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::integer(10).unwrap(),
                    count: 0,
                    breaks: Vec::new(),
                    significant: None,
                    samples: Samples::F32(Box::new(std::iter::empty())),
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
            Ratio::integer(96_000_000).unwrap(),
        )],
    };
    let gap = Gap {
        from: 12,
        to: 15,
        level: false,
    };
    for with_gap in [false, true] {
        let mut w = Writer::new(declaration(), options()).unwrap();
        w.flush(10).unwrap();
        if with_gap {
            w.gap(0, gap).unwrap();
        }
        let mut file = w.take_output();
        file.extend(w.finish(20, &[20], Finish::default()).unwrap().tail);
        let gaps = if with_gap { vec![gap] } else { Vec::new() };
        let err = verify(&file[..], source(gaps)).unwrap_err().to_string();
        // The flush at 10 writes no block: there is nothing before it. A's one block runs to 20.
        assert_eq!(
            err,
            "channel A: a block ends at tick 20, past the source's length, 10"
        );
    }
}

/// FORMAT.md, "Gap and run records": the header leaves out a gap whose `to` is not below its
/// channel's end and a run at or past its channel's count, though their records are in the file.
/// Verify compares the source with the header's lists and takes such records as left out: a
/// recorder that stops inside a loss writes one, and repair writes both again. A gap past the
/// list that the header should have listed is still refused, by the reader's check at the footer.
#[test]
fn verify_takes_the_gap_and_run_records_the_header_leaves_out() {
    let source = |length: i64, end: i64, gaps: Vec<Gap>, values: Vec<f32>| Recording {
        timebase: Ratio::integer(96_000_000).unwrap(),
        length,
        channels: vec![
            Channel {
                name: "A".into(),
                source_name: "dio0".into(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end,
                    count: 1,
                    edges: Box::new([5].into_iter()),
                    gaps,
                }),
                shaping: Default::default(),
            },
            Channel {
                name: "V".into(),
                source_name: "v".into(),
                source: 0,
                data: ChannelData::Analog(Analog {
                    unit: "V".into(),
                    first: 0,
                    period: Ratio::integer(10).unwrap(),
                    count: values.len() as u64,
                    breaks: Vec::new(),
                    significant: None,
                    samples: Samples::F32(Box::new(values.into_iter())),
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
            Ratio::integer(96_000_000).unwrap(),
        )],
    };
    let a = |h: &escapement_tickwave_format::header::Header| match &h.channels[0].data {
        ChannelKind::Digital { end, gaps, .. } => (*end, gaps.clone()),
        _ => unreachable!(),
    };
    let v_count = |h: &escapement_tickwave_format::header::Header| match &h.channels[1].data {
        ChannelKind::Analog { count, .. } => *count,
        _ => unreachable!(),
    };
    let values: Vec<f32> = (0..10).map(|i| i as f32).collect();
    let started = || {
        let mut w = Writer::new(declaration(), options()).unwrap();
        w.edge(0, 5).unwrap();
        w.run(1, 0, 0).unwrap();
        w
    };

    // Finished inside a loss that runs past the length: A ends at the gap's `from`.
    let mut w = started();
    for &v in &values {
        w.sample(1, v).unwrap();
    }
    w.gap(
        0,
        Gap {
            from: 90,
            to: 200,
            level: true,
        },
    )
    .unwrap();
    let mut file = w.take_output();
    let f = w.finish(100, &[100], Finish::default()).unwrap();
    file.extend(f.tail);
    assert_eq!(a(&f.header), (90, Vec::new()));
    verify(&file[..], source(100, 90, Vec::new(), values.clone())).unwrap();

    // Cut off after a gap that began before the flush and a run whose sample came after it:
    // repair finishes at the flush, A ending at the gap's `from` and V before the run.
    let mut w = started();
    for &v in &values[..5] {
        w.sample(1, v).unwrap();
    }
    w.flush(50).unwrap();
    w.gap(
        0,
        Gap {
            from: 40,
            to: 70,
            level: true,
        },
    )
    .unwrap();
    w.run(1, 60, 0).unwrap();
    w.sample(1, 5.0f32).unwrap();
    w.flush(55).unwrap();
    let cut = w.take_output();
    let r = repair(&cut, 3).unwrap();
    let mut fixed = cut[..r.keep as usize].to_vec();
    fixed.extend_from_slice(&r.tail);
    assert_eq!(
        (r.header.length, a(&r.header), v_count(&r.header)),
        (55, (40, Vec::new()), 5)
    );
    // The copy holds the gap record, and the run at 0 and the one past the count.
    let mut reader = Reader::default();
    reader.feed(&fixed);
    let (mut gaps, mut runs) = (0, 0);
    while let Some(e) = reader.next_event().unwrap() {
        match e {
            Event::Gap { .. } => gaps += 1,
            Event::Run { .. } => runs += 1,
            _ => {}
        }
    }
    assert_eq!((gaps, runs), (1, 2));
    verify(&fixed[..], source(55, 40, Vec::new(), values[..5].to_vec())).unwrap();

    // A gap inside the channel's end that the header doesn't list: it is no gap left out. The
    // header states the content id of the same content without it.
    let ended = |gap: Option<Gap>| {
        let mut w = started();
        for &v in &values {
            w.sample(1, v).unwrap();
        }
        if let Some(gap) = gap {
            w.gap(0, gap).unwrap();
        }
        let mut file = w.take_output();
        let f = w.finish(100, &[100], Finish::default()).unwrap();
        file.extend(f.tail);
        (file, f.header)
    };
    let inside = Gap {
        from: 90,
        to: 95,
        level: true,
    };
    let (file, header) = ended(Some(inside));
    assert_eq!(a(&header), (100, vec![inside]));
    let mut h = header.clone();
    if let ChannelKind::Digital { gaps, .. } = &mut h.channels[0].data {
        gaps.clear();
    }
    h.content_id = ended(None).1.content_id;
    assert_ne!(h.content_id, header.content_id);
    let unlisted = common::with_header(&file, &h);
    let err = verify(&unlisted[..], source(100, 100, Vec::new(), values.clone()))
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "channel 0: the footer's gaps or runs differs from what was read"
    );
}

/// FORMAT.md, "Gap and run records": a gap that reaches its channel's end is left out of the
/// header only where its `to` maps at or past the length. The in-order reader refuses a gap
/// record left out whose `to` maps inside it: here the header of a file that lists the gap edited
/// to leave it out.
#[test]
fn a_gap_left_out_ends_at_or_past_the_length() {
    let written = |length: i64, end: i64| {
        let mut w = Writer::new(declaration(), options()).unwrap();
        w.gap(
            0,
            Gap {
                from: 10,
                to: 20,
                level: true,
            },
        )
        .unwrap();
        w.flush(15).unwrap();
        let mut file = w.take_output();
        let f = w.finish(length, &[end], Finish::default()).unwrap();
        file.extend(f.tail);
        (file, f.header)
    };
    let in_order = |file: &[u8]| {
        let mut r = Reader::default();
        r.feed(file);
        while r.next_event()?.is_some() {}
        r.end()
    };
    let (file, header) = written(20, 18);
    let ChannelKind::Digital { end, gaps, .. } = &header.channels[0].data else {
        unreachable!()
    };
    assert_eq!((*end, gaps.len()), (10, 0));
    assert_eq!(in_order(&file).unwrap(), Ending::Complete);
    let (file, mut header) = written(60, 60);
    let ChannelKind::Digital { end, gaps, .. } = &mut header.channels[0].data else {
        unreachable!()
    };
    assert_eq!(gaps.len(), 1);
    (*end, *gaps) = (10, Vec::new());
    assert_eq!(
        in_order(&common::with_header(&file, &header))
            .unwrap_err()
            .to_string(),
        "channel 0: the gap from 10 to 20 reaches past the end, 10, and ends inside the length, 60"
    );
}

/// A file cut off after a gap record whose loss began before its latest known-through tick, and
/// an annotation after it. Where the annotation sets a length past the gap's `to`, repair ends the
/// channel at the gap's `from` and doesn't write the gap record again, so its copy reads in order,
/// the gap list in pages included; where the length stops at or before the `to`, it writes the
/// record again. Recut finishes the cut file as repair does.
#[test]
fn repair_writes_a_gap_left_out_again_only_where_it_ends_at_or_past_the_length() {
    use escapement_tickwave_format::recut::Recutter;
    let cut = |mark: i64| {
        let mut w = Writer::new(declaration(), options()).unwrap();
        // Seventy gaps listed: more than a header holds, so the list goes in pages.
        for i in 0..70 {
            w.gap(
                0,
                Gap {
                    from: 10 * i + 1,
                    to: 10 * i + 2,
                    level: false,
                },
            )
            .unwrap();
        }
        w.flush(1000).unwrap();
        w.gap(
            0,
            Gap {
                from: 990,
                to: 1100,
                level: true,
            },
        )
        .unwrap();
        let serde_json::Value::Object(m) = serde_json::json!({"id": "m", "start": mark}) else {
            unreachable!()
        };
        w.annotation("g", m).unwrap();
        w.take_output()
    };
    for (mark, length, again) in [(1200, 1201, false), (1050, 1051, true)] {
        let file = cut(mark);
        let r = repair(&file, 3).unwrap();
        let mut fixed = file[..r.keep as usize].to_vec();
        fixed.extend_from_slice(&r.tail);
        let ChannelKind::Digital { end, gap_count, .. } = &r.header.channels[0].data else {
            unreachable!()
        };
        assert_eq!((r.header.length, *end, *gap_count), (length, 990, Some(70)));
        let mut recut = Recutter::new(options());
        recut.feed(&file).unwrap();
        let mut copy = recut.take_output();
        let finished = recut.finish().unwrap();
        copy.extend(finished.tail);
        assert_eq!(
            finished.header.content_id, r.header.content_id,
            "mark at {mark}"
        );
        for (what, bytes) in [("repaired", &fixed), ("recut", &copy)] {
            let mut reader = Reader::default();
            reader.feed(bytes);
            let mut gaps = 0;
            while let Some(e) = reader.next_event().unwrap() {
                gaps += u32::from(matches!(e, Event::Gap { .. }));
            }
            assert_eq!(
                reader.end().unwrap(),
                Ending::Complete,
                "{what}, mark at {mark}"
            );
            assert_eq!(gaps, 70 + u32::from(again), "{what}, mark at {mark}");
        }
    }
}

/// A gap record before the cut that reaches its channel's end, where the length repair finishes
/// at passes its `to`: the header can't leave the gap out, and repair keeps the bytes before the
/// cut, so it refuses the file, naming the gap. Recut writes the file again without the record,
/// the channel ending at the gap's `from` or before, and its copy reads in order. In the first,
/// an annotation after the cut sets the length; in the second there is no known-through record,
/// and V's block reaches past the gap's end.
#[test]
fn repair_refuses_a_gap_record_it_keeps_that_the_header_cant_leave_out_and_recut_drops_it() {
    use escapement_tickwave_format::recut::Recutter;
    use escapement_tickwave_format::repair::RepairError;
    let gap = |from, to| Gap {
        from,
        to,
        level: true,
    };
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.edge(0, 5).unwrap();
    w.flush(50).unwrap();
    w.gap(0, gap(40, 70)).unwrap();
    w.flush(55).unwrap();
    let serde_json::Value::Object(m) = serde_json::json!({"id": "m", "start": 80}) else {
        unreachable!()
    };
    w.annotation("g", m).unwrap();
    let marked = w.take_output();
    let mut w = Writer::new(
        declaration(),
        WriteOptions {
            block_bytes: 1,
            ..options()
        },
    )
    .unwrap();
    w.run(1, 0, 0).unwrap();
    for i in 0..10 {
        w.sample(1, i as f32).unwrap();
    }
    w.promise_channel(1, 100).unwrap();
    w.edge(0, 3).unwrap();
    w.promise_channel(0, 50).unwrap();
    w.gap(0, gap(60, 80)).unwrap();
    let unflushed = w.take_output();
    for (what, cut, g, length, end) in [
        ("marked", marked, gap(40, 70), 81, 40),
        ("unflushed", unflushed, gap(60, 80), 100, 50),
    ] {
        match repair(&cut, 3) {
            Err(RepairError::GapRecordKept {
                channel,
                from,
                to,
                end: e,
                length: l,
            }) => assert_eq!(
                (channel.as_str(), from, to, e, l),
                ("A", g.from, g.to, end, length),
                "{what}"
            ),
            Err(e) => panic!("{what}: {e}"),
            Ok(_) => panic!("{what}: repaired"),
        }
        let mut r = Recutter::new(options());
        r.feed(&cut).unwrap();
        let mut copy = r.take_output();
        let f = r.finish().unwrap();
        copy.extend(f.tail);
        let ChannelKind::Digital { end: e, gaps, .. } = &f.header.channels[0].data else {
            unreachable!()
        };
        assert_eq!(
            (f.header.length, *e, gaps.len()),
            (length, end, 0),
            "{what}"
        );
        let mut reader = Reader::default();
        reader.feed(&copy);
        while let Some(e) = reader.next_event().unwrap() {
            assert!(!matches!(e, Event::Gap { .. }), "{what}: a gap record");
        }
        assert_eq!(reader.end().unwrap(), Ending::Complete, "{what}");
    }
}

/// A generator's name names its file, `<stem>.<source>.jsonl` beside the recording, which repair
/// and recut write. The writer refuses a name that is empty or holds whitespace, a '/' or a '\',
/// and the in-order reader refuses an annotation record that carries one, so repair never names
/// a file outside the recording's folder.
#[test]
fn an_annotation_source_that_can_t_name_a_file_on_its_own_is_refused() {
    let mark = || {
        let mut m = serde_json::Map::new();
        m.insert("id".into(), "m".into());
        m.insert("start".into(), 1.into());
        m
    };
    for source in ["verge", "", "a b", "x/../../escaped", "x\\..\\escaped"] {
        let named = source == "verge";
        let mut w = Writer::new(declaration(), options()).unwrap();
        let wrote = w.annotation(source, mark());
        w.edge(0, 1).unwrap();
        w.flush(2).unwrap();
        let mut file = w.take_output();
        let a = record::Annotation {
            source: source.into(),
            line: r#"{"id":"n","start":1}"#.into(),
        };
        record::write(&mut file, Kind::Annotation, &[&a.to_payload()]);
        let repaired = repair(&file, 3);
        if named {
            wrote.unwrap();
            let r = repaired.unwrap();
            let fixed = [&file[..r.keep as usize], &r.tail].concat();
            assert_eq!(common::each_generated(&fixed)["verge"].len(), 2);
            continue;
        }
        match wrote {
            Err(WriteError::Annotation(e)) if e.contains("generator name") => {}
            other => panic!("{source:?}: written: {other:?}"),
        }
        let err = repaired.map(|r| r.keep).unwrap_err().to_string();
        assert!(
            err.starts_with("an annotation record: ") && err.contains("generator name"),
            "{source:?}: {err}"
        );
    }
}

/// ANNOTATIONS.md, "Versions": a line may hold a key or a `measure` a later minor version adds,
/// and its generator's file's first line, which says so, comes only with the bundle. Repair and
/// recut carry such a line whole and name what they carried; a line breaking a rule on a key they
/// know they still leave out.
#[test]
fn repair_and_recut_carry_and_name_what_a_later_minor_version_may_add() {
    use escapement_tickwave_format::recut::Recutter;
    use escapement_tickwave_format::repair::Repairer;
    use escapement_tickwave_format::writer::Bundle;
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.edge(0, 1).unwrap();
    w.flush(2).unwrap();
    let mut cut = w.take_output();
    let carried = [
        r#"{"id":"m/1","start":1,"confidence":"0.9"}"#,
        r#"{"id":"m/2","start":1,"end":2,"measure":{"channel":"A","what":"skew","window":3}}"#,
    ];
    for line in carried
        .iter()
        .chain([&r#"{"id":"m/3","start":1,"channels":["nope"]}"#])
    {
        let a = record::Annotation {
            source: "gen".into(),
            line: (*line).into(),
        };
        record::write(&mut cut, Kind::Annotation, &[&a.to_payload()]);
    }
    let named = [
        "generator \"gen\": carried what this version doesn't know, annotation.confidence, \
         annotation.measure.what \"skew\", annotation.measure.window",
    ];
    let left = |left: &[String]| {
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(
            left[0].contains("annotation \"m/3\": no channel \"nope\""),
            "{left:?}"
        );
    };
    let bundled = || Bundle::Generated { stem: "rec".into() };
    let mut r = Repairer::new(3);
    r.bundle(bundled());
    r.feed(&cut).unwrap();
    let repaired = r.finish().unwrap();
    assert_eq!(repaired.carried, named);
    left(&repaired.left_out);
    let mut whole = cut[..repaired.keep as usize].to_vec();
    whole.extend_from_slice(&repaired.tail);
    assert_eq!(common::each_generated(&whole)["gen"], carried);

    let mut rc = Recutter::new(options());
    rc.bundle(bundled());
    rc.feed(&cut).unwrap();
    assert_eq!(rc.carried(), named);
    left(rc.left_out());
    let mut out = rc.take_output();
    out.extend_from_slice(&rc.finish().unwrap().tail);
    assert_eq!(common::each_generated(&out)["gen"], carried);
}

/// Annotation records whose lines break a rule ANNOTATIONS.md gives a generator's line on a key
/// this version knows, as a writer other than tickwave's, or one from before it held lines to
/// them, leaves them: repair and recut leave each out and name it, the rest resolve bundled, and
/// a line left out sets no length. An id given twice is the reader's to drop: the bundle's
/// sidecar keeps the first. A recut of the repaired file, finished, keeps its content id.
#[test]
fn repair_and_recut_leave_out_an_annotation_its_bundle_would_not_resolve() {
    use escapement_tickwave_format::annotations::{Included, resolve};
    use escapement_tickwave_format::recut::Recutter;
    use escapement_tickwave_format::repair::Repairer;
    use escapement_tickwave_format::writer::Bundle;
    let mut w = Writer::new(declaration(), options()).unwrap();
    w.edge(0, 1).unwrap();
    w.flush(2).unwrap();
    let mut cut = w.take_output();
    for line in [
        r#"{"id": "m/1", "start": 1}"#,
        r#"{"id": "m/2", "start": 1, "channels": ["nope"]}"#,
        r#"{"start": 900}"#,
        r#"{"id": "m/1", "start": 1}"#,
    ] {
        let a = record::Annotation {
            source: "gen".into(),
            line: line.into(),
        };
        record::write(&mut cut, Kind::Annotation, &[&a.to_payload()]);
    }
    let left_as_wanted = |left: &[String]| {
        let want = [
            "generator \"gen\": annotation \"m/2\": no channel \"nope\"",
            "generator \"gen\": annotation with no id: missing field `id`",
        ];
        assert_eq!(left.len(), want.len(), "{left:?}");
        for (got, want) in left.iter().zip(want) {
            assert!(got.starts_with(want), "{got}");
        }
    };
    let ids = |file: &[u8]| -> Vec<String> {
        let footer = Footer::parse_file(file).unwrap();
        let header = &footer.header;
        let files = footer
            .read_sidecar(from_memory(file))
            .map(|s| s.map(|s| s.files))
            .unwrap()
            .expect("bundled");
        let included: Vec<Included<'_>> = files
            .iter()
            .filter(|(name, _)| name.ends_with(".jsonl"))
            .map(|(name, text)| Included { name, text })
            .collect();
        resolve(&files["rec.annotations.toml"], &included, header, None)
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect()
    };
    let bundled = || Bundle::Generated { stem: "rec".into() };

    let mut r = Repairer::new(3);
    r.bundle(bundled());
    r.feed(&cut).unwrap();
    let repaired = r.finish().unwrap();
    left_as_wanted(&repaired.left_out);
    assert_eq!(repaired.header.length, 2);
    let mut whole = cut[..repaired.keep as usize].to_vec();
    whole.extend_from_slice(&repaired.tail);
    assert_eq!(common::each_generated(&whole)["gen"].len(), 2);
    assert_eq!(ids(&whole), ["gen/m/1"]);

    for (input, finished) in [(&cut, false), (&whole, true)] {
        let mut rc = Recutter::new(options());
        rc.bundle(bundled());
        rc.feed(input).unwrap();
        if finished {
            // Repair cut the file at its known-through record and wrote again only the
            // annotation that resolves.
            assert!(rc.left_out().is_empty(), "{:?}", rc.left_out());
        } else {
            left_as_wanted(rc.left_out());
        }
        let mut out = rc.take_output();
        let f = rc.finish().unwrap();
        out.extend_from_slice(&f.tail);
        let h = Footer::parse_file(&out).unwrap().header;
        assert_eq!(h.length, 2, "finished: {finished}");
        assert_eq!(ids(&out), ["gen/m/1"], "finished: {finished}");
        if finished {
            assert_eq!(h.content_id, repaired.header.content_id);
        }
    }
}
