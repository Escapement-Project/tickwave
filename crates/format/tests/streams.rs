//! Each channel a stream of its own (FORMAT.md, "Blocks", "Known-through records", "Writing"):
//! per-channel order and promises, flushes that split open blocks and write a known-through
//! record after their blocks, repair to the latest one, losses at either end, directory pages,
//! and compression spread over the calls or offloaded with the same bytes.

use std::collections::BTreeMap;

use escapement_tickwave_format::compress::compress;
use escapement_tickwave_format::decode::{BlockChannel, Footer, Index, from_memory};
use escapement_tickwave_format::header::{
    AnalogCoding, ChannelKind, Declaration, DeclaredChannel, DeclaredKind, Derived, IntCoding,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::index::{PAGE, PAGE_HEAD, depth_of, pages_at};
use escapement_tickwave_format::model::{Gap, Ratio, Sample};
use escapement_tickwave_format::reader::{Ending, Event, Reader};
use escapement_tickwave_format::record::{self, BlockHead, Kind, Next};
use escapement_tickwave_format::recut::Recutter;
use escapement_tickwave_format::repair::{RepairError, repair};
use escapement_tickwave_format::writer::{Compression, Finish, WriteError, WriteOptions, Writer};

/// verge's channels: V and I, 32-bit codes a pair every 1,500 ticks at 150 MHz, P their product,
/// and DIO0.
fn verge() -> Declaration {
    let timebase = Ratio::integer(150_000_000).unwrap();
    let analog = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period: Ratio::integer(1500).unwrap(),
            coding: AnalogCoding::Int(IntCoding::new(32, false)),
            significant: None,
            phase: None,
        },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    Declaration {
        timebase,
        channels: vec![
            analog("V"),
            analog("I"),
            DeclaredChannel {
                name: "DIO0".into(),
                source_name: "dio0".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Default::default(),
                overview_shift: None,
                codec: None,
                requires: Vec::new(),
            },
        ],
        derived: vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            op: escapement_tickwave_format::header::Op::Multiply,
            operands: vec!["I".into(), "V".into()],
            params: Default::default(),
            requires: Vec::new(),
            significant: None,
        }],
        sources: vec![SourceHeader::new(
            SourceFormat::Other("verge".into()),
            timebase,
        )],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 17,
        requires: Vec::new(),
    }
}

fn options(level: i32) -> WriteOptions {
    WriteOptions {
        level,
        ..WriteOptions::default()
    }
}

/// Every record of `file`: its kind, offset and payload, up to the first that is not whole.
fn records(file: &[u8]) -> Vec<(u8, usize, Vec<u8>)> {
    let mut out = Vec::new();
    let mut at = escapement_tickwave_format::header::PREAMBLE_LEN;
    while let Next::Record { kind, payload, len } = record::next(&file[at..], usize::MAX) {
        out.push((kind, at, payload.to_vec()));
        at += len;
    }
    out
}

/// Pair `i`'s codes: V about 3.3 V in mV, I stepping in nA.
fn pair(i: u64) -> (u64, u64) {
    (3300 + i % 7, 1_000_000 + (i * 7919) % 100_000)
}

#[test]
fn a_reader_fetches_only_the_channels_it_reads() {
    let mut w = Writer::new(verge(), options(3)).unwrap();
    let mut file = w.take_output();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    for i in 0..20_000u64 {
        let (v, a) = pair(i);
        w.sample(0, v).unwrap();
        w.sample(1, a).unwrap();
        if i % 100 == 0 {
            w.edge(2, i as i64 * 1500).unwrap();
        }
        if i % 2000 == 1999 {
            w.flush((i as i64 + 1) * 1500).unwrap();
        }
        file.extend(w.take_output());
    }
    let length = 20_000 * 1500;
    file.extend(w.finish(length, &[length], Finish::default()).unwrap().tail);
    let whole = Index::of_file(&file).unwrap();
    // I's pages and frames, from the whole index; everything else is another channel's.
    let mine: Vec<std::ops::Range<u64>> = whole.footer.channels[1]
        .blocks
        .as_ref()
        .unwrap()
        .levels
        .iter()
        .flatten()
        .map(|e| e.offset..e.offset + u64::from(e.length))
        .chain(
            whole.blocks[1]
                .iter()
                .map(|b| b.row.offset..b.row.offset + u64::from(b.row.length)),
        )
        .collect();
    let footer = Footer::parse_file(&file).unwrap();
    let mut fetched = Vec::new();
    let mut fetch = |offset: u64, len: u32| {
        fetched.push(offset..offset + u64::from(len));
        from_memory(&file)(offset, len)
    };
    let blocks = footer.block_refs(1, &mut fetch).unwrap();
    let mut samples = 0;
    for b in &blocks {
        let frame = fetch(b.row.offset, b.row.length).unwrap();
        samples += footer.decode_block(1, b, &frame).unwrap().data.len();
    }
    assert_eq!(samples, 20_000);
    assert_eq!(blocks.len(), 10);
    for r in &fetched {
        assert!(mine.contains(r), "{r:?} is not one of I's pages or blocks");
    }
    assert_eq!(fetched.len(), 1 + 10, "one page, ten blocks");
}

#[test]
fn each_channel_takes_its_own_order_and_refuses_an_item_before_its_promise() {
    let mut w = Writer::new(verge(), options(3)).unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    // V and I well ahead of DIO0, whose edges then come in its own order.
    for i in 0..100u64 {
        let (v, a) = pair(i);
        w.sample(0, v).unwrap();
        w.sample(1, a).unwrap();
    }
    w.edge(2, 3_000).unwrap();
    w.edge(2, 4_500).unwrap();
    // Every item before 90,000 is in: one before it is refused, one at it goes.
    w.promise(90_000).unwrap();
    assert!(matches!(
        w.edge(2, 89_999),
        Err(WriteError::BeforePromise {
            tick: 89_999,
            promise: 90_000,
            ..
        })
    ));
    w.edge(2, 90_000).unwrap();
    // A channel's own promise: DIO0's to 200,000, past the others'.
    w.promise_channel(2, 200_000).unwrap();
    assert!(matches!(
        w.edge(2, 150_000),
        Err(WriteError::BeforePromise { .. })
    ));
    for i in 100..200u64 {
        let (v, a) = pair(i);
        w.sample(0, v).unwrap();
        w.sample(1, a).unwrap();
    }
    // Promises go forward.
    assert!(matches!(
        w.promise(80_000),
        Err(WriteError::Promise {
            before: 80_000,
            last: 90_000
        })
    ));
    let mut file = w.take_output();
    file.extend(
        w.finish(300_000, &[300_000], Finish::default())
            .unwrap()
            .tail,
    );
    let (edges, samples) = items(&file);
    assert_eq!(edges[2], [3_000, 4_500, 90_000]);
    assert_eq!((samples[0].len(), samples[1].len()), (200, 200));
}

/// A block of 400 bytes waits for a promise past its first item, however full; at the promise
/// its items before it close, and those at or after it stay.
#[test]
fn a_full_block_closes_only_at_a_promise_and_keeps_what_comes_after_it() {
    let mut w = Writer::new(
        verge(),
        WriteOptions {
            block_bytes: 16,
            ..options(3)
        },
    )
    .unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    for i in 0..100u64 {
        let (v, a) = pair(i);
        w.sample(0, v).unwrap();
        w.sample(1, a).unwrap();
    }
    let s = w.snapshot();
    let open = |s: &escapement_tickwave_format::snapshot::Snapshot, c| {
        s.channel(c).unwrap().unwritten.clone()
    };
    let u = open(&s, 0);
    assert_eq!(
        s.channel(0).unwrap().rows.len(),
        0,
        "no block written: no promise"
    );
    assert_eq!(
        (u.len(), u[0].first_index, u[0].head.word),
        (1, 0, 100),
        "every sample in the open block"
    );
    // Every item before pair 60's tick is in.
    w.promise(60 * 1500).unwrap();
    let s = w.snapshot();
    let u = open(&s, 0);
    assert_eq!(u.len(), 2);
    assert_eq!(
        (u[0].head.start, u[0].head.end, u[0].head.word),
        (0, 90_000, 60)
    );
    assert_eq!((u[1].head.start, u[1].first_index), (90_000, 60));
    // I closes with V, at the same tick.
    let i = open(&s, 1);
    assert_eq!((i[0].head.end, i[0].head.word), (90_000, 60));
}

#[test]
fn a_flush_splits_open_blocks_at_its_tick_and_writes_its_known_through_record_after_them() {
    let mut w = Writer::new(verge(), options(3)).unwrap();
    let mut file = w.take_output();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    w.edge(2, 100).unwrap();
    // V and I to pair 99, at 148,500, past the flush at 90,000.
    for i in 0..100u64 {
        let (v, a) = pair(i);
        w.sample(0, v).unwrap();
        w.sample(1, a).unwrap();
    }
    w.flush(90_000).unwrap();
    file.extend(w.take_output());
    let s = w.snapshot();
    for c in 0..2 {
        let u = &s.channel(c).unwrap().unwritten;
        assert_eq!(u.last().unwrap().head.start, 90_000, "channel {c}");
        assert_eq!(u.last().unwrap().first_index, 60, "channel {c}");
    }
    // Flushed again at its tick: nothing new. Before it: refused.
    let at = w.position();
    w.flush(90_000).unwrap();
    assert_eq!(w.position(), at);
    assert!(matches!(w.flush(89_000), Err(WriteError::Flush { .. })));
    // The next flush writes V's and I's items before it; DIO0 has none, and writes no block.
    w.flush(100_000).unwrap();
    file.extend(w.take_output());
    let rs = records(&file);
    let kinds: Vec<u8> = rs.iter().map(|r| r.0).collect();
    let kt = Kind::KnownThrough as u8;
    let known: Vec<i64> = rs
        .iter()
        .filter(|r| r.0 == kt)
        .map(|r| record::parse_known_through(&r.2).unwrap())
        .collect();
    assert_eq!(known, [90_000, 100_000]);
    // Each flush's blocks before its record: V's, I's and DIO0's to 90,000, then nothing new.
    let first_kt = kinds.iter().position(|&k| k == kt).unwrap();
    let blocks_before: Vec<(u32, i64, i64)> = rs[..first_kt]
        .iter()
        .filter(|r| r.0 == Kind::Block as u8)
        .map(|r| {
            let (h, _) = BlockHead::parse(&r.2).unwrap();
            (h.channel, h.start, h.end)
        })
        .collect();
    assert_eq!(
        blocks_before,
        [(0, 0, 90_000), (1, 0, 90_000), (2, 0, 90_000)]
    );
    let last_kt = kinds.iter().rposition(|&k| k == kt).unwrap();
    let between: Vec<(u32, i64, i64)> = rs[first_kt + 1..last_kt]
        .iter()
        .filter(|r| r.0 == Kind::Block as u8)
        .map(|r| {
            let (h, _) = BlockHead::parse(&r.2).unwrap();
            (h.channel, h.start, h.end)
        })
        .collect();
    assert_eq!(between, [(0, 90_000, 100_000), (1, 90_000, 100_000)]);
}

/// FORMAT.md, "Known-through records": every item before the tick is in a block record before
/// it, so a block record after it holding an item before its tick is refused. A gap is no item:
/// one reported after the record may start before its tick.
#[test]
fn a_known_through_record_holds_items_and_not_a_gap_reported_after_it() {
    let mut w = Writer::new(verge(), options(3)).unwrap();
    let mut file = w.take_output();
    w.flush(100).unwrap();
    // DIO0 lost from 50, reported at 150 when the loss ends.
    w.gap(
        2,
        Gap {
            from: 50,
            to: 150,
            level: true,
        },
    )
    .unwrap();
    w.edge(2, 160).unwrap();
    file.extend(w.take_output());
    let f = w.finish(1000, &[1000], Finish::default()).unwrap();
    file.extend(&f.tail);
    let mut r = Reader::default();
    r.feed(&file);
    while r.next_event().unwrap().is_some() {}
    assert_eq!(r.end().unwrap(), Ending::Complete);
    let ChannelKind::Digital { gaps, .. } = &f.header.channels[2].data else {
        panic!()
    };
    assert_eq!(gaps.len(), 1);

    // A block of DIO0 with an edge at 50, after the known-through record at 100.
    let mut bad = file[..records(&file)
        .iter()
        .find(|r| r.0 == Kind::KnownThrough as u8)
        .map(|r| r.1 + record::FRAMING + 8)
        .unwrap()]
        .to_vec();
    let mut section = Vec::new();
    escapement_tickwave_format::codec::put_uleb(&mut section, 1);
    escapement_tickwave_format::codec::put_uleb(&mut section, 50);
    let frame = compress(&section, 3).unwrap();
    let head = BlockHead {
        channel: 2,
        start: 0,
        end: 1000,
        word: 1,
        decoded_length: section.len() as u32,
    };
    record::write(&mut bad, Kind::Block, &[&head.to_bytes(), &frame]);
    let mut r = Reader::default();
    r.feed(&bad);
    let err = loop {
        match r.next_event() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("read through"),
            Err(e) => break e,
        }
    };
    assert!(
        err.to_string()
            .contains("an edge lies before the latest known-through tick"),
        "{err}"
    );
}

/// verge's capture as its recorder writes it: `seconds` of `rate` pairs a second, flushed every
/// `flush` pairs, with a pause from `pause.0` to `pause.1` seconds where nothing comes but the
/// flushes, DIO0 toggling every 250 pairs.
fn capture(rate: u64, seconds: u64, flush: u64, pause: (u64, u64)) -> (Vec<u8>, Vec<u64>) {
    let period = 150_000_000 / rate as i64;
    let mut d = verge();
    for c in &mut d.channels {
        if let DeclaredKind::Analog { period: p, .. } = &mut c.data {
            *p = Ratio::integer(period as u64).unwrap();
        }
    }
    let mut w = Writer::new(d, options(3)).unwrap();
    let mut file = w.take_output();
    // After each record-producing call, where the file ends: the cuts to try.
    let mut ends = vec![file.len() as u64];
    let mut index = 0u64;
    let tick = |slot: u64| slot as i64 * period;
    for slot in 0..rate * seconds {
        let second = slot / rate;
        let paused = (pause.0..pause.1).contains(&second);
        if !paused {
            if index == 0 || slot == pause.1 * rate {
                w.run(0, tick(slot), 0).unwrap();
                w.run(1, tick(slot), 0).unwrap();
            }
            let (v, a) = pair(index);
            w.sample(0, v).unwrap();
            w.sample(1, a).unwrap();
            if index.is_multiple_of(250) {
                w.edge(2, tick(slot)).unwrap();
            }
            index += 1;
        }
        if slot % flush == flush - 1 {
            w.flush(tick(slot + 1)).unwrap();
        }
        let out = w.take_output();
        if !out.is_empty() {
            file.extend(out);
            ends.push(file.len() as u64);
        }
    }
    let length = tick(rate * seconds);
    file.extend(w.finish(length, &[length], Finish::default()).unwrap().tail);
    (file, ends)
}

/// Each channel's items, decoded from every block.
fn items(file: &[u8]) -> (Vec<Vec<i64>>, Vec<Vec<u64>>) {
    let index = Index::of_file(file).unwrap();
    let n = index.blocks.len();
    let (mut edges, mut samples) = (vec![Vec::new(); n], vec![Vec::new(); n]);
    for c in 0..n {
        for block in index.channel_blocks(file, c).unwrap() {
            match block.data {
                BlockChannel::Digital { edges: e, .. } => edges[c].extend(e),
                BlockChannel::Analog { values, .. } => samples[c].extend(values.iter().map(|s| {
                    let Sample::U64(c) = s else { panic!() };
                    c
                })),
            }
        }
    }
    (edges, samples)
}

/// The peer's case: a live capture with a long pause and flushes through it, cut at every record
/// boundary and inside records. Repair and the Recutter recover a prefix of what was written,
/// cut at the latest known-through record, V and I as long as each other.
#[test]
fn a_live_capture_with_a_long_pause_cut_anywhere_recovers_to_a_prefix() {
    // 1,000 pairs a second for 12 s, flushed every 100 pairs, paused from 3 s to 9 s.
    let (file, ends) = capture(1000, 12, 100, (3, 9));
    let (all_edges, all_samples) = items(&file);
    let mut cuts: Vec<u64> = ends.clone();
    // Inside records: a byte into each, and halfway.
    for r in records(&file) {
        cuts.push(r.1 as u64 + 1);
        cuts.push((r.1 + r.2.len() / 2) as u64);
    }
    cuts.sort();
    cuts.dedup();
    let footer_at = Footer::parse_file(&file).unwrap().trailer.footer_offset;
    let mut repaired_n = 0;
    let mut longest = 0;
    for &cut in &cuts {
        if cut >= file.len() as u64 || cut > footer_at + 100 {
            continue;
        }
        let bytes = &file[..cut as usize];
        let r = match repair(bytes, 3) {
            Ok(r) => r,
            Err(RepairError::NoBlock | RepairError::NoDeclaration) => continue,
            Err(e) => panic!("cut at {cut}: {e}"),
        };
        let fixed = [&bytes[..r.keep as usize], &r.tail].concat();
        let (edges, samples) = items(&fixed);
        assert_eq!(samples[0].len(), samples[1].len(), "cut at {cut}");
        for c in 0..2 {
            assert!(all_samples[c].starts_with(&samples[c]), "cut at {cut}");
        }
        assert!(all_edges[2].starts_with(&edges[2]), "cut at {cut}");
        // Cut just after the latest known-through record the cut left whole: the length is its
        // tick, every channel known to it.
        let h = &r.header;
        let kt = records(bytes)
            .iter()
            .rev()
            .find(|x| x.0 == Kind::KnownThrough as u8)
            .map(|x| {
                (
                    x.1 + record::FRAMING + 8,
                    record::parse_known_through(&x.2).unwrap(),
                )
            });
        if let Some((end, tick)) = kt {
            assert_eq!((r.keep as usize, h.length), (end, tick), "cut at {cut}");
        }
        // The Recutter finishes it the same.
        let mut rc = Recutter::new(options(3));
        rc.feed(bytes).unwrap();
        let f = rc.finish().unwrap();
        assert_eq!(f.header.content_id, h.content_id, "cut at {cut}");
        longest = longest.max(samples[0].len());
        repaired_n += 1;
    }
    assert!(repaired_n > 100, "{repaired_n} cuts repaired");
    assert_eq!(
        longest,
        all_samples[0].len(),
        "a cut in the footer keeps everything"
    );
}

/// Without a known-through record each channel keeps its blocks, but a derived channel's operands
/// keep only the samples both hold: the cut moves back to the last record at which they hold as
/// many, so a cut between V's block and I's repairs, and reads as its footer says.
#[test]
fn a_cut_between_a_derived_channels_operands_repairs_without_known_through_records() {
    let mut w = Writer::new(
        verge(),
        WriteOptions {
            block_bytes: 400,
            promise_with_items: true,
            ..options(3)
        },
    )
    .unwrap();
    let mut file = w.take_output();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    for i in 0..1000u64 {
        let (v, a) = pair(i);
        w.sample(0, v).unwrap();
        w.sample(1, a).unwrap();
        if i % 37 == 0 {
            w.edge(2, i as i64 * 1500).unwrap();
        }
        file.extend(w.take_output());
    }
    let length = 1000 * 1500;
    file.extend(w.finish(length, &[length], Finish::default()).unwrap().tail);
    let (_, all) = items(&file);
    // Every V block record whose I block follows it: cut between them.
    let rs = records(&file);
    let mut tried = 0;
    for (k, r) in rs.iter().enumerate() {
        if r.0 != Kind::Block as u8 || BlockHead::parse(&r.2).unwrap().0.channel != 0 {
            continue;
        }
        let next_block = rs[k + 1..]
            .iter()
            .find(|x| x.0 == Kind::Block as u8)
            .map(|x| BlockHead::parse(&x.2).unwrap().0.channel);
        if next_block != Some(1) {
            continue;
        }
        let cut = rs[k + 1].1;
        let bytes = &file[..cut];
        let i_before = rs[..k]
            .iter()
            .any(|x| x.0 == Kind::Block as u8 && BlockHead::parse(&x.2).unwrap().0.channel == 1);
        let repaired = match repair(bytes, 3) {
            Ok(r) => r,
            // Before I's first block nothing pairs with V's.
            Err(RepairError::NoBlock) if !i_before => continue,
            Err(e) => panic!("cut at {cut}: {e}"),
        };
        let fixed = [&bytes[..repaired.keep as usize], &repaired.tail].concat();
        // The repaired file reads, in order and by its footer.
        let mut reader = Reader::default();
        reader.feed(&fixed);
        while reader.next_event().unwrap().is_some() {}
        assert_eq!(reader.end().unwrap(), Ending::Complete, "cut at {cut}");
        let (_, samples) = items(&fixed);
        assert_eq!(samples[0].len(), samples[1].len(), "cut at {cut}");
        assert!(all[0].starts_with(&samples[0]) && all[1].starts_with(&samples[1]));
        // Cut back to I's last block, the V block that I lacks gone with what followed it.
        assert!(repaired.keep as usize <= r.1, "cut at {cut}");
        tried += 1;
    }
    assert!(tried >= 3, "{tried} cuts");
}

/// FORMAT.md, "Gaps" and F5: samples lost before the first sit on the first run's grid before
/// it, those after the last on the last run's after it; a loss below tick 0, or a length that
/// doesn't cover one, is refused; both enter the content id, and only where stated.
#[test]
fn losses_at_either_end_sit_on_the_runs_grid_and_enter_the_content_id() {
    let write = |first: i64, before: u64, after: u64, length: i64| {
        let mut w = Writer::new(verge(), options(3)).unwrap();
        let mut file = w.take_output();
        w.run(0, first, before)?;
        w.run(1, first, before)?;
        for i in 0..10u64 {
            let (v, a) = pair(i);
            w.sample(0, v)?;
            w.sample(1, a)?;
        }
        let lost: BTreeMap<usize, u64> = [(0, after), (1, after)].into();
        let f = w.finish(
            length,
            &[length],
            Finish {
                lost_after: lost,
                ..Finish::default()
            },
        )?;
        file.extend(f.tail);
        Ok::<_, WriteError>((file, f.header))
    };
    // The first sample at 3,000: two lost before it start at 0; three would start at -1,500.
    let (file, h) = write(3000, 2, 0, 20_000).unwrap();
    let ChannelKind::Analog { lost_before, .. } = h.channels[0].data else {
        panic!()
    };
    assert_eq!(lost_before, 2);
    assert!(matches!(
        write(3000, 3, 0, 20_000),
        Err(WriteError::LostBefore { lost: 3, .. })
    ));
    // The last sample at 3,000 + 9 x 1,500 = 16,500: four lost after it end at 16,500 + 5 x
    // 1,500 = 24,000, which a length of 24,000 covers and one of 23,999 doesn't.
    let (_, h4) = write(3000, 0, 4, 24_000).unwrap();
    let ChannelKind::Analog { lost_after, .. } = h4.channels[1].data else {
        panic!()
    };
    assert_eq!(lost_after, 4);
    assert!(matches!(
        write(3000, 0, 4, 23_999),
        Err(WriteError::LostAfter { lost: 4, .. })
    ));
    // Each loss changes the id; none leaves it as it was.
    let (_, none) = write(3000, 0, 0, 24_000).unwrap();
    let (_, before) = write(3000, 2, 0, 24_000).unwrap();
    let (_, after) = write(3000, 0, 2, 24_000).unwrap();
    let (_, both) = write(3000, 2, 2, 24_000).unwrap();
    let ids = [
        none.content_id,
        before.content_id,
        after.content_id,
        both.content_id,
    ];
    for (i, a) in ids.iter().enumerate() {
        for b in &ids[i + 1..] {
            assert_ne!(a, b);
        }
    }
    // A derived channel's operands share their losses.
    let mut w = Writer::new(verge(), options(3)).unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    w.sample(0, 1u64).unwrap();
    w.sample(1, 1u64).unwrap();
    let err = w
        .finish(
            10_000,
            &[10_000],
            Finish {
                lost_after: [(0, 1)].into(),
                ..Finish::default()
            },
        )
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("differ in first, phase, count, breaks or losses"),
        "{err}"
    );
    // The file reads in order, its first run's loss on the run record.
    let mut r = Reader::default();
    r.feed(&file);
    let mut first_run = None;
    while let Some(e) = r.next_event().unwrap() {
        if let Event::Run { channel: 0, run } = e {
            first_run.get_or_insert(run);
        }
    }
    assert_eq!(first_run.unwrap().lost, 2);
}

/// FORMAT.md, "Index": past 16 pages of 4,096 a table's pages are listed by directory pages, and
/// the top index lists those, so it holds at most 16 entries a table at any length.
#[test]
fn a_channel_past_the_directory_threshold_gets_directory_pages() {
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
        meta: BTreeMap::new(),
        overview_shift: 40,
        requires: Vec::new(),
    };
    // An edge every 10 ticks, a block each: 70,000 blocks, 18 pages, one directory.
    const N: i64 = 70_000;
    let mut w = Writer::new(
        d,
        WriteOptions {
            block_bytes: 1,
            promise_with_items: true,
            level: 1,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let mut file = w.take_output();
    for k in 0..N {
        w.edge(0, 10 * k).unwrap();
        if k % 1000 == 0 {
            file.extend(w.take_output());
        }
    }
    file.extend(w.take_output());
    let f = w.finish(10 * N, &[10 * N], Finish::default()).unwrap();
    file.extend(&f.tail);
    assert_eq!(f.header.channels[0].blocks, N as u64);
    let footer = Footer::parse_file(&file).unwrap();
    let table = footer.channels[0].blocks.as_ref().unwrap();
    assert_eq!(table.levels.len(), 2);
    assert_eq!(table.levels[1].len(), 1, "one directory in the top index");
    assert_eq!((depth_of(N as u64), pages_at(N as u64, 0)), (1, 18));
    let index = Index::of_file(&file).unwrap();
    assert_eq!(index.blocks[0].len(), N as usize);
    assert!(
        index.blocks[0]
            .iter()
            .enumerate()
            .all(|(i, b)| b.start == 10 * i as i64 && b.first_index == i as u64)
    );
    // The directory's 18 entries, one a page: its second says where page 1 starts. Say another
    // tick: the page refuses the directory.
    let dir = table.levels[1][0];
    let at = dir.offset as usize;
    let mut payload = file[at + 5..at + dir.length as usize - 8].to_vec();
    let entry1 = PAGE_HEAD + 28;
    let first = i64::from_le_bytes(payload[entry1..entry1 + 8].try_into().unwrap());
    assert_eq!(first, 10 * PAGE as i64);
    payload[entry1..entry1 + 8].copy_from_slice(&(first + 1).to_le_bytes());
    let mut fixed = Vec::new();
    record::write(&mut fixed, Kind::IndexPage, &[&payload]);
    let mut bad = file.clone();
    bad[at..at + fixed.len()].copy_from_slice(&fixed);
    let Err(err) = Index::of_file(&bad) else {
        panic!("the page refuses its directory")
    };
    assert!(
        err.to_string()
            .contains("it starts where its directory says it doesn't"),
        "{err}"
    );
    // A header whose count, 16 pages of blocks, needs no directory, where the top index lists one:
    // refused before any page is read.
    let mut h = footer.header.clone();
    h.channels[0].blocks = 16 * PAGE as u64;
    let t = file.len() - 24;
    let fat = u64::from_le_bytes(file[t..t + 8].try_into().unwrap()) as usize;
    let flen = u32::from_le_bytes(file[t + 8..t + 12].try_into().unwrap()) as usize;
    let payload = &file[fat + 5..fat + flen - 8];
    let hlen = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
    let json = serde_json::to_vec(&h).unwrap();
    let mut p = (json.len() as u32).to_le_bytes().to_vec();
    p.extend_from_slice(&payload[4..8]);
    p.extend_from_slice(&json);
    p.extend_from_slice(&payload[8 + hlen..]);
    let mut reheadered = file[..fat].to_vec();
    record::write_footer(&mut reheadered, 0, &p);
    let Err(err) = Footer::parse_file(&reheadered) else {
        panic!("the top index refuses its depth")
    };
    assert!(err.to_string().contains("depth 1 and 1 entries"), "{err}");
}

/// Writes verge's layout at 100,000 pairs a second for `seconds`, flushed every second, through
/// `options`.
fn verge_live(options: WriteOptions, seconds: u64) -> Vec<u8> {
    let mut w = Writer::new(verge(), options).unwrap();
    let mut file = w.take_output();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    for i in 0..100_000 * seconds {
        let (v, a) = pair(i);
        w.sample(0, v).unwrap();
        w.sample(1, a).unwrap();
        if i % 997 == 0 {
            w.edge(2, i as i64 * 1500).unwrap();
        }
        if i % 100_000 == 99_999 {
            w.flush((i as i64 + 1) * 1500).unwrap();
        }
        if i % 20_000 == 0 {
            file.extend(w.take_output());
        }
    }
    let length = 100_000 * seconds as i64 * 1500;
    file.extend(w.take_output());
    file.extend(w.finish(length, &[length], Finish::default()).unwrap().tail);
    file
}

/// FORMAT.md, "tickwave's writer", Compression: no call compresses a whole block. A second of
/// verge's samples is a 400,000-byte block a channel, 48 slices of 8 KiB and one of 6,784 bytes.
/// Each call from the flush that closes them on compresses one slice, so V's block leaves on the
/// 49th call counting the flush; I's slices start on a call of their own, the 50th, after V's
/// short last one, so I's block leaves on the 98th, and the known-through record with it.
#[test]
fn each_call_compresses_one_slice_and_the_records_leave_in_file_order() {
    let mut d = verge();
    d.derived.clear();
    d.channels.truncate(2);
    let mut w = Writer::new(d, options(16)).unwrap();
    drop(w.take_output());
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    for i in 0..100_000u64 {
        let (v, a) = pair(i);
        w.sample(0, v).unwrap();
        w.sample(1, a).unwrap();
    }
    assert!(
        w.take_output().len() < 100,
        "nothing compressed before the flush"
    );
    w.flush(100_000 * 1500).unwrap();
    let mut out = Vec::new();
    let mut left = Vec::new();
    for call in 1..=120 {
        if call > 1 {
            out.extend(w.take_output());
        }
        let mut at = 0;
        while let Next::Record { kind, payload, len } = record::next(&out[at..], usize::MAX) {
            if kind == Kind::Block as u8 {
                left.push((call, BlockHead::parse(payload).unwrap().0.channel));
            } else if kind == Kind::KnownThrough as u8 {
                left.push((call, 99));
            }
            at += len;
        }
        out.drain(..at);
    }
    assert_eq!(left, [(49, 0), (98, 1), (98, 99)]);
}

/// The offload compresses on other threads and gives the bytes the spread gives.
#[test]
fn the_offload_gives_the_bytes_the_spread_gives() {
    let spread = verge_live(options(16), 3);
    let offloaded = verge_live(
        WriteOptions {
            compression: Compression::Offload,
            ..options(16)
        },
        3,
    );
    assert!(spread == offloaded, "the offload's bytes differ");
    Index::of_file(&spread).unwrap();
}

/// A derived channel's operands close their blocks together, at the latest promise to both, so
/// their blocks tile alike even where one's samples are twice the other's size or one is promised
/// ahead of the other.
#[test]
fn a_derived_channels_operands_close_their_blocks_together() {
    let mut d = verge();
    let DeclaredKind::Analog { coding, .. } = &mut d.channels[0].data else {
        panic!()
    };
    *coding = AnalogCoding::Int(IntCoding::new(16, false));
    let mut w = Writer::new(
        d,
        WriteOptions {
            block_bytes: 400,
            ..options(3)
        },
    )
    .unwrap();
    let mut file = w.take_output();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    for i in 0..1000u64 {
        let (v, a) = pair(i);
        w.sample(0, v).unwrap();
        w.sample(1, a).unwrap();
        if i % 10 == 9 {
            w.promise((i as i64 + 1) * 1500).unwrap();
        }
        file.extend(w.take_output());
    }
    // I's samples to pair 1,999 and its promise with them; V's stay at pair 999.
    for i in 1000..2000u64 {
        w.sample(1, pair(i).1).unwrap();
        w.promise_channel(1, (i as i64 + 1) * 1500).unwrap();
        file.extend(w.take_output());
    }
    let s = w.snapshot();
    let open_i = s.channel(1).unwrap().unwritten.last().unwrap().head.start;
    assert!(
        open_i <= 1000 * 1500,
        "I closed past V's promise, at {open_i}"
    );
    for i in 1000..2000u64 {
        w.sample(0, pair(i).0).unwrap();
        file.extend(w.take_output());
    }
    let length = 2000 * 1500;
    file.extend(w.finish(length, &[length], Finish::default()).unwrap().tail);
    let index = Index::of_file(&file).unwrap();
    let tiles = |c: usize| -> Vec<(i64, u64)> {
        index.blocks[c]
            .iter()
            .map(|b| (b.start, b.first_index))
            .collect()
    };
    assert!(tiles(1).len() > 10, "{} blocks", tiles(1).len());
    assert_eq!(tiles(0), tiles(1));
}

/// A live view hands each snapshot the blocks and overview rows it took from the last one: the
/// pages that held them don't change, so only the pages after them are read again, and the
/// refs are those read from scratch.
#[test]
fn a_snapshot_reads_only_the_pages_after_those_a_view_holds() {
    let timebase = Ratio::integer(1_000_000_000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![DeclaredChannel {
            name: "D".into(),
            source_name: "d".into(),
            source: 0,
            data: DeclaredKind::Digital { initial: false },
            shaping: Default::default(),
            overview_shift: Some(0),
            codec: None,
            requires: Vec::new(),
        }],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 0,
        requires: Vec::new(),
    };
    // An edge a block, 2,000 to 4,000 ticks apart, so each overview block of 4,096 one-tick
    // buckets holds a change at its own place and none join a run: past 4,096 of either, a page.
    let mut w = Writer::new(
        d,
        WriteOptions {
            block_bytes: 1,
            promise_with_items: true,
            level: 1,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let mut file = w.take_output();
    let (mut tick, mut x) = (0i64, 0x2545_f491_4f6c_dd1du64);
    let mut snapshots = Vec::new();
    for k in 0..3 * PAGE as u64 * 2 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        tick += 2000 + (x % 2000) as i64;
        w.edge(0, tick).unwrap();
        file.extend(w.take_output());
        if k == 6000 || k == 5 * PAGE as u64 + 10 {
            snapshots.push(w.snapshot());
        }
    }
    let [s1, s2] = &snapshots[..] else { panic!() };
    let fetched = std::cell::RefCell::new(Vec::new());
    let fetch = |offset: u64, len: u32| {
        fetched.borrow_mut().push(offset);
        from_memory(&file)(offset, len)
    };
    let blocks = s1.block_refs(0, &[], fetch).unwrap();
    let level = s1.overview(0).unwrap();
    assert!(level.levels[0].in_blocks);
    let rows = s1.level_refs(0, 0, &[], fetch).unwrap();
    assert!(
        blocks.len() > PAGE && rows.len() > PAGE,
        "{} {}",
        blocks.len(),
        rows.len()
    );
    fetched.borrow_mut().clear();
    let again = s2.block_refs(0, &blocks, fetch).unwrap();
    let block_pages = std::mem::take(&mut *fetched.borrow_mut());
    let again_rows = s2.level_refs(0, 0, &rows, fetch).unwrap();
    let row_pages = std::mem::take(&mut *fetched.borrow_mut());
    assert_eq!(again, s2.block_refs(0, &[], fetch).unwrap());
    assert_eq!(again_rows, s2.level_refs(0, 0, &[], fetch).unwrap());
    // s2 holds pages 0 to 4 of blocks; page 0 came with s1's refs.
    let block_table = &s2.channel(0).unwrap().blocks;
    assert_eq!(block_table.rows, 5 * PAGE as u64);
    assert_eq!(block_pages.len(), 4, "{block_pages:?}");
    let row_table = s2.overview(0).unwrap().levels[0].table.as_ref().unwrap();
    let paged = row_table.rows as usize / PAGE;
    assert_eq!(row_pages.len(), paged - rows.len() / PAGE, "{row_pages:?}");
}

/// A digital channel declared high whose one block comes at the finish, as a converter closes a
/// sparse channel's: a file cut before it repairs, with the channel never known and its declared
/// level kept. Repair, the Recutter on the cut file and a recut of the repaired file agree.
#[test]
fn a_cut_before_a_high_channels_only_block_repairs_with_it_never_known() {
    let mut d = verge();
    d.derived.clear();
    d.channels[2].data = DeclaredKind::Digital { initial: true };
    let mut w = Writer::new(
        d,
        WriteOptions {
            block_bytes: 400,
            promise_with_items: true,
            ..options(3)
        },
    )
    .unwrap();
    let mut file = w.take_output();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    for i in 0..2000u64 {
        let (v, a) = pair(i);
        w.sample(0, v).unwrap();
        w.sample(1, a).unwrap();
        if i % 500 == 250 {
            w.edge(2, i as i64 * 1500).unwrap();
        }
        file.extend(w.take_output());
    }
    let length = 2000 * 1500;
    let whole = w.finish(length, &[length], Finish::default()).unwrap();
    // Cut after V's and I's blocks so far, where DIO0 has none.
    let cut = &file[..file.len() * 9 / 10];
    let blocks_of = |c: u32| {
        records(cut)
            .iter()
            .filter(|r| r.0 == Kind::Block as u8 && BlockHead::parse(&r.2).unwrap().0.channel == c)
            .count()
    };
    assert!(blocks_of(0) > 5 && blocks_of(2) == 0);
    let repaired = repair(cut, 3).unwrap();
    let fixed = [&cut[..repaired.keep as usize], &repaired.tail].concat();
    let h = Footer::parse_file(&fixed).unwrap().header;
    let ChannelKind::Digital {
        initial,
        end,
        edges,
        ..
    } = h.channels[2].data
    else {
        panic!()
    };
    assert_eq!((initial, Some(end), edges), (true, h.start(2), 0));
    let mut r = Reader::default();
    r.feed(&fixed);
    while r.next_event().unwrap().is_some() {}
    assert_eq!(r.end().unwrap(), Ending::Complete);
    let recut = |bytes: &[u8]| {
        let mut rc = Recutter::new(options(3));
        rc.feed(bytes).unwrap();
        rc.finish().unwrap().header.content_id
    };
    assert_eq!(recut(cut), repaired.header.content_id);
    assert_eq!(recut(&fixed), repaired.header.content_id);
    assert_ne!(repaired.header.content_id, whole.header.content_id);
}

/// A full block waits on its partners' promises as well as its own: V's, 32-bit and full, waits
/// on I, 16-bit and not full, and I's own promise closes both at its tick.
#[test]
fn a_partners_promise_closes_a_full_block_that_waited_on_it() {
    let mut d = verge();
    let DeclaredKind::Analog { coding, .. } = &mut d.channels[1].data else {
        panic!()
    };
    *coding = AnalogCoding::Int(IntCoding::new(16, false));
    let mut w = Writer::new(
        d,
        WriteOptions {
            block_bytes: 400,
            ..options(3)
        },
    )
    .unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    for i in 0..150u64 {
        let (v, a) = pair(i);
        w.sample(0, v).unwrap();
        w.sample(1, a % 65536).unwrap();
    }
    let at = 150 * 1500;
    // V, 600 bytes, is promised; I, 300, is not: V waits.
    w.promise_channel(0, at).unwrap();
    let open = |w: &Writer, c| w.snapshot().channel(c).unwrap().unwritten.clone();
    assert_eq!(open(&w, 0).len(), 1, "V waits on I's promise");
    w.promise_channel(1, at).unwrap();
    for c in 0..2 {
        let u = open(&w, c);
        assert_eq!(u.len(), 2, "channel {c}");
        assert_eq!((u[0].head.end, u[0].head.word), (at, 150), "channel {c}");
    }
}

/// Only a gap list's last page may leave out entries, the gaps past the end. A block table's last
/// page that leaves out its last row is refused, read in order and repaired, though its own check
/// and head hold.
#[test]
fn a_block_tables_short_last_page_is_refused() {
    let mut w = Writer::new(
        verge(),
        WriteOptions {
            block_bytes: 1,
            promise_with_items: true,
            ..options(3)
        },
    )
    .unwrap();
    let mut file = w.take_output();
    for k in 0..10i64 {
        w.edge(2, 1000 * k).unwrap();
        file.extend(w.take_output());
    }
    file.extend(w.finish(20_000, &[20_000], Finish::default()).unwrap().tail);
    let (at, mut payload) = records(&file)
        .into_iter()
        .find(|r| r.0 == Kind::IndexPage as u8 && r.2[..4] == 2u32.to_le_bytes() && r.2[4] == 0)
        .map(|r| (r.1, r.2))
        .expect("DIO0's page of blocks");
    let len = record::FRAMING + payload.len();
    let count = u32::from_le_bytes(payload[15..19].try_into().unwrap());
    assert_eq!(count, 10, "a block an edge, the last closed at the finish");
    payload[15..19].copy_from_slice(&(count - 1).to_le_bytes());
    payload.truncate(payload.len() - 28);
    let mut short = Vec::new();
    record::write(&mut short, Kind::IndexPage, &[&payload]);
    let spliced = [&file[..at], &short[..], &file[at + len..]].concat();
    let mut r = Reader::default();
    r.feed(&spliced);
    let err = loop {
        match r.next_event() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("read through"),
            Err(e) => break e,
        }
    };
    assert!(
        err.to_string().contains(
            "a page of channel 2's blocks differs from the one the records before it give"
        ),
        "{err}"
    );
    let footer_at = records(&spliced)
        .iter()
        .find(|r| r.0 == Kind::Footer as u8)
        .unwrap()
        .1;
    // Repair reads in order, so its reader refuses the page before repair's own tables meet it.
    let Err(err) = repair(&spliced[..footer_at], 3) else {
        panic!("repaired")
    };
    assert!(
        err.to_string().contains(
            "a page of channel 2's blocks differs from the one the records before it give"
        ),
        "{err}"
    );
}

/// An encoder's file has no known-through record, so its finish's last overview blocks follow its
/// last blocks in the file. Cut anywhere from there to its footer, it repairs to the file it was,
/// where its digital channels are known to the length, as repair takes them to be.
#[test]
fn a_converted_file_cut_in_its_finish_repairs_to_the_file_it_was() {
    use escapement_tickwave_format::encode::{Options, encode};
    use escapement_tickwave_format::model::{
        Analog, Channel, ChannelData, Digital, Recording, Samples, Source,
    };
    const LENGTH: i64 = 1 << 22;
    let recording = Recording {
        timebase: Ratio::ONE,
        length: LENGTH,
        channels: vec![
            Channel {
                name: "D".into(),
                source_name: "d".into(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: true,
                    end: LENGTH,
                    count: 1000,
                    edges: Box::new((0..1000).map(|k| k * 4000 + 3)),
                    gaps: Vec::new(),
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
                    period: Ratio::integer(1000).unwrap(),
                    count: 4000,
                    breaks: Vec::new(),
                    lost_before: 0,
                    lost_after: 0,
                    significant: None,
                    samples: Samples::F32(Box::new((0..4000).map(|i| (i % 17) as f32))),
                    phase: None,
                }),
                shaping: Default::default(),
            },
        ],
        derived: Vec::new(),
        sources: vec![Source::new(SourceFormat::Other("t".into()), Ratio::ONE)],
    };
    let options = Options {
        level: 3,
        overview_shift: Some(4),
        ..Options::default()
    };
    let file = encode(recording, &options).unwrap();
    let footer = Footer::parse_file(&file).unwrap();
    assert!(
        footer
            .header
            .channels
            .iter()
            .all(|c| !c.overview_blocks.is_empty())
    );
    let rs = records(&file);
    let last_block = rs.iter().rposition(|r| r.0 == Kind::Block as u8).unwrap();
    assert!(
        rs[last_block + 1..]
            .iter()
            .any(|r| r.0 == Kind::OverviewBlock as u8),
        "the finish's overview blocks follow the last block"
    );
    let cuts = rs[last_block + 1..].iter().map(|r| r.1);
    for cut in cuts.chain([footer.trailer.footer_offset as usize]) {
        let r = repair(&file[..cut], 3).unwrap_or_else(|e| panic!("cut at {cut}: {e}"));
        let fixed = [&file[..r.keep as usize], &r.tail].concat();
        assert!(fixed == file, "cut at {cut}: the repaired file differs");
    }
}

/// A writer never closes a channel's block between two of its items at one axis tick: a pulse
/// stamped with one time stays in one block, however small the blocks.
#[test]
fn a_channels_items_at_one_tick_share_a_block() {
    let mut w = Writer::new(
        verge(),
        WriteOptions {
            block_bytes: 1,
            promise_with_items: true,
            ..options(3)
        },
    )
    .unwrap();
    let mut file = w.take_output();
    for t in [5i64, 5, 9, 12, 12, 12, 20] {
        w.edge(2, t).unwrap();
    }
    file.extend(w.take_output());
    file.extend(w.finish(30, &[30], Finish::default()).unwrap().tail);
    let index = Index::of_file(&file).unwrap();
    let blocks: Vec<Vec<i64>> = index
        .channel_blocks(&file, 2)
        .unwrap()
        .into_iter()
        .map(|b| match b.data {
            BlockChannel::Digital { edges, .. } => edges,
            BlockChannel::Analog { .. } => unreachable!(),
        })
        .collect();
    assert_eq!(blocks, [vec![5, 5], vec![9], vec![12, 12, 12], vec![20]]);
}

/// A derived channel's operand sample is paired with the other operand's sample of its index:
/// one at another tick is refused as it comes, and one that never comes at the finish.
#[test]
fn an_operand_sample_its_partner_never_pairs_is_refused() {
    let mut w = Writer::new(verge(), options(3)).unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 1, 0).unwrap();
    w.sample(0, 1u64).unwrap();
    let err = w.sample(1, 1u64).unwrap_err();
    assert_eq!(
        err.to_string(),
        "derived channel P: I's sample 0 starts or sits on another run than V's"
    );
    let mut w = Writer::new(verge(), options(3)).unwrap();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    w.sample(0, 1u64).unwrap();
    w.sample(0, 2u64).unwrap();
    w.sample(1, 1u64).unwrap();
    let Err(err) = w.finish(10_000, &[10_000], Finish::default()) else {
        panic!("finished")
    };
    assert_eq!(
        err.to_string(),
        "derived channel P: V has sample 1 that I lacks"
    );
}

/// Verify takes a file's overview blocks in any order across channels and levels: a live
/// writer's, flushed each 1,000 pairs, the encoder's of the same content, and the encoder's with
/// its overview records moved coarsest first.
#[test]
fn verify_takes_the_overview_blocks_of_a_span_in_any_order() {
    use escapement_tickwave_format::encode::{Options, encode};
    use escapement_tickwave_format::model::{
        Analog, Channel, ChannelData, Digital, Recording, Samples, Source,
    };
    use escapement_tickwave_format::verify::verify;
    const PAIRS: u64 = 20_000;
    let length = PAIRS as i64 * 1500;
    let edges = || (0..PAIRS / 100).map(|k| k as i64 * 150_000 + 7);
    let model = || {
        let int = IntCoding::new(32, false);
        let analog = |name: &str, f: fn((u64, u64)) -> u64| Channel {
            name: name.into(),
            source_name: name.to_lowercase(),
            source: 0,
            data: ChannelData::Analog(Analog {
                unit: "V".into(),
                first: 0,
                period: Ratio::integer(1500).unwrap(),
                count: PAIRS,
                breaks: Vec::new(),
                lost_before: 0,
                lost_after: 0,
                significant: None,
                samples: Samples::U64(int, Box::new((0..PAIRS).map(move |i| f(pair(i))))),
                phase: None,
            }),
            shaping: Default::default(),
        };
        let timebase = Ratio::integer(150_000_000).unwrap();
        Recording {
            timebase,
            length,
            channels: vec![
                analog("V", |p| p.0),
                analog("I", |p| p.1),
                Channel {
                    name: "DIO0".into(),
                    source_name: "dio0".into(),
                    source: 0,
                    data: ChannelData::Digital(Digital {
                        initial: false,
                        end: length,
                        count: PAIRS / 100,
                        edges: Box::new(edges()),
                        gaps: Vec::new(),
                    }),
                    shaping: Default::default(),
                },
            ],
            derived: verge().derived,
            sources: vec![Source::new(SourceFormat::Other("verge".into()), timebase)],
        }
    };
    let mut d = verge();
    d.overview_shift = 8;
    let mut w = Writer::new(d, options(3)).unwrap();
    let mut live = w.take_output();
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    let mut edge = edges().peekable();
    for i in 0..PAIRS {
        let t = i as i64 * 1500;
        if i > 0 && i % 1000 == 0 {
            w.flush(t).unwrap();
        }
        while let Some(e) = edge.next_if(|&e| e < t + 1500) {
            w.edge(2, e).unwrap();
        }
        let (v, a) = pair(i);
        w.sample(0, v).unwrap();
        w.sample(1, a).unwrap();
        live.extend(w.take_output());
    }
    live.extend(w.finish(length, &[length], Finish::default()).unwrap().tail);
    let encoded = encode(
        model(),
        &Options {
            level: 3,
            overview_shift: Some(8),
            ..Options::default()
        },
    )
    .unwrap();
    let order = |file: &[u8]| -> Vec<(u32, u8, u64)> {
        records(file)
            .iter()
            .filter(|r| r.0 == Kind::OverviewBlock as u8)
            .map(|r| {
                let (h, _) = record::OverviewHead::parse(&r.2).unwrap();
                (h.channel, h.shift, h.k)
            })
            .collect()
    };
    let (a, b) = (order(&live), order(&encoded));
    assert!(
        a.len() > 50 && a != b,
        "{} and {} overview records",
        a.len(),
        b.len()
    );
    let mut sorted = (a.clone(), b.clone());
    sorted.0.sort();
    sorted.1.sort();
    assert_eq!(sorted.0, sorted.1, "the same blocks");
    verify(&live[..], model()).unwrap();
    verify(&encoded[..], model()).unwrap();
    // The encoder's file with each run of overview records next to each other put coarsest
    // level first, each level's records still in their order, and the level pages' rows moved
    // with them: an order no writer here gives, which verify takes too.
    let reordered = reordered(&encoded);
    assert!(order(&reordered) != b);
    verify(&reordered[..], model()).unwrap();
    // Repair takes that order too, comparing each channel's levels on their own.
    let footer = Footer::parse_file(&reordered).unwrap();
    let at = footer.trailer.footer_offset as usize;
    let fixed = escapement_tickwave_format::repair::repair(&reordered[..at], 3).unwrap();
    assert_eq!(fixed.header.content_id, footer.header.content_id);
}

/// `file` with each run of adjacent overview block records stably sorted coarsest level first,
/// and each index page of overview rows rewritten to where its rows' frames now lie.
fn reordered(file: &[u8]) -> Vec<u8> {
    use escapement_tickwave_format::header::OverviewRow;
    use std::collections::HashMap;
    let rs = records(file);
    let mut out = file.to_vec();
    // Old frame offset to new.
    let mut moved: HashMap<u64, u64> = HashMap::new();
    let head_len = record::PAYLOAD_AT + record::OverviewHead::LEN;
    let mut i = 0;
    while i < rs.len() {
        if rs[i].0 != Kind::OverviewBlock as u8 {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < rs.len() && rs[j].0 == Kind::OverviewBlock as u8 {
            j += 1;
        }
        let run = &rs[i..j];
        let mut sorted: Vec<&(u8, usize, Vec<u8>)> = run.iter().collect();
        sorted
            .sort_by_key(|r| std::cmp::Reverse(record::OverviewHead::parse(&r.2).unwrap().0.shift));
        let mut at = run[0].1;
        for r in sorted {
            let len = record::FRAMING + r.2.len();
            out[at..at + len].copy_from_slice(&file[r.1..r.1 + len]);
            moved.insert((r.1 + head_len) as u64, (at + head_len) as u64);
            at += len;
        }
        i = j;
    }
    for r in &rs {
        // An index page of an overview level's rows.
        if r.0 != Kind::IndexPage as u8 || r.2[4] != 1 || r.2[6] != 0 {
            continue;
        }
        let mut payload = r.2.clone();
        for row in payload[PAGE_HEAD..]
            .as_chunks_mut::<{ OverviewRow::SIZE }>()
            .0
        {
            let mut parsed = OverviewRow::parse(row);
            parsed.offset = moved[&parsed.offset];
            let mut bytes = Vec::new();
            parsed.write(&mut bytes);
            row.copy_from_slice(&bytes);
        }
        let mut page = Vec::new();
        record::write(&mut page, Kind::IndexPage, &[&payload]);
        out[r.1..r.1 + page.len()].copy_from_slice(&page);
    }
    out
}
