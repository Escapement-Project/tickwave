//! tickwave's decoder for the browser. JavaScript fetches the bytes (the file's end, then the
//! index pages it asks for, then the blocks a view covers) and hands them in; this decodes them.

use std::collections::HashMap;

#[cfg(feature = "annotations")]
use escapement_tickwave_format::annotations::{self, Included};
use escapement_tickwave_format::decode::{
    self, BlockChannel, BlockRef, DecodeError, Footer, Index, LevelRef,
};
use escapement_tickwave_format::header::AnalogCoding;
use escapement_tickwave_format::index::TableIndex;
use escapement_tickwave_format::overview::{AnalogOverview, ChannelOverview, Level};
use escapement_tickwave_format::samples::{Product, Values};
use js_sys::{Float32Array, Float64Array, Uint8Array};
use wasm_bindgen::prelude::*;

fn limit(max_decoded: Option<u32>) -> usize {
    max_decoded.map_or(decode::MAX_DECODED, |m| m as usize)
}

fn js(e: DecodeError) -> JsError {
    JsError::new(&e.to_string())
}

fn no_channel(c: usize) -> JsError {
    JsError::new(&format!("no channel {c}"))
}

/// Where the footer starts, from the file's last bytes, `tail`, of a file `fileLen` long: the
/// offset to read from to the end. When `tail` already holds the footer, that is where `tail`
/// starts. Throws when the bytes are not a finished tickwave recording.
#[wasm_bindgen(js_name = footerStart)]
pub fn footer_start(tail: &[u8], file_len: f64) -> Result<f64, JsError> {
    match Footer::parse(tail, file_len as u64) {
        Ok(_) => Ok(file_len - tail.len() as f64),
        Err(DecodeError::NeedFrom(at)) => Ok(at as f64),
        Err(e) => Err(js(e)),
    }
}

/// A recording as its footer and index say: its header, and once JavaScript has handed in the
/// index pages `indexRanges` asks for, each channel's blocks and overview rows.
#[wasm_bindgen]
pub struct Recording {
    read: Read,
    /// The index pages handed in, by offset, until the index is read.
    pages: HashMap<u64, Vec<u8>>,
    header: String,
    /// The most it decodes from one frame.
    max: usize,
}

/// How far a recording is read: its footer, then its index, which holds the footer.
enum Read {
    Footer(Footer),
    Index(Index),
}

/// A fetch from what JavaScript handed in, which notes what it lacks.
fn from_pages<'a>(
    pages: &'a HashMap<u64, Vec<u8>>,
    missing: &'a mut Vec<(u64, u32)>,
) -> impl FnMut(u64, u32) -> Result<Vec<u8>, DecodeError> + 'a {
    move |offset, len| match pages.get(&offset) {
        Some(bytes) if bytes.len() == len as usize => Ok(bytes.clone()),
        _ => {
            missing.push((offset, len));
            Err(DecodeError::Corrupt(format!(
                "the index page at byte {offset} is not in yet"
            )))
        }
    }
}

#[wasm_bindgen]
impl Recording {
    /// Parses the footer from the file's last bytes, which hold at least what `footerStart` asks
    /// for. `maxDecoded` is the most it decodes from one frame, its blocks' included, 64 MiB
    /// unless given.
    #[wasm_bindgen(constructor)]
    pub fn new(tail: &[u8], file_len: f64, max_decoded: Option<u32>) -> Result<Recording, JsError> {
        let footer = Footer::parse(tail, file_len as u64).map_err(js)?;
        // The header's JSON as the file stores it: after the footer record's framing and the
        // footer's two lengths.
        let at =
            (footer.trailer.footer_offset - (file_len as u64 - tail.len() as u64)) as usize + 5;
        let header_len = u32::from_le_bytes(tail[at..at + 4].try_into().unwrap()) as usize;
        let stored = &tail[at + 8..at + 8 + header_len];
        Ok(Self {
            header: String::from_utf8_lossy(stored).into_owned(),
            read: Read::Footer(footer),
            pages: HashMap::new(),
            max: limit(max_decoded),
        })
    }

    /// The header, as the file stores it.
    #[wasm_bindgen(getter)]
    pub fn header(&self) -> String {
        self.header.clone()
    }

    /// The index pages and directories still to hand in through `addIndexPage`, as byte ranges
    /// `[offset, end, offset, end, ...]`; empty once the index is read. A directory's pages come
    /// in a later call, once it is in. Throws on an index that does not read.
    #[wasm_bindgen(js_name = indexRanges)]
    pub fn index_ranges(&mut self) -> Result<Vec<f64>, JsError> {
        let Read::Footer(footer) = &self.read else {
            return Ok(Vec::new());
        };
        let mut missing = Vec::new();
        let tables = footer.channels.iter().flat_map(|ch| {
            ch.blocks
                .iter()
                .chain(ch.levels.iter().map(|(_, t)| t))
                .chain(ch.list.iter())
        });
        for table in tables {
            each_page(table, &self.pages, &mut missing);
        }
        if missing.is_empty() {
            // On a page that doesn't read, the footer stays, and so does every answer it gives.
            let mut none = Vec::new();
            let index =
                Index::read(footer.clone(), from_pages(&self.pages, &mut none)).map_err(js)?;
            self.read = Read::Index(index);
            self.pages.clear();
            return Ok(Vec::new());
        }
        missing.sort_unstable();
        missing.dedup();
        Ok(missing
            .iter()
            .flat_map(|&(o, l)| [o as f64, (o + u64::from(l)) as f64])
            .collect())
    }

    /// Hands in the index page or directory at `offset`, as `indexRanges` asked.
    #[wasm_bindgen(js_name = addIndexPage)]
    pub fn add_index_page(&mut self, offset: f64, bytes: &[u8]) {
        if matches!(self.read, Read::Footer(_)) {
            self.pages.insert(offset as u64, bytes.to_vec());
        }
    }

    fn index(&self) -> Result<&Index, JsError> {
        match &self.read {
            Read::Index(i) => Ok(i),
            Read::Footer(_) => Err(JsError::new(
                "the index is not read: hand in what indexRanges asks for",
            )),
        }
    }

    fn footer(&self) -> &Footer {
        match &self.read {
            Read::Footer(f) => f,
            Read::Index(i) => &i.footer,
        }
    }

    fn refs(&self, c: usize) -> Result<&[BlockRef], JsError> {
        Ok(self.index()?.blocks.get(c).ok_or_else(|| no_channel(c))?)
    }

    /// Channel `c`'s blocks.
    #[wasm_bindgen(js_name = blockCount)]
    pub fn block_count(&self, c: usize) -> Result<usize, JsError> {
        Ok(self.refs(c)?.len())
    }

    // An index past the end throws rather than panics: a panic ends the module, and with it every
    // recording on the page.

    fn block_ref(&self, c: usize, i: usize) -> Result<&BlockRef, JsError> {
        self.refs(c)?
            .get(i)
            .ok_or_else(|| JsError::new(&format!("channel {c} has no block {i}")))
    }

    #[wasm_bindgen(js_name = blockStart)]
    pub fn block_start(&self, c: usize, i: usize) -> Result<f64, JsError> {
        Ok(self.block_ref(c, i)?.start as f64)
    }

    /// Channel `c`'s block `i`'s byte range in the file, `[offset, end)`, for an HTTP Range
    /// request.
    #[wasm_bindgen(js_name = blockRange)]
    pub fn block_range(&self, c: usize, i: usize) -> Result<Vec<f64>, JsError> {
        let r = self.block_ref(c, i)?.row;
        Ok(vec![
            r.offset as f64,
            (r.offset + u64::from(r.length)) as f64,
        ])
    }

    /// The first and one past the last of channel `c`'s blocks holding any tick in
    /// `[start, end)`. A fractional span is widened to whole ticks.
    #[wasm_bindgen(js_name = blocksCovering)]
    pub fn blocks_covering(&self, c: usize, start: f64, end: f64) -> Result<Vec<u32>, JsError> {
        let range = Footer::blocks_covering(self.refs(c)?, start.floor() as i64, end.ceil() as i64);
        Ok(vec![range.start as u32, range.end as u32])
    }

    /// Decodes channel `c`'s block `i` from exactly its bytes.
    #[wasm_bindgen(js_name = decodeBlock)]
    pub fn decode_block(&self, c: usize, i: usize, frame: &[u8]) -> Result<Block, JsError> {
        let b = *self.block_ref(c, i)?;
        let footer = self.footer();
        let block = footer
            .decode_block_with(c, &b, frame, |f, len| {
                decode::inflate_limited(f, len, self.max)
            })
            .map_err(js)?;
        Ok(Block {
            start: block.start,
            end: block.end,
            coding: footer.header.channels.get(c).and_then(|ch| ch.coding()),
            data: block.data,
        })
    }

    /// Derived channel `d`'s values from its operands' blocks `a` and `b`, which hold the same
    /// samples: each through its product's scale.
    #[wasm_bindgen(js_name = derivedValues)]
    pub fn derived_values(&self, d: usize, a: &Block, b: &Block) -> Result<Float64Array, JsError> {
        let (_, _, product) = *self
            .footer()
            .products
            .get(d)
            .ok_or_else(|| JsError::new(&format!("no derived channel {d}")))?;
        let values = decode::derived_values(&a.data, &b.data, product)
            .map_err(|what| JsError::new(&format!("derived channel {d}: {what}")))?;
        let values: Vec<f64> = values.iter().map(|s| product.value(s)).collect();
        Ok(Float64Array::from(&values[..]))
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
            &self.footer().header,
            recording_file.as_deref(),
        )
        .map(|resolved| serde_json::to_string(&resolved).expect("annotations serialise"))
        .map_err(|problems| JsError::new(&problems.join("\n")))
    }

    /// Overview channel `o`'s strip, its coarsest level, decoded: a channel, or a derived channel
    /// numbered after the channels.
    pub fn strip(&self, o: usize) -> Result<OverviewLevel, JsError> {
        let footer = self.footer();
        let level = footer
            .strip_with(o, |f, len| decode::inflate_limited(f, len, self.max))
            .map_err(js)?;
        Ok(self.level(o, 0, 1, level))
    }

    /// Overview channel `o`'s finest shift and the shifts of its levels in overview blocks,
    /// finest first, then its strip's.
    pub fn shifts(&self, o: usize) -> Result<Vec<u8>, JsError> {
        let ch = self.footer().channels.get(o).ok_or_else(|| no_channel(o))?;
        Ok(ch
            .levels
            .iter()
            .map(|(l, _)| l.shift)
            .chain([ch.strip_shift])
            .collect())
    }

    fn rows(&self, o: usize, li: usize) -> Result<&[LevelRef], JsError> {
        self.index()?
            .levels
            .get(o)
            .and_then(|l| l.get(li))
            .map(|r| &r[..])
            .ok_or_else(|| {
                JsError::new(&format!("channel {o} has no overview level {li} in blocks"))
            })
    }

    /// The first and one past the last row of overview channel `o`'s level `li` in overview
    /// blocks holding any tick in `[start, end)`.
    #[wasm_bindgen(js_name = overviewRowsCovering)]
    pub fn overview_rows_covering(
        &self,
        o: usize,
        li: usize,
        start: f64,
        end: f64,
    ) -> Result<Vec<u32>, JsError> {
        let shift = *self
            .shifts(o)?
            .get(li)
            .ok_or_else(|| JsError::new(&format!("channel {o} has no overview level {li}")))?;
        let range = Footer::rows_covering(
            self.rows(o, li)?,
            shift,
            start.floor() as i64,
            end.ceil() as i64,
        );
        Ok(vec![range.start as u32, range.end as u32])
    }

    /// Row `r` of overview channel `o`'s level `li`: its frame's byte range in the file,
    /// `[offset, end)`.
    #[wasm_bindgen(js_name = overviewRowRange)]
    pub fn overview_row_range(&self, o: usize, li: usize, r: usize) -> Result<Vec<f64>, JsError> {
        let row = self
            .rows(o, li)?
            .get(r)
            .ok_or_else(|| JsError::new(&format!("no row {r} of overview level {li}")))?
            .row;
        Ok(vec![
            row.offset as f64,
            (row.offset + u64::from(row.length)) as f64,
        ])
    }

    /// Decodes row `r` of overview channel `o`'s level `li` from exactly its frame's bytes:
    /// buckets from `first`, the same for each of the `n` blocks the row stands for.
    #[wasm_bindgen(js_name = decodeOverviewRow)]
    pub fn decode_overview_row(
        &self,
        o: usize,
        li: usize,
        r: usize,
        frame: &[u8],
    ) -> Result<OverviewLevel, JsError> {
        let row = *self
            .rows(o, li)?
            .get(r)
            .ok_or_else(|| JsError::new(&format!("no row {r} of overview level {li}")))?;
        let block = self
            .footer()
            .decode_overview_with(o, li, &row, frame, |f, len| {
                decode::inflate_limited(f, len, self.max)
            })
            .map_err(js)?;
        Ok(self.level(o, block.first, block.n, block.level))
    }

    fn level(&self, o: usize, first: u64, n: u32, level: Level) -> OverviewLevel {
        let footer = self.footer();
        let stored = footer.header.channels.len();
        OverviewLevel {
            first,
            n,
            level,
            coding: footer.header.channels.get(o).and_then(|c| c.coding()),
            product: o
                .checked_sub(stored)
                .and_then(|d| footer.products.get(d))
                .map(|&(_, _, p)| p),
        }
    }
}

/// Notes each of `table`'s pages that `pages` lacks: its directories first, then, once they are
/// in, the pages they list.
fn each_page(table: &TableIndex, pages: &HashMap<u64, Vec<u8>>, missing: &mut Vec<(u64, u32)>) {
    let mut lacking = Vec::new();
    if let Ok(entries) = table.pages(0, &mut from_pages(pages, &mut lacking)) {
        for e in entries {
            if pages
                .get(&e.offset)
                .is_none_or(|b| b.len() != e.length as usize)
            {
                missing.push((e.offset, e.length));
            }
        }
    }
    missing.extend(lacking);
}

/// Buckets of one channel's overview level from bucket `first`: a strip, or an overview block that
/// stands for `n` blocks alike.
#[wasm_bindgen]
pub struct OverviewLevel {
    first: u64,
    n: u32,
    level: Level,
    coding: Option<AnalogCoding>,
    product: Option<Product>,
}

#[wasm_bindgen]
impl OverviewLevel {
    #[wasm_bindgen(getter)]
    pub fn first(&self) -> f64 {
        self.first as f64
    }

    /// The blocks the buckets stand for, each alike: 1 for a strip.
    #[wasm_bindgen(getter)]
    pub fn n(&self) -> u32 {
        self.n
    }

    #[wasm_bindgen(getter)]
    pub fn buckets(&self) -> f64 {
        self.level.buckets as f64
    }

    /// A digital channel's level bits and change bits, one byte array each, bucket `first + i` in
    /// bit i % 8 of byte i / 8.
    pub fn digital(&self) -> Option<Vec<Uint8Array>> {
        digital(&self.level.data)
    }

    /// An `f32` or `codes` channel's minimums and maximums.
    pub fn analog(&self) -> Option<Vec<Float32Array>> {
        analog(&self.level.data)
    }

    /// An `f64` or `int` channel's minimums and maximums, as values: an `int` channel's codes
    /// through its scale and offset in f64. A bucket with no sample holds infinity and minus
    /// infinity.
    pub fn analog64(&self) -> Option<Vec<Float64Array>> {
        analog64(&self.level, self.coding?)
    }

    /// Each bucket's sum of an analog channel's stored numbers: an `int` channel's codes, every
    /// other channel's values.
    pub fn sums(&self) -> Option<Float64Array> {
        self.coding?;
        sums(&self.level.data)
    }

    /// A derived channel's minimums, maximums and sums, as values.
    pub fn derived(&self) -> Option<Vec<Float64Array>> {
        derived_overview(&self.level.data, self.product?)
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

fn analog64(level: &Level, coding: AnalogCoding) -> Option<Vec<Float64Array>> {
    let (min, max): (Vec<f64>, Vec<f64>) = match (level.data.analog()?, coding) {
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

/// One channel's block, decoded.
#[wasm_bindgen]
pub struct Block {
    start: i64,
    end: i64,
    coding: Option<AnalogCoding>,
    data: BlockChannel,
}

#[wasm_bindgen]
impl Block {
    #[wasm_bindgen(getter)]
    pub fn start(&self) -> f64 {
        self.start as f64
    }

    /// Exclusive: the channel's next block's start, or the recording's length for its last.
    #[wasm_bindgen(getter)]
    pub fn end(&self) -> f64 {
        self.end as f64
    }

    /// A digital channel's level at the block's start; `undefined` in JavaScript for an analog
    /// channel.
    pub fn initial(&self) -> Option<bool> {
        match &self.data {
            BlockChannel::Digital { initial, .. } => Some(*initial),
            BlockChannel::Analog { .. } => None,
        }
    }

    /// A digital channel's edge ticks in the block. Ticks stay below 2^53, so a double holds
    /// each one exactly.
    pub fn edges(&self) -> Option<Float64Array> {
        match &self.data {
            BlockChannel::Digital { edges, .. } => {
                let ticks: Vec<f64> = edges.iter().map(|&t| t as f64).collect();
                Some(Float64Array::from(&ticks[..]))
            }
            BlockChannel::Analog { .. } => None,
        }
    }

    /// An `f32` or `codes` channel's values in the block.
    pub fn values(&self) -> Option<Float32Array> {
        Some(Float32Array::from(self.data.values()?.as_f32()?))
    }

    /// An `f64` or `int` channel's values in the block: an `int` channel's codes through its
    /// scale and offset in f64.
    pub fn values64(&self) -> Option<Float64Array> {
        let values: Vec<f64> = match (self.data.values()?, self.coding?) {
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

    /// An analog channel's index of the block's first sample.
    #[wasm_bindgen(js_name = firstIndex)]
    pub fn first_index(&self) -> Option<f64> {
        match &self.data {
            BlockChannel::Analog { first_index, .. } => Some(*first_index as f64),
            BlockChannel::Digital { .. } => None,
        }
    }
}
