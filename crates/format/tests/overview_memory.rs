//! A long span's overview settled in steps: what that holds at once, counted by an allocator
//! that tracks the bytes live, and the order its blocks come in.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::PREAMBLE_LEN;
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Ratio, Recording, Samples, Source, SourceFormat,
};
use escapement_tickwave_format::record::{self, Kind};
use escapement_tickwave_format::repair::repair;
use escapement_tickwave_format::verify::verify;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    PEAK.fetch_max(LIVE.fetch_add(by, Relaxed) + by, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        LIVE.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, layout, size) };
        if !q.is_null() {
            if size > layout.size() {
                grew(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// Taken by every test here: the count is the whole process's, so they run one at a time.
static ONE: Mutex<()> = Mutex::new(());

fn one() -> std::sync::MutexGuard<'static, ()> {
    ONE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The most bytes `f` holds at once beyond what was live before it.
fn peak<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let out = f();
    (out, PEAK.load(Relaxed).saturating_sub(before))
}

/// Overview blocks of the finest level, each holding an edge, so each differs from the next.
const BLOCKS: i64 = 2000;

/// A digital channel with an edge in every overview block of a finest level of one-tick buckets,
/// beside an analog channel with no sample: each block holds 4,096 of its buckets, about 76 KB.
fn recording() -> Recording {
    let length = BLOCKS * 4096;
    Recording {
        timebase: Ratio::ONE,
        length,
        channels: vec![
            Channel {
                name: "D".into(),
                source_name: "d".into(),
                source: 0,
                data: ChannelData::Digital(Digital {
                    initial: false,
                    end: length,
                    count: BLOCKS as u64,
                    edges: Box::new((0..BLOCKS).map(|b| b * 4096 + 7)),
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
                    period: Ratio::ONE,
                    count: 0,
                    breaks: Vec::new(),
                    significant: None,
                    samples: Samples::F32(Box::new(std::iter::empty())),
                }),
                shaping: Default::default(),
            },
        ],
        derived: Vec::new(),
        sources: vec![Source::new(SourceFormat::Other("t".into()), Ratio::ONE)],
    }
}

/// One block holds every edge, so the overview settles its whole span at once: the writer, repair
/// and verify each take its 2,000 overview blocks, 150 MB of buckets, a few at a time.
#[test]
fn a_settle_of_overview_blocks_that_differ_holds_a_few_at_a_time() {
    let _one = one();
    const MOST: usize = 32 << 20;
    let (file, held) = peak(file);
    let footer = Footer::parse_file(&file).unwrap();
    assert_eq!(footer.rows.len(), 1);
    assert!(footer.header.overview_blocks[0].blocks >= BLOCKS as u32);
    assert!(held < MOST, "the writer held {held} bytes");

    let cut = &file[..footer.trailer.footer_offset as usize];
    let (repaired, held) = peak(|| repair(cut, 1).unwrap());
    assert_eq!(
        repaired.header.overview_blocks,
        footer.header.overview_blocks
    );
    assert!(held < MOST, "repair held {held} bytes");

    let (verified, held) = peak(|| verify(&file[..], recording()));
    verified.unwrap();
    assert!(held < MOST, "verify held {held} bytes");
}

/// The recording, its finest level at one tick a bucket, in one block.
fn file() -> Vec<u8> {
    let options = Options {
        level: 1,
        block_bytes: 64 << 20,
        overview_shift: Some(0),
        ..Options::default()
    };
    encode(recording(), &options).unwrap()
}

/// A record of `file` before its footer, and the overview block it is, if one.
type Record = (Option<(u8, u64)>, Vec<u8>);

fn records(file: &[u8]) -> Vec<Record> {
    let end = Footer::parse_file(file).unwrap().trailer.footer_offset as usize;
    let mut at = PREAMBLE_LEN;
    let mut records = Vec::new();
    while at < end {
        let record::Next::Record { kind, payload, len } = record::next(&file[at..], usize::MAX)
        else {
            panic!("a record at {at}")
        };
        let place = (kind == Kind::OverviewBlock as u8).then(|| {
            record::parse_overview(payload)
                .map(|(shift, k, _)| (shift, k))
                .unwrap()
        });
        records.push((place, file[at..at + len].to_vec()));
        at += len;
    }
    records
}

/// `file` with `records` in place of its own, and its footer's overview rows where they now lie.
fn rewritten(file: &[u8], records: &[Record]) -> Vec<u8> {
    let footer = Footer::parse_file(file).unwrap();
    let mut out = file[..PREAMBLE_LEN].to_vec();
    let mut rows = footer.overview_rows.clone();
    for (place, bytes) in records {
        if let Some((shift, k)) = place {
            let level = footer
                .header
                .overview_blocks
                .iter()
                .position(|l| l.shift == *shift)
                .unwrap();
            let row = &mut rows[level][*k as usize];
            row.offset = (out.len() + record::PAYLOAD_AT + record::OVERVIEW_HEAD_LEN) as u64;
            let record::Next::Record { payload, .. } = record::next(bytes, usize::MAX) else {
                panic!("an overview record")
            };
            row.length = (payload.len() - record::OVERVIEW_HEAD_LEN) as u32;
        }
        out.extend_from_slice(bytes);
    }
    let rows: Vec<_> = rows.into_iter().flatten().collect();
    let bytes = record::footer_bytes(
        &footer.header,
        &footer.rows,
        &footer.words,
        &rows,
        &footer.overview_frame,
    )
    .unwrap();
    record::write_footer(&mut out, 0, &bytes);
    out
}

/// The writer settles a long span in steps, so its levels' overview blocks interleave as the steps
/// fall; one that settled it at once wrote each level's in turn, as earlier tickwave did. Verify
/// takes either.
#[test]
fn verify_takes_the_overview_blocks_of_a_span_in_any_order() {
    let _one = one();
    let file = file();
    let records = records(&file);
    let first = records.iter().position(|(p, _)| p.is_some()).unwrap();
    let mut reordered = records.clone();
    reordered[first..].sort_by_key(|(p, _)| p.unwrap_or((u8::MAX, u64::MAX)));
    assert!(
        reordered != records,
        "the writer wrote each level's blocks in turn"
    );
    verify(&file[..], recording()).unwrap();
    verify(&rewritten(&file, &reordered)[..], recording()).unwrap();
}

/// Verify and repair check a frame that repeats the one before once: a frame repeated where the
/// content settles another block is refused. The edges here leave the finest level's blocks two
/// kinds, low then high, and high then low, so block 1 differs from block 0.
#[test]
fn verify_and_repair_refuse_an_overview_frame_repeated_where_the_content_differs() {
    let _one = one();
    let file = file();
    let mut records = records(&file);
    let at = |place| records.iter().position(|(p, _)| *p == Some(place)).unwrap();
    let (a, b) = (at((0, 0)), at((0, 1)));
    assert_eq!(b, a + 1);
    let record::Next::Record { payload, .. } = record::next(&records[a].1, usize::MAX) else {
        panic!("an overview record")
    };
    let frame = payload[record::OVERVIEW_HEAD_LEN..].to_vec();
    let mut repeated = Vec::new();
    record::write(
        &mut repeated,
        Kind::OverviewBlock,
        &[&record::overview_head(0, 1), &frame],
    );
    records[b].1 = repeated;
    let repeated = rewritten(&file, &records);
    let err = verify(&repeated[..], recording()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "overview level 2^0, block 1: differs from the content"
    );
    let cut = Footer::parse_file(&repeated).unwrap().trailer.footer_offset as usize;
    assert!(matches!(
        repair(&repeated[..cut], 1),
        Err(escapement_tickwave_format::repair::RepairError::Overview)
    ));
}
