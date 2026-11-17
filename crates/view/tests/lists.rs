//! A channel whose gaps or breaks are in list pages (more than 64 of them): a query, statistics,
//! `edges`, `index_at` and a probe's search read only the pages their span needs, and answer as
//! a recording whose lists are read whole does; `runs` and `known` read the list whole; a sketch
//! reads none of it. Each page a view reads is checked, and the blocks it places are checked
//! against the list entries their ticks need.

use std::collections::BTreeMap;
use std::ops::{ControlFlow, Range};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use escapement_tickwave_format::decode::{DecodeError, Footer, from_memory};
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, IntCoding, SourceFormat,
    SourceHeader,
};
use escapement_tickwave_format::index::{Located, PAGE_HEAD};
use escapement_tickwave_format::model::{Gap, Ratio};
use escapement_tickwave_format::record::{self, Kind};
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};
use escapement_tickwave_view::{Answer, Bytes, Cache, Query, Recording, probe};

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

impl Counted {
    fn new(bytes: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            bytes,
            reads: Mutex::default(),
        })
    }

    fn take(&self) -> Vec<Range<u64>> {
        std::mem::take(&mut *self.reads.lock().unwrap())
    }
}

const A: usize = 0;
const B: usize = 1;
const D: usize = 2;
const P: usize = 3;
/// A and B: a sample every 10 ticks, a run of 20 then a pause of 50 ticks, 2 samples lost at
/// each break but every fifth, which only pauses: 10,000 breaks, three pages.
const RUN: u64 = 20;
const BREAKS: u64 = 10_000;
/// D: from a gap at its start to tick 30, an edge every 250 ticks and a gap from 50 to 100 ticks
/// after it: 9,001 gaps, three pages.
const GAPS: i64 = 9_000;

fn run_tick(k: u64) -> i64 {
    (k * (RUN * 10 + 50)) as i64
}

/// The recording, and its length.
fn listed() -> (Vec<u8>, i64) {
    listed_with(true)
}

/// The recording, without A's and B's samples where `pairs` is false: D alone has data.
fn listed_with(pairs: bool) -> (Vec<u8>, i64) {
    let timebase = Ratio::integer(1_000_000).unwrap();
    let analog = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period: Ratio::integer(10).unwrap(),
            coding: AnalogCoding::Int(IntCoding::new(16, false)),
            significant: None,
            phase: None,
        },
        shaping: Default::default(),
        overview_shift: Some(6),
        codec: None,
        requires: Vec::new(),
    };
    let d = Declaration {
        timebase,
        channels: vec![
            analog("A"),
            analog("B"),
            DeclaredChannel {
                name: "D".into(),
                source_name: "d".into(),
                source: 0,
                data: DeclaredKind::Digital { initial: false },
                shaping: Default::default(),
                overview_shift: Some(6),
                codec: None,
                requires: Vec::new(),
            },
        ],
        derived: vec![Derived {
            name: "P".into(),
            unit: "W".into(),
            op: escapement_tickwave_format::header::Op::Multiply,
            operands: vec!["A".into(), "B".into()],
            params: Default::default(),
            requires: Vec::new(),
            significant: None,
        }],
        sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
        title: None,
        meta: BTreeMap::new(),
        overview_shift: 6,
        requires: Vec::new(),
    };
    let mut w = Writer::new(
        d,
        WriteOptions {
            block_bytes: 4096,
            promise_with_items: true,
            level: 1,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    let mut file = w.take_output();
    // Every item in axis order: (tick, what), what 0 a pair, 1 an edge, 2 a gap from it.
    let mut items: Vec<(i64, u8, u64)> = Vec::new();
    for k in (0..=BREAKS).filter(|_| pairs) {
        for i in 0..RUN {
            items.push((run_tick(k) + 10 * i as i64, 0, k * RUN + i));
        }
    }
    for g in 0..GAPS {
        let t = 200 + 250 * g;
        items.push((t, 1, g as u64));
        items.push((t + 50, 2, g as u64));
    }
    items.sort();
    let mut x = 0x2545_f491_4f6c_dd1du64;
    w.gap(
        D,
        Gap {
            from: 0,
            to: 30,
            level: false,
        },
    )
    .unwrap();
    let mut level = false;
    for (t, what, n) in items {
        match what {
            0 => {
                if n % RUN == 0 {
                    let k = n / RUN;
                    let lost = if k.is_multiple_of(5) { 0 } else { 2 };
                    w.run(A, t, lost).unwrap();
                    w.run(B, t, lost).unwrap();
                }
                w.sample(A, 1000 + n % 97).unwrap();
                w.sample(B, 2000 + (n * 31) % 89).unwrap();
            }
            1 => {
                w.edge(D, t).unwrap();
                level = !level;
            }
            _ => {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let to_level = x.is_multiple_of(3);
                w.gap(
                    D,
                    Gap {
                        from: t,
                        to: t + 50,
                        level: to_level,
                    },
                )
                .unwrap();
                level = to_level;
            }
        }
        if n % 1000 == 0 {
            file.extend(w.take_output());
        }
    }
    let _ = level;
    let length = run_tick(BREAKS) + 10 * RUN as i64 + 100;
    file.extend(&w.finish(length, &[length], Finish::default()).unwrap().tail);
    (file, length)
}

/// Every list page of channel `c`: where it lies, and its place.
fn list_pages(file: &[u8], c: usize) -> Vec<(Located, Range<u64>)> {
    let footer = Footer::parse_file(file).unwrap();
    let table = footer.channels[c].list.as_ref().unwrap();
    let mut out = Vec::new();
    table
        .visit(&mut from_memory(file), |at, _| {
            let span = at.entry.offset..at.entry.offset + u64::from(at.entry.length);
            out.push((at, span));
            Ok::<_, DecodeError>(ControlFlow::Continue(()))
        })
        .unwrap();
    out
}

/// How many of channel `c`'s list pages `reads` touch.
fn read_pages(pages: &[(Located, Range<u64>)], reads: &[Range<u64>]) -> usize {
    pages
        .iter()
        .filter(|(_, p)| reads.iter().any(|r| r.start < p.end && p.start < r.end))
        .count()
}

/// A recording of `file` with every list read whole, as `runs` and `known` read them: the
/// answers the span-limited reads are held to.
fn whole(file: &[u8]) -> Recording {
    let rec = Recording::open(Arc::new(file.to_vec()), Cache::new(64 << 20)).unwrap();
    for c in [A, B] {
        rec.runs(c).unwrap();
    }
    rec.known(D).unwrap();
    rec
}

fn query(window: Range<i64>, columns: u32) -> Query {
    let mut query = Query::new(window.clone(), columns);
    query.statistics = vec![window.clone()];
    query.channels = vec![A, D, P, B];
    query.flags = vec![(A, D)];
    query
}

/// `got` and `want`, made by two recordings of one file, the same but for their revisions.
fn same(got: &Answer, want: &Answer, at: &str) {
    let got = Answer {
        revision: want.revision,
        ..got.clone()
    };
    assert_eq!(got.channels.len(), want.channels.len(), "{at}");
    for (i, (g, w)) in got.channels.iter().zip(&want.channels).enumerate() {
        assert_eq!(g, w, "{at}, channel {i} of the query");
    }
    assert_eq!(got, *want, "{at}");
}

#[test]
fn a_query_reads_only_the_list_pages_its_span_needs_and_answers_as_the_whole_list() {
    let (file, length) = listed();
    let (pa, pd) = (list_pages(&file, A), list_pages(&file, D));
    assert_eq!((pa.len(), pd.len()), (3, 3), "list pages of A and D");
    let oracle = whole(&file);
    let bytes = Counted::new(file.clone());
    let rec = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    let cancel = AtomicBool::new(false);
    // Each page's first entry, on the axis: where a span crosses from one page to the next.
    let a_page = |j: usize| pa[j].0.entry.first;
    let d_page = |j: usize| pd[j].0.entry.first;
    let windows = [
        // Inside A's page 1, a few runs wide, and inside D's page 0.
        a_page(1) + 30_000..a_page(1) + 33_000,
        // Across A's pages 1 and 2, and D's 1 and 2.
        a_page(2) - 700..a_page(2) + 700,
        d_page(2) - 700..d_page(2) + 700,
        // `around` from just past page 2's first break, which needs page 1's last for the
        // sample before it; and to just before it, which needs it for the samples lost there.
        a_page(2) + 1_500..a_page(2) + 2_500,
        a_page(2) - 220..a_page(2) - 120,
        d_page(1) + 1_500..d_page(1) + 2_500,
        // From the start, and to the end.
        0..2_000,
        length - 2_000..length,
    ];
    for window in windows {
        let at = format!("{window:?}");
        bytes.take();
        let q = query(window.clone(), 120);
        let got = rec.query(&q, &cancel).unwrap();
        let reads = bytes.take();
        // `around` is the window and a width either side: a page holds about 30 widths of A's
        // and 4 of D's.
        assert!(read_pages(&pa, &reads) <= 2, "{at}: A's pages");
        assert!(read_pages(&pd, &reads) <= 3, "{at}: D's pages");
        same(&got, &oracle.query(&q, &cancel).unwrap(), &at);
        let s = rec.statistics(window.clone(), &cancel).unwrap();
        assert_eq!(
            s,
            oracle.statistics(window.clone(), &cancel).unwrap(),
            "{at}"
        );
        let (from, to) = (window.start, window.end);
        assert_eq!(
            rec.edges(D, from, to, &cancel).unwrap(),
            oracle.edges(D, from, to, &cancel).unwrap(),
            "{at}"
        );
        for t in [from, (from + to) / 2, to - 1] {
            assert_eq!(
                rec.index_at(A, t).unwrap(),
                oracle.index_at(A, t).unwrap(),
                "{at}"
            );
            let found = |r: &Recording, forward| {
                probe::find_edge(r, D, t - 3000, t + 3000, forward, &cancel).unwrap()
            };
            assert_eq!(found(&rec, true), found(&oracle, true), "{at}, at {t}");
            assert_eq!(found(&rec, false), found(&oracle, false), "{at}, at {t}");
        }
    }
    // Nothing above read a list whole.
    assert!(read_pages(&pa, &bytes.take()) == 0);
    assert_eq!(rec.extent(), oracle.extent());
    assert_eq!(rec.gaps(D), GAPS as usize + 1);
    // `runs` and `known` give the whole list: every page.
    bytes.take();
    assert_eq!(rec.runs(A).unwrap().breaks.len() as u64, BREAKS);
    assert_eq!(read_pages(&pa, &bytes.take()), 3);
    let ChannelPlace::Digital { gaps, .. } = rec.known(D).unwrap() else {
        panic!("D is digital")
    };
    assert_eq!(gaps.len(), GAPS as usize);
    assert_eq!(read_pages(&pd, &bytes.take()), 3);
}

use escapement_tickwave_view::spans::ChannelPlace;

/// The extent reads at most the pages of each list's last run; a sketch reads nothing past the
/// footer, its channels with lists in pages drawn without their gaps and breaks.
#[test]
fn a_first_draw_reads_no_list_page_and_the_extent_only_the_last() {
    let (file, length) = listed();
    let (pa, pb, pd) = (
        list_pages(&file, A),
        list_pages(&file, B),
        list_pages(&file, D),
    );
    let bytes = Counted::new(file.clone());
    let rec = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    bytes.take();
    let cancel = AtomicBool::new(false);
    let q = {
        let mut query = query(0..length, 300);
        query.strip = Some((A, 300));
        query
    };
    let sketch = rec.sketch(&q, &cancel).unwrap();
    assert_eq!(bytes.take(), [], "the sketch reads nothing");
    assert_eq!(
        sketch.extent,
        Some(0..length),
        "from the header: A and B to the length"
    );
    let extent = rec.extent().unwrap();
    let reads = bytes.take();
    assert_eq!(read_pages(&pa, &reads), 1, "A's last page");
    assert_eq!(read_pages(&pb, &reads), 1, "B's last page");
    // D's gap from its start sets where it is known from, on its first page.
    assert_eq!(
        read_pages(&pd, &reads),
        0,
        "no digital channel in the extent"
    );
    assert_eq!(Some(extent), whole(&file).extent());
    // With D alone, the extent is D's known span: from its gap from the start, on its first page.
    let (file, length) = listed_with(false);
    let pd = list_pages(&file, D);
    let bytes = Counted::new(file.clone());
    let rec = Recording::open(bytes.clone(), Cache::new(64 << 20)).unwrap();
    bytes.take();
    assert_eq!(rec.extent(), Some(30..length));
    assert_eq!(read_pages(&pd, &bytes.take()), 1, "D's first page");
}

/// A list page of channel `c` edited by `edit`, its record's check made again.
fn list_edited(file: &[u8], c: usize, j: usize, edit: impl FnOnce(&mut [u8])) -> Vec<u8> {
    let (_, span) = list_pages(file, c)[j].clone();
    let (at, len) = (span.start as usize, (span.end - span.start) as usize);
    let mut payload = file[at + 5..at + len - 8].to_vec();
    edit(&mut payload[PAGE_HEAD..]);
    let mut page = Vec::new();
    record::write(&mut page, Kind::ListPage, &[&payload]);
    let mut out = file.to_vec();
    out[at..at + len].copy_from_slice(&page);
    out
}

/// Blocks that disagree with the list entries their ticks need: a gap of D stretched over the
/// next edge, and a run of A moved later so that a block's last sample lies past its end. Each
/// list page still reads, and a query that decodes the block refuses it from the page its ticks
/// need, as a reader of the whole list does.
#[test]
fn a_block_that_disagrees_with_the_list_pages_its_ticks_need_is_refused() {
    let (file, _) = listed();
    let footer = {
        let mut f = Footer::parse_file(&file).unwrap();
        f.load_lists(from_memory(&file)).unwrap();
        f
    };
    let cancel = AtomicBool::new(false);
    let refused = |edited: Vec<u8>, at: i64, what: &str| {
        let whole = escapement_tickwave_format::decode::Index::of_file(&edited)
            .and_then(|i| (0..3).try_for_each(|c| i.channel_blocks(&edited, c).map(drop)));
        assert!(whole.is_err(), "{what}: the whole list's reader takes it");
        let rec = Recording::open(Counted::new(edited), Cache::new(64 << 20)).unwrap();
        let err = rec
            .query(&query(at - 1000..at + 1000, 120), &cancel)
            .unwrap_err()
            .to_string();
        assert!(err.contains(what), "{err}");
    };
    // Gap 6,000 of D, on page 1, from 50 to 210 ticks after its edge: over the next edge, at 200.
    let g = 6_000;
    let gap = footer.gaps(D).unwrap()[g];
    let edited = list_edited(&file, D, g / 4096, |entries| {
        let to = gap.from + 210;
        entries[(g % 4096) * 17 + 8..][..8].copy_from_slice(&to.to_le_bytes());
    });
    refused(edited, gap.to, "lies inside a gap");
    // The break that starts the run holding the end of one of A's blocks on page 1, 15 ticks
    // later: the block's last sample, 10 ticks before its end, then lies 5 past it.
    let breaks = footer.breaks(A).unwrap().to_vec();
    let refs = footer.block_refs(A, from_memory(&file)).unwrap();
    let (k, end) = refs
        .iter()
        .find_map(|b| {
            let k = breaks.partition_point(|x| x.tick < b.end).checked_sub(1)?;
            ((4096..8192).contains(&k) && b.end - breaks[k].tick >= 20).then_some((k, b.end))
        })
        .expect("a block ending inside a run on page 1");
    let edited = list_edited(&file, A, 1, |entries| {
        let tick = breaks[k].tick + 15;
        entries[(k - 4096) * 28 + 8..][..8].copy_from_slice(&tick.to_le_bytes());
    });
    refused(edited, end, "samples lie outside its ticks");
}

/// A list page whose entries don't follow on from the page before, or reach past where the next
/// starts, is refused by the query that reads it.
#[test]
fn a_list_page_out_of_place_is_refused() {
    let (file, _) = listed();
    let pa = list_pages(&file, A);
    let cancel = AtomicBool::new(false);
    // Page 1's last break moved past page 2's first.
    let edited = list_edited(&file, A, 1, |entries| {
        let last = &mut entries[4095 * 28..][..28];
        let tick = pa[2].0.entry.first + 5;
        last[8..16].copy_from_slice(&tick.to_le_bytes());
    });
    let rec = Recording::open(Arc::new(edited), Cache::new(64 << 20)).unwrap();
    let at = pa[2].0.entry.first;
    let err = rec
        .query(&query(at - 500..at + 500, 120), &cancel)
        .unwrap_err()
        .to_string();
    assert!(err.contains("out of place"), "{err}");
}
