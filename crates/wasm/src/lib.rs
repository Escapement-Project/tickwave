//! tickwave's decoder for the browser. JavaScript fetches the bytes (the file's end, then the
//! blocks a view covers) and hands them in; this decodes them.

#[cfg(feature = "annotations")]
use escapement_tickwave_format::annotations::{self, Included};
use escapement_tickwave_format::decode::{self, BlockChannel, DecodeError, Footer};
use escapement_tickwave_format::overview::ChannelOverview;
use js_sys::{Float32Array, Float64Array, Uint8Array};
use wasm_bindgen::prelude::*;

mod inflate;
use inflate::Inflater;

fn limit(max_decoded: Option<u32>) -> usize {
    max_decoded.map_or(decode::MAX_DECODED, |m| m as usize)
}

fn no_block(i: usize) -> JsError {
    JsError::new(&format!("no block {i}"))
}

fn js(e: DecodeError) -> JsError {
    JsError::new(&e.to_string())
}

/// Where the footer starts, from the file's last bytes, `tail`, of a file `fileLen` long: the
/// offset to read from to the end. When `tail` already holds the footer, that is where `tail`
/// starts. Throws when the bytes are not a finished tickwave recording. `maxDecoded` is the most
/// it decodes from one frame, 64 MiB unless given.
#[wasm_bindgen(js_name = footerStart)]
pub fn footer_start(tail: &[u8], file_len: f64, max_decoded: Option<u32>) -> Result<f64, JsError> {
    let inflater = Inflater::new(limit(max_decoded));
    match Footer::parse_with(tail, file_len as u64, |f, len| inflater.inflate(f, len)) {
        Ok(_) => Ok(file_len - tail.len() as f64),
        Err(DecodeError::NeedFrom(at)) => Ok(at as f64),
        Err(e) => Err(js(e)),
    }
}

#[wasm_bindgen]
pub struct Recording {
    footer: Footer,
    header: String,
    inflater: Inflater,
}

#[wasm_bindgen]
impl Recording {
    /// Parses the footer from the file's last bytes, which hold at least what `footerStart` asks
    /// for. `maxDecoded` is the most it decodes from one frame, its blocks' included, 64 MiB
    /// unless given.
    #[wasm_bindgen(constructor)]
    pub fn new(tail: &[u8], file_len: f64, max_decoded: Option<u32>) -> Result<Recording, JsError> {
        let inflater = Inflater::new(limit(max_decoded));
        let footer = Footer::parse_with(tail, file_len as u64, |f, len| inflater.inflate(f, len))
            .map_err(js)?;
        // The header's JSON as the file stores it: after the footer record's framing and the
        // footer's two lengths.
        let at =
            (footer.trailer.footer_offset - (file_len as u64 - tail.len() as u64)) as usize + 5;
        let header_len = u32::from_le_bytes(tail[at..at + 4].try_into().unwrap()) as usize;
        let stored = &tail[at + 8..at + 8 + header_len];
        Ok(Self {
            header: String::from_utf8_lossy(stored).into_owned(),
            footer,
            inflater,
        })
    }

    /// The header, as the file stores it.
    #[wasm_bindgen(getter)]
    pub fn header(&self) -> String {
        self.header.clone()
    }

    #[wasm_bindgen(getter, js_name = blockCount)]
    pub fn block_count(&self) -> usize {
        self.footer.rows.len()
    }

    // An index past the end throws rather than panics: a panic ends the module, and with it every
    // recording on the page.

    #[wasm_bindgen(js_name = blockStart)]
    pub fn block_start(&self, i: usize) -> Result<f64, JsError> {
        Ok(self.footer.rows.get(i).ok_or_else(|| no_block(i))?.start as f64)
    }

    /// The block's byte range in the file, `[offset, end)`, for an HTTP Range request.
    #[wasm_bindgen(js_name = blockRange)]
    pub fn block_range(&self, i: usize) -> Result<Vec<f64>, JsError> {
        if i >= self.footer.rows.len() {
            return Err(no_block(i));
        }
        let (offset, len) = self.footer.block_location(i);
        Ok(vec![offset as f64, (offset + u64::from(len)) as f64])
    }

    /// The first and one past the last block holding any tick in `[start, end)`. A fractional
    /// span is widened to whole ticks.
    #[wasm_bindgen(js_name = blocksCovering)]
    pub fn blocks_covering(&self, start: f64, end: f64) -> Vec<u32> {
        let range = self
            .footer
            .blocks_covering(start.floor() as i64, end.ceil() as i64);
        vec![range.start as u32, range.end as u32]
    }

    /// Decodes block `i` from exactly its bytes.
    #[wasm_bindgen(js_name = decodeBlock)]
    pub fn decode_block(&self, i: usize, frame: &[u8]) -> Result<Block, JsError> {
        let block = self
            .footer
            .decode_block_with(i, frame, |f, len| self.inflater.inflate(f, len))
            .map_err(js)?;
        Ok(Block {
            start: block.start,
            end: block.end,
            channels: block.channels,
        })
    }

    /// Resolves and checks a sidecar: the hand-written TOML file parsed and written out as JSON,
    /// and the generator files it includes, by name and text. Returns the annotations as JSON.
    /// Throws with every problem found, one a line.
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
        annotations::resolve_json(
            hand,
            &included,
            &self.footer.header,
            recording_file.as_deref(),
        )
        .map(|resolved| serde_json::to_string(&resolved).expect("annotations serialise"))
        .map_err(|problems| JsError::new(&problems.join("\n")))
    }

    /// The overview's level bits and change bits for a digital channel, one byte array each,
    /// bucket i in bit i % 8 of byte i / 8. `level` indexes the header's `overview`.
    #[wasm_bindgen(js_name = overviewDigital)]
    pub fn overview_digital(&self, level: usize, channel: usize) -> Option<Vec<Uint8Array>> {
        digital(self.footer.overview.get(level)?.channels.get(channel)?)
    }

    /// The overview's minimums and maximums for an analog channel.
    #[wasm_bindgen(js_name = overviewAnalog)]
    pub fn overview_analog(&self, level: usize, channel: usize) -> Option<Vec<Float32Array>> {
        analog(self.footer.overview.get(level)?.channels.get(channel)?)
    }

    /// The first and one past the last overview block of a finer level holding any tick in
    /// `[start, end)`. `level` indexes the header's `overview_blocks`.
    #[wasm_bindgen(js_name = overviewBlocksCovering)]
    pub fn overview_blocks_covering(
        &self,
        level: usize,
        start: f64,
        end: f64,
    ) -> Result<Vec<u32>, JsError> {
        if level >= self.footer.header.overview_blocks.len() {
            return Err(JsError::new(&format!(
                "no overview level {level} in blocks"
            )));
        }
        let range =
            self.footer
                .overview_blocks_covering(level, start.floor() as i64, end.ceil() as i64);
        Ok(vec![range.start as u32, range.end as u32])
    }

    /// Overview block `k`'s byte range in the file, `[offset, end)`.
    #[wasm_bindgen(js_name = overviewBlockRange)]
    pub fn overview_block_range(&self, level: usize, k: usize) -> Result<Vec<f64>, JsError> {
        if self
            .footer
            .overview_rows
            .get(level)
            .is_none_or(|rows| k >= rows.len())
        {
            return Err(JsError::new(&format!(
                "no overview block {k} of level {level}"
            )));
        }
        let (offset, len) = self.footer.overview_block_location(level, k);
        Ok(vec![offset as f64, (offset + u64::from(len)) as f64])
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
            self.footer
                .decode_overview_block_with(level, k, frame, |f, len| self.inflater.inflate(f, len))
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

/// Buckets `first..first + buckets` of a finer overview level, laid out as the footer's levels.
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

    /// Exclusive: the next block's start, or the recording's length for the last block.
    #[wasm_bindgen(getter)]
    pub fn end(&self) -> f64 {
        self.end as f64
    }

    /// A digital channel's level at the block's start; `undefined` in JavaScript for an analog
    /// channel.
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
