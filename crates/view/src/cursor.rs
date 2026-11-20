//! Exact cursors over a recording's stored channels, for a decoder: a digital channel's edges and
//! where its level becomes known or unknown, an analog channel's samples, run starts and losses,
//! in source order, each with its source tick and its place among the channel's items. A cursor
//! reads a batch at a time before a bound and resumes by ordinal, so two edges on one axis tick
//! are neither lost nor repeated. Blocks and pages come through the recording's own reads and
//! cache, so a block a cursor decodes is the one a view draws from, and a channel's gaps or
//! breaks in list pages are read a page at a time. `Merge` puts several channels' observations
//! in one exact order.
//!
//! A cursor borrows its recording for each call, and reads only the recording it was made from:
//! given another, a call returns `Error::OtherRecording`. On a live recording it
//! carries on across `Recording::update` and reports only what is settled: nothing its writer
//! can still add comes before it. A writer can report a gap after the tick it starts at, and the
//! samples lost before a run once the run starts, so that can lag the channel's promise.

mod analog;
mod digital;
mod exact;
mod lists;
mod merge;

pub use analog::AnalogCursor;
pub use digital::{DigitalCursor, Stretch, timeline_map, whole_line};
pub use exact::Exact;
pub use merge::{Group, Groups, Merge, PortCursor};

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use escapement_tickwave_format::decode::{Block, BlockChannel, DecodeError};
use escapement_tickwave_format::model::Sample;

use crate::cache::Reading;
use crate::recording::{BlockAt, BlockFrom, Error, Place, Recording, Store};

/// What one call may take: at most `items` observations, and no block that decodes to more than
/// `block_bytes`. A live recording's blocks not yet written are decoded already, and pass any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub items: usize,
    pub block_bytes: u32,
}

impl Budget {
    pub const LARGE: Budget = Budget {
        items: usize::MAX,
        block_bytes: u32::MAX,
    };
}

/// Where a batch stops: before an axis tick, or before a tick on the source's timeline, a source
/// tick moved by its channel's offset. Ports on one source share its timeline, where two source
/// ticks never share a tick as they can share an axis tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Until {
    Axis(i64),
    Source(i64),
}

impl Until {
    pub fn limit(self) -> i64 {
        match self {
            Until::Axis(t) | Until::Source(t) => t,
        }
    }

    /// An observation's position in these terms.
    fn key(self, axis: i64, source_tick: i64, offset: i64) -> i64 {
        match self {
            Until::Axis(_) => axis,
            Until::Source(_) => source_tick.saturating_add(offset),
        }
    }

    /// The bound a batch stops at: the caller's, or a live channel's settled bound where that
    /// comes first.
    fn cap(self, settled: Option<Settled>, offset: i64) -> i64 {
        let limit = self.limit();
        match (self, settled) {
            (_, None) => limit,
            (Until::Axis(_), Some(s)) => limit.min(s.axis),
            (Until::Source(_), Some(s)) => limit.min(s.source.saturating_add(offset)),
        }
    }
}

/// What a call did: how many observations it appended, and the tick before which every
/// observation has been reported, in the terms it was asked in. Observations at that tick may
/// remain. Short of the bound with none taken, a live recording is settled no further yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Batch {
    pub taken: usize,
    pub reached: i64,
}

/// One edge: its ordinal on the channel, its source tick, the axis tick that maps to, and the
/// level after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub ordinal: u64,
    pub source_tick: i64,
    pub axis: i64,
    pub level: bool,
}

/// One analog sample: its index, its place in `1/d` source ticks and the whole source tick at or
/// before it, the axis tick that maps to, and its value as stored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sampled {
    pub index: u64,
    pub place: i128,
    pub source_tick: i64,
    pub axis: i64,
    pub value: Sample,
}

/// What a cursor reports, in source order. A digital channel's edges, and its level becoming
/// known or unknown; an analog channel's samples, run starts and lost samples. An analog
/// observation's `source_tick` is the whole tick at or before its place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seen {
    Edge(Edge),
    /// The level is known from here: the channel's start, or a gap's end.
    Known {
        source_tick: i64,
        axis: i64,
        level: bool,
    },
    /// The level is not known from here: a gap's start, or a finished file's channel's end.
    Unknown {
        source_tick: i64,
        axis: i64,
    },
    Sample(Sampled),
    /// Sample `index` starts a new run at `place`; `lost` samples were lost before it.
    Break {
        index: u64,
        place: i128,
        source_tick: i64,
        axis: i64,
        lost: u64,
    },
    /// `count` samples the instrument lost, where they would have sat: places `from..to`
    /// (FORMAT.md, "Gaps"). Before the first sample, at a break, or after the last.
    Lost {
        from: i128,
        to: i128,
        source_tick: i64,
        axis: i64,
        to_axis: i64,
        count: u64,
    },
}

impl Seen {
    pub fn axis(&self) -> i64 {
        match *self {
            Seen::Edge(e) => e.axis,
            Seen::Sample(s) => s.axis,
            Seen::Known { axis, .. }
            | Seen::Unknown { axis, .. }
            | Seen::Break { axis, .. }
            | Seen::Lost { axis, .. } => axis,
        }
    }

    pub fn source_tick(&self) -> i64 {
        match *self {
            Seen::Edge(e) => e.source_tick,
            Seen::Sample(s) => s.source_tick,
            Seen::Known { source_tick, .. }
            | Seen::Unknown { source_tick, .. }
            | Seen::Break { source_tick, .. }
            | Seen::Lost { source_tick, .. } => source_tick,
        }
    }
}

/// Where a live channel is settled to: nothing its writer can still add lies before axis tick
/// `axis`, or before source tick `source`. A finished file's channel has none: it is settled
/// throughout.
#[derive(Clone, Copy, Debug)]
struct Settled {
    axis: i64,
    source: i64,
}

/// A block a cursor holds: where it lies on the axis, its first item's index, and the block. It
/// holds exactly the items its row counts.
#[derive(Clone)]
struct Held {
    start: i64,
    end: i64,
    first: u64,
    block: Arc<Block>,
}

fn corrupt(what: String) -> Error {
    DecodeError::Corrupt(what).into()
}

/// Stored channel `c`'s place, for its kind and the facts the recording states of it.
fn place_of(rec: &Recording, c: usize) -> Result<&Place, Error> {
    let stored = rec.places.len().saturating_sub(rec.products.len());
    match rec.places.get(c) {
        Some(Place::Unavailable(why)) => Err(Error::Unavailable {
            channel: c,
            why: why.clone(),
        }),
        Some(place) if c < stored => Ok(place),
        _ => Err(Error::NoChannel(c)),
    }
}

/// Channel `c`'s source, by its index among the recording's sources.
fn source_of(rec: &Recording, c: usize) -> u32 {
    match &rec.store {
        Store::Finished(f) => f.footer().header.channels.get(c).map_or(0, |ch| ch.source),
        #[cfg(feature = "live")]
        Store::Live(live) => live
            .snapshot
            .declaration()
            .channels
            .get(c)
            .map_or(0, |ch| ch.source),
    }
}

/// Live channel `c`'s promise: every item of it before this axis tick is in. None for a finished
/// file's channel.
#[cfg_attr(
    not(feature = "live"),
    expect(
        unused_variables,
        reason = "only a live recording's channels have promises"
    )
)]
fn promise_of(rec: &Recording, c: usize) -> Result<Option<i64>, Error> {
    match &rec.store {
        Store::Finished(_) => Ok(None),
        #[cfg(feature = "live")]
        Store::Live(live) => {
            let ch = live.snapshot.channel(c).ok_or(Error::NoChannel(c))?;
            Ok(Some(ch.promise))
        }
    }
}

/// Channel `c`'s block holding axis tick `t`.
fn block_at(
    rec: &Recording,
    c: usize,
    t: i64,
    cancel: &AtomicBool,
) -> Result<Option<BlockAt>, Error> {
    let r = Reading::new(cancel);
    Ok(rec
        .blocks_covering(&r, c, t, t.saturating_add(1))?
        .into_iter()
        .next())
}

/// Block `at`'s first item's index and how many items it holds: a written block's from its row,
/// with nothing decoded.
#[cfg_attr(
    not(feature = "live"),
    expect(
        unused_variables,
        reason = "only a live recording's blocks can be unwritten"
    )
)]
fn items_of(
    rec: &Recording,
    c: usize,
    at: &BlockAt,
    digital: bool,
    cancel: &AtomicBool,
) -> Result<(u64, u64), Error> {
    match at.from {
        BlockFrom::Frame(b) => Ok((b.first_index, u64::from(b.row.items(digital)))),
        #[cfg(feature = "live")]
        BlockFrom::Unwritten(j) => {
            let first = match &rec.store {
                Store::Live(live) => live
                    .snapshot
                    .channel(c)
                    .and_then(|ch| ch.unwritten.get(j))
                    .map(|u| u.first_index),
                Store::Finished(_) => None,
            }
            .ok_or(Error::NoChannel(c))?;
            let block = rec.block(&Reading::new(cancel), c, at)?;
            Ok((first, block.data.len() as u64))
        }
    }
}

/// Channel `c`'s block `at`, decoded, or refused where it decodes to more than `budget` allows.
fn decode(
    rec: &Recording,
    c: usize,
    at: &BlockAt,
    budget: &Budget,
    cancel: &AtomicBool,
) -> Result<Arc<Block>, Error> {
    match at.from {
        BlockFrom::Frame(b) if b.row.decoded_length > budget.block_bytes => {
            return Err(Error::NeedsBudget {
                channel: c,
                at: b.start,
                block_bytes: b.row.decoded_length,
            });
        }
        _ => {}
    }
    rec.block(&Reading::new(cancel), c, at)
}

/// Channel `c`'s block `at`, whose first item and count are `items`, decoded and checked to hold
/// those items, of the channel's kind.
fn hold(
    rec: &Recording,
    c: usize,
    at: &BlockAt,
    (first, n): (u64, u64),
    digital: bool,
    budget: &Budget,
    cancel: &AtomicBool,
) -> Result<Held, Error> {
    let block = decode(rec, c, at, budget, cancel)?;
    let fits = match &block.data {
        BlockChannel::Digital { edges, source, .. } if digital => {
            edges.len() as u64 == n && source.as_ref().is_none_or(|s| s.len() == edges.len())
        }
        BlockChannel::Analog {
            first_index,
            values,
        } if !digital => values.len() as u64 == n && *first_index == first,
        _ => false,
    };
    if !fits {
        return Err(corrupt(format!(
            "channel {c}: the block at tick {} holds other items than its row says",
            at.start
        )));
    }
    Ok(Held {
        start: at.start,
        end: at.end,
        first,
        block,
    })
}

/// The block of channel `c` holding item `next`, walking on from the block holding axis tick
/// `from`, at or before it; None past the channel's last block.
fn walk(
    rec: &Recording,
    c: usize,
    digital: bool,
    from: i64,
    next: u64,
    budget: &Budget,
    cancel: &AtomicBool,
) -> Result<Option<Held>, Error> {
    let mut t = from;
    loop {
        crate::read::check(cancel)?;
        let Some(at) = block_at(rec, c, t, cancel)? else {
            return Ok(None);
        };
        let (first, n) = items_of(rec, c, &at, digital, cancel)?;
        if next < first {
            return Err(corrupt(format!(
                "channel {c}: item {next} lies in no block, the block at tick {} starting at item \
                 {first}",
                at.start
            )));
        }
        if next - first < n {
            return hold(rec, c, &at, (first, n), digital, budget, cancel).map(Some);
        }
        // A block covering `t` ends past it, so the walk moves on.
        t = at.end;
    }
}

/// A live channel's last block, for a cursor placed past every block. None for a finished file.
#[cfg_attr(
    not(feature = "live"),
    expect(
        unused_variables,
        reason = "only a live recording's blocks are held whole"
    )
)]
fn last_block(rec: &Recording, c: usize) -> Option<BlockAt> {
    match &rec.store {
        Store::Finished(_) => None,
        #[cfg(feature = "live")]
        Store::Live(_) => rec.blocks.get(c)?.last().copied(),
    }
}
