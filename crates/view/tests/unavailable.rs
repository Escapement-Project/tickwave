//! A view opens a recording whose channel this version can't read (FORMAT.md, "Unavailable
//! channels"), with that channel in its place and unavailable: one of a kind it doesn't know, one
//! whose row states more than a block may decode, and one with more gaps inline than a header
//! holds. Nothing of the channel's index is read, and the other channels read as ever.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::header::{
    AnalogCoding, BlockRow, Declaration, DeclaredChannel, DeclaredKind, Derived, Exact, IntCoding,
    SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::record::{self, Kind};
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
use escapement_tickwave_view::{Bytes, Cache, ChannelKind, Drawn, Query, Recording};

/// A file in memory that records each read.
struct Counted {
    bytes: Vec<u8>,
    reads: Mutex<Vec<Range<u64>>>,
}

impl Bytes for Counted {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.bytes.len() as u64)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.reads
            .lock()
            .unwrap()
            .push(offset..offset + buf.len() as u64);
        let at = offset as usize;
        buf.copy_from_slice(&self.bytes[at..at + buf.len()]);
        Ok(())
    }
}

fn channel(name: &str, data: DeclaredKind) -> DeclaredChannel {
    DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data,
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    }
}

/// V and I analog, D digital with an edge, P = I × V.
fn three_channels() -> Vec<u8> {
    let analog = || DeclaredKind::Analog {
        unit: "V".into(),
        period: Ratio::ONE,
        coding: AnalogCoding::Int(IntCoding {
            scale: Exact::new(1, 1000),
            ..IntCoding::new(16, false)
        }),
        significant: None,
        phase: None,
    };
    let timebase = Ratio::integer(1_000_000).unwrap();
    let d = Declaration {
        timebase,
        channels: vec![
            channel("V", analog()),
            channel("I", analog()),
            channel("D", DeclaredKind::Digital { initial: false }),
        ],
        derived: vec![Derived::multiply("P", "W", "I", "V")],
        sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 4,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    for c in 0..2 {
        w.run(c, 0, 0).unwrap();
    }
    for i in 0..4u64 {
        w.sample(0, i).unwrap();
        w.sample(1, i + 1).unwrap();
    }
    w.edge(2, 2).unwrap();
    let mut file = w.take_output();
    file.extend(w.finish(8, &[8], Finish::default()).unwrap().tail);
    file
}

/// `file` with its footer's header's JSON edited, the top index and the strips kept.
fn with_header(file: &[u8], edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let t = file.len() - 24;
    let at = u64::from_le_bytes(file[t..t + 8].try_into().unwrap()) as usize;
    let len = u32::from_le_bytes(file[t + 8..t + 12].try_into().unwrap()) as usize;
    let payload = &file[at + 5..at + len - 8];
    let header_len = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
    let mut header: serde_json::Value =
        serde_json::from_slice(&payload[8..8 + header_len]).unwrap();
    edit(&mut header);
    let json = serde_json::to_vec(&header).unwrap();
    let mut new = (json.len() as u32).to_le_bytes().to_vec();
    new.extend_from_slice(&payload[4..8]);
    new.extend_from_slice(&json);
    new.extend_from_slice(&payload[8 + header_len..]);
    let mut out = file[..at].to_vec();
    record::write_footer(&mut out, 0, &new);
    out
}

/// `file` with channel `c`'s first block row stating that its block decodes to 8 MiB.
fn big_first_block(file: &[u8], c: usize) -> Vec<u8> {
    let footer = Footer::parse_file(file).unwrap();
    let entry = footer.channels[c].blocks.as_ref().unwrap().levels[0][0];
    let (at, len) = (entry.offset as usize, entry.length as usize);
    let payload = &file[at + 5..at + len - 8];
    let head = escapement_tickwave_format::index::PAGE_HEAD;
    let mut edited = payload[..head].to_vec();
    for (k, r) in payload[head..]
        .as_chunks::<{ BlockRow::SIZE }>()
        .0
        .iter()
        .enumerate()
    {
        let mut row = BlockRow::parse(r);
        if k == 0 {
            row.decoded_length = 8 << 20;
        }
        row.write(&mut edited);
    }
    let mut page = Vec::new();
    record::write(&mut page, Kind::IndexPage, &[&edited]);
    let mut out = file.to_vec();
    out[at..at + len].copy_from_slice(&page);
    out
}

/// The byte ranges of overview channel `o`'s index pages: its blocks' and its levels'.
fn index_pages(file: &[u8], o: usize) -> Vec<u64> {
    let footer = Footer::parse_file(file).unwrap();
    let ch = &footer.channels[o];
    ch.blocks
        .iter()
        .chain(ch.levels.iter().map(|(_, t)| t))
        .chain(ch.list.iter())
        .flat_map(|t| t.levels[0].iter().map(|e| e.offset))
        .collect()
}

/// `file` opened: which channels are unavailable, and whether any of `pages` was read.
fn opened(file: Vec<u8>, pages: &[u64]) -> (Recording, Vec<Option<String>>, bool) {
    let bytes = Arc::new(Counted {
        bytes: file,
        reads: Mutex::default(),
    });
    let rec = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    let mut q = Query::new(0..8, 8);
    q.channels = (0..rec.channels().len()).collect();
    rec.query(&q, &AtomicBool::new(false)).unwrap();
    let why = rec
        .channels()
        .iter()
        .map(|c| c.unavailable.clone())
        .collect();
    let read = bytes
        .reads
        .lock()
        .unwrap()
        .iter()
        .any(|r| pages.contains(&r.start));
    (rec, why, read)
}

#[test]
fn a_view_opens_a_kind_it_doesnt_know_as_unavailable() {
    let file = three_channels();
    let pages = index_pages(&file, 2);
    assert!(!pages.is_empty());
    let edited = with_header(&file, |h| {
        let d = h["channels"][2].as_object_mut().unwrap();
        d.insert("kind".into(), "events".into());
        d.insert("initial".into(), "high".into());
        d.insert("frames".into(), serde_json::json!([1, 2]));
    });
    let (rec, why, read) = opened(edited, &pages);
    assert!(!read, "an index page of the unavailable channel was read");
    assert_eq!(rec.channels()[2].kind, ChannelKind::Unavailable);
    let why: Vec<bool> = why.iter().map(Option::is_some).collect();
    assert_eq!(why, [false, false, true, false]);
    assert!(
        rec.channels()[2]
            .unavailable
            .as_deref()
            .is_some_and(|w| w.contains("of kind events")),
        "{:?}",
        rec.channels()[2]
    );
    let mut q = Query::new(0..8, 8);
    q.channels = vec![1];
    let drawn = rec.query(&q, &AtomicBool::new(false)).unwrap();
    assert!(
        matches!(&drawn.channels[0], Drawn::Analog { samples: Some(s), .. } if s.len() == 4),
        "{:?}",
        drawn.channels[0]
    );
}

#[test]
fn a_view_opens_a_channel_past_this_versions_limits_as_unavailable() {
    let file = three_channels();
    // V's row says its block decodes to 8 MiB; V requires a feature that allows it.
    let pages = index_pages(&file, 0);
    let big = with_header(&big_first_block(&file, 0), |h| {
        h["channels"][0]["requires"] = serde_json::json!(["big-blocks"]);
    });
    let (rec, why, read) = opened(big, &pages);
    assert!(!read, "an index page of the unavailable channel was read");
    let why: Vec<bool> = why.iter().map(Option::is_some).collect();
    assert_eq!(why, [true, false, false, true]);
    assert_eq!(rec.edge_count(2, 0, 8, &AtomicBool::new(false)).unwrap(), 1);
    // D states 65 gaps inline, one past what a header holds; D requires a feature that allows it.
    let gaps: Vec<(i64, i64, bool)> = (0..65).map(|k| (k * 2 - 200, k * 2 - 199, false)).collect();
    let pages = index_pages(&file, 2);
    let listed = with_header(&file, |h| {
        h["channels"][2]["gaps"] = serde_json::json!(gaps);
        h["channels"][2]["requires"] = serde_json::json!(["long-lists"]);
    });
    let (_, why, read) = opened(listed, &pages);
    assert!(!read, "an index page of the unavailable channel was read");
    let why: Vec<bool> = why.iter().map(Option::is_some).collect();
    assert_eq!(why, [false, false, true, false]);
}
