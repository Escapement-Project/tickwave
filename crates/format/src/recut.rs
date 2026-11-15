//! Re-cutting (FORMAT.md, "Re-cutting"): a file read in order and written again with blocks of
//! the usual size, as a recorder that flushed often needs, and each channel's overview at the
//! shift given or declared. Sans-io, like the writer: the input's bytes go in through `feed`, the
//! output's come out through `output` and `consume`.

use std::collections::{BTreeMap, VecDeque};

use crate::decode::{BlockChannel, DecodeError, Footer, parse_bundle};
use crate::encode::spacing;
use crate::header::{ChannelKind, ContentId, Declaration, Header};
use crate::mapping;
use crate::model::{Break, Gap};
use crate::reader::{Ending, Event, ReadBlock, Reader};
use crate::repair::{HOLD, Hold, latest_start, repaired_length};
use crate::writer::{Bundle, Finish, Finished, WriteError, WriteOptions, Writer};

#[derive(Debug, thiserror::Error)]
pub enum RecutError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error("the input holds no whole declaration")]
    NoDeclaration,
    #[error("the input is cut off and holds neither a whole block nor an annotation record")]
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
}

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
        })
        .collect();
    (0..header.channels.len())
        .map(|c| mapping::finest_shift(spacing(&declaration, &maps, &items, c)))
        .collect()
}

/// Reads a file in order, its bytes fed as they come, and writes the same declaration, items,
/// gaps, runs and annotations through a `Writer` that cuts blocks to its own options. It holds
/// the input's blocks after its latest known-through record, which a cut-off input drops, and
/// what one derived channel's operand holds ahead of the other, besides what the writer holds.
pub struct Recutter {
    reader: Reader,
    options: WriteOptions,
    shifts: Option<Vec<u8>>,
    writer: Option<Writer>,
    hold: Option<Hold>,
    /// Each digital channel's gaps not yet handed on: a gap goes in once the channel's edges
    /// before its `from` are in.
    gaps: Vec<VecDeque<Gap>>,
    /// Each analog channel's runs not yet handed on: a run goes in before its sample.
    runs: Vec<VecDeque<Break>>,
    /// Each channel's end in the input: every item of it before this tick is handed on.
    known: Vec<i64>,
    /// Each channel's partners, which are promised together so their blocks close together.
    partners: Vec<Vec<usize>>,
    footer: Option<Box<Footer>>,
    bundled: Option<BTreeMap<String, String>>,
    annotations: bool,
    blocks: bool,
    known_through: Option<i64>,
    latest_annotation: Option<i64>,
    left_out: Vec<String>,
    bundle: Option<Bundle>,
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
            annotations: false,
            blocks: false,
            known_through: None,
            latest_annotation: None,
            left_out: Vec::new(),
            bundle: None,
        }
    }

    /// Gives each channel this finest overview shift in the output, by channel, rather than the
    /// input's (`shifts_for` works them out from a finished input's header). Set before the
    /// input's declaration is fed.
    pub fn shifts(&mut self, shifts: Vec<u8>) {
        self.shifts = Some(shifts);
    }

    /// The annotation records read so far that the writer refused, each as its generator and
    /// the rule its line breaks, as `Repairer::left_out` gives them. The recut leaves them out.
    pub fn left_out(&self) -> &[String] {
        &self.left_out
    }

    /// Bundles a cut-off input's annotations into the output, as the writer would have, as
    /// `Repairer::bundle` does. A finished input keeps the bundle it holds.
    pub fn bundle(&mut self, bundle: Bundle) {
        self.bundle = Some(bundle);
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
        self.reader.feed(bytes);
        while let Some(event) = self.reader.next_event()? {
            let at = self.reader.record_at();
            if let Event::Declaration(d) = event {
                self.declare(*d)?;
                continue;
            }
            // Annotations keep no order with the items, and every one stands wherever a cut
            // falls: they go to the writer as they come.
            if let Event::Annotation(_) = event {
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
        self.hold = Some(Hold::new(&d));
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
        self.writer = Some(Writer::new(d, self.options.clone())?);
        Ok(())
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
            Event::KnownThrough { tick } => {
                self.known_through = Some(tick);
                let w = self.writer();
                if tick > w.promised() {
                    w.promise(tick)?;
                }
            }
            Event::Annotation(a) => {
                let latest = latest_start(self.latest_annotation, &a);
                match self.writer().annotation(&a.source, a.annotation) {
                    Ok(()) => {
                        self.annotations = true;
                        self.latest_annotation = latest;
                    }
                    Err(WriteError::Annotation(why)) => {
                        self.left_out
                            .push(format!("generator {:?}: {why}", a.source));
                    }
                    Err(e) => return Err(e.into()),
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
                for &k in got.source_ticks().unwrap_or_default() {
                    while let Some(g) = self.gaps[c].front().filter(|g| g.from < k) {
                        writer.gap(c, *g)?;
                        self.gaps[c].pop_front();
                    }
                    writer.edge(c, k)?;
                }
                // The edges before the block's end are in, so are those before its gaps' starts.
                let map = writer.declaration().mapping(c);
                while let Some(g) = self.gaps[c].front().filter(|g| {
                    map.and_then(|m| m.axis(g.from))
                        .is_some_and(|t| t < block.head.end)
                }) {
                    writer.gap(c, *g)?;
                    self.gaps[c].pop_front();
                }
            }
            BlockChannel::Analog {
                first_index,
                values,
            } => {
                for (index, value) in (*first_index..).zip(values.iter()) {
                    while let Some(run) = self.runs[c].front().filter(|r| r.index <= index) {
                        if run.index == index {
                            writer.run(c, run.tick, run.lost)?;
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
        let writer = self
            .writer
            .as_mut()
            .expect("records follow the declaration");
        for g in group {
            let promise = if g == c && self.partners[c].is_empty() {
                self.known[c]
            } else {
                to
            };
            writer.promise_channel(g, promise.max(0))?;
        }
        Ok(())
    }

    /// No more bytes come. A finished input finishes with its footer's length, ends, losses,
    /// extras and bundled annotations, and must give its footer's content id. A cut-off one
    /// finishes as repair would finish it.
    pub fn finish(mut self) -> Result<Finished, RecutError> {
        let ending = self.reader.end()?;
        let mut writer = self.writer.take().ok_or(RecutError::NoDeclaration)?;
        for (c, gaps) in self.gaps.iter_mut().enumerate() {
            for g in gaps.drain(..) {
                writer.gap(c, g)?;
            }
        }
        match (ending, self.footer) {
            (Ending::Complete, Some(footer)) => {
                let h = &footer.header;
                let declared = writer.declaration();
                let ends: Vec<i64> = h
                    .channels
                    .iter()
                    .filter_map(|c| match c.data {
                        ChannelKind::Digital { end, .. } => Some(end),
                        ChannelKind::Analog { .. } => None,
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
                let finish = Finish {
                    extra,
                    bundle: self.bundled.map(Bundle::Files),
                    // A repaired input leaves out a gap that reaches its channel's end inside
                    // the length; the content id checks the ends that come of it.
                    drop_gaps_past_end: true,
                    lost_after,
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
                // The records held back after the cut go.
                drop(self.hold.take());
                if !self.blocks && !self.annotations {
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
                // Each digital channel's end as repair has it.
                let ends: Vec<i64> = (0..d.channels.len())
                    .filter(|&c| d.channels[c].coding().is_none())
                    .map(|c| {
                        let map = d.mapping(c).unwrap_or(mapping::Mapping::IDENTITY);
                        let start = d.start(c).unwrap_or(0);
                        match self.known_through {
                            Some(k) => map.first_at(k).max(start),
                            None if self.known[c] == 0 => start,
                            None => map.first_at(self.known[c]).max(start),
                        }
                    })
                    .collect();
                let finish = Finish {
                    bundle: self.bundle,
                    drop_gaps_past_end: true,
                    ..Finish::default()
                };
                Ok(writer.finish(length, &ends, finish)?)
            }
        }
    }
}
