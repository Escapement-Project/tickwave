//! tickwave's decoder for the browser. JavaScript fetches the bytes (the file's end, then the
//! blocks a view covers) and hands them in; this decodes them.

#[cfg(feature = "annotations")]
use escapement_tickwave_format::annotations::{self, Included};
use escapement_tickwave_format::decode::{self, BlockChannel, DecodeError, Footer};
use escapement_tickwave_format::header::AnalogCoding;
use escapement_tickwave_format::overview::{AnalogOverview, ChannelOverview, Level};
use escapement_tickwave_format::samples::{Product, Values};
use js_sys::{Float32Array, Float64Array, Uint8Array};
use wasm_bindgen::prelude::*;

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
    let max = limit(max_decoded);
    match Footer::parse_with(tail, file_len as u64, |f, len| {
        decode::inflate_limited(f, len, max)
    }) {
        Ok(_) => Ok(file_len - tail.len() as f64),
        Err(DecodeError::NeedFrom(at)) => Ok(at as f64),
        Err(e) => Err(js(e)),
    }
}

#[wasm_bindgen]
pub struct Recording {
    footer: Footer,
    header: String,
    /// The most it decodes from one frame.
    max: usize,
}

#[wasm_bindgen]
impl Recording {
    /// Parses the footer from the file's last bytes, which hold at least what `footerStart` asks
    /// for. `maxDecoded` is the most it decodes from one frame, its blocks' included, 64 MiB
    /// unless given.
    #[wasm_bindgen(constructor)]
    pub fn new(tail: &[u8], file_len: f64, max_decoded: Option<u32>) -> Result<Recording, JsError> {
        let max = limit(max_decoded);
        let footer = Footer::parse_with(tail, file_len as u64, |f, len| {
            decode::inflate_limited(f, len, max)
        })
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
            max,
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
            .decode_block_with(i, frame, |f, len| decode::inflate_limited(f, len, self.max))
            .map_err(js)?;
        // A derived channel's samples come from its operands' here, by the format's own rule.
        let derived = (0..self.footer.products.len())
            .map(|d| self.footer.derived(&block, d))
            .collect::<Result<Vec<_>, _>>()
            .map_err(js)?;
        Ok(Block {
            start: block.start,
            end: block.end,
            codings: self
                .footer
                .header
                .channels
                .iter()
                .map(|c| c.coding())
                .collect(),
            products: self.footer.products.iter().map(|&(_, _, p)| p).collect(),
            channels: block.channels,
            derived,
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

    /// The overview's minimums and maximums for an `f32` or `codes` channel.
    #[wasm_bindgen(js_name = overviewAnalog)]
    pub fn overview_analog(&self, level: usize, channel: usize) -> Option<Vec<Float32Array>> {
        analog(self.footer.overview.get(level)?.channels.get(channel)?)
    }

    /// The overview's minimums and maximums for an `f64` or `int` channel, as values: an `int`
    /// channel's codes through its scale and offset in f64. A bucket with no sample holds
    /// infinity and minus infinity.
    #[wasm_bindgen(js_name = overviewAnalog64)]
    pub fn overview_analog64(&self, level: usize, channel: usize) -> Option<Vec<Float64Array>> {
        analog64(
            self.footer.overview.get(level)?,
            channel,
            self.footer.header.channels.get(channel)?.coding()?,
        )
    }

    /// Each bucket's sum of an analog channel's stored numbers: an `int` channel's codes, every
    /// other channel's values.
    #[wasm_bindgen(js_name = overviewSums)]
    pub fn overview_sums(&self, level: usize, channel: usize) -> Option<Float64Array> {
        if channel >= self.footer.header.channels.len() {
            return None;
        }
        sums(self.footer.overview.get(level)?.channels.get(channel)?)
    }

    /// A derived channel's minimums, maximums and sums, as values.
    #[wasm_bindgen(js_name = overviewDerived)]
    pub fn overview_derived(&self, level: usize, derived: usize) -> Option<Vec<Float64Array>> {
        let (_, _, product) = *self.footer.products.get(derived)?;
        let channels = self.footer.header.channels.len();
        derived_overview(
            self.footer
                .overview
                .get(level)?
                .channels
                .get(channels + derived)?,
            product,
        )
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
        Ok(OverviewBlock {
            block: self
                .footer
                .decode_overview_block_with(level, k, frame, |f, len| {
                    decode::inflate_limited(f, len, self.max)
                })
                .map_err(js)?,
            codings: self
                .footer
                .header
                .channels
                .iter()
                .map(|c| c.coding())
                .collect(),
            products: self.footer.products.iter().map(|&(_, _, p)| p).collect(),
        })
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
    match channel.analog()? {
        AnalogOverview::F32(c) => Some(vec![
            Float32Array::from(&c.min[..]),
            Float32Array::from(&c.max[..]),
        ]),
        _ => None,
    }
}

/// Lower and upper values for buckets whose stored minimum and maximum give `lo` and `hi`: a
/// negative scale turns them round, and an empty bucket holds infinity and minus infinity.
fn ordered(lo: f64, hi: f64, empty: bool) -> (f64, f64) {
    if empty {
        (f64::INFINITY, f64::NEG_INFINITY)
    } else if lo > hi {
        (hi, lo)
    } else {
        (lo, hi)
    }
}

fn analog64(level: &Level, channel: usize, coding: AnalogCoding) -> Option<Vec<Float64Array>> {
    let (min, max): (Vec<f64>, Vec<f64>) = match (level.channels.get(channel)?.analog()?, coding) {
        (AnalogOverview::F64(c), _) => (c.min.clone(), c.max.clone()),
        (AnalogOverview::U64(c), AnalogCoding::Int(int)) => c
            .min
            .iter()
            .zip(&c.max)
            .map(|(&lo, &hi)| ordered(int.value(lo.into()), int.value(hi.into()), lo > hi))
            .unzip(),
        (AnalogOverview::I64(c), AnalogCoding::Int(int)) => c
            .min
            .iter()
            .zip(&c.max)
            .map(|(&lo, &hi)| ordered(int.value(lo.into()), int.value(hi.into()), lo > hi))
            .unzip(),
        _ => return None,
    };
    Some(vec![
        Float64Array::from(&min[..]),
        Float64Array::from(&max[..]),
    ])
}

fn sums(channel: &ChannelOverview) -> Option<Float64Array> {
    let (_, _, sum) = channel.analog()?.as_f64();
    Some(Float64Array::from(&sum[..]))
}

/// A derived channel's buckets as values: an integer product's minimum, maximum and sum each
/// through its scale by the rule for an `int` value.
fn derived_overview(channel: &ChannelOverview, product: Product) -> Option<Vec<Float64Array>> {
    let (min, max, sum) = match channel.analog()? {
        AnalogOverview::F64(c) => (c.min.clone(), c.max.clone(), c.sum.clone()),
        a => {
            let (lo, hi, sum) = a.as_f64();
            let empty: Vec<bool> = match a {
                AnalogOverview::U64(c) => c.min.iter().zip(&c.max).map(|(l, h)| l > h).collect(),
                AnalogOverview::I64(c) => c.min.iter().zip(&c.max).map(|(l, h)| l > h).collect(),
                _ => return None,
            };
            let (min, max) = lo
                .iter()
                .zip(&hi)
                .zip(empty)
                .map(|((&l, &h), e)| ordered(product.scaled(l), product.scaled(h), e))
                .unzip();
            (min, max, sum.iter().map(|&s| product.scaled(s)).collect())
        }
    };
    Some(vec![
        Float64Array::from(&min[..]),
        Float64Array::from(&max[..]),
        Float64Array::from(&sum[..]),
    ])
}

/// Buckets `first..first + buckets` of a finer overview level, laid out as the footer's levels.
#[wasm_bindgen]
pub struct OverviewBlock {
    block: decode::OverviewBlock,
    codings: Vec<Option<AnalogCoding>>,
    products: Vec<Product>,
}

#[wasm_bindgen]
impl OverviewBlock {
    #[wasm_bindgen(getter)]
    pub fn first(&self) -> f64 {
        self.block.first as f64
    }

    #[wasm_bindgen(getter)]
    pub fn buckets(&self) -> f64 {
        self.block.level.buckets as f64
    }

    /// As `Recording.overviewDigital`; bucket `first + i` is bit i.
    pub fn digital(&self, channel: usize) -> Option<Vec<Uint8Array>> {
        digital(self.block.level.channels.get(channel)?)
    }

    /// As `Recording.overviewAnalog`; bucket `first + i` is element i.
    pub fn analog(&self, channel: usize) -> Option<Vec<Float32Array>> {
        analog(self.block.level.channels.get(channel)?)
    }

    /// As `Recording.overviewAnalog64`.
    pub fn analog64(&self, channel: usize) -> Option<Vec<Float64Array>> {
        analog64(&self.block.level, channel, (*self.codings.get(channel)?)?)
    }

    /// As `Recording.overviewSums`.
    pub fn sums(&self, channel: usize) -> Option<Float64Array> {
        if channel >= self.codings.len() {
            return None;
        }
        sums(self.block.level.channels.get(channel)?)
    }

    /// As `Recording.overviewDerived`.
    pub fn derived(&self, derived: usize) -> Option<Vec<Float64Array>> {
        derived_overview(
            self.block
                .level
                .channels
                .get(self.codings.len() + derived)?,
            *self.products.get(derived)?,
        )
    }
}

#[wasm_bindgen]
pub struct Block {
    start: i64,
    end: i64,
    codings: Vec<Option<AnalogCoding>>,
    products: Vec<Product>,
    channels: Vec<BlockChannel>,
    derived: Vec<Values>,
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

    /// An `f32` or `codes` channel's values in the block.
    pub fn values(&self, channel: usize) -> Option<Float32Array> {
        Some(Float32Array::from(
            self.channels.get(channel)?.values()?.as_f32()?,
        ))
    }

    /// An `f64` or `int` channel's values in the block: an `int` channel's codes through its
    /// scale and offset in f64.
    pub fn values64(&self, channel: usize) -> Option<Float64Array> {
        let coding = (*self.codings.get(channel)?)?;
        let values: Vec<f64> = match (self.channels.get(channel)?.values()?, coding) {
            (Values::F64(v), _) => v.clone(),
            (Values::U64(v), AnalogCoding::Int(int)) => {
                v.iter().map(|&c| int.value(c.into())).collect()
            }
            (Values::I64(v), AnalogCoding::Int(int)) => {
                v.iter().map(|&c| int.value(c.into())).collect()
            }
            _ => return None,
        };
        Some(Float64Array::from(&values[..]))
    }

    /// A derived channel's values in the block, from its operands' samples there.
    pub fn derived(&self, derived: usize) -> Option<Float64Array> {
        let product = self.products.get(derived)?;
        let values: Vec<f64> = self
            .derived
            .get(derived)?
            .iter()
            .map(|s| product.value(s))
            .collect();
        Some(Float64Array::from(&values[..]))
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
