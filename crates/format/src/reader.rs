//! The in-order reader: a file's bytes in, from its start, and what they hold out, record by
//! record. It does no I/O. It needs no footer, so it reads a file cut off at any byte, or one a
//! recorder is still writing, up to its last whole record. It follows every table as the writer
//! built it, so each page and directory must be the one the records before it give, and the
//! footer's top index the one they all give.

use std::collections::{HashMap, VecDeque};

use crate::decode::{
    BlockChannel, DecodeError, Footer, MAX_DECODED, Section, check_declaration, corrupt,
    decode_section, edge_in_gap, inflate_limited, level_after, lost_before_start,
};
use crate::header::{
    BlockRow, ChannelKind, Declaration, DeclaredKind, INLINE_LIST, JSON_SAFE, MAJOR, OverviewRow,
    PREAMBLE_LEN, Preamble, TRAILER_LEN, Trailer,
};
use crate::index::{Due, PageHead, Row, Table, TableWriter};
use crate::mapping::Mapping;
use crate::model::{Break, Gap, run_sample_tick};
use crate::overview;
use crate::record::{self, BlockHead, Kind, Next, OverviewHead};

/// One block, decoded, and where its frame sits in the file.
#[derive(Debug, PartialEq)]
pub struct ReadBlock {
    pub head: BlockHead,
    /// Where the block's zstd frame starts in the file, and its length.
    pub frame_offset: u64,
    pub frame_len: u32,
    pub data: BlockChannel,
}

#[derive(Debug)]
pub enum Event {
    Declaration(Box<Declaration>),
    Block(ReadBlock),
    Gap {
        channel: usize,
        gap: Gap,
    },
    /// Sample `run.index` of analog channel `channel` starts a run at `run.tick`, after
    /// `run.lost` samples the instrument lost.
    Run {
        channel: usize,
        run: Break,
    },
    /// Every item before `tick` is in a block record before this one.
    KnownThrough {
        tick: i64,
    },
    /// An overview block's frame, not decoded: its size follows from the length, which the
    /// footer gives.
    OverviewBlock {
        head: OverviewHead,
        frame_offset: u64,
        frame: Vec<u8>,
    },
    /// An index or list page, which the reader has checked against the records before it: its
    /// head and its record's length.
    Page {
        head: PageHead,
        len: u32,
    },
    Annotation(record::Annotation),
    /// The annotation bundle record's payload (`decode::parse_bundle` reads it).
    Bundle(Vec<u8>),
    /// The footer, which the trailer has been checked against.
    Footer(Box<Footer>),
    /// A record a later minor version added, skipped.
    Skipped {
        kind: u8,
    },
}

/// How a file read in order ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ending {
    /// The footer and the trailer were read.
    Complete,
    /// Every record before byte `at` is whole; what follows is cut off, or does not check.
    CutOff { at: u64 },
}

enum Phase {
    Preamble,
    Records,
    /// The footer was read; the trailer follows. The footer record started at `footer_at`.
    Trailer {
        footer_at: u64,
        footer: Box<Footer>,
    },
    Done,
    /// Stopped at a record that is not whole.
    Stopped,
}

/// A table as the reader follows it: the writer's state, and the page record due next, which
/// must come before another entry does.
struct Follow<R: Row> {
    channel: u32,
    table: TableWriter<R>,
    due: Option<Due>,
    /// Its last pages have begun: no entry comes after.
    finishing: bool,
}

impl<R: Row> Follow<R> {
    fn new(channel: u32, table: Table, digital: bool) -> Self {
        Self {
            channel,
            table: TableWriter::new(channel, table, digital),
            due: None,
            finishing: false,
        }
    }

    fn push(&mut self, row: R) -> Result<(), DecodeError> {
        if self.due.is_some() || self.finishing {
            return Err(corrupt(format!(
                "channel {}'s {}: an entry comes where a page is due",
                self.channel(),
                crate::index::table_name(self.table.table())
            )));
        }
        self.due = self.table.push(row);
        Ok(())
    }

    fn channel(&self) -> u32 {
        self.channel
    }

    /// The page record `record`, at `at`, of this table: the one due, or the next of its last
    /// pages, a list's after dropping the entries its page leaves out.
    fn page(&mut self, record: &[u8], at: u64, count: u32, inline: u64) -> Result<(), DecodeError> {
        let due = match self.due.take() {
            Some(due) => due,
            None => {
                let mut finishing = self.table.clone();
                // Only a gap list's last page leaves entries out: the gaps past the end.
                if !self.finishing && finishing.table() == Table::Gaps {
                    let open = finishing.open().len() as u64;
                    if let Some(drop) = open.checked_sub(u64::from(count))
                        && drop > 0
                    {
                        finishing.drop_last(drop);
                    }
                }
                let due = finishing
                    .finish_next(inline)
                    .ok_or_else(|| corrupt("a page record that no table's entries give"))?;
                self.table = finishing;
                self.finishing = true;
                due
            }
        };
        if due.record != record {
            return Err(corrupt(format!(
                "a page of channel {}'s {} differs from the one the records before it give",
                self.channel,
                crate::index::table_name(self.table.table())
            )));
        }
        self.due = self.table.placed(&due, at);
        Ok(())
    }

    /// The table's top index entry once every page is read: a list of `inline` entries or fewer
    /// has none.
    fn top(&mut self, inline: u64) -> Result<Option<(u8, Vec<crate::index::Entry>)>, DecodeError> {
        if self.due.is_some() {
            return Err(corrupt("a page is due before the footer"));
        }
        if !self.finishing {
            self.finishing = true;
            if self.table.finish_next(inline).is_some() {
                return Err(corrupt("a table's last page is missing before the footer"));
            }
        } else if self.table.finish_next(inline).is_some() {
            return Err(corrupt(
                "a table's last directory is missing before the footer",
            ));
        }
        Ok(self.table.top())
    }
}

enum Channel {
    Digital {
        /// The level at the next block's start, as far as the blocks read so far say.
        level: bool,
        /// The last edge's source tick, or the channel's start before the first.
        last_edge: i64,
        edges: u64,
        gaps: Vec<Gap>,
        /// Gaps whose `to` no block has passed yet, each with its `to` on the axis.
        pending: VecDeque<(Gap, i64)>,
        map: Mapping,
        /// The channel's start, in its source's ticks.
        start: i64,
        list: Follow<Gap>,
    },
    Analog {
        next_index: u64,
        runs: Vec<Break>,
        coding: crate::header::AnalogCoding,
        period: crate::model::Ratio,
        map: Mapping,
        list: Follow<Break>,
    },
}

/// A source tick's place on the axis, where both lie in `0..2^53`.
fn on_axis(map: &Mapping, k: i64) -> Option<i64> {
    let safe = 0..JSON_SAFE as i64;
    map.axis(k)
        .filter(|t| safe.contains(t) && safe.contains(&k))
}

/// One overview channel's levels as the reader follows them: by shift, the next block's `k` and
/// the level's table.
struct Levels {
    finest: u8,
    by_shift: HashMap<u8, (u64, Follow<OverviewRow>)>,
}

pub struct Reader {
    max: usize,
    buf: Vec<u8>,
    /// Where `buf[0]` sits in the file.
    at: u64,
    /// How far into `buf` the reader has read.
    pos: usize,
    phase: Phase,
    declaration: Option<Declaration>,
    channels: Vec<Channel>,
    /// Each channel's next block's start, its blocks so far and its index.
    next_start: Vec<i64>,
    blocks: Vec<Follow<BlockRow>>,
    levels: Vec<Levels>,
    /// The latest known-through record's tick.
    known: Option<i64>,
    /// The annotation bundle record's place, once read.
    bundle: Option<(u64, u32)>,
    /// Where the record the last event came from starts.
    record_at: u64,
    /// Bytes fed past the trailer.
    after_end: bool,
}

impl Default for Reader {
    fn default() -> Self {
        Self::new(MAX_DECODED)
    }
}

impl Reader {
    /// A reader that holds at most `max` bytes of one record, and decodes at most `max` bytes of
    /// one frame.
    pub fn new(max: usize) -> Self {
        Self {
            max,
            buf: Vec::new(),
            at: 0,
            pos: 0,
            phase: Phase::Preamble,
            declaration: None,
            channels: Vec::new(),
            next_start: Vec::new(),
            blocks: Vec::new(),
            levels: Vec::new(),
            known: None,
            bundle: None,
            record_at: 0,
            after_end: false,
        }
    }

    /// The file's next bytes.
    pub fn feed(&mut self, bytes: &[u8]) {
        if matches!(self.phase, Phase::Done) {
            self.after_end |= !bytes.is_empty();
            return;
        }
        if self.pos > 0 && self.pos >= self.buf.len() / 2 {
            self.buf.drain(..self.pos);
            self.at += self.pos as u64;
            self.pos = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Where the reader has got to: every byte before this is read.
    pub fn position(&self) -> u64 {
        self.at + self.pos as u64
    }

    /// Where the record the last event came from starts.
    pub fn record_at(&self) -> u64 {
        self.record_at
    }

    /// Where channel `c`'s next block starts: the end of its last one read.
    pub fn next_start(&self, c: usize) -> Option<i64> {
        self.next_start.get(c).copied()
    }

    /// The latest known-through record's tick.
    pub fn known_through(&self) -> Option<i64> {
        self.known
    }

    pub fn declaration(&self) -> Option<&Declaration> {
        self.declaration.as_ref()
    }

    /// Channel `c`'s mapping from its source's ticks to the axis, once the declaration is read.
    pub fn mapping(&self, c: usize) -> Option<Mapping> {
        match self.channels.get(c)? {
            Channel::Digital { map, .. } | Channel::Analog { map, .. } => Some(*map),
        }
    }

    /// The axis tick of sample `index` of analog channel `c`, from the runs read so far.
    pub fn sample_axis_tick(&self, c: usize, index: u64) -> Option<i64> {
        on_axis(&self.mapping(c)?, self.sample_tick(c, index)?)
    }

    /// The source tick of sample `index` of analog channel `c`, from the runs read so far.
    pub fn sample_tick(&self, c: usize, index: u64) -> Option<i64> {
        match self.channels.get(c)? {
            Channel::Analog { runs, period, .. } => {
                let first = runs.first().filter(|r| r.index == 0)?;
                Some(run_sample_tick(first.tick, *period, &runs[1..], index))
            }
            Channel::Digital { .. } => None,
        }
    }

    /// The next event, or None until more bytes come, or after the end or a record that is not
    /// whole. An error is a file that cannot be a recording: a record that checks but holds what
    /// the format forbids.
    pub fn next_event(&mut self) -> Result<Option<Event>, DecodeError> {
        loop {
            let rest = &self.buf[self.pos..];
            match &self.phase {
                Phase::Done | Phase::Stopped => return Ok(None),
                Phase::Preamble => {
                    if rest.len() < PREAMBLE_LEN {
                        if !crate::header::MAGIC.starts_with(&rest[..rest.len().min(8)]) {
                            return Err(DecodeError::NotTickwave);
                        }
                        return Ok(None);
                    }
                    let p = Preamble::parse(rest).ok_or(DecodeError::NotTickwave)?;
                    if p.major != MAJOR {
                        return Err(DecodeError::Version {
                            major: p.major,
                            minor: p.minor,
                        });
                    }
                    self.pos += PREAMBLE_LEN;
                    self.phase = Phase::Records;
                }
                Phase::Trailer { footer_at, .. } => {
                    if rest.len() < TRAILER_LEN {
                        return Ok(None);
                    }
                    let footer_at = *footer_at;
                    let trailer = Trailer::parse(&rest[..TRAILER_LEN])
                        .ok_or_else(|| corrupt("the footer is not followed by a trailer"))?;
                    let footer_len = self.position() - footer_at;
                    if trailer.footer_offset != footer_at
                        || u64::from(trailer.footer_len) != footer_len
                    {
                        return Err(corrupt("the trailer does not point at the footer"));
                    }
                    if rest.len() > TRAILER_LEN {
                        return Err(corrupt("bytes follow the trailer"));
                    }
                    self.pos += TRAILER_LEN;
                    let Phase::Trailer { footer, .. } =
                        std::mem::replace(&mut self.phase, Phase::Done)
                    else {
                        unreachable!()
                    };
                    self.record_at = footer_at;
                    return Ok(Some(Event::Footer(footer)));
                }
                Phase::Records => {
                    let (kind, payload, len) = match record::next(rest, self.max) {
                        Next::Need(_) => return Ok(None),
                        Next::Bad => {
                            self.phase = Phase::Stopped;
                            return Ok(None);
                        }
                        Next::TooLarge(n) => {
                            return Err(corrupt(format!(
                                "the record at byte {} holds {n} bytes, more than this reader holds ({})",
                                self.position(),
                                self.max
                            )));
                        }
                        Next::Record { kind, payload, len } => (kind, payload, len),
                    };
                    let record_at = self.position();
                    let record = rest[..len].to_vec();
                    let payload = payload.to_vec();
                    self.pos += len;
                    self.record_at = record_at;
                    if let Some(event) = self.take(kind, &payload, &record, record_at, len)? {
                        return Ok(Some(event));
                    }
                }
            }
        }
    }

    /// Ends the reading: no more bytes come.
    pub fn end(&self) -> Result<Ending, DecodeError> {
        if self.after_end {
            return Err(corrupt("bytes follow the trailer"));
        }
        Ok(match &self.phase {
            Phase::Done => Ending::Complete,
            // A footer without its whole trailer goes too: a repair writes both again.
            Phase::Trailer { footer_at, .. } => Ending::CutOff { at: *footer_at },
            _ => Ending::CutOff {
                at: self.position(),
            },
        })
    }

    fn take(
        &mut self,
        kind: u8,
        payload: &[u8],
        record_bytes: &[u8],
        record_at: u64,
        len: usize,
    ) -> Result<Option<Event>, DecodeError> {
        let Some(kind) = Kind::from_byte(kind) else {
            if record::skippable(kind) {
                return Ok(Some(Event::Skipped { kind }));
            }
            return Err(corrupt(format!("a record of unknown kind {kind}")));
        };
        if (kind == Kind::Declaration) == self.declaration.is_some() {
            return Err(corrupt(if kind == Kind::Declaration {
                "a second declaration"
            } else {
                "the first record is not the declaration"
            }));
        }
        if self.bundle.is_some() && kind != Kind::Footer {
            return Err(corrupt(
                "a record comes between the annotation bundle and the footer",
            ));
        }
        let channel_count = self.channels.len();
        let channel = |c: u32| -> Result<usize, DecodeError> {
            let c = c as usize;
            if c < channel_count {
                Ok(c)
            } else {
                Err(corrupt(format!(
                    "a record names channel {c}, past the last"
                )))
            }
        };
        Ok(Some(match kind {
            Kind::Declaration => {
                let d: Declaration = serde_json::from_slice(payload)?;
                check_declaration(&d)?;
                let unmapped = || corrupt("a channel has no mapping to the axis");
                let maps = d.mappings().ok_or_else(unmapped)?;
                let mut channels = Vec::with_capacity(d.channels.len());
                let mut blocks = Vec::with_capacity(d.channels.len());
                for (c, (channel, map)) in d.channels.iter().zip(maps).enumerate() {
                    let digital = matches!(channel.data, DeclaredKind::Digital { .. });
                    blocks.push(Follow::new(c as u32, Table::Blocks, digital));
                    channels.push(match &channel.data {
                        DeclaredKind::Digital { initial } => {
                            let start = d.start(c).ok_or_else(unmapped)?;
                            Channel::Digital {
                                level: *initial,
                                last_edge: start,
                                edges: 0,
                                gaps: Vec::new(),
                                pending: VecDeque::new(),
                                map,
                                start,
                                list: Follow::new(c as u32, Table::Gaps, true),
                            }
                        }
                        DeclaredKind::Analog { coding, period, .. } => Channel::Analog {
                            next_index: 0,
                            runs: Vec::new(),
                            coding: *coding,
                            period: *period,
                            map,
                            list: Follow::new(c as u32, Table::Breaks, false),
                        },
                    });
                }
                let overview_channels = d.channels.len() + d.derived.len();
                self.levels = (0..overview_channels)
                    .map(|o| Levels {
                        finest: d.shift(o).expect("a checked declaration's channel"),
                        by_shift: HashMap::new(),
                    })
                    .collect();
                self.next_start = vec![0; d.channels.len()];
                self.channels = channels;
                self.blocks = blocks;
                self.declaration = Some(d.clone());
                Event::Declaration(Box::new(d))
            }
            Kind::Gap => {
                let (c, gap) =
                    record::parse_gap(payload).ok_or_else(|| corrupt("a gap record's size"))?;
                let c = channel(c)?;
                let start = self.next_start[c];
                let Channel::Digital {
                    last_edge,
                    edges,
                    gaps,
                    pending,
                    map,
                    start: channel_start,
                    list,
                    ..
                } = &mut self.channels[c]
                else {
                    return Err(corrupt("a gap on an analog channel"));
                };
                let after = gaps.last().map_or(*last_edge, |g| g.to.max(*last_edge));
                let first = gaps.is_empty() && *edges == 0;
                let to = on_axis(map, gap.to);
                if gap.to <= gap.from
                    || gap.from < *channel_start
                    || (!first && gap.from <= after)
                    || on_axis(map, gap.from).is_none()
                    || to.is_none_or(|t| t < start)
                {
                    return Err(corrupt(format!(
                        "channel {c}: a gap from {} to {} is out of place",
                        gap.from, gap.to
                    )));
                }
                gaps.push(gap);
                pending.push_back((gap, to.unwrap_or(i64::MAX)));
                list.push(gap)?;
                Event::Gap { channel: c, gap }
            }
            Kind::Run => {
                let (c, run) =
                    record::parse_run(payload).ok_or_else(|| corrupt("a run record's size"))?;
                let c = channel(c)?;
                let Channel::Analog {
                    next_index,
                    runs,
                    map,
                    period,
                    list,
                    ..
                } = &mut self.channels[c]
                else {
                    return Err(corrupt("a run on a digital channel"));
                };
                let in_order = match runs.last() {
                    None => {
                        run.index == 0
                            && (run.lost == 0
                                || lost_before_start(run.tick, *period, run.lost)
                                    .and_then(|t| on_axis(map, t))
                                    .is_some())
                    }
                    Some(r) => run.index > r.index && run.tick >= r.tick,
                };
                if !in_order
                    || run.index < *next_index
                    || run.lost >= JSON_SAFE
                    || on_axis(map, run.tick).is_none()
                {
                    return Err(corrupt(format!("channel {c}: a run is out of place")));
                }
                runs.push(run);
                if run.index > 0 {
                    list.push(run)?;
                }
                Event::Run { channel: c, run }
            }
            Kind::KnownThrough => {
                let tick = record::parse_known_through(payload)
                    .ok_or_else(|| corrupt("a known-through record's size"))?;
                if tick < 0 || self.known.is_some_and(|k| tick <= k) {
                    return Err(corrupt(format!(
                        "a known-through record at tick {tick} does not come after the last"
                    )));
                }
                self.known = Some(tick);
                Event::KnownThrough { tick }
            }
            Kind::Block => Event::Block(self.block(payload, record_at)?),
            Kind::OverviewBlock => {
                let (head, frame) = OverviewHead::parse(payload)
                    .ok_or_else(|| corrupt("an overview block record's size"))?;
                let o = head.channel as usize;
                let levels = self
                    .levels
                    .get_mut(o)
                    .ok_or_else(|| corrupt("an overview block names a channel past the last"))?;
                let on_ladder = head.shift >= levels.finest
                    && head.shift <= overview::MAX_SHIFT
                    && (head.shift - levels.finest).is_multiple_of(overview::STEP);
                let (k, follow) = levels
                    .by_shift
                    .entry(head.shift)
                    .or_insert_with(|| (0, Follow::new(o as u32, Table::Level(head.shift), false)));
                if !on_ladder || head.k != *k || head.n == 0 {
                    return Err(corrupt(format!(
                        "channel {o}: an overview block of level 2^{} at block {} is out of place",
                        head.shift, head.k
                    )));
                }
                *k += u64::from(head.n);
                let frame_offset = record_at + (record::PAYLOAD_AT + OverviewHead::LEN) as u64;
                // A level's last block may hold fewer buckets, which only the length says: the
                // row takes the size the frame states, which the footer's rows are held to.
                let decoded = zstd::zstd_safe::get_frame_content_size(frame)
                    .ok()
                    .flatten()
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or_else(|| corrupt("an overview block's frame does not state its size"))?;
                follow.push(OverviewRow {
                    offset: frame_offset,
                    length: frame.len() as u32,
                    decoded_length: decoded,
                    n: head.n,
                })?;
                Event::OverviewBlock {
                    head,
                    frame_offset,
                    frame: frame.to_vec(),
                }
            }
            Kind::IndexPage | Kind::ListPage => {
                let (head, _) =
                    PageHead::parse(payload).ok_or_else(|| corrupt("a page record's size"))?;
                self.page(head, record_bytes, record_at)?;
                Event::Page {
                    head,
                    len: len as u32,
                }
            }
            Kind::Annotation => {
                let a: record::Annotation = serde_json::from_slice(payload)?;
                // Repair and recut write each source's annotations to a file named after it.
                if let Some(problem) = record::check_source(&a.source) {
                    return Err(corrupt(format!("an annotation record: {problem}")));
                }
                Event::Annotation(a)
            }
            Kind::Bundle => {
                self.bundle = Some((record_at, len as u32));
                Event::Bundle(payload.to_vec())
            }
            Kind::Footer => {
                // The footer parses as a random-access reader parses it: from its own bytes and
                // a trailer made for them, which the real one is checked against next.
                let mut bytes = Vec::with_capacity(len + TRAILER_LEN);
                record::write_footer(&mut bytes, record_at, payload);
                let file_len = record_at + bytes.len() as u64;
                let footer = Footer::parse(&bytes, file_len)?;
                self.check_footer(&footer, record_at)?;
                self.phase = Phase::Trailer {
                    footer_at: record_at,
                    footer: Box::new(footer),
                };
                return Ok(None);
            }
        }))
    }

    /// A page record: the one due in its table, or its next last page.
    fn page(&mut self, head: PageHead, record: &[u8], at: u64) -> Result<(), DecodeError> {
        let c = head.channel as usize;
        let unexpected = || corrupt("a page record that no table's entries give");
        match head.table {
            0 => self
                .blocks
                .get_mut(c)
                .ok_or_else(unexpected)?
                .page(record, at, head.count, 0),
            1 => {
                let (_, follow) = self
                    .levels
                    .get_mut(c)
                    .and_then(|l| l.by_shift.get_mut(&head.shift))
                    .ok_or_else(unexpected)?;
                follow.page(record, at, head.count, 0)
            }
            2 | 3 => match (self.channels.get_mut(c), head.table) {
                (Some(Channel::Digital { list, .. }), 2) => {
                    list.page(record, at, head.count, INLINE_LIST as u64)
                }
                (Some(Channel::Analog { list, .. }), 3) => {
                    list.page(record, at, head.count, INLINE_LIST as u64)
                }
                _ => Err(unexpected()),
            },
            _ => Err(unexpected()),
        }
    }

    /// The footer says what the records said: the declaration, each channel's blocks and its
    /// index, each level's overview blocks and its index, the gaps and runs that lie inside the
    /// recording, and the annotation bundle where it was read.
    fn check_footer(&mut self, footer: &Footer, footer_at: u64) -> Result<(), DecodeError> {
        let h = &footer.header;
        let d = self
            .declaration
            .as_ref()
            .expect("records follow the declaration");
        let mut sources = h.sources.clone();
        for (s, declared) in sources.iter_mut().zip(&d.sources) {
            s.extra.retain(|k, _| declared.extra.contains_key(k));
        }
        let declared = h.timebase == d.timebase
            && h.title == d.title
            && h.meta == d.meta
            && h.overview_shift == d.overview_shift
            && sources == d.sources
            && h.derived.len() == d.derived.len()
            && h.derived
                .iter()
                .zip(&d.derived)
                .all(|(h, d)| h.declared() == *d)
            && h.channels.len() == d.channels.len()
            && h.channels
                .iter()
                .zip(&d.channels)
                .all(|(h, d)| h.declared() == *d);
        if !declared {
            return Err(corrupt("the footer's header differs from the declaration"));
        }
        if self.known.is_some_and(|k| k > h.length) {
            return Err(corrupt("a known-through record lies past the length"));
        }
        if footer.bundle != self.bundle
            || self
                .bundle
                .is_some_and(|(at, len)| at + u64::from(len) != footer_at)
        {
            return Err(corrupt(
                "the footer's annotation bundle differs from the one read",
            ));
        }
        let differs = |c: usize, what: &str| {
            corrupt(format!(
                "channel {c}: the footer's {what} differs from what was read"
            ))
        };
        for (c, ch) in h.channels.iter().enumerate() {
            if self.next_start[c] != h.length || self.blocks[c].table.count() != ch.blocks {
                return Err(differs(c, "blocks"));
            }
            let top = self.blocks[c].top(0)?;
            if top != footer.channels[c].blocks.as_ref().map(top_of) {
                return Err(differs(c, "block index"));
            }
            let fits = match (&mut self.channels[c], &ch.data) {
                // A gap left out of the header reaches its channel's end, which is then at its
                // `from` or sooner; a repaired file's can end inside the length its marks gave.
                (
                    Channel::Digital {
                        gaps, edges, list, ..
                    },
                    ChannelKind::Digital {
                        gaps: kept,
                        gap_count,
                        end,
                        edges: e,
                        ..
                    },
                ) => {
                    let n = gap_count.unwrap_or(kept.len() as u64) as usize;
                    // A paged list's pages were each held to the records' gaps as they came.
                    let prefix = gap_count.is_some() || gaps.starts_with(kept);
                    let rest_past = gaps
                        .get(n..)
                        .is_some_and(|r| r.first().is_none_or(|g| g.from >= *end));
                    let top = list.top(INLINE_LIST as u64)?;
                    let stated = footer.channels[c].list.as_ref().map(top_of);
                    *edges == *e && prefix && rest_past && top == stated
                }
                (
                    Channel::Analog {
                        runs,
                        next_index,
                        list,
                        ..
                    },
                    ChannelKind::Analog {
                        first,
                        count,
                        breaks,
                        break_count,
                        lost_before,
                        ..
                    },
                ) => {
                    let kept: Vec<Break> =
                        runs.iter().copied().filter(|r| r.index < *count).collect();
                    let runs_fit = match kept.split_first() {
                        Some((r, rest)) => {
                            r.tick == *first
                                && r.lost == *lost_before
                                && match break_count {
                                    None => rest == breaks.as_slice(),
                                    Some(n) => rest.len() as u64 == *n,
                                }
                        }
                        None => *count == 0 && breaks.is_empty() && break_count.is_none(),
                    };
                    let top = list.top(INLINE_LIST as u64)?;
                    let stated = footer.channels[c].list.as_ref().map(top_of);
                    *next_index == *count && runs_fit && top == stated
                }
                _ => false,
            };
            if !fits {
                return Err(differs(c, "gaps or runs"));
            }
        }
        for (o, index) in footer.channels.iter().enumerate() {
            let levels = &mut self.levels[o];
            if levels.by_shift.len() != index.levels.len() {
                return Err(differs(o, "overview levels"));
            }
            for (level, table) in &index.levels {
                let Some((k, follow)) = levels.by_shift.get_mut(&level.shift) else {
                    return Err(differs(o, "overview levels"));
                };
                let top = follow.top(0)?;
                if *k != level.blocks
                    || follow.table.count() != level.rows
                    || top != Some(top_of(table))
                {
                    return Err(differs(o, "overview levels"));
                }
            }
        }
        Ok(())
    }

    fn block(&mut self, payload: &[u8], record_at: u64) -> Result<ReadBlock, DecodeError> {
        let (head, frame) =
            BlockHead::parse(payload).ok_or_else(|| corrupt("a block record's size"))?;
        let c = head.channel as usize;
        if c >= self.channels.len() {
            return Err(corrupt(format!("a block names channel {c}, past the last")));
        }
        if head.start != self.next_start[c] || head.end <= head.start {
            return Err(corrupt(format!(
                "channel {c}: a block from {} to {} is out of place",
                head.start, head.end
            )));
        }
        let raw = inflate_limited(frame, head.decoded_length as usize, self.max)?;
        let section = match &self.channels[c] {
            Channel::Digital { start, map, .. } => Section::Digital {
                start: *start,
                end: i64::MAX,
                map: *map,
            },
            Channel::Analog {
                next_index, coding, ..
            } => Section::Analog {
                coding: *coding,
                first_index: *next_index,
            },
        };
        let data = decode_section(&raw, &section, head.word, head.start, head.end)
            .map_err(|what| corrupt(format!("channel {c}, the block at {}: {what}", head.start)))?;
        let known = self.known.unwrap_or(i64::MIN);
        let bad = |what: &str| corrupt(format!("channel {c}, the block at {}: {what}", head.start));
        match (&mut self.channels[c], &data) {
            (
                Channel::Digital {
                    level,
                    last_edge,
                    edges: seen,
                    gaps,
                    pending,
                    ..
                },
                BlockChannel::Digital { initial, edges, .. },
            ) => {
                let source = data.source_ticks().unwrap_or_default();
                // Gap ends before this block's start have taken effect.
                while let Some(&(g, _)) = pending.front().filter(|&&(_, t)| t < head.start) {
                    *level = g.level;
                    pending.pop_front();
                }
                if *initial != *level {
                    return Err(bad("the block starts at the wrong level"));
                }
                if source.first().is_some_and(|&t| t < *last_edge) {
                    return Err(bad("an edge comes before the last one"));
                }
                if edges.first().is_some_and(|&t| t < known) {
                    return Err(bad("an edge lies before the latest known-through tick"));
                }
                if edge_in_gap(source, gaps).is_some() {
                    return Err(bad("an edge lies inside a gap"));
                }
                let ending: Vec<Gap> = pending
                    .iter()
                    .take_while(|&&(_, t)| t < head.end)
                    .map(|&(g, _)| g)
                    .collect();
                *level = level_after(*initial, source, &ending);
                pending.drain(..ending.len());
                if let Some(&t) = source.last() {
                    *last_edge = t;
                }
                *seen += source.len() as u64;
            }
            (
                Channel::Analog {
                    next_index,
                    runs,
                    period,
                    map,
                    ..
                },
                BlockChannel::Analog { values, .. },
            ) => {
                let n = values.len() as u64;
                if n > 0 {
                    let (from, to) = (*next_index, *next_index + n - 1);
                    let Some(first) = runs.first().filter(|r| r.index == 0) else {
                        return Err(bad("a sample before the channel's first run"));
                    };
                    let tick = |index| {
                        on_axis(map, run_sample_tick(first.tick, *period, &runs[1..], index))
                    };
                    if tick(from).is_none_or(|t| t < head.start.max(known))
                        || tick(to).is_none_or(|t| t >= head.end)
                    {
                        return Err(bad(
                            "the samples lie outside the block's ticks, or before the latest \
                             known-through tick",
                        ));
                    }
                }
                *next_index += n;
            }
            _ => unreachable!("sections follow the channels"),
        }
        self.next_start[c] = head.end;
        let frame_offset = record_at + (record::PAYLOAD_AT + BlockHead::LEN) as u64;
        self.blocks[c].push(BlockRow {
            start: head.start,
            offset: frame_offset,
            length: frame.len() as u32,
            decoded_length: head.decoded_length,
            word: head.word,
        })?;
        Ok(ReadBlock {
            frame_offset,
            frame_len: frame.len() as u32,
            head,
            data,
        })
    }
}

/// A finished table's depth and top-index entries.
fn top_of(t: &crate::index::TableIndex) -> (u8, Vec<crate::index::Entry>) {
    let depth = t.levels.len() - 1;
    (depth as u8, t.levels[depth].clone())
}
