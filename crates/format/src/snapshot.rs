//! A writer's state as a live view in the same process reads it: what `Writer::snapshot` gives.
//! The written blocks' and overview blocks' frames are in the file; the snapshot says where, and
//! holds the ones not yet written.

use std::sync::Arc;

use crate::decode::{
    Block, DecodeError, MAX_DECODED, OverviewBlock, Section, corrupt, decode_sections,
    derived_values, inflate_limited,
};
use crate::header::{BlockRow, Declaration, DeclaredKind, OverviewLevel, OverviewRow};
use crate::model::{Break, Gap};
use crate::overview::{self, BLOCK_BUCKETS, Column, HeldLevel};
use crate::record::{self, BlockHead};
use crate::samples::{Product, Values};
use crate::shared::Chunks;

/// A block the writer has written: its index row, where it ends, and each channel's count of
/// items before it.
#[derive(Clone, Debug)]
pub struct Written {
    pub row: BlockRow,
    pub end: i64,
    pub firsts: Vec<u64>,
}

/// A block not yet written: its head, its decoded payload and each channel's count of items
/// before it. The open block's head ends at `i64::MAX`.
type Unwritten = (BlockHead, Vec<u8>, Vec<u64>);

/// A level the writer keeps in overview blocks: block `k` of it starts at bucket `65,536 k`.
/// Blocks `0..rows.len()` are in the file, and the `queued` ones, settled but not yet written,
/// follow them.
#[derive(Clone, Debug)]
struct BlockLevel {
    shift: u8,
    rows: Vec<OverviewRow>,
    queued: Vec<Arc<Vec<u8>>>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    declaration: Arc<Declaration>,
    written: Chunks<Written>,
    queued: Vec<Unwritten>,
    open: Unwritten,
    gaps: Vec<Chunks<Gap>>,
    runs: Vec<Chunks<Break>>,
    annotations: Chunks<record::Annotation>,
    overview: Vec<HeldLevel>,
    block_levels: Vec<BlockLevel>,
    position: u64,
    products: Vec<(usize, usize, Product)>,
    columns: Vec<Column>,
}

impl Snapshot {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        declaration: Arc<Declaration>,
        written: Chunks<Written>,
        queued: Vec<Unwritten>,
        open: Unwritten,
        gaps: Vec<Chunks<Gap>>,
        runs: Vec<Chunks<Break>>,
        annotations: Chunks<record::Annotation>,
        overview: Vec<HeldLevel>,
        overview_rows: &[(u8, u64, OverviewRow)],
        queued_overview: Vec<(u8, u64, Arc<Vec<u8>>)>,
        position: u64,
    ) -> Self {
        let products = overview::products(&declaration.channels, &declaration.derived);
        let columns = overview::columns(&declaration.channels, &declaration.derived);
        fn level(levels: &mut Vec<BlockLevel>, shift: u8) -> &mut BlockLevel {
            let at = levels.partition_point(|l| l.shift < shift);
            if levels.get(at).is_none_or(|l| l.shift != shift) {
                levels.insert(
                    at,
                    BlockLevel {
                        shift,
                        rows: Vec::new(),
                        queued: Vec::new(),
                    },
                );
            }
            &mut levels[at]
        }
        let mut block_levels = Vec::new();
        // A level's blocks settle, and are written, in order from block 0.
        for &(shift, k, row) in overview_rows {
            let l = level(&mut block_levels, shift);
            debug_assert_eq!(k, l.rows.len() as u64, "overview blocks written in order");
            l.rows.push(row);
        }
        for (shift, k, raw) in queued_overview {
            let l = level(&mut block_levels, shift);
            debug_assert_eq!(k, (l.rows.len() + l.queued.len()) as u64);
            l.queued.push(raw);
        }
        Self {
            declaration,
            written,
            queued,
            open,
            gaps,
            runs,
            annotations,
            overview,
            block_levels,
            position,
            products,
            columns,
        }
    }

    pub fn declaration(&self) -> &Declaration {
        &self.declaration
    }

    /// The file's length when the snapshot was taken, `output`'s bytes included.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// The blocks written, in time order: frame offset and length in the file, and words.
    pub fn rows(&self) -> impl Iterator<Item = &BlockRow> + '_ {
        self.written.iter().map(|w| &w.row)
    }

    pub fn block_count(&self) -> usize {
        self.written.len()
    }

    pub fn row(&self, i: usize) -> Option<&BlockRow> {
        self.written.get(i).map(|w| &w.row)
    }

    /// Where written block `i` ends.
    pub fn block_end(&self, i: usize) -> Option<i64> {
        self.written.get(i).map(|w| w.end)
    }

    /// Where the blocks not yet written start: the end of the last one written.
    pub fn unwritten_start(&self) -> i64 {
        self.written.last().map_or(0, |w| w.end)
    }

    /// Digital channel `c`'s gaps so far, every one the writer was given; a gap that reaches the
    /// end is only left out once the recording finishes.
    pub fn gaps(&self, c: usize) -> impl Iterator<Item = &Gap> + '_ {
        self.gaps.get(c).into_iter().flat_map(|g| g.iter())
    }

    /// Analog channel `c`'s runs so far: the first at index 0, then each break.
    pub fn runs(&self, c: usize) -> impl Iterator<Item = &Break> + '_ {
        self.runs.get(c).into_iter().flat_map(|r| r.iter())
    }

    pub fn annotations(&self) -> impl Iterator<Item = &record::Annotation> + '_ {
        self.annotations.iter()
    }

    /// The overview's settled buckets the writer holds, level by level, finest first. A level
    /// already in overview blocks holds only the buckets after its last block, written or queued.
    pub fn overview(&self) -> &[HeldLevel] {
        &self.overview
    }

    /// The levels the writer keeps in overview blocks, finest first: each one's shift, and how
    /// many of its blocks are in the file. Block `k` of a level starts at bucket `65,536 k` and
    /// holds 65,536 buckets. The blocks settled but not yet written follow, in
    /// `queued_overview_blocks`.
    pub fn overview_blocks(&self) -> Vec<OverviewLevel> {
        self.block_levels
            .iter()
            .map(|l| OverviewLevel {
                shift: l.shift,
                blocks: l.rows.len() as u32,
            })
            .collect()
    }

    /// Where overview block `k` of `overview_blocks()[level]` sits in the file: offset and
    /// length.
    pub fn overview_block_location(&self, level: usize, k: usize) -> (u64, u32) {
        let row = self.block_levels[level].rows[k];
        (row.offset, row.length)
    }

    /// Decodes overview block `k` of `overview_blocks()[level]` from exactly its frame's bytes,
    /// as `Footer::decode_overview_block` does.
    pub fn decode_overview_block(
        &self,
        level: usize,
        k: usize,
        frame: &[u8],
    ) -> Result<OverviewBlock, DecodeError> {
        let l = self
            .block_levels
            .get(level)
            .ok_or_else(|| corrupt(format!("no overview block level {level}")))?;
        let row = *l
            .rows
            .get(k)
            .ok_or_else(|| corrupt(format!("no overview block {k} of level {level}")))?;
        if frame.len() != row.length as usize {
            return Err(corrupt(format!(
                "overview block {k} is {} bytes, the index says {}",
                frame.len(),
                row.length
            )));
        }
        if u64::from(row.decoded_length) != overview::decoded_len(BLOCK_BUCKETS, &self.columns) {
            return Err(corrupt(format!(
                "overview block {k}'s decoded length is wrong"
            )));
        }
        let raw = inflate_limited(frame, row.decoded_length as usize, MAX_DECODED)?;
        self.parse_overview_block(l.shift, k, &raw)
    }

    /// The overview blocks of `overview_blocks()[level]` settled but not yet written, decoded:
    /// blocks `overview_blocks()[level].blocks` on, in order.
    pub fn queued_overview_blocks(&self, level: usize) -> Result<Vec<OverviewBlock>, DecodeError> {
        let l = self
            .block_levels
            .get(level)
            .ok_or_else(|| corrupt(format!("no overview block level {level}")))?;
        l.queued
            .iter()
            .enumerate()
            .map(|(j, raw)| self.parse_overview_block(l.shift, l.rows.len() + j, raw))
            .collect()
    }

    fn parse_overview_block(
        &self,
        shift: u8,
        k: usize,
        raw: &[u8],
    ) -> Result<OverviewBlock, DecodeError> {
        let level = overview::parse_block(raw, shift, BLOCK_BUCKETS, &self.columns)
            .ok_or_else(|| corrupt(format!("overview block {k} does not parse")))?;
        Ok(OverviewBlock {
            first: k as u64 * BLOCK_BUCKETS,
            level,
        })
    }

    fn sections(&self, firsts: &[u64]) -> Vec<Section> {
        self.declaration
            .channels
            .iter()
            .zip(firsts)
            .map(|(c, &first)| match &c.data {
                DeclaredKind::Digital { .. } => Section::Digital { end: i64::MAX },
                DeclaredKind::Analog { coding, .. } => Section::Analog {
                    coding: *coding,
                    first_index: first,
                },
            })
            .collect()
    }

    fn decode_raw(&self, (head, raw, firsts): &Unwritten) -> Result<Block, DecodeError> {
        let channels = decode_sections(
            raw,
            &self.sections(firsts),
            &head.words,
            head.start,
            head.end,
        )
        .map_err(|what| corrupt(format!("a block not yet written: {what}")))?;
        Ok(Block {
            start: head.start,
            end: head.end,
            channels,
        })
    }

    /// Decodes written block `i` from exactly its frame's bytes, as `Footer::decode_block` does.
    pub fn decode_block(&self, i: usize, frame: &[u8]) -> Result<Block, DecodeError> {
        let w = self
            .written
            .get(i)
            .ok_or_else(|| corrupt(format!("no block {i}")))?;
        if frame.len() != w.row.length as usize {
            return Err(corrupt(format!(
                "block {i} is {} bytes, the index says {}",
                frame.len(),
                w.row.length
            )));
        }
        let raw = inflate_limited(frame, w.row.decoded_length as usize, MAX_DECODED)?;
        let channels = decode_sections(
            &raw,
            &self.sections(&w.firsts),
            &w.row.channels,
            w.row.start,
            w.end,
        )
        .map_err(|what| corrupt(format!("block {i}: {what}")))?;
        Ok(Block {
            start: w.row.start,
            end: w.end,
            channels,
        })
    }

    /// The blocks closed but not yet written, decoded; none unless the writer compresses several
    /// together.
    pub fn queued_blocks(&self) -> Result<Vec<Block>, DecodeError> {
        self.queued.iter().map(|q| self.decode_raw(q)).collect()
    }

    /// The open block's items, decoded. Its `end` is `i64::MAX`: the writer has not closed it.
    pub fn open_block(&self) -> Result<Block, DecodeError> {
        self.decode_raw(&self.open)
    }

    /// Derived channel `d`'s samples in `block`, one this snapshot decoded.
    pub fn derived(&self, block: &Block, d: usize) -> Result<Values, DecodeError> {
        let operands = *self
            .products
            .get(d)
            .ok_or_else(|| corrupt(format!("no derived channel {d}")))?;
        derived_values(&block.channels, operands).map_err(|what| {
            corrupt(format!(
                "derived channel {}: {what}",
                self.declaration.derived[d].name
            ))
        })
    }
}
