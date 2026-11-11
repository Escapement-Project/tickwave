//! escapement-tickwave-wasm's decoder surface with libzstd, compiled to wasm32, doing every
//! inflate: the head's overview (`Head::parse_with`), blocks (`decode_block_with`) and overview
//! blocks (`decode_overview_block_with`). Everything else is crates/wasm/src/lib.rs's, so the two
//! builds differ only in the zstd decoder, and ruzstd is not linked in.
//!
//! Without the default `libzstd` feature the same surface inflates with
//! escapement-tickwave-format's ruzstd `inflate`. With `probe`, `inflateBlock` and `parseBlock`
//! time the two halves of a block's decode apart; the benchmark builds both decoders that way to
//! attribute their difference.

#[cfg(feature = "probe")]
use std::cell::RefCell;

use escapement_tickwave_format::decode::{self, BlockChannel, DecodeError, Head};
use escapement_tickwave_format::overview::ChannelOverview;
use js_sys::{Float32Array, Float64Array, Uint8Array};
use wasm_bindgen::prelude::*;

fn js(e: DecodeError) -> JsError {
    JsError::new(&e.to_string())
}

#[cfg(feature = "libzstd")]
mod inflater {
    use std::cell::RefCell;

    use escapement_tickwave_format::decode::DecodeError;
    use zstd::zstd_safe::{self, DCtx};

    fn zstd_error(code: usize) -> DecodeError {
        DecodeError::Zstd(zstd_safe::get_error_name(code).to_owned())
    }

    /// One DCtx, reused for every frame.
    pub struct Inflater(RefCell<DCtx<'static>>);

    impl Inflater {
        pub fn new() -> Self {
            Self(RefCell::new(DCtx::create()))
        }

        /// `decode_block_with`'s contract: exactly `len` bytes from one frame, with a checksum that
        /// is present and matches. libzstd checks a checksum the frame declares (unless told to
        /// ignore it, which nothing here does), but accepts a frame that declares none, and
        /// decodes every frame in its input.
        pub fn inflate(&self, frame: &[u8], len: usize) -> Result<Vec<u8>, DecodeError> {
            if frame.len() < 5 || frame[..4] != [0x28, 0xb5, 0x2f, 0xfd] {
                return Err(DecodeError::Zstd("not a zstd frame".into()));
            }
            if frame[4] & 0x04 == 0 {
                return Err(DecodeError::Corrupt("a frame carries no checksum".into()));
            }
            if frame[4] & 0x03 != 0 {
                return Err(DecodeError::Corrupt("a frame names a dictionary".into()));
            }
            let size = zstd_safe::find_frame_compressed_size(frame).map_err(zstd_error)?;
            if size != frame.len() {
                return Err(DecodeError::Corrupt(format!(
                    "{} bytes follow the zstd frame",
                    frame.len() - size
                )));
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
}

#[cfg(not(feature = "libzstd"))]
mod inflater {
    use escapement_tickwave_format::decode::{self, DecodeError};

    pub struct Inflater;

    impl Inflater {
        pub fn new() -> Self {
            Self
        }

        pub fn inflate(&self, frame: &[u8], len: usize) -> Result<Vec<u8>, DecodeError> {
            decode::inflate(frame, len)
        }
    }
}

use inflater::Inflater;

#[wasm_bindgen(js_name = headLength)]
pub fn head_length(bytes: &[u8]) -> Result<u32, JsError> {
    let inflater = Inflater::new();
    match Head::parse_with(bytes, |f, len| inflater.inflate(f, len)) {
        Ok(head) => Ok(head.preamble.head_len),
        Err(DecodeError::NeedMore(n)) => Ok(n as u32),
        Err(e) => Err(js(e)),
    }
}

#[wasm_bindgen]
pub struct Recording {
    head: Head,
    inflater: Inflater,
    /// `inflateBlock`'s payload, which the next `parseBlock` takes.
    #[cfg(feature = "probe")]
    stash: RefCell<Vec<u8>>,
}

#[wasm_bindgen]
impl Recording {
    #[wasm_bindgen(constructor)]
    pub fn new(head: &[u8]) -> Result<Recording, JsError> {
        let inflater = Inflater::new();
        let head = Head::parse_with(head, |f, len| inflater.inflate(f, len)).map_err(js)?;
        Ok(Self {
            head,
            inflater,
            #[cfg(feature = "probe")]
            stash: RefCell::new(Vec::new()),
        })
    }

    #[wasm_bindgen(getter)]
    pub fn header(&self) -> String {
        serde_json::to_string(&self.head.header).expect("a header always serialises")
    }

    #[wasm_bindgen(getter, js_name = blockCount)]
    pub fn block_count(&self) -> usize {
        self.head.rows.len()
    }

    #[wasm_bindgen(js_name = blockRange)]
    pub fn block_range(&self, i: usize) -> Vec<f64> {
        let (offset, len) = self.head.block_location(i);
        vec![offset as f64, (offset + u64::from(len)) as f64]
    }

    #[wasm_bindgen(js_name = decodeBlock)]
    pub fn decode_block(&self, i: usize, frame: &[u8]) -> Result<Block, JsError> {
        let block = self
            .head
            .decode_block_with(i, frame, |f, len| self.inflater.inflate(f, len))
            .map_err(js)?;
        Ok(Block {
            start: block.start,
            end: block.end,
            channels: block.channels,
        })
    }

    /// Inflates block `i` and keeps the payload for `parseBlock`. Returns its length.
    #[cfg(feature = "probe")]
    #[wasm_bindgen(js_name = inflateBlock)]
    pub fn inflate_block(&self, i: usize, frame: &[u8]) -> Result<usize, JsError> {
        let len = self.head.rows[i].decoded_length as usize;
        let out = self.inflater.inflate(frame, len).map_err(js)?;
        let n = out.len();
        *self.stash.borrow_mut() = out;
        Ok(n)
    }

    /// `decodeBlock` with the payload `inflateBlock` kept in place of the inflate. `frame` is only
    /// checked for its length.
    #[cfg(feature = "probe")]
    #[wasm_bindgen(js_name = parseBlock)]
    pub fn parse_block(&self, i: usize, frame: &[u8]) -> Result<Block, JsError> {
        let block = self
            .head
            .decode_block_with(i, frame, |_, _| {
                Ok(std::mem::take(&mut *self.stash.borrow_mut()))
            })
            .map_err(js)?;
        Ok(Block {
            start: block.start,
            end: block.end,
            channels: block.channels,
        })
    }

    #[wasm_bindgen(js_name = overviewBlockRange)]
    pub fn overview_block_range(&self, level: usize, k: usize) -> Vec<f64> {
        let (offset, len) = self.head.overview_block_location(level, k);
        vec![offset as f64, (offset + u64::from(len)) as f64]
    }

    #[wasm_bindgen(js_name = decodeOverviewBlock)]
    pub fn decode_overview_block(
        &self,
        level: usize,
        k: usize,
        frame: &[u8],
    ) -> Result<OverviewBlock, JsError> {
        Ok(OverviewBlock(
            self.head
                .decode_overview_block_with(level, k, frame, |f, len| self.inflater.inflate(f, len))
                .map_err(js)?,
        ))
    }
}

#[wasm_bindgen]
pub struct OverviewBlock(decode::OverviewBlock);

#[wasm_bindgen]
impl OverviewBlock {
    #[wasm_bindgen(getter)]
    pub fn first(&self) -> f64 {
        self.0.first as f64
    }

    #[wasm_bindgen(getter)]
    pub fn buckets(&self) -> f64 {
        self.0.level.buckets as f64
    }

    pub fn digital(&self, channel: usize) -> Option<Vec<Uint8Array>> {
        match self.0.level.channels.get(channel)? {
            ChannelOverview::Digital { level, change } => Some(vec![
                Uint8Array::from(&level[..]),
                Uint8Array::from(&change[..]),
            ]),
            ChannelOverview::Analog { .. } => None,
        }
    }

    pub fn analog(&self, channel: usize) -> Option<Vec<Float32Array>> {
        match self.0.level.channels.get(channel)? {
            ChannelOverview::Analog { min, max } => Some(vec![
                Float32Array::from(&min[..]),
                Float32Array::from(&max[..]),
            ]),
            ChannelOverview::Digital { .. } => None,
        }
    }
}

#[wasm_bindgen]
pub struct Block {
    start: i64,
    end: i64,
    channels: Vec<BlockChannel>,
}

#[wasm_bindgen]
impl Block {
    #[wasm_bindgen(getter)]
    pub fn start(&self) -> f64 {
        self.start as f64
    }

    #[wasm_bindgen(getter)]
    pub fn end(&self) -> f64 {
        if self.end == i64::MAX {
            f64::MAX
        } else {
            self.end as f64
        }
    }

    pub fn initial(&self, channel: usize) -> Option<bool> {
        match self.channels.get(channel)? {
            BlockChannel::Digital { initial, .. } => Some(*initial),
            BlockChannel::Analog { .. } => None,
        }
    }

    pub fn edges(&self, channel: usize) -> Option<Float64Array> {
        match self.channels.get(channel)? {
            BlockChannel::Digital { edges, .. } => {
                let ticks: Vec<f64> = edges.iter().map(|&t| t as f64).collect();
                Some(Float64Array::from(&ticks[..]))
            }
            BlockChannel::Analog { .. } => None,
        }
    }

    pub fn values(&self, channel: usize) -> Option<Float32Array> {
        match self.channels.get(channel)? {
            BlockChannel::Analog { values, .. } => Some(Float32Array::from(&values[..])),
            BlockChannel::Digital { .. } => None,
        }
    }

    #[wasm_bindgen(js_name = firstIndex)]
    pub fn first_index(&self, channel: usize) -> Option<f64> {
        match self.channels.get(channel)? {
            BlockChannel::Analog { first_index, .. } => Some(*first_index as f64),
            BlockChannel::Digital { .. } => None,
        }
    }
}
