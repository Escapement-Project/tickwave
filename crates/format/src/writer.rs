//! The writer: a recording's items in, the file's bytes out, with no I/O of its own.
//!
//! A caller declares the recording, then hands in each channel's items in that channel's order,
//! in its source's ticks, and promises as it goes that every item before a tick is in. Each
//! channel's block closes at a promise once it reaches its size, or at `flush`, which a live
//! recorder calls on its own clock and which writes a known-through record. Frames are
//! compressed a slice at a time over the calls that follow, on the caller's thread, or on other
//! threads by option. The caller drains the bytes as they come: `output` and `consume`, or
//! `take_output`. `snapshot` gives a view in the same process what is held so far. `finish`
//! closes each channel's last block and writes the last pages, the annotation bundle if asked,
//! the footer and the trailer.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use crate::codec::put_uleb;
use crate::compress::{SLICE, Slicer, Slot, compress};
use crate::decode::{check_declaration, level_after, lost_after_end, lost_before_start};
use crate::hash::{self, ChannelHasher};
use crate::header::{
    AnalogCoding, BlockRow, ChannelHeader, ChannelKind, ContentId, Declaration, DeclaredKind,
    DerivedHeader, Header, INLINE_LIST, JSON_SAFE, LEVEL_BIT, OverviewLevel, OverviewRow, Preamble,
    code_value,
};
use crate::index::{Due, Entry, Row, Table, TableIndex, TableWriter};
use crate::mapping::Mapping;
use crate::model::{Break, Gap, Ratio, Sample};
use crate::overview::{self, Column, Ladder, Pair, Settled, Unpaired};
use crate::record::{self, BlockHead, Kind, OverviewHead};
use crate::samples;
use crate::shared::Chunks;
use crate::snapshot::{ChannelSnapshot, LevelSnapshot, OverviewSnapshot, Snapshot, Unwritten};

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("{what} {value} does not fit the header, which holds integers below 2^53")]
    TooLarge { what: &'static str, value: u64 },
    #[error("the block size target must lie between 1 and 2^30 bytes")]
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
        "{channel}: the end leaves out a gap already written in a list page, which can't change"
    )]
    GapPaged { channel: String },
    #[error("source {index}'s extra already holds {key}")]
    Extra { index: usize, key: String },
    #[error("extra keys for {given} sources, but the recording has {sources}")]
    Extras { given: usize, sources: usize },
    /// A caller's annotation that would not resolve in a sidecar: which, and the rule it breaks.
    #[error("{0}")]
    Annotation(String),
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
    /// The zstd level for blocks, strips and the annotation bundle.
    pub level: i32,
    /// The decoded size at which a channel's block closes, at its next promise.
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
#[derive(Clone, Debug, Default)]
struct EdgeRun {
    ticks: Vec<i64>,
    /// The greatest common divisor of the intervals that are not zero; 0 while there is none.
    step: u64,
    /// The intervals' bytes over `step`.
    intervals: usize,
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
            let step = step.max(1);
            self.ticks
                .windows(2)
                .map(|w| uleb_len((w[1] - w[0]) as u64 / step))
                .sum::<usize>()
                + uleb_len(interval / step)
        };
        uleb_len(step.max(1)) + uleb_len(first as u64) + intervals
    }

    /// Takes an edge at `tick`, no earlier than the last, and returns the section's new size.
    fn push(&mut self, tick: i64) -> usize {
        if let Some(&last) = self.ticks.last() {
            let interval = (tick - last) as u64;
            let step = gcd(self.step, interval);
            self.ticks.push(tick);
            if step != self.step {
                // A smaller step: every interval is coded again. The step only shrinks, by a
                // factor of 2 at least each time, so this happens fewer than 64 times a block.
                self.step = step;
                let step = self.step();
                self.intervals = self
                    .ticks
                    .windows(2)
                    .map(|w| uleb_len((w[1] - w[0]) as u64 / step))
                    .sum();
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
        for w in self.ticks.windows(2) {
            put_uleb(out, (w[1] - w[0]) as u64 / step);
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
    gaps: Chunks<Gap>,
    /// Gaps whose `to` no closed block has passed, each with its `to` on the axis.
    pending: VecDeque<(Gap, i64)>,
    list: TableWriter<Gap>,
}

struct AnalogState {
    period: Ratio,
    coding: AnalogCoding,
    /// Every run so far, the first at index 0.
    runs: Chunks<Break>,
    /// A run the next sample starts: its source tick and the samples lost before it.
    next_run: Option<(i64, u64)>,
    count: u64,
    /// The last sample's place, a source tick.
    last_tick: Option<i64>,
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

enum Placed {
    Nothing,
    Gap(usize, Gap),
    Run(usize, Break),
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
    /// Each generator's annotations, its file's lines after the first, by source.
    pub generated: BTreeMap<String, Vec<String>>,
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
    /// Bundle annotations into the file, so the recording is one file.
    pub bundle: Option<Bundle>,
    /// Take a gap that reaches a digital channel's end as ending what is known at its `from`,
    /// even where the gap ends inside the recording, as repair does for a file cut off. Otherwise
    /// such a gap is refused, as most likely a caller's mistake.
    pub drop_gaps_past_end: bool,
    /// Samples the instrument lost after an analog channel's last, by channel index.
    pub lost_after: BTreeMap<usize, u64>,
}

/// What a finished file bundles in its annotation bundle record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bundle {
    /// The writer's own annotations: each source's generator file `<stem>.<source>.jsonl`, and
    /// a hand-written `<stem>.annotations.toml` that holds no annotation and includes them, where
    /// `<stem>` names the recording `<stem>.tickwave`.
    Generated { stem: String },
    /// These files exactly: a re-cutter carries a file's bundle over.
    Files(BTreeMap<String, String>),
}

/// The files a bundle of `generated` holds for a recording `<stem>.tickwave` with `id`: each
/// source's generator file, and a hand-written file that includes them and holds no annotation
/// of its own.
pub(crate) fn bundled_files(
    stem: &str,
    id: ContentId,
    generated: &BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, String> {
    // A JSON string is a TOML basic string: serde_json escapes nothing TOML lacks.
    let quoted = |s: &str| serde_json::to_string(s).expect("a string serialises");
    let mut files = BTreeMap::new();
    let mut include = Vec::new();
    for (source, lines) in generated {
        let name = format!("{stem}.{source}.jsonl");
        files.insert(name.clone(), generator_file(source, None, id, lines));
        include.push(quoted(&name));
    }
    files.insert(
        format!("{stem}.annotations.toml"),
        format!(
            "format = \"tickwave-annotations/1\"\nrecording_id = {}\ninclude = [{}]\n",
            quoted(&id.to_string()),
            include.join(", ")
        ),
    );
    files
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
    // In the order ANNOTATIONS.md shows it.
    let json = |s: &str| serde_json::to_string(s).expect("a string serialises");
    let version = version.map_or(String::new(), |v| format!(r#","version":{}"#, json(v)));
    let mut out = format!(
        r#"{{"format":"tickwave-annotations/1","source":{}{version},"recording_id":{}}}"#,
        json(source),
        json(&recording_id.to_string())
    );
    out.push('\n');
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Drops an annotation that starts at or past `length`, and ends a span that runs past it at the
/// length. Times are axis ticks.
pub(crate) fn clip_annotation(
    mut a: serde_json::Map<String, serde_json::Value>,
    length: i64,
) -> Option<String> {
    let start = a.get("start")?.as_i64()?;
    if start >= length {
        return None;
    }
    if let Some(end) = a.get("end").and_then(|e| e.as_i64())
        && end > length
    {
        a.insert("end".into(), length.into());
    }
    Some(serde_json::to_string(&a).expect("a map serialises"))
}

/// Each source's annotations clipped to `length`, as its generator file's lines.
pub(crate) fn generated<'a>(
    annotations: impl IntoIterator<Item = &'a record::Annotation>,
    length: i64,
) -> BTreeMap<String, Vec<String>> {
    let mut generated: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for a in annotations {
        if let Some(line) = clip_annotation(a.annotation.clone(), length) {
            generated.entry(a.source.clone()).or_default().push(line);
        }
    }
    generated
}

fn check_safe(what: &'static str, value: u64) -> Result<(), WriteError> {
    if value < JSON_SAFE {
        Ok(())
    } else {
        Err(WriteError::TooLarge { what, value })
    }
}

/// The run that holds sample `index` of a channel whose runs are `runs`, the first at index 0.
fn run_of(runs: &Chunks<Break>, index: u64) -> Option<Break> {
    let (mut lo, mut hi) = (0, runs.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if runs.get(mid)?.index <= index {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    runs.get(lo.checked_sub(1)?).copied()
}

/// Sample `index`'s place on the run `run` starts: a source tick, saturating.
#[inline]
fn place_in(run: Break, period: Ratio, index: u64) -> i64 {
    // A whole period, as most are, needs no division.
    let steps = if period.den() == 1 {
        u128::from(index - run.index) * u128::from(period.num())
    } else {
        u128::from(index - run.index) * u128::from(period.num()) / u128::from(period.den())
    };
    i64::try_from(steps)
        .ok()
        .and_then(|s| run.tick.checked_add(s))
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
    annotations: Chunks<record::Annotation>,
    lines: crate::annotations::Lines,
    /// A compression that failed where no error could be returned.
    failed: Option<String>,
}

impl Writer {
    /// Writes the preamble and the declaration to the output. The declaration is held to the
    /// rules a reader holds it to.
    pub fn new(declaration: Declaration, options: WriteOptions) -> Result<Self, WriteError> {
        if options.block_bytes == 0 || options.block_bytes > 1 << 30 {
            return Err(WriteError::BlockSize);
        }
        check_declaration(&declaration).map_err(|e| WriteError::Declaration(e.to_string()))?;
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
                        gaps: Chunks::new(),
                        pending: VecDeque::new(),
                        list: TableWriter::new(c as u32, Table::Gaps, true),
                    })
                }
                DeclaredKind::Analog { period, coding, .. } => Data::Analog(AnalogState {
                    period: *period,
                    coding: *coding,
                    runs: Chunks::new(),
                    next_run: None,
                    count: 0,
                    last_tick: None,
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
            .map(|(o, &column)| {
                let initial = match declaration.channels.get(o).map(|c| &c.data) {
                    Some(DeclaredKind::Digital { initial }) => Some(*initial),
                    _ => None,
                };
                let shift = declaration
                    .shift(o)
                    .expect("a checked declaration's channel");
                OChan {
                    ladder: Some(Ladder::new(shift, column, initial)),
                    column,
                    levels: Vec::new(),
                }
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
            annotations: Chunks::new(),
            lines,
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
        let max = if matches!(self.chans[c].data, Data::Digital(_)) {
            LEVEL_BIT - 1
        } else {
            u32::MAX
        };
        let ch = &self.chans[c];
        if ch.items >= max {
            return Err(WriteError::BlockTooLarge(ch.items as usize));
        }
        let size = match (&ch.data, edge) {
            (Data::Digital(d), Some(tick)) if d.open.may_pass(u32::MAX as usize) => {
                d.open.size_with(tick)
            }
            (Data::Digital(_), Some(_)) => 0,
            (Data::Analog(a), _) => ch.size + a.coding.sample_len(),
            (Data::Digital(_), None) => unreachable!("a digital channel's item is an edge"),
        };
        if size > u32::MAX as usize {
            return Err(WriteError::BlockTooLarge(size));
        }
        Ok(())
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
        if ch.size >= self.options.block_bytes && !ch.full {
            ch.full = true;
            self.full.push(c);
            self.try_close(c)?;
            if !self.chans[c].full {
                self.full.retain(|&x| x != c);
            }
        }
        self.advance()
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
        let ch = &self.chans[c];
        if p > ch.start
            && ch.items > 0
            && ch.first_tick < p
            && self.size_before(c, p) >= self.options.block_bytes
        {
            self.close(c, p)?;
        }
        let ch = &mut self.chans[c];
        ch.full = ch.size >= self.options.block_bytes;
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
                    if ch.map.axis_saturating(place_in(run, a.period, index)) < p {
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
        if let Some(g) = d.gaps.last()
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
        let last = d.gaps.last().map_or(d.last_edge, |g| g.to.max(d.last_edge));
        let after_last = if d.gaps.is_empty() && d.edges == 0 {
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
        if to < self.chans[c].start {
            return Err(WriteError::GapLate {
                channel: name,
                to: gap.to,
                closed: self.chans[c].start,
            });
        }
        let Data::Digital(d) = &mut self.chans[c].data else {
            unreachable!()
        };
        d.gaps.push(gap);
        d.pending.push_back((gap, to));
        self.ochans[c]
            .ladder
            .as_mut()
            .expect("a ladder until the finish")
            .gap(gap, to);
        let mut bytes = Vec::new();
        record::write(&mut bytes, Kind::Gap, &[&record::gap_bytes(c as u32, gap)]);
        self.put(bytes, Placed::Gap(c, gap));
        self.advance()
    }

    /// Analog channel `c`'s next sample starts a run at source tick `tick`, after `lost` samples
    /// the instrument lost: 0 when it only paused. The first run's `lost` are samples lost
    /// before the channel's first. A run no sample has followed yet is replaced.
    pub fn run(&mut self, c: usize, tick: i64, lost: u64) -> Result<(), WriteError> {
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
        let previous = a.last_tick.unwrap_or(0);
        if tick < previous {
            return Err(WriteError::RunOrder {
                channel: name,
                tick,
                previous,
            });
        }
        if a.count == 0 && lost > 0 {
            let start = lost_before_start(tick, a.period, lost);
            if start.is_none_or(|t| map.axis(t).is_none_or(|t| t < 0)) {
                return Err(WriteError::LostBefore {
                    channel: name,
                    lost,
                });
            }
        }
        a.next_run = Some((tick, lost));
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
        let (run, starts) = match (a.next_run, a.runs.last()) {
            (Some((tick, lost)), _) => (Break { index, tick, lost }, true),
            (None, Some(run)) => (*run, false),
            (None, None) => {
                return Err(WriteError::NoRun {
                    channel: self.name(c),
                });
            }
        };
        let place = place_in(run, a.period, index);
        let coding = a.coding;
        let tick = self.place(c, place)?;
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
            self.pairs[d].pairs_with(c, tick)?;
        }
        self.before_item(c, tick, None)?;
        if starts {
            let mut bytes = Vec::new();
            record::write(&mut bytes, Kind::Run, &[&record::run_bytes(c as u32, run)]);
            self.put(bytes, Placed::Run(c, run));
        }
        let ch = &mut self.chans[c];
        let Data::Analog(a) = &mut ch.data else {
            unreachable!()
        };
        if starts {
            a.runs.push(run);
            a.next_run = None;
        }
        samples::put(&mut a.open, sample, coding, code);
        let size = a.open.len();
        a.count += 1;
        a.last_tick = Some(place);
        ch.hasher.sample(sample);
        self.ochans[c]
            .ladder
            .as_mut()
            .expect("a ladder until the finish")
            .sample(tick, sample);
        let stored = self.chans.len();
        for i in 0..self.operand_of[c].len() {
            let d = self.operand_of[c][i];
            if let Some(product) = self.pairs[d]
                .take(c, tick, sample)
                .expect("pairs_with checked the derived channels")
            {
                self.ochans[stored + d]
                    .ladder
                    .as_mut()
                    .expect("a ladder until the finish")
                    .sample(tick, product);
            }
        }
        self.after_item(c, tick, size)
    }

    /// One generated annotation, from generator `source`. Its times are axis ticks.
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
        let a = record::Annotation {
            source: source.to_owned(),
            annotation,
        };
        let json = serde_json::to_vec(&a).expect("an annotation serialises");
        let mut bytes = Vec::new();
        record::write(&mut bytes, Kind::Annotation, &[&json]);
        self.put(bytes, Placed::Nothing);
        self.annotations.push(a);
        self.advance()
    }

    /// Every item before axis tick `before` is in: promises it, closes there every channel's open
    /// block that holds an item before it, and writes a known-through record after those blocks.
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
            let ch = &self.chans[c];
            if ch.start < before && ch.items > 0 && ch.first_tick < before {
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
                    if ch.map.axis_saturating(place_in(run, a.period, index)) < end {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                let rest = a.open.split_off(lo * len);
                let raw = analog_raw(&a.open, a.coding);
                let size = rest.len();
                a.open = rest;
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
        ch.start = end;
        ch.items = items_left;
        ch.size = kept_size;
        if items_left > 0 {
            self.chans[c].first_tick = self.first_open_tick(c);
        }
        let level = self.options.level;
        self.queue_frame(FrameOf::Block { head, first_index }, Arc::new(raw), level);
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

    /// The axis tick of the first item in channel `c`'s open block, which holds one.
    fn first_open_tick(&self, c: usize) -> i64 {
        let ch = &self.chans[c];
        match &ch.data {
            Data::Digital(d) => ch.map.axis_saturating(d.open.ticks[0]),
            Data::Analog(a) => {
                let index = ch.closed_items;
                let run = run_of(&a.runs, index).expect("a sample follows its run");
                ch.map.axis_saturating(place_in(run, a.period, index))
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

    /// A record without a frame now in the file at `at`: its list takes it.
    fn placed(&mut self, placed: Placed, _at: u64) {
        let taken = self.taken;
        match placed {
            Placed::Nothing => {}
            Placed::Gap(c, gap) => {
                if let Data::Digital(d) = &mut self.chans[c].data {
                    let due = d.list.push(gap);
                    place_dues(&mut d.list, due, &mut self.out, taken);
                }
            }
            Placed::Run(c, run) => {
                if run.index > 0
                    && let Data::Analog(a) = &mut self.chans[c].data
                {
                    let due = a.list.push(run);
                    place_dues(&mut a.list, due, &mut self.out, taken);
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
    /// the blocks closed but not yet written and the open block, each decoded, its gaps and runs,
    /// each overview level's rows, blocks not yet written, open run and settled buckets, and the
    /// annotations. Its cost follows the channels and what waits to be written, not the
    /// recording's length.
    ///
    /// What it lists in the file may sit in bytes still in `output`: publish a snapshot to a view
    /// only once those bytes are in the file the view reads.
    pub fn snapshot(&self) -> Snapshot {
        let mut unwritten: Vec<Vec<Unwritten>> = vec![Vec::new(); self.chans.len()];
        let mut queued: Vec<Vec<Vec<Settled>>> = vec![Vec::new(); self.ochans.len()];
        for item in &self.queue {
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
                }
            }
        }
        let channels = self
            .chans
            .iter()
            .zip(unwritten)
            .enumerate()
            .map(|(c, (ch, mut unwritten))| {
                let (raw, word, gaps, runs) = match &ch.data {
                    Data::Digital(d) => {
                        let mut raw = Vec::new();
                        d.open.encode(&mut raw);
                        let word = ch.items | if d.level { LEVEL_BIT } else { 0 };
                        (raw, word, d.gaps.clone(), Chunks::new())
                    }
                    Data::Analog(a) => (
                        analog_raw(&a.open, a.coding),
                        ch.items,
                        Chunks::new(),
                        a.runs.clone(),
                    ),
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
                    runs,
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
            self.annotations.clone(),
            self.position(),
            self.promise,
            self.known,
        )
    }

    /// The recording ends at axis tick `length`. `ends` gives each digital channel's `end`, a
    /// source tick, in channel order. Closes each channel's last block, then writes the last
    /// pages, the annotation bundle if asked, the footer and the trailer, with `finish`'s extras
    /// and losses.
    pub fn finish(
        mut self,
        length: i64,
        ends: &[i64],
        finish: Finish,
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
            let fits = a.runs.last().is_some_and(|&run| {
                lost_after_end(run, a.period, a.count, lost)
                    .and_then(|k| {
                        self.chans[c]
                            .map
                            .axis(k)
                            .filter(|_| (0..JSON_SAFE as i64).contains(&k))
                    })
                    .is_some_and(|t| t <= length)
            });
            if !fits {
                return Err(WriteError::LostAfter {
                    channel: self.name(c),
                    lost,
                });
            }
        }
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
            // A gap that reaches the end: the level is not known from its `from` on.
            let mut kept = d.gaps.len();
            if let Some((i, &g)) = d.gaps.iter().enumerate().find(|(_, g)| g.to >= end) {
                let inside = ch.map.axis(g.to).is_some_and(|t| t < length);
                if inside && !finish.drop_gaps_past_end {
                    return Err(WriteError::GapPastEnd {
                        channel: name,
                        to: g.to,
                        end,
                    });
                }
                end = end.min(g.from);
                kept = i;
            }
            // Gaps go in list pages a page at a time as they come: one already written can't be
            // left out.
            let paged = d.gaps.len() / crate::index::PAGE * crate::index::PAGE;
            if kept < paged {
                return Err(WriteError::GapPaged { channel: name });
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
                    let dropped = d.list.count() - kept as u64;
                    d.list
                        .drop_last(dropped)
                        .expect("a dropped gap is in the open page, checked above");
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
                    let gaps: Vec<Gap> = d.gaps.iter().take(kept).copied().collect();
                    let paged = gaps.len() > INLINE_LIST;
                    digests.push(ch.hasher.clone().finish_digital(end, &gaps));
                    ChannelKind::Digital {
                        initial: d.initial,
                        end,
                        edges: d.edges,
                        gap_count: paged.then_some(gaps.len() as u64),
                        gaps: if paged { Vec::new() } else { gaps },
                    }
                }
                Data::Analog(a) => {
                    let first = a.runs.get(0).map_or(0, |r| r.tick);
                    let lost_before = a.runs.get(0).map_or(0, |r| r.lost);
                    let lost_after = finish.lost_after.get(&c).copied().unwrap_or(0);
                    let breaks: Vec<Break> = a.runs.iter().skip(1).copied().collect();
                    digests.push(ch.hasher.clone().finish_analog(
                        first,
                        a.count,
                        &breaks,
                        (lost_before, lost_after),
                    ));
                    let DeclaredKind::Analog {
                        unit, significant, ..
                    } = &declared.data
                    else {
                        unreachable!("states follow the declaration")
                    };
                    let paged = breaks.len() > INLINE_LIST;
                    ChannelKind::Analog {
                        unit: unit.clone(),
                        first,
                        period: a.period,
                        count: a.count,
                        break_count: paged.then_some(breaks.len() as u64),
                        breaks: if paged { Vec::new() } else { breaks },
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
            });
        }
        let stored = self.chans.len();
        let derived = self
            .declaration
            .derived
            .iter()
            .enumerate()
            .map(|(d, x)| DerivedHeader {
                name: x.name.clone(),
                unit: x.unit.clone(),
                product: x.product.clone(),
                significant: x.significant,
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
        };
        // The reader's own rules, so the writer never writes a header the reader refuses.
        crate::decode::check_header(&header).map_err(|e| WriteError::Header(e.to_string()))?;
        let generated = generated(self.annotations.iter(), length);
        let files = match finish.bundle {
            None => None,
            Some(Bundle::Generated { stem }) => {
                Some(bundled_files(&stem, header.content_id, &generated))
            }
            Some(Bundle::Files(files)) => Some(files),
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
            generated,
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

/// The footer's payload: the header, the top index from `bundle` and each channel's `tops`, and
/// each channel's strip compressed at `level`.
pub(crate) fn footer_payload(
    header: &Header,
    bundle: (u64, u32),
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
                    },
                    shaping: Default::default(),
                    overview_shift: None,
                },
                DeclaredChannel {
                    name: "D".into(),
                    source_name: "d".into(),
                    source: 0,
                    data: DeclaredKind::Digital { initial: false },
                    shaping: Default::default(),
                    overview_shift: None,
                },
            ],
            derived: Vec::new(),
            sources: vec![SourceHeader::new(SourceFormat::Other("t".into()), timebase)],
            title: None,
            meta: Default::default(),
            overview_shift: 4,
        };
        Writer::new(d, WriteOptions::default()).unwrap()
    }

    /// An item that would take its open block's decoded size past what a block record's length
    /// holds is refused at the item, before the writer holds it: here the open blocks are said
    /// to be that large already, as 4 GiB of items would leave them.
    #[test]
    fn an_item_past_a_blocks_length_is_refused_at_the_item() {
        let mut w = writer();
        w.run(0, 0, 0).unwrap();
        w.sample(0, 1u64).unwrap();
        // Two edges, so a third two ticks on keeps the step and adds an interval's byte.
        w.edge(1, 5).unwrap();
        w.edge(1, 7).unwrap();
        w.chans[0].size = u32::MAX as usize - 3;
        let Data::Digital(d) = &mut w.chans[1].data else {
            unreachable!()
        };
        // A step byte, the first edge's byte and these: one more interval's byte passes u32.
        d.open.intervals = u32::MAX as usize - 2;
        assert!(matches!(
            w.sample(0, 2u64),
            Err(WriteError::BlockTooLarge(n)) if n == u32::MAX as usize + 1
        ));
        assert!(matches!(w.edge(1, 9), Err(WriteError::BlockTooLarge(_))));
        // A sample that still fits goes in.
        w.chans[0].size = u32::MAX as usize - 4;
        w.sample(0, 2u64).unwrap();
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
