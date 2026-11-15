//! What a writer holds at once, counted by an allocator that tracks the bytes live: its open
//! blocks, its tables' open pages and the frames waiting to be compressed, none of which grows
//! with the recording's length (FORMAT.md, "tickwave's writer", Memory); and what the writer,
//! repair and verify hold while a long span's overview settles at once.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use escapement_tickwave_format::decode::Footer;
use escapement_tickwave_format::encode::{Options, encode};
use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, IntCoding, SourceFormat, SourceHeader,
};
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Ratio, Recording, Samples, Source,
};
use escapement_tickwave_format::repair::repair;
use escapement_tickwave_format::verify::verify;
use escapement_tickwave_format::writer::{Finish, WriteOptions, Writer};

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

/// verge's layout, small: V and I, 32-bit codes every 1,500 ticks, a pin, flushed every
/// `per` pairs, so each flush closes a block of each.
fn declaration() -> Declaration {
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
        },
        shaping: Default::default(),
        overview_shift: None,
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
            },
        ],
        derived: Vec::new(),
        sources: vec![SourceHeader::new(
            SourceFormat::Other("verge".into()),
            timebase,
        )],
        title: None,
        meta: Default::default(),
        overview_shift: 17,
    }
}

/// The most bytes the writer holds at once while it writes `flushes` flushes of 20 pairs and an
/// edge each, its output taken and dropped as it goes.
fn held(flushes: i64) -> usize {
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let mut w = Writer::new(
        declaration(),
        WriteOptions {
            level: 3,
            ..WriteOptions::default()
        },
    )
    .unwrap();
    drop(w.take_output());
    w.run(0, 0, 0).unwrap();
    w.run(1, 0, 0).unwrap();
    let mut pair = 0u64;
    for f in 0..flushes {
        for _ in 0..20 {
            w.sample(0, pair % 4096).unwrap();
            w.sample(1, (pair * 7) % 65536).unwrap();
            pair += 1;
        }
        w.edge(2, (pair as i64 - 1) * 1500).unwrap();
        w.flush(pair as i64 * 1500).unwrap();
        if f % 16 == 0 {
            drop(w.take_output());
        }
    }
    let peak = PEAK.load(Relaxed).saturating_sub(before);
    let length = pair as i64 * 1500;
    drop(w.finish(length, &[length], Finish::default()).unwrap());
    peak
}

/// Past 4,096 flushes each table's rows go in pages and leave memory; the peak at four times as
/// many flushes stays about where it was. The gaps and breaks a recording loses are held to the
/// end, for the content id: here there are none.
#[test]
fn what_the_writer_holds_does_not_grow_with_the_recording() {
    let _one = one();
    let short = held(5_000);
    let long = held(20_000);
    eprintln!("held: {short} bytes at 5,000 flushes, {long} at 20,000");
    assert!(long < short + short / 4, "{short} then {long}");
    assert!(long < 4 << 20, "{long}");
}

/// The most bytes `f` holds at once beyond what was live before it.
fn peak<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let out = f();
    (out, PEAK.load(Relaxed).saturating_sub(before))
}

/// Overview blocks of the finest level, each holding a sample, so each differs from the next.
const BLOCKS: i64 = 2000;

/// An analog channel with a sample in every overview block of a finest level of one-tick
/// buckets, all in one block: each overview block holds 4,096 of its buckets, 64 KB.
fn sparse() -> Recording {
    let length = BLOCKS * 4096;
    Recording {
        timebase: Ratio::ONE,
        length,
        channels: vec![Channel {
            name: "V".into(),
            source_name: "v".into(),
            source: 0,
            data: ChannelData::Analog(Analog {
                unit: "V".into(),
                first: 7,
                period: Ratio::integer(4096).unwrap(),
                count: BLOCKS as u64,
                breaks: Vec::new(),
                lost_before: 0,
                lost_after: 0,
                significant: None,
                samples: Samples::F32(Box::new((0..BLOCKS).map(|b| b as f32))),
            }),
            shaping: Default::default(),
        }],
        derived: Vec::new(),
        sources: vec![Source::new(SourceFormat::Other("t".into()), Ratio::ONE)],
    }
}

/// One block holds every sample, so the overview settles its whole span at once: the writer,
/// repair and verify each take its 2,000 overview blocks, 128 MB of buckets, a few at a time.
#[test]
fn a_settle_of_overview_blocks_that_differ_holds_a_few_at_a_time() {
    let _one = one();
    const MOST: usize = 32 << 20;
    let options = Options {
        level: 1,
        overview_shift: Some(0),
        ..Options::default()
    };
    let (file, held) = peak(|| encode(sparse(), &options).unwrap());
    let footer = Footer::parse_file(&file).unwrap();
    let h = &footer.header.channels[0];
    assert_eq!(h.blocks, 1);
    assert!(h.overview_blocks[0].blocks >= BLOCKS as u64);
    eprintln!("the writer held {held} bytes");
    assert!(held < MOST, "the writer held {held} bytes");

    let cut = &file[..footer.trailer.footer_offset as usize];
    let (repaired, held) = peak(|| repair(cut, 1).unwrap());
    assert_eq!(repaired.header.content_id, footer.header.content_id);
    eprintln!("repair held {held} bytes");
    assert!(held < MOST, "repair held {held} bytes");

    let (verified, held) = peak(|| verify(&file[..], sparse()));
    verified.unwrap();
    eprintln!("verify held {held} bytes");
    assert!(held < MOST, "verify held {held} bytes");
}
