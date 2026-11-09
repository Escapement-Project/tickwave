//! The browser's zstd decoder: libzstd, compiled to wasm32 with clang, in place of tickwave-format's
//! ruzstd `inflate`. It decodes a digital block about a quarter faster and an analog block about
//! four times as fast (bench/RESULTS.md). ruzstd stays out of the module only while nothing here
//! calls tickwave-format's `inflate`, `Head::parse`, `decode_block` or `decode_overview_block`;
//! everything goes through their `_with` forms and `Inflater::inflate`.

use std::cell::RefCell;

use tickwave_format::decode::DecodeError;
use zstd::zstd_safe::{self, DCtx};

fn zstd_error(code: usize) -> DecodeError {
    DecodeError::Zstd(zstd_safe::get_error_name(code).to_owned())
}

/// One decompression context, reused for every frame.
pub struct Inflater(RefCell<DCtx<'static>>);

impl Inflater {
    pub fn new() -> Self {
        Self(RefCell::new(DCtx::create()))
    }

    /// What tickwave-format's `inflate` does: exactly one frame filling `frame`, with a checksum,
    /// naming no dictionary, decoding to exactly `len` bytes. libzstd checks a checksum the frame
    /// declares, but accepts a frame that declares none, and decodes every frame in its input, so
    /// those two are checked here first.
    pub fn inflate(&self, frame: &[u8], len: usize) -> Result<Vec<u8>, DecodeError> {
        if frame.len() < 5 || frame[..4] != [0x28, 0xb5, 0x2f, 0xfd] {
            return Err(DecodeError::Zstd("not a zstd frame".into()));
        }
        // The frame header descriptor: bit 2 the checksum flag, bits 0-1 the dictionary id's size.
        if frame[4] & 0x04 == 0 {
            return Err(DecodeError::Corrupt("a frame carries no checksum".into()));
        }
        if frame[4] & 0x03 != 0 {
            return Err(DecodeError::Corrupt("a frame names a dictionary".into()));
        }
        let size = zstd_safe::find_frame_compressed_size(frame).map_err(zstd_error)?;
        if size != frame.len() {
            return Err(DecodeError::Corrupt(
                "the bytes hold more than one frame".into(),
            ));
        }
        let mut out = Vec::with_capacity(len);
        // Writes at most `len` bytes: a frame that decodes to more fails with dstSize_tooSmall.
        self.0
            .borrow_mut()
            .decompress(&mut out, frame)
            .map_err(zstd_error)?;
        if out.len() != len {
            return Err(DecodeError::Corrupt(format!(
                "a frame decoded to {} bytes, not {len}",
                out.len()
            )));
        }
        Ok(out)
    }
}
