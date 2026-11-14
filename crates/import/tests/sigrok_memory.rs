//! What a sigrok session file's logic stream costs to read, counted by an allocator that tracks
//! the bytes live.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Cursor, Write};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use escapement_tickwave_format::model::ChannelData;
use escapement_tickwave_import::sigrok;
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

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

/// One logic sample of 64 MiB of zeros, deflated to about 64 KB, and one probe named among its
/// 2^29 bits: the scan keeps state for that probe and one span of the rest, and the probe's
/// reader a chunk of the stream, so reading it holds a few MB. Sized by the unitsize, the scan
/// took about 4.4 GB, and each named probe's reader 64 MiB.
#[test]
fn a_wide_deflated_sample_is_read_in_little_more_than_its_compressed_bytes() {
    const UNIT: usize = 64 << 20;
    let meta = format!(
        "[global]\nsigrok version=0.5.2\n\n[device 1]\ncapturefile=logic-1\ntotal probes=8\n\
         samplerate=1 MHz\nprobe1=A\nunitsize={UNIT}\n"
    );
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, bytes) in [
        ("version", b"2".to_vec()),
        ("metadata", meta.into_bytes()),
        ("logic-1", vec![0; UNIT]),
    ] {
        zip.start_file(name, deflated).unwrap();
        zip.write_all(&bytes).unwrap();
    }
    let zip = zip.finish().unwrap().into_inner();
    assert!(zip.len() < 1 << 20, "{} bytes", zip.len());

    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let import = sigrok::read_bytes(zip, "wide.sr").unwrap();
    let mut channels = import.recording.channels;
    assert_eq!(channels.len(), 1);
    let ChannelData::Digital(d) = channels.remove(0).data else {
        panic!("A is digital")
    };
    assert_eq!(d.edges.count(), 0);
    let held = PEAK.load(Relaxed) - before;
    assert!(held < 16 << 20, "reading it held {held} bytes");
}
