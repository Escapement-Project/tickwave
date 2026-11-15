//! A writer's state as a live view in the same process reads it: what `Writer::snapshot` gives.
//! Each channel's written blocks are in the file, listed by its index's pages there and by the
//! rows no page holds yet; the blocks closed but not yet written, and the open one, come with
//! their decoded bytes. Each overview level likewise: rows in the file, then blocks not yet
//! written, the open run of alike blocks, and the buckets settled after it. Past the promise a
//! channel's open block can still change.

use std::sync::Arc;

use crate::decode::{
    Block, BlockRef, DecodeError, LevelRef, MAX_DECODED, OverviewBlock, Section,
    decode_overview_frame, decode_section, inflate_limited,
};
use crate::header::{BlockRow, Declaration, DeclaredKind, LEVEL_BIT, OverviewRow};
use crate::index::{Page, TableIndex};
use crate::mapping::Mapping;
use crate::model::{Break, Gap};
use crate::overview::{self, BLOCK_BUCKETS, Column, HeldLevel, Level, Run, Settled};
use crate::record::{self, BlockHead};
use crate::samples::Product;
use crate::shared::Chunks;

pub(crate) fn corrupt(what: impl Into<String>) -> DecodeError {
    DecodeError::Corrupt(what.into())
}

/// A block not yet in the file: its head, its decoded bytes and the index of its first item. The
/// open block's head ends at `i64::MAX`.
#[derive(Clone, Debug)]
pub struct Unwritten {
    pub head: BlockHead,
    pub raw: Arc<Vec<u8>>,
    pub first_index: u64,
}

/// One channel as the writer holds it.
#[derive(Clone, Debug)]
pub struct ChannelSnapshot {
    /// Every item of the channel before this axis tick is in; past it the open block can change.
    pub promise: i64,
    /// Its blocks in the file that index pages list.
    pub blocks: TableIndex,
    /// Its blocks in the file after those, which no page lists yet.
    pub rows: Chunks<BlockRow>,
    /// The index of the first item of `rows`' first block.
    pub rows_first: u64,
    /// The blocks closed and not yet written, then the open block.
    pub unwritten: Vec<Unwritten>,
    /// A digital channel's gaps so far, every one the writer was given: a gap that reaches the
    /// end is only left out once the recording finishes.
    pub gaps: Chunks<Gap>,
    /// An analog channel's runs so far: the first at index 0, then each break.
    pub runs: Chunks<Break>,
}

/// One level of a channel's overview as the writer holds it.
#[derive(Clone, Debug)]
pub struct LevelSnapshot {
    pub shift: u8,
    /// Whether it goes in overview blocks: it has more buckets than a strip holds.
    pub in_blocks: bool,
    /// Its rows in the file that index pages list.
    pub table: Option<TableIndex>,
    /// Its rows in the file after those.
    pub rows: Chunks<OverviewRow>,
    /// The first block `rows`' first row stands for.
    pub rows_first: u64,
    /// The overview blocks settled and not yet written, in order after `rows`.
    pub queued: Vec<Settled>,
    /// The run of alike blocks after those, not yet written.
    pub run: Option<Run>,
    /// The settled buckets after the run, or every settled bucket of a level not in blocks.
    pub held: HeldLevel,
}

/// One channel's or derived channel's overview as the writer holds it.
#[derive(Clone, Debug)]
pub struct OverviewSnapshot {
    pub column: Column,
    /// Its ladder, finest first.
    pub levels: Vec<LevelSnapshot>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    declaration: Arc<Declaration>,
    channels: Vec<ChannelSnapshot>,
    overview: Vec<OverviewSnapshot>,
    annotations: Chunks<record::Annotation>,
    position: u64,
    promise: i64,
    known: i64,
    products: Vec<(usize, usize, Product)>,
    /// Each channel's mapping to the axis, and its start in its source's ticks.
    places: Vec<(Mapping, i64)>,
}

impl Snapshot {
    pub(crate) fn new(
        declaration: Arc<Declaration>,
        channels: Vec<ChannelSnapshot>,
        overview: Vec<OverviewSnapshot>,
        annotations: Chunks<record::Annotation>,
        position: u64,
        promise: i64,
        known: i64,
    ) -> Self {
        let products = overview::products(&declaration.channels, &declaration.derived);
        let places = (0..declaration.channels.len())
            .map(|c| {
                (
                    declaration.mapping(c).unwrap_or(Mapping::IDENTITY),
                    declaration.start(c).unwrap_or(0),
                )
            })
            .collect();
        Self {
            declaration,
            channels,
            overview,
            annotations,
            position,
            promise,
            known,
            products,
            places,
        }
    }

    pub fn declaration(&self) -> &Declaration {
        &self.declaration
    }

    /// The file's length when the snapshot was taken, `output`'s bytes included.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Every item of every channel before this axis tick was promised in.
    pub fn promise(&self) -> i64 {
        self.promise
    }

    /// The latest known-through record's tick, 0 before the first.
    pub fn known_through(&self) -> i64 {
        self.known
    }

    pub fn channel(&self, c: usize) -> Option<&ChannelSnapshot> {
        self.channels.get(c)
    }

    /// Overview channel `o`'s: a derived channel numbered after the channels.
    pub fn overview(&self, o: usize) -> Option<&OverviewSnapshot> {
        self.overview.get(o)
    }

    pub fn products(&self) -> &[(usize, usize, Product)] {
        &self.products
    }

    /// Channel `c`'s mapping from its source's ticks to the axis.
    pub fn mapping(&self, c: usize) -> Option<Mapping> {
        self.places.get(c).map(|&(map, _)| map)
    }

    pub fn annotations(&self) -> impl Iterator<Item = &record::Annotation> + '_ {
        self.annotations_from(0)
    }

    /// The annotation records from record `from` on: what came since a reader last looked,
    /// without walking what it has.
    pub fn annotations_from(&self, from: usize) -> impl Iterator<Item = &record::Annotation> + '_ {
        self.annotations.iter_from(from)
    }

    /// Digital channel `c`'s gaps from gap `from` on.
    pub fn gaps_from(&self, c: usize, from: usize) -> impl Iterator<Item = &Gap> + '_ {
        self.channels
            .get(c)
            .into_iter()
            .flat_map(move |ch| ch.gaps.iter_from(from))
    }

    /// Analog channel `c`'s runs from run `from` on: the first at index 0, then each break.
    pub fn runs_from(&self, c: usize, from: usize) -> impl Iterator<Item = &Break> + '_ {
        self.channels
            .get(c)
            .into_iter()
            .flat_map(move |ch| ch.runs.iter_from(from))
    }

    fn digital(&self, c: usize) -> bool {
        matches!(
            self.declaration.channels.get(c).map(|x| &x.data),
            Some(DeclaredKind::Digital { .. })
        )
    }

    /// Channel `c`'s blocks in the file, their pages read through `fetch` from those it holds
    /// already in `known` on: pass the refs a previous snapshot gave, whose pages don't change.
    pub fn block_refs<E: From<DecodeError>>(
        &self,
        c: usize,
        known: &[BlockRef],
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Vec<BlockRef>, E> {
        let ch = self
            .channels
            .get(c)
            .ok_or_else(|| corrupt(format!("no channel {c}")))?;
        let digital = self.digital(c);
        let mut rows: Vec<(u64, BlockRow)> = Vec::new();
        let paged = ch.blocks.rows as usize;
        // The refs a view holds already cover the pages they came from; past those, read again.
        let reuse = known.len().min(paged) / crate::index::PAGE * crate::index::PAGE;
        let mut items = 0u64;
        for r in &known[..reuse] {
            rows.push((r.first_index, r.row));
            items = r.first_index + u64::from(r.row.items(digital));
        }
        if reuse < paged {
            let pages = ch.blocks.pages(0, &mut fetch)?;
            for (j, e) in pages.iter().enumerate().skip(reuse / crate::index::PAGE) {
                let bytes = fetch(e.offset, e.length)?;
                let Page::Blocks { number, rows: page } =
                    ch.blocks.parse_page(0, j as u64, e, &bytes)?
                else {
                    unreachable!("a block table's pages hold blocks")
                };
                if number != items {
                    return Err(corrupt(format!(
                        "channel {c}: its index pages don't follow on from each other"
                    ))
                    .into());
                }
                for row in page {
                    rows.push((items, row));
                    items += u64::from(row.items(digital));
                }
            }
        }
        let mut items = ch.rows_first;
        for row in ch.rows.iter() {
            rows.push((items, *row));
            items += u64::from(row.items(digital));
        }
        let after = ch.unwritten.first().map(|u| u.head.start);
        let mut out = Vec::with_capacity(rows.len());
        for (i, &(first_index, row)) in rows.iter().enumerate() {
            let next = rows.get(i + 1).map(|(_, r)| *r);
            out.push(BlockRef {
                start: row.start,
                end: next.map(|r| r.start).or(after).unwrap_or(i64::MAX),
                first_index,
                row,
                next_word: next.map(|r| r.word),
            });
        }
        Ok(out)
    }

    fn section(&self, c: usize, first_index: u64) -> Section {
        let (map, start) = self.places[c];
        match &self.declaration.channels[c].data {
            DeclaredKind::Digital { .. } => Section::Digital {
                start,
                end: i64::MAX,
                map,
            },
            DeclaredKind::Analog { coding, .. } => Section::Analog {
                coding: *coding,
                first_index,
            },
        }
    }

    /// Decodes channel `c`'s written block `b` from exactly its frame's bytes, as
    /// `Footer::decode_block` does.
    pub fn decode_block(&self, c: usize, b: &BlockRef, frame: &[u8]) -> Result<Block, DecodeError> {
        if c >= self.channels.len() {
            return Err(corrupt(format!("no channel {c}")));
        }
        if frame.len() != b.row.length as usize {
            return Err(corrupt(format!(
                "channel {c}: a block is {} bytes, the index says {}",
                frame.len(),
                b.row.length
            )));
        }
        let raw = inflate_limited(frame, b.row.decoded_length as usize, MAX_DECODED)?;
        let data = decode_section(
            &raw,
            &self.section(c, b.first_index),
            b.row.word,
            b.start,
            b.end,
        )
        .map_err(|what| corrupt(format!("channel {c}, a block: {what}")))?;
        Ok(Block {
            channel: c,
            start: b.start,
            end: b.end,
            data,
        })
    }

    /// Channel `c`'s blocks not yet written, decoded, the open one last with its end at
    /// `i64::MAX`.
    pub fn unwritten_blocks(&self, c: usize) -> Result<Vec<Block>, DecodeError> {
        let ch = self
            .channels
            .get(c)
            .ok_or_else(|| corrupt(format!("no channel {c}")))?;
        let mut out = Vec::with_capacity(ch.unwritten.len());
        for u in &ch.unwritten {
            let data = decode_section(
                &u.raw,
                &self.section(c, u.first_index),
                u.head.word,
                u.head.start,
                u.head.end,
            )
            .map_err(|what| corrupt(format!("channel {c}, a block not yet written: {what}")))?;
            out.push(Block {
                channel: c,
                start: u.head.start,
                end: u.head.end,
                data,
            });
        }
        Ok(out)
    }

    /// Overview channel `o`'s level `li`'s rows in the file, their pages read through `fetch`
    /// from those it holds already in `known` on, as `block_refs`.
    pub fn level_refs<E: From<DecodeError>>(
        &self,
        o: usize,
        li: usize,
        known: &[LevelRef],
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Vec<LevelRef>, E> {
        let level = self
            .overview
            .get(o)
            .and_then(|x| x.levels.get(li))
            .ok_or_else(|| corrupt(format!("channel {o} has no overview level {li}")))?;
        let mut out = Vec::new();
        let mut k = 0u64;
        if let Some(table) = &level.table {
            let paged = table.rows as usize;
            let reuse = known.len().min(paged) / crate::index::PAGE * crate::index::PAGE;
            for r in &known[..reuse] {
                out.push(*r);
                k = r.k + u64::from(r.row.n);
            }
            let pages = table.pages(0, &mut fetch)?;
            for (j, e) in pages.iter().enumerate().skip(reuse / crate::index::PAGE) {
                let bytes = fetch(e.offset, e.length)?;
                let Page::Level { k: first, rows } = table.parse_page(0, j as u64, e, &bytes)?
                else {
                    unreachable!("a level's pages hold its rows")
                };
                if first != k {
                    return Err(corrupt(format!(
                        "channel {o}: overview level 2^{}'s pages don't follow on",
                        level.shift
                    ))
                    .into());
                }
                for row in rows {
                    out.push(LevelRef { k, row });
                    k += u64::from(row.n);
                }
            }
        }
        let mut k = level.rows_first;
        for row in level.rows.iter() {
            out.push(LevelRef { k, row: *row });
            k += u64::from(row.n);
        }
        Ok(out)
    }

    /// Decodes overview channel `o`'s level `li` row `r` from exactly its frame's bytes. Every
    /// written block holds a whole block's buckets.
    pub fn decode_overview(
        &self,
        o: usize,
        li: usize,
        r: &LevelRef,
        frame: &[u8],
    ) -> Result<OverviewBlock, DecodeError> {
        let x = self
            .overview
            .get(o)
            .ok_or_else(|| corrupt(format!("no channel {o}")))?;
        let level = x
            .levels
            .get(li)
            .ok_or_else(|| corrupt(format!("channel {o} has no overview level {li}")))?;
        // A level the writer still writes has no last block yet: as long as a whole block.
        decode_overview_frame(x.column, level.shift, i64::MAX, r, frame, |f, n| {
            inflate_limited(f, n, MAX_DECODED)
        })
    }

    /// Overview channel `o`'s level `li`'s blocks not yet written, then its open run, decoded:
    /// each its first bucket, the blocks it stands for, and their buckets.
    pub fn unwritten_overview(
        &self,
        o: usize,
        li: usize,
    ) -> Result<Vec<OverviewBlock>, DecodeError> {
        let x = self
            .overview
            .get(o)
            .ok_or_else(|| corrupt(format!("no channel {o}")))?;
        let level = x
            .levels
            .get(li)
            .ok_or_else(|| corrupt(format!("channel {o} has no overview level {li}")))?;
        let parse = |k: u64, n: u64, raw: &[u8]| {
            Level::parse(raw, level.shift, BLOCK_BUCKETS, x.column)
                .map(|level| OverviewBlock {
                    first: k * BLOCK_BUCKETS,
                    n: n as u32,
                    level,
                })
                .ok_or_else(|| corrupt(format!("channel {o}: an overview block does not parse")))
        };
        let mut out = Vec::new();
        for s in &level.queued {
            out.push(parse(s.k, u64::from(s.n), &s.raw)?);
        }
        if let Some(r) = &level.run {
            out.push(parse(r.k, r.n, &r.raw)?);
        }
        Ok(out)
    }

    /// Derived channel `d`'s samples from its operands' samples in `a` and `b`, which hold the
    /// same indices.
    pub fn derived(
        &self,
        a: &Block,
        b: &Block,
        d: usize,
    ) -> Result<crate::samples::Values, DecodeError> {
        let (_, _, product) = *self
            .products
            .get(d)
            .ok_or_else(|| corrupt(format!("no derived channel {d}")))?;
        crate::decode::derived_values(&a.data, &b.data, product).map_err(|what| {
            corrupt(format!(
                "derived channel {}: {what}",
                self.declaration.derived[d].name
            ))
        })
    }

    /// The level at channel `c`'s open block's start, for a digital channel.
    pub fn open_level(&self, c: usize) -> Option<bool> {
        let u = self.channels.get(c)?.unwritten.last()?;
        self.digital(c).then_some(u.head.word & LEVEL_BIT != 0)
    }
}
