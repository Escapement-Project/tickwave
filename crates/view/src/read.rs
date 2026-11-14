//! What a query reads of a recording: decoded blocks, through the cache, and from them a
//! channel's edges and sample values; overview buckets at a level.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use escapement_tickwave_format::decode::{BlockChannel, OverviewBlock};
use escapement_tickwave_format::model::Sample;
use escapement_tickwave_format::overview::{AnalogOverview, BLOCK_BUCKETS, ChannelOverview, Level};
use escapement_tickwave_format::samples::Values;

use crate::bytes::read;
use crate::cache::{Decoded, Entry, Key};
use crate::recording::{BlockFrom, Error, Part, Place, Recording, Rule, Store};
use crate::runs::index_at_or_after;

pub(crate) fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

/// A level's buckets from one source: bucket `first` of the level is `level`'s bucket 0.
pub(crate) enum Piece<'a> {
    Borrowed(&'a Level),
    #[cfg(feature = "live")]
    Shared(Arc<Level>),
    Block(Arc<OverviewBlock>),
}

impl Piece<'_> {
    pub fn level(&self) -> &Level {
        match self {
            Self::Borrowed(l) => l,
            #[cfg(feature = "live")]
            Self::Shared(l) => l,
            Self::Block(b) => &b.level,
        }
    }
}

/// One analog channel's buckets over a range, as values: the smallest and largest of each, +∞ and
/// −∞ where a bucket holds none, and the sum of each bucket's codes where they are integers.
pub(crate) struct AnalogBuckets {
    pub min: Vec<f64>,
    pub max: Vec<f64>,
    pub sums: Option<Vec<i128>>,
}

impl Recording {
    /// The blocks holding any tick of [from, to).
    pub(crate) fn blocks_covering(&self, from: i64, to: i64) -> Range<usize> {
        let b = &self.blocks;
        let first = b.partition_point(|x| x.end <= from);
        let last = b.partition_point(|x| x.start < to).max(first);
        first..last
    }

    pub(crate) fn block(&self, i: usize, cancel: &AtomicBool) -> Result<Arc<Decoded>, Error> {
        check(cancel)?;
        let at = self.blocks[i];
        match at.from {
            #[cfg(feature = "live")]
            BlockFrom::Unwritten(j) => match &self.store {
                Store::Live(live) => Ok(Arc::clone(&live.unwritten[j])),
                Store::Finished(_) => unreachable!("a finished file's blocks are written {j}"),
            },
            BlockFrom::Frame {
                index,
                offset,
                length,
            } => {
                let entry = self.cache.get_or_make(Key::Block(offset), || {
                    let _span =
                        tracing::debug_span!("decode block", block = index, length).entered();
                    let frame = read(&*self.bytes, offset, length as usize)?;
                    let (block, derived) = match &self.store {
                        Store::Finished(footer) => {
                            let block = footer.decode_block(index, &frame)?;
                            let derived = (0..self.products.len())
                                .map(|d| footer.derived(&block, d))
                                .collect::<Result<Vec<_>, _>>()?;
                            (block, derived)
                        }
                        #[cfg(feature = "live")]
                        Store::Live(live) => {
                            let block = live.snapshot.decode_block(index, &frame)?;
                            let derived = (0..self.products.len())
                                .map(|d| live.snapshot.derived(&block, d))
                                .collect::<Result<Vec<_>, _>>()?;
                            (block, derived)
                        }
                    };
                    let decoded = Decoded { block, derived };
                    let size = decoded.size();
                    Ok::<_, Error>((Entry::Block(Arc::new(decoded)), size))
                })?;
                match entry {
                    Entry::Block(b) => Ok(b),
                    Entry::Overview(_) => unreachable!("a block's key holds a block"),
                }
            }
        }
    }

    /// Digital channel `channel`'s level at `from` and its edges in [from, to), within the
    /// recording. Past a gap's end the level is the gap's.
    pub fn edges(
        &self,
        channel: usize,
        from: i64,
        to: i64,
        cancel: &AtomicBool,
    ) -> Result<(bool, Vec<i64>), Error> {
        let Place::Digital {
            gaps,
            leading,
            initial,
            ..
        } = self.place(channel)?
        else {
            return Err(Error::NoChannel(channel));
        };
        let from = from.clamp(0, self.length.max(1) - 1);
        let to = to.min(self.length);
        let range = self.blocks_covering(from, to.max(from + 1));
        let mut level = *initial;
        let mut out = Vec::new();
        for (k, i) in range.enumerate() {
            let block = self.block(i, cancel)?;
            let BlockChannel::Digital { initial, edges, .. } = &block.block.channels[channel]
            else {
                return Err(Error::NoChannel(channel));
            };
            let lo = edges.partition_point(|&t| t < from);
            if k == 0 {
                level = *initial ^ (lo % 2 == 1);
                // A block's level bit carries the level from before a gap until the gap's end, which
                // sets it: the last gap ending in the block, at or before `from`. A gap ending at
                // the block's start is in the block. The gap from tick 0 comes before the others.
                let start = block.block.start;
                let last_ended = match gaps.partition_point(|g| g.to <= from) {
                    0 => leading.as_ref().filter(|g| g.to <= from),
                    n => Some(&gaps[n - 1]),
                };
                if let Some(g) = last_ended.filter(|g| start <= g.to) {
                    let after = edges.partition_point(|&t| t < g.to);
                    level = g.level ^ ((lo - after) % 2 == 1);
                }
            }
            let hi = lo + edges[lo..].partition_point(|&t| t < to);
            out.extend_from_slice(&edges[lo..hi]);
        }
        Ok((level, out))
    }

    /// Analog or derived channel `channel`'s sample values, samples `i0..i1`.
    pub(crate) fn values(
        &self,
        channel: usize,
        i0: u64,
        i1: u64,
        cancel: &AtomicBool,
    ) -> Result<Vec<f64>, Error> {
        let mut out = vec![f64::NAN; i1.saturating_sub(i0) as usize];
        self.each_sample(channel, i0, i1, cancel, |i, value, _| {
            out[(i - i0) as usize] = value;
            Ok(())
        })?;
        Ok(out)
    }

    /// Calls `f` with each of samples `i0..i1` of analog or derived channel `channel`: its index,
    /// its value, and its code where the channel stores integer codes. Stops at `f`'s first
    /// error. Blocks come through the cache, and cancel is checked between them.
    pub fn each_sample(
        &self,
        channel: usize,
        i0: u64,
        i1: u64,
        cancel: &AtomicBool,
        mut f: impl FnMut(u64, f64, Option<i128>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let place = self.place(channel)?;
        let (Some(runs), Place::Analog { rule, count, .. }) = (place.runs(), place) else {
            return Err(Error::NoChannel(channel));
        };
        let i1 = i1.min(*count);
        if i1 <= i0 {
            return Ok(());
        }
        let stored = self.places.len() - self.products.len();
        let (from, to) = (runs.tick(i0), runs.tick(i1 - 1) + 1);
        for b in self.blocks_covering(from, to) {
            let block = self.block(b, cancel)?;
            let (first, values) = if channel < stored {
                match &block.block.channels[channel] {
                    BlockChannel::Analog {
                        first_index,
                        values,
                    } => (*first_index, values),
                    BlockChannel::Digital { .. } => return Err(Error::NoChannel(channel)),
                }
            } else {
                // A derived channel's samples sit where its first operand's do.
                let (a, _, _) = self.products[channel - stored];
                let BlockChannel::Analog { first_index, .. } = &block.block.channels[a] else {
                    return Err(Error::NoChannel(channel));
                };
                (*first_index, &block.derived[channel - stored])
            };
            let lo = i0.max(first);
            let hi = i1.min(first + values.len() as u64);
            let (a, z) = ((lo - first) as usize, (hi - first) as usize);
            // One match a block on how it stores its samples, rather than one a sample.
            match values {
                Values::F32(v) => (lo..)
                    .zip(&v[a..z])
                    .try_for_each(|(i, &x)| f(i, rule.value(Sample::F32(x)), None))?,
                Values::F64(v) => (lo..)
                    .zip(&v[a..z])
                    .try_for_each(|(i, &x)| f(i, rule.value(Sample::F64(x)), None))?,
                Values::U64(v) => (lo..).zip(&v[a..z]).try_for_each(|(i, &x)| {
                    f(i, rule.value(Sample::U64(x)), Some(i128::from(x)))
                })?,
                Values::I64(v) => (lo..).zip(&v[a..z]).try_for_each(|(i, &x)| {
                    f(i, rule.value(Sample::I64(x)), Some(i128::from(x)))
                })?,
            }
        }
        Ok(())
    }

    /// The coarsest level whose buckets are no wider than `ticks`, of those with any bucket.
    pub(crate) fn level_for(&self, ticks: f64) -> Option<usize> {
        self.levels
            .iter()
            .rposition(|l| ((1u64 << l.shift) as f64) <= ticks && l.covered > 0)
    }

    /// Level `level`'s buckets that hold any of `k0..k1`, from each place that holds them, in
    /// order: each with the level's index of its first bucket.
    pub(crate) fn pieces(
        &self,
        level: usize,
        k0: u64,
        k1: u64,
        cancel: &AtomicBool,
    ) -> Result<Vec<(u64, Piece<'_>)>, Error> {
        let l = &self.levels[level];
        let k1 = k1.min(l.covered);
        let mut out = Vec::new();
        if k1 <= k0 {
            return Ok(out);
        }
        let blocks = |n: usize| {
            let first = (k0 / BLOCK_BUCKETS) as usize;
            first..(((k1 - 1) / BLOCK_BUCKETS) as usize + 1).min(n)
        };
        for part in &l.parts {
            match (*part, &self.store) {
                (Part::Head(i), Store::Finished(footer)) => {
                    out.push((0, Piece::Borrowed(&footer.overview[i])));
                }
                (Part::Blocks(level), Store::Finished(footer)) => {
                    for k in blocks(footer.overview_rows[level].len()) {
                        let (offset, length) = footer.overview_block_location(level, k);
                        let block = self.overview_block(offset, length, cancel, |frame| {
                            Ok(footer.decode_overview_block(level, k, frame)?)
                        })?;
                        out.push((block.first, Piece::Block(block)));
                    }
                }
                #[cfg(feature = "live")]
                (
                    Part::LiveBlocks {
                        level,
                        written,
                        queued,
                    },
                    Store::Live(live),
                ) => {
                    for k in blocks(written + queued) {
                        let block = if k < written {
                            let (offset, length) = live.snapshot.overview_block_location(level, k);
                            self.overview_block(offset, length, cancel, |frame| {
                                Ok(live.snapshot.decode_overview_block(level, k, frame)?)
                            })?
                        } else {
                            Arc::clone(&live.queued_overview[level][k - written])
                        };
                        out.push((block.first, Piece::Block(block)));
                    }
                }
                #[cfg(feature = "live")]
                (Part::Held(i), Store::Live(live)) => {
                    let held = &live.snapshot.overview()[i];
                    let mut first = held.first;
                    for chunk in &held.chunks {
                        if first < k1 && first + chunk.buckets > k0 {
                            out.push((first, Piece::Shared(Arc::clone(chunk))));
                        }
                        first += chunk.buckets;
                    }
                }
                #[allow(unreachable_patterns)]
                _ => unreachable!("a level's parts follow its store"),
            }
        }
        Ok(out)
    }

    fn overview_block(
        &self,
        offset: u64,
        length: u32,
        cancel: &AtomicBool,
        decode: impl FnOnce(&[u8]) -> Result<OverviewBlock, Error>,
    ) -> Result<Arc<OverviewBlock>, Error> {
        check(cancel)?;
        let entry = self.cache.get_or_make(Key::Overview(offset), || {
            let _span = tracing::debug_span!("decode overview block", offset, length).entered();
            let frame = read(&*self.bytes, offset, length as usize)?;
            let block = decode(&frame)?;
            let size = format_size(&block.level);
            Ok::<_, Error>((Entry::Overview(Arc::new(block)), size))
        })?;
        match entry {
            Entry::Overview(b) => Ok(b),
            Entry::Block(_) => unreachable!("an overview block's key holds one"),
        }
    }

    /// Analog or derived channel `channel`'s buckets `k0..k1` of level `level`, as values.
    /// Buckets past what the level holds read as empty.
    pub(crate) fn analog_buckets(
        &self,
        channel: usize,
        level: usize,
        k0: u64,
        k1: u64,
        cancel: &AtomicBool,
    ) -> Result<AnalogBuckets, Error> {
        let Place::Analog { rule, .. } = self.place(channel)? else {
            return Err(Error::NoChannel(channel));
        };
        let n = k1.saturating_sub(k0) as usize;
        let mut out = AnalogBuckets {
            min: vec![f64::INFINITY; n],
            max: vec![f64::NEG_INFINITY; n],
            sums: rule.exact().map(|_| vec![0; n]),
        };
        for (first, piece) in self.pieces(level, k0, k1, cancel)? {
            let level = piece.level();
            let Some(a) = level.channels[channel].analog() else {
                return Err(Error::NoChannel(channel));
            };
            let from = k0.max(first);
            let to = k1.min(first + level.buckets);
            for k in from..to {
                let (i, o) = ((k - first) as usize, (k - k0) as usize);
                let (lo, hi, sum) = bucket(a, i, *rule);
                out.min[o] = lo;
                out.max[o] = hi;
                if let (Some(sums), Some(sum)) = (&mut out.sums, sum) {
                    sums[o] = sum;
                }
            }
        }
        Ok(out)
    }

    /// Digital channel `channel`'s buckets `k0..k1` of level `level`: the level entering each and
    /// whether it changes inside. Buckets past what the level holds read as low and quiet.
    pub(crate) fn digital_buckets(
        &self,
        channel: usize,
        level: usize,
        k0: u64,
        k1: u64,
        cancel: &AtomicBool,
    ) -> Result<(Vec<bool>, Vec<bool>), Error> {
        let n = k1.saturating_sub(k0) as usize;
        let (mut levels, mut changes) = (vec![false; n], vec![false; n]);
        for (first, piece) in self.pieces(level, k0, k1, cancel)? {
            let level = piece.level();
            let c = &level.channels[channel];
            if !matches!(c, ChannelOverview::Digital { .. }) {
                return Err(Error::NoChannel(channel));
            }
            for k in k0.max(first)..k1.min(first + level.buckets) {
                let (l, ch) = c.digital(k - first).expect("digital");
                levels[(k - k0) as usize] = l;
                changes[(k - k0) as usize] = ch;
            }
        }
        Ok((levels, changes))
    }

    /// The index of the first sample at or after `tick` of analog or derived channel `channel`.
    pub fn index_at(&self, channel: usize, tick: i64) -> Result<u64, Error> {
        let runs = self
            .place(channel)?
            .runs()
            .ok_or(Error::NoChannel(channel))?;
        Ok(index_at_or_after(&runs, tick as f64))
    }
}

/// Bucket `i`'s smallest and largest value and its codes' sum. A code's value through a negative
/// scale turns the order over, so its largest code gives the smallest value.
fn bucket(a: &AnalogOverview, i: usize, rule: Rule) -> (f64, f64, Option<i128>) {
    use escapement_tickwave_format::model::Sample;
    let codes = |lo: Sample, hi: Sample, empty: bool, sum: i128| {
        if empty {
            return (f64::INFINITY, f64::NEG_INFINITY, Some(0));
        }
        let (a, b) = (rule.value(lo), rule.value(hi));
        (a.min(b), a.max(b), Some(sum))
    };
    match a {
        AnalogOverview::F32(c) => {
            let b = c.bucket(i);
            (f64::from(b.min), f64::from(b.max), None)
        }
        AnalogOverview::F64(c) => {
            let b = c.bucket(i);
            (b.min, b.max, None)
        }
        AnalogOverview::U64(c) => {
            let b = c.bucket(i);
            codes(Sample::U64(b.min), Sample::U64(b.max), b.min > b.max, b.sum)
        }
        AnalogOverview::I64(c) => {
            let b = c.bucket(i);
            codes(Sample::I64(b.min), Sample::I64(b.max), b.min > b.max, b.sum)
        }
    }
}

/// About the bytes a decoded overview level holds.
fn format_size(level: &Level) -> usize {
    level
        .channels
        .iter()
        .map(|c| match c {
            ChannelOverview::Digital { level, change } => level.len() + change.len(),
            ChannelOverview::Analog(a) => {
                a.len()
                    * match a {
                        AnalogOverview::F32(_) => 16,
                        AnalogOverview::F64(_) => 24,
                        AnalogOverview::U64(_) | AnalogOverview::I64(_) => 32,
                    }
            }
        })
        .sum::<usize>()
        + 64
}
