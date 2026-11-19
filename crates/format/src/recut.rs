//! Re-cutting (FORMAT.md, "Re-cutting"): a file read in order and written again with blocks of
//! the usual size, as a recorder that flushed often needs, and each channel's overview at the
//! shift given or declared. Sans-io, like the writer: the input's bytes go in through `feed`, the
//! output's come out through `output` and `consume`.

use std::collections::{BTreeMap, VecDeque};

use crate::annotations::{Carried, Lines, carried_notes};
use crate::decode::{BlockChannel, DecodeError, Footer, parse_bundle};
use crate::encode::spacing;
use crate::header::{ChannelKind, ContentId, Declaration, Header};
use crate::mapping;
use crate::model::{Break, Gap};
use crate::reader::{Ending, Event, ReadBlock, Reader};
use crate::record::Rewrite;
use crate::repair::{HOLD, Hold, Tail, latest_start, repaired_length};
use crate::writer::{Bundle, Finish, Finished, WriteError, WriteOptions, Writer};

#[derive(Debug, thiserror::Error)]
pub enum RecutError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error("the input holds no whole declaration")]
    NoDeclaration,
    #[error(
        "the input is cut off and holds no whole block, no known-through record and no \
         annotation record"
    )]
    Nothing,
    #[error(
        "the input is cut off, and channel {0} states the span it keeps, so it isn't repaired: \
         make it again from its sources"
    )]
    Shaped(String),
    #[error("{0} overview shifts given, for {1} channels")]
    Shifts(usize, usize),
    #[error(
        "the input holds {0} bytes of decoded records past its latest known-through record, or \
         since its derived channels' operands last held as many samples: more than a re-cut \
         holds back, {HOLD}, before it can tell whether they stand"
    )]
    HeldBack(usize),
    #[error("the input's items give content id {got}, but its footer says {stated}")]
    ContentId { stated: ContentId, got: ContentId },
    #[error(
        "the input is finished, its footer and trailer whole, but its records stop at byte {at}: \
         it is damaged, not cut off"
    )]
    Damaged { at: u64 },
    #[error(transparent)]
    Rewrite(#[from] Rewrite),
}

/// What a re-cut does, for a refusal's words.
const DOING: &str = "re-cutting";

/// Each channel's finest overview shift by the rule tickwave's writers follow (FORMAT.md,
/// "Overview"): about 64 of its items in a bucket, from what a finished recording's header says
/// of it.
pub fn shifts_for(header: &Header) -> Vec<u8> {
    let declaration = header.declaration();
    let maps: Vec<_> = (0..header.channels.len())
        .map(|c| header.mapping(c).unwrap_or(mapping::Mapping::IDENTITY))
        .collect();
    let items: Vec<(u64, i64)> = header
        .channels
        .iter()
        .map(|c| match c.data {
            ChannelKind::Digital { edges, end, .. } => (edges, end),
            ChannelKind::Analog { count, .. } => (count, 0),
            ChannelKind::Other { .. } => (0, 0),
        })
        .collect();
    (0..header.channels.len())
        .map(|c| mapping::finest_shift(spacing(&declaration, &maps, &items, c)))
        .collect()
}

/// Reads a file in order, its bytes fed as they come, and writes the same declaration, items,
/// gaps, runs and annotations through a `Writer` that cuts blocks to its own options. It holds
/// the input's blocks after its latest known-through record, which a cut-off input drops, what
/// one derived channel's operand holds ahead of the other, and a channel's gaps over two of its
/// input blocks at most, besides what the writer holds. Each annotation line goes to the writer
/// as it is read, for parts that stand alone, and the input's bundle is carried as it is.
pub struct Recutter {
    reader: Reader,
    options: WriteOptions,
    shifts: Option<Vec<u8>>,
    writer: Option<Writer>,
    hold: Option<Hold>,
    /// Each digital channel's gaps not yet handed on. A gap the header may leave out, as it
    /// reaches the channel's end, is handed on once the input shows it can't: an edge after it,
    /// or a block of the channel after the one that ends past its `to`, since the input's writer
    /// hashed the gap when that one closed. Otherwise at the finish, which leaves out those that
    /// reach the end as the header does. Until then the channel isn't promised past its `from`,
    /// so the block that holds the gap's `to` stays open and the gap is never hashed before the
    /// finish knows the end.
    gaps: Vec<VecDeque<Gap>>,
    /// Each analog channel's runs not yet handed on: a run goes in before its sample.
    runs: Vec<VecDeque<Break>>,
    /// Each channel's end in the input: every item of it before this tick is handed on.
    known: Vec<i64>,
    /// Each channel's partners, which are promised together so their blocks close together.
    partners: Vec<Vec<usize>>,
    footer: Option<Box<Footer>>,
    bundled: Option<BTreeMap<String, String>>,
    checks: Option<Lines>,
    /// Whether any annotation line went to the writer, and the latest start among them.
    annotations: bool,
    latest_annotation: Option<i64>,
    /// A sidecar to write in place of the input's annotations.
    sidecar: Option<BTreeMap<String, String>>,
    blocks: bool,
    known_through: Option<i64>,
    left_out: Vec<String>,
    /// What the lines carried held that this version doesn't know, by generator.
    carried: BTreeMap<String, Carried>,
    bundle: Option<Bundle>,
    /// Whether the input ends as a finished one does.
    tail: Tail,
}

impl Recutter {
    pub fn new(options: WriteOptions) -> Self {
        Self {
            reader: Reader::default(),
            options,
            shifts: None,
            writer: None,
            hold: None,
            gaps: Vec::new(),
            runs: Vec::new(),
            known: Vec::new(),
            partners: Vec::new(),
            footer: None,
            bundled: None,
            checks: None,
            annotations: false,
            latest_annotation: None,
            sidecar: None,
            blocks: false,
            known_through: None,
            left_out: Vec::new(),
            carried: BTreeMap::new(),
            bundle: None,
            tail: Tail::default(),
        }
    }

    /// Gives each channel this finest overview shift in the output, by channel, rather than the
    /// input's (`shifts_for` works them out from a finished input's header). Set before the
    /// input's declaration is fed.
    pub fn shifts(&mut self, shifts: Vec<u8>) {
        self.shifts = Some(shifts);
    }

    /// The annotation lines read so far, in records or parts, that break a rule ANNOTATIONS.md
    /// gives a generator's line, each as its generator and the rule, as `Repairer::left_out`
    /// gives them. The recut leaves them out.
    pub fn left_out(&self) -> &[String] {
        &self.left_out
    }

    /// For each generator whose lines held a key or a `measure` this version doesn't know, which
    /// a later minor version may add, a note naming what the recut carried (ANNOTATIONS.md,
    /// "Versions"): the line's generator file's first line, which says, comes only with the
    /// bundle.
    pub fn carried(&self) -> Vec<String> {
        carried_notes(&self.carried)
    }

    /// Bundles a cut-off input's annotations into the output, as the writer would have, as
    /// `Repairer::bundle` does. A finished input keeps the bundle it holds.
    pub fn bundle(&mut self, bundle: Bundle) {
        self.bundle = Some(bundle);
    }

    /// Writes `files`, a sidecar's whole, as the copy's annotations, as `tickwave bundle` does,
    /// in place of the input's annotation records, parts and bundle, which are left out. Set
    /// before the input is fed.
    pub fn sidecar(&mut self, files: BTreeMap<String, String>) {
        self.sidecar = Some(files);
    }

    /// The output's bytes written and not yet taken.
    pub fn output(&self) -> &[u8] {
        self.writer.as_ref().map_or(&[], Writer::output)
    }

    /// Takes the first `n` bytes of `output`.
    pub fn consume(&mut self, n: usize) {
        if let Some(w) = &mut self.writer {
            w.consume(n);
        }
    }

    /// Takes every output byte written so far.
    pub fn take_output(&mut self) -> Vec<u8> {
        self.writer
            .as_mut()
            .map_or_else(Vec::new, Writer::take_output)
    }

    /// The input's next bytes, from its start.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), RecutError> {
        self.tail.feed(bytes);
        self.reader.feed(bytes);
        while let Some(event) = self.reader.next_event()? {
            let at = self.reader.record_at();
            if let Event::Declaration(d) = event {
                let minor = self.reader.minor().expect("the preamble comes first");
                if let Some(why) = Rewrite::of_minor(minor, DOING) {
                    return Err(why.into());
                }
                self.declare(*d)?;
                continue;
            }
            // An auxiliary record goes, as the writer gives none; an optional one can't.
            if let Event::Skipped { kind } = event {
                if let Some(why) = Rewrite::of_kind(kind, DOING) {
                    return Err(why.into());
                }
                continue;
            }
            // Annotations keep no order with the items, and every one stands wherever a cut
            // falls.
            if let Event::Annotation(_) | Event::Part { .. } = event {
                self.take(event)?;
                continue;
            }
            let Some(hold) = self.hold.as_mut() else {
                continue;
            };
            for (_, event) in hold.take(at, event).map_err(RecutError::HeldBack)? {
                self.take(event)?;
            }
        }
        Ok(())
    }

    fn declare(&mut self, mut d: Declaration) -> Result<(), RecutError> {
        if let Some(shifts) = &self.shifts {
            if shifts.len() != d.channels.len() {
                return Err(RecutError::Shifts(shifts.len(), d.channels.len()));
            }
            let recording = shifts.iter().copied().min().unwrap_or(d.overview_shift);
            d.overview_shift = recording;
            for (c, &s) in d.channels.iter_mut().zip(shifts) {
                c.overview_shift = (s != recording).then_some(s);
            }
        }
        // The writer refuses a declaration this version can't write again, before anything
        // here takes its derived channels.
        let writer = Writer::new(d.clone(), self.options.clone())?;
        self.hold = Some(Hold::new(&d));
        self.checks = Some(Lines::declared(&d).carrying());
        self.gaps = vec![VecDeque::new(); d.channels.len()];
        self.runs = vec![VecDeque::new(); d.channels.len()];
        self.known = vec![0; d.channels.len()];
        let products = crate::overview::products(&d.channels, &d.derived);
        let mut partners = vec![Vec::new(); d.channels.len()];
        for &(a, b, _) in &products {
            if a != b {
                partners[a].push(b);
                partners[b].push(a);
            }
        }
        self.partners = partners;
        self.writer = Some(writer);
        Ok(())
    }

    /// The axis tick channel `c` may be promised to: the `from` of its first gap not handed on.
    fn cap(&self, c: usize) -> i64 {
        let Some(g) = self.gaps[c].front() else {
            return i64::MAX;
        };
        self.writer
            .as_ref()
            .and_then(|w| w.declaration().mapping(c))
            .and_then(|m| m.axis(g.from))
            .unwrap_or(i64::MAX)
    }

    fn writer(&mut self) -> &mut Writer {
        self.writer
            .as_mut()
            .expect("records follow the declaration")
    }

    fn take(&mut self, event: Event) -> Result<(), RecutError> {
        match event {
            Event::Gap { channel, gap } => self.gaps[channel].push_back(gap),
            Event::Run { channel, run } => self.runs[channel].push_back(run),
            Event::Block(block) => self.block(block)?,
            Event::UnreadBlock(_) | Event::UnreadOverviewBlock(_) => {
                return Err(crate::decode::corrupt(
                    "a block of a channel this version can't read, so it can't write again",
                )
                .into());
            }
            Event::KnownThrough { tick } => {
                self.known_through = Some(tick);
                for c in 0..self.known.len() {
                    let to = tick.min(self.cap(c));
                    let w = self.writer();
                    if to > w.promised_to(c) {
                        w.promise_channel(c, to)?;
                    }
                }
            }
            _ if self.sidecar.is_some()
                && matches!(
                    event,
                    Event::Annotation(_) | Event::Part { .. } | Event::Bundle(_)
                ) => {}
            Event::Annotation(a) => {
                let checked = self
                    .checks
                    .as_mut()
                    .map_or(Ok(Carried::new()), |l| l.take_line(&a.source, &a.line));
                self.carry(&a.source, &a.line, checked)?;
            }
            // A part that covers records holds their lines, which went on with the records.
            Event::Part { covers, .. } if covers > 0 => {}
            Event::Part { part, .. } => {
                let timebase = self.writer().declaration().timebase;
                for line in part.lines() {
                    let checked = self.checks.as_mut().map_or(Ok(Carried::new()), |l| {
                        l.take_file_line(&part.source, line, timebase)
                    });
                    self.carry(&part.source, line, checked)?;
                }
            }
            Event::Bundle(payload) => {
                self.bundled = Some(parse_bundle(&payload, crate::decode::inflate)?);
            }
            Event::Footer(footer) => self.footer = Some(footer),
            Event::Declaration(_)
            | Event::OverviewBlock { .. }
            | Event::Page { .. }
            | Event::Skipped { .. } => {}
        }
        Ok(())
    }

    /// Generator `source`'s `line` to the writer, for a part that stands alone, where `checked`
    /// says it keeps the rules ANNOTATIONS.md gives a line, and where the writer takes it; one
    /// that doesn't is named in `left_out`. What `checked` carried is noted.
    fn carry(
        &mut self,
        source: &str,
        line: &str,
        checked: Result<Carried, String>,
    ) -> Result<(), RecutError> {
        let taken = match checked {
            Ok(carried) => self
                .writer()
                .standalone_line(source, line)
                .map(|()| carried),
            Err(why) => Err(WriteError::Annotation(why)),
        };
        match taken {
            Ok(carried) => {
                if !carried.is_empty() {
                    self.carried
                        .entry(source.to_owned())
                        .or_default()
                        .extend(carried);
                }
                self.annotations = true;
                self.latest_annotation = latest_start(self.latest_annotation, line);
                Ok(())
            }
            Err(WriteError::Annotation(why)) => {
                self.left_out.push(format!("generator {source:?}: {why}"));
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Hands on a block's items, each in the order of its source ticks, then promises its
    /// channel, and its partners as far as they all go, to its end.
    fn block(&mut self, block: ReadBlock) -> Result<(), RecutError> {
        self.blocks = true;
        let c = block.head.channel as usize;
        let writer = self
            .writer
            .as_mut()
            .expect("records follow the declaration");
        match &block.data {
            got @ BlockChannel::Digital { .. } => {
                // The channel's block before this one ends at `passed`, and closed before the
                // input's finish: a gap that ends before it was hashed then, so the header keeps
                // it.
                let passed = self.known[c];
                let map = writer.declaration().mapping(c);
                while let Some(g) = self.gaps[c]
                    .front()
                    .filter(|g| map.and_then(|m| m.axis(g.to)).is_some_and(|t| t < passed))
                {
                    writer.gap(c, *g)?;
                    self.gaps[c].pop_front();
                }
                // An edge after a gap shows the header keeps it: the channel is known past it.
                for &k in got.source_ticks().unwrap_or_default() {
                    while let Some(g) = self.gaps[c].front().filter(|g| g.from < k) {
                        writer.gap(c, *g)?;
                        self.gaps[c].pop_front();
                    }
                    writer.edge(c, k)?;
                }
            }
            BlockChannel::Analog {
                first_index,
                values,
            } => {
                for (index, value) in (*first_index..).zip(values.iter()) {
                    while let Some(run) = self.runs[c].front().filter(|r| r.index <= index) {
                        if run.index == index {
                            writer.run_at(c, run.tick, run.phase, run.lost)?;
                        }
                        self.runs[c].pop_front();
                    }
                    writer.sample(c, value)?;
                }
            }
        }
        self.known[c] = block.head.end;
        let to = self.partners[c]
            .iter()
            .map(|&p| self.known[p])
            .fold(self.known[c], i64::min);
        let group: Vec<usize> = std::iter::once(c)
            .chain(self.partners[c].iter().copied())
            .collect();
        for g in group {
            let promise = if g == c && self.partners[c].is_empty() {
                self.known[c]
            } else {
                to
            };
            let promise = promise.max(0).min(self.cap(g));
            let writer = self.writer();
            if promise > writer.promised_to(g) {
                writer.promise_channel(g, promise)?;
            }
        }
        Ok(())
    }

    /// No more bytes come. A finished input finishes with its footer's length, ends, losses,
    /// extras and bundled annotations, and must give its footer's content id. A cut-off one
    /// finishes as repair would finish it.
    pub fn finish(mut self) -> Result<Finished, RecutError> {
        let ending = self.reader.end()?;
        // Finishing a file whose records stop before its whole footer would drop what follows.
        if let Ending::CutOff { at } = &ending
            && self.tail.finished()
        {
            return Err(RecutError::Damaged { at: *at });
        }
        let mut writer = self.writer.take().ok_or(RecutError::NoDeclaration)?;
        let sidecar = self.sidecar.take().map(Bundle::Files);
        match (ending, self.footer) {
            (Ending::Complete, Some(footer)) => {
                let h = &footer.header;
                // The header leaves out the gaps that reach their channel's end, and so does the
                // writer's finish given the same ends.
                for (c, gaps) in self.gaps.iter_mut().enumerate() {
                    for g in gaps.drain(..) {
                        writer.gap(c, g)?;
                    }
                }
                let declared = writer.declaration();
                let ends: Vec<i64> = h
                    .channels
                    .iter()
                    .filter_map(|c| match c.data {
                        ChannelKind::Digital { end, .. } => Some(end),
                        ChannelKind::Analog { .. } | ChannelKind::Other { .. } => None,
                    })
                    .collect();
                let lost_after: BTreeMap<usize, u64> = h
                    .channels
                    .iter()
                    .enumerate()
                    .filter_map(|(c, ch)| match ch.data {
                        ChannelKind::Analog { lost_after, .. } if lost_after > 0 => {
                            Some((c, lost_after))
                        }
                        _ => None,
                    })
                    .collect();
                let extra: Vec<BTreeMap<String, String>> = h
                    .sources
                    .iter()
                    .zip(&declared.sources)
                    .map(|(s, d)| {
                        s.extra
                            .iter()
                            .filter(|(k, _)| !d.extra.contains_key(*k))
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    })
                    .collect();
                // The input's bundle as it is, its generator files' first lines, or the sidecar
                // given in its place.
                let bundle = sidecar.or(self.bundled.map(Bundle::Files)).or(self.bundle);
                // The input's view, where it has one, is in its bundle's hand-written file.
                let finish = Finish {
                    extra,
                    bundle,
                    lost_after,
                    view: None,
                };
                let finished = writer.finish(h.length, &ends, finish)?;
                if finished.header.content_id != h.content_id {
                    return Err(RecutError::ContentId {
                        stated: h.content_id,
                        got: finished.header.content_id,
                    });
                }
                Ok(finished)
            }
            _ => {
                if let Some(c) = writer
                    .declaration()
                    .channels
                    .iter()
                    .find(|c| !c.shaping.is_whole())
                {
                    return Err(RecutError::Shaped(c.name.clone()));
                }
                // FORMAT.md's repair step 1. With none of the three the length below comes to 0,
                // which refuses the file too.
                if !self.blocks && self.known_through.is_none() && !self.annotations {
                    return Err(RecutError::Nothing);
                }
                let d = writer.declaration().clone();
                let last_end = self.known.iter().copied().max().unwrap_or(0);
                let length = repaired_length(
                    self.known_through.unwrap_or(last_end),
                    self.latest_annotation,
                );
                if length < 1 {
                    return Err(RecutError::Nothing);
                }
                // The records held back after the cut go, but for a gap record whose `from`
                // maps before the latest known-through tick: a loss the recorder had begun to
                // report ends its channel there, as repair has it.
                let held = self.hold.take().map(Hold::dropped).unwrap_or_default();
                if let Some(k) = self.known_through {
                    for (_, event) in held {
                        if let Event::Gap { channel, gap } = event
                            && d.mapping(channel)
                                .and_then(|m| m.axis(gap.from))
                                .is_some_and(|t| t < k)
                        {
                            self.gaps[channel].push_back(gap);
                        }
                    }
                }
                // Each digital channel is known as repair has it.
                let known = |c: usize| {
                    let map = d.mapping(c).unwrap_or(mapping::Mapping::IDENTITY);
                    let start = d.start(c).unwrap_or(0);
                    match self.known_through {
                        Some(k) => map.first_at(k).max(start),
                        None if self.known[c] == 0 => start,
                        None => map.first_at(self.known[c]).max(start),
                    }
                };
                // A gap that reaches its channel's end ends the channel at its `from`, and the
                // header leaves it out; its record goes again only where its `to` maps at or past
                // the length (FORMAT.md, "Gap and run records"), wherever it lay in the input.
                let mut cut = vec![i64::MAX; d.channels.len()];
                for c in 0..self.gaps.len() {
                    let end = known(c);
                    for g in std::mem::take(&mut self.gaps[c]) {
                        let inside = d
                            .mapping(c)
                            .and_then(|m| m.axis(g.to))
                            .is_some_and(|t| t < length);
                        if g.to >= end && inside {
                            cut[c] = cut[c].min(g.from);
                        } else {
                            writer.gap(c, g)?;
                        }
                    }
                }
                let ends: Vec<i64> = (0..d.channels.len())
                    .filter(|&c| d.channels[c].coding().is_none())
                    .map(|c| known(c).min(cut[c]))
                    .collect();
                let finish = Finish {
                    bundle: sidecar.or(self.bundle),
                    ..Finish::default()
                };
                Ok(writer.finish(length, &ends, finish)?)
            }
        }
    }
}
