//! What repair and a re-cut hold back while they can't tell whether a record stands, counted by an
//! allocator that tracks the bytes live: a crafted file whose records would make it hold far more
//! than the file is refused once `repair::HOLD` is passed (requirement 13: no allocation the file
//! alone makes unbounded).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use escapement_tickwave_format::header::{
    AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, Derived, IntCoding, SourceFormat,
    SourceHeader,
};
use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::record::MAX_BLOCK;
use escapement_tickwave_format::record::{self, BlockHead, Kind};
use escapement_tickwave_format::recut::{RecutError, Recutter};
use escapement_tickwave_format::repair::{HOLD, RepairError, repair};
use escapement_tickwave_format::writer::{WriteOptions, Writer};

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

/// V and I, 32-bit codes a tick apart, and P their product: V's blocks then each decode to
/// 64 MiB from some 32 KB of zeros, and with no block of I's they never pair.
fn crafted(blocks: i64) -> Vec<u8> {
    let timebase = Ratio::integer(1_000_000_000).unwrap();
    let analog = |name: &str| DeclaredChannel {
        name: name.into(),
        source_name: name.to_lowercase(),
        source: 0,
        data: DeclaredKind::Analog {
            unit: "V".into(),
            period: Ratio::integer(1).unwrap(),
            coding: AnalogCoding::Int(IntCoding::new(32, false)),
            significant: None,
            phase: None,
        },
        shaping: Default::default(),
        overview_shift: None,
        codec: None,
        requires: Vec::new(),
    };
    let d = Declaration {
        timebase,
        channels: vec![analog("V"), analog("I")],
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
            SourceFormat::Other("crafted".into()),
            timebase,
        )],
        title: None,
        meta: Default::default(),
        overview_shift: 30,
        requires: Vec::new(),
    };
    let mut w = Writer::new(d, WriteOptions::default()).unwrap();
    // The declaration, and V's first run record, which its first sample brings.
    w.run(0, 0, 0).unwrap();
    w.sample(0, 0u64).unwrap();
    let mut file = w.take_output();
    let n = (MAX_BLOCK / 4) as i64;
    let frame = escapement_tickwave_format::compress::compress(&vec![0; MAX_BLOCK], 1).unwrap();
    for k in 0..blocks {
        let head = BlockHead {
            channel: 0,
            start: k * n,
            end: (k + 1) * n,
            word: n as u32,
            decoded_length: MAX_BLOCK as u32,
        };
        record::write(&mut file, Kind::Block, &[&head.to_bytes(), &frame]);
    }
    file
}

/// The most bytes live at once while `f` runs, from where they stood when it began.
fn peak<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let out = f();
    (out, PEAK.load(Relaxed).saturating_sub(before))
}

#[test]
fn records_held_back_past_the_hold_are_refused_not_held() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    // 128 blocks of the most a block holds, 512 MiB of codes and 1 GiB as u64 values, in a file
    // of a few hundred KB.
    let file = crafted(128);
    assert!(file.len() < 512 << 10, "{} bytes", file.len());
    // At most what the hold allows, one block decoding past it, and its raw bytes.
    let bound = HOLD + 3 * MAX_BLOCK;
    let (repaired, held) = peak(|| repair(&file, 3));
    eprintln!("repair held {held} bytes at most, the bound {bound}");
    assert!(
        held < bound,
        "repair held {held} bytes at most, past {bound}"
    );
    let Err(err) = repaired else {
        panic!("repaired")
    };
    assert!(matches!(err, RepairError::HeldBack(n) if n > HOLD), "{err}");
    assert!(
        err.to_string().contains("more than repair holds back"),
        "{err}"
    );
    let (recut, held) = peak(|| {
        let mut r = Recutter::new(WriteOptions::default());
        r.feed(&file)
    });
    let Err(err) = recut else { panic!("re-cut") };
    assert!(matches!(err, RecutError::HeldBack(n) if n > HOLD), "{err}");
    assert!(
        held < bound,
        "the re-cut held {held} bytes at most, past {bound}"
    );
}
