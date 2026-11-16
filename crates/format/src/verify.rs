//! Checks a file against the source it was converted from, reading the file in order: every edge
//! and sample, every gap and run, each channel's overview blocks and strip, and the content id.
//! A channel stores its source's ticks, so the two compare directly.

use std::collections::{HashMap, VecDeque};
use std::io::Read;

use crate::decode::{BlockChannel, DecodeError, HOLD, MAX_DECODED, inflate_limited};
use crate::hash::{self, ChannelHasher};
use crate::header::{AnalogCoding, ChannelKind, DeclaredKind, Header};
use crate::model::{Break, ChannelData, Edges, Gap, Ratio, Recording, Sample, Samples};
use crate::overview::{self, Column, Ladder, Level, Pair, Settled};
use crate::reader::{Ending, Event, Reader};

/// Whether two samples are the same stored number: floats by their bits.
fn same(a: Sample, b: Sample) -> bool {
    match (a, b) {
        (Sample::F32(x), Sample::F32(y)) => x.to_bits() == y.to_bits(),
        (Sample::F64(x), Sample::F64(y)) => x.to_bits() == y.to_bits(),
        (Sample::U64(x), Sample::U64(y)) => x == y,
        (Sample::I64(x), Sample::I64(y)) => x == y,
        _ => false,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("{0}")]
    Mismatch(String),
    #[error("reading: {0}")]
    Io(#[from] std::io::Error),
}

fn mismatch(what: impl Into<String>) -> VerifyError {
    VerifyError::Mismatch(what.into())
}

enum Want {
    Digital {
        edges: Edges,
        initial: bool,
        end: i64,
        gaps: Vec<Gap>,
        next_gap: usize,
        count: u64,
        hasher: Option<ChannelHasher>,
    },
    Analog {
        samples: Box<dyn Iterator<Item = Sample> + Send>,
        unit: String,
        first: i64,
        period: Ratio,
        phase: Option<Ratio>,
        /// The coding the samples' type has: f32 samples may also be stored as codes.
        natural: AnalogCoding,
        significant: Option<u8>,
        count: u64,
        breaks: Vec<Break>,
        lost_before: u64,
        lost_after: u64,
        hasher: Option<ChannelHasher>,
    },
}

/// Checks that `file` holds exactly what `source` holds, and returns its header.
pub fn verify(file: impl Read, source: Recording) -> Result<Header, VerifyError> {
    verify_limited(file, source, MAX_DECODED)
}

/// The overview the content gives: each overview channel's ladder, the derived channels'
/// pairing, and, by channel and shift, the overview block records the content settles and the
/// file has not shown yet.
struct Overview {
    ladders: Vec<Option<Ladder>>,
    columns: Vec<Column>,
    pairs: Vec<Pair>,
    operand_of: Vec<Vec<usize>>,
    expected: HashMap<(usize, u8), VecDeque<Settled>>,
    /// The bytes `expected` holds.
    held: usize,
    /// What the content asked of the ladders and they have not done: done as the file's overview
    /// blocks need it, so a long span's blocks are made a few at a time.
    asked: VecDeque<Ask>,
    /// Every ladder's end has been asked: no item comes after.
    ending: bool,
    /// Each ladder's strip, once it ended.
    strips: Option<Vec<Level>>,
}

/// What the content asks of the ladders, in the order it asks.
enum Ask {
    /// Channel `0`'s ladder settled to the tick.
    Settle(usize, i64),
    /// Every ladder ended at the length.
    End(i64),
}

impl Overview {
    fn settle(&mut self, o: usize, tick: i64) {
        self.asked.push_back(Ask::Settle(o, tick));
    }

    fn end(&mut self, length: i64) {
        self.ending = true;
        self.asked.push_back(Ask::End(length));
    }

    fn push(&mut self, o: usize, s: Settled) {
        self.held += s.raw.len();
        self.expected.entry((o, s.shift)).or_default().push_back(s);
    }

    /// Does the first thing asked, or a step of it: whether anything was asked.
    fn step(&mut self) -> bool {
        match self.asked.front() {
            None => false,
            Some(&Ask::Settle(o, tick)) => {
                let reached = match self.ladders[o].as_mut() {
                    Some(l) => {
                        let reached = l.settle_some(tick, SETTLE_STEP);
                        for s in l.take_settled() {
                            self.push(o, s);
                        }
                        reached
                    }
                    None => true,
                };
                if reached {
                    self.asked.pop_front();
                }
                true
            }
            Some(&Ask::End(length)) => {
                let mut strips = Vec::with_capacity(self.ladders.len());
                for o in 0..self.ladders.len() {
                    let end = self.ladders[o]
                        .take()
                        .expect("a ladder until the length")
                        .finish(length);
                    for s in end.settled {
                        self.push(o, s);
                    }
                    strips.push(end.strip);
                }
                self.strips = Some(strips);
                self.asked.pop_front();
                true
            }
        }
    }

    /// The next overview block the content gives channel `o`'s level 2^`shift`, doing what was
    /// asked until it comes; refused once the blocks of other levels it makes on the way hold
    /// more than `HOLD`.
    fn next(&mut self, o: usize, shift: u8) -> Result<Option<Settled>, VerifyError> {
        loop {
            if let Some(s) = self
                .expected
                .get_mut(&(o, shift))
                .and_then(VecDeque::pop_front)
            {
                self.held -= s.raw.len();
                return Ok(Some(s));
            }
            if self.held > HOLD {
                return Err(mismatch(format!(
                    "the overview blocks come in an order that holds back {} bytes of other \
                     levels' before channel {o}'s level 2^{shift}, more than {HOLD}",
                    self.held
                )));
            }
            if !self.step() {
                return Ok(None);
            }
        }
    }
}

/// The overview blocks a ladder settles at a step, as a writer does.
const SETTLE_STEP: usize = 16;

/// `verify`, holding at most `max` decoded bytes a frame.
pub fn verify_limited(
    mut file: impl Read,
    source: Recording,
    max: usize,
) -> Result<Header, VerifyError> {
    let Recording {
        timebase,
        length,
        channels,
        derived,
        sources,
    } = source;
    let mut names = Vec::with_capacity(channels.len());
    let mut shapings = Vec::with_capacity(channels.len());
    let mut source_of = Vec::with_capacity(channels.len());
    let mut wants: Vec<Want> = Vec::with_capacity(channels.len());
    for channel in channels {
        names.push((channel.name.clone(), channel.source_name.clone()));
        source_of.push(channel.source);
        wants.push(match channel.data {
            ChannelData::Digital(d) => Want::Digital {
                hasher: None,
                edges: d.edges,
                initial: d.initial,
                end: d.end,
                gaps: d.gaps,
                next_gap: 0,
                count: 0,
            },
            ChannelData::Analog(a) => Want::Analog {
                hasher: None,
                natural: match &a.samples {
                    Samples::F32(_) => AnalogCoding::F32,
                    Samples::F64(_) => AnalogCoding::F64,
                    Samples::U64(int, _) | Samples::I64(int, _) => AnalogCoding::Int(*int),
                },
                samples: a.samples.into_samples(),
                unit: a.unit,
                first: a.first,
                period: a.period,
                phase: a.phase,
                significant: a.significant,
                count: a.count,
                breaks: a.breaks,
                lost_before: a.lost_before,
                lost_after: a.lost_after,
            },
        });
        shapings.push(channel.shaping);
    }
    let mut reader = Reader::new(max);
    let mut ov: Option<Overview> = None;
    let mut grids: Vec<Option<crate::model::Grid>> = Vec::new();
    let mut ended = vec![false; wants.len()];
    let mut header = None;
    let mut chunk = vec![0; 1 << 20];
    let mut eof = false;
    loop {
        let event = match reader.next_event()? {
            Some(event) => event,
            None if eof => break,
            None => {
                let n = file.read(&mut chunk)?;
                if n == 0 {
                    eof = true;
                } else {
                    reader.feed(&chunk[..n]);
                }
                continue;
            }
        };
        match event {
            Event::Declaration(d) => {
                if d.timebase != timebase {
                    return Err(mismatch("the timebase differs"));
                }
                if d.channels.len() != wants.len() {
                    return Err(mismatch("the channel count differs"));
                }
                if d.derived != derived {
                    return Err(mismatch("the derived channels differ"));
                }
                if d.sources.len() != sources.len() {
                    return Err(mismatch("the number of sources differs"));
                }
                for (i, (file, source)) in d.sources.iter().zip(&sources).enumerate() {
                    let mapping = |format, timebase, origin, align, stretch| {
                        (format, timebase, origin, align, stretch)
                    };
                    if mapping(
                        &file.format,
                        file.timebase,
                        file.origin,
                        file.align,
                        file.stretch,
                    ) != mapping(
                        &source.format,
                        source.timebase,
                        source.origin,
                        source.align,
                        source.stretch,
                    ) {
                        return Err(mismatch(format!(
                            "source {i}: the format, timebase, origin, align or stretch differs"
                        )));
                    }
                }
                for (c, ((((declared, want), (name, source_name)), shaping), source)) in d
                    .channels
                    .iter()
                    .zip(&mut wants)
                    .zip(&names)
                    .zip(&shapings)
                    .zip(&source_of)
                    .enumerate()
                {
                    if declared.name != *name || declared.source_name != *source_name {
                        return Err(mismatch(format!("channel {name}: the names differ")));
                    }
                    if declared.source != *source {
                        return Err(mismatch(format!("channel {name}: the source differs")));
                    }
                    if declared.shaping != *shaping {
                        return Err(mismatch(format!(
                            "channel {name}: the offset or the kept span differs"
                        )));
                    }
                    let fits = match (&declared.data, &mut *want) {
                        (
                            DeclaredKind::Digital { initial },
                            Want::Digital {
                                initial: i, hasher, ..
                            },
                        ) if initial == i => {
                            *hasher = ChannelHasher::declared(&d, c);
                            hasher.is_some()
                        }
                        (
                            DeclaredKind::Analog {
                                unit,
                                period,
                                coding,
                                significant,
                                phase,
                            },
                            Want::Analog {
                                unit: u,
                                period: p,
                                phase: f,
                                natural,
                                significant: s,
                                hasher,
                                ..
                            },
                        ) if unit == u
                            && period == p
                            && phase == f
                            && significant == s
                            && (coding == natural
                                || matches!(
                                    (coding, &*natural),
                                    (AnalogCoding::Codes { .. }, AnalogCoding::F32)
                                )) =>
                        {
                            *hasher = ChannelHasher::declared(&d, c);
                            hasher.is_some()
                        }
                        _ => false,
                    };
                    if !fits {
                        return Err(mismatch(format!(
                            "channel {name}: the kind, initial level, sampling or coding differs"
                        )));
                    }
                }
                let columns = overview::columns(&d.channels, &d.derived);
                let ladders = columns
                    .iter()
                    .enumerate()
                    .map(|(o, &column)| Some(overview::ladder_of(&d, o, column)))
                    .collect();
                grids = d.channels.iter().map(|c| c.grid()).collect();
                let products = overview::products(&d.channels, &d.derived);
                let mut operand_of = vec![Vec::new(); d.channels.len()];
                for (k, &(a, b, _)) in products.iter().enumerate() {
                    operand_of[a].push(k);
                    if b != a {
                        operand_of[b].push(k);
                    }
                }
                ov = Some(Overview {
                    ladders,
                    columns,
                    pairs: Pair::of(&d.channels, &d.derived),
                    operand_of,
                    expected: HashMap::new(),
                    held: 0,
                    asked: VecDeque::new(),
                    ending: false,
                    strips: None,
                });
            }
            Event::Gap { channel: c, gap } => {
                let Want::Digital { gaps, next_gap, .. } = &mut wants[c] else {
                    unreachable!("the reader checks a gap's channel")
                };
                if gaps.get(*next_gap) != Some(&gap) {
                    return Err(mismatch(format!("channel {}: a gap differs", names[c].0)));
                }
                *next_gap += 1;
                let to = reader
                    .mapping(c)
                    .and_then(|map| map.axis(gap.to))
                    .ok_or_else(|| {
                        mismatch(format!("channel {}: a gap has no place", names[c].0))
                    })?;
                let Some(l) = ov
                    .as_mut()
                    .filter(|o| !o.ending)
                    .and_then(|o| o.ladders[c].as_mut())
                else {
                    return Err(mismatch(format!(
                        "channel {}: a gap after the block that ends the recording",
                        names[c].0
                    )));
                };
                l.gap(gap, to);
            }
            Event::Run { channel: c, run } => {
                let Want::Analog {
                    first,
                    breaks,
                    lost_before,
                    ..
                } = &wants[c]
                else {
                    unreachable!("the reader checks a run's channel")
                };
                let fits = if run.index == 0 {
                    let declared = grids[c].map_or(0, |g| g.phase());
                    run.tick == *first && run.lost == *lost_before && run.phase == declared
                } else {
                    breaks.contains(&run)
                };
                if !fits {
                    return Err(mismatch(format!("channel {}: a run differs", names[c].0)));
                }
            }
            Event::KnownThrough { tick } => {
                let ov = ov.as_mut().expect("records follow the declaration");
                for o in 0..ov.ladders.len() {
                    ov.settle(o, tick);
                }
            }
            Event::Block(block) => {
                let c = block.head.channel as usize;
                let ov = ov.as_mut().expect("records follow the declaration");
                if ended[c] || block.head.end > length {
                    return Err(mismatch(format!(
                        "channel {}: a block ends at tick {}, past the source's length, {length}",
                        names[c].0, block.head.end
                    )));
                }
                let name = &names[c].0;
                let got = &block.data;
                let source = got.source_ticks().unwrap_or_default();
                match (got, &mut wants[c]) {
                    (
                        BlockChannel::Digital { edges, .. },
                        Want::Digital {
                            edges: want,
                            hasher,
                            count,
                            ..
                        },
                    ) => {
                        let hasher = hasher.as_mut().expect("the declaration came first");
                        let l = ov.ladders[c].as_mut().expect("a ladder until the length");
                        for (&k, &tick) in source.iter().zip(edges) {
                            if want.next() != Some(k) {
                                return Err(mismatch(format!(
                                    "channel {name}: edge at source tick {k} differs"
                                )));
                            }
                            hasher.edge(k);
                            l.edge(k, tick);
                            *count += 1;
                        }
                    }
                    (
                        BlockChannel::Analog {
                            values,
                            first_index,
                        },
                        Want::Analog {
                            samples,
                            first,
                            breaks,
                            hasher,
                            ..
                        },
                    ) => {
                        let hasher = hasher.as_mut().expect("the declaration came first");
                        let map = reader.mapping(c);
                        let grid = grids[c].expect("an analog channel's grid");
                        let stored = wants_len(&ov.columns, &ov.pairs);
                        for (index, value) in (*first_index..).zip(values.iter()) {
                            if !samples.next().is_some_and(|s| same(s, value)) {
                                return Err(mismatch(format!(
                                    "channel {name}: sample {index} differs"
                                )));
                            }
                            hasher.sample(value);
                            let tick = map
                                .and_then(|map| {
                                    map.axis_place(
                                        grid.sample_place(*first, breaks, index),
                                        grid.d(),
                                    )
                                })
                                .ok_or_else(|| {
                                    mismatch(format!("channel {name}: sample {index} has no place"))
                                })?;
                            ov.ladders[c]
                                .as_mut()
                                .expect("a ladder until the length")
                                .sample(tick, value);
                            for i in 0..ov.operand_of[c].len() {
                                let d = ov.operand_of[c][i];
                                if let Some(p) = ov.pairs[d].take(c, value) {
                                    ov.ladders[stored + d]
                                        .as_mut()
                                        .expect("a ladder until the length")
                                        .sample(tick, p);
                                }
                            }
                        }
                    }
                    _ => unreachable!("blocks follow the declaration"),
                }
                let known = reader.known_through().unwrap_or(0);
                ov.settle(c, block.head.end.max(known));
                let stored = wants_len(&ov.columns, &ov.pairs);
                for i in 0..ov.operand_of[c].len() {
                    let d = ov.operand_of[c][i];
                    let (a, b) = (ov.pairs[d].operands[0].0, ov.pairs[d].operands[1].0);
                    let to = reader
                        .next_start(a)
                        .unwrap_or(0)
                        .min(reader.next_start(b).unwrap_or(0))
                        .max(known);
                    ov.settle(stored + d, to);
                }
                if block.head.end == length {
                    ended[c] = true;
                    if ended.iter().all(|&e| e) {
                        if let Some(e) = ov.pairs.iter().find_map(Pair::unpaired) {
                            return Err(mismatch(e.to_string()));
                        }
                        ov.end(length);
                    }
                }
            }
            Event::UnreadBlock(_) => {
                return Err(mismatch("a block of a channel this version can't read"));
            }
            Event::OverviewBlock { head, frame, .. } => {
                let ov = ov.as_mut().expect("records follow the declaration");
                let o = head.channel as usize;
                let (k, shift) = (head.k, head.shift);
                let want = ov
                    .next(o, shift)?
                    .filter(|s| s.k == k && s.n == head.n)
                    .ok_or_else(|| {
                        mismatch(format!(
                            "channel {o}: overview block {k} of level 2^{shift} is out of place: \
                             the content settles no such block there"
                        ))
                    })?;
                let raw = inflate_limited(&frame, want.raw.len(), max)?;
                if raw != *want.raw {
                    return Err(mismatch(format!(
                        "channel {o}: overview level 2^{shift}, block {k}: differs from the content"
                    )));
                }
            }
            Event::Annotation(_)
            | Event::Skipped { .. }
            | Event::Page { .. }
            | Event::Bundle(_) => {}
            Event::Footer(footer) => header = Some(*footer),
        }
    }
    if let Ending::CutOff { at } = reader.end()? {
        return Err(VerifyError::Decode(DecodeError::Corrupt(format!(
            "the file stops being whole at byte {at}: the record there is cut off or does not check"
        ))));
    }
    let footer = header.ok_or_else(|| mismatch("the file has no footer"))?;
    let h = &footer.header;
    if h.length != length {
        return Err(mismatch("the length differs"));
    }
    let mut ov = ov.expect("a footer follows the declaration");
    // What is still asked: any block it gives is one the file lacks.
    while ov.held == 0 && ov.step() {}
    if ov.held > 0 {
        return Err(mismatch(
            "the file lacks overview blocks the content settles",
        ));
    }
    let strips = ov
        .strips
        .take()
        .ok_or_else(|| mismatch("the blocks don't reach the length"))?;
    for (o, strip) in strips.iter().enumerate() {
        let got = footer.strip(o)?;
        if got.to_bytes(ov.columns[o]) != strip.to_bytes(ov.columns[o]) {
            return Err(mismatch(format!(
                "channel {o}: the footer's strip differs from the content"
            )));
        }
    }
    let mut digests = Vec::with_capacity(wants.len());
    for ((want, c), (name, _)) in wants.into_iter().zip(&h.channels).zip(&names) {
        let (left, digest) = match (want, &c.data) {
            (
                Want::Digital {
                    mut edges,
                    end,
                    gaps,
                    next_gap,
                    count,
                    hasher,
                    ..
                },
                ChannelKind::Digital {
                    end: e,
                    edges: n,
                    gaps: g,
                    gap_count,
                    ..
                },
            ) => {
                let listed = match gap_count {
                    Some(k) => *k == gaps.len() as u64,
                    None => gaps == *g,
                };
                if end != *e || count != *n || !listed || next_gap != gaps.len() {
                    return Err(mismatch(format!(
                        "channel {name}: the end, count or gaps differ"
                    )));
                }
                (
                    edges.next().is_some(),
                    hasher
                        .expect("the declaration came first")
                        .finish_digital(end, hash::gaps_listed(&gaps)),
                )
            }
            (
                Want::Analog {
                    mut samples,
                    first,
                    count,
                    breaks,
                    lost_before,
                    lost_after,
                    hasher,
                    ..
                },
                ChannelKind::Analog {
                    first: f,
                    count: n,
                    breaks: b,
                    break_count,
                    lost_before: lb,
                    lost_after: la,
                    ..
                },
            ) => {
                let first = if count == 0 { 0 } else { first };
                let listed = match break_count {
                    Some(k) => *k == breaks.len() as u64,
                    None => breaks == *b,
                };
                if first != *f || count != *n || !listed || (lost_before, lost_after) != (*lb, *la)
                {
                    return Err(mismatch(format!(
                        "channel {name}: the first tick, count, breaks or losses differ"
                    )));
                }
                (
                    samples.next().is_some(),
                    hasher.expect("the declaration came first").finish_analog(
                        first,
                        count,
                        hash::breaks_listed(&breaks),
                        (lost_before, lost_after),
                    ),
                )
            }
            _ => return Err(mismatch(format!("channel {name}: the kind differs"))),
        };
        if left {
            return Err(mismatch(format!(
                "channel {name}: the source holds more than the file"
            )));
        }
        digests.push(digest);
    }
    digests.extend(derived.iter().map(hash::derived_digest));
    if hash::content_id(timebase, length, &digests) != h.content_id {
        return Err(mismatch("the content id does not match the content"));
    }
    Ok(footer.header)
}

/// The stored channels, from every overview channel's column less the derived ones.
fn wants_len(columns: &[Column], pairs: &[Pair]) -> usize {
    columns.len() - pairs.len()
}
