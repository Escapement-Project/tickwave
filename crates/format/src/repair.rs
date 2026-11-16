//! Repair: a footer and a trailer for a file cut off before them, by a crash, a power cut or a
//! full disk (FORMAT.md, "Repairing a cut-off file"). The file is cut just after its latest
//! known-through record, or, with none, after its last record at which every derived channel's
//! operands hold as many samples, and before the first record only a finish writes; the records
//! before the cut stay as they are.

use std::collections::VecDeque;

use crate::annotations::Lines;
use crate::compress::compress;
use crate::decode::{
    BlockChannel, DecodeError, MAX_DECODED, check_header, corrupt, gaps_ending_in, inflate_limited,
    level_after,
};
use crate::hash::{self, ChannelHasher};
use crate::header::{
    BlockRow, ChannelHeader, ChannelKind, Declaration, DeclaredKind, DerivedHeader, Header,
    INLINE_LIST, LEVEL_BIT, OverviewLevel, OverviewRow,
};
use crate::index::{Due, Row, Table, TableWriter};
use crate::mapping::Mapping;
use crate::model::{Break, Gap};
use crate::overview::{self, Column, Ladder, Level, Pair, Settled};
use crate::parts::{Part, PartRow, placed_span};
use crate::reader::{Ending, Event, Reader};
use crate::record::{self, BlockHead, Kind, OverviewHead};
use crate::writer::{
    Bundle, Bundling, ChannelTops, OVERVIEW_BLOCK_LEVEL, Pending, WriteError, bundle_payload,
    bundled_files, footer_payload, write_part_record,
};

/// A cut-off file's length: the later of `end`, the latest known-through tick or, with none,
/// the latest end of any kept block, 0 with none, and one past the latest start among the
/// annotations kept.
pub fn repaired_length(end: i64, latest_annotation: Option<i64>) -> i64 {
    latest_annotation.map_or(end, |start| end.max(start.saturating_add(1)))
}

/// The latest start among annotations, with the line `line` taken in.
pub(crate) fn latest_start(latest: Option<i64>, line: &str) -> Option<i64> {
    let start = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(line)
        .ok()?
        .get("start")?
        .as_i64();
    match start {
        Some(start) => Some(latest.map_or(start, |l| l.max(start))),
        None => latest,
    }
}

/// A record only a writer's finish writes (FORMAT.md, "Repairing a cut-off file"): a table's last
/// page, one of fewer than 4,096 entries, or the annotation bundle. A file cut where one lies was
/// cut inside its finish.
fn finish_only(event: &Event) -> bool {
    match event {
        Event::Page { head, .. } => (head.count as usize) < crate::index::PAGE,
        Event::Bundle(_) => true,
        _ => false,
    }
}

pub use crate::decode::HOLD;

/// The overview blocks a ladder settles at a step, as a writer does.
const SETTLE_STEP: usize = 16;

/// About the bytes `event` takes in memory once read.
fn held_size(event: &Event) -> usize {
    64 + match event {
        Event::Block(b) => match &b.data {
            BlockChannel::Digital { edges, source, .. } => {
                8 * (edges.len() + source.as_ref().map_or(0, Vec::len))
            }
            BlockChannel::Analog { values, .. } => match values {
                crate::samples::Values::F32(v) => 4 * v.len(),
                v => 8 * v.len(),
            },
        },
        Event::OverviewBlock { frame, .. } => frame.len(),
        Event::Annotation(a) => a.source.len() + a.line.len(),
        Event::Part { part, .. } => part.source.len() + part.lines.len(),
        Event::Bundle(b) => b.len(),
        _ => 0,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RepairError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("the file is finished: it has its footer")]
    Finished,
    #[error("the file holds no whole declaration")]
    NoDeclaration,
    #[error("the file holds neither a whole block nor an annotation record")]
    NoBlock,
    #[error("an overview block or page in the file differs from the one its content gives")]
    Overview,
    #[error(
        "the file holds {0} bytes of decoded records past its latest known-through record, or \
         since its derived channels' operands last held as many samples: more than repair holds \
         back, {HOLD}, before it can tell whether they stand"
    )]
    HeldBack(usize),
    /// Channel `0` states a kept span: it was moved or cut, or the recording merges several
    /// sources. The declaration fixed each kept span before the length was known, and the content
    /// id pins it, so a file cut off before them has no footer that would hold. Its sources make
    /// it again.
    #[error(
        "channel {0} states the span it keeps, as a moved, trimmed or merged recording does, so \
         it isn't repaired: make it again from its sources"
    )]
    Shaped(String),
    #[error(transparent)]
    Write(#[from] WriteError),
}

/// What repair comes to: keep the file's first `keep` bytes, then append `tail`. The annotations
/// it keeps are in the copy's parts: `Footer::each_generated_line` reads them back a part at a
/// time.
pub struct Repaired {
    pub keep: u64,
    pub tail: Vec<u8>,
    pub header: Header,
    /// The annotation records after the cut whose line breaks a rule ANNOTATIONS.md gives a
    /// generator's line, each as its generator and the rule: repair leaves them out of what it
    /// writes again and of the length the marks set.
    pub left_out: Vec<String>,
}

/// The records of a file read in order that a repair or a re-cut may still drop: those after its
/// latest known-through record, or, before any, those after the last record at which every
/// derived channel's operands hold as many samples. What it holds back it gives up, in order,
/// once a later record shows it stands. It holds at most `HOLD` bytes of what it decoded: a
/// writer's file holds back a flush interval, or one block of a derived channel's operand.
pub(crate) struct Hold {
    held: Vec<(u64, Event)>,
    /// About the bytes `held` takes in memory.
    bytes: usize,
    known: bool,
    /// Each derived channel's operands, by channel.
    pairs: Vec<(usize, usize)>,
    /// Each channel's samples in the blocks taken, held back or not.
    counts: Vec<u64>,
}

impl Hold {
    pub fn new(d: &Declaration) -> Self {
        Self {
            held: Vec::new(),
            bytes: 0,
            known: false,
            // A derived channel this version can't compute pairs nothing: repair and the
            // re-cutter refuse its file once they come to write it.
            pairs: overview::rules(&d.channels, &d.derived)
                .into_iter()
                .flatten()
                .map(|(a, b, _)| (a, b))
                .collect(),
            counts: vec![0; d.channels.len()],
        }
    }

    /// The record at `at` read as `event`: the records that now stand, in order. Past `HOLD`
    /// bytes held back, the bytes held.
    pub fn take(&mut self, at: u64, event: Event) -> Result<Vec<(u64, Event)>, usize> {
        let mut stands = matches!(event, Event::KnownThrough { .. } | Event::Footer(_));
        if let Event::Block(b) = &event
            && let BlockChannel::Analog { values, .. } = &b.data
        {
            self.counts[b.head.channel as usize] += values.len() as u64;
        }
        if matches!(event, Event::KnownThrough { .. }) {
            self.known = true;
        }
        self.bytes += held_size(&event);
        self.held.push((at, event));
        if !self.known {
            stands |= self
                .pairs
                .iter()
                .all(|&(a, b)| self.counts[a] == self.counts[b]);
        }
        if stands {
            self.bytes = 0;
            Ok(std::mem::take(&mut self.held))
        } else if self.bytes > HOLD {
            Err(self.bytes)
        } else {
            Ok(Vec::new())
        }
    }

    /// The records held back when the file ends: they go.
    pub fn dropped(self) -> Vec<(u64, Event)> {
        self.held
    }
}

/// A table as repair keeps it, with the page its entries make due until the file shows it.
struct Follow<R: Row> {
    table: TableWriter<R>,
    due: Option<Due>,
    /// Entries behind the page due, as a block's gaps or breaks fill a page partway.
    backlog: VecDeque<R>,
}

impl<R: Row> Follow<R> {
    fn new(channel: usize, table: Table, digital: bool) -> Self {
        Self {
            table: TableWriter::new(channel as u32, table, digital),
            due: None,
            backlog: VecDeque::new(),
        }
    }

    fn push(&mut self, row: R) {
        if self.due.is_some() {
            self.backlog.push_back(row);
        } else {
            self.due = self.table.push(row);
        }
    }

    /// The entries behind a page once it is placed, until the next page falls due.
    fn drain(&mut self) {
        while self.due.is_none()
            && let Some(row) = self.backlog.pop_front()
        {
            self.due = self.table.push(row);
        }
    }

    /// The file's page record at `at`, `len` bytes, which the reader has held to the one due. A
    /// table's last pages are a finish's, which repair writes again.
    fn page(&mut self, at: u64, len: u32) -> Result<(), RepairError> {
        let due = self.due.take().ok_or(RepairError::Overview)?;
        if due.record.len() != len as usize {
            return Err(RepairError::Overview);
        }
        self.due = self.table.placed(&due, at);
        self.drain();
        Ok(())
    }

    /// Writes what is due, then the table's last pages, to `tail` at `keep`.
    fn finish(&mut self, inline: u64, tail: &mut Vec<u8>, keep: u64) {
        self.finish_due(tail, keep);
        let mut due = self.table.finish_next(inline);
        loop {
            while let Some(d) = due {
                let at = keep + tail.len() as u64;
                tail.extend_from_slice(&d.record);
                due = self.table.placed(&d, at);
            }
            due = self.table.finish_next(inline);
            if due.is_none() {
                return;
            }
        }
    }
}

enum Data {
    Digital {
        initial: bool,
        edges: u64,
        gaps: Vec<Gap>,
        /// Each gap's `to` on the axis.
        gap_ends: Vec<i64>,
        /// The gaps the list holds: those whose ends a block read holds.
        listed: usize,
        /// The level after the last block: its start's, through its edges and gap ends.
        level: bool,
        list: Follow<Gap>,
    },
    Analog {
        count: u64,
        runs: Vec<Break>,
        list: Follow<Break>,
    },
}

struct Chan {
    hasher: ChannelHasher,
    map: Mapping,
    start: i64,
    next_start: i64,
    blocks: Follow<BlockRow>,
    blocks_count: u64,
    data: Data,
}

struct OChan {
    ladder: Option<Ladder>,
    column: Column,
    levels: Vec<Option<Follow<OverviewRow>>>,
}

struct State {
    declaration: Declaration,
    chans: Vec<Chan>,
    ochans: Vec<OChan>,
    pairs: Vec<Pair>,
    operand_of: Vec<Vec<usize>>,
    known: Option<i64>,
    /// The overview blocks the content settles that the file has not shown, by overview
    /// channel, in the order a writer settles them, which is the order it writes them.
    expected: VecDeque<(usize, Settled)>,
    /// What the content asked of the ladders and they have not done: an overview channel and
    /// the tick it settles to.
    asked: VecDeque<(usize, i64)>,
    /// The ladders' ends, once the file shows a writer's finish began them: the length, and
    /// each overview channel's strip and levels in blocks.
    ended: Option<(i64, Vec<Level>, Vec<Vec<u8>>)>,
}

impl State {
    fn new(d: Declaration) -> Result<Self, String> {
        let mut chans = Vec::with_capacity(d.channels.len());
        for (c, channel) in d.channels.iter().enumerate() {
            let (Some(hasher), Some(map), Some(start)) =
                (ChannelHasher::declared(&d, c), d.mapping(c), d.start(c))
            else {
                return Err(format!("channel {c} has no mapping to the axis"));
            };
            let digital = matches!(channel.data, DeclaredKind::Digital { .. });
            chans.push(Chan {
                hasher,
                map,
                start,
                next_start: 0,
                blocks: Follow::new(c, Table::Blocks, digital),
                blocks_count: 0,
                data: match &channel.data {
                    DeclaredKind::Digital { initial } => Data::Digital {
                        initial: *initial,
                        edges: 0,
                        gaps: Vec::new(),
                        gap_ends: Vec::new(),
                        listed: 0,
                        level: *initial,
                        list: Follow::new(c, Table::Gaps, true),
                    },
                    DeclaredKind::Analog { .. } => Data::Analog {
                        count: 0,
                        runs: Vec::new(),
                        list: Follow::new(c, Table::Breaks, false),
                    },
                },
            });
        }
        if let Some(o) = (0..d.channels.len() + d.derived.len()).find(|&o| !d.available(o)) {
            return Err(format!(
                "channel {o} is unavailable to this version, which can't write it again"
            ));
        }
        let columns = overview::columns(&d.channels, &d.derived);
        let ochans = columns
            .iter()
            .enumerate()
            .map(|(o, &column)| OChan {
                ladder: Some(overview::ladder_of(&d, o, column)),
                column,
                levels: Vec::new(),
            })
            .collect();
        let products = overview::products(&d.channels, &d.derived);
        let mut operand_of = vec![Vec::new(); d.channels.len()];
        for (k, &(a, b, _)) in products.iter().enumerate() {
            operand_of[a].push(k);
            if b != a {
                operand_of[b].push(k);
            }
        }
        Ok(Self {
            pairs: Pair::of(&d.channels, &d.derived),
            declaration: d,
            chans,
            ochans,
            operand_of,
            known: None,
            expected: VecDeque::new(),
            asked: VecDeque::new(),
            ended: None,
        })
    }

    /// An overview block record comes that the content has settled no block for: where every
    /// channel's blocks end at one tick, a writer's finish closed them there and then ended the
    /// ladders, whose last blocks follow. They end here as they did there.
    fn end_ladders(&mut self) {
        let length = self.chans.first().map_or(0, |c| c.next_start);
        if self.ended.is_some() || length < 1 || self.chans.iter().any(|c| c.next_start != length) {
            return;
        }
        let mut strips = Vec::with_capacity(self.ochans.len());
        let mut levels = Vec::with_capacity(self.ochans.len());
        for o in 0..self.ochans.len() {
            let Some(ladder) = self.ochans[o].ladder.take() else {
                return;
            };
            let end = ladder.finish(length);
            for s in end.settled {
                self.expected.push_back((o, s));
            }
            strips.push(end.strip);
            levels.push(end.levels);
        }
        self.ended = Some((length, strips, levels));
    }

    fn known_of(&self, o: usize) -> i64 {
        let k = self.known.unwrap_or(0);
        match self.chans.get(o) {
            Some(ch) => ch.next_start.max(k),
            None => {
                let [(a, _), (b, _)] = &self.pairs[o - self.chans.len()].operands;
                self.known_of(*a).min(self.known_of(*b))
            }
        }
    }

    /// Overview channel `o` settles to where it is known: asked now, done as the file's overview
    /// blocks or the finish need it, in the order asked, so a long span's blocks are made a few
    /// at a time.
    fn settle(&mut self, o: usize) {
        let to = self.known_of(o);
        self.asked.push_back((o, to));
    }

    /// Does what was asked, a step at a time, until an overview block of overview channel `o`'s
    /// level of `shift` is expected, or nothing is left to do. A channel's levels keep their own
    /// order, and no other.
    fn fill_for(&mut self, o: usize, shift: u8) {
        while !self
            .expected
            .iter()
            .any(|(x, s)| (*x, s.shift) == (o, shift))
        {
            let Some(&(a, to)) = self.asked.front() else {
                return;
            };
            let reached = match self.ochans[a].ladder.as_mut() {
                Some(l) => {
                    let reached = l.settle_some(to, SETTLE_STEP);
                    for s in l.take_settled() {
                        self.expected.push_back((a, s));
                    }
                    reached
                }
                None => true,
            };
            if reached {
                self.asked.pop_front();
            }
        }
    }

    /// The next overview block expected of overview channel `o`'s level of `shift`, taken.
    fn take_expected(&mut self, o: usize, shift: u8) -> Option<Settled> {
        let i = self
            .expected
            .iter()
            .position(|(x, s)| (*x, s.shift) == (o, shift))?;
        self.expected.remove(i).map(|(_, s)| s)
    }

    /// Does what was asked, a step at a time, until an overview block is expected or nothing is
    /// left to do.
    fn fill(&mut self) {
        while self.expected.is_empty() {
            let Some(&(o, to)) = self.asked.front() else {
                return;
            };
            let reached = match self.ochans[o].ladder.as_mut() {
                Some(l) => {
                    let reached = l.settle_some(to, SETTLE_STEP);
                    for s in l.take_settled() {
                        self.expected.push_back((o, s));
                    }
                    reached
                }
                None => true,
            };
            if reached {
                self.asked.pop_front();
            }
        }
    }

    /// Writes every overview block the content settles and the file lacks, in order, to `tail`
    /// at `keep`, each with its row and the pages that come due.
    fn write_expected(&mut self, tail: &mut Vec<u8>, keep: u64) -> Result<(), RepairError> {
        loop {
            self.fill();
            let Some((o, s)) = self.expected.pop_front() else {
                return Ok(());
            };
            let at = keep + tail.len() as u64;
            let frame = compress(&s.raw, OVERVIEW_BLOCK_LEVEL).map_err(WriteError::from)?;
            let head = OverviewHead {
                channel: o as u32,
                shift: s.shift,
                k: s.k,
                n: s.n,
            };
            record::write(tail, Kind::OverviewBlock, &[&head.to_bytes(), &frame]);
            let levels = &mut self.ochans[o].levels;
            if levels.len() <= s.li {
                levels.resize_with(s.li + 1, || None);
            }
            let follow =
                levels[s.li].get_or_insert_with(|| Follow::new(o, Table::Level(s.shift), false));
            follow.due = follow.table.push(OverviewRow {
                offset: at + (record::PAYLOAD_AT + OverviewHead::LEN) as u64,
                length: frame.len() as u32,
                decoded_length: s.raw.len() as u32,
                n: s.n,
            });
            follow.finish_due(tail, keep);
        }
    }

    fn settle_channel(&mut self, c: usize) {
        self.settle(c);
        for i in 0..self.operand_of[c].len() {
            let d = self.operand_of[c][i];
            self.settle(self.chans.len() + d);
        }
    }
}

/// Reads a cut-off file in order, its bytes fed as they come, and works out its footer.
pub struct Repairer {
    reader: Reader,
    level: i32,
    hold: Option<Hold>,
    state: Option<State>,
    /// Whether any annotation is kept.
    annotated: bool,
    /// The latest start among the annotations kept.
    latest_annotation: Option<i64>,
    /// Held to the declaration's channels once it is read.
    lines: Option<Lines>,
    left_out: Vec<String>,
    bundle: Option<Bundle>,
    /// A record that does not hold together: a derived channel's operands that don't pair.
    error: Option<RepairError>,
    /// Each generator's lines since its last part kept, in the order of its first annotation.
    generators: Vec<Pending>,
    parts: Follow<PartRow>,
    /// Where the file's finish began, at its first record only a finish writes, and the records
    /// from there on, which go as those after the cut do.
    finishing: Option<u64>,
    after: Vec<(u64, Event)>,
}

impl Repairer {
    /// `level` is the zstd level for what repair writes.
    pub fn new(level: i32) -> Self {
        Self {
            reader: Reader::new(MAX_DECODED),
            level,
            hold: None,
            state: None,
            annotated: false,
            latest_annotation: None,
            lines: None,
            left_out: Vec::new(),
            bundle: None,
            error: None,
            generators: Vec::new(),
            parts: Follow::new(0, Table::Parts, false),
            finishing: None,
            after: Vec::new(),
        }
    }

    /// Bundles the annotations into the file, as the writer would have. A file that holds
    /// annotations needs one: a recording with them ends with them bundled.
    pub fn bundle(&mut self, bundle: Bundle) {
        self.bundle = Some(bundle);
    }

    /// What `Repaired::left_out` will hold: empty until `finish`, which alone knows where the
    /// cut falls. A record before the cut stays where it is, and the part after it covers it
    /// whatever it holds.
    pub fn left_out(&self) -> &[String] {
        &self.left_out
    }

    /// The file's next bytes, from its start.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), RepairError> {
        self.reader.feed(bytes);
        while let Some(event) = self.reader.next_event()? {
            let at = self.reader.record_at();
            if let Event::Declaration(d) = &event {
                self.hold = Some(Hold::new(d));
                self.lines = Some(Lines::declared(d));
                match State::new((**d).clone()) {
                    Ok(state) => self.state = Some(state),
                    Err(e) => {
                        self.error.get_or_insert(RepairError::Decode(corrupt(e)));
                    }
                }
                continue;
            }
            if self.finishing.is_none() && finish_only(&event) {
                self.finishing = Some(at);
            }
            if self.finishing.is_some() {
                self.after.push((at, event));
                continue;
            }
            let Some(hold) = self.hold.as_mut() else {
                continue;
            };
            let stand = match hold.take(at, event) {
                Ok(stand) => stand,
                Err(held) => return Err(RepairError::HeldBack(held)),
            };
            for (at, event) in stand {
                if self.error.is_none()
                    && let Err(e) = self.commit(at, event)
                {
                    self.error = Some(e);
                }
            }
        }
        match self.error.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Generator `source`'s lines since its last part, met for the first time or not.
    fn generator(&mut self, source: &str) -> &mut Pending {
        let g = match self.generators.iter().position(|g| g.source == source) {
            Some(g) => g,
            None => {
                self.generators.push(Pending::new(source));
                self.generators.len() - 1
            }
        };
        &mut self.generators[g]
    }

    /// An annotation record kept: its line waits for a part.
    fn annotation(&mut self, a: record::Annotation) -> Result<(), RepairError> {
        let timebase = self.timebase();
        let span = placed_span(&a.line, timebase).map_err(|e| {
            corrupt(format!(
                "an annotation record of generator {:?}, which a part can't hold: {e}",
                a.source
            ))
        })?;
        self.latest_annotation = latest_start(self.latest_annotation, &a.line);
        self.generator(&a.source).push(&a.line, span);
        self.annotated = true;
        Ok(())
    }

    /// A part kept where it lies, or written again after the cut: its row, and its lines where it
    /// stands alone. One that covers records takes the lines its generator held for it.
    fn part(&mut self, part: &Part, row: PartRow, covers: u64) {
        let pending = self.generator(&part.source);
        if covers > 0 {
            pending.take();
        }
        self.parts.push(row);
        for line in part.lines() {
            self.latest_annotation = latest_start(self.latest_annotation, line);
            self.annotated = true;
        }
    }

    fn timebase(&self) -> crate::model::Ratio {
        self.reader
            .declaration()
            .expect("records follow the declaration")
            .timebase
    }

    /// A record that stands: its content counts, and its overview blocks and pages are held to
    /// what that content gives.
    fn commit(&mut self, at: u64, event: Event) -> Result<(), RepairError> {
        match event {
            Event::Annotation(a) => return self.annotation(a),
            Event::Part { part, row, covers } => {
                self.part(&part, row, covers);
                return Ok(());
            }
            Event::Page { head, len } if head.table == Table::Parts.code() => {
                return self.parts.page(at, len);
            }
            _ => {}
        }
        let Some(s) = self.state.as_mut() else {
            return Ok(());
        };
        match event {
            Event::Gap { channel, gap } => {
                let ch = &mut s.chans[channel];
                if let Data::Digital { gaps, gap_ends, .. } = &mut ch.data {
                    // The reader holds a gap's ends inside the axis. Its list takes it with the
                    // block that holds its end.
                    let to = ch.map.axis(gap.to).unwrap_or(i64::MAX);
                    gaps.push(gap);
                    gap_ends.push(to);
                    if let Some(l) = s.ochans[channel].ladder.as_mut() {
                        l.gap(gap, to);
                    }
                }
            }
            Event::Run { channel, run } => {
                // Its list takes it with the block that holds its first sample.
                if let Data::Analog { runs, .. } = &mut s.chans[channel].data {
                    runs.push(run);
                }
            }
            Event::UnreadBlock(_) => {
                return Err(corrupt("a block of a channel this version can't read").into());
            }
            Event::KnownThrough { tick } => {
                s.known = Some(tick);
                for o in 0..s.ochans.len() {
                    s.settle(o);
                }
            }
            Event::Block(_) if s.ended.is_some() => {
                return Err(corrupt("a block record after the overview's last blocks").into());
            }
            Event::Block(block) => {
                let c = block.head.channel as usize;
                let stored = s.chans.len();
                let source = block.data.source_ticks().unwrap_or_default().to_vec();
                let ch = &mut s.chans[c];
                match (&block.data, &mut ch.data) {
                    (
                        BlockChannel::Digital { edges, initial, .. },
                        Data::Digital {
                            edges: n,
                            gaps,
                            gap_ends,
                            listed,
                            level,
                            list,
                            ..
                        },
                    ) => {
                        let l = s.ochans[c]
                            .ladder
                            .as_mut()
                            .expect("a ladder until the finish");
                        for (&k, &tick) in source.iter().zip(edges) {
                            ch.hasher.edge(k);
                            l.edge(k, tick);
                            *n += 1;
                        }
                        *level = level_after(
                            *initial,
                            &source,
                            gaps_ending_in(gaps, gap_ends, block.head.start, block.head.end),
                        );
                        while *listed < gaps.len() && gap_ends[*listed] < block.head.end {
                            list.push(gaps[*listed]);
                            *listed += 1;
                        }
                    }
                    (
                        BlockChannel::Analog {
                            values,
                            first_index,
                        },
                        Data::Analog { count, runs, list },
                    ) => {
                        let to = *first_index + values.len() as u64;
                        let from = runs.partition_point(|r| r.index < (*first_index).max(1));
                        for r in runs[from..].iter().take_while(|r| r.index < to) {
                            list.push(*r);
                        }
                        for (index, value) in (*first_index..).zip(values.iter()) {
                            let tick = self.reader.sample_axis_tick(c, index).ok_or_else(|| {
                                corrupt(format!(
                                    "channel {c}: sample {index} has no place on the axis"
                                ))
                            })?;
                            ch.hasher.sample(value);
                            s.ochans[c]
                                .ladder
                                .as_mut()
                                .expect("a ladder until the finish")
                                .sample(tick, value);
                            for i in 0..s.operand_of[c].len() {
                                let d = s.operand_of[c][i];
                                if let Some(p) = s.pairs[d].take(c, value) {
                                    s.ochans[stored + d]
                                        .ladder
                                        .as_mut()
                                        .expect("a ladder until the finish")
                                        .sample(tick, p);
                                }
                            }
                            *count += 1;
                        }
                    }
                    _ => unreachable!("blocks follow the declaration"),
                }
                let ch = &mut s.chans[c];
                ch.next_start = block.head.end;
                ch.blocks_count += 1;
                ch.blocks.push(BlockRow {
                    start: block.head.start,
                    offset: block.frame_offset,
                    length: block.frame_len,
                    decoded_length: block.head.decoded_length,
                    word: block.head.word,
                });
                s.settle_channel(c);
            }
            Event::OverviewBlock { head, frame, .. } => {
                let o = head.channel as usize;
                s.fill_for(o, head.shift);
                if s.expected
                    .iter()
                    .all(|(x, w)| (*x, w.shift) != (o, head.shift))
                {
                    s.end_ladders();
                }
                let want = s
                    .take_expected(o, head.shift)
                    .filter(|w| (w.k, w.n) == (head.k, head.n))
                    .ok_or(RepairError::Overview)?;
                let oc = &mut s.ochans[o];
                if inflate_limited(&frame, want.raw.len(), MAX_DECODED)? != *want.raw {
                    return Err(RepairError::Overview);
                }
                let levels = &mut oc.levels;
                if levels.len() <= want.li {
                    levels.resize_with(want.li + 1, || None);
                }
                levels[want.li]
                    .get_or_insert_with(|| Follow::new(o, Table::Level(head.shift), false))
                    .push(OverviewRow {
                        offset: at + (record::PAYLOAD_AT + OverviewHead::LEN) as u64,
                        length: frame.len() as u32,
                        decoded_length: want.raw.len() as u32,
                        n: head.n,
                    });
            }
            Event::Page { head, len } => {
                let c = head.channel as usize;
                match head.table {
                    0 => s.chans[c].blocks.page(at, len)?,
                    1 => {
                        let oc = &mut s.ochans[c];
                        let li = oc
                            .levels
                            .iter()
                            .position(|l| {
                                l.as_ref()
                                    .is_some_and(|l| l.table.table() == Table::Level(head.shift))
                            })
                            .ok_or(RepairError::Overview)?;
                        oc.levels[li].as_mut().expect("found").page(at, len)?;
                    }
                    _ => match &mut s.chans[c].data {
                        Data::Digital { list, .. } => list.page(at, len)?,
                        Data::Analog { list, .. } => list.page(at, len)?,
                    },
                }
            }
            Event::Footer(_) => return Err(RepairError::Finished),
            Event::Declaration(_)
            | Event::Annotation(_)
            | Event::Part { .. }
            | Event::Bundle(_)
            | Event::Skipped { .. } => {}
        }
        Ok(())
    }

    /// No more bytes come: the footer for what was read.
    pub fn finish(mut self) -> Result<Repaired, RepairError> {
        let Ending::CutOff { at: end_at } = self.reader.end()? else {
            return Err(RepairError::Finished);
        };
        let mut state = self.state.take().ok_or(RepairError::NoDeclaration)?;
        let d = state.declaration.clone();
        if let Some(c) = d.channels.iter().find(|c| !c.shaping.is_whole()) {
            return Err(RepairError::Shaped(c.name.clone()));
        }
        let mut dropped = self.hold.take().map(Hold::dropped).unwrap_or_default();
        let keep = dropped
            .first()
            .map(|&(at, _)| at)
            .or(self.finishing)
            .unwrap_or(end_at);
        dropped.append(&mut self.after);
        // What stands after the cut, to write again in the order it came: annotation records
        // whose line checks, parts that stand alone, and, where a known-through record cuts the
        // file, gap records whose `from` maps before its tick.
        let mut again: Vec<Again> = Vec::new();
        for (_, event) in dropped {
            match event {
                Event::Annotation(a) => {
                    let checked = self
                        .lines
                        .as_mut()
                        .map_or(Ok(()), |l| l.take_line(&a.source, &a.line));
                    match checked {
                        Ok(()) => again.push(Again::Annotation(a)),
                        Err(why) => self
                            .left_out
                            .push(format!("generator {:?}: {why}", a.source)),
                    }
                }
                Event::Part {
                    part, covers: 0, ..
                } => again.push(Again::Part(part)),
                Event::Gap { channel, gap } => {
                    let map = state.chans[channel].map;
                    if state
                        .known
                        .is_some_and(|k| map.axis(gap.from).is_some_and(|t| t < k))
                    {
                        again.push(Again::Gap(channel, gap));
                    }
                }
                _ => {}
            }
        }
        for a in &again {
            match a {
                Again::Annotation(a) => {
                    self.latest_annotation = latest_start(self.latest_annotation, &a.line)
                }
                Again::Part(p) => {
                    for line in p.lines() {
                        self.latest_annotation = latest_start(self.latest_annotation, line);
                    }
                }
                Again::Gap(c, gap) => {
                    // A loss the recorder had begun to report: its channel ends at its `from`.
                    let to = state.chans[*c].map.axis(gap.to).unwrap_or(i64::MAX);
                    if let Data::Digital { gaps, gap_ends, .. } = &mut state.chans[*c].data {
                        gap_ends.push(to);
                        gaps.push(*gap);
                    }
                }
            }
        }
        let any_block = state.chans.iter().any(|c| c.blocks_count > 0);
        let any_annotation = self.annotated
            || again
                .iter()
                .any(|a| matches!(a, Again::Annotation(_) | Again::Part(_)));
        if !any_block && !any_annotation {
            return Err(RepairError::NoBlock);
        }
        let last_end = state.chans.iter().map(|c| c.next_start).max().unwrap_or(0);
        let length = repaired_length(state.known.unwrap_or(last_end), self.latest_annotation);
        if length < 1 {
            return Err(RepairError::NoBlock);
        }
        // The generators kept, those after the cut among them, which the bundle names.
        let mut generators = self.generators.clone();
        for a in &again {
            let source = match a {
                Again::Annotation(a) => &a.source,
                Again::Part(p) => &p.source,
                Again::Gap(..) => continue,
            };
            if !generators.iter().any(|g| &g.source == source) {
                generators.push(Pending::new(source));
            }
        }
        let records = !generators.is_empty();
        let bundling = Bundling::of(self.bundle.take(), &generators, records, d.timebase)?;
        let mut tail = Vec::new();
        // What the content settled and the file lacks, as a writer's queue would hold it.
        state.write_expected(&mut tail, keep)?;
        for a in again {
            match a {
                Again::Annotation(a) => {
                    record::write(&mut tail, Kind::Annotation, &[&a.to_payload()]);
                    self.annotation(a)?;
                }
                Again::Part(part) => {
                    // The generator's records since its last part go in a part of their own
                    // first, so this one still covers none.
                    self.write_pending(&part.source, &mut tail, keep)?;
                    let span = part
                        .span(d.timebase)
                        .map_err(|e| corrupt(format!("an annotation part: {e}")))?;
                    let row = write_part_record(&mut tail, keep, &part.to_raw(), span, self.level)?;
                    self.part(&part, row, 0);
                    self.parts.finish_due(&mut tail, keep);
                }
                Again::Gap(c, gap) => {
                    record::write(&mut tail, Kind::Gap, &[&record::gap_bytes(c as u32, gap)]);
                }
            }
        }
        // Each digital channel is known to the source tick at the latest known-through tick, or,
        // with none, at its own last block's end, or its start with no block.
        let known_to: Vec<i64> = state
            .chans
            .iter()
            .map(|ch| match (state.known, ch.blocks_count) {
                (Some(k), _) => ch.map.first_at(k).max(ch.start),
                (None, 0) => ch.start,
                (None, _) => ch.map.first_at(ch.next_start).max(ch.start),
            })
            .collect();
        // The gaps each channel's end keeps: those that end before it.
        let kept_gaps: Vec<usize> = state
            .chans
            .iter()
            .zip(&known_to)
            .map(|(ch, &last)| match &ch.data {
                Data::Digital { gaps, .. } => {
                    gaps.iter().position(|g| g.to >= last).unwrap_or(gaps.len())
                }
                Data::Analog { .. } => 0,
            })
            .collect();
        for (c, &kept) in kept_gaps.iter().enumerate() {
            if let Data::Digital { listed, gaps, .. } = &state.chans[c].data
                && *listed > kept
            {
                return Err(corrupt(format!(
                    "channel {c}: the end leaves out the gap from {}, whose block was written",
                    gaps[kept].from
                ))
                .into());
            }
            if let Data::Digital { gaps, .. } = &state.chans[c].data
                && let Some(g) = gaps.get(kept)
                && let Some(l) = state.ochans[c].ladder.as_mut()
            {
                l.forget_gaps(g.from);
            }
        }
        // Each channel's block from its last one's end to the length, as a writer's finish closes
        // them, each followed by the overview blocks it settles.
        let level = self.level;
        for c in 0..state.chans.len() {
            let ch = &mut state.chans[c];
            if ch.next_start >= length {
                continue;
            }
            let word = match &ch.data {
                Data::Digital { level: true, .. } => LEVEL_BIT,
                _ => 0,
            };
            let frame = compress(&[], level).map_err(WriteError::from)?;
            let head = BlockHead {
                channel: c as u32,
                start: ch.next_start,
                end: length,
                word,
                decoded_length: 0,
            };
            let at = keep + tail.len() as u64;
            record::write(&mut tail, Kind::Block, &[&head.to_bytes(), &frame]);
            ch.blocks.due = ch.blocks.table.push(BlockRow {
                start: head.start,
                offset: at + (record::PAYLOAD_AT + BlockHead::LEN) as u64,
                length: frame.len() as u32,
                decoded_length: 0,
                word,
            });
            ch.blocks.finish_due(&mut tail, keep);
            ch.blocks_count += 1;
            ch.next_start = length;
            if let Data::Digital {
                gaps, listed, list, ..
            } = &mut ch.data
            {
                list.finish_due(&mut tail, keep);
                while *listed < kept_gaps[c] {
                    list.push(gaps[*listed]);
                    *listed += 1;
                    list.finish_due(&mut tail, keep);
                }
            }
            state.settle_channel(c);
            state.write_expected(&mut tail, keep)?;
        }
        // The known-through record a writer's finish writes at the length, after the closes.
        if state.known.is_some_and(|k| length > k) {
            record::write(
                &mut tail,
                Kind::KnownThrough,
                &[&record::known_through_bytes(length)],
            );
        }
        // Each ladder to the length, its last overview blocks and its strip, unless the file
        // holds the finish that ended them.
        let (strips, ladders) = match state.ended.take() {
            Some((ended, strips, ladders)) if ended == length => (strips, ladders),
            Some(_) => return Err(RepairError::Overview),
            None => {
                let mut strips = Vec::with_capacity(state.ochans.len());
                let mut ladders = Vec::with_capacity(state.ochans.len());
                for o in 0..state.ochans.len() {
                    let end = state.ochans[o]
                        .ladder
                        .take()
                        .expect("a ladder until the finish")
                        .finish(length);
                    for s in end.settled {
                        state.expected.push_back((o, s));
                    }
                    strips.push(end.strip);
                    ladders.push(end.levels);
                }
                (strips, ladders)
            }
        };
        state.write_expected(&mut tail, keep)?;
        // Each generator's records since its last part, in parts, in the order of its first
        // annotation; then the parts a bundle of given files holds.
        for g in 0..self.generators.len() {
            let source = self.generators[g].source.clone();
            self.write_pending(&source, &mut tail, keep)?;
        }
        let mut bundling = bundling;
        if let Bundling::Files { parts, .. } = &mut bundling {
            for (raw, span) in std::mem::take(parts) {
                let row = write_part_record(&mut tail, keep, &raw, span, self.level)?;
                self.parts.push(row);
                self.parts.finish_due(&mut tail, keep);
            }
        }
        // Each channel's end, gaps and runs; each table's last pages.
        let mut digests = Vec::with_capacity(state.chans.len());
        let mut channels = Vec::with_capacity(state.chans.len());
        for (c, (ch, declared)) in state.chans.iter_mut().zip(&d.channels).enumerate() {
            ch.blocks.finish(0, &mut tail, keep);
            let data = match (&mut ch.data, &declared.data) {
                (
                    Data::Digital {
                        initial,
                        edges,
                        gaps,
                        list,
                        ..
                    },
                    DeclaredKind::Digital { .. },
                ) => {
                    // A gap that reaches the end ends what is known at its `from`, unless that
                    // lies past the end too: a recorder can learn of a loss before it writes the
                    // block the loss starts in.
                    let last = known_to[c];
                    let kept = kept_gaps[c];
                    let end = gaps.get(kept).map_or(last, |g| g.from.min(last));
                    list.finish(INLINE_LIST as u64, &mut tail, keep);
                    let gaps = gaps[..kept].to_vec();
                    digests.push(
                        ch.hasher
                            .clone()
                            .finish_digital(end, hash::gaps_listed(&gaps)),
                    );
                    let paged = gaps.len() > INLINE_LIST;
                    ChannelKind::Digital {
                        initial: *initial,
                        end,
                        edges: *edges,
                        gap_count: paged.then_some(gaps.len() as u64),
                        gaps: if paged { Vec::new() } else { gaps },
                    }
                }
                (
                    Data::Analog { count, runs, list },
                    DeclaredKind::Analog {
                        unit,
                        period,
                        coding,
                        significant,
                        phase,
                    },
                ) => {
                    list.finish(INLINE_LIST as u64, &mut tail, keep);
                    let runs: Vec<Break> =
                        runs.iter().copied().filter(|r| r.index < *count).collect();
                    let first = runs.first().map_or(0, |r| r.tick);
                    let lost_before = runs.first().map_or(0, |r| r.lost);
                    let breaks = runs.get(1..).unwrap_or_default().to_vec();
                    digests.push(ch.hasher.clone().finish_analog(
                        first,
                        *count,
                        hash::breaks_listed(&breaks),
                        (lost_before, 0),
                    ));
                    let paged = breaks.len() > INLINE_LIST;
                    ChannelKind::Analog {
                        unit: unit.clone(),
                        first,
                        period: *period,
                        phase: *phase,
                        count: *count,
                        break_count: paged.then_some(breaks.len() as u64),
                        breaks: if paged { Vec::new() } else { breaks },
                        coding: *coding,
                        significant: *significant,
                        lost_before,
                        lost_after: 0,
                    }
                }
                _ => unreachable!("channels follow the declaration"),
            };
            let levels = &state.ochans[c].levels;
            channels.push(ChannelHeader {
                name: declared.name.clone(),
                source_name: declared.source_name.clone(),
                source: declared.source,
                data,
                shaping: declared.shaping.clone(),
                overview_shift: declared.overview_shift,
                blocks: ch.blocks_count,
                overview_blocks: level_list(levels, &ladders[c], length),
                requires: Vec::new(),
                codec: None,
            });
        }
        digests.extend(d.derived.iter().map(hash::derived_digest));
        for oc in &mut state.ochans {
            for follow in oc.levels.iter_mut().flatten() {
                follow.finish(0, &mut tail, keep);
            }
        }
        self.parts.finish(0, &mut tail, keep);
        let stored = state.chans.len();
        let derived = d
            .derived
            .iter()
            .enumerate()
            .map(|(k, x)| DerivedHeader {
                name: x.name.clone(),
                unit: x.unit.clone(),
                op: x.op.clone(),
                operands: x.operands.clone(),
                params: x.params.clone(),
                significant: x.significant,
                requires: x.requires.clone(),
                overview_blocks: level_list(
                    &state.ochans[stored + k].levels,
                    &ladders[stored + k],
                    length,
                ),
            })
            .collect();
        let header = Header {
            timebase: d.timebase,
            length,
            channels,
            derived,
            sources: d.sources.clone(),
            title: d.title.clone(),
            meta: d.meta.clone(),
            content_id: hash::content_id(d.timebase, length, &digests),
            overview_shift: d.overview_shift,
            requires: Vec::new(),
            annotation_parts: {
                let parts = self.parts.table.count();
                (parts > 0).then_some(parts)
            },
        };
        check_header(&header)?;
        let files = match bundling {
            Bundling::None => None,
            Bundling::Generated { stem } => (!self.generators.is_empty()).then(|| {
                bundled_files(
                    &stem,
                    header.content_id,
                    self.generators.iter().map(|g| g.source.as_str()),
                )
            }),
            Bundling::Files { files, .. } => Some(files),
        };
        let bundle = match files {
            None => (0, 0),
            Some(files) => {
                let at = keep + tail.len() as u64;
                let payload = bundle_payload(&files, self.level)?;
                record::write(&mut tail, Kind::Bundle, &[&payload]);
                (at, (keep + tail.len() as u64 - at) as u32)
            }
        };
        let tops: Vec<ChannelTops> = (0..state.ochans.len())
            .map(|o| ChannelTops {
                blocks: state.chans.get(o).and_then(|ch| ch.blocks.table.top()),
                levels: state.ochans[o]
                    .levels
                    .iter()
                    .take(ladders[o].len())
                    .map(|t| t.as_ref().and_then(|f| f.table.top()))
                    .collect(),
                list: state.chans.get(o).and_then(|ch| match &ch.data {
                    Data::Digital { list, .. } => list.table.top(),
                    Data::Analog { list, .. } => list.table.top(),
                }),
            })
            .collect();
        let columns: Vec<Column> = state.ochans.iter().map(|o| o.column).collect();
        let footer = footer_payload(
            &header,
            bundle,
            self.parts.table.top(),
            &tops,
            &strips,
            &columns,
            self.level,
        )?;
        let at = keep + tail.len() as u64;
        let mut end = Vec::new();
        record::write_footer(&mut end, at, &footer);
        tail.extend_from_slice(&end);
        Ok(Repaired {
            keep,
            tail,
            header,
            left_out: self.left_out,
        })
    }
}

/// A record after the cut that stands, to write again.
enum Again {
    Annotation(record::Annotation),
    Part(Part),
    Gap(usize, Gap),
}

impl Repairer {
    /// Generator `source`'s lines since its last part, in parts of about `PART_TARGET` bytes and
    /// none past 4 MiB, to `tail` at `keep`, each with its row.
    fn write_pending(
        &mut self,
        source: &str,
        tail: &mut Vec<u8>,
        keep: u64,
    ) -> Result<(), RepairError> {
        let level = self.level;
        let timebase = self.timebase();
        let Some(g) = self.generators.iter().position(|g| g.source == source) else {
            return Ok(());
        };
        let Some((raw, _)) = self.generators[g].take() else {
            return Ok(());
        };
        let part = Part::parse_raw(&raw).map_err(corrupt)?;
        let parts =
            crate::writer::split_into_parts(source, part.lines(), timebase).map_err(corrupt)?;
        for (raw, span) in parts {
            let row = write_part_record(tail, keep, &raw, span, level)?;
            self.parts.push(row);
            self.parts.finish_due(tail, keep);
        }
        Ok(())
    }
}

impl<R: Row> Follow<R> {
    /// Writes the page due, if one is, and the directories it fills.
    fn finish_due(&mut self, tail: &mut Vec<u8>, keep: u64) {
        loop {
            let mut due = self.due.take();
            while let Some(d) = due {
                let at = keep + tail.len() as u64;
                tail.extend_from_slice(&d.record);
                due = self.table.placed(&d, at);
            }
            self.drain();
            if self.due.is_none() {
                return;
            }
        }
    }
}

/// A channel's levels in overview blocks for the header: each one's shift, blocks and rows.
fn level_list(
    levels: &[Option<Follow<OverviewRow>>],
    shifts: &[u8],
    length: i64,
) -> Vec<OverviewLevel> {
    shifts
        .iter()
        .enumerate()
        .map(|(li, &shift)| OverviewLevel {
            shift,
            blocks: overview::block_count(length, shift),
            rows: levels
                .get(li)
                .and_then(Option::as_ref)
                .map_or(0, |f| f.table.count()),
        })
        .collect()
}

/// Repairs a cut-off file held in memory.
pub fn repair(file: &[u8], level: i32) -> Result<Repaired, RepairError> {
    let mut r = Repairer::new(level);
    r.feed(file)?;
    r.finish()
}
