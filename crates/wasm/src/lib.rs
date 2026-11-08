//! chrono's decoder for the browser. JavaScript fetches the bytes (the head, then the blocks a view
//! covers) and hands them in; this decodes them.

#[cfg(feature = "annotations")]
use chrono_format::annotations::{self, Included};
use chrono_format::decode::{self, BlockChannel, DecodeError, Head};
use chrono_format::header::PREAMBLE_LEN;
use chrono_format::overview::ChannelOverview;
use js_sys::{Float32Array, Float64Array, Uint8Array};
use wasm_bindgen::prelude::*;

fn js(e: DecodeError) -> JsError {
    JsError::new(&e.to_string())
}

/// How many bytes the head needs, from the file's first bytes: 32 until the preamble is in hand.
/// Throws when the bytes are not a chrono recording.
#[wasm_bindgen(js_name = headLength)]
pub fn head_length(bytes: &[u8]) -> Result<u32, JsError> {
    match Head::parse(bytes) {
        Ok(head) => Ok(head.preamble.head_len),
        Err(DecodeError::NeedMore(n)) => Ok(n as u32),
        Err(e) => Err(js(e)),
    }
}

#[wasm_bindgen]
pub struct Recording {
    head: Head,
}

#[wasm_bindgen]
impl Recording {
    /// Parses the head from bytes that hold at least `headLength` of them.
    #[wasm_bindgen(constructor)]
    pub fn new(head: &[u8]) -> Result<Recording, JsError> {
        Ok(Self {
            head: Head::parse(head).map_err(js)?,
        })
    }

    /// The header, as the file stores it.
    #[wasm_bindgen(getter)]
    pub fn header(&self) -> String {
        serde_json::to_string(&self.head.header).expect("a header always serialises")
    }

    #[wasm_bindgen(getter, js_name = blockCount)]
    pub fn block_count(&self) -> usize {
        self.head.rows.len()
    }

    #[wasm_bindgen(js_name = blockStart)]
    pub fn block_start(&self, i: usize) -> f64 {
        self.head.rows[i].start as f64
    }

    /// The block's byte range in the file, `[offset, end)`, for an HTTP Range request.
    #[wasm_bindgen(js_name = blockRange)]
    pub fn block_range(&self, i: usize) -> Vec<f64> {
        let (offset, len) = self.head.block_location(i);
        vec![offset as f64, (offset + u64::from(len)) as f64]
    }

    /// The first and one past the last block holding any tick in `[start, end)`.
    #[wasm_bindgen(js_name = blocksCovering)]
    pub fn blocks_covering(&self, start: f64, end: f64) -> Vec<u32> {
        let range = self.head.blocks_covering(start as i64, end as i64);
        vec![range.start as u32, range.end as u32]
    }

    /// Decodes block `i` from exactly its bytes.
    #[wasm_bindgen(js_name = decodeBlock)]
    pub fn decode_block(&self, i: usize, frame: &[u8]) -> Result<Block, JsError> {
        let block = self.head.decode_block(i, frame).map_err(js)?;
        Ok(Block {
            start: block.start,
            end: block.end,
            channels: block.channels,
        })
    }

    /// Resolves and checks a sidecar: the hand-written TOML text, and the generator files it
    /// includes, by name and text. Returns the annotations as JSON. Throws with every problem
    /// found, one a line.
    #[cfg(feature = "annotations")]
    #[wasm_bindgen(js_name = resolveAnnotations)]
    pub fn resolve_annotations(
        &self,
        hand: &str,
        names: Vec<String>,
        texts: Vec<String>,
        recording_file: Option<String>,
    ) -> Result<String, JsError> {
        let included: Vec<Included<'_>> = names
            .iter()
            .zip(&texts)
            .map(|(name, text)| Included { name, text })
            .collect();
        annotations::resolve(
            hand,
            &included,
            &self.head.header,
            recording_file.as_deref(),
        )
        .map(|resolved| serde_json::to_string(&resolved).expect("annotations serialise"))
        .map_err(|problems| JsError::new(&problems.join("\n")))
    }

    /// The overview's level bits and change bits for a digital channel, one byte array each,
    /// bucket i in bit i % 8 of byte i / 8. `level` indexes the header's `overview`.
    #[wasm_bindgen(js_name = overviewDigital)]
    pub fn overview_digital(&self, level: usize, channel: usize) -> Option<Vec<Uint8Array>> {
        digital(self.head.overview.get(level)?.channels.get(channel)?)
    }

    /// The overview's minimums and maximums for an analog channel.
    #[wasm_bindgen(js_name = overviewAnalog)]
    pub fn overview_analog(&self, level: usize, channel: usize) -> Option<Vec<Float32Array>> {
        analog(self.head.overview.get(level)?.channels.get(channel)?)
    }

    /// The first and one past the last overview block of a finer level holding any tick in
    /// `[start, end)`. `level` indexes the header's `overview_blocks`.
    #[wasm_bindgen(js_name = overviewBlocksCovering)]
    pub fn overview_blocks_covering(&self, level: usize, start: f64, end: f64) -> Vec<u32> {
        let range = self
            .head
            .overview_blocks_covering(level, start as i64, end as i64);
        vec![range.start as u32, range.end as u32]
    }

    /// Overview block `k`'s byte range in the file, `[offset, end)`.
    #[wasm_bindgen(js_name = overviewBlockRange)]
    pub fn overview_block_range(&self, level: usize, k: usize) -> Vec<f64> {
        let (offset, len) = self.head.overview_block_location(level, k);
        vec![offset as f64, (offset + u64::from(len)) as f64]
    }

    /// Decodes overview block `k` from exactly its bytes.
    #[wasm_bindgen(js_name = decodeOverviewBlock)]
    pub fn decode_overview_block(
        &self,
        level: usize,
        k: usize,
        frame: &[u8],
    ) -> Result<OverviewBlock, JsError> {
        Ok(OverviewBlock(
            self.head
                .decode_overview_block(level, k, frame)
                .map_err(js)?,
        ))
    }
}

fn digital(channel: &ChannelOverview) -> Option<Vec<Uint8Array>> {
    match channel {
        ChannelOverview::Digital { level, change } => Some(vec![
            Uint8Array::from(&level[..]),
            Uint8Array::from(&change[..]),
        ]),
        ChannelOverview::Analog { .. } => None,
    }
}

fn analog(channel: &ChannelOverview) -> Option<Vec<Float32Array>> {
    match channel {
        ChannelOverview::Analog { min, max } => Some(vec![
            Float32Array::from(&min[..]),
            Float32Array::from(&max[..]),
        ]),
        ChannelOverview::Digital { .. } => None,
    }
}

/// Buckets `first..first + buckets` of a finer overview level, laid out as the head's levels.
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

    /// As `Recording.overviewDigital`; bucket `first + i` is bit i.
    pub fn digital(&self, channel: usize) -> Option<Vec<Uint8Array>> {
        digital(self.0.level.channels.get(channel)?)
    }

    /// As `Recording.overviewAnalog`; bucket `first + i` is element i.
    pub fn analog(&self, channel: usize) -> Option<Vec<Float32Array>> {
        analog(self.0.level.channels.get(channel)?)
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

    /// Exclusive. The last block's end is `Number.MAX_VALUE`.
    #[wasm_bindgen(getter)]
    pub fn end(&self) -> f64 {
        if self.end == i64::MAX {
            f64::MAX
        } else {
            self.end as f64
        }
    }

    /// A digital channel's level at the block's start; undefined for an analog channel.
    pub fn initial(&self, channel: usize) -> Option<bool> {
        match self.channels.get(channel)? {
            BlockChannel::Digital { initial, .. } => Some(*initial),
            BlockChannel::Analog { .. } => None,
        }
    }

    /// A digital channel's edge ticks in the block. Ticks stay below 2^53, so a double holds
    /// each one exactly.
    pub fn edges(&self, channel: usize) -> Option<Float64Array> {
        match self.channels.get(channel)? {
            BlockChannel::Digital { edges, .. } => {
                let ticks: Vec<f64> = edges.iter().map(|&t| t as f64).collect();
                Some(Float64Array::from(&ticks[..]))
            }
            BlockChannel::Analog { .. } => None,
        }
    }

    /// An analog channel's values in the block.
    pub fn values(&self, channel: usize) -> Option<Float32Array> {
        match self.channels.get(channel)? {
            BlockChannel::Analog { values, .. } => Some(Float32Array::from(&values[..])),
            BlockChannel::Digital { .. } => None,
        }
    }

    /// An analog channel's index of the block's first sample.
    #[wasm_bindgen(js_name = firstIndex)]
    pub fn first_index(&self, channel: usize) -> Option<f64> {
        match self.channels.get(channel)? {
            BlockChannel::Analog { first_index, .. } => Some(*first_index as f64),
            BlockChannel::Digital { .. } => None,
        }
    }
}

/// The preamble's size: a reader's first fetch must cover at least this much.
#[wasm_bindgen(js_name = preambleLength)]
pub fn preamble_length() -> u32 {
    PREAMBLE_LEN as u32
}
