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
use crate::index::{PAGE, Page, TableIndex};
use crate::mapping::Mapping;
use crate::model::{Break, Gap};
use crate::overview::{self, BLOCK_BUCKETS, Column, HeldLevel, Level, Run, Settled};
use crate::parts::{Part, PartRow};
use crate::record::BlockHead;
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
    /// end is only left out once the recording finishes. Empty for an analog channel.
    pub gaps: ListSnapshot<Gap>,
    /// An analog channel's breaks so far. Empty for a digital channel.
    pub breaks: ListSnapshot<Break>,
    /// An analog channel's first run, once it has one.
    pub first_run: Option<Break>,
}

/// A channel's gap or break list as the writer holds it: the pages in the file, the open page,
/// then the entries it lists once their block closes.
#[derive(Clone, Debug)]
pub struct ListSnapshot<T> {
    /// The list's pages in the file; None for a list of the channel's other kind.
    pub table: Option<TableIndex>,
    /// Its entries after those, which no page holds yet.
    pub open: Chunks<T>,
    /// The entries after those, not yet listed.
    pub waiting: Vec<T>,
}

impl<T> Default for ListSnapshot<T> {
    fn default() -> Self {
        Self {
            table: None,
            open: Chunks::default(),
            waiting: Vec::new(),
        }
    }
}

impl<T: Copy> ListSnapshot<T> {
    /// Its entries from entry `from` on, those in pages read through `fetch` and taken out of
    /// each page by `entries`.
    fn from<E: From<DecodeError>>(
        &self,
        from: u64,
        fetch: &mut impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
        entries: impl Fn(Page) -> Option<Vec<T>>,
    ) -> Result<Vec<T>, E> {
        let mut out = Vec::new();
        let paged = self.table.as_ref().map_or(0, |t| t.rows);
        if let Some(table) = self.table.as_ref().filter(|_| from < paged) {
            let first = from / PAGE as u64;
            for (j, e) in table
                .pages(0, fetch)?
                .iter()
                .enumerate()
                .skip(first as usize)
            {
                let bytes = fetch(e.offset, e.length)?;
                let page = table.parse_page(0, j as u64, e, &bytes)?;
                let page = entries(page)
                    .ok_or_else(|| corrupt("a list page holds another table's entries"))?;
                let skip = from.saturating_sub(j as u64 * PAGE as u64) as usize;
                out.extend(page.into_iter().skip(skip));
            }
        }
        let open = from.saturating_sub(paged) as usize;
        out.extend(self.open.iter_from(open).copied());
        let waiting = from.saturating_sub(paged + self.open.len() as u64) as usize;
        out.extend(self.waiting.iter().skip(waiting).copied());
        Ok(out)
    }
}

/// The annotations as the writer holds them: the parts table's pages in the file and its open
/// page's rows, the parts not yet written, and each generator's lines since its last part.
#[derive(Clone, Debug)]
pub(crate) struct Annotations {
    pub table: TableIndex,
    pub rows: Chunks<PartRow>,
    /// Each part waiting to be compressed, its decoded bytes, in the order they go out.
    pub queued: Vec<Arc<Vec<u8>>>,
    /// Each generator, in the order of its first annotation, and its lines since its last part,
    /// each with its newline.
    pub generators: Vec<(String, String)>,
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
    annotations: Annotations,
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
        annotations: Annotations,
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

    /// The generators, in the order of their first annotations: a generator's place here, from
    /// 1, is its file's in the bundle the writer's finish writes.
    pub fn generators(&self) -> impl Iterator<Item = &str> + '_ {
        self.annotations.generators.iter().map(|(s, _)| s.as_str())
    }

    /// How many annotation parts are in the file.
    pub fn part_count(&self) -> u64 {
        self.annotations.table.rows + self.annotations.rows.len() as u64
    }

    /// The rows of the annotation parts in the file from row `from` on, those in the table's
    /// pages read through `fetch`: what came since a reader last looked.
    pub fn part_rows_from<E: From<DecodeError>>(
        &self,
        from: u64,
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Vec<PartRow>, E> {
        let list = ListSnapshot {
            table: Some(self.annotations.table.clone()),
            open: self.annotations.rows.clone(),
            waiting: Vec::new(),
        };
        list.from(from, &mut fetch, |page| match page {
            Page::Parts(rows) => Some(rows),
            _ => None,
        })
    }

    /// Decodes the annotation part `row` places from exactly its frame's bytes, as
    /// `Footer::decode_part` does.
    pub fn decode_part(&self, row: &PartRow, frame: &[u8]) -> Result<Part, DecodeError> {
        crate::parts::decode_part(row, frame, self.declaration.timebase)
    }

    /// The annotation parts not yet in the file, in the order they will be: those waiting to be
    /// compressed, then each generator's lines since its last part, as the part they will go in,
    /// in the order of `generators`.
    pub fn unwritten_parts(&self) -> Result<Vec<Part>, DecodeError> {
        let mut out = Vec::new();
        for raw in &self.annotations.queued {
            out.push(
                Part::parse_raw(raw).map_err(|e| corrupt(format!("an annotation part: {e}")))?,
            );
        }
        for (source, lines) in &self.annotations.generators {
            if !lines.is_empty() {
                out.push(Part {
                    source: source.clone(),
                    lines: lines.clone(),
                });
            }
        }
        Ok(out)
    }

    /// Digital channel `c`'s gaps from gap `from` on, those in its list's pages read through
    /// `fetch`.
    pub fn gaps_from<E: From<DecodeError>>(
        &self,
        c: usize,
        from: u64,
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Vec<Gap>, E> {
        let Some(ch) = self.channels.get(c) else {
            return Ok(Vec::new());
        };
        ch.gaps.from(from, &mut fetch, |page| match page {
            Page::Gaps(gaps) => Some(gaps),
            _ => None,
        })
    }

    /// Analog channel `c`'s runs from run `from` on, the first at index 0, then each break,
    /// those in its list's pages read through `fetch`.
    pub fn runs_from<E: From<DecodeError>>(
        &self,
        c: usize,
        from: u64,
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Vec<Break>, E> {
        let Some(ch) = self.channels.get(c) else {
            return Ok(Vec::new());
        };
        let Some(first) = ch.first_run else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        if from == 0 {
            out.push(first);
        }
        out.extend(
            ch.breaks
                .from(from.saturating_sub(1), &mut fetch, |page| match page {
                    Page::Breaks(breaks) => Some(breaks),
                    _ => None,
                })?,
        );
        Ok(out)
    }

    fn digital(&self, c: usize) -> bool {
        matches!(
            self.declaration.channels.get(c).map(|x| &x.data),
            Some(DeclaredKind::Digital { .. })
        )
    }

    /// Channel `c`'s blocks in the file, their pages read through `fetch` from those it holds
    /// already in `known` on: pass the refs a previous snapshot of this writer gave, whose rows
    /// don't change.
    pub fn block_refs<E: From<DecodeError>>(
        &self,
        c: usize,
        known: &[BlockRef],
        fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Vec<BlockRef>, E> {
        let (keep, mut after) = self.block_refs_from(c, known, fetch)?;
        let mut out = known[..keep].to_vec();
        out.append(&mut after);
        Ok(out)
    }

    /// `block_refs` as how many of `known` it keeps, every one but the last, and the refs after
    /// those: its cost follows the blocks written since `known`, not how many the channel has.
    pub fn block_refs_from<E: From<DecodeError>>(
        &self,
        c: usize,
        known: &[BlockRef],
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<(usize, Vec<BlockRef>), E> {
        let ch = self
            .channels
            .get(c)
            .ok_or_else(|| corrupt(format!("no channel {c}")))?;
        let digital = self.digital(c);
        let paged = ch.blocks.rows as usize;
        // A written block's row stays as it is, and so does a ref to it with a row after it. The
        // last of `known` is made again: its end and the word after it are the next row's, which
        // may be new.
        let keep = known.len().min(paged + ch.rows.len()).saturating_sub(1);
        let mut rows: Vec<(u64, BlockRow)> = Vec::new();
        // The pages from the one that holds row `keep`, read again.
        let first_page = keep / crate::index::PAGE;
        let mut j = first_page * crate::index::PAGE;
        if j < paged {
            let mut items = known.get(j).map_or(0, |r| r.first_index);
            let pages = ch.blocks.pages(0, &mut fetch)?;
            for (p, e) in pages.iter().enumerate().skip(first_page) {
                let bytes = fetch(e.offset, e.length)?;
                let Page::Blocks { number, rows: page } =
                    ch.blocks.parse_page(0, p as u64, e, &bytes)?
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
                    if j >= keep {
                        rows.push((items, row));
                    }
                    items += u64::from(row.items(digital));
                    j += 1;
                }
            }
        }
        let mut items = match known.get(keep) {
            Some(r) if keep > paged => r.first_index,
            _ => ch.rows_first,
        };
        for row in ch.rows.iter_from(keep.saturating_sub(paged)) {
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
        Ok((keep, out))
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
        (0..ch.unwritten.len())
            .map(|j| self.unwritten_block(c, j))
            .collect()
    }

    /// `unwritten_blocks`' `j`th, alone. A block is the same however many snapshots hold it: an
    /// `Unwritten` with the same `raw`, head and first index decodes the same.
    pub fn unwritten_block(&self, c: usize, j: usize) -> Result<Block, DecodeError> {
        let u = self
            .channels
            .get(c)
            .ok_or_else(|| corrupt(format!("no channel {c}")))?
            .unwritten
            .get(j)
            .ok_or_else(|| corrupt(format!("channel {c} has no block {j} not yet written")))?;
        let data = decode_section(
            &u.raw,
            &self.section(c, u.first_index),
            u.head.word,
            u.head.start,
            u.head.end,
        )
        .map_err(|what| corrupt(format!("channel {c}, a block not yet written: {what}")))?;
        Ok(Block {
            channel: c,
            start: u.head.start,
            end: u.head.end,
            data,
        })
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
        let level = self.level(o, li)?;
        (0..level.queued.len() + usize::from(level.run.is_some()))
            .map(|j| self.unwritten_overview_block(o, li, j))
            .collect()
    }

    /// `unwritten_overview`'s `j`th, alone: a queued block below the level's `queued.len()`, its
    /// run at it. Either is the same however many snapshots hold it: the same `raw`, `k` and `n`
    /// at a level of the same shift parse the same.
    pub fn unwritten_overview_block(
        &self,
        o: usize,
        li: usize,
        j: usize,
    ) -> Result<OverviewBlock, DecodeError> {
        let level = self.level(o, li)?;
        let (k, n, raw) = match (level.queued.get(j), &level.run) {
            (Some(s), _) => (s.k, u64::from(s.n), &s.raw),
            (None, Some(r)) if j == level.queued.len() => (r.k, r.n, &r.raw),
            _ => {
                return Err(corrupt(format!(
                    "channel {o}'s overview level {li} has no block {j} not yet written"
                )));
            }
        };
        Level::parse(raw, level.shift, BLOCK_BUCKETS, self.overview[o].column)
            .map(|level| OverviewBlock {
                first: k * BLOCK_BUCKETS,
                n: n as u32,
                level,
            })
            .ok_or_else(|| corrupt(format!("channel {o}: an overview block does not parse")))
    }

    fn level(&self, o: usize, li: usize) -> Result<&LevelSnapshot, DecodeError> {
        self.overview
            .get(o)
            .ok_or_else(|| corrupt(format!("no channel {o}")))?
            .levels
            .get(li)
            .ok_or_else(|| corrupt(format!("channel {o} has no overview level {li}")))
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
