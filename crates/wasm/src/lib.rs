//! tickwave's decoder for the browser. JavaScript fetches the bytes (the file's end, then the
//! pages a view's channels and span need, then the blocks they place) and hands them in; this
//! decodes them.
//!
//! Two ways to read the index. A page reads it a page at a time: `blockPages`, `levelPages` and
//! `listPages` say which pages of a channel's table hold a span, `blockPage`, `levelPage` and
//! `listPage` check one and give its entries, and `decodeBlockAt` and `decodeOverviewAt` decode
//! the frames they place. A script that reads everything hands in the whole index through
//! `indexRanges` and `addIndexPage` and asks by block and row number.

use std::collections::HashMap;
use std::ops::ControlFlow;

#[cfg(feature = "annotations")]
use escapement_tickwave_format::annotations::{self, Included};
use escapement_tickwave_format::decode::{
    self, BlockChannel, BlockRef, DecodeError, Footer, Index, LevelRef, List,
};
use escapement_tickwave_format::header::{
    AnalogCoding, BlockRow, MAJOR, OverviewRow, PREAMBLE_LEN, TRAILER_LEN, Trailer,
};
use escapement_tickwave_format::index::{Entry, Located, TableIndex};
use escapement_tickwave_format::model::{Break, Gap};
use escapement_tickwave_format::overview::{AnalogOverview, ChannelOverview, DigitalBucket, Level};
use escapement_tickwave_format::samples::{self, Product, Values};
use js_sys::{BigInt64Array, Float32Array, Float64Array, Uint8Array};
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
/// starts. It reads only the trailer, so the footer is parsed once, by `new Recording`. Throws
/// when the trailer is not a finished tickwave recording's.
#[wasm_bindgen(js_name = footerStart)]
pub fn footer_start(tail: &[u8], file_len: f64) -> Result<f64, JsError> {
    let len = file_len as u64;
    let tail_start = len
        .checked_sub(tail.len() as u64)
        .ok_or_else(|| JsError::new("more bytes than the file holds"))?;
    if len < (PREAMBLE_LEN + TRAILER_LEN) as u64 {
        return Err(js(DecodeError::NotTickwave));
    }
    if tail.len() < TRAILER_LEN {
        return Ok((len - TRAILER_LEN as u64) as f64);
    }
    let trailer = Trailer::parse(&tail[tail.len() - TRAILER_LEN..])
        .ok_or_else(|| js(DecodeError::Unfinished))?;
    if trailer.major != MAJOR {
        return Err(js(DecodeError::Version {
            major: trailer.major,
            minor: trailer.minor,
        }));
    }
    let ends = trailer.footer_offset >= PREAMBLE_LEN as u64
        && trailer
            .footer_offset
            .checked_add(u64::from(trailer.footer_len))
            .is_some_and(|end| end == len - TRAILER_LEN as u64);
    if !ends {
        return Err(JsError::new(
            "the trailer's footer does not end where the trailer starts",
        ));
    }
    Ok(trailer.footer_offset.min(tail_start) as f64)
}

/// A recording as its footer and index say: its header, and once JavaScript has handed in the
/// index pages `indexRanges` asks for, each channel's blocks and overview rows.
#[wasm_bindgen]
pub struct Recording {
    read: Read,
    /// The index pages handed in, by offset, until the index is read.
    pages: HashMap<u64, Vec<u8>>,
    /// The directory pages handed in through `addDirectory`, by offset: one for every 16 M
    /// entries of a table, so they are kept.
    dirs: HashMap<u64, Vec<u8>>,
    /// The directory pages the last look-up lacked.
    missing: Vec<(u64, u32)>,
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
            dirs: HashMap::new(),
            missing: Vec::new(),
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
        let (_, _, product) = self
            .footer()
            .products
            .get(d)
            .copied()
            .flatten()
            .ok_or_else(|| {
                JsError::new(&format!("no derived channel {d} this version computes"))
            })?;
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
                .and_then(|d| footer.products.get(d).copied().flatten())
                .map(|(_, _, p)| p),
        }
    }
}

/// A located page as `blockPages`, `levelPages` and `listPages` give it: its place among the
/// table's pages, its record's offset and length, the `first` and `number` of its entry, and the
/// next page's, NaN for the table's last page.
const LOCATED: usize = 7;
/// A block as `blockPage` gives it: its start and end, its first item's index, its frame's offset,
/// length and decoded length, its word, and the next block's word, NaN for a page's last block.
const BLOCK: usize = 8;
/// An overview row as `levelPage` gives it: the first block it stands for, its frame's offset,
/// length and decoded length, and how many blocks it stands for.
const ROW: usize = 5;

fn located(at: &[f64]) -> Result<Located, JsError> {
    let &[j, offset, length, first, number, next_first, next_number] = at else {
        return Err(JsError::new(&format!(
            "a located page is {LOCATED} numbers, not {}",
            at.len()
        )));
    };
    Ok(Located {
        j: j as u64,
        entry: Entry {
            first: first as i64,
            number: number as u64,
            offset: offset as u64,
            length: length as u32,
        },
        next: (!next_first.is_nan()).then_some((next_first as i64, next_number as u64)),
    })
}

fn located_numbers(pages: &[Located]) -> Vec<f64> {
    let mut out = Vec::with_capacity(pages.len() * LOCATED);
    for p in pages {
        let (nf, nn) = p
            .next
            .map_or((f64::NAN, f64::NAN), |(f, n)| (f as f64, n as f64));
        out.extend([
            p.j as f64,
            p.entry.offset as f64,
            f64::from(p.entry.length),
            p.entry.first as f64,
            p.entry.number as f64,
            nf,
            nn,
        ]);
    }
    out
}

fn block_ref(b: &[f64]) -> Result<BlockRef, JsError> {
    let &[start, end, first_index, offset, length, decoded, word, next] = b else {
        return Err(JsError::new(&format!(
            "a block is {BLOCK} numbers, not {}",
            b.len()
        )));
    };
    Ok(BlockRef {
        start: start as i64,
        end: end as i64,
        first_index: first_index as u64,
        row: BlockRow {
            start: start as i64,
            offset: offset as u64,
            length: length as u32,
            decoded_length: decoded as u32,
            word: word as u32,
        },
        next_word: (!next.is_nan()).then_some(next as u32),
    })
}

/// A channel's gaps or breaks as `listPage` gives them, three numbers an entry.
fn list_of(digital: bool, entries: &[f64]) -> Result<List, JsError> {
    let (entries, rest) = entries.as_chunks::<3>();
    if !rest.is_empty() {
        return Err(JsError::new("a list is three numbers an entry"));
    }
    Ok(if digital {
        List::Gaps(
            entries
                .iter()
                .map(|&[from, to, level]| Gap {
                    from: from as i64,
                    to: to as i64,
                    level: level != 0.0,
                })
                .collect(),
        )
    } else {
        List::Breaks(
            entries
                .iter()
                .map(|&[index, tick, lost]| Break::new(index as u64, tick as i64, lost as u64))
                .collect(),
        )
    })
}

/// The page-at-a-time index: what a view reads for the channels and the span it shows.
#[wasm_bindgen]
impl Recording {
    /// Hands in the directory page at `offset`, as `missing` asked.
    #[wasm_bindgen(js_name = addDirectory)]
    pub fn add_directory(&mut self, offset: f64, bytes: &[u8]) {
        self.dirs.insert(offset as u64, bytes.to_vec());
    }

    /// The directory pages the last `blockPages`, `levelPages` or `listPages` lacked, as byte
    /// ranges `[offset, end, offset, end, ...]`.
    pub fn missing(&self) -> Vec<f64> {
        self.missing
            .iter()
            .flat_map(|&(o, l)| [o as f64, (o + u64::from(l)) as f64])
            .collect()
    }

    /// The pages of `table` holding entries whose keys lie in `keys`, located through the
    /// directories handed in; None where one on the way is not in yet, which `missing` names.
    fn locate(
        &mut self,
        table: &TableIndex,
        keys: std::ops::Range<i64>,
    ) -> Result<Option<Vec<f64>>, JsError> {
        let Self { dirs, missing, .. } = self;
        missing.clear();
        let mut fetch = |offset: u64, len: u32| match dirs.get(&offset) {
            Some(bytes) if bytes.len() == len as usize => Ok(bytes.clone()),
            _ => {
                missing.push((offset, len));
                Err(DecodeError::Corrupt(format!(
                    "the directory page at byte {offset} is not in yet"
                )))
            }
        };
        let mut pages = Vec::new();
        let walked = table.locate(keys, &mut fetch, |at| {
            pages.push(at);
            Ok::<_, DecodeError>(ControlFlow::Continue(()))
        });
        if !self.missing.is_empty() {
            return Ok(None);
        }
        walked.map_err(js)?;
        Ok(Some(located_numbers(&pages)))
    }

    /// The pages of channel `c`'s block table that hold any tick of `[start, end)`, each as
    /// seven numbers: its place, its record's offset and length, its entry's `first` and `number`,
    /// and the next page's, NaN after the last. Undefined where a directory page on the way is
    /// not in yet: `missing` says which, to hand in through `addDirectory` before asking again.
    #[wasm_bindgen(js_name = blockPages)]
    pub fn block_pages(
        &mut self,
        c: usize,
        start: f64,
        end: f64,
    ) -> Result<Option<Vec<f64>>, JsError> {
        let table = self
            .footer()
            .channels
            .get(c)
            .and_then(|ch| ch.blocks.clone())
            .ok_or_else(|| no_channel(c))?;
        let from = start.floor() as i64;
        let to = (end.ceil() as i64).max(from.saturating_add(1));
        self.locate(&table, from..to)
    }

    /// The pages of overview channel `o`'s level `li` (of those in overview blocks, finest first)
    /// that hold rows standing for any of its blocks `k0..k1`, as `blockPages` gives them.
    #[wasm_bindgen(js_name = levelPages)]
    pub fn level_pages(
        &mut self,
        o: usize,
        li: usize,
        k0: f64,
        k1: f64,
    ) -> Result<Option<Vec<f64>>, JsError> {
        let table = self
            .footer()
            .channels
            .get(o)
            .and_then(|ch| ch.levels.get(li))
            .map(|(_, t)| t.clone())
            .ok_or_else(|| {
                JsError::new(&format!("channel {o} has no overview level {li} in blocks"))
            })?;
        let from = k0.floor() as i64;
        self.locate(&table, from..(k1.ceil() as i64).max(from.saturating_add(1)))
    }

    /// The pages of channel `c`'s gaps or breaks, where they are in list pages, that hold entries
    /// keyed `[k0, k1)`: a gap's `from`, a break's `tick`, source ticks. Where the keys lie before
    /// every entry, the first page, whose entries bound the run before them. As `blockPages` gives
    /// them.
    #[wasm_bindgen(js_name = listPages)]
    pub fn list_pages(&mut self, c: usize, k0: f64, k1: f64) -> Result<Option<Vec<f64>>, JsError> {
        let table = self
            .footer()
            .channels
            .get(c)
            .and_then(|ch| ch.list.clone())
            .ok_or_else(|| JsError::new(&format!("channel {c} has no list in pages")))?;
        let from = k0.floor() as i64;
        let pages = self.locate(&table, from..(k1.ceil() as i64).max(from.saturating_add(1)))?;
        let first = table.levels.last().and_then(|l| l.first()).map(|e| e.first);
        match (pages, first) {
            (Some(p), Some(first)) if p.is_empty() => {
                self.locate(&table, first..first.saturating_add(1))
            }
            (pages, _) => Ok(pages),
        }
    }

    /// Channel `c`'s blocks on the page `at` locates (seven numbers, as `blockPages` gives it),
    /// from exactly the page record's bytes, each checked as the whole table would be: eight
    /// numbers a block, its start and end, its first item's index, its frame's offset, length and
    /// decoded length, its word, and the next block's word, NaN for the page's last. A channel
    /// whose gaps or breaks are in pages is checked against them block by block, as
    /// `decodeBlockAt` decodes each.
    #[wasm_bindgen(js_name = blockPage)]
    pub fn block_page(&self, c: usize, at: &[f64], bytes: &[u8]) -> Result<Vec<f64>, JsError> {
        let at = located(at)?;
        let footer = self.footer();
        let table = footer
            .channels
            .get(c)
            .and_then(|ch| ch.blocks.as_ref())
            .ok_or_else(|| no_channel(c))?;
        let page = table.parse_page(0, at.j, &at.entry, bytes).map_err(js)?;
        let refs = footer.block_page(c, &at, &page, None).map_err(js)?;
        let mut out = Vec::with_capacity(refs.len() * BLOCK);
        for b in refs {
            out.extend([
                b.start as f64,
                b.end as f64,
                b.first_index as f64,
                b.row.offset as f64,
                f64::from(b.row.length),
                f64::from(b.row.decoded_length),
                f64::from(b.row.word),
                b.next_word.map_or(f64::NAN, f64::from),
            ]);
        }
        Ok(out)
    }

    /// Overview channel `o`'s level `li`'s rows on the page `at` locates, from exactly its bytes,
    /// checked: five numbers a row, the first block it stands for, its frame's offset, length and
    /// decoded length, and how many blocks it stands for.
    #[wasm_bindgen(js_name = levelPage)]
    pub fn level_page(
        &self,
        o: usize,
        li: usize,
        at: &[f64],
        bytes: &[u8],
    ) -> Result<Vec<f64>, JsError> {
        let at = located(at)?;
        let footer = self.footer();
        let (_, table) = footer
            .channels
            .get(o)
            .and_then(|ch| ch.levels.get(li))
            .ok_or_else(|| {
                JsError::new(&format!("channel {o} has no overview level {li} in blocks"))
            })?;
        let page = table.parse_page(0, at.j, &at.entry, bytes).map_err(js)?;
        let rows = footer.level_page(o, li, &at, &page).map_err(js)?;
        let mut out = Vec::with_capacity(rows.len() * ROW);
        for r in rows {
            out.extend([
                r.k as f64,
                r.row.offset as f64,
                f64::from(r.row.length),
                f64::from(r.row.decoded_length),
                f64::from(r.row.n),
            ]);
        }
        Ok(out)
    }

    /// Channel `c`'s gaps or breaks on the list page `at` locates, from exactly its bytes, checked
    /// as the whole list would be: three numbers an entry, a gap's `from`, `to` and level (0 or
    /// 1), or a break's index, tick and samples lost, in source ticks.
    #[wasm_bindgen(js_name = listPage)]
    pub fn list_page(&self, c: usize, at: &[f64], bytes: &[u8]) -> Result<Vec<f64>, JsError> {
        let at = located(at)?;
        let footer = self.footer();
        let table = footer
            .channels
            .get(c)
            .and_then(|ch| ch.list.as_ref())
            .ok_or_else(|| JsError::new(&format!("channel {c} has no list in pages")))?;
        let page = table.parse_page(0, at.j, &at.entry, bytes).map_err(js)?;
        Ok(match footer.list_page(c, &at, &page).map_err(js)? {
            List::Gaps(g) => g
                .iter()
                .flat_map(|g| [g.from as f64, g.to as f64, f64::from(u8::from(g.level))])
                .collect(),
            List::Breaks(b) => b
                .iter()
                .flat_map(|b| [b.index as f64, b.tick as f64, b.lost as f64])
                .collect(),
        })
    }

    /// Decodes channel `c`'s block `block` (eight numbers, as `blockPage` gives it) from exactly
    /// its frame's bytes. `list` is a channel's gaps or breaks that the block's ticks need, as
    /// `listPage` gives them, where they are in pages; undefined where the header holds them.
    #[wasm_bindgen(js_name = decodeBlockAt)]
    pub fn decode_block_at(
        &self,
        c: usize,
        block: &[f64],
        frame: &[u8],
        list: Option<Vec<f64>>,
    ) -> Result<Block, JsError> {
        let b = block_ref(block)?;
        let footer = self.footer();
        let ch = footer.header.channels.get(c).ok_or_else(|| no_channel(c))?;
        let inflate = |f: &[u8], len| decode::inflate_limited(f, len, self.max);
        let block = match list {
            Some(entries) => {
                let digital = ch.coding().is_none();
                footer.decode_block_in_with(c, &b, frame, &list_of(digital, &entries)?, inflate)
            }
            None => footer.decode_block_with(c, &b, frame, inflate),
        }
        .map_err(js)?;
        Ok(Block {
            start: block.start,
            end: block.end,
            coding: ch.coding(),
            data: block.data,
        })
    }

    /// Decodes overview channel `o`'s level `li`'s row `row` (five numbers, as `levelPage` gives
    /// it) from exactly its frame's bytes.
    #[wasm_bindgen(js_name = decodeOverviewAt)]
    pub fn decode_overview_at(
        &self,
        o: usize,
        li: usize,
        row: &[f64],
        frame: &[u8],
    ) -> Result<OverviewLevel, JsError> {
        let &[k, offset, length, decoded, n] = row else {
            return Err(JsError::new(&format!(
                "an overview row is {ROW} numbers, not {}",
                row.len()
            )));
        };
        let r = LevelRef {
            k: k as u64,
            row: OverviewRow {
                offset: offset as u64,
                length: length as u32,
                decoded_length: decoded as u32,
                n: n as u32,
            },
        };
        let block = self
            .footer()
            .decode_overview_with(o, li, &r, frame, |f, len| {
                decode::inflate_limited(f, len, self.max)
            })
            .map_err(js)?;
        Ok(self.level(o, block.first, block.n, block.level))
    }

    /// Derived channel `d`'s values from its operands' samples at the same indices, each as its
    /// channel stores them: an `f32` or `codes` channel's values as a Float32Array, an `f64`
    /// channel's as a Float64Array, an `int` channel's codes as a BigInt64Array (an unsigned
    /// code's bits as they are), as `Block.codes` gives them.
    #[wasm_bindgen(js_name = productValues)]
    pub fn product_values(
        &self,
        d: usize,
        a: JsValue,
        b: JsValue,
    ) -> Result<Float64Array, JsError> {
        let footer = self.footer();
        let (ia, ib, product) = footer.products.get(d).copied().flatten().ok_or_else(|| {
            JsError::new(&format!("no derived channel {d} this version computes"))
        })?;
        let coding = |c: usize| footer.header.channels.get(c).and_then(|ch| ch.coding());
        let (a, b) = (stored(a, coding(ia))?, stored(b, coding(ib))?);
        let values = samples::product_values(product, &a, &b)
            .map_err(|what| JsError::new(&format!("derived channel {d}: {what}")))?;
        let values: Vec<f64> = values.iter().map(|s| product.value(s)).collect();
        Ok(Float64Array::from(&values[..]))
    }

    /// The annotation bundle record's byte range, `[offset, end)`; empty without one.
    #[wasm_bindgen(js_name = bundleRange)]
    pub fn bundle_range(&self) -> Vec<f64> {
        self.footer().bundle.map_or_else(Vec::new, |(at, len)| {
            vec![at as f64, (at + u64::from(len)) as f64]
        })
    }

    /// The annotation bundle's files, from exactly its record's bytes: a JSON object of each
    /// file's name and text.
    #[wasm_bindgen(js_name = bundleFiles)]
    pub fn bundle_files(&self, bytes: &[u8]) -> Result<String, JsError> {
        let files = self
            .footer()
            .read_bundle(|_, len| {
                if bytes.len() == len as usize {
                    Ok(bytes.to_vec())
                } else {
                    Err(DecodeError::Corrupt(format!(
                        "the annotation bundle is {len} bytes, not the {} handed in",
                        bytes.len()
                    )))
                }
            })
            .map_err(js)?
            .ok_or_else(|| JsError::new("the recording bundles no annotations"))?;
        Ok(serde_json::to_string(&files).expect("strings serialise"))
    }
}

/// An operand's samples as JavaScript holds them, read as its coding stores them.
fn stored(v: JsValue, coding: Option<AnalogCoding>) -> Result<Values, JsError> {
    let wrong = || JsError::new("an operand's samples are not as its channel stores them");
    Ok(match coding.ok_or_else(wrong)? {
        AnalogCoding::Int(int) => {
            let codes = v.dyn_into::<BigInt64Array>().map_err(|_| wrong())?.to_vec();
            if int.signed {
                Values::I64(codes)
            } else {
                Values::U64(codes.into_iter().map(|c| c as u64).collect())
            }
        }
        AnalogCoding::F64 => {
            Values::F64(v.dyn_into::<Float64Array>().map_err(|_| wrong())?.to_vec())
        }
        _ => Values::F32(v.dyn_into::<Float32Array>().map_err(|_| wrong())?.to_vec()),
    })
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

/// A digital level as the page draws it until it draws density and duty (phase 6): each
/// bucket's level and whether it changes inside, as bits, bucket i in bit `i % 8` of byte
/// `i / 8`. A busy bucket's level says nothing; one wholly outside the kept span reads low.
fn digital(channel: &ChannelOverview) -> Option<Vec<Uint8Array>> {
    let ChannelOverview::Digital { density, .. } = channel else {
        return None;
    };
    let n = density.len();
    let (mut level, mut change) = (vec![0u8; n.div_ceil(8)], vec![0u8; n.div_ceil(8)]);
    for i in 0..n {
        let b = channel.digital(i as u64)?;
        let busy = b.density > 0 || !matches!(b.duty, 0 | 14 | DigitalBucket::OUTSIDE);
        if b.duty >= 7 && b.duty != DigitalBucket::OUTSIDE {
            level[i / 8] |= 1 << (i % 8);
        }
        if busy {
            change[i / 8] |= 1 << (i % 8);
        }
    }
    Some(vec![
        Uint8Array::from(&level[..]),
        Uint8Array::from(&change[..]),
    ])
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

    /// An `int` channel's codes in the block, an unsigned code's bits as they are: what
    /// `productValues` takes of an integer operand.
    pub fn codes(&self) -> Option<BigInt64Array> {
        let codes: Vec<i64> = match self.data.values()? {
            Values::I64(v) => v.clone(),
            Values::U64(v) => v.iter().map(|&c| c as i64).collect(),
            _ => return None,
        };
        Some(BigInt64Array::from(&codes[..]))
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
