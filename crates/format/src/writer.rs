//! The writer: a recording's items in, the file's bytes out, with no I/O of its own.
//!
//! A caller declares the recording, then hands in its items in axis order across the channels,
//! each in its own source's ticks, and drains the bytes as they come: `output` and `consume`, or
//! `take_output`. Blocks close on their size, or at `flush`, which a live recorder calls on its
//! own clock. `snapshot` gives a view in the same process what is written so far. `finish` closes
//! the last block and writes the footer and the trailer, bundling the annotations if asked.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use rayon::prelude::*;

use crate::codec::put_uleb;
use crate::decode::check_declaration;
use crate::hash::{self, ChannelHasher};
use crate::header::{
    AnalogCoding, BlockRow, ChannelHeader, ChannelKind, ContentId, Declaration, DeclaredKind,
    Header, JSON_SAFE, LEVEL_BIT, OverviewLevel, OverviewRow, Preamble, Words, code_value,
};
use crate::mapping::Mapping;
use crate::model::{Break, Gap, Ratio, Sample};
use crate::overview::{self, Builder, Settled, Unpaired};
use crate::record::{self, BlockHead, Kind};
use crate::samples;
use crate::shared::Chunks;
use crate::snapshot::{Snapshot, Written};

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("{what} {value} does not fit the header, which holds integers below 2^53")]
    TooLarge { what: &'static str, value: u64 },
    #[error("the block size target must lie between 1 and 2^30 bytes")]
    BlockSize,
    #[error("a batch holds at least one block")]
    Batch,
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
    #[error("{channel}: the first run loses {lost} samples; nothing comes before it to lose")]
    FirstRunLost { channel: String, lost: u64 },
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
    #[error("a flush at tick {before} does not lie past every item in, the last at {last}")]
    Flush { before: i64, last: i64 },
    #[error("a block of {0} items or bytes, more than its index word or length holds")]
    BlockTooLarge(usize),
    #[error("the length {length} does not lie past every item and block in, the last at {last}")]
    Length { length: i64, last: i64 },
    #[error("{expected} digital channel ends given, {given} expected")]
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
    #[error("{channel}: never known, so its initial level is false")]
    UnknownInitial { channel: String },
    #[error("source {index}'s extra already holds {key}")]
    Extra { index: usize, key: String },
    #[error("extra keys for {given} sources, but the recording has {sources}")]
    Extras { given: usize, sources: usize },
    #[error("an annotation is not a JSON object with a tick `start`: {0}")]
    Annotation(String),
    #[error("the footer is {0} bytes, more than its u32 length holds")]
    FooterTooLarge(usize),
    #[error("the header would not read back: {0}")]
    Header(String),
    #[error("zstd: {0}")]
    Zstd(#[from] std::io::Error),
}

/// The zstd level overview blocks are compressed at, whatever `WriteOptions::level`: a level
/// that moves into overview blocks writes every settled one at once.
pub const OVERVIEW_BLOCK_LEVEL: i32 = 3;

#[derive(Clone, Debug)]
pub struct WriteOptions {
    /// The zstd level for blocks and the footer's overview.
    pub level: i32,
    /// The decoded size at which a block closes.
    pub block_bytes: usize,
    /// How many closed blocks compress together, in parallel. 1 writes each block as it closes.
    pub batch: usize,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            level: 19,
            block_bytes: 1 << 20,
            batch: 1,
        }
    }
}

/// One zstd frame with a content checksum, which the readers require.
pub(crate) fn compress(raw: &[u8], level: i32) -> std::io::Result<Vec<u8>> {
    let mut compressor = zstd::bulk::Compressor::new(level)?;
    compressor.set_parameter(zstd::stream::raw::CParameter::ChecksumFlag(true))?;
    compressor.compress(raw)
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

struct DigitalState {
    initial: bool,
    /// The level after the edges and the gap ends the blocks have passed.
    level: bool,
    map: Mapping,
    /// The channel's start, in its source's ticks.
    start: i64,
    /// The last edge's source tick, or the channel's start before the first.
    last_edge: i64,
    edges: u64,
    gaps: Chunks<Gap>,
    /// Gaps whose `to` no block has passed yet, each with its `to` on the axis.
    pending: VecDeque<(Gap, i64)>,
    hasher: ChannelHasher,
}

struct AnalogState {
    period: Ratio,
    coding: AnalogCoding,
    map: Mapping,
    /// Every run so far, the first at index 0.
    runs: Chunks<Break>,
    /// A run the next sample starts: its source tick and the samples lost before it.
    next_run: Option<(i64, u64)>,
    count: u64,
    /// The last sample's place, a source tick.
    last_tick: Option<i64>,
    hasher: ChannelHasher,
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

/// A digital channel's edges in the open block, in its source's ticks, and the size its section
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

enum State {
    Digital(DigitalState),
    Analog(AnalogState),
}

struct Open {
    start: i64,
    /// Each analog channel's section so far; a digital channel's is empty.
    sections: Vec<Vec<u8>>,
    /// Each digital channel's edges so far; an analog channel's holds none.
    edges: Vec<EdgeRun>,
    words: Vec<u32>,
    size: usize,
    /// The last item's axis tick; `start` while it holds none.
    last: i64,
    items: bool,
}

impl Open {
    /// A stand-in while the open block is being replaced.
    fn empty(start: i64) -> Self {
        Self {
            start,
            sections: Vec::new(),
            edges: Vec::new(),
            words: Vec::new(),
            size: 0,
            last: start,
            items: false,
        }
    }

    /// Every channel's section as the block holds it, a digital channel's coded now.
    fn sections(&self) -> Vec<Vec<u8>> {
        self.sections
            .iter()
            .zip(&self.edges)
            .map(|(section, edges)| {
                if edges.ticks.is_empty() {
                    section.clone()
                } else {
                    let mut out = Vec::with_capacity(edges.size());
                    edges.encode(&mut out);
                    out
                }
            })
            .collect()
    }
}

/// A block's decoded payload from its channels' sections as the writer builds them: the section
/// table, then each section, codes split into byte planes.
pub(crate) fn assemble(sections: &[Vec<u8>], codings: &[Option<AnalogCoding>]) -> Vec<u8> {
    let size: usize = sections.iter().map(Vec::len).sum();
    let mut raw = Vec::with_capacity(size + 4 * sections.len());
    for section in sections {
        raw.extend_from_slice(&(section.len() as u32).to_le_bytes());
    }
    for (section, coding) in sections.iter().zip(codings) {
        match coding {
            Some(coding) if coding.planes() => {
                samples::to_planes(section, coding.sample_len(), &mut raw)
            }
            _ => raw.extend_from_slice(section),
        }
    }
    raw
}

/// What waits to be written, in file order.
pub(crate) enum Queued {
    Block {
        head: BlockHead,
        raw: Vec<u8>,
        /// Each channel's count of items before the block.
        firsts: Vec<u64>,
    },
    Overview {
        shift: u8,
        k: u64,
        /// Shared with the snapshots taken before it is written.
        raw: Arc<Vec<u8>>,
    },
    Record(Vec<u8>),
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
    /// Bundle annotations into the footer's header, so the recording is one file.
    pub bundle: Option<Bundle>,
    /// Take a gap that reaches a digital channel's end as ending what is known at its `from`,
    /// even where the gap ends inside the recording, as repair does for a file cut off. Otherwise
    /// such a gap is refused, as most likely a caller's mistake.
    pub drop_gaps_past_end: bool,
}

/// What a finished file bundles into its header's `annotations`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bundle {
    /// The writer's own annotations: each source's generator file `<stem>.<source>.jsonl`, and
    /// a hand-written `<stem>.annotations.toml` that holds no annotation and includes them, where
    /// `<stem>` names the recording `<stem>.tickwave`.
    Generated { stem: String },
    /// These files exactly: a re-cutter carries a file's bundle over.
    Files(BTreeMap<String, String>),
}

/// The footer's header for what a writer declared and what the end settled: the declaration's
/// values, `extra` added to each source's, the channels' totals, and the levels `plan` sets.
pub(crate) fn finished_header(
    declaration: &Declaration,
    length: i64,
    channels: Vec<ChannelHeader>,
    digests: &[blake3::Hash],
    blocks: u32,
    plan: &overview::Plan,
    extra: Vec<BTreeMap<String, String>>,
) -> Header {
    let mut sources = declaration.sources.clone();
    for (source, extra) in sources.iter_mut().zip(extra) {
        source.extra.extend(extra);
    }
    Header {
        timebase: declaration.timebase,
        length,
        channels,
        derived: declaration.derived.clone(),
        sources,
        title: declaration.title.clone(),
        meta: declaration.meta.clone(),
        content_id: hash::content_id(declaration.timebase, length, digests),
        blocks,
        overview_shift: declaration.overview_shift,
        overview: plan.head.clone(),
        overview_blocks: plan
            .blocks
            .iter()
            .map(|&shift| OverviewLevel {
                shift,
                blocks: overview::block_count(length, shift) as u32,
            })
            .collect(),
        annotations: None,
    }
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

pub struct Writer {
    declaration: Arc<Declaration>,
    options: WriteOptions,
    states: Vec<State>,
    codings: Vec<Option<AnalogCoding>>,
    out: Vec<u8>,
    /// Bytes handed out before `out`.
    taken: u64,
    open: Open,
    /// The latest item's axis tick, across every channel.
    last_tick: i64,
    /// Where the last closed block ended; 0 before the first.
    closed: i64,
    /// Each channel's count of items in the blocks closed so far.
    counts: Vec<u64>,
    written: Chunks<Written>,
    overview_rows: Vec<(u8, u64, OverviewRow)>,
    queue: Vec<Queued>,
    queued_blocks: usize,
    overview: Builder,
    annotations: Chunks<record::Annotation>,
    /// The last overview block's level and its decoded bytes: a span with no item settles block
    /// after block of one shared level, which is written out once.
    last_level: Option<(Arc<overview::Level>, Arc<Vec<u8>>)>,
    /// The last overview block's decoded bytes and frame, which a block of the same bytes takes
    /// rather than compressing them again.
    last_frame: Option<(Arc<Vec<u8>>, Vec<u8>)>,
}

fn check_safe(what: &'static str, value: u64) -> Result<(), WriteError> {
    if value < JSON_SAFE {
        Ok(())
    } else {
        Err(WriteError::TooLarge { what, value })
    }
}

impl Writer {
    /// Writes the preamble and the declaration to the output. The declaration is held to the
    /// rules a reader holds it to.
    pub fn new(declaration: Declaration, options: WriteOptions) -> Result<Self, WriteError> {
        if options.block_bytes == 0 || options.block_bytes > 1 << 30 {
            return Err(WriteError::BlockSize);
        }
        if options.batch == 0 {
            return Err(WriteError::Batch);
        }
        check_declaration(&declaration).map_err(|e| WriteError::Declaration(e.to_string()))?;
        let unmapped = || WriteError::Declaration("a channel has no mapping to the axis".into());
        let mut states = Vec::with_capacity(declaration.channels.len());
        let mut codings = Vec::with_capacity(declaration.channels.len());
        for (c, channel) in declaration.channels.iter().enumerate() {
            let hasher = ChannelHasher::declared(&declaration, c).ok_or_else(unmapped)?;
            let map = declaration.mapping(c).ok_or_else(unmapped)?;
            match &channel.data {
                DeclaredKind::Digital { initial } => {
                    let start = declaration.start(c).ok_or_else(unmapped)?;
                    states.push(State::Digital(DigitalState {
                        initial: *initial,
                        level: *initial,
                        map,
                        start,
                        last_edge: start,
                        edges: 0,
                        gaps: Chunks::new(),
                        pending: VecDeque::new(),
                        hasher,
                    }));
                    codings.push(None);
                }
                DeclaredKind::Analog { period, coding, .. } => {
                    states.push(State::Analog(AnalogState {
                        period: *period,
                        coding: *coding,
                        map,
                        runs: Chunks::new(),
                        next_run: None,
                        count: 0,
                        last_tick: None,
                        hasher,
                    }));
                    codings.push(Some(*coding));
                }
            }
        }
        let json = serde_json::to_vec(&declaration).expect("a declaration serialises");
        let mut out = Vec::new();
        out.extend_from_slice(&Preamble::CURRENT.to_bytes());
        record::write(&mut out, Kind::Declaration, &[&json]);
        let mut writer = Self {
            overview: Builder::new(
                declaration.overview_shift,
                &declaration.channels,
                &declaration.derived,
            ),
            counts: vec![0; states.len()],
            declaration: Arc::new(declaration),
            options,
            states,
            codings,
            out,
            taken: 0,
            open: Open::empty(0),
            last_tick: 0,
            closed: 0,
            written: Chunks::new(),
            overview_rows: Vec::new(),
            queue: Vec::new(),
            queued_blocks: 0,
            annotations: Chunks::new(),
            last_level: None,
            last_frame: None,
        };
        writer.open = writer.new_block(0);
        Ok(writer)
    }

    /// The bytes written and not yet taken.
    pub fn output(&self) -> &[u8] {
        &self.out
    }

    /// Takes the first `n` bytes of `output`.
    pub fn consume(&mut self, n: usize) {
        self.out.drain(..n);
        self.taken += n as u64;
    }

    /// Takes every byte written so far.
    pub fn take_output(&mut self) -> Vec<u8> {
        self.taken += self.out.len() as u64;
        std::mem::take(&mut self.out)
    }

    /// The file's length so far: the bytes taken and those in `output`.
    pub fn position(&self) -> u64 {
        self.taken + self.out.len() as u64
    }

    pub fn declaration(&self) -> &Declaration {
        &self.declaration
    }

    fn name(&self, c: usize) -> String {
        self.declaration.channels[c].name.clone()
    }

    fn new_block(&self, start: i64) -> Open {
        Open {
            start,
            sections: vec![Vec::new(); self.states.len()],
            edges: vec![EdgeRun::default(); self.states.len()],
            words: self
                .states
                .iter()
                .map(|s| match s {
                    State::Digital(d) if d.level => LEVEL_BIT,
                    _ => 0,
                })
                .collect(),
            size: 0,
            last: start,
            items: false,
        }
    }

    fn check_channel(&self, c: usize) -> Result<(), WriteError> {
        if c < self.states.len() {
            Ok(())
        } else {
            Err(WriteError::NoChannel(c))
        }
    }

    /// Where source tick `k` of channel `c` lies on the axis, refusing either tick outside
    /// `0..2^53` (FORMAT.md, "Times"), as a reader would.
    fn place(&self, c: usize, map: &Mapping, k: i64) -> Result<i64, WriteError> {
        let safe = 0..JSON_SAFE as i64;
        if !safe.contains(&k) {
            return Err(WriteError::SourceTick {
                channel: self.name(c),
                tick: k,
            });
        }
        match map.axis(k) {
            Some(t) if safe.contains(&t) => Ok(t),
            _ => Err(WriteError::AxisTick {
                channel: self.name(c),
                tick: k,
            }),
        }
    }

    fn check_order(&self, c: usize, tick: i64) -> Result<(), WriteError> {
        if tick < self.last_tick || tick < self.open.start {
            return Err(WriteError::Order {
                channel: self.name(c),
                tick,
                previous: self.last_tick.max(self.open.start),
            });
        }
        Ok(())
    }

    /// An item at `tick` is about to go in: close the open block first if it is full and the item
    /// is later than its last.
    fn before_item(&mut self, tick: i64) -> Result<(), WriteError> {
        if self.open.size >= self.options.block_bytes && self.open.items && tick > self.open.last {
            self.close(tick)?;
        }
        Ok(())
    }

    fn word_room(&self, c: usize, digital: bool) -> Result<(), WriteError> {
        let max = if digital { LEVEL_BIT - 1 } else { u32::MAX };
        let count = self.open.words[c] & if digital { !LEVEL_BIT } else { u32::MAX };
        if count >= max {
            return Err(WriteError::BlockTooLarge(count as usize));
        }
        Ok(())
    }

    /// An edge of digital channel `c` at source tick `tick`. An edge at the channel's start flips
    /// the level from the declared `initial`, as any edge does.
    pub fn edge(&mut self, c: usize, tick: i64) -> Result<(), WriteError> {
        self.check_channel(c)?;
        let State::Digital(d) = &self.states[c] else {
            return Err(WriteError::Kind {
                channel: self.name(c),
                kind: "analog",
            });
        };
        let t = self.place(c, &d.map, tick)?;
        self.check_order(c, t)?;
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
        self.word_room(c, true)?;
        self.before_item(t)?;
        let State::Digital(d) = &mut self.states[c] else {
            unreachable!()
        };
        while let Some(&(g, _)) = d.pending.front().filter(|(g, _)| g.to < tick) {
            d.level = g.level;
            d.pending.pop_front();
        }
        let run = &mut self.open.edges[c];
        let before = run.size();
        let after = run.push(tick);
        self.open.size = self.open.size - before + after;
        self.open.words[c] += 1;
        d.last_edge = tick;
        d.edges += 1;
        d.level = !d.level;
        d.hasher.edge(tick);
        self.overview.edge(c, tick, t);
        self.item_in(t);
        Ok(())
    }

    fn item_in(&mut self, tick: i64) {
        self.open.last = tick;
        self.open.items = true;
        self.last_tick = tick;
    }

    /// A gap of digital channel `c`, in its source's ticks. It may start in a block already
    /// written, but not end in one.
    pub fn gap(&mut self, c: usize, gap: Gap) -> Result<(), WriteError> {
        self.check_channel(c)?;
        let name = self.name(c);
        let State::Digital(d) = &self.states[c] else {
            return Err(WriteError::Kind {
                channel: name,
                kind: "analog",
            });
        };
        if gap.to <= gap.from {
            return Err(WriteError::GapSpan {
                channel: name,
                from: gap.from,
                to: gap.to,
            });
        }
        self.place(c, &d.map, gap.from)?;
        let to = self.place(c, &d.map, gap.to)?;
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
        if to < self.open.start {
            return Err(WriteError::GapLate {
                channel: name,
                to: gap.to,
                closed: self.open.start,
            });
        }
        let State::Digital(d) = &mut self.states[c] else {
            unreachable!()
        };
        d.gaps.push(gap);
        d.pending.push_back((gap, to));
        self.overview.gap(c, gap, to);
        let mut bytes = Vec::new();
        record::write(&mut bytes, Kind::Gap, &[&record::gap_bytes(c as u32, gap)]);
        self.put_record(bytes);
        Ok(())
    }

    /// Analog channel `c`'s next sample starts a run at source tick `tick`, after `lost` samples
    /// the instrument lost: 0 when it only paused, and 0 for the first run. A run no sample has
    /// followed yet is replaced.
    pub fn run(&mut self, c: usize, tick: i64, lost: u64) -> Result<(), WriteError> {
        self.check_channel(c)?;
        let name = self.name(c);
        let State::Analog(a) = &self.states[c] else {
            return Err(WriteError::Kind {
                channel: name,
                kind: "digital",
            });
        };
        self.place(c, &a.map, tick)?;
        let State::Analog(a) = &mut self.states[c] else {
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
            return Err(WriteError::FirstRunLost {
                channel: name,
                lost,
            });
        }
        check_safe("lost count", lost)?;
        a.next_run = Some((tick, lost));
        Ok(())
    }

    /// The next sample of analog channel `c`, in the type its coding stores: an f32 for an `f32`
    /// channel, or for a `codes` one if a code reproduces it exactly; an f64 for an `f64` one; a
    /// u64 or an i64 code for an `int` one, in its range. Its place, a source tick, follows from
    /// its run and the period.
    pub fn sample(&mut self, c: usize, value: impl Into<Sample>) -> Result<(), WriteError> {
        let sample = value.into();
        self.check_channel(c)?;
        // The channel's name only for an error: a sample is the writer's commonest call.
        let State::Analog(a) = &self.states[c] else {
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
        let steps =
            u128::from(index - run.index) * u128::from(a.period.num()) / u128::from(a.period.den());
        let place = i64::try_from(steps)
            .ok()
            .and_then(|s| run.tick.checked_add(s))
            .unwrap_or(i64::MAX);
        let map = a.map;
        let tick = self.place(c, &map, place)?;
        self.check_order(c, tick)?;
        if let Some(why) = samples::misfit(sample, a.coding) {
            return Err(WriteError::Sample {
                channel: self.name(c),
                index,
                why,
            });
        }
        let code = match (a.coding, sample) {
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
        self.overview.pairs_with(c, tick)?;
        self.word_room(c, false)?;
        self.before_item(tick)?;
        if starts {
            let mut bytes = Vec::new();
            record::write(&mut bytes, Kind::Run, &[&record::run_bytes(c as u32, run)]);
            self.put_record(bytes);
        }
        let State::Analog(a) = &mut self.states[c] else {
            unreachable!()
        };
        if starts {
            a.runs.push(run);
            a.next_run = None;
        }
        let section = &mut self.open.sections[c];
        samples::put(section, sample, a.coding, code);
        self.open.size += a.coding.sample_len();
        self.open.words[c] += 1;
        a.count += 1;
        a.last_tick = Some(place);
        a.hasher.sample(sample);
        self.overview
            .sample(c, tick, sample)
            .expect("pairs_with checked the derived channels");
        self.item_in(tick);
        Ok(())
    }

    /// One generated annotation, from generator `source`. Its times are axis ticks.
    pub fn annotation(
        &mut self,
        source: &str,
        annotation: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), WriteError> {
        if annotation.get("start").and_then(|s| s.as_i64()).is_none() {
            return Err(WriteError::Annotation(
                serde_json::to_string(&annotation).expect("a map serialises"),
            ));
        }
        let a = record::Annotation {
            source: source.to_owned(),
            annotation,
        };
        let json = serde_json::to_vec(&a).expect("an annotation serialises");
        let mut bytes = Vec::new();
        record::write(&mut bytes, Kind::Annotation, &[&json]);
        self.put_record(bytes);
        self.annotations.push(a);
        Ok(())
    }

    /// Every item before axis tick `before` is in: close the open block there. A live recorder
    /// calls this so a crash loses little. At the open block's start it does nothing.
    pub fn flush(&mut self, before: i64) -> Result<(), WriteError> {
        if before == self.open.start && !self.open.items {
            return Ok(());
        }
        check_safe("flush tick", before.max(0) as u64)?;
        if before <= self.open.start || (self.open.items && before <= self.open.last) {
            return Err(WriteError::Flush {
                before,
                last: if self.open.items {
                    self.open.last
                } else {
                    self.open.start
                },
            });
        }
        self.close(before)?;
        self.drain_queue()
    }

    /// Closes the open block at `end` and opens the next there.
    fn close(&mut self, end: i64) -> Result<(), WriteError> {
        let open = std::mem::replace(&mut self.open, Open::empty(end));
        let raw = assemble(&open.sections(), &self.codings);
        let decoded_length =
            u32::try_from(raw.len()).map_err(|_| WriteError::BlockTooLarge(raw.len()))?;
        let firsts = self.counts.clone();
        for (count, (&word, coding)) in self
            .counts
            .iter_mut()
            .zip(open.words.iter().zip(&self.codings))
        {
            *count += u64::from(if coding.is_some() {
                word
            } else {
                word & !LEVEL_BIT
            });
        }
        self.queue.push(Queued::Block {
            head: BlockHead {
                start: open.start,
                end,
                decoded_length,
                words: open.words,
            },
            raw,
            firsts,
        });
        self.queued_blocks += 1;
        self.closed = end;
        // The next block's level bits: gap ends before its start have taken effect.
        for state in &mut self.states {
            if let State::Digital(d) = state {
                while let Some(&(g, _)) = d.pending.front().filter(|&&(_, to)| to < end) {
                    d.level = g.level;
                    d.pending.pop_front();
                }
            }
        }
        self.open = self.new_block(end);
        // A derived channel's operand sample still waiting for its partner never gets one: the
        // partner would come at a tick before the next block.
        self.overview.settle(end)?;
        for settled in self.overview.take_settled() {
            self.queue_overview(settled);
        }
        if self.queued_blocks >= self.options.batch {
            self.drain_queue()?;
        }
        Ok(())
    }

    /// A record that needs no compressing: straight out, unless blocks before it still wait.
    fn put_record(&mut self, bytes: Vec<u8>) {
        if self.queue.is_empty() {
            self.out.extend_from_slice(&bytes);
        } else {
            self.queue.push(Queued::Record(bytes));
        }
    }

    /// An overview block that has settled, decoded, queued to be written. One that shares the
    /// last one's level shares its bytes.
    fn queue_overview(&mut self, Settled { shift, k, level }: Settled) {
        let raw = match &self.last_level {
            Some((last, raw)) if Arc::ptr_eq(last, &level) => Arc::clone(raw),
            _ => {
                let mut raw = Vec::new();
                overview::write_level(&level, &mut raw);
                let raw = Arc::new(raw);
                self.last_level = Some((level, Arc::clone(&raw)));
                raw
            }
        };
        self.queue.push(Queued::Overview { shift, k, raw });
    }

    /// Compresses what waits, together, and writes it in order. An overview block whose bytes are
    /// the one before's takes that one's frame.
    fn drain_queue(&mut self) -> Result<(), WriteError> {
        let level = self.options.level;
        let mut before = self.last_frame.as_ref().map(|(raw, _)| Arc::clone(raw));
        let repeats: Vec<bool> = self
            .queue
            .iter()
            .map(|q| match q {
                Queued::Overview { raw, .. } => {
                    let same = before.as_ref().is_some_and(|b| Arc::ptr_eq(b, raw));
                    before = Some(Arc::clone(raw));
                    same
                }
                _ => false,
            })
            .collect();
        let frames = self
            .queue
            .par_iter()
            .zip(&repeats)
            .map(|(q, &repeat)| match q {
                Queued::Block { raw, .. } => compress(raw, level).map(Some),
                Queued::Overview { .. } if repeat => Ok(None),
                Queued::Overview { raw, .. } => compress(raw, OVERVIEW_BLOCK_LEVEL).map(Some),
                Queued::Record(_) => Ok(None),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let channels = self.states.len();
        for (q, frame) in std::mem::take(&mut self.queue).into_iter().zip(frames) {
            let at = self.position();
            match (q, frame) {
                (Queued::Block { head, raw, firsts }, Some(frame)) => {
                    let length = u32::try_from(frame.len())
                        .map_err(|_| WriteError::BlockTooLarge(frame.len()))?;
                    let head_bytes = head.to_bytes();
                    record::write(&mut self.out, Kind::Block, &[&head_bytes, &frame]);
                    self.written.push(Written {
                        row: BlockRow {
                            start: head.start,
                            offset: at + (record::PAYLOAD_AT + BlockHead::len(channels)) as u64,
                            length,
                            decoded_length: raw.len() as u32,
                        },
                        words: head.words,
                        end: head.end,
                        firsts,
                    });
                }
                (Queued::Overview { shift, k, raw }, frame) => {
                    let frame = match (frame, &self.last_frame) {
                        (Some(frame), _) => {
                            self.last_frame = Some((Arc::clone(&raw), frame.clone()));
                            frame
                        }
                        (None, Some((_, frame))) => frame.clone(),
                        (None, None) => unreachable!("a repeat follows a frame"),
                    };
                    let length = u32::try_from(frame.len())
                        .map_err(|_| WriteError::BlockTooLarge(frame.len()))?;
                    record::write(
                        &mut self.out,
                        Kind::OverviewBlock,
                        &[&record::overview_head(shift, k), &frame],
                    );
                    self.overview_rows.push((
                        shift,
                        k,
                        OverviewRow {
                            offset: at + (record::PAYLOAD_AT + record::OVERVIEW_HEAD_LEN) as u64,
                            length,
                            decoded_length: raw.len() as u32,
                        },
                    ));
                }
                (Queued::Record(bytes), None) => self.out.extend_from_slice(&bytes),
                _ => unreachable!("frames follow the queue"),
            }
        }
        self.queued_blocks = 0;
        Ok(())
    }

    /// What the writer holds so far, for a view of the recording while it is written: the
    /// blocks and overview blocks written, the gaps, runs and annotations, the overview's held
    /// levels, and the blocks and overview blocks not yet written, the open block last. Taking
    /// one shares what the writer holds rather than copying it, apart from the blocks not yet
    /// written.
    ///
    /// The blocks it lists may sit in bytes still in `output`: publish a snapshot to a view only
    /// once those bytes are in the file the view reads.
    pub fn snapshot(&self) -> Snapshot {
        let queued = self
            .queue
            .iter()
            .filter_map(|q| match q {
                Queued::Block { head, raw, firsts } => {
                    Some((head.clone(), raw.clone(), firsts.clone()))
                }
                _ => None,
            })
            .collect();
        let queued_overview = self
            .queue
            .iter()
            .filter_map(|q| match q {
                Queued::Overview { shift, k, raw } => Some((*shift, *k, Arc::clone(raw))),
                _ => None,
            })
            .collect();
        let mut gaps = Vec::with_capacity(self.states.len());
        let mut runs = Vec::with_capacity(self.states.len());
        for state in &self.states {
            match state {
                State::Digital(d) => {
                    gaps.push(d.gaps.clone());
                    runs.push(Chunks::new());
                }
                State::Analog(a) => {
                    gaps.push(Chunks::new());
                    runs.push(a.runs.clone());
                }
            }
        }
        // `close` counts a block's items as it queues it, so the queued blocks are in already.
        let firsts = self.counts.clone();
        Snapshot::new(
            Arc::clone(&self.declaration),
            self.written.clone(),
            queued,
            (
                BlockHead {
                    start: self.open.start,
                    end: i64::MAX,
                    decoded_length: 0,
                    words: self.open.words.clone(),
                },
                assemble(&self.open.sections(), &self.codings),
                firsts,
            ),
            gaps,
            runs,
            self.annotations.clone(),
            self.overview.held(),
            &self.overview_rows,
            queued_overview,
            self.position(),
            self.states
                .iter()
                .map(|s| match s {
                    State::Digital(d) => (d.map, d.start),
                    State::Analog(a) => (a.map, 0),
                })
                .collect(),
        )
    }

    /// The recording ends at axis tick `length`. `ends` gives each digital channel's `end`, a
    /// source tick, in channel order. Closes the last block, then writes the footer and the
    /// trailer, with `finish`'s extras and bundle.
    pub fn finish(
        mut self,
        length: i64,
        ends: &[i64],
        finish: Finish,
    ) -> Result<Finished, WriteError> {
        let last = if self.open.items {
            self.open.last
        } else {
            self.open.start - 1
        };
        if length < 1 || length <= last || length < self.open.start {
            return Err(WriteError::Length {
                length,
                last: last.max(self.open.start),
            });
        }
        check_safe("length", length as u64)?;
        let digital = self
            .states
            .iter()
            .filter(|s| matches!(s, State::Digital(_)))
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
        if let Some(e) = self.overview.unpaired() {
            return Err(e.into());
        }
        // Check every channel's end before anything is closed, so a refusal leaves no half-written
        // footer.
        let mut settled_ends = Vec::with_capacity(self.states.len());
        let mut ends = ends.iter();
        for (c, state) in self.states.iter().enumerate() {
            let State::Digital(d) = state else {
                settled_ends.push(None);
                continue;
            };
            let name = self.declaration.channels[c].name.clone();
            let Some(&given) = ends.next() else {
                unreachable!("one end a digital channel, counted above")
            };
            let mut end = given;
            let on_axis = d
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
                let inside = d.map.axis(g.to).is_some_and(|t| t < length);
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
            if d.edges > 0 && d.last_edge >= end {
                return Err(WriteError::EdgePastEnd {
                    channel: name,
                    tick: d.last_edge,
                    end,
                });
            }
            if end == d.start && d.initial {
                return Err(WriteError::UnknownInitial { channel: name });
            }
            check_safe("edge count", d.edges)?;
            settled_ends.push(Some((end, kept)));
        }
        for state in &self.states {
            if let State::Analog(a) = state {
                check_safe("analog sample count", a.count)?;
            }
        }
        if length > self.open.start || self.written.is_empty() && self.queue.is_empty() {
            self.close(length)?;
        }
        let (done, settled) =
            std::mem::replace(&mut self.overview, Builder::new(0, &[], &[])).finish(length)?;
        for settled in settled {
            self.queue_overview(settled);
        }
        self.drain_queue()?;

        let mut digests = Vec::with_capacity(self.states.len());
        let mut channels = Vec::with_capacity(self.states.len());
        for ((state, declared), settled) in std::mem::take(&mut self.states)
            .into_iter()
            .zip(&self.declaration.channels)
            .zip(settled_ends)
        {
            let data = match state {
                State::Digital(d) => {
                    let (end, kept) = settled.unwrap();
                    let gaps: Vec<Gap> = d.gaps.iter().take(kept).copied().collect();
                    digests.push(d.hasher.finish_digital(end, &gaps));
                    ChannelKind::Digital {
                        initial: d.initial,
                        end,
                        edges: d.edges,
                        gaps,
                    }
                }
                State::Analog(a) => {
                    let first = a.runs.get(0).map_or(0, |r| r.tick);
                    let breaks: Vec<Break> = a.runs.iter().skip(1).copied().collect();
                    digests.push(a.hasher.finish_analog(first, a.count, &breaks));
                    let DeclaredKind::Analog {
                        unit, significant, ..
                    } = &declared.data
                    else {
                        unreachable!("states follow the declaration")
                    };
                    ChannelKind::Analog {
                        unit: unit.clone(),
                        first,
                        period: a.period,
                        count: a.count,
                        breaks,
                        coding: a.coding,
                        significant: *significant,
                    }
                }
            };
            channels.push(ChannelHeader {
                name: declared.name.clone(),
                source_name: declared.source_name.clone(),
                source: declared.source,
                data,
                shaping: declared.shaping.clone(),
            });
        }
        let mut overview_rows = std::mem::take(&mut self.overview_rows);
        overview_rows.sort_by_key(|&(shift, k, _)| (shift, k));
        let rows: Vec<BlockRow> = self.written.iter().map(|w| w.row).collect();
        let mut words = Words::with_capacity(self.declaration.channels.len(), rows.len());
        for w in self.written.iter() {
            words.push(&w.words);
        }
        let mut header = finished_header(
            &self.declaration,
            length,
            channels,
            &digests,
            rows.len() as u32,
            &done.plan,
            finish.extra,
        );
        let generated = generated(self.annotations.iter(), length);
        header.annotations = match finish.bundle {
            None => None,
            Some(Bundle::Generated { stem }) => {
                Some(bundled_files(&stem, header.content_id, &generated))
            }
            Some(Bundle::Files(files)) => Some(files),
        };
        // The reader's own rules, so the writer never writes a header the reader refuses.
        crate::decode::check_header(&header).map_err(|e| WriteError::Header(e.to_string()))?;
        let mut overview_raw = Vec::new();
        overview::write(&done.head, &mut overview_raw);
        let overview_frame = compress(&overview_raw, self.options.level)?;
        let overview_rows: Vec<OverviewRow> =
            overview_rows.into_iter().map(|(_, _, r)| r).collect();
        let footer = record::footer_bytes(&header, &rows, &words, &overview_rows, &overview_frame)
            .map_err(WriteError::FooterTooLarge)?;
        record::write_footer(&mut self.out, self.taken, &footer);
        Ok(Finished {
            header,
            tail: std::mem::take(&mut self.out),
            generated,
        })
    }
}
