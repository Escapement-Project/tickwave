//! Re-cutting (FORMAT.md, "Re-cutting"): a file read in order and written again with blocks of
//! the usual size, as a recorder that flushed often needs. Sans-io, like the writer: the input's
//! bytes go in through `feed`, the output's come out through `output` and `consume`.

use std::collections::{BTreeMap, VecDeque};

use crate::decode::{BlockChannel, DecodeError, Footer};
use crate::header::{ChannelKind, ContentId};
use crate::model::{Break, Gap};
use crate::reader::{Ending, Event, ReadBlock, Reader};
use crate::repair::{high_without_block, latest_start, repaired_length};
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
    #[error(
        "the input is cut off with no block, and channel {0} is declared high at tick 0: no \
         block carries that level, so it isn't finished"
    )]
    HighWithoutBlock(String),
    #[error("the input's items give content id {got}, but its footer says {stated}")]
    ContentId { stated: ContentId, got: ContentId },
}

/// Reads a file in order, its bytes fed as they come, and writes the same declaration, items,
/// gaps, runs and annotations through a `Writer` that cuts blocks to its own options. It holds
/// one decoded block besides what the writer holds.
pub struct Recutter {
    reader: Reader,
    options: WriteOptions,
    writer: Option<Writer>,
    /// Each digital channel's gaps not yet handed on: a gap goes in once the items pass its
    /// `from`, so the edges before it are in first.
    gaps: Vec<VecDeque<Gap>>,
    /// Each analog channel's runs not yet handed on: a run goes in before its sample.
    runs: Vec<VecDeque<Break>>,
    /// The latest axis tick handed on.
    pushed: Option<i64>,
    footer: Option<Box<Footer>>,
    annotations: bool,
    blocks: bool,
    latest_annotation: Option<i64>,
    left_out: Vec<String>,
    bundle: Option<Bundle>,
}

impl Recutter {
    pub fn new(options: WriteOptions) -> Self {
        Self {
            reader: Reader::default(),
            options,
            writer: None,
            gaps: Vec::new(),
            runs: Vec::new(),
            pushed: None,
            footer: None,
            annotations: false,
            blocks: false,
            latest_annotation: None,
            left_out: Vec::new(),
            bundle: None,
        }
    }

    /// The annotation records read so far that the writer refused, each as its generator and
    /// the rule its line breaks, as `Repairer::left_out` gives them. The recut leaves them out.
    pub fn left_out(&self) -> &[String] {
        &self.left_out
    }

    /// Bundles a cut-off input's annotations into the footer, as the writer would have, as
    /// `Repairer::bundle` does. A finished input keeps the bundle its footer holds.
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
            self.take(event)?;
        }
        Ok(())
    }

    fn writer(&mut self) -> &mut Writer {
        self.writer
            .as_mut()
            .expect("records follow the declaration")
    }

    /// Hands on every waiting gap that comes before an item at axis tick `tick`: one whose
    /// `from` maps before it, and, of channel `item`'s, one whose `from` lies before the item's
    /// source tick, which can share its axis tick.
    fn gaps_before(&mut self, tick: i64, item: Option<(usize, i64)>) -> Result<(), RecutError> {
        let writer = self
            .writer
            .as_mut()
            .expect("records follow the declaration");
        for (c, gaps) in self.gaps.iter_mut().enumerate() {
            let map = self.reader.mapping(c);
            let before = |g: &Gap| {
                map.and_then(|m| m.axis(g.from)).is_some_and(|t| t < tick)
                    || item.is_some_and(|(ic, k)| ic == c && g.from < k)
            };
            while let Some(&g) = gaps.front().filter(|g| before(g)) {
                writer.gap(c, g)?;
                gaps.pop_front();
            }
        }
        Ok(())
    }

    fn take(&mut self, event: Event) -> Result<(), RecutError> {
        match event {
            Event::Declaration(d) => {
                self.gaps = vec![VecDeque::new(); d.channels.len()];
                self.runs = vec![VecDeque::new(); d.channels.len()];
                self.writer = Some(Writer::new(*d, self.options.clone())?);
            }
            Event::Gap { channel, gap } => {
                // A gap learned after items past its `from` went in: the edges before it are in.
                let from = self.reader.mapping(channel).and_then(|m| m.axis(gap.from));
                let passed = self.pushed.zip(from).is_some_and(|(t, from)| t > from);
                if passed && self.gaps[channel].is_empty() {
                    self.writer().gap(channel, gap)?;
                } else {
                    self.gaps[channel].push_back(gap);
                }
            }
            Event::Run { channel, run } => self.runs[channel].push_back(run),
            Event::Block(block) => self.block(block)?,
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
            Event::Footer(footer) => self.footer = Some(footer),
            Event::OverviewBlock { .. } | Event::Skipped { .. } => {}
        }
        Ok(())
    }

    /// Hands on a block's items in axis order across the channels, each channel's in the order
    /// of its source ticks.
    fn block(&mut self, block: ReadBlock) -> Result<(), RecutError> {
        self.blocks = true;
        let channels = block.channels;
        let mut at = vec![0usize; channels.len()];
        // The item's axis tick, and a digital edge's source tick. The reader holds every sample of
        // a block it hands on inside the block, so each has a place.
        let tick_of = |reader: &Reader, c: usize, i: usize| -> Option<(i64, i64)> {
            match &channels[c] {
                got @ BlockChannel::Digital { edges, .. } => {
                    let source = got.source_ticks()?;
                    Some((*edges.get(i)?, *source.get(i)?))
                }
                BlockChannel::Analog {
                    first_index,
                    values,
                } => {
                    let index = first_index + i as u64;
                    (i < values.len()).then(|| {
                        let place = reader.sample_tick(c, index).unwrap_or(i64::MAX);
                        let tick = reader.sample_axis_tick(c, index).unwrap_or(i64::MAX);
                        (tick, place)
                    })
                }
            }
        };
        loop {
            let next = (0..channels.len())
                .filter_map(|c| tick_of(&self.reader, c, at[c]).map(|t| (t, c)))
                .min_by_key(|&((tick, _), c)| (tick, c));
            let Some(((tick, source), c)) = next else {
                break;
            };
            self.gaps_before(tick, Some((c, source)))?;
            let i = at[c];
            at[c] += 1;
            let writer = self
                .writer
                .as_mut()
                .expect("records follow the declaration");
            match &channels[c] {
                BlockChannel::Digital { .. } => writer.edge(c, source)?,
                BlockChannel::Analog {
                    first_index,
                    values,
                } => {
                    let index = first_index + i as u64;
                    while let Some(run) = self.runs[c].front().filter(|r| r.index <= index) {
                        if run.index == index {
                            writer.run(c, run.tick, run.lost)?;
                        }
                        self.runs[c].pop_front();
                    }
                    writer.sample(c, values.get(i).expect("inside"))?;
                }
            }
            self.pushed = Some(tick);
        }
        self.gaps_before(block.head.end, None)?;
        Ok(())
    }

    /// No more bytes come. A finished input finishes with its footer's length, ends, extras and
    /// bundled annotations, and must give its footer's content id. A cut-off one finishes as
    /// repair would finish it.
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
                    bundle: h.annotations.clone().map(Bundle::Files),
                    // A repaired input leaves out a gap that reaches its channel's end inside
                    // the length; the content id checks the ends that come of it.
                    drop_gaps_past_end: true,
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
                if !self.blocks && !self.annotations {
                    return Err(RecutError::Nothing);
                }
                if !self.blocks
                    && let Some(name) = high_without_block(writer.declaration())
                {
                    return Err(RecutError::HighWithoutBlock(name.to_owned()));
                }
                let last_end = self.reader.next_start();
                let length = repaired_length(last_end, self.latest_annotation);
                if length < 1 {
                    return Err(RecutError::Nothing);
                }
                // The source tick at the last block's end, as repair has it.
                let d = writer.declaration();
                let ends: Vec<i64> = (0..d.channels.len())
                    .filter(|&c| d.channels[c].coding().is_none())
                    .map(|c| d.mapping(c).map_or(last_end, |m| m.first_at(last_end)))
                    .collect();
                // Repair's gap rule: a gap that reaches the last block's end ends what is known at
                // its `from`, even one that ends inside the length the marks gave.
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
