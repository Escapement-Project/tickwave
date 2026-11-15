//! A recorder attached to a stream already running: its source's tick 1,000 lies at the
//! recording's tick 0, an origin of -1,000. Repair and recut keep its channels' source ticks and
//! its origin, and end each digital channel at the source tick of the last block's end.

use escapement_tickwave_format::decode::{BlockChannel, Footer, Index};
use escapement_tickwave_format::header::{
    AnalogCoding, ChannelKind, Declaration, DeclaredChannel, DeclaredKind, SourceFormat,
    SourceHeader,
};
use escapement_tickwave_format::model::{Gap, Ratio, Sample, Shaping};
use escapement_tickwave_format::recut::Recutter;
use escapement_tickwave_format::repair::repair;
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};

fn declaration() -> Declaration {
    let tb = Ratio::integer(1_000).unwrap();
    Declaration {
        timebase: tb,
        channels: vec![
            DeclaredChannel {
                name: "V".into(),
                source_name: "v".into(),
                source: 0,
                data: DeclaredKind::Analog {
                    unit: "V".into(),
                    period: Ratio::integer(10).unwrap(),
                    coding: AnalogCoding::F32,
                    significant: None,
                },
                shaping: Shaping::default(),
                overview_shift: None,
            },
            DeclaredChannel {
                name: "D".into(),
                source_name: "d".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Shaping::default(),
                overview_shift: None,
            },
        ],
        derived: Vec::new(),
        sources: vec![SourceHeader {
            origin: -1_000,
            ..SourceHeader::new(SourceFormat::Other("verge".into()), tb)
        }],
        title: None,
        meta: Default::default(),
        overview_shift: 4,
    }
}

fn options() -> WriteOptions {
    WriteOptions {
        level: 3,
        block_bytes: 1 << 20,
        ..WriteOptions::default()
    }
}

/// The flushes, at axis ticks: 300 and 700, source ticks 1,300 and 1,700.
const FLUSHES: [i64; 2] = [300, 700];

/// V: 100 samples 10 ticks apart from source tick 1,000, a second run from 1,600 after 2 lost.
/// D: edges at 1,005, 1,500 and 2,000, unknown from 1,200 to 1,350, high after. Flushed at each
/// of `FLUSHES`, in source-tick order, stopping after `stop` of them, and at 1,100 after the last
/// item. The writer, and where each flush's bytes end.
fn written(stop: usize) -> (Writer, Vec<u8>, Vec<usize>) {
    let mut w = Writer::new(declaration(), options()).unwrap();
    let mut out = w.take_output();
    let mut at = Vec::new();
    // (source tick, what, sample index): a gap, a sample, an edge.
    let mut items: Vec<(i64, u8, u64)> = (0..100u64)
        .map(|i| match i {
            0..50 => (1_000 + 10 * i as i64, 1, i),
            _ => (1_600 + 10 * (i as i64 - 50), 1, i),
        })
        .collect();
    items.extend([1_005, 1_500, 2_000].map(|t| (t, 2, 0)));
    items.push((1_200, 0, 0));
    items.sort();
    let mut flushes = FLUSHES.iter().peekable();
    for (tick, kind, i) in items {
        if let Some(&&f) = flushes.peek()
            && tick - 1_000 >= f
        {
            w.flush(f).unwrap();
            out.extend(w.take_output());
            at.push(out.len());
            flushes.next();
            if at.len() == stop {
                return (w, out, at);
            }
        }
        match kind {
            0 => w
                .gap(
                    1,
                    Gap {
                        from: tick,
                        to: tick + 150,
                        level: true,
                    },
                )
                .unwrap(),
            1 => {
                match i {
                    0 => w.run(0, tick, 0).unwrap(),
                    50 => w.run(0, tick, 2).unwrap(),
                    _ => {}
                }
                w.sample(0, Sample::F32(i as f32)).unwrap();
            }
            _ => w.edge(1, tick).unwrap(),
        }
    }
    // Every item in, flushed at the axis's 1,100, source tick 2,100.
    w.flush(1_100).unwrap();
    out.extend(w.take_output());
    at.push(out.len());
    (w, out, at)
}

/// Each channel's source ticks: edges, and the samples' count, from every block.
fn source_items(file: &[u8]) -> (Vec<i64>, u64) {
    let index = Index::of_file(file).unwrap();
    let (mut edges, mut samples) = (Vec::new(), 0);
    for c in 0..index.blocks.len() {
        for block in index.channel_blocks(file, c).unwrap() {
            match &block.data {
                ch @ BlockChannel::Digital { .. } => edges.extend(ch.source_ticks().unwrap()),
                BlockChannel::Analog { values, .. } => samples += values.len() as u64,
            }
        }
    }
    (edges, samples)
}

#[test]
fn repair_ends_a_digital_channel_at_the_source_tick_of_the_last_block_s_end() {
    let (_, file, at) = written(usize::MAX);
    // Cut after the flush at 700 with half a record after it: the blocks up to axis 700 stay.
    let cut = at[1];
    let crashed = [&file[..cut], &file[cut..][..9.min(file.len() - cut)]].concat();
    let r = repair(&crashed, 3).unwrap();
    assert_eq!(r.keep, cut as u64);
    let fixed = [&crashed[..cut], &r.tail[..]].concat();
    let h = &Footer::parse_file(&fixed).unwrap().header;
    assert_eq!((h.length, h.sources[0].origin), (700, -1_000));
    // D known to source tick 1,700, the axis's 700; its gap from 1,200 to 1,350 kept, as it ends
    // inside.
    let ChannelKind::Digital { end, gaps, .. } = &h.channels[1].data else {
        panic!("D is digital")
    };
    assert_eq!(
        (
            *end,
            gaps.iter().map(|g| (g.from, g.to)).collect::<Vec<_>>()
        ),
        (1_700, vec![(1_200, 1_350)])
    );
    // V's samples to source tick 1,690, the 50 of its first run and 10 of its second; D's edges
    // at 1,005 and 1,500, in its source's ticks.
    assert_eq!(source_items(&fixed), (vec![1_005, 1_500], 60));
    // What finishing there would have written, D's end the source tick at 700.
    let (w, mut finished, _) = written(2);
    finished.extend(w.finish(700, &[1_700], Finish::default()).unwrap().tail);
    assert!(
        fixed == finished,
        "the repaired file differs from the finished one"
    );
    // Cut before the first flush's blocks end their gap: D's end is where the gap starts.
    let cut = at[0];
    let crashed = [&file[..cut], &file[cut..][..9.min(file.len() - cut)]].concat();
    let r = repair(&crashed, 3).unwrap();
    let fixed = [&crashed[..cut], &r.tail[..]].concat();
    let h = &Footer::parse_file(&fixed).unwrap().header;
    let ChannelKind::Digital { end, gaps, .. } = &h.channels[1].data else {
        panic!("D is digital")
    };
    assert_eq!((h.length, *end, gaps.len()), (300, 1_200, 0));
}

#[test]
fn a_recut_keeps_the_source_ticks_the_origin_and_the_content() {
    let (_, file, _) = written(usize::MAX);
    let mut r = Recutter::new(options());
    r.feed(&file).unwrap();
    let mut out = r.output().to_vec();
    let n = r.output().len();
    r.consume(n);
    let f = r.finish().unwrap();
    out.extend_from_slice(&f.tail);
    let h = &Footer::parse_file(&out).unwrap().header;
    assert_eq!(h.sources[0].origin, -1_000);
    assert_eq!(h.overview_shift, 4);
    // Known to the source tick of the last block's end, 1,100 on the axis, every item in.
    let ChannelKind::Digital { end, .. } = &h.channels[1].data else {
        panic!("D is digital")
    };
    assert_eq!((h.length, *end), (1_100, 2_100));
    assert_eq!(source_items(&out), (vec![1_005, 1_500, 2_000], 100));
    // The same content a repair of the unfinished file gives.
    let repaired = repair(&file, 3).unwrap();
    assert_eq!(repaired.header.content_id, h.content_id);
}
