//! What a query reads of a recording: a finished file's index pages, a page at a time for the
//! span asked for, and its decoded blocks, through the cache, and from them a channel's edges and
//! sample values; overview buckets at a level.

use std::ops::{ControlFlow, Range};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use escapement_tickwave_format::decode::{
    Block, BlockChannel, BlockRef, DecodeError, LevelRef, OverviewBlock,
};
use escapement_tickwave_format::header::ChannelKind as HeaderKind;
use escapement_tickwave_format::index::{Located, Table, TableIndex};
use escapement_tickwave_format::model::Sample;
use escapement_tickwave_format::overview::{
    AnalogOverview, BLOCK_BUCKETS, ChannelOverview, DigitalBucket, Level,
};
use escapement_tickwave_format::samples::{self, Values};

use crate::bytes::read;
use crate::cache::{Entry, Key, Reading, block_size, list_size};
use crate::recording::{BlockAt, BlockFrom, Error, Finished, Part, Place, Recording, Rule, Store};
use crate::runs::index_at_or_after;

/// The most samples `values` takes room for before any block decodes: 128 MB of them.
const SAMPLES_UP_FRONT: u64 = 1 << 24;

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

/// An entry of a kind its key doesn't hold, which a file can make by listing one record in two
/// tables.
fn misplaced(what: &str) -> Error {
    DecodeError::Corrupt(format!("{what} is listed where another kind of record is")).into()
}

impl Recording {
    /// Channel `c`'s blocks holding any tick of [from, to). A finished file's come from the pages
    /// of its index that hold them, each read once and kept in the cache.
    pub(crate) fn blocks_covering(
        &self,
        r: &Reading,
        c: usize,
        from: i64,
        to: i64,
    ) -> Result<Vec<BlockAt>, Error> {
        let f = match &self.store {
            Store::Finished(f) => f,
            #[cfg(feature = "live")]
            Store::Live(_) => {
                let b = self.blocks.get(c).ok_or(Error::NoChannel(c))?;
                let first = b.partition_point(|x| x.end <= from);
                let last = b.partition_point(|x| x.start < to).max(first);
                return Ok(b[first..last].to_vec());
            }
        };
        let table = f
            .footer()
            .channels
            .get(c)
            .and_then(|ch| ch.blocks.clone())
            .ok_or(Error::NoChannel(c))?;
        let mut out = Vec::new();
        for at in self.pages(r, &table, c, from..to.max(from.saturating_add(1)))? {
            let refs = self.block_page(r, f, &table, c, &at)?;
            let first = refs.partition_point(|x| x.end <= from);
            let last = refs.partition_point(|x| x.start < to).max(first);
            out.extend(refs[first..last].iter().map(|b| BlockAt {
                start: b.start,
                end: b.end,
                from: BlockFrom::Frame(*b),
            }));
        }
        Ok(out)
    }

    /// The pages at depth 0 of channel `o`'s `table` that hold entries of `keys`, its directory
    /// pages on the way read through the cache.
    pub(crate) fn pages(
        &self,
        r: &Reading,
        table: &TableIndex,
        o: usize,
        keys: Range<i64>,
    ) -> Result<Vec<Located>, Error> {
        let mut fetch = |offset: u64, len: u32| -> Result<Vec<u8>, Error> {
            let key = Key::Page(o, table.table, offset);
            let entry = r.get_or_make(&self.cache, key, || {
                let _span = tracing::debug_span!("read directory", channel = o, offset).entered();
                let bytes = read(&*self.bytes, offset, len as usize)?;
                let size = list_size(&bytes);
                Ok::<_, Error>((Entry::Bytes(Arc::new(bytes)), size))
            })?;
            match entry {
                Entry::Bytes(b) if b.len() == len as usize => Ok(b.to_vec()),
                _ => Err(misplaced("a directory page")),
            }
        };
        let mut out = Vec::new();
        table.locate(keys, &mut fetch, |at| {
            out.push(at);
            Ok(ControlFlow::Continue(()))
        })?;
        Ok(out)
    }

    /// The blocks on page `at` of finished channel `c`'s block table. Where the channel's list
    /// is in pages not read whole, they are not checked against it here: a page's ticks can span
    /// the whole list. `block` checks each block it decodes against the entries its own ticks
    /// need instead.
    fn block_page(
        &self,
        r: &Reading,
        f: &Finished,
        table: &TableIndex,
        c: usize,
        at: &Located,
    ) -> Result<Arc<Vec<BlockRef>>, Error> {
        let key = Key::Leaf(c, Table::Blocks, at.j);
        let entry = r.get_or_make(&self.cache, key, || {
            let _span = tracing::debug_span!("read page", channel = c, page = at.j).entered();
            let bytes = read(&*self.bytes, at.entry.offset, at.entry.length as usize)?;
            let page = table.parse_page(0, at.j, &at.entry, &bytes)?;
            let refs = f.footer().block_page(c, at, &page, None)?;
            let size = list_size(&refs);
            Ok::<_, Error>((Entry::Blocks(Arc::new(refs)), size))
        })?;
        match entry {
            Entry::Blocks(b) => Ok(b),
            _ => Err(misplaced("a page of blocks")),
        }
    }

    /// Finished overview channel `o`'s level `li`'s rows standing for any of its blocks `b0..b1`.
    fn rows_covering(
        &self,
        r: &Reading,
        f: &Finished,
        o: usize,
        li: usize,
        b0: u64,
        b1: u64,
    ) -> Result<Vec<LevelRef>, Error> {
        let footer = f.footer();
        let (level, table) = footer
            .channels
            .get(o)
            .and_then(|ch| ch.levels.get(li))
            .ok_or(Error::NoChannel(o))?;
        let keys = i64::try_from(b0).unwrap_or(i64::MAX)..i64::try_from(b1).unwrap_or(i64::MAX);
        let mut out = Vec::new();
        for at in self.pages(r, table, o, keys)? {
            let key = Key::Leaf(o, Table::Level(level.shift), at.j);
            let entry = r.get_or_make(&self.cache, key, || {
                let _span = tracing::debug_span!("read page", channel = o, page = at.j).entered();
                let bytes = read(&*self.bytes, at.entry.offset, at.entry.length as usize)?;
                let page = table.parse_page(0, at.j, &at.entry, &bytes)?;
                let rows = footer.level_page(o, li, &at, &page)?;
                let size = list_size(&rows);
                Ok::<_, Error>((Entry::Rows(Arc::new(rows)), size))
            })?;
            let Entry::Rows(rows) = entry else {
                return Err(misplaced("a page of overview rows"));
            };
            let first = rows.partition_point(|x| x.k + u64::from(x.row.n) <= b0);
            let last = rows.partition_point(|x| x.k < b1).max(first);
            out.extend_from_slice(&rows[first..last]);
        }
        Ok(out)
    }

    /// Finished overview channel `o`'s strip, decoded the first time it is asked for.
    fn strip<'s>(&'s self, f: &'s Finished, o: usize) -> Result<&'s Level, Error> {
        let cell = f.strips.get(o).ok_or(Error::NoChannel(o))?;
        if let Some(strip) = cell.get() {
            return Ok(strip);
        }
        let strip = {
            let _span = tracing::debug_span!("decode strip", channel = o).entered();
            f.footer().strip(o)?
        };
        Ok(cell.get_or_init(|| strip))
    }

    /// Channel `c`'s block `at`, decoded.
    pub(crate) fn block(&self, r: &Reading, c: usize, at: &BlockAt) -> Result<Arc<Block>, Error> {
        check(r.cancel)?;
        match at.from {
            #[cfg(feature = "live")]
            BlockFrom::Unwritten(j) => match &self.store {
                Store::Live(live) => live
                    .unwritten
                    .get(c)
                    .and_then(|u| u.get(j))
                    .cloned()
                    .ok_or(Error::NoChannel(c)),
                Store::Finished(_) => unreachable!("a finished file's blocks are written {j}"),
            },
            BlockFrom::Frame(b) => {
                let entry = r.get_or_make(&self.cache, Key::Block(b.row.offset), || {
                    let _span = tracing::debug_span!(
                        "decode block",
                        channel = c,
                        start = b.start,
                        length = b.row.length
                    )
                    .entered();
                    let frame = read(&*self.bytes, b.row.offset, b.row.length as usize)?;
                    let block = match &self.store {
                        // A list in pages not read whole: the entries the block's ticks need.
                        Store::Finished(f) if self.paged(c) => {
                            let list = self.list_over(r, f, c, b.start..b.end)?.list;
                            f.footer().decode_block_in(c, &b, &frame, &list)?
                        }
                        Store::Finished(f) => f.footer().decode_block(c, &b, &frame)?,
                        #[cfg(feature = "live")]
                        Store::Live(live) => live.snapshot.decode_block(c, &b, &frame)?,
                    };
                    let size = block_size(&block);
                    Ok::<_, Error>((Entry::Block(Arc::new(block)), size))
                })?;
                match entry {
                    Entry::Block(b) => Ok(b),
                    _ => Err(misplaced("a block")),
                }
            }
        }
    }

    /// Stored analog channel `c`'s samples `i0..i1` as it stores them, from the blocks that
    /// hold them; `place` is the channel's, exact over those samples.
    fn stored_values(
        &self,
        r: &Reading,
        c: usize,
        place: &Place,
        i0: u64,
        i1: u64,
    ) -> Result<Values, Error> {
        let runs = place.runs().ok_or(Error::NoChannel(c))?;
        let (from, to) = (runs.tick(i0), runs.tick(i1 - 1) + 1);
        let mut out: Option<Values> = None;
        for at in self.blocks_covering(r, c, from, to)? {
            let block = self.block(r, c, &at)?;
            let BlockChannel::Analog {
                first_index,
                values,
            } = &block.data
            else {
                return Err(Error::NoChannel(c));
            };
            let lo = i0.max(*first_index);
            let hi = i1.min(first_index + values.len() as u64);
            if hi <= lo {
                continue;
            }
            let (a, z) = ((lo - first_index) as usize, (hi - first_index) as usize);
            match (out.get_or_insert_with(|| empty_like(values)), values) {
                (Values::F32(o), Values::F32(v)) => o.extend_from_slice(&v[a..z]),
                (Values::F64(o), Values::F64(v)) => o.extend_from_slice(&v[a..z]),
                (Values::U64(o), Values::U64(v)) => o.extend_from_slice(&v[a..z]),
                (Values::I64(o), Values::I64(v)) => o.extend_from_slice(&v[a..z]),
                _ => unreachable!("a channel's blocks store its samples alike"),
            }
        }
        Ok(out.unwrap_or(Values::F64(Vec::new())))
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
        let r = Reading::new(cancel);
        let place = self.place_over(&r, channel, from..to.max(from.saturating_add(1)))?;
        self.edges_in(&r, channel, &place, from, to)
    }

    /// How many edges digital channel `channel` has in [from, to), within the recording. A
    /// finished file's are counted from its index, its page numbers and block words, with only
    /// the blocks holding `from` and `to` decoded; a live recording's edges are read.
    pub fn edge_count(
        &self,
        channel: usize,
        from: i64,
        to: i64,
        cancel: &AtomicBool,
    ) -> Result<u64, Error> {
        let r = Reading::new(cancel);
        match self.places.get(channel) {
            Some(Place::Digital { .. }) => {}
            Some(Place::Unavailable(why)) => {
                return Err(Error::Unavailable {
                    channel,
                    why: why.clone(),
                });
            }
            _ => return Err(Error::NoChannel(channel)),
        }
        let (from, to) = (from.max(0), to.min(self.length));
        if to <= from {
            return Ok(0);
        }
        if self.finished().is_none() {
            let place = self.place_over(&r, channel, from..to)?;
            return Ok(self.edges_in(&r, channel, &place, from, to)?.1.len() as u64);
        }
        Ok(self.edges_before(&r, channel, to)? - self.edges_before(&r, channel, from)?)
    }

    /// How many of finished digital channel `c`'s edges lie before tick `t`.
    fn edges_before(&self, r: &Reading, c: usize, t: i64) -> Result<u64, Error> {
        if t <= 0 {
            return Ok(0);
        }
        if t >= self.length {
            let f = self.finished().ok_or(Error::NoChannel(c))?;
            return match f.footer().header.channels.get(c).map(|ch| &ch.data) {
                Some(HeaderKind::Digital { edges, .. }) => Ok(*edges),
                _ => Err(Error::NoChannel(c)),
            };
        }
        let covering = self.blocks_covering(r, c, t, t + 1)?;
        let Some(at) = covering.first() else {
            return Err(
                DecodeError::Corrupt(format!("channel {c}: no block holds tick {t}")).into(),
            );
        };
        // A finished file's blocks are all in it.
        #[cfg(feature = "live")]
        let BlockFrom::Frame(b) = at.from else {
            return Err(Error::NoChannel(c));
        };
        #[cfg(not(feature = "live"))]
        let BlockFrom::Frame(b) = at.from;
        if b.start == t {
            return Ok(b.first_index);
        }
        let block = self.block(r, c, at)?;
        let BlockChannel::Digital { edges, .. } = &block.data else {
            return Err(Error::NoChannel(c));
        };
        Ok(b.first_index + edges.partition_point(|&e| e < t) as u64)
    }

    /// `edges`, `place` the channel's, exact over [from, to).
    pub(crate) fn edges_in(
        &self,
        r: &Reading,
        channel: usize,
        place: &Place,
        from: i64,
        to: i64,
    ) -> Result<(bool, Vec<i64>), Error> {
        let Place::Digital {
            gaps,
            leading,
            initial,
            ..
        } = place
        else {
            return Err(Error::NoChannel(channel));
        };
        let from = from.clamp(0, self.length.max(1) - 1);
        let to = to.min(self.length);
        let covering = self.blocks_covering(r, channel, from, to.max(from + 1))?;
        let mut level = *initial;
        let mut out = Vec::new();
        for (k, at) in covering.iter().enumerate() {
            let block = self.block(r, channel, at)?;
            let BlockChannel::Digital { initial, edges, .. } = &block.data else {
                return Err(Error::NoChannel(channel));
            };
            let lo = edges.partition_point(|&t| t < from);
            if k == 0 {
                level = *initial ^ (lo % 2 == 1);
                // A block's level bit carries the level from before a gap until the gap's end, which
                // sets it: the last gap ending in the block, at or before `from`. A gap ending at
                // the block's start is in the block. The gap from tick 0 comes before the others.
                let start = block.start;
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

    /// Analog or derived channel `channel`'s sample values, samples `i0..i1`, as far as the
    /// blocks that hold them decode; `place` the channel's, exact over them.
    pub(crate) fn values(
        &self,
        r: &Reading,
        channel: usize,
        place: &Place,
        i0: u64,
        i1: u64,
    ) -> Result<Vec<f64>, Error> {
        let Place::Analog { rule, .. } = place else {
            return Err(Error::NoChannel(channel));
        };
        let rule = *rule;
        let n = i1.saturating_sub(i0);
        // The count is the header's, which a block that does not decode can't back: up front it
        // costs at most 128 MB before the blocks refuse it, and past that the values grow with
        // what the blocks give. A view's samples are fewer than that by far.
        if n <= SAMPLES_UP_FRONT {
            let mut out = vec![f64::NAN; n as usize];
            self.each_stored(r, channel, place, i0, i1, |first, values, range| {
                fill(&mut out[(first - i0) as usize..], values, range, rule);
                Ok(())
            })?;
            return Ok(out);
        }
        let mut out = Vec::new();
        self.each_stored(r, channel, place, i0, i1, |first, values, range| {
            // Stretches come in index order; a sample the blocks skip stays NaN, as up front.
            let at = (first - i0) as usize;
            out.resize(at + range.len(), f64::NAN);
            fill(&mut out[at..], values, range, rule);
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
        f: impl FnMut(u64, f64, Option<i128>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let place = self.place(channel)?;
        self.each_sample_in(&Reading::new(cancel), channel, place, i0, i1, f)
    }

    /// `each_sample`, `place` the channel's, exact over samples `i0..i1`.
    pub(crate) fn each_sample_in(
        &self,
        r: &Reading,
        channel: usize,
        place: &Place,
        i0: u64,
        i1: u64,
        mut f: impl FnMut(u64, f64, Option<i128>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let Place::Analog { rule, .. } = place else {
            return Err(Error::NoChannel(channel));
        };
        let rule = *rule;
        self.each_stored(r, channel, place, i0, i1, |first, values, range| {
            each_value(values, range, first, rule, &mut f)
        })
    }

    /// Calls `f` with each stretch of samples `i0..i1` of analog or derived channel `channel` as
    /// it is stored, in index order: the index of its first sample, the samples that hold it, and
    /// where in them it lies. A stored channel's stretches are its blocks' samples, not copied.
    fn each_stored(
        &self,
        r: &Reading,
        channel: usize,
        place: &Place,
        i0: u64,
        i1: u64,
        mut f: impl FnMut(u64, &Values, Range<usize>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let Place::Analog { count, .. } = place else {
            return Err(Error::NoChannel(channel));
        };
        let i1 = i1.min(*count);
        if i1 <= i0 {
            return Ok(());
        }
        let stored = self.places.len() - self.products.len();
        if channel < stored {
            let runs = place.runs().ok_or(Error::NoChannel(channel))?;
            let (from, to) = (runs.tick(i0), runs.tick(i1 - 1) + 1);
            for at in self.blocks_covering(r, channel, from, to)? {
                let block = self.block(r, channel, &at)?;
                let BlockChannel::Analog {
                    first_index,
                    values,
                } = &block.data
                else {
                    return Err(Error::NoChannel(channel));
                };
                let lo = i0.max(*first_index);
                let hi = i1.min(first_index + values.len() as u64);
                if hi > lo {
                    f(
                        lo,
                        values,
                        (lo - first_index) as usize..(hi - first_index) as usize,
                    )?;
                }
            }
            return Ok(());
        }
        let values = {
            // A derived channel's samples are its operands' at the same indices, which share its
            // breaks: their places over the samples' ticks.
            let (a, b, product) = self.product(channel - stored)?;
            let runs = place.runs().ok_or(Error::NoChannel(channel))?;
            let ticks = runs.tick(i0)..runs.tick(i1 - 1).saturating_add(1);
            let (pa, pb) = (
                self.place_over(r, a, ticks.clone())?,
                self.place_over(r, b, ticks)?,
            );
            let (va, vb) = (
                self.stored_values(r, a, &pa, i0, i1)?,
                self.stored_values(r, b, &pb, i0, i1)?,
            );
            samples::product_values(product, &va, &vb).map_err(|what| {
                DecodeError::Corrupt(format!(
                    "derived channel {}: {what}",
                    self.channels[channel].name
                ))
            })?
        };
        f(i0, &values, 0..values.len())
    }

    /// Channel `c`'s coarsest level whose buckets are no wider than `ticks`, of those with any
    /// bucket.
    pub(crate) fn level_for(&self, c: usize, ticks: f64) -> Option<usize> {
        self.levels
            .get(c)?
            .iter()
            .rposition(|l| ((1u64 << l.shift) as f64) <= ticks && l.covered > 0)
    }

    /// Channel `c`'s level `level`'s buckets that hold any of `k0..k1`, from each place that
    /// holds them, in order: each with the level's index of its first bucket.
    pub(crate) fn pieces(
        &self,
        r: &Reading,
        c: usize,
        level: usize,
        k0: u64,
        k1: u64,
    ) -> Result<Vec<(u64, Piece<'_>)>, Error> {
        let l = &self.levels[c][level];
        let k1 = k1.min(l.covered);
        let mut out = Vec::new();
        if k1 <= k0 {
            return Ok(out);
        }
        // A run's block stands for each of its `n` blocks: those that hold any of `k0..k1`.
        let push = |block: Arc<OverviewBlock>, out: &mut Vec<(u64, Piece<'_>)>| {
            let (first, n) = (block.first, u64::from(block.n));
            let a = k0.saturating_sub(first) / BLOCK_BUCKETS;
            let z = (k1.saturating_sub(first)).div_ceil(BLOCK_BUCKETS).min(n);
            for j in a..z {
                out.push((first + j * BLOCK_BUCKETS, Piece::Block(Arc::clone(&block))));
            }
        };
        let (b0, b1) = (k0 / BLOCK_BUCKETS, (k1 - 1) / BLOCK_BUCKETS);
        for part in &l.parts {
            match (*part, &self.store) {
                (Part::Strip, Store::Finished(f)) => {
                    out.push((0, Piece::Borrowed(self.strip(f, c)?)));
                }
                (Part::Blocks(li), Store::Finished(f)) => {
                    for row in self.rows_covering(r, f, c, li, b0, b1 + 1)? {
                        let block = self.overview_block(r, &row, |frame| {
                            Ok(f.footer().decode_overview(c, li, &row, frame)?)
                        })?;
                        push(block, &mut out);
                    }
                }
                #[cfg(feature = "live")]
                (Part::LiveBlocks(li), Store::Live(live)) => {
                    let rows = &live.rows[c][li];
                    let first = rows.partition_point(|r| r.k + u64::from(r.row.n) <= b0);
                    let last = rows.partition_point(|r| r.k <= b1).max(first);
                    for row in &rows[first..last] {
                        let block = self.overview_block(r, row, |frame| {
                            Ok(live.snapshot.decode_overview(c, li, row, frame)?)
                        })?;
                        push(block, &mut out);
                    }
                    for block in &live.unwritten_overview[c][li] {
                        let end = block.first + u64::from(block.n) * BLOCK_BUCKETS;
                        if block.first < k1 && end > k0 {
                            push(Arc::clone(block), &mut out);
                        }
                    }
                }
                #[cfg(feature = "live")]
                (Part::Held(li), Store::Live(live)) => {
                    let ladder = live
                        .snapshot
                        .overview(c)
                        .expect("an overview for every channel");
                    let held = &ladder.levels[li].held;
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
        r: &Reading,
        row: &LevelRef,
        decode: impl FnOnce(&[u8]) -> Result<OverviewBlock, Error>,
    ) -> Result<Arc<OverviewBlock>, Error> {
        check(r.cancel)?;
        let (offset, length) = (row.row.offset, row.row.length);
        let entry = r.get_or_make(&self.cache, Key::Overview(offset), || {
            let _span = tracing::debug_span!("decode overview block", offset, length).entered();
            let frame = read(&*self.bytes, offset, length as usize)?;
            let block = decode(&frame)?;
            let size = format_size(&block.level);
            Ok::<_, Error>((Entry::Overview(Arc::new(block)), size))
        })?;
        match entry {
            Entry::Overview(b) => Ok(b),
            _ => Err(misplaced("an overview block")),
        }
    }

    /// Analog or derived channel `channel`'s buckets `k0..k1` of level `level`, as values.
    /// Buckets past what the level holds read as empty.
    pub(crate) fn analog_buckets(
        &self,
        r: &Reading,
        channel: usize,
        level: usize,
        k0: u64,
        k1: u64,
    ) -> Result<AnalogBuckets, Error> {
        // The rule alone, which a place holds whether or not its list is read.
        let Some(Place::Analog { rule, .. }) = self.places.get(channel) else {
            return Err(Error::NoChannel(channel));
        };
        let n = k1.saturating_sub(k0) as usize;
        let mut out = AnalogBuckets {
            min: vec![f64::INFINITY; n],
            max: vec![f64::NEG_INFINITY; n],
            sums: rule.exact().map(|_| vec![0; n]),
        };
        for (first, piece) in self.pieces(r, channel, level, k0, k1)? {
            let level = piece.level();
            let Some(a) = level.data.analog() else {
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
        r: &Reading,
        channel: usize,
        level: usize,
        k0: u64,
        k1: u64,
    ) -> Result<(Vec<bool>, Vec<bool>), Error> {
        let Some(Place::Digital { .. }) = self.places.get(channel) else {
            return Err(Error::NoChannel(channel));
        };
        let n = k1.saturating_sub(k0) as usize;
        let (mut levels, mut changes) = (vec![false; n], vec![false; n]);
        for (first, piece) in self.pieces(r, channel, level, k0, k1)? {
            let level = piece.level();
            let c = &level.data;
            if !matches!(c, ChannelOverview::Digital { .. }) {
                return Err(Error::NoChannel(channel));
            }
            for k in k0.max(first)..k1.min(first + level.buckets) {
                let (l, ch) = level_and_change(c.digital(k - first).expect("digital"));
                levels[(k - k0) as usize] = l;
                changes[(k - k0) as usize] = ch;
            }
        }
        Ok((levels, changes))
    }

    /// The index of the first sample at or after `tick` of analog or derived channel `channel`.
    pub fn index_at(&self, channel: usize, tick: i64) -> Result<u64, Error> {
        let r = Reading::new(&NEVER);
        let place = self.place_over(&r, channel, tick..tick.saturating_add(1))?;
        index_in(channel, &place, tick)
    }
}

/// A cancel flag nothing raises.
static NEVER: AtomicBool = AtomicBool::new(false);

/// The index of the first sample at or after `tick` of analog or derived channel `channel`,
/// `place` its place, exact at `tick`.
pub(crate) fn index_in(channel: usize, place: &Place, tick: i64) -> Result<u64, Error> {
    let runs = place.runs().ok_or(Error::NoChannel(channel))?;
    Ok(index_at_or_after(&runs, tick as f64))
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

/// A digital bucket as the view draws it until it draws density and duty (phase 6): whether its
/// level changes inside it, from an edge or a gap's end, and its level where it holds still. A
/// busy bucket's level says nothing; one wholly outside the kept span reads low.
pub(crate) fn level_and_change(b: DigitalBucket) -> (bool, bool) {
    let busy = b.density > 0 || !matches!(b.duty, 0 | 14 | DigitalBucket::OUTSIDE);
    (b.duty >= 7 && b.duty != DigitalBucket::OUTSIDE, busy)
}

/// About the bytes a decoded overview level holds.
fn format_size(level: &Level) -> usize {
    64 + match &level.data {
        ChannelOverview::Digital { density, duty } => density.len() + duty.len(),
        ChannelOverview::Analog(a) => {
            a.len()
                * match a {
                    AnalogOverview::F32(_) => 16,
                    AnalogOverview::F64(_) => 24,
                    AnalogOverview::U64(_) | AnalogOverview::I64(_) => 32,
                }
        }
    }
}

/// An empty list of samples stored as `like`'s are.
fn empty_like(like: &Values) -> Values {
    match like {
        Values::F32(_) => Values::F32(Vec::new()),
        Values::F64(_) => Values::F64(Vec::new()),
        Values::U64(_) => Values::U64(Vec::new()),
        Values::I64(_) => Values::I64(Vec::new()),
    }
}

/// `values[range]` under `rule` into `out`, as many as both hold. One match on how the samples are
/// stored, rather than one a sample, and a loop the compiler can vectorise.
fn fill(out: &mut [f64], values: &Values, range: Range<usize>, rule: Rule) {
    match values {
        Values::F32(v) => {
            for (o, &x) in out.iter_mut().zip(&v[range]) {
                *o = rule.value(Sample::F32(x));
            }
        }
        Values::F64(v) => {
            for (o, &x) in out.iter_mut().zip(&v[range]) {
                *o = rule.value(Sample::F64(x));
            }
        }
        Values::U64(v) => {
            for (o, &x) in out.iter_mut().zip(&v[range]) {
                *o = rule.value(Sample::U64(x));
            }
        }
        Values::I64(v) => {
            for (o, &x) in out.iter_mut().zip(&v[range]) {
                *o = rule.value(Sample::I64(x));
            }
        }
    }
}

/// Calls `f` with each of `values[range]`, the first sample `first`: its index, its value under
/// `rule`, and its code where it is an integer. One match on how the samples are stored, rather
/// than one a sample.
fn each_value(
    values: &Values,
    range: Range<usize>,
    first: u64,
    rule: Rule,
    mut f: impl FnMut(u64, f64, Option<i128>) -> Result<(), Error>,
) -> Result<(), Error> {
    match values {
        Values::F32(v) => (first..)
            .zip(&v[range])
            .try_for_each(|(i, &x)| f(i, rule.value(Sample::F32(x)), None)),
        Values::F64(v) => (first..)
            .zip(&v[range])
            .try_for_each(|(i, &x)| f(i, rule.value(Sample::F64(x)), None)),
        Values::U64(v) => (first..)
            .zip(&v[range])
            .try_for_each(|(i, &x)| f(i, rule.value(Sample::U64(x)), Some(i128::from(x)))),
        Values::I64(v) => (first..)
            .zip(&v[range])
            .try_for_each(|(i, &x)| f(i, rule.value(Sample::I64(x)), Some(i128::from(x)))),
    }
}
