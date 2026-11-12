//! The in-order reader: a file's bytes in, from its start, and what they hold out, record by
//! record. It does no I/O. It needs no footer, so it reads a file cut off at any byte, or one a
//! recorder is still writing, up to its last whole record.

use std::collections::VecDeque;

use crate::decode::{
    BlockChannel, DecodeError, Footer, MAX_DECODED, Section, corrupt, decode_sections, edge_in_gap,
    inflate_limited, level_after,
};
use crate::header::{
    BlockRow, ChannelKind, Declaration, DeclaredKind, MAJOR, PREAMBLE_LEN, Preamble, TRAILER_LEN,
    Trailer,
};
use crate::model::{Break, Gap, run_sample_tick};
use crate::record::{self, BlockHead, Kind, Next};

/// One block, decoded, and where its frame sits in the file.
#[derive(Debug, PartialEq)]
pub struct ReadBlock {
    pub head: BlockHead,
    /// Where the block's zstd frame starts in the file, and its length.
    pub frame_offset: u64,
    pub frame_len: u32,
    pub channels: Vec<BlockChannel>,
}

#[derive(Debug)]
pub enum Event {
    Declaration(Box<Declaration>),
    Block(ReadBlock),
    Gap {
        channel: usize,
        gap: Gap,
    },
    /// Sample `index` of analog channel `channel` starts a run at `tick`.
    Run {
        channel: usize,
        index: u64,
        tick: i64,
    },
    /// An overview block's frame, not decoded: its size follows from the length, which the
    /// footer gives.
    OverviewBlock {
        shift: u8,
        k: u64,
        frame_offset: u64,
        frame: Vec<u8>,
    },
    Annotation(record::Annotation),
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

enum Channel {
    Digital {
        /// The level at the next block's start, as far as the blocks read so far say.
        level: bool,
        last_edge: i64,
        gaps: Vec<Gap>,
        /// Gaps whose `to` no block has passed yet.
        pending: VecDeque<Gap>,
    },
    Analog {
        next_index: u64,
        runs: Vec<Break>,
        coding: crate::header::AnalogCoding,
        period: crate::model::Ratio,
    },
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
    /// The next block's start.
    next_start: i64,
    /// The blocks read, as the footer's index must have them.
    rows: Vec<BlockRow>,
    /// The overview blocks read: shift, `k`, frame offset and length.
    overview_seen: Vec<(u8, u64, u64, u32)>,
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
            next_start: 0,
            rows: Vec::new(),
            overview_seen: Vec::new(),
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

    /// Where the next block starts: the end of the last one read.
    pub fn next_start(&self) -> i64 {
        self.next_start
    }

    pub fn declaration(&self) -> Option<&Declaration> {
        self.declaration.as_ref()
    }

    /// The tick of sample `index` of analog channel `c`, from the runs read so far.
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
                    let payload = payload.to_vec();
                    self.pos += len;
                    if let Some(event) = self.take(kind, &payload, record_at, len)? {
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
                self.channels = d
                    .channels
                    .iter()
                    .map(|c| match &c.data {
                        DeclaredKind::Digital { initial } => Channel::Digital {
                            level: *initial,
                            last_edge: 0,
                            gaps: Vec::new(),
                            pending: VecDeque::new(),
                        },
                        DeclaredKind::Analog { coding, period, .. } => Channel::Analog {
                            next_index: 0,
                            runs: Vec::new(),
                            coding: *coding,
                            period: *period,
                        },
                    })
                    .collect();
                self.declaration = Some(d.clone());
                Event::Declaration(Box::new(d))
            }
            Kind::Gap => {
                let (c, gap) =
                    record::parse_gap(payload).ok_or_else(|| corrupt("a gap record's size"))?;
                let c = channel(c)?;
                let start = self.next_start;
                let Channel::Digital {
                    last_edge,
                    gaps,
                    pending,
                    ..
                } = &mut self.channels[c]
                else {
                    return Err(corrupt("a gap on an analog channel"));
                };
                let after = gaps.last().map_or(*last_edge, |g| g.to.max(*last_edge));
                let first = gaps.is_empty() && *last_edge == 0;
                if gap.to <= gap.from
                    || gap.from < 0
                    || (!first && gap.from <= after)
                    || gap.to < start
                {
                    return Err(corrupt(format!(
                        "channel {c}: a gap from {} to {} is out of place",
                        gap.from, gap.to
                    )));
                }
                gaps.push(gap);
                pending.push_back(gap);
                Event::Gap { channel: c, gap }
            }
            Kind::Run => {
                let (c, index, tick) =
                    record::parse_run(payload).ok_or_else(|| corrupt("a run record's size"))?;
                let c = channel(c)?;
                let Channel::Analog {
                    next_index, runs, ..
                } = &mut self.channels[c]
                else {
                    return Err(corrupt("a run on a digital channel"));
                };
                let in_order = match runs.last() {
                    None => index == 0,
                    Some(r) => index > r.index && tick >= r.tick,
                };
                if !in_order || index < *next_index || tick < 0 {
                    return Err(corrupt(format!("channel {c}: a run is out of place")));
                }
                runs.push(Break { index, tick });
                Event::Run {
                    channel: c,
                    index,
                    tick,
                }
            }
            Kind::Block => Event::Block(self.block(payload, record_at)?),
            Kind::OverviewBlock => {
                let (shift, k, frame) = record::parse_overview(payload)
                    .ok_or_else(|| corrupt("an overview block record's size"))?;
                let frame_offset =
                    record_at + (record::PAYLOAD_AT + record::OVERVIEW_HEAD_LEN) as u64;
                self.overview_seen
                    .push((shift, k, frame_offset, frame.len() as u32));
                Event::OverviewBlock {
                    shift,
                    k,
                    frame_offset,
                    frame: frame.to_vec(),
                }
            }
            Kind::Annotation => Event::Annotation(serde_json::from_slice(payload)?),
            Kind::Footer => {
                // The footer parses as a random-access reader parses it: from its own bytes and
                // a trailer made for them, which the real one is checked against next.
                let mut bytes = Vec::with_capacity(len + TRAILER_LEN);
                record::write_footer(&mut bytes, record_at, payload);
                let file_len = record_at + bytes.len() as u64;
                let max = self.max;
                let footer =
                    Footer::parse_with(&bytes, file_len, |f, n| inflate_limited(f, n, max))?;
                self.check_footer(&footer)?;
                self.phase = Phase::Trailer {
                    footer_at: record_at,
                    footer: Box::new(footer),
                };
                return Ok(None);
            }
        }))
    }

    /// The footer says what the records said: the declaration, the blocks and the overview
    /// blocks where they were read, and the gaps and runs that lie inside the recording.
    fn check_footer(&mut self, footer: &Footer) -> Result<(), DecodeError> {
        let h = &footer.header;
        let d = self
            .declaration
            .as_ref()
            .expect("records follow the declaration");
        let mut source = h.source.clone();
        source.extra.retain(|k, _| d.source.extra.contains_key(k));
        let declared = h.timebase == d.timebase
            && h.title == d.title
            && h.meta == d.meta
            && h.overview_shift == d.overview_shift
            && source == d.source
            && h.channels.len() == d.channels.len()
            && h.channels
                .iter()
                .zip(&d.channels)
                .all(|(h, d)| h.declared() == *d);
        if !declared {
            return Err(corrupt("the footer's header differs from the declaration"));
        }
        if footer.rows != self.rows || self.next_start != h.length {
            return Err(corrupt("the footer's index differs from the blocks"));
        }
        let mut seen = std::mem::take(&mut self.overview_seen);
        seen.sort_by_key(|&(shift, k, ..)| (shift, k));
        let listed = h
            .overview_blocks
            .iter()
            .zip(&footer.overview_rows)
            .flat_map(|(l, rows)| {
                rows.iter()
                    .enumerate()
                    .map(move |(k, r)| (l.shift, k as u64, r.offset, r.length))
            });
        if !seen.iter().copied().eq(listed) {
            return Err(corrupt(
                "the footer's overview blocks differ from those read",
            ));
        }
        for ((state, c), i) in self.channels.iter().zip(&h.channels).zip(0..) {
            let fits = match (state, &c.data) {
                (Channel::Digital { gaps, .. }, ChannelKind::Digital { gaps: kept, .. }) => {
                    gaps.starts_with(kept)
                        && gaps[kept.len()..].first().is_none_or(|g| g.to >= h.length)
                }
                (
                    Channel::Analog { runs, .. },
                    ChannelKind::Analog {
                        first,
                        count,
                        breaks,
                        ..
                    },
                ) => {
                    let kept: Vec<Break> =
                        runs.iter().copied().filter(|r| r.index < *count).collect();
                    match kept.split_first() {
                        Some((r, rest)) => r.tick == *first && rest == breaks.as_slice(),
                        None => *count == 0 && breaks.is_empty(),
                    }
                }
                _ => false,
            };
            if !fits {
                return Err(corrupt(format!(
                    "channel {i}: the footer's gaps or runs differ from those read"
                )));
            }
        }
        Ok(())
    }

    fn block(&mut self, payload: &[u8], record_at: u64) -> Result<ReadBlock, DecodeError> {
        let channels = self.channels.len();
        let (head, frame) =
            BlockHead::parse(payload, channels).ok_or_else(|| corrupt("a block record's size"))?;
        let i = self.rows.len();
        if head.start != self.next_start || head.end <= head.start {
            return Err(corrupt(format!("block {i} is out of place")));
        }
        let raw = inflate_limited(frame, head.decoded_length as usize, self.max)?;
        let sections: Vec<Section> = self
            .channels
            .iter()
            .map(|c| match c {
                Channel::Digital { .. } => Section::Digital { end: i64::MAX },
                Channel::Analog {
                    next_index, coding, ..
                } => Section::Analog {
                    coding: *coding,
                    first_index: *next_index,
                },
            })
            .collect();
        let decoded = decode_sections(&raw, &sections, &head.words, head.start, head.end)
            .map_err(|what| corrupt(format!("block {i}: {what}")))?;
        for (c, (state, got)) in self.channels.iter_mut().zip(&decoded).enumerate() {
            let bad = |what: &str| corrupt(format!("block {i}, channel {c}: {what}"));
            match (state, got) {
                (
                    Channel::Digital {
                        level,
                        last_edge,
                        gaps,
                        pending,
                    },
                    BlockChannel::Digital { initial, edges },
                ) => {
                    // Gap ends before this block's start have taken effect.
                    while pending.front().is_some_and(|g| g.to < head.start) {
                        *level = pending.pop_front().unwrap().level;
                    }
                    if *initial != *level {
                        return Err(bad("the block starts at the wrong level"));
                    }
                    if edges.first().is_some_and(|&t| t < *last_edge) {
                        return Err(bad("an edge comes before the last one"));
                    }
                    if edge_in_gap(edges, gaps).is_some() {
                        return Err(bad("an edge lies inside a gap"));
                    }
                    let ending: Vec<Gap> = pending
                        .iter()
                        .take_while(|g| g.to < head.end)
                        .copied()
                        .collect();
                    *level = level_after(*initial, edges, &ending);
                    pending.drain(..ending.len());
                    if let Some(&t) = edges.last() {
                        *last_edge = t;
                    }
                }
                (
                    Channel::Analog {
                        next_index,
                        runs,
                        period,
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
                        let tick = |index| run_sample_tick(first.tick, *period, &runs[1..], index);
                        if tick(from) < head.start || tick(to) >= head.end {
                            return Err(bad("the samples lie outside the block's ticks"));
                        }
                    }
                    *next_index += n;
                }
                _ => unreachable!("sections follow the channels"),
            }
        }
        self.next_start = head.end;
        let frame_offset = record_at + (record::PAYLOAD_AT + BlockHead::len(channels)) as u64;
        self.rows.push(BlockRow {
            start: head.start,
            offset: frame_offset,
            length: frame.len() as u32,
            decoded_length: head.decoded_length,
            channels: head.words.clone(),
        });
        Ok(ReadBlock {
            frame_offset,
            frame_len: frame.len() as u32,
            head,
            channels: decoded,
        })
    }
}
