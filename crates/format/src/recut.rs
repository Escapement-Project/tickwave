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
    /// The latest tick handed on.
    pushed: Option<i64>,
    footer: Option<Box<Footer>>,
    annotations: bool,
    blocks: bool,
    latest_annotation: Option<i64>,
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
            bundle: None,
        }
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

    /// Hands on every waiting gap whose `from` lies before `tick`.
    fn gaps_before(&mut self, tick: i64) -> Result<(), RecutError> {
        let writer = self
            .writer
            .as_mut()
            .expect("records follow the declaration");
        for (c, gaps) in self.gaps.iter_mut().enumerate() {
            while let Some(g) = gaps.front().filter(|g| g.from < tick) {
                writer.gap(c, *g)?;
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
                if self.pushed.is_some_and(|t| t > gap.from) && self.gaps[channel].is_empty() {
                    self.writer().gap(channel, gap)?;
                } else {
                    self.gaps[channel].push_back(gap);
                }
            }
            Event::Run { channel, run } => self.runs[channel].push_back(run),
            Event::Block(block) => self.block(block)?,
            Event::Annotation(a) => {
                self.annotations = true;
                self.latest_annotation = latest_start(self.latest_annotation, &a);
                self.writer().annotation(&a.source, a.annotation)?;
            }
            Event::Footer(footer) => self.footer = Some(footer),
            Event::OverviewBlock { .. } | Event::Skipped { .. } => {}
        }
        Ok(())
    }

    /// Hands on a block's items in tick order across the channels.
    fn block(&mut self, block: ReadBlock) -> Result<(), RecutError> {
        self.blocks = true;
        let channels = block.channels;
        let mut at = vec![0usize; channels.len()];
        let tick_of = |reader: &Reader, c: usize, i: usize| -> Option<i64> {
            match &channels[c] {
                BlockChannel::Digital { edges, .. } => edges.get(i).copied(),
                BlockChannel::Analog {
                    first_index,
                    values,
                } => (i < values.len()).then(|| {
                    reader
                        .sample_tick(c, first_index + i as u64)
                        .expect("the reader checks a block's samples have a run")
                }),
            }
        };
        loop {
            let next = (0..channels.len())
                .filter_map(|c| tick_of(&self.reader, c, at[c]).map(|t| (t, c)))
                .min();
            let Some((tick, c)) = next else {
                break;
            };
            self.gaps_before(tick)?;
            let i = at[c];
            at[c] += 1;
            let writer = self
                .writer
                .as_mut()
                .expect("records follow the declaration");
            match &channels[c] {
                BlockChannel::Digital { .. } => writer.edge(c, tick)?,
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
        self.gaps_before(block.head.end)?;
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
                let ends: Vec<i64> = writer
                    .declaration()
                    .channels
                    .iter()
                    .filter(|c| c.coding().is_none())
                    .map(|_| last_end)
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
