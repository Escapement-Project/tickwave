//! The writer: a recording's items in, the file's bytes out, with no I/O of its own.
//!
//! A caller declares the recording, then hands in each channel's items in that channel's order,
//! in its source's ticks, and promises as it goes that every item before a tick is in. Each
//! channel's block closes at a promise once it reaches its size, or at `flush`, which a live
//! recorder calls on its own clock and which writes a known-through record. Frames are
//! compressed a slice at a time over the calls that follow, on the caller's thread, or on other
//! threads by option. The caller drains the bytes as they come: `output` and `consume`, or
//! `take_output`. `snapshot` gives a view in the same process what is held so far. `finish`
//! closes each channel's last block and writes each generator's last annotation part, the last
//! pages, the annotation bundle where it took any annotation, the footer and the trailer.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use crate::annotations::View;
use crate::codec::put_uleb;
use crate::compress::{SLICE, Slicer, Slot, compress};
use crate::decode::{check_declaration, level_after, lost_after_end, lost_before_start};
use crate::hash::{self, ChannelHasher, ListHasher};
use crate::header::{
    AnalogCoding, BlockRow, ChannelHeader, ChannelKind, ContentId, Declaration, DeclaredKind,
    DerivedHeader, Header, INLINE_LIST, JSON_SAFE, LEVEL_BIT, OverviewLevel, OverviewRow, Preamble,
    code_value,
};
use crate::index::{Due, Entry, Row, Table, TableIndex, TableWriter};
use crate::mapping::Mapping;
use crate::model::{Break, Gap, Grid, Sample};
use crate::overview::{self, Column, Ladder, Pair, Settled, Unpaired};
use crate::parts::{Part, PartRow, placed_span};
use crate::record::{self, BlockHead, Kind, MAX_BLOCK, MAX_GENERATORS, MAX_PART, OverviewHead};
use crate::samples;
use crate::snapshot::{
    Annotations, ChannelSnapshot, LevelSnapshot, ListSnapshot, OverviewSnapshot, Snapshot,
    Unwritten,
};

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("{what} {value} does not fit the header, which holds integers below 2^53")]
    TooLarge { what: &'static str, value: u64 },
    #[error("the block size target must lie between 1 byte and 2 MiB, half the most a block holds")]
    BlockSize,
    #[error("the declaration: {0}")]
    Declaration(String),
    #[error("no channel {0}")]
    NoChannel(usize),
    #[error("{channel} is {kind}")]
    Kind { channel: String, kind: &'static str },
    #[error("{channel}: an item at tick {tick} comes before tick {previous}, which is already in")]
    Order {
        channel: String,
        tick: i64,
        previous: i64,
    },
    #[error(
        "{channel}: an item at axis tick {tick} comes before tick {promise}, before which every \
         item was promised in"
    )]
    BeforePromise {
        channel: String,
        tick: i64,
        promise: i64,
    },
    #[error("a promise or flush at tick {before} comes before the latest, at {last}")]
    Promise { before: i64, last: i64 },
    #[error("{channel}: an edge at tick {tick} lies inside the gap to {to}")]
    EdgeInGap { channel: String, tick: i64, to: i64 },
    #[error("{channel}: a gap from {from} to {to} is empty or runs backwards")]
    GapSpan { channel: String, from: i64, to: i64 },
    #[error(
        "{channel}: a gap from {from} does not lie past the channel's last edge or gap, {last}"
    )]
    GapOrder {
        channel: String,
        from: i64,
        last: i64,
    },
    #[error("{channel}: a gap from the channel's start needs an initial level of false")]
    GapFromStart { channel: String },
    #[error("{channel}: source tick {tick} lies outside 0 to 2^53")]
    SourceTick { channel: String, tick: i64 },
    #[error("{channel}: source tick {tick} maps outside the axis's 0 to 2^53")]
    AxisTick { channel: String, tick: i64 },
    #[error("{channel}: an item at source tick {tick} comes before the channel's start, {start}")]
    BeforeStart {
        channel: String,
        tick: i64,
        start: i64,
    },
    #[error(
        "{channel}: a gap ending at {to} comes after the block that holds it, which ended at {closed}"
    )]
    GapLate {
        channel: String,
        to: i64,
        closed: i64,
    },
    #[error(
        "{channel}: a run at tick {tick} starts before the channel's last sample, at {previous}"
    )]
    RunOrder {
        channel: String,
        tick: i64,
        previous: i64,
    },
    #[error(
        "{channel}: a run at tick {tick} states {lost} lost samples, but starts no later than \
         where the first of them would sit"
    )]
    LostRunStart {
        channel: String,
        tick: i64,
        lost: u64,
    },
    #[error(
        "{channel}: a run's phase of {phase} is not below the channel's grid, {d}, or the first \
         run's is not the declared one"
    )]
    RunPhase { channel: String, phase: u32, d: u64 },
    #[error("{channel}: a sample before the channel's first run")]
    NoRun { channel: String },
    #[error(
        "{channel}: the {lost} samples lost before the first run would start before source or \
         axis tick 0"
    )]
    LostBefore { channel: String, lost: u64 },
    #[error(
        "{channel}: the {lost} samples lost after the last would end past the length, or the \
         channel has no sample"
    )]
    LostAfter { channel: String, lost: u64 },
    #[error("channel {0} is not an analog channel, so it loses no samples after its last")]
    LostAfterChannel(usize),
    #[error("{channel}: sample {index}: {why}")]
    Sample {
        channel: String,
        index: u64,
        why: String,
    },
    #[error(transparent)]
    Derived(#[from] Unpaired),
    #[error("{channel}: sample {index}, {value}, is not exactly a code of {coding:?}")]
    Coding {
        channel: String,
        index: u64,
        value: f32,
        coding: AnalogCoding,
    },
    #[error("a flush at tick {before} comes before the latest promise or flush, at {last}")]
    Flush { before: i64, last: i64 },
    #[error("a block of {0} items or bytes, more than its index word or length holds")]
    BlockTooLarge(usize),
    #[error(
        "{channel}: an item at axis tick {tick} would take its open block past 4 MiB, and the \
         block can't close before it: promise or flush past the items it holds"
    )]
    BlockFull { channel: String, tick: i64 },
    #[error(
        "{channel}: a sample would leave more than 4 MiB of its samples waiting for {partner}'s, \
         with which derived channel {derived} pairs it"
    )]
    OperandAhead {
        channel: String,
        partner: String,
        derived: String,
    },
    #[error(
        "{channel}: a gap would leave more than 4 MiB of gaps waiting for the blocks that hold \
         their ends: promise or flush past them"
    )]
    GapsWaiting { channel: String },
    #[error("the length {length} does not lie past every item and block in, the last at {last}")]
    Length { length: i64, last: i64 },
    #[error("{given} digital channel ends given, {expected} expected")]
    Ends { given: usize, expected: usize },
    #[error(
        "{channel}: its end, source tick {end}, lies outside its start to the length, {length}"
    )]
    End {
        channel: String,
        end: i64,
        length: i64,
    },
    #[error("{channel}: an edge at tick {tick} lies at or past the channel's end, {end}")]
    EdgePastEnd {
        channel: String,
        tick: i64,
        end: i64,
    },
    #[error("{channel}: the gap to {to} lies inside the recording but not before the end, {end}")]
    GapPastEnd { channel: String, to: i64, end: i64 },
    #[error(
        "{channel}: the end, source tick {end}, leaves out the gap from {from} to {to}, which is \
         already in the content id and the overview"
    )]
    EndLeavesOutGap {
        channel: String,
        from: i64,
        to: i64,
        end: i64,
    },
    #[error("source {index}'s extra already holds {key}")]
    Extra { index: usize, key: String },
    #[error("extra keys for {given} sources, but the recording has {sources}")]
    Extras { given: usize, sources: usize },
    /// A caller's annotation that would not resolve in a sidecar: which, and the rule it breaks.
    #[error("{0}")]
    Annotation(String),
    /// The annotation bundle the finish would write: none asked for where the writer took
    /// annotations, or files it can't split into parts.
    #[error("the annotation bundle: {0}")]
    Bundle(String),
    #[error("the footer is {0} bytes, more than its u32 length holds")]
    FooterTooLarge(usize),
    #[error("the header would not read back: {0}")]
    Header(String),
    #[error("zstd: {0}")]
    Zstd(#[from] std::io::Error),
}

/// The overview blocks a ladder settles before the writer compresses what waits: a call that
/// settles a long span holds this many at a time.
const SETTLE_STEP: usize = 16;

/// The zstd level overview blocks are compressed at, whatever `WriteOptions::level`.
pub const OVERVIEW_BLOCK_LEVEL: i32 = 3;

/// The most a writer's block size target can be: half the most a block holds, so a block can
/// take the items that come before it can close (FORMAT.md, "tickwave's writer").
pub const MAX_TARGET: usize = MAX_BLOCK / 2;

/// The most bytes of gaps a channel holds waiting for the blocks that hold their ends, at 17
/// bytes each.
pub const MAX_WAITING_GAPS: usize = 4 << 20;

/// Where frames are compressed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compression {
    /// On the caller's thread, a slice of at most 8 KiB of decoded bytes a call, so no call does
    /// much work however large a block is.
    #[default]
    Spread,
    /// On rayon's threads, channels in parallel, giving the bytes `Spread` gives. A call waits
    /// only when more frames are being compressed than twice the threads and two more.
    Offload,
}

#[derive(Clone, Debug)]
pub struct WriteOptions {
    /// The zstd level for blocks, annotation parts, strips and the annotation bundle.
    pub level: i32,
    /// The decoded size at which a channel's block closes, at its next promise: 1 byte to
    /// `MAX_TARGET`.
    pub block_bytes: usize,
    /// Each item promises its own axis tick: for a caller that hands items in axis order across
    /// the channels, as the encoder does.
    pub promise_with_items: bool,
    pub compression: Compression,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            level: 19,
            block_bytes: 1 << 20,
            promise_with_items: false,
            compression: Compression::Spread,
        }
    }
}

/// The code for `value` under `coding`, if one reproduces it exactly.
pub fn code_of(value: f32, coding: AnalogCoding) -> Option<u16> {
    let AnalogCoding::Codes {
        bits,
        scale,
        offset,
    } = coding
    else {
        return None;
    };
    let code = ((f64::from(value) - offset) / scale).round();
    if !(0.0..f64::from(1u32 << bits)).contains(&code) {
        return None;
    }
    let code = code as u16;
    (code_value(code, scale, offset).to_bits() == value.to_bits()).then_some(code)
}

fn uleb_len(v: u64) -> usize {
    (64 - v.leading_zeros() as usize).div_ceil(7).max(1)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A digital channel's edges in its open block, in its source's ticks, and the size its section
/// comes to (FORMAT.md, "Blocks"): `step`, the first edge, then each later interval over `step`.
#[derive(Clone, Debug)]
struct EdgeRun {
    ticks: Vec<i64>,
    /// The greatest common divisor of the intervals that are not zero; 0 while there is none.
    step: u64,
    /// The intervals' bytes over `step`.
    intervals: usize,
    /// How many intervals have each bit length, 0 to 64: a smaller step's bytes come from these.
    lengths: Box<[u32; 65]>,
}

impl Default for EdgeRun {
    fn default() -> Self {
        Self {
            ticks: Vec::new(),
            step: 0,
            intervals: 0,
            lengths: Box::new([0; 65]),
        }
    }
}

/// The inverse of `a`, odd, modulo 2^64: `a × it` is 1. Each step of Newton's method doubles the
/// bits it is right in, from the 3 that `a` itself is (an odd number is its own inverse modulo 8).
fn odd_inverse(a: u64) -> u64 {
    debug_assert!(a % 2 == 1, "{a} is even");
    let mut x = a;
    for _ in 0..5 {
        x = x.wrapping_mul(2u64.wrapping_sub(a.wrapping_mul(x)));
    }
    x
}

/// Bits in `v`: 0 for 0.
fn bit_length(v: u64) -> usize {
    64 - v.leading_zeros() as usize
}

impl EdgeRun {
    fn of(ticks: &[i64]) -> Self {
        let mut run = Self::default();
        for &t in ticks {
            run.push(t);
        }
        run
    }

    fn step(&self) -> u64 {
        self.step.max(1)
    }

    /// The section's size.
    fn size(&self) -> usize {
        match self.ticks.first() {
            None => 0,
            Some(&first) => uleb_len(self.step()) + uleb_len(first as u64) + self.intervals,
        }
    }

    /// Whether an edge taken in could make the section larger than `limit`: each number it codes
    /// takes at most 10 bytes, and a smaller step codes every interval again.
    fn may_pass(&self, limit: usize) -> bool {
        self.size().saturating_add(10) > limit
            || self.ticks.len().saturating_add(2).saturating_mul(10) > limit
    }

    /// The intervals' bytes over `step`, a divisor of each. An interval over `step` takes a byte,
    /// and one more for each `step × 2^7j` it reaches. The intervals' bit lengths say which reach
    /// each such bound, except where the bound falls inside a bit length some interval has: that
    /// length's intervals are read.
    fn coded(&self, step: u64) -> usize {
        let mut bytes = self.ticks.len().saturating_sub(1);
        let (mut read, mut reads) = ([0u64; 9], 0);
        for j in 1..10 {
            let Some(reach) = step.checked_mul(1 << (7 * j)) else {
                break;
            };
            let b = bit_length(reach);
            let longer: usize = self.lengths[b + 1..].iter().map(|&c| c as usize).sum();
            if reach.is_power_of_two() {
                bytes += longer + self.lengths[b] as usize;
            } else if self.lengths[b] == 0 {
                bytes += longer;
            } else {
                bytes += longer;
                read[reads] = reach;
                reads += 1;
            }
        }
        if reads > 0 {
            for w in self.ticks.windows(2) {
                let interval = (w[1] - w[0]) as u64;
                for &reach in &read[..reads] {
                    // `reach` lies inside the length's span, so only that length's intervals
                    // reach it without being counted above.
                    bytes +=
                        usize::from(interval >= reach && bit_length(interval) == bit_length(reach));
                }
            }
        }
        bytes
    }

    /// The section's size with an edge at `tick`, no earlier than the last, taken in.
    fn size_with(&self, tick: i64) -> usize {
        let Some(&last) = self.ticks.last() else {
            return uleb_len(1) + uleb_len(tick as u64);
        };
        let first = self.ticks[0];
        let interval = (tick - last) as u64;
        let step = gcd(self.step, interval);
        let intervals = if step == self.step {
            self.intervals + uleb_len(interval / self.step())
        } else {
            self.coded(step.max(1)) + uleb_len(interval / step.max(1))
        };
        uleb_len(step.max(1)) + uleb_len(first as u64) + intervals
    }

    /// Takes an edge at `tick`, no earlier than the last, and returns the section's new size.
    fn push(&mut self, tick: i64) -> usize {
        if let Some(&last) = self.ticks.last() {
            let interval = (tick - last) as u64;
            let step = gcd(self.step, interval);
            self.ticks.push(tick);
            self.lengths[bit_length(interval)] += 1;
            if step != self.step {
                // A smaller step codes every interval again, at most 64 times a block: the step
                // only shrinks, by a factor of 2 at least each time.
                self.step = step;
                self.intervals = self.coded(self.step());
            } else {
                self.intervals += uleb_len(interval / self.step());
            }
        } else {
            self.ticks.push(tick);
        }
        self.size()
    }

    fn encode(&self, out: &mut Vec<u8>) {
        let Some(&first) = self.ticks.first() else {
            return;
        };
        let step = self.step();
        put_uleb(out, step);
        put_uleb(out, first as u64);
        // Every interval is a multiple of the step, so it divides exactly as a shift by the step's
        // factors of 2 and a product with the inverse of its odd part, modulo 2^64.
        let shift = step.trailing_zeros();
        let inverse = odd_inverse(step >> shift);
        for w in self.ticks.windows(2) {
            put_uleb(out, (((w[1] - w[0]) as u64) >> shift).wrapping_mul(inverse));
        }
    }
}

/// A block's decoded payload from an analog channel's samples as the writer holds them, each
/// sample's bytes together: codes split into byte planes.
pub(crate) fn analog_raw(section: &[u8], coding: AnalogCoding) -> Vec<u8> {
    if coding.planes() {
        let mut out = Vec::with_capacity(section.len());
        samples::to_planes(section, coding.sample_len(), &mut out);
        out
    } else {
        section.to_vec()
    }
}

struct DigitalState {
    initial: bool,
    /// The level at the open block's start.
    level: bool,
    /// The channel's start, in its source's ticks.
    start: i64,
    /// The last edge's source tick, or the channel's start before the first.
    last_edge: i64,
    edges: u64,
    open: EdgeRun,
    /// The last gap taken, and how many.
    last_gap: Option<Gap>,
    gap_count: u64,
    /// Gaps whose `to` no closed block has passed, each with its `to` on the axis. They are
    /// hashed and listed once one has.
    pending: VecDeque<(Gap, i64)>,
    /// The gaps hashed, in the header's order, and the last one's `to`.
    listed: ListHasher,
    last_listed: Option<Gap>,
    list: TableWriter<Gap>,
}

struct AnalogState {
    grid: Grid,
    coding: AnalogCoding,
    /// The runs a sample still needs: from the run holding the open block's first sample, or a
    /// lagging partner's next sample, on.
    runs: VecDeque<Break>,
    /// The first run, and how many runs there are.
    first_run: Option<Break>,
    run_count: u64,
    /// A run the next sample starts, its `index` the sample's.
    next_run: Option<Break>,
    count: u64,
    /// The breaks hashed, in order: each with its first sample's block.
    listed: ListHasher,
    /// The open block's samples, each one's bytes together.
    open: Vec<u8>,
    list: TableWriter<Break>,
}

enum Data {
    Digital(DigitalState),
    Analog(AnalogState),
}

struct Chan {
    map: Mapping,
    /// Every item before this axis tick is in: the channel's own promise, or a later global one.
    promise: i64,
    /// The open block's start, where the last closed block ended.
    start: i64,
    /// The open block's items, its decoded size, and its first and last item's axis ticks.
    items: u32,
    size: usize,
    first_tick: i64,
    last_tick: i64,
    /// The items in the blocks closed.
    closed_items: u64,
    blocks_closed: u64,
    blocks: TableWriter<BlockRow>,
    hasher: ChannelHasher,
    data: Data,
    full: bool,
}

/// An overview channel's ladder and the tables of its levels in overview blocks.
struct OChan {
    ladder: Option<Ladder>,
    column: Column,
    levels: Vec<Option<TableWriter<OverviewRow>>>,
}

/// What a record's place in the file settles besides its bytes: the gaps and breaks a closed
/// block lists, whose list pages come after it.
enum Placed {
    Nothing,
    Gaps(usize, Vec<Gap>),
    Breaks(usize, Vec<Break>),
}

enum State {
    Waiting,
    Running { consumed: usize, out: Vec<u8> },
    Done(Vec<u8>),
    Off(Arc<Slot>),
}

enum FrameOf {
    Block {
        head: BlockHead,
        first_index: u64,
    },
    Overview {
        o: usize,
        li: usize,
        head: OverviewHead,
    },
    /// An annotation part, its frame decoding to `raw`, its annotations covering `span`.
    Part {
        span: (i64, i64),
    },
}

enum Item {
    Record(Vec<u8>, Placed),
    Frame {
        of: FrameOf,
        raw: Arc<Vec<u8>>,
        level: i32,
        state: State,
    },
}

impl Item {
    fn pending(&self) -> bool {
        matches!(
            self,
            Item::Frame {
                state: State::Waiting | State::Running { .. },
                ..
            }
        )
    }
}

/// What a finished recording comes to.
#[derive(Debug)]
pub struct Finished {
    pub header: Header,
    /// The bytes not yet taken: the end of the file.
    pub tail: Vec<u8>,
}

impl Finished {
    pub fn content_id(&self) -> ContentId {
        self.header.content_id
    }
}

/// What a writer's `finish` is told besides the length and the digital channels' ends.
#[derive(Clone, Debug, Default)]
pub struct Finish {
    /// What each source states only at its end, such as a probe's totals, added to its `extra`,
    /// by source index. It may list fewer sources than the recording has.
    pub extra: Vec<BTreeMap<String, String>>,
    /// How to bundle the annotations the writer took, so the recording is one file. A writer
    /// that took any, or is given a `view`, bundles them whatever this says, its files named
    /// after `DEFAULT_STEM` where it is None.
    pub bundle: Option<Bundle>,
    /// Take a gap that reaches a digital channel's end as ending what is known at its `from`,
    /// even where the gap ends inside the recording, as repair does for a file cut off. Otherwise
    /// such a gap is refused, as most likely a caller's mistake.
    pub drop_gaps_past_end: bool,
    /// Samples the instrument lost after an analog channel's last, by channel index.
    pub lost_after: BTreeMap<usize, u64>,
    /// The stored view (ANNOTATIONS.md, "The stored view"): the `[view]` table of the hand-written
    /// file in a generated bundle, which the writer then writes even with no annotation. A view
    /// `annotations check` would refuse is refused with the problems it names, and so is any
    /// view with `Bundle::Files`, whose hand-written file holds its own.
    pub view: Option<View>,
}

/// What a finished file bundles in its annotation bundle record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bundle {
    /// The writer's own annotations: each source's generator file `<stem>.<source>.jsonl`, its
    /// first line alone, the rest in its parts, and a hand-written `<stem>.annotations.toml` that
    /// holds no annotation and includes them in the order of their first annotations, where
    /// `<stem>` names the recording `<stem>.tickwave`.
    Generated { stem: String },
    /// These files, for a writer that took no annotation of its own: each generator file the
    /// hand-written file includes goes in parts that stand alone, in its order, its first line in
    /// the bundle, and every other file in the bundle whole.
    Files(BTreeMap<String, String>),
}

/// The decoded size past which a writer writes a generator's lines as a part (FORMAT.md,
/// "tickwave's writer").
pub const PART_TARGET: usize = 64 << 10;

/// A part as a writer holds it before compressing it: what its frame decodes to, and the span
/// its annotations cover.
pub(crate) type RawPart = (Vec<u8>, (i64, i64));

/// One generator's lines since its last part, and the span they cover.
#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub source: String,
    /// Each line with its newline.
    pub lines: String,
    pub span: Option<(i64, i64)>,
}

impl Pending {
    pub fn new(source: &str) -> Self {
        Self {
            source: source.to_owned(),
            lines: String::new(),
            span: None,
        }
    }

    /// What a part of its lines and `more` bytes of lines more decodes to.
    pub fn part_len(&self, more: usize) -> usize {
        2 + self.source.len() + self.lines.len() + more
    }

    /// `line`, without its newline, covering `span`.
    pub fn push(&mut self, line: &str, span: (i64, i64)) {
        self.lines.push_str(line);
        self.lines.push('\n');
        self.span = Some(
            self.span
                .map_or(span, |(s, e)| (s.min(span.0), e.max(span.1))),
        );
    }

    /// Its lines as a part's decoded bytes, and their span; None while it holds none.
    pub fn take(&mut self) -> Option<RawPart> {
        let span = self.span.take()?;
        let part = Part {
            source: self.source.clone(),
            lines: std::mem::take(&mut self.lines),
        };
        Some((part.to_raw(), span))
    }
}

/// The lines of generator `source`, without their newlines, as parts that stand alone: each
/// part's decoded bytes and span, about `PART_TARGET` bytes each and none past 4 MiB. Blank
/// lines are skipped. Err where a line's times don't read or a line alone passes 4 MiB.
pub(crate) fn split_into_parts<'a>(
    source: &str,
    lines: impl IntoIterator<Item = &'a str>,
    timebase: crate::model::Ratio,
) -> Result<Vec<RawPart>, String> {
    let mut parts = Vec::new();
    let mut pending = Pending::new(source);
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let span = placed_span(line, timebase).map_err(|e| format!("generator {source:?}: {e}"))?;
        if Pending::new(source).part_len(line.len() + 1) > MAX_PART {
            return Err(format!(
                "generator {source:?}: a line of {} bytes, past what a part holds, 4 MiB",
                line.len()
            ));
        }
        if pending.part_len(line.len() + 1) > MAX_PART {
            parts.extend(pending.take());
        }
        pending.push(line, span);
        if pending.lines.len() >= PART_TARGET {
            parts.extend(pending.take());
        }
    }
    parts.extend(pending.take());
    Ok(parts)
}

/// A part record for `raw`, which covers `span`, compressed at `level`, appended to `out`, whose
/// first byte is at `at` in the file; its row.
pub(crate) fn write_part_record(
    out: &mut Vec<u8>,
    at: u64,
    raw: &[u8],
    span: (i64, i64),
    level: i32,
) -> Result<PartRow, WriteError> {
    let frame = compress(raw, level)?;
    let row = PartRow {
        offset: at + out.len() as u64 + (record::PAYLOAD_AT + 4) as u64,
        length: frame.len() as u32,
        decoded_length: raw.len() as u32,
        start: span.0,
        end: span.1,
    };
    record::write(
        out,
        Kind::Part,
        &[&(raw.len() as u32).to_le_bytes(), &frame],
    );
    Ok(row)
}

/// The name a bundle gives its files where a writer that took annotations is asked for none:
/// `recording.annotations.toml` and `recording.<source>.jsonl`.
pub const DEFAULT_STEM: &str = "recording";

/// What a finish bundles given `bundle`: the bundle's files but the parts' lines, and the
/// generator files' other lines as parts that stand alone, in the order the hand-written file
/// includes them. `generators` are the writer's own, in the order of their first annotations,
/// which a bundle of given files can't hold too.
pub(crate) enum Bundling {
    None,
    Generated {
        stem: String,
    },
    Files {
        files: BTreeMap<String, String>,
        parts: Vec<RawPart>,
    },
}

impl Bundling {
    /// `generators` are the writer's, `records` whether it wrote annotation records. A writer's
    /// parts that stand alone may go with given files, a re-cutter's carrying its input's bundle,
    /// where each generator has a file among them.
    pub fn of(
        bundle: Option<Bundle>,
        generators: &[Pending],
        records: bool,
        timebase: crate::model::Ratio,
    ) -> Result<Self, WriteError> {
        match bundle {
            None if generators.is_empty() => Ok(Self::None),
            // A recording that holds annotations ends with them bundled, whatever the caller
            // asked: under names of its own where it named none.
            None => Ok(Self::Generated {
                stem: DEFAULT_STEM.into(),
            }),
            Some(Bundle::Generated { stem }) => Ok(Self::Generated { stem }),
            Some(Bundle::Files(_)) if records => Err(WriteError::Bundle(
                "files given to bundle, and the writer took annotations of its own".into(),
            )),
            Some(Bundle::Files(files))
                if generators.iter().any(|g| {
                    !files.values().any(|t| {
                        crate::annotations::generator_source(t).as_deref() == Some(&g.source)
                    })
                }) =>
            {
                Err(WriteError::Bundle(
                    "files given to bundle, and a generator of the writer's parts has none".into(),
                ))
            }
            Some(Bundle::Files(mut files)) => {
                let mut parts = Vec::new();
                for name in included_generators(&files) {
                    let text = files.get_mut(&name).expect("an included file");
                    let source = crate::annotations::generator_source(text)
                        .expect("an included generator file");
                    let mut lines = text.lines();
                    let first = lines.next().unwrap_or_default().to_owned();
                    parts.extend(
                        split_into_parts(&source, lines, timebase).map_err(WriteError::Bundle)?,
                    );
                    *text = format!("{first}\n");
                }
                Ok(Self::Files { files, parts })
            }
        }
    }
}

/// The generator files among `files` that its hand-written file includes, in its order. None
/// where no hand-written TOML file reads.
fn included_generators(files: &BTreeMap<String, String>) -> Vec<String> {
    let Some(hand) = files
        .iter()
        .find(|(name, _)| name.ends_with(".toml"))
        .map(|(_, text)| text)
    else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    crate::annotations::include_order(hand)
        .unwrap_or_default()
        .into_iter()
        .filter(|name| {
            files
                .get(name)
                .is_some_and(|t| crate::annotations::generator_source(t).is_some())
                && seen.insert(name.clone())
        })
        .collect()
}

/// `view` as the `[view]` table `bundled_files` writes reads: its fields as JSON, as a hand-written
/// file's TOML gives them to the checks.
fn view_table(v: &View) -> serde_json::Value {
    let mut table = serde_json::Map::new();
    if let Some(channels) = &v.channels {
        table.insert("channels".into(), channels.clone().into());
    }
    if let Some(start) = v.start {
        table.insert("start".into(), start.into());
    }
    if let Some(end) = v.end {
        table.insert("end".into(), end.into());
    }
    if let Some(c) = &v.colour {
        table.insert("colour".into(), vec![c.day.clone(), c.night.clone()].into());
    }
    table.into()
}

/// The files a bundle holds for a recording `<stem>.tickwave` with `id` whose generators are
/// `sources`, each with the roles it declares, in the order of their first annotations: each
/// one's file, its first line alone, and a hand-written file that includes them in that order,
/// holds `view` as its `[view]` where there is one, and holds no annotation of its own.
pub(crate) fn bundled_files<'a>(
    stem: &str,
    id: ContentId,
    sources: impl IntoIterator<Item = (&'a str, &'a [serde_json::Value])>,
    view: Option<&View>,
) -> BTreeMap<String, String> {
    // A JSON string is a TOML basic string: serde_json escapes nothing TOML lacks.
    let quoted = |s: &str| serde_json::to_string(s).expect("a string serialises");
    let mut files = BTreeMap::new();
    let mut include = Vec::new();
    for (source, roles) in sources {
        let name = format!("{stem}.{source}.jsonl");
        files.insert(name.clone(), generator_head(source, None, id, roles) + "\n");
        include.push(quoted(&name));
    }
    let mut hand = format!(
        "format = \"tickwave-annotations/1\"\nrecording_id = {}\ninclude = [{}]\n",
        quoted(&id.to_string()),
        include.join(", ")
    );
    if let Some(v) = view {
        hand.push_str("\n[view]\n");
        if let Some(channels) = &v.channels {
            let names: Vec<String> = channels.iter().map(|c| quoted(c)).collect();
            hand.push_str(&format!("channels = [{}]\n", names.join(", ")));
        }
        // Ticks, which a time in the table may be.
        if let Some(start) = v.start {
            hand.push_str(&format!("start = {start}\n"));
        }
        if let Some(end) = v.end {
            hand.push_str(&format!("end = {end}\n"));
        }
        if let Some(c) = &v.colour {
            hand.push_str(&format!(
                "colour = [{}, {}]\n",
                quoted(&c.day),
                quoted(&c.night)
            ));
        }
    }
    files.insert(format!("{stem}.annotations.toml"), hand);
    files
}

/// A generator file's first line, `roles` its role tables, without its newline (ANNOTATIONS.md).
pub fn generator_head(
    source: &str,
    version: Option<&str>,
    recording_id: ContentId,
    roles: &[serde_json::Value],
) -> String {
    // In the order ANNOTATIONS.md shows it.
    let json = |s: &str| serde_json::to_string(s).expect("a string serialises");
    let version = version.map_or(String::new(), |v| format!(r#","version":{}"#, json(v)));
    let roles = if roles.is_empty() {
        String::new()
    } else {
        format!(
            r#","role":{}"#,
            serde_json::to_string(roles).expect("roles serialise")
        )
    };
    format!(
        r#"{{"format":"tickwave-annotations/1","source":{}{version},"recording_id":{}{roles}}}"#,
        json(source),
        json(&recording_id.to_string())
    )
}

/// The annotation bundle record's payload for `files`, compressed at `level`.
pub(crate) fn bundle_payload(
    files: &BTreeMap<String, String>,
    level: i32,
) -> Result<Vec<u8>, WriteError> {
    crate::bundle::bundle_payload(files, level)
        .map_err(|e| WriteError::Zstd(std::io::Error::other(e.to_string())))
}

/// A generator file: its first line, then `lines` (ANNOTATIONS.md).
pub fn generator_file(
    source: &str,
    version: Option<&str>,
    recording_id: ContentId,
    lines: &[String],
) -> String {
    let mut out = generator_head(source, version, recording_id, &[]);
    out.push('\n');
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn check_safe(what: &'static str, value: u64) -> Result<(), WriteError> {
    if value < JSON_SAFE {
        Ok(())
    } else {
        Err(WriteError::TooLarge { what, value })
    }
}

/// The run that holds sample `index` of a channel whose runs from one at or before `index` on
/// are `runs`.
fn run_of(runs: &VecDeque<Break>, index: u64) -> Option<Break> {
    let after = runs.partition_point(|r| r.index <= index);
    runs.get(after.checked_sub(1)?).copied()
}

/// Sample `index`'s axis tick on the run `run` starts, saturating at the ends of i64.
#[inline]
fn axis_in(map: &Mapping, grid: Grid, run: Break, index: u64) -> i64 {
    map.axis_place(grid.place(run, index), grid.d())
        .unwrap_or(i64::MAX)
}

/// Writes `due`, and any directory pages it fills, to `out`, whose first byte is at `taken`.
pub(crate) fn place_dues<R: Row>(
    table: &mut TableWriter<R>,
    mut due: Option<Due>,
    out: &mut Vec<u8>,
    taken: u64,
) {
    while let Some(d) = due {
        let at = taken + out.len() as u64;
        out.extend_from_slice(&d.record);
        due = table.placed(&d, at);
    }
}

/// Which channels close their blocks together: each derived channel's operands, and theirs in
/// turn. Each channel's partners.
fn groups(channels: usize, products: &[(usize, usize, samples::Product)]) -> Vec<Vec<usize>> {
    let mut group: Vec<usize> = (0..channels).collect();
    fn root(g: &[usize], mut c: usize) -> usize {
        while g[c] != c {
            c = g[c];
        }
        c
    }
    for &(a, b, _) in products {
        let (ra, rb) = (root(&group, a), root(&group, b));
        if ra != rb {
            group[ra] = rb;
        }
    }
    let roots: Vec<usize> = (0..channels).map(|c| root(&group, c)).collect();
    (0..channels)
        .map(|c| {
            (0..channels)
                .filter(|&p| p != c && roots[p] == roots[c])
                .collect()
        })
        .collect()
}

/// Writes one recording (see the module's documentation).
///
/// Its memory follows the channels, the block size and the frames waiting for compression, not
/// the recording's length (FORMAT.md, "Memory"): a snapshot's gaps, breaks and annotations come
/// from the file's list pages and parts. A coarse overview level's open block takes days to fill
/// and a directory's open page months, so it rises slowly toward its bound.
pub struct Writer {
    declaration: Arc<Declaration>,
    options: WriteOptions,
    chans: Vec<Chan>,
    ochans: Vec<OChan>,
    pairs: Vec<Pair>,
    /// Each channel's derived channels, by derived index.
    operand_of: Vec<Vec<usize>>,
    /// Each channel's partners, which close their blocks with it.
    partners: Vec<Vec<usize>>,
    /// Channels whose open block has reached its size, waiting for a promise past their first
    /// item.
    full: Vec<usize>,
    /// The latest promise to every channel.
    promise: i64,
    /// The latest known-through record's tick; 0 before the first.
    known: i64,
    out: Vec<u8>,
    /// Bytes handed out before `out`.
    taken: u64,
    queue: VecDeque<Item>,
    /// The sequence number of the queue's first item, and of the first frame still to compress.
    head: u64,
    cursor: u64,
    slicers: Vec<Slicer>,
    /// The queue's frames in `State::Off`: every change of a frame's state to or from it moves
    /// this count.
    offloaded: usize,
    /// The most frames compressing on other threads before a call waits for the oldest.
    offload_limit: usize,
    lines: crate::annotations::Lines,
    /// Each generator's lines since its last part, in the order of its first annotation.
    generators: Vec<Pending>,
    /// The roles each generator declares in its file's first line, by its name (`roles`).
    roles: BTreeMap<String, Vec<serde_json::Value>>,
    /// Whether it wrote an annotation record, which a bundle of given files can't hold.
    took_records: bool,
    parts: TableWriter<PartRow>,
    /// A compression that failed where no error could be returned.
    failed: Option<String>,
}

impl Writer {
    /// Writes the preamble and the declaration to the output. The declaration is held to the
    /// rules a reader holds it to.
    pub fn new(declaration: Declaration, options: WriteOptions) -> Result<Self, WriteError> {
        if options.block_bytes == 0 || options.block_bytes > MAX_TARGET {
            return Err(WriteError::BlockSize);
        }
        check_declaration(&declaration).map_err(|e| WriteError::Declaration(e.to_string()))?;
        // This version defines no feature, codec or further operation: it writes none.
        let unknown = |what: String| {
            WriteError::Declaration(format!(
                "{what}, which this version doesn't define, so it writes no such channel"
            ))
        };
        if let Some(c) = declaration.channels.iter().find(|c| !c.available()) {
            return Err(unknown(format!(
                "channel {} states a feature or codec",
                c.name
            )));
        }
        if let Some(d) = (0..declaration.derived.len())
            .find(|&d| !declaration.available(declaration.channels.len() + d))
        {
            return Err(unknown(format!(
                "derived channel {} states an operation, params or features",
                declaration.derived[d].name
            )));
        }
        let unmapped = || WriteError::Declaration("a channel has no mapping to the axis".into());
        let mut chans = Vec::with_capacity(declaration.channels.len());
        for (c, channel) in declaration.channels.iter().enumerate() {
            let hasher = ChannelHasher::declared(&declaration, c).ok_or_else(unmapped)?;
            let map = declaration.mapping(c).ok_or_else(unmapped)?;
            let data = match &channel.data {
                DeclaredKind::Digital { initial } => {
                    let start = declaration.start(c).ok_or_else(unmapped)?;
                    Data::Digital(DigitalState {
                        initial: *initial,
                        level: *initial,
                        start,
                        last_edge: start,
                        edges: 0,
                        open: EdgeRun::default(),
                        last_gap: None,
                        gap_count: 0,
                        pending: VecDeque::new(),
                        listed: ListHasher::new(),
                        last_listed: None,
                        list: TableWriter::new(c as u32, Table::Gaps, true),
                    })
                }
                DeclaredKind::Analog { coding, .. } => Data::Analog(AnalogState {
                    grid: channel.grid().ok_or_else(unmapped)?,
                    coding: *coding,
                    runs: VecDeque::new(),
                    first_run: None,
                    run_count: 0,
                    next_run: None,
                    count: 0,
                    listed: ListHasher::new(),
                    open: Vec::new(),
                    list: TableWriter::new(c as u32, Table::Breaks, false),
                }),
            };
            let digital = matches!(data, Data::Digital(_));
            chans.push(Chan {
                map,
                promise: 0,
                start: 0,
                items: 0,
                size: 0,
                first_tick: 0,
                last_tick: 0,
                closed_items: 0,
                blocks_closed: 0,
                blocks: TableWriter::new(c as u32, Table::Blocks, digital),
                hasher,
                data,
                full: false,
            });
        }
        let columns = overview::columns(&declaration.channels, &declaration.derived);
        let ochans = columns
            .iter()
            .enumerate()
            .map(|(o, &column)| OChan {
                ladder: Some(overview::ladder_of(&declaration, o, column)),
                column,
                levels: Vec::new(),
            })
            .collect();
        let pairs = Pair::of(&declaration.channels, &declaration.derived);
        let products = overview::products(&declaration.channels, &declaration.derived);
        let mut operand_of = vec![Vec::new(); chans.len()];
        for (d, &(a, b, _)) in products.iter().enumerate() {
            operand_of[a].push(d);
            if b != a {
                operand_of[b].push(d);
            }
        }
        let partners = groups(chans.len(), &products);
        let json = serde_json::to_vec(&declaration).expect("a declaration serialises");
        let mut out = Vec::new();
        out.extend_from_slice(&Preamble::CURRENT.to_bytes());
        record::write(&mut out, Kind::Declaration, &[&json]);
        let lines = crate::annotations::Lines::declared(&declaration);
        let offload_limit = match options.compression {
            Compression::Offload => 2 * rayon::current_num_threads().max(1) + 2,
            Compression::Spread => 0,
        };
        Ok(Self {
            declaration: Arc::new(declaration),
            options,
            chans,
            ochans,
            pairs,
            operand_of,
            partners,
            full: Vec::new(),
            promise: 0,
            known: 0,
            out,
            taken: 0,
            queue: VecDeque::new(),
            head: 0,
            cursor: 0,
            slicers: Vec::new(),
            offloaded: 0,
            offload_limit,
            lines,
            generators: Vec::new(),
            roles: BTreeMap::new(),
            took_records: false,
            parts: TableWriter::new(0, Table::Parts, false),
            failed: None,
        })
    }

    /// The bytes written and not yet taken.
    pub fn output(&self) -> &[u8] {
        &self.out
    }

    /// Takes the first `n` bytes of `output`, then compresses the next slice of what waits. A
    /// compression that fails here is the next fallible call's error.
    pub fn consume(&mut self, n: usize) {
        self.out.drain(..n);
        self.taken += n as u64;
        self.advance_quietly();
    }

    /// Compresses the next slice of what waits, then takes every byte written so far. A
    /// compression that fails here is the next fallible call's error.
    pub fn take_output(&mut self) -> Vec<u8> {
        self.advance_quietly();
        self.taken += self.out.len() as u64;
        std::mem::take(&mut self.out)
    }

    fn advance_quietly(&mut self) {
        if self.failed.is_none()
            && let Err(e) = self.advance()
        {
            self.failed = Some(e.to_string());
        }
    }

    /// A compression that failed in a call that returns no error: the writer stops there.
    #[inline]
    fn check_failed(&self) -> Result<(), WriteError> {
        match &self.failed {
            Some(e) => Err(WriteError::Zstd(std::io::Error::other(e.clone()))),
            None => Ok(()),
        }
    }

    /// The file's length so far: the bytes taken and those in `output`.
    pub fn position(&self) -> u64 {
        self.taken + self.out.len() as u64
    }

    pub fn declaration(&self) -> &Declaration {
        &self.declaration
    }

    /// The latest promise to every channel.
    pub fn promised(&self) -> i64 {
        self.promise
    }

    /// The latest known-through record's tick, 0 before the first.
    pub fn known_through(&self) -> i64 {
        self.known
    }

    /// Whether frames wait to be compressed or written: the bytes of every record made so far are
    /// in `output` once this is false.
    pub fn pending(&self) -> bool {
        !self.queue.is_empty()
    }

    fn name(&self, c: usize) -> String {
        self.declaration.channels[c].name.clone()
    }

    #[inline]
    fn check_channel(&self, c: usize) -> Result<(), WriteError> {
        if c < self.chans.len() {
            Ok(())
        } else {
            Err(WriteError::NoChannel(c))
        }
    }

    /// The latest promise made to channel `c` alone, which `promise_channel` can't go back from.
    pub(crate) fn promised_to(&self, c: usize) -> i64 {
        self.chans[c].promise
    }

    fn promise_of(&self, c: usize) -> i64 {
        self.chans[c].promise.max(self.promise)
    }

    /// Where source tick `k` of channel `c` lies on the axis, refusing either tick outside
    /// `0..2^53` (FORMAT.md, "Times"), as a reader would.
    #[inline]
    fn place(&self, c: usize, k: i64) -> Result<i64, WriteError> {
        let safe = 0..JSON_SAFE as i64;
        if !safe.contains(&k) {
            return Err(WriteError::SourceTick {
                channel: self.name(c),
                tick: k,
            });
        }
        match self.chans[c].map.axis(k) {
            Some(t) if safe.contains(&t) => Ok(t),
            _ => Err(WriteError::AxisTick {
                channel: self.name(c),
                tick: k,
            }),
        }
    }

    /// Where a sample's place of `units / d` source ticks lies on the axis, refusing a place whose
    /// whole tick lies outside `0..2^53`, or that maps outside it.
    #[inline]
    fn place_units(&self, c: usize, units: i128, grid: Grid) -> Result<i64, WriteError> {
        let safe = 0..JSON_SAFE as i64;
        let k = grid.floor(units);
        if !safe.contains(&k) {
            return Err(WriteError::SourceTick {
                channel: self.name(c),
                tick: k,
            });
        }
        match self.chans[c].map.axis_place(units, grid.d()) {
            Some(t) if safe.contains(&t) => Ok(t),
            _ => Err(WriteError::AxisTick {
                channel: self.name(c),
                tick: k,
            }),
        }
    }

    /// Sample `index` of channel `c`, on the run `run`, against derived channel `d`'s other
    /// operand: it sits on the run the partner's sample `index` sits on, where the partner has
    /// one, and it leaves no more than `MAX_AHEAD` bytes of `c`'s samples waiting.
    fn check_partner(&self, c: usize, d: usize, index: u64, run: Break) -> Result<(), WriteError> {
        let pair = &self.pairs[d];
        let [(a, _), (b, _)] = pair.operands;
        let p = if a == c { b } else { a };
        if p == c {
            return Ok(());
        }
        if pair.too_far_ahead(c) {
            return Err(WriteError::OperandAhead {
                channel: self.name(c),
                partner: self.name(p),
                derived: pair.name.clone(),
            });
        }
        if let Data::Analog(pa) = &self.chans[p].data
            && pa.count > index
            && run_of(&pa.runs, index) != Some(run)
        {
            return Err(Unpaired(
                pair.name.clone(),
                format!(
                    "{}'s sample {index} starts or sits on another run than {}'s",
                    self.name(c),
                    self.name(p)
                ),
            )
            .into());
        }
        Ok(())
    }

    /// An item of channel `c` at axis tick `t`, an edge at source tick `edge` or else a sample, is
    /// about to go in: promise it where every item promises its own tick, and refuse it before
    /// the channel's promise or open block, or past what a block's word and length hold.
    #[inline]
    fn before_item(&mut self, c: usize, t: i64, edge: Option<i64>) -> Result<(), WriteError> {
        if self.options.promise_with_items && t > self.promise {
            self.promise(t)?;
        }
        let promise = self.promise_of(c);
        if t < promise {
            return Err(WriteError::BeforePromise {
                channel: self.name(c),
                tick: t,
                promise,
            });
        }
        if t < self.chans[c].start {
            return Err(WriteError::Order {
                channel: self.name(c),
                tick: t,
                previous: self.chans[c].start,
            });
        }
        if self.size_with(c, edge) > MAX_BLOCK {
            // Closing at the latest promise to the channel and its partners may make room.
            let p = self.partners[c]
                .iter()
                .map(|&x| self.promise_of(x))
                .fold(self.promise_of(c), i64::min);
            let ch = &self.chans[c];
            if p > ch.start && ch.items > 0 && ch.first_tick < p {
                self.close(c, p)?;
            }
            if self.size_with(c, edge) > MAX_BLOCK {
                return Err(WriteError::BlockFull {
                    channel: self.name(c),
                    tick: t,
                });
            }
        }
        Ok(())
    }

    /// Channel `c`'s open block's decoded size with its next item taken: an edge at source tick
    /// `edge`, or else a sample. An edge's is worked out only near the limit.
    #[inline]
    fn size_with(&self, c: usize, edge: Option<i64>) -> usize {
        let ch = &self.chans[c];
        match (&ch.data, edge) {
            (Data::Digital(d), Some(tick)) if d.open.may_pass(MAX_BLOCK) => d.open.size_with(tick),
            (Data::Digital(_), Some(_)) => 0,
            (Data::Analog(a), _) => ch.size + a.coding.sample_len(),
            (Data::Digital(_), None) => unreachable!("a digital channel's item is an edge"),
        }
    }

    /// An item of channel `c` at axis tick `t` went in, its open block now `size` bytes.
    #[inline]
    fn after_item(&mut self, c: usize, t: i64, size: usize) -> Result<(), WriteError> {
        let ch = &mut self.chans[c];
        if ch.items == 0 {
            ch.first_tick = t;
        }
        ch.items += 1;
        ch.size = size;
        ch.last_tick = t;
        self.mark_full(c)?;
        self.advance()
    }

    /// Channel `c`'s open block, once what it holds reaches the target: full, closing at the next
    /// promise past what it holds, or now if one has come.
    fn mark_full(&mut self, c: usize) -> Result<(), WriteError> {
        if self.chans[c].full {
            return Ok(());
        }
        let held = self.chans[c].size + self.waiting_bytes(c, i64::MAX);
        if held >= self.options.block_bytes {
            self.chans[c].full = true;
            self.full.push(c);
            self.try_close(c)?;
            if !self.chans[c].full {
                self.full.retain(|&x| x != c);
            }
        }
        Ok(())
    }

    /// The gaps digital channel `c` has taken that end before axis tick `p` and wait for their
    /// block, at 17 bytes each: they count toward the target (FORMAT.md, "tickwave's writer").
    fn waiting_bytes(&self, c: usize, p: i64) -> usize {
        match &self.chans[c].data {
            // In order of their ends, as they come.
            Data::Digital(d) => 17 * d.pending.partition_point(|&(_, t)| t < p),
            Data::Analog(_) => 0,
        }
    }

    /// Whether channel `c`'s open block holds an item, or the end of a gap it has taken, before
    /// axis tick `p`: a block a promise or flush there can close.
    fn holds_before(&self, c: usize, p: i64) -> bool {
        let ch = &self.chans[c];
        (ch.items > 0 && ch.first_tick < p) || self.waiting_bytes(c, p) > 0
    }

    /// Every item of every channel before axis tick `before` is in. A channel's open block that
    /// has reached its size closes there. Promises go forward.
    pub fn promise(&mut self, before: i64) -> Result<(), WriteError> {
        self.check_failed()?;
        if before < self.promise {
            return Err(WriteError::Promise {
                before,
                last: self.promise,
            });
        }
        if before == self.promise {
            return Ok(());
        }
        self.promise = before;
        if self.full.is_empty() {
            return Ok(());
        }
        let full = self.full.clone();
        for c in full {
            self.try_close(c)?;
        }
        let chans = &self.chans;
        self.full.retain(|&c| chans[c].full);
        Ok(())
    }

    /// Every item of channel `c` before axis tick `before` is in: its open block closes there if
    /// it has reached its size, and so does a full partner's that waited on `c`. A channel's
    /// promises go forward.
    pub fn promise_channel(&mut self, c: usize, before: i64) -> Result<(), WriteError> {
        self.check_failed()?;
        self.check_channel(c)?;
        if before < self.chans[c].promise {
            return Err(WriteError::Promise {
                before,
                last: self.chans[c].promise,
            });
        }
        self.chans[c].promise = before;
        for i in 0..=self.partners[c].len() {
            let x = if i == 0 { c } else { self.partners[c][i - 1] };
            if self.chans[x].full {
                self.try_close(x)?;
                if !self.chans[x].full {
                    self.full.retain(|&y| y != x);
                }
            }
        }
        Ok(())
    }

    /// Closes channel `c`'s full block at the latest promise to it and its partners, once its
    /// items before that tick reach the target size. Whether it is still full after is its
    /// `full`.
    fn try_close(&mut self, c: usize) -> Result<(), WriteError> {
        let p = self.partners[c]
            .iter()
            .map(|&x| self.promise_of(x))
            .fold(self.promise_of(c), i64::min);
        if p > self.chans[c].start
            && self.holds_before(c, p)
            && self.size_before(c, p) + self.waiting_bytes(c, p) >= self.options.block_bytes
        {
            self.close(c, p)?;
        }
        let held = self.chans[c].size + self.waiting_bytes(c, i64::MAX);
        self.chans[c].full = held >= self.options.block_bytes;
        Ok(())
    }

    /// The decoded size of channel `c`'s open block's items before axis tick `p`.
    fn size_before(&self, c: usize, p: i64) -> usize {
        let ch = &self.chans[c];
        if ch.last_tick < p {
            return ch.size;
        }
        match &ch.data {
            Data::Digital(d) => {
                let n = d
                    .open
                    .ticks
                    .partition_point(|&k| ch.map.axis_saturating(k) < p);
                EdgeRun::of(&d.open.ticks[..n]).size()
            }
            Data::Analog(a) => {
                let len = a.coding.sample_len();
                let n = a.open.len() / len;
                let first = ch.closed_items;
                let (mut lo, mut hi) = (0usize, n);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let index = first + mid as u64;
                    let run = run_of(&a.runs, index).expect("a sample follows its run");
                    if axis_in(&ch.map, a.grid, run, index) < p {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                lo * len
            }
        }
    }

    /// An edge of digital channel `c` at source tick `tick`. An edge at the channel's start flips
    /// the level from the declared `initial`, as any edge does.
    pub fn edge(&mut self, c: usize, tick: i64) -> Result<(), WriteError> {
        self.check_failed()?;
        self.check_channel(c)?;
        let Data::Digital(d) = &self.chans[c].data else {
            return Err(WriteError::Kind {
                channel: self.name(c),
                kind: "analog",
            });
        };
        let t = self.place(c, tick)?;
        if tick < d.start {
            return Err(WriteError::BeforeStart {
                channel: self.name(c),
                tick,
                start: d.start,
            });
        }
        if tick < d.last_edge {
            return Err(WriteError::Order {
                channel: self.name(c),
                tick,
                previous: d.last_edge,
            });
        }
        if let Some(g) = d.last_gap
            && tick <= g.to
        {
            return Err(WriteError::EdgeInGap {
                channel: self.name(c),
                tick,
                to: g.to,
            });
        }
        self.before_item(c, t, Some(tick))?;
        let ch = &mut self.chans[c];
        let Data::Digital(d) = &mut ch.data else {
            unreachable!()
        };
        let size = d.open.push(tick);
        d.last_edge = tick;
        d.edges += 1;
        ch.hasher.edge(tick);
        self.ochans[c]
            .ladder
            .as_mut()
            .expect("a ladder until the finish")
            .edge(tick, t);
        self.after_item(c, t, size)
    }

    /// A gap of digital channel `c`, in its source's ticks. It may start in a block already
    /// written, but not end in one.
    pub fn gap(&mut self, c: usize, gap: Gap) -> Result<(), WriteError> {
        self.check_failed()?;
        self.check_channel(c)?;
        let name = self.name(c);
        if !matches!(self.chans[c].data, Data::Digital(_)) {
            return Err(WriteError::Kind {
                channel: name,
                kind: "analog",
            });
        }
        if gap.to <= gap.from {
            return Err(WriteError::GapSpan {
                channel: name,
                from: gap.from,
                to: gap.to,
            });
        }
        self.place(c, gap.from)?;
        let to = self.place(c, gap.to)?;
        let Data::Digital(d) = &self.chans[c].data else {
            unreachable!()
        };
        if gap.from < d.start {
            return Err(WriteError::BeforeStart {
                channel: name,
                tick: gap.from,
                start: d.start,
            });
        }
        let last = d.last_gap.map_or(d.last_edge, |g| g.to.max(d.last_edge));
        let after_last = if d.gap_count == 0 && d.edges == 0 {
            true
        } else {
            gap.from > last
        };
        if !after_last {
            return Err(WriteError::GapOrder {
                channel: name,
                from: gap.from,
                last,
            });
        }
        if gap.from == d.start && d.initial {
            return Err(WriteError::GapFromStart { channel: name });
        }
        // Its end can't lie where the overview has settled: before the channel's last block's
        // end or the latest known-through tick.
        let settled = self.known_of(c);
        if to < settled {
            return Err(WriteError::GapLate {
                channel: name,
                to: gap.to,
                closed: settled,
            });
        }
        if (d.pending.len() + 1) * 17 > MAX_WAITING_GAPS {
            return Err(WriteError::GapsWaiting { channel: name });
        }
        let Data::Digital(d) = &mut self.chans[c].data else {
            unreachable!()
        };
        d.last_gap = Some(gap);
        d.gap_count += 1;
        d.pending.push_back((gap, to));
        self.ochans[c]
            .ladder
            .as_mut()
            .expect("a ladder until the finish")
            .gap(gap, to);
        let mut bytes = Vec::new();
        record::write(&mut bytes, Kind::Gap, &[&record::gap_bytes(c as u32, gap)]);
        self.put(bytes, Placed::Nothing);
        self.mark_full(c)?;
        self.advance()
    }

    /// Analog channel `c`'s next sample starts a run at source tick `tick` plus the channel's
    /// declared phase, after `lost` samples the instrument lost: 0 when it only paused. The first
    /// run's `lost` are samples lost before the channel's first. A run no sample has followed yet
    /// is replaced.
    pub fn run(&mut self, c: usize, tick: i64, lost: u64) -> Result<(), WriteError> {
        self.check_channel(c)?;
        let phase = match &self.chans[c].data {
            Data::Analog(a) => a.grid.phase(),
            Data::Digital(_) => 0,
        };
        self.run_at(c, tick, phase, lost)
    }

    /// `run` at another phase: sample `index` starts a run at `tick` plus `phase / D`, `D` the
    /// channel's grid, as a run after a pause on a clock that kept running, or a cut inside a
    /// run, can. The first run's phase is the declared one. Each run starts at or after the place
    /// of the sample before it, and where it states lost samples, past where the first of them
    /// would sit (FORMAT.md, "Gaps").
    pub fn run_at(&mut self, c: usize, tick: i64, phase: u32, lost: u64) -> Result<(), WriteError> {
        self.check_failed()?;
        self.check_channel(c)?;
        let name = self.name(c);
        if !matches!(self.chans[c].data, Data::Analog(_)) {
            return Err(WriteError::Kind {
                channel: name,
                kind: "digital",
            });
        }
        self.place(c, tick)?;
        check_safe("lost count", lost)?;
        let map = self.chans[c].map;
        let Data::Analog(a) = &mut self.chans[c].data else {
            unreachable!()
        };
        let grid = a.grid;
        let first = a.count == 0;
        if u64::from(phase) >= grid.d() || (first && phase != grid.phase()) {
            return Err(WriteError::RunPhase {
                channel: name,
                phase,
                d: grid.d(),
            });
        }
        let run = Break {
            index: a.count,
            tick,
            lost,
            phase,
        };
        let start = grid.start(run);
        if map
            .axis_place(start, grid.d())
            .is_none_or(|t| !(0..JSON_SAFE as i64).contains(&t))
        {
            return Err(WriteError::AxisTick {
                channel: name,
                tick,
            });
        }
        if let Some(&last) = a.runs.back().filter(|_| !first) {
            if start < grid.place(last, a.count - 1) {
                return Err(WriteError::RunOrder {
                    channel: name,
                    tick,
                    previous: grid.floor(grid.place(last, a.count - 1)),
                });
            }
            if lost > 0 && start <= grid.place(last, a.count) {
                return Err(WriteError::LostRunStart {
                    channel: name,
                    tick,
                    lost,
                });
            }
        }
        if first && lost > 0 {
            let start = lost_before_start(grid, tick, lost);
            if start.is_none_or(|u| map.axis_place(u, grid.d()).is_none_or(|t| t < 0)) {
                return Err(WriteError::LostBefore {
                    channel: name,
                    lost,
                });
            }
        }
        a.next_run = Some(run);
        self.advance()
    }

    /// The next sample of analog channel `c`, in the type its coding stores: an f32 for an `f32`
    /// channel, or for a `codes` one if a code reproduces it exactly; an f64 for an `f64` one; a
    /// u64 or an i64 code for an `int` one, in its range. Its place, a source tick, follows from
    /// its run and the period.
    pub fn sample(&mut self, c: usize, value: impl Into<Sample>) -> Result<(), WriteError> {
        self.sample_one(c, value.into())
    }

    /// `sample`, compiled in this crate whatever the caller's type, so the item path inlines.
    fn sample_one(&mut self, c: usize, sample: Sample) -> Result<(), WriteError> {
        self.check_failed()?;
        self.check_channel(c)?;
        // The channel's name only for an error: a sample is the writer's commonest call.
        let Data::Analog(a) = &self.chans[c].data else {
            return Err(WriteError::Kind {
                channel: self.name(c),
                kind: "digital",
            });
        };
        let index = a.count;
        let (run, starts) = match (a.next_run, a.runs.back()) {
            (Some(run), _) => (run, true),
            (None, Some(run)) => (*run, false),
            (None, None) => {
                return Err(WriteError::NoRun {
                    channel: self.name(c),
                });
            }
        };
        let grid = a.grid;
        let coding = a.coding;
        let tick = self.place_units(c, grid.place(run, index), grid)?;
        if let Some(why) = samples::misfit(sample, coding) {
            return Err(WriteError::Sample {
                channel: self.name(c),
                index,
                why,
            });
        }
        let code = match (coding, sample) {
            (coding @ AnalogCoding::Codes { .. }, Sample::F32(value)) => {
                Some(code_of(value, coding).ok_or_else(|| WriteError::Coding {
                    channel: self.name(c),
                    index,
                    value,
                    coding,
                })?)
            }
            _ => None,
        };
        for &d in &self.operand_of[c] {
            self.check_partner(c, d, index, run)?;
        }
        self.before_item(c, tick, None)?;
        if starts {
            let mut bytes = Vec::new();
            record::write(&mut bytes, Kind::Run, &[&record::run_bytes(c as u32, run)]);
            self.put(bytes, Placed::Nothing);
        }
        let ch = &mut self.chans[c];
        let Data::Analog(a) = &mut ch.data else {
            unreachable!()
        };
        if starts {
            a.runs.push_back(run);
            a.first_run.get_or_insert(run);
            a.run_count += 1;
            a.next_run = None;
        }
        samples::put(&mut a.open, sample, coding, code);
        let size = a.open.len();
        a.count += 1;
        ch.hasher.sample(sample);
        self.ochans[c]
            .ladder
            .as_mut()
            .expect("a ladder until the finish")
            .sample(tick, sample);
        let stored = self.chans.len();
        for i in 0..self.operand_of[c].len() {
            let d = self.operand_of[c][i];
            if let Some(product) = self.pairs[d].take(c, sample) {
                self.ochans[stored + d]
                    .ladder
                    .as_mut()
                    .expect("a ladder until the finish")
                    .sample(tick, product);
            }
        }
        self.after_item(c, tick, size)
    }

    /// The paint roles generator `source` declares (ANNOTATIONS.md, "Roles"), each a role's table,
    /// for its file's first line in a bundle the writer makes of its own annotations. They are
    /// held to the schema there, by whatever resolves the bundle.
    pub fn roles(&mut self, source: &str, roles: Vec<serde_json::Value>) {
        self.roles.insert(source.to_owned(), roles);
    }

    /// One generated annotation, from generator `source`. Its times are axis ticks. Its record
    /// goes out at once; its generator's lines go in a part once they reach `PART_TARGET` bytes,
    /// after its record, and before a line that would take them past 4 MiB.
    pub fn annotation(
        &mut self,
        source: &str,
        annotation: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), WriteError> {
        self.check_failed()?;
        // As `resolve` reads a generator's line, so a bundle of them resolves.
        self.lines
            .take(source, &annotation)
            .map_err(WriteError::Annotation)?;
        let line = serde_json::to_string(&annotation).expect("a map serialises");
        let span = placed_span(&line, self.declaration.timebase)
            .map_err(|e| WriteError::Annotation(format!("generator {source:?}: {e}")))?;
        let g = self.generator_for(source, &line)?;
        let a = record::Annotation {
            source: source.to_owned(),
            line,
        };
        let mut bytes = Vec::new();
        record::write(&mut bytes, Kind::Annotation, &[&a.to_payload()]);
        self.put(bytes, Placed::Nothing);
        self.took_records = true;
        self.generators[g].push(&a.line, span);
        if self.generators[g].lines.len() >= PART_TARGET {
            self.write_part(g);
        }
        self.advance()
    }

    /// One line of generator `source`'s file, which goes in a part that stands alone, with no
    /// annotation record: a re-cutter's (FORMAT.md, "Re-cutting"). Its parts go out as
    /// `annotation`'s do, and its times may be decimals with a unit.
    pub(crate) fn standalone_line(&mut self, source: &str, line: &str) -> Result<(), WriteError> {
        self.check_failed()?;
        let span = placed_span(line, self.declaration.timebase)
            .map_err(|e| WriteError::Annotation(format!("generator {source:?}: {e}")))?;
        let g = self.generator_for(source, line)?;
        self.generators[g].push(line, span);
        if self.generators[g].lines.len() >= PART_TARGET {
            self.write_part(g);
        }
        self.advance()
    }

    /// Generator `source`'s place among the writer's, met for the first time or not, ready to
    /// take `line`: one no part can hold, or a 257th generator, is refused, and the lines held
    /// go in a part first where `line` would take them past 4 MiB.
    fn generator_for(&mut self, source: &str, line: &str) -> Result<usize, WriteError> {
        if Pending::new(source).part_len(line.len() + 1) > MAX_PART {
            return Err(WriteError::Annotation(format!(
                "generator {source:?}: a line of {} bytes, past what a part holds, 4 MiB",
                line.len()
            )));
        }
        let g = match self.generators.iter().position(|g| g.source == source) {
            Some(g) => g,
            None if self.generators.len() == MAX_GENERATORS => {
                return Err(WriteError::Annotation(format!(
                    "generator {source:?}: a recording has at most {MAX_GENERATORS} generators"
                )));
            }
            None => {
                self.generators.push(Pending::new(source));
                self.generators.len() - 1
            }
        };
        if self.generators[g].part_len(line.len() + 1) > MAX_PART {
            self.write_part(g);
        }
        Ok(g)
    }

    /// Generator `g`'s lines since its last part, as a part queued for compression.
    fn write_part(&mut self, g: usize) {
        if let Some((raw, span)) = self.generators[g].take() {
            self.queue_frame(FrameOf::Part { span }, Arc::new(raw), self.options.level);
        }
    }

    /// Every item before axis tick `before` is in: promises it, closes there every channel's open
    /// block that holds an item, or the end of a gap it has taken, before it, and writes a
    /// known-through record after those blocks.
    /// Items at or after `before` stay in the open blocks. A flush at the latest known-through
    /// tick writes nothing new. A live recorder calls this so a crash loses little.
    pub fn flush(&mut self, before: i64) -> Result<(), WriteError> {
        self.check_failed()?;
        check_safe("flush tick", before.max(0) as u64)?;
        if before < self.promise {
            return Err(WriteError::Flush {
                before,
                last: self.promise,
            });
        }
        self.promise(before)?;
        for c in 0..self.chans.len() {
            if self.chans[c].start < before && self.holds_before(c, before) {
                self.close(c, before)?;
            }
        }
        if before > self.known {
            self.known = before;
            let mut bytes = Vec::new();
            record::write(
                &mut bytes,
                Kind::KnownThrough,
                &[&record::known_through_bytes(before)],
            );
            self.put(bytes, Placed::Nothing);
            for o in 0..self.ochans.len() {
                self.settle(o)?;
            }
        }
        self.advance()
    }

    /// Where overview channel `o` is known to: every item before it is in. A channel is known to
    /// the end of its last closed block and to the latest known-through tick; a derived channel
    /// where both operands are.
    fn known_of(&self, o: usize) -> i64 {
        match self.chans.get(o) {
            Some(ch) => ch.start.max(self.known),
            None => {
                let d = o - self.chans.len();
                let [(a, _), (b, _)] = &self.pairs[d].operands;
                self.known_of(*a).min(self.known_of(*b))
            }
        }
    }

    /// Settles overview channel `o` to where it is known, and queues the overview blocks that
    /// settles.
    fn settle(&mut self, o: usize) -> Result<(), WriteError> {
        let to = self.known_of(o);
        loop {
            let Some(ladder) = self.ochans[o].ladder.as_mut() else {
                return Ok(());
            };
            let reached = ladder.settle_some(to, SETTLE_STEP);
            for s in ladder.take_settled() {
                self.queue_overview(o, s);
            }
            if reached {
                return Ok(());
            }
            // A long span settles many blocks: what waits is compressed before more settle, so
            // the queue holds a few.
            self.drain_all()?;
        }
    }

    fn queue_overview(&mut self, o: usize, s: Settled) {
        let head = OverviewHead {
            channel: o as u32,
            shift: s.shift,
            k: s.k,
            n: s.n,
        };
        self.queue_frame(
            FrameOf::Overview { o, li: s.li, head },
            s.raw,
            OVERVIEW_BLOCK_LEVEL,
        );
    }

    /// Closes channel `c`'s open block at axis tick `end`: its items before `end` go in the
    /// closed block, those at or after it stay. Its partners close there too where they can.
    fn close(&mut self, c: usize, end: i64) -> Result<(), WriteError> {
        let ch = &mut self.chans[c];
        let start = ch.start;
        debug_assert!(end > start, "a block ends after it starts");
        let listed;
        let (raw, word, kept, kept_size) = match &mut ch.data {
            Data::Digital(d) => {
                let map = ch.map;
                let split = d
                    .open
                    .ticks
                    .partition_point(|&k| map.axis_saturating(k) < end);
                let (closing, kept) = if split == d.open.ticks.len() {
                    (std::mem::take(&mut d.open), EdgeRun::default())
                } else {
                    (
                        EdgeRun::of(&d.open.ticks[..split]),
                        EdgeRun::of(&d.open.ticks[split..]),
                    )
                };
                let mut raw = Vec::with_capacity(closing.size());
                closing.encode(&mut raw);
                let word = closing.ticks.len() as u32 | if d.level { LEVEL_BIT } else { 0 };
                let ending = d.pending.iter().take_while(|&&(_, t)| t < end).count();
                let gaps: Vec<Gap> = d.pending.drain(..ending).map(|(g, _)| g).collect();
                d.level = level_after(d.level, &closing.ticks, &gaps);
                // The gaps this block ends go in the content id and the list now.
                for &g in &gaps {
                    d.listed.gap(g);
                }
                if let Some(&g) = gaps.last() {
                    d.last_listed = Some(g);
                }
                listed = Placed::Gaps(c, gaps);
                let n = closing.ticks.len();
                let size = kept.size();
                d.open = kept;
                (raw, word, n as u32, size)
            }
            Data::Analog(a) => {
                let first = ch.closed_items;
                let len = a.coding.sample_len();
                let n = a.open.len() / len;
                // The first sample at or after `end`.
                let (mut lo, mut hi) = (0usize, n);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let index = first + mid as u64;
                    let run = run_of(&a.runs, index).expect("a sample follows its run");
                    if axis_in(&ch.map, a.grid, run, index) < end {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                let rest = a.open.split_off(lo * len);
                let raw = analog_raw(&a.open, a.coding);
                let size = rest.len();
                a.open = rest;
                // The breaks whose first sample this block holds.
                let to = first + lo as u64;
                let breaks: Vec<Break> = a
                    .runs
                    .iter()
                    .rev()
                    .take_while(|r| r.index >= first)
                    .filter(|r| r.index > 0 && r.index < to)
                    .copied()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                for &b in &breaks {
                    a.listed.brk(b);
                }
                listed = Placed::Breaks(c, breaks);
                (raw, lo as u32, lo as u32, size)
            }
        };
        let decoded_length =
            u32::try_from(raw.len()).map_err(|_| WriteError::BlockTooLarge(raw.len()))?;
        let ch = &mut self.chans[c];
        let items_left = ch.items - kept;
        let head = BlockHead {
            channel: c as u32,
            start,
            end,
            word,
            decoded_length,
        };
        let first_index = ch.closed_items;
        ch.closed_items += u64::from(kept);
        ch.blocks_closed += 1;
        self.prune_runs(c);
        let ch = &mut self.chans[c];
        ch.start = end;
        ch.items = items_left;
        ch.size = kept_size;
        if items_left > 0 {
            self.chans[c].first_tick = self.first_open_tick(c);
        }
        let level = self.options.level;
        self.queue_frame(FrameOf::Block { head, first_index }, Arc::new(raw), level);
        // Its list's entries follow the block, so a list page comes after it.
        self.put(Vec::new(), listed);
        self.settle(c)?;
        for i in 0..self.operand_of[c].len() {
            let d = self.operand_of[c][i];
            self.settle(self.chans.len() + d)?;
        }
        for i in 0..self.partners[c].len() {
            let p = self.partners[c][i];
            if self.chans[p].start < end && self.promise_of(p) >= end {
                self.close(p, end)?;
            }
        }
        Ok(())
    }

    /// Drops the runs of channel `c` before the one holding its open block's first sample. A
    /// lagging partner's next sample, which `check_partner` places on these runs, comes no
    /// earlier: a block closes only at a promise, and the partner's samples before it are in.
    fn prune_runs(&mut self, c: usize) {
        let from = self.chans[c].closed_items;
        if let Data::Analog(a) = &mut self.chans[c].data {
            while a.runs.get(1).is_some_and(|r| r.index <= from) {
                a.runs.pop_front();
            }
        }
    }

    /// The axis tick of the first item in channel `c`'s open block, which holds one.
    fn first_open_tick(&self, c: usize) -> i64 {
        let ch = &self.chans[c];
        match &ch.data {
            Data::Digital(d) => ch.map.axis_saturating(d.open.ticks[0]),
            Data::Analog(a) => {
                let index = ch.closed_items;
                let run = run_of(&a.runs, index).expect("a sample follows its run");
                axis_in(&ch.map, a.grid, run, index)
            }
        }
    }

    /// A record that needs no compressing: straight out, unless records before it still wait.
    fn put(&mut self, bytes: Vec<u8>, placed: Placed) {
        if self.queue.is_empty() {
            let at = self.position();
            self.out.extend_from_slice(&bytes);
            self.placed(placed, at);
        } else {
            self.queue.push_back(Item::Record(bytes, placed));
        }
    }

    fn queue_frame(&mut self, of: FrameOf, raw: Arc<Vec<u8>>, level: i32) {
        let state = match self.options.compression {
            Compression::Spread => State::Waiting,
            Compression::Offload => {
                self.offloaded += 1;
                State::Off(Slot::spawn(Arc::clone(&raw), level))
            }
        };
        self.queue.push_back(Item::Frame {
            of,
            raw,
            level,
            state,
        });
    }

    /// A record without a frame now in the file at `at`: its list takes what it lists.
    fn placed(&mut self, placed: Placed, _at: u64) {
        let taken = self.taken;
        match placed {
            Placed::Nothing => {}
            Placed::Gaps(c, gaps) => {
                if let Data::Digital(d) = &mut self.chans[c].data {
                    for gap in gaps {
                        let due = d.list.push(gap);
                        place_dues(&mut d.list, due, &mut self.out, taken);
                    }
                }
            }
            Placed::Breaks(c, breaks) => {
                if let Data::Analog(a) = &mut self.chans[c].data {
                    for run in breaks {
                        let due = a.list.push(run);
                        place_dues(&mut a.list, due, &mut self.out, taken);
                    }
                }
            }
        }
    }

    /// Which of `slicers` is `level`'s, made the first time it's needed.
    fn slicer(&mut self, level: i32) -> Result<usize, WriteError> {
        Ok(match self.slicers.iter().position(|s| s.level() == level) {
            Some(i) => i,
            None => {
                self.slicers.push(Slicer::new(level)?);
                self.slicers.len() - 1
            }
        })
    }

    /// The next slices of what waits to be compressed, at most `budget` decoded bytes of them
    /// (the first slice always runs), then every record now ready.
    fn compress(&mut self, budget: usize) -> Result<(), WriteError> {
        if self.options.compression == Compression::Spread {
            let mut spent = 0usize;
            loop {
                // The first frame still to compress, from the cursor on.
                let mut at = (self.cursor - self.head) as usize;
                while at < self.queue.len() && !self.queue[at].pending() {
                    at += 1;
                }
                self.cursor = self.head + at as u64;
                if at == self.queue.len() {
                    break;
                }
                let Item::Frame {
                    raw, level, state, ..
                } = &self.queue[at]
                else {
                    unreachable!("a pending item is a frame")
                };
                let consumed = match state {
                    State::Running { consumed, .. } => *consumed,
                    _ => 0,
                };
                let next = SLICE.min(raw.len() - consumed);
                if spent > 0 && spent + next > budget {
                    break;
                }
                let (raw, level) = (Arc::clone(raw), *level);
                let si = self.slicer(level)?;
                let slicer = &mut self.slicers[si];
                let Item::Frame { state, .. } = &mut self.queue[at] else {
                    unreachable!()
                };
                if matches!(state, State::Waiting) {
                    slicer.begin(raw.len())?;
                    *state = State::Running {
                        consumed: 0,
                        out: Vec::new(),
                    };
                }
                let State::Running { consumed, out } = state else {
                    unreachable!()
                };
                let end = *consumed + next;
                slicer.slice(&raw[*consumed..end], end == raw.len(), out)?;
                *consumed = end;
                spent += next;
                if end == raw.len() {
                    *state = State::Done(std::mem::take(out));
                }
            }
        } else {
            debug_assert_eq!(
                self.offloaded,
                self.queue
                    .iter()
                    .filter(|i| matches!(
                        i,
                        Item::Frame {
                            state: State::Off(_),
                            ..
                        }
                    ))
                    .count(),
                "the frames compressing on other threads"
            );
            // Wait for the oldest frames while more are being compressed than the threads hold.
            while self.offloaded > self.offload_limit {
                let Some(Item::Frame { state, .. }) = self.queue.iter_mut().find(|i| {
                    matches!(
                        i,
                        Item::Frame {
                            state: State::Off(_),
                            ..
                        }
                    )
                }) else {
                    break;
                };
                let State::Off(slot) = state else {
                    unreachable!()
                };
                *state = State::Done(slot.wait()?);
                self.offloaded -= 1;
                self.emit()?;
            }
        }
        self.emit()
    }

    /// What every call does: the next slice of compression, then the records ready.
    fn advance(&mut self) -> Result<(), WriteError> {
        if self.queue.is_empty() {
            return Ok(());
        }
        self.compress(SLICE)
    }

    /// Writes the queue's ready records, in order, up to the first frame not yet compressed.
    fn emit(&mut self) -> Result<(), WriteError> {
        loop {
            let ready = match self.queue.front_mut() {
                None => break,
                Some(Item::Record(..)) => true,
                Some(Item::Frame { state, .. }) => match state {
                    State::Done(_) => true,
                    State::Off(slot) => match slot.try_take() {
                        Some(frame) => {
                            *state = State::Done(frame?);
                            self.offloaded -= 1;
                            true
                        }
                        None => false,
                    },
                    _ => false,
                },
            };
            if !ready {
                break;
            }
            let item = self.queue.pop_front().expect("a front item");
            self.head += 1;
            self.cursor = self.cursor.max(self.head);
            let at = self.position();
            match item {
                Item::Record(bytes, placed) => {
                    self.out.extend_from_slice(&bytes);
                    self.placed(placed, at);
                }
                Item::Frame {
                    of,
                    raw,
                    state: State::Done(frame),
                    ..
                } => self.write_frame(of, &raw, &frame, at)?,
                Item::Frame { .. } => unreachable!("only a compressed frame is ready"),
            }
        }
        Ok(())
    }

    /// A compressed frame's record, at `at` in the file, and its row in its table.
    fn write_frame(
        &mut self,
        of: FrameOf,
        raw: &[u8],
        frame: &[u8],
        at: u64,
    ) -> Result<(), WriteError> {
        let length =
            u32::try_from(frame.len()).map_err(|_| WriteError::BlockTooLarge(frame.len()))?;
        let taken = self.taken;
        match of {
            FrameOf::Block { head, .. } => {
                record::write(&mut self.out, Kind::Block, &[&head.to_bytes(), frame]);
                let row = BlockRow {
                    start: head.start,
                    offset: at + (record::PAYLOAD_AT + BlockHead::LEN) as u64,
                    length,
                    decoded_length: head.decoded_length,
                    word: head.word,
                };
                let c = head.channel as usize;
                let due = self.chans[c].blocks.push(row);
                place_dues(&mut self.chans[c].blocks, due, &mut self.out, taken);
            }
            FrameOf::Overview { o, li, head } => {
                record::write(
                    &mut self.out,
                    Kind::OverviewBlock,
                    &[&head.to_bytes(), frame],
                );
                let row = OverviewRow {
                    offset: at + (record::PAYLOAD_AT + OverviewHead::LEN) as u64,
                    length,
                    decoded_length: raw.len() as u32,
                    n: head.n,
                };
                let levels = &mut self.ochans[o].levels;
                if levels.len() <= li {
                    levels.resize_with(li + 1, || None);
                }
                let table = levels[li].get_or_insert_with(|| {
                    TableWriter::new(o as u32, Table::Level(head.shift), false)
                });
                let due = table.push(row);
                place_dues(table, due, &mut self.out, taken);
            }
            FrameOf::Part { span } => {
                let decoded = raw.len() as u32;
                record::write(&mut self.out, Kind::Part, &[&decoded.to_le_bytes(), frame]);
                let row = PartRow {
                    offset: at + (record::PAYLOAD_AT + 4) as u64,
                    length,
                    decoded_length: decoded,
                    start: span.0,
                    end: span.1,
                };
                let due = self.parts.push(row);
                place_dues(&mut self.parts, due, &mut self.out, taken);
            }
        }
        Ok(())
    }

    /// Compresses and writes everything that waits.
    fn drain_all(&mut self) -> Result<(), WriteError> {
        if self.options.compression == Compression::Offload {
            for item in &mut self.queue {
                if let Item::Frame { state, .. } = item
                    && let State::Off(slot) = state
                {
                    *state = State::Done(slot.wait()?);
                    self.offloaded -= 1;
                }
            }
        }
        while !self.queue.is_empty() {
            self.compress(usize::MAX)?;
        }
        Ok(())
    }

    /// What the writer holds so far, for a view of the recording while it is written: each
    /// channel's blocks in the file through its index's pages and the rows no page holds yet,
    /// the blocks closed but not yet written and the open block, each decoded, its gap or break
    /// list's pages and open page and the entries not yet listed, each overview level's rows,
    /// blocks not yet written, open run and settled buckets, and the annotation parts' table, the
    /// parts not yet written and each generator's lines since its last part. Its cost follows
    /// the channels and what waits to be written, not the recording's length.
    ///
    /// What it lists in the file may sit in bytes still in `output`: publish a snapshot to a view
    /// only once those bytes are in the file the view reads.
    pub fn snapshot(&self) -> Snapshot {
        let mut unwritten: Vec<Vec<Unwritten>> = vec![Vec::new(); self.chans.len()];
        let mut queued: Vec<Vec<Vec<Settled>>> = vec![Vec::new(); self.ochans.len()];
        let mut queued_parts = Vec::new();
        // The gaps and breaks a closed block took off its channel's waiting ones, which its list
        // takes once the block is written.
        let mut queued_gaps: Vec<Vec<Gap>> = vec![Vec::new(); self.chans.len()];
        let mut queued_breaks: Vec<Vec<Break>> = vec![Vec::new(); self.chans.len()];
        for item in &self.queue {
            match item {
                Item::Record(_, Placed::Gaps(c, gaps)) => queued_gaps[*c].extend_from_slice(gaps),
                Item::Record(_, Placed::Breaks(c, breaks)) => {
                    queued_breaks[*c].extend_from_slice(breaks)
                }
                _ => {}
            }
            if let Item::Frame { of, raw, .. } = item {
                match of {
                    FrameOf::Block { head, first_index } => {
                        unwritten[head.channel as usize].push(Unwritten {
                            head: *head,
                            raw: Arc::clone(raw),
                            first_index: *first_index,
                        });
                    }
                    FrameOf::Overview { o, li, head } => {
                        let q = &mut queued[*o];
                        if q.len() <= *li {
                            q.resize_with(li + 1, Vec::new);
                        }
                        q[*li].push(Settled {
                            li: *li,
                            shift: head.shift,
                            k: head.k,
                            n: head.n,
                            raw: Arc::clone(raw),
                        });
                    }
                    FrameOf::Part { .. } => queued_parts.push(Arc::clone(raw)),
                }
            }
        }
        let channels = self
            .chans
            .iter()
            .zip(unwritten)
            .enumerate()
            .map(|(c, (ch, mut unwritten))| {
                let (raw, word, gaps, breaks, first_run) = match &ch.data {
                    Data::Digital(d) => {
                        let mut raw = Vec::new();
                        d.open.encode(&mut raw);
                        let word = ch.items | if d.level { LEVEL_BIT } else { 0 };
                        let mut waiting = std::mem::take(&mut queued_gaps[c]);
                        waiting.extend(d.pending.iter().map(|&(g, _)| g));
                        let gaps = ListSnapshot {
                            table: Some(TableIndex::of(&d.list)),
                            open: d.list.open().clone(),
                            waiting,
                        };
                        (raw, word, gaps, ListSnapshot::default(), None)
                    }
                    Data::Analog(a) => {
                        // Then the breaks whose first sample no closed block holds yet.
                        let mut waiting = std::mem::take(&mut queued_breaks[c]);
                        waiting.extend(
                            a.runs
                                .iter()
                                .filter(|r| r.index > 0 && r.index >= ch.closed_items),
                        );
                        let breaks = ListSnapshot {
                            table: Some(TableIndex::of(&a.list)),
                            open: a.list.open().clone(),
                            waiting,
                        };
                        let raw = analog_raw(&a.open, a.coding);
                        (raw, ch.items, ListSnapshot::default(), breaks, a.first_run)
                    }
                };
                unwritten.push(Unwritten {
                    head: BlockHead {
                        channel: c as u32,
                        start: ch.start,
                        end: i64::MAX,
                        word,
                        decoded_length: raw.len() as u32,
                    },
                    raw: Arc::new(raw),
                    first_index: ch.closed_items,
                });
                ChannelSnapshot {
                    promise: ch.promise.max(self.promise),
                    blocks: TableIndex::of(&ch.blocks),
                    rows: ch.blocks.open().clone(),
                    rows_first: ch.blocks.paged_sum(),
                    unwritten,
                    gaps,
                    breaks,
                    first_run,
                }
            })
            .collect();
        let overview = self
            .ochans
            .iter()
            .zip(queued)
            .map(|(oc, queued)| OverviewSnapshot {
                column: oc.column,
                levels: oc
                    .ladder
                    .as_ref()
                    .map(|l| l.held())
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                    .map(|(li, held)| {
                        let table = oc.levels.get(li).and_then(Option::as_ref);
                        LevelSnapshot {
                            shift: held.shift,
                            in_blocks: held.blocks,
                            table: table.map(TableIndex::of),
                            rows: table.map(|t| t.open().clone()).unwrap_or_default(),
                            rows_first: table.map_or(0, TableWriter::paged_sum),
                            queued: queued.get(li).cloned().unwrap_or_default(),
                            run: held.run,
                            held: held.held,
                        }
                    })
                    .collect(),
            })
            .collect();
        Snapshot::new(
            Arc::clone(&self.declaration),
            channels,
            overview,
            Annotations {
                table: TableIndex::of(&self.parts),
                rows: self.parts.open().clone(),
                queued: queued_parts,
                generators: self
                    .generators
                    .iter()
                    .map(|g| (g.source.clone(), g.lines.clone()))
                    .collect(),
            },
            self.position(),
            self.promise,
            self.known,
        )
    }

    /// The recording ends at axis tick `length`. `ends` gives each digital channel's `end`, a
    /// source tick, in channel order. Closes each channel's last block, then writes each
    /// generator's last annotation part, the last pages, the annotation bundle where it took any
    /// annotation or was given files, the footer and the trailer, with `finish`'s extras and
    /// losses.
    pub fn finish(
        mut self,
        length: i64,
        ends: &[i64],
        mut finish: Finish,
    ) -> Result<Finished, WriteError> {
        self.check_failed()?;
        let last = self
            .chans
            .iter()
            .map(|ch| {
                if ch.items > 0 {
                    ch.last_tick.max(ch.start - 1)
                } else {
                    ch.start - 1
                }
            })
            .max()
            .unwrap_or(-1)
            .max(self.known - 1);
        if length < 1 || length <= last {
            return Err(WriteError::Length {
                length,
                last: last.max(0),
            });
        }
        check_safe("length", length as u64)?;
        let digital = self
            .chans
            .iter()
            .filter(|c| matches!(c.data, Data::Digital(_)))
            .count();
        if ends.len() != digital {
            return Err(WriteError::Ends {
                given: ends.len(),
                expected: digital,
            });
        }
        if finish.extra.len() > self.declaration.sources.len() {
            return Err(WriteError::Extras {
                given: finish.extra.len(),
                sources: self.declaration.sources.len(),
            });
        }
        for (s, extra) in finish.extra.iter().enumerate() {
            for key in extra.keys() {
                if self.declaration.sources[s].extra.contains_key(key) {
                    return Err(WriteError::Extra {
                        index: s,
                        key: key.clone(),
                    });
                }
            }
        }
        if let Some(e) = self.pairs.iter().find_map(Pair::unpaired) {
            return Err(e.into());
        }
        for (&c, &lost) in &finish.lost_after {
            let Some(Data::Analog(a)) = self.chans.get(c).map(|ch| &ch.data) else {
                return Err(WriteError::LostAfterChannel(c));
            };
            if lost == 0 {
                continue;
            }
            check_safe("lost count", lost)?;
            let fits = a.runs.back().is_some_and(|&run| {
                lost_after_end(a.grid, run, a.count, lost)
                    .filter(|&u| (0..JSON_SAFE as i64).contains(&a.grid.floor(u)))
                    .and_then(|u| self.chans[c].map.axis_place(u, a.grid.d()))
                    .is_some_and(|t| t <= length)
            });
            if !fits {
                return Err(WriteError::LostAfter {
                    channel: self.name(c),
                    lost,
                });
            }
        }
        let view = finish.view.take();
        let asked = match (finish.bundle.take(), &view) {
            (Some(Bundle::Files(_)), Some(_)) => {
                return Err(WriteError::Bundle(
                    "a view given with files to bundle, whose hand-written file holds its own"
                        .into(),
                ));
            }
            (None, Some(_)) => Some(Bundle::Generated {
                stem: DEFAULT_STEM.into(),
            }),
            (asked, _) => asked,
        };
        let mut bundling = Bundling::of(
            asked,
            &self.generators,
            self.took_records,
            self.declaration.timebase,
        )?;
        // Every channel's end, before anything is closed, so a refusal leaves no half-written
        // footer.
        let mut settled_ends = Vec::with_capacity(self.chans.len());
        let mut ends = ends.iter();
        for (c, ch) in self.chans.iter().enumerate() {
            let Data::Digital(d) = &ch.data else {
                settled_ends.push(None);
                continue;
            };
            let name = self.declaration.channels[c].name.clone();
            let Some(&given) = ends.next() else {
                unreachable!("one end a digital channel, counted above")
            };
            let mut end = given;
            let on_axis = ch
                .map
                .axis(end)
                .filter(|_| (0..JSON_SAFE as i64).contains(&end));
            if end < d.start || on_axis.is_none_or(|t| !(0..=length).contains(&t)) {
                return Err(WriteError::End {
                    channel: name,
                    end,
                    length,
                });
            }
            // A gap already hashed, its block closed, is in the header whatever the end.
            if let Some(g) = d.last_listed
                && g.to >= end
            {
                return Err(WriteError::EndLeavesOutGap {
                    channel: name,
                    from: g.from,
                    to: g.to,
                    end,
                });
            }
            // A waiting gap that reaches the end is left out: the level is not known from its
            // `from` on. A flush past a gap's end closes its block, so the overview never takes
            // the end of a gap still waiting.
            let mut kept = d.gap_count as usize;
            if let Some((i, &(g, to))) =
                d.pending.iter().enumerate().find(|(_, (g, _))| g.to >= end)
            {
                let inside = to < length;
                if inside && !finish.drop_gaps_past_end {
                    return Err(WriteError::GapPastEnd {
                        channel: name,
                        to: g.to,
                        end,
                    });
                }
                end = end.min(g.from);
                kept = d.gap_count as usize - (d.pending.len() - i);
            }
            if d.edges > 0 && d.last_edge >= end {
                return Err(WriteError::EdgePastEnd {
                    channel: name,
                    tick: d.last_edge,
                    end,
                });
            }
            check_safe("edge count", d.edges)?;
            settled_ends.push(Some((end, kept)));
        }
        for ch in &self.chans {
            if let Data::Analog(a) = &ch.data {
                check_safe("analog sample count", a.count)?;
            }
        }
        // The gaps the ends leave out: out of what the last blocks list, and out of the overview.
        for (c, end) in settled_ends.iter().enumerate() {
            if let (Some((_, kept)), Data::Digital(d)) = (end, &mut self.chans[c].data) {
                let out = d.gap_count as usize - kept;
                let first_out = d.pending.len() - out;
                if let Some(&(g, _)) = d.pending.get(first_out) {
                    d.pending.truncate(first_out);
                    self.ochans[c]
                        .ladder
                        .as_mut()
                        .expect("a ladder until the finish")
                        .forget_gaps(g.from);
                }
            }
        }
        for c in 0..self.chans.len() {
            if self.chans[c].start < length {
                self.close(c, length)?;
            }
        }
        // A file a crash cuts off in its last pages or its footer repairs to the length, where
        // known-through records would cut it at the last flush.
        if self.known > 0 && length > self.known {
            self.known = length;
            let mut bytes = Vec::new();
            record::write(
                &mut bytes,
                Kind::KnownThrough,
                &[&record::known_through_bytes(length)],
            );
            self.put(bytes, Placed::Nothing);
        }
        // The overview to the length, each ladder's last blocks and its strip.
        let mut strips = Vec::with_capacity(self.ochans.len());
        let mut ladders = Vec::with_capacity(self.ochans.len());
        for o in 0..self.ochans.len() {
            let ladder = self.ochans[o]
                .ladder
                .take()
                .expect("a ladder until the finish");
            let end = ladder.finish(length);
            for s in end.settled {
                self.queue_overview(o, s);
            }
            strips.push(end.strip);
            ladders.push(end.levels);
        }
        // Each generator's last part, in the order of its first annotation, then the parts a
        // bundle of given files holds.
        for g in 0..self.generators.len() {
            self.write_part(g);
        }
        if let Bundling::Files { parts, .. } = &mut bundling {
            for (raw, span) in parts.drain(..) {
                self.queue_frame(FrameOf::Part { span }, Arc::new(raw), self.options.level);
            }
        }
        self.drain_all()?;

        // Each table's last pages.
        let taken = self.taken;
        for c in 0..self.chans.len() {
            let ch = &mut self.chans[c];
            loop {
                let due = ch.blocks.finish_next(0);
                if due.is_none() {
                    break;
                }
                place_dues(&mut ch.blocks, due, &mut self.out, taken);
            }
            match (&mut ch.data, settled_ends[c]) {
                (Data::Digital(d), Some((_, kept))) => {
                    debug_assert_eq!(d.list.count(), kept as u64, "the gaps the header lists");
                    loop {
                        let due = d.list.finish_next(INLINE_LIST as u64);
                        if due.is_none() {
                            break;
                        }
                        place_dues(&mut d.list, due, &mut self.out, taken);
                    }
                }
                (Data::Analog(a), _) => loop {
                    let due = a.list.finish_next(INLINE_LIST as u64);
                    if due.is_none() {
                        break;
                    }
                    place_dues(&mut a.list, due, &mut self.out, taken);
                },
                _ => unreachable!("a digital channel's end was settled"),
            }
        }
        for oc in &mut self.ochans {
            for table in oc.levels.iter_mut().flatten() {
                loop {
                    let due = table.finish_next(0);
                    if due.is_none() {
                        break;
                    }
                    place_dues(table, due, &mut self.out, taken);
                }
            }
        }
        loop {
            let due = self.parts.finish_next(0);
            if due.is_none() {
                break;
            }
            place_dues(&mut self.parts, due, &mut self.out, taken);
        }

        let mut digests = Vec::with_capacity(self.chans.len());
        let mut channels = Vec::with_capacity(self.chans.len());
        for (c, (ch, declared)) in self
            .chans
            .iter()
            .zip(&self.declaration.channels)
            .enumerate()
        {
            let data = match &ch.data {
                Data::Digital(d) => {
                    let (end, kept) = settled_ends[c].expect("a digital channel's end");
                    debug_assert_eq!(d.listed.count(), kept as u64, "the gaps hashed");
                    let paged = kept > INLINE_LIST;
                    digests.push(ch.hasher.clone().finish_digital(end, d.listed.finish()));
                    ChannelKind::Digital {
                        initial: d.initial,
                        end,
                        edges: d.edges,
                        gap_count: paged.then_some(kept as u64),
                        gaps: if paged { Vec::new() } else { d.list.inline() },
                    }
                }
                Data::Analog(a) => {
                    let first = a.first_run.map_or(0, |r| r.tick);
                    let lost_before = a.first_run.map_or(0, |r| r.lost);
                    let lost_after = finish.lost_after.get(&c).copied().unwrap_or(0);
                    let breaks = a.listed.count();
                    debug_assert_eq!(breaks, a.run_count.saturating_sub(1));
                    digests.push(ch.hasher.clone().finish_analog(
                        first,
                        a.count,
                        a.listed.finish(),
                        (lost_before, lost_after),
                    ));
                    let DeclaredKind::Analog {
                        unit,
                        significant,
                        period,
                        phase,
                        ..
                    } = &declared.data
                    else {
                        unreachable!("states follow the declaration")
                    };
                    let paged = breaks > INLINE_LIST as u64;
                    ChannelKind::Analog {
                        unit: unit.clone(),
                        first,
                        period: *period,
                        phase: *phase,
                        count: a.count,
                        break_count: paged.then_some(breaks),
                        breaks: if paged { Vec::new() } else { a.list.inline() },
                        coding: a.coding,
                        significant: *significant,
                        lost_before,
                        lost_after,
                    }
                }
            };
            channels.push(ChannelHeader {
                name: declared.name.clone(),
                source_name: declared.source_name.clone(),
                source: declared.source,
                data,
                shaping: declared.shaping.clone(),
                overview_shift: declared.overview_shift,
                blocks: ch.blocks_closed,
                overview_blocks: levels_of(&self.ochans[c], &ladders[c], length),
                requires: Vec::new(),
                codec: None,
            });
        }
        let stored = self.chans.len();
        digests.extend(self.declaration.derived.iter().map(hash::derived_digest));
        let derived = self
            .declaration
            .derived
            .iter()
            .enumerate()
            .map(|(d, x)| DerivedHeader {
                name: x.name.clone(),
                unit: x.unit.clone(),
                op: x.op.clone(),
                operands: x.operands.clone(),
                params: x.params.clone(),
                significant: x.significant,
                requires: x.requires.clone(),
                overview_blocks: levels_of(&self.ochans[stored + d], &ladders[stored + d], length),
            })
            .collect();
        let mut sources = self.declaration.sources.clone();
        for (source, extra) in sources.iter_mut().zip(finish.extra) {
            source.extra.extend(extra);
        }
        let header = Header {
            timebase: self.declaration.timebase,
            length,
            channels,
            derived,
            sources,
            title: self.declaration.title.clone(),
            meta: self.declaration.meta.clone(),
            content_id: hash::content_id(self.declaration.timebase, length, &digests),
            overview_shift: self.declaration.overview_shift,
            requires: Vec::new(),
            annotation_parts: (self.parts.count() > 0).then_some(self.parts.count()),
        };
        // The reader's own rules, so the writer never writes a header the reader refuses.
        crate::decode::check_header(&header).map_err(|e| WriteError::Header(e.to_string()))?;
        let files = match bundling {
            Bundling::None => None,
            Bundling::Generated { .. } if self.generators.is_empty() && view.is_none() => None,
            Bundling::Generated { stem } => {
                let files = bundled_files(
                    &stem,
                    header.content_id,
                    self.generators.iter().map(|g| {
                        let roles = self.roles.get(&g.source).map_or(&[][..], Vec::as_slice);
                        (g.source.as_str(), roles)
                    }),
                    view.as_ref(),
                );
                // Held as `annotations check` holds the table, so the bundle never holds a view a
                // reader refuses. Held as a value, not read back from the file's TOML, so the
                // writer needs no parser for a hand-written file.
                if let Some(v) = &view {
                    crate::annotations::view::view(&view_table(v), &header)
                        .map_err(|problems| WriteError::Bundle(problems.join("; ")))?;
                }
                Some(files)
            }
            Bundling::Files { files, .. } => Some(files),
        };
        let bundle = match files {
            None => (0, 0),
            Some(files) => {
                let at = self.position();
                let payload = bundle_payload(&files, self.options.level)?;
                record::write(&mut self.out, Kind::Bundle, &[&payload]);
                (at, (self.position() - at) as u32)
            }
        };
        let tops: Vec<ChannelTops> = (0..self.ochans.len())
            .map(|o| ChannelTops {
                blocks: self.chans.get(o).and_then(|ch| ch.blocks.top()),
                levels: self.ochans[o]
                    .levels
                    .iter()
                    .take(ladders[o].len())
                    .map(|t| t.as_ref().and_then(TableWriter::top))
                    .collect(),
                list: self.chans.get(o).and_then(|ch| match &ch.data {
                    Data::Digital(d) => d.list.top(),
                    Data::Analog(a) => a.list.top(),
                }),
            })
            .collect();
        let footer = footer_payload(
            &header,
            bundle,
            self.parts.top(),
            &tops,
            &strips,
            &self.ochans_columns(),
            self.options.level,
        )?;
        let taken = self.taken;
        record::write_footer(&mut self.out, taken, &footer);
        Ok(Finished {
            header,
            tail: std::mem::take(&mut self.out),
        })
    }

    fn ochans_columns(&self) -> Vec<Column> {
        self.ochans.iter().map(|o| o.column).collect()
    }
}

/// A channel's levels in overview blocks, as its header states them.
fn levels_of(oc: &OChan, shifts: &[u8], length: i64) -> Vec<OverviewLevel> {
    shifts
        .iter()
        .enumerate()
        .map(|(li, &shift)| OverviewLevel {
            shift,
            blocks: overview::block_count(length, shift),
            rows: oc
                .levels
                .get(li)
                .and_then(Option::as_ref)
                .map_or(0, TableWriter::count),
        })
        .collect()
}

/// One channel's tables in the top index: its blocks', its levels', its list's, each a depth and
/// its entries where it has pages.
pub(crate) struct ChannelTops {
    pub blocks: Option<(u8, Vec<Entry>)>,
    pub levels: Vec<Option<(u8, Vec<Entry>)>>,
    pub list: Option<(u8, Vec<Entry>)>,
}

/// The footer's payload: the header, the top index from `bundle`, the annotation parts table's
/// `parts` and each channel's `tops`, and each channel's strip compressed at `level`.
pub(crate) fn footer_payload(
    header: &Header,
    bundle: (u64, u32),
    parts: Option<(u8, Vec<Entry>)>,
    tops: &[ChannelTops],
    strips: &[overview::Level],
    columns: &[Column],
    level: i32,
) -> Result<Vec<u8>, WriteError> {
    let json = serde_json::to_vec(header).expect("a header always serialises");
    let mut top = Vec::new();
    top.extend_from_slice(&bundle.0.to_le_bytes());
    top.extend_from_slice(&bundle.1.to_le_bytes());
    let mut frames = Vec::new();
    let table = |top: &mut Vec<u8>, t: &(u8, Vec<Entry>)| {
        top.push(t.0);
        top.push(t.1.len() as u8);
        for e in &t.1 {
            e.write(top);
        }
    };
    if let Some(t) = &parts {
        table(&mut top, t);
    }
    for ((tops, strip), &column) in tops.iter().zip(strips).zip(columns) {
        let raw = strip.to_bytes(column);
        let frame = compress(&raw, level)?;
        top.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        top.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        frames.extend_from_slice(&frame);
        if let Some(t) = &tops.blocks {
            table(&mut top, t);
        }
        for t in tops.levels.iter().flatten() {
            table(&mut top, t);
        }
        if let Some(t) = &tops.list {
            table(&mut top, t);
        }
    }
    record::footer_bytes(&json, &top, &frames).map_err(WriteError::FooterTooLarge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::{DeclaredChannel, IntCoding, SourceFormat, SourceHeader};
    use crate::model::Ratio;

    fn writer() -> Writer {
        let timebase = Ratio::integer(1000).unwrap();
        let d = Declaration {
            timebase,
            channels: vec![
                DeclaredChannel {
                    name: "V".into(),
                    source_name: "v".into(),
                    source: 0,
                    data: DeclaredKind::Analog {
                        unit: "V".into(),
                        period: Ratio::integer(1).unwrap(),
                        coding: AnalogCoding::Int(IntCoding::new(32, false)),
                        significant: None,
                        phase: None,
                    },
                    shaping: Default::default(),
                    overview_shift: None,
                    codec: None,
                    requires: Vec::new(),
                },
                DeclaredChannel {
                    name: "D".into(),
                    source_name: "d".into(),
                    source: 0,
                    data: DeclaredKind::Digital { initial: false },
                    shaping: Default::default(),
                    overview_shift: None,
                    codec: None,
                    requires: Vec::new(),
                },
            ],
            derived: Vec::new(),
            sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
            title: None,
            meta: Default::default(),
            overview_shift: 4,
            requires: Vec::new(),
        };
        Writer::new(d, WriteOptions::default()).unwrap()
    }

    /// FORMAT.md, "Annotations": a re-cutter's lines go in parts that stand alone and carry its
    /// input's bundle as files given, which must hold a file for each of their generators. A
    /// writer that wrote records of its own can't take files given.
    #[test]
    fn standalone_lines_go_with_given_files_that_name_their_generators() {
        let line = r#"{"id":"g/1","start":1}"#;
        let files = |source: &str| {
            let id: ContentId =
                "blake3:00ff000000000000000000000000000000000000000000000000000000000000"
                    .parse()
                    .unwrap();
            BTreeMap::from([(
                "rec.g.jsonl".to_owned(),
                generator_file(source, None, id, &[]),
            )])
        };
        let finish = |files| Finish {
            bundle: Some(Bundle::Files(files)),
            ..Finish::default()
        };
        let mut w = writer();
        w.standalone_line("g", line).unwrap();
        let f = w.finish(10, &[10], finish(files("g"))).unwrap();
        assert_eq!(f.header.annotation_parts, Some(1));
        let mut w = writer();
        w.standalone_line("g", line).unwrap();
        match w.finish(10, &[10], finish(files("other"))) {
            Err(WriteError::Bundle(e)) => assert!(e.ends_with("has none"), "{e}"),
            other => panic!("{other:?}"),
        }
        let mut w = writer();
        w.annotation("g", serde_json::from_str(line).unwrap())
            .unwrap();
        match w.finish(10, &[10], finish(files("g"))) {
            Err(WriteError::Bundle(e)) => assert!(e.ends_with("of its own"), "{e}"),
            other => panic!("{other:?}"),
        }
    }

    /// An item that would take its open block's decoded size past 4 MiB, the most a block
    /// holds, is refused at the item where the block can't close before it: here the open
    /// blocks are said to be that full already. Once a promise lets the block close, the item
    /// goes in.
    #[test]
    fn an_item_past_the_most_a_block_holds_is_refused_unless_the_block_can_close() {
        let mut w = writer();
        w.run(0, 0, 0).unwrap();
        w.sample(0, 1u64).unwrap();
        // Two edges, so a third two ticks on keeps the step and adds an interval's byte.
        w.edge(1, 5).unwrap();
        w.edge(1, 7).unwrap();
        w.chans[0].size = MAX_BLOCK - 3;
        let Data::Digital(d) = &mut w.chans[1].data else {
            unreachable!()
        };
        // A step byte, the first edge's byte and these: one more interval's byte passes 4 MiB.
        d.open.intervals = MAX_BLOCK - 2;
        assert!(matches!(
            w.sample(0, 2u64),
            Err(WriteError::BlockFull { tick: 1, .. })
        ));
        assert!(matches!(
            w.edge(1, 9),
            Err(WriteError::BlockFull { tick: 9, .. })
        ));
        // A sample that still fits goes in.
        w.chans[0].size = MAX_BLOCK - 4;
        w.sample(0, 2u64).unwrap();
        // A promise past the items held lets each block close before the item: it goes in.
        w.chans[0].size = MAX_BLOCK - 3;
        let Data::Digital(d) = &mut w.chans[1].data else {
            unreachable!()
        };
        d.open.intervals = MAX_BLOCK - 2;
        w.promise_channel(0, 2).unwrap();
        w.promise_channel(1, 8).unwrap();
        w.sample(0, 3u64).unwrap();
        w.edge(1, 9).unwrap();
        assert_eq!((w.chans[0].start, w.chans[1].start), (2, 8));
    }

    /// The section's size worked out from the edges alone, as FORMAT.md defines it.
    fn section_size(ticks: &[i64]) -> usize {
        let intervals = || ticks.windows(2).map(|w| (w[1] - w[0]) as u64);
        let step = intervals().fold(0, gcd).max(1);
        uleb_len(step)
            + uleb_len(ticks[0] as u64)
            + intervals().map(|i| uleb_len(i / step)).sum::<usize>()
    }

    /// Each edge's size, and the size `size_with` gives first, are the section's, through steps
    /// that shrink by powers of two and not, with intervals over the new step on both sides of
    /// each byte's reach and sharing its bit length.
    #[test]
    fn a_shrinking_steps_size_is_the_sections() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        // Intervals over 60 of 5 and 7, 300 and 420, sit either side of the reach 384 of a step
        // of 3, both 9 bits long.
        let mut cases: Vec<Vec<i64>> = vec![vec![0, 300, 720, 1020, 1083]];
        for _ in 0..400 {
            let base = [96u64, 960, 60, 1000, 3 << 10, 7 * 128][next(6) as usize];
            let mut t = next(1 << 20) as i64;
            let mut ticks = vec![t];
            for _ in 0..1 + next(300) {
                // Quotients around 2^7 and 2^14 over the base and over its divisors.
                let q = [
                    1 + next(4),
                    100 + next(60),
                    16_000 + next(800),
                    next(1 << 30),
                ][next(4) as usize];
                t += (base * q) as i64;
                ticks.push(t);
            }
            // An edge off the grid: the step shrinks to a divisor of the base, or to 1.
            t += [1, 2, 3, 5, base as i64 / 2, base as i64 / 3][next(6) as usize].max(1);
            ticks.push(t);
            for _ in 0..next(20) {
                t += (base * (1 + next(200))) as i64;
                ticks.push(t);
            }
            cases.push(ticks);
        }
        for ticks in cases {
            let mut run = EdgeRun::default();
            for (i, &t) in ticks.iter().enumerate() {
                let ahead = run.size_with(t);
                let size = run.push(t);
                let want = section_size(&ticks[..=i]);
                assert_eq!(size, want, "{:?} at {i}", &ticks[..=i]);
                assert_eq!(ahead, want, "size_with at {i}");
            }
        }
    }

    #[test]
    fn an_odd_numbers_inverse_times_it_is_one() {
        for a in [
            1u64,
            3,
            5,
            7,
            95,
            99,
            255,
            3 * 128 + 1,
            u64::MAX,
            u64::MAX - 2,
            1 << 63 | 1,
        ] {
            assert_eq!(a.wrapping_mul(odd_inverse(a)), 1, "{a}");
        }
    }

    /// A section's bytes are the step, the first edge and each interval over the step, divided as
    /// integers, whatever the step: odd, even, a power of two, or 1.
    #[test]
    fn a_section_codes_each_interval_over_the_step() {
        for step in [1u64, 2, 3, 96, 960, 1 << 20, 7 * 128, 3 << 40] {
            let ticks: Vec<i64> = [5i64, 5, 6, 12, 300, 301, 16_400, 1 << 20]
                .iter()
                .scan(1_000i64, |t, &q| {
                    *t += q * step as i64;
                    Some(*t)
                })
                .collect();
            let mut want = Vec::new();
            put_uleb(&mut want, step);
            put_uleb(&mut want, ticks[0] as u64);
            for w in ticks.windows(2) {
                put_uleb(&mut want, (w[1] - w[0]) as u64 / step);
            }
            let mut got = Vec::new();
            let run = EdgeRun::of(&ticks);
            assert_eq!(run.step(), step, "{ticks:?}");
            run.encode(&mut got);
            assert_eq!(got, want, "step {step}");
            assert_eq!(got.len(), run.size(), "step {step}");
        }
    }

    /// `size_with` gives the size `push` then gives, through a step that shrinks.
    #[test]
    fn an_edges_size_with_is_its_size_once_pushed() {
        let mut run = EdgeRun::default();
        for t in [
            1000i64,
            3000,
            5000,
            5000,
            6001,
            70_000_000_000,
            70_000_000_003,
        ] {
            let ahead = run.size_with(t);
            assert_eq!(run.push(t), ahead, "at {t}");
        }
    }
}
