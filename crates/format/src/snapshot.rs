//! A writer's state as a live view in the same process reads it: what `Writer::snapshot` gives.
//! The written blocks' frames are in the file; the snapshot says where, and holds the blocks not
//! yet written.

use std::sync::Arc;

use crate::decode::{
    Block, DecodeError, MAX_DECODED, Section, corrupt, decode_sections, derived_values,
    inflate_limited,
};
use crate::header::{BlockRow, Declaration, DeclaredKind};
use crate::model::{Break, Gap};
use crate::overview::{self, HeldLevel};
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
    position: u64,
    products: Vec<(usize, usize, Product)>,
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
        position: u64,
    ) -> Self {
        let products = overview::products(&declaration.channels, &declaration.derived);
        Self {
            declaration,
            written,
            queued,
            open,
            gaps,
            runs,
            annotations,
            overview,
            position,
            products,
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
    /// already in overview blocks holds only those not yet written.
    pub fn overview(&self) -> &[HeldLevel] {
        &self.overview
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
