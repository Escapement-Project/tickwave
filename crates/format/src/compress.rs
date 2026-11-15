//! Compression as tickwave's writers do it: one zstd frame with a content checksum and its size,
//! compressed 8 KiB of decoded bytes at a time, each slice ending a zstd block. So a frame's
//! bytes depend on its data, its level and the slice, never on how the calls that compress it
//! fall, and a frame compressed a slice a call on the writer's thread is the one a thread of its
//! own makes.

use std::cell::RefCell;
use std::io;
#[cfg(feature = "encode")]
use std::sync::{Arc, Condvar, Mutex};

use zstd::zstd_safe::{self, CCtx, CParameter, InBuffer, OutBuffer, ResetDirective};

/// The decoded bytes one slice compresses: what a writer's call does at most.
pub const SLICE: usize = 8 << 10;

fn zstd_error(code: usize) -> io::Error {
    io::Error::other(zstd_safe::get_error_name(code))
}

/// A zstd context at one level, which compresses frames a slice at a time.
pub struct Slicer {
    cctx: CCtx<'static>,
    level: i32,
}

impl Slicer {
    pub fn new(level: i32) -> io::Result<Self> {
        let mut cctx = CCtx::try_create().ok_or_else(|| io::Error::other("no zstd context"))?;
        cctx.set_parameter(CParameter::CompressionLevel(level))
            .map_err(zstd_error)?;
        cctx.set_parameter(CParameter::ChecksumFlag(true))
            .map_err(zstd_error)?;
        Ok(Self { cctx, level })
    }

    pub fn level(&self) -> i32 {
        self.level
    }

    /// Starts a frame of `len` decoded bytes.
    pub fn begin(&mut self, len: usize) -> io::Result<()> {
        self.cctx
            .reset(ResetDirective::SessionOnly)
            .map_err(zstd_error)?;
        self.cctx
            .set_pledged_src_size(Some(len as u64))
            .map_err(zstd_error)?;
        Ok(())
    }

    /// The frame's next slice, the whole of `input`, its output after `out`'s bytes. `last`
    /// ends the frame; any other slice ends a zstd block.
    pub fn slice(&mut self, input: &[u8], last: bool, out: &mut Vec<u8>) -> io::Result<()> {
        use zstd_safe::zstd_sys::ZSTD_EndDirective;
        let directive = if last {
            ZSTD_EndDirective::ZSTD_e_end
        } else {
            ZSTD_EndDirective::ZSTD_e_flush
        };
        let mut input = InBuffer::around(input);
        loop {
            out.reserve(zstd_safe::compress_bound(input.src.len() - input.pos()) + 32);
            let at = out.len();
            let mut output = OutBuffer::around_pos(out, at);
            let left = self
                .cctx
                .compress_stream2(&mut output, &mut input, directive)
                .map_err(zstd_error)?;
            if left == 0 && input.pos() == input.src.len() {
                return Ok(());
            }
        }
    }

    /// A whole frame of `raw`, slice after slice.
    pub fn frame(&mut self, raw: &[u8]) -> io::Result<Vec<u8>> {
        self.begin(raw.len())?;
        let mut out = Vec::with_capacity(zstd_safe::compress_bound(raw.len()) + 32);
        let mut at = 0;
        loop {
            let end = (at + SLICE).min(raw.len());
            self.slice(&raw[at..end], end == raw.len(), &mut out)?;
            at = end;
            if at == raw.len() {
                return Ok(out);
            }
        }
    }
}

thread_local! {
    static SLICERS: RefCell<Vec<Slicer>> = const { RefCell::new(Vec::new()) };
}

/// `raw` as one frame at `level`, the bytes a writer gives it, on this thread's context for the
/// level.
pub fn compress(raw: &[u8], level: i32) -> io::Result<Vec<u8>> {
    SLICERS.with(|s| {
        let mut slicers = s.borrow_mut();
        let i = match slicers.iter().position(|x| x.level() == level) {
            Some(i) => i,
            None => {
                slicers.push(Slicer::new(level)?);
                slicers.len() - 1
            }
        };
        slicers[i].frame(raw)
    })
}

/// A frame another thread compresses: the writer waits on it in file order.
#[cfg(feature = "encode")]
pub(crate) struct Slot {
    done: Mutex<Option<io::Result<Vec<u8>>>>,
    ready: Condvar,
}

#[cfg(feature = "encode")]
impl Slot {
    /// Compresses `raw` at `level` on rayon's threads.
    pub fn spawn(raw: Arc<Vec<u8>>, level: i32) -> Arc<Self> {
        let slot = Arc::new(Self {
            done: Mutex::new(None),
            ready: Condvar::new(),
        });
        let filled = Arc::clone(&slot);
        rayon::spawn(move || {
            let frame = compress(&raw, level);
            *filled.done.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame);
            filled.ready.notify_all();
        });
        slot
    }

    /// The frame, if it is done.
    pub fn try_take(&self) -> Option<io::Result<Vec<u8>>> {
        self.done.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// The frame, once it is done.
    pub fn wait(&self) -> io::Result<Vec<u8>> {
        let mut done = self.done.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(frame) = done.take() {
                return frame;
            }
            done = self.ready.wait(done).unwrap_or_else(|e| e.into_inner());
        }
    }
}

#[cfg(all(test, feature = "encode"))]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_the_same_in_slices_however_the_calls_fall_and_decodes_whole() {
        let raw: Vec<u8> = (0..200_000u32)
            .flat_map(|i| (i / 7).to_le_bytes())
            .collect();
        let whole = compress(&raw, 3).unwrap();
        // A slice a call, with other frames between, on one context.
        let mut s = Slicer::new(3).unwrap();
        let mut other = Slicer::new(3).unwrap();
        s.begin(raw.len()).unwrap();
        let mut out = Vec::new();
        for (i, chunk) in raw.chunks(SLICE).enumerate() {
            other.frame(&raw[..1000 + i]).unwrap();
            s.slice(chunk, (i + 1) * SLICE >= raw.len(), &mut out)
                .unwrap();
        }
        assert_eq!(out, whole);
        assert_eq!(crate::decode::inflate(&whole, raw.len()).unwrap(), raw);
        // On another thread.
        let slot = Slot::spawn(Arc::new(raw.clone()), 3);
        assert_eq!(slot.wait().unwrap(), whole);
        // Nothing at all is a frame too.
        let empty = compress(&[], 19).unwrap();
        assert_eq!(crate::decode::inflate(&empty, 0).unwrap(), Vec::<u8>::new());
    }
}
