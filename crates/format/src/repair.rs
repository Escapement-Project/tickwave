//! Repair: a footer and a trailer for a file cut off before them, by a crash, a power cut or a
//! full disk. The records up to the first that is not whole stay as they are.

use std::collections::{BTreeMap, HashMap};

use crate::decode::{BlockChannel, DecodeError, MAX_DECODED, check_header, inflate_limited};
use crate::hash::ChannelHasher;
use crate::header::{
    BlockRow, ChannelHeader, ChannelKind, Declaration, DeclaredKind, Header, OverviewRow,
};
use crate::model::{Break, Gap};
use crate::overview::{self, Builder, Settled};
use crate::reader::{Ending, Event, Reader};
use crate::record::{self, Kind};
use crate::writer::{WriteError, clip_annotation, compress, finished_header};

#[derive(Debug, thiserror::Error)]
pub enum RepairError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("the file is finished: it has its footer")]
    Finished,
    #[error("the file holds no whole declaration")]
    NoDeclaration,
    #[error("the file holds no whole block")]
    NoBlock,
    #[error("an overview block in the file differs from the one its content gives")]
    Overview,
    /// A converter moved or cut channel `0`. The declaration fixed each kept span before the
    /// length was known, and the content id pins it, so a file cut off before them has no footer
    /// that would hold. Its source makes it again.
    #[error(
        "channel {0} was moved or trimmed when the recording was converted, so it isn't \
         repaired: convert it again from its source"
    )]
    Shaped(String),
    #[error(transparent)]
    Write(#[from] WriteError),
}

/// What repair comes to: keep the file's first `keep` bytes, then append `tail`.
pub struct Repaired {
    pub keep: u64,
    pub tail: Vec<u8>,
    pub header: Header,
    /// Each generator's annotations, its file's lines after the first, by source.
    pub generated: BTreeMap<String, Vec<String>>,
}

enum Channel {
    Digital {
        hasher: ChannelHasher,
        edges: u64,
        gaps: Vec<Gap>,
    },
    Analog {
        hasher: ChannelHasher,
        count: u64,
        runs: Vec<Break>,
    },
}

/// Reads a cut-off file in order, its bytes fed as they come, and works out its footer.
pub struct Repairer {
    reader: Reader,
    level: i32,
    declaration: Option<Declaration>,
    builder: Option<Builder>,
    channels: Vec<Channel>,
    rows: Vec<BlockRow>,
    settled: Vec<Settled>,
    /// The overview blocks the file holds: their frames' offsets and bytes.
    present: HashMap<(u8, u64), (u64, Vec<u8>)>,
    annotations: Vec<record::Annotation>,
}

impl Repairer {
    /// `level` is the zstd level for what repair writes.
    pub fn new(level: i32) -> Self {
        Self {
            reader: Reader::new(MAX_DECODED),
            level,
            declaration: None,
            builder: None,
            channels: Vec::new(),
            rows: Vec::new(),
            settled: Vec::new(),
            present: HashMap::new(),
            annotations: Vec::new(),
        }
    }

    /// The file's next bytes, from its start.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), RepairError> {
        self.reader.feed(bytes);
        while let Some(event) = self.reader.next_event()? {
            self.take(event);
        }
        Ok(())
    }

    fn take(&mut self, event: Event) {
        match event {
            Event::Declaration(d) => {
                self.builder = Some(Builder::new(d.overview_shift, &d.channels));
                self.channels = d
                    .channels
                    .iter()
                    .map(|c| match &c.data {
                        DeclaredKind::Digital { initial } => Channel::Digital {
                            hasher: ChannelHasher::digital(&c.name, *initial, &c.shaping),
                            edges: 0,
                            gaps: Vec::new(),
                        },
                        DeclaredKind::Analog { unit, period, .. } => Channel::Analog {
                            hasher: ChannelHasher::analog(&c.name, unit, *period, &c.shaping),
                            count: 0,
                            runs: Vec::new(),
                        },
                    })
                    .collect();
                self.declaration = Some(*d);
            }
            Event::Gap { channel, gap } => {
                if let Channel::Digital { gaps, .. } = &mut self.channels[channel] {
                    gaps.push(gap);
                }
                self.builder.as_mut().unwrap().gap(channel, gap);
            }
            Event::Run {
                channel,
                index,
                tick,
            } => {
                if let Channel::Analog { runs, .. } = &mut self.channels[channel] {
                    runs.push(Break { index, tick });
                }
            }
            Event::Block(block) => {
                let builder = self.builder.as_mut().unwrap();
                for (c, got) in block.channels.into_iter().enumerate() {
                    match (got, &mut self.channels[c]) {
                        (
                            BlockChannel::Digital { edges, .. },
                            Channel::Digital {
                                hasher, edges: n, ..
                            },
                        ) => {
                            for tick in edges {
                                hasher.edge(tick);
                                builder.edge(c, tick);
                                *n += 1;
                            }
                        }
                        (
                            BlockChannel::Analog {
                                values,
                                first_index,
                            },
                            Channel::Analog { hasher, count, .. },
                        ) => {
                            for (index, value) in (first_index..).zip(values) {
                                let tick = self
                                    .reader
                                    .sample_tick(c, index)
                                    .expect("the reader checks a block's samples have a run");
                                hasher.sample(value);
                                builder.sample(c, tick, value);
                                *count += 1;
                            }
                        }
                        _ => unreachable!("blocks follow the declaration"),
                    }
                }
                builder.settle(block.head.end);
                self.settled.extend(builder.take_settled());
                self.rows.push(BlockRow {
                    start: block.head.start,
                    offset: block.frame_offset,
                    length: block.frame_len,
                    decoded_length: block.head.decoded_length,
                    channels: block.head.words,
                });
            }
            Event::OverviewBlock {
                shift,
                k,
                frame_offset,
                frame,
            } => {
                self.present.insert((shift, k), (frame_offset, frame));
            }
            Event::Annotation(a) => self.annotations.push(a),
            Event::Footer(_) | Event::Skipped { .. } => {}
        }
    }

    /// No more bytes come: the footer for what was read.
    pub fn finish(mut self) -> Result<Repaired, RepairError> {
        let Ending::CutOff { at: keep } = self.reader.end()? else {
            return Err(RepairError::Finished);
        };
        let declaration = self.declaration.take().ok_or(RepairError::NoDeclaration)?;
        if let Some(c) = declaration.channels.iter().find(|c| !c.shaping.is_whole()) {
            return Err(RepairError::Shaped(c.name.clone()));
        }
        let builder = self.builder.take().ok_or(RepairError::NoDeclaration)?;
        if self.rows.is_empty() {
            return Err(RepairError::NoBlock);
        }
        let length = self.reader.next_start();
        let (done, last) = builder.finish(length);
        let mut tail = Vec::new();
        let mut overview_rows = Vec::new();
        for Settled { shift, k, level } in std::mem::take(&mut self.settled).into_iter().chain(last)
        {
            let mut raw = Vec::new();
            overview::write(&[level], &mut raw);
            let row = match self.present.get(&(shift, k)) {
                Some((offset, frame)) => {
                    let got = inflate_limited(frame, raw.len(), MAX_DECODED)?;
                    if got != raw {
                        return Err(RepairError::Overview);
                    }
                    OverviewRow {
                        offset: *offset,
                        length: frame.len() as u32,
                        decoded_length: raw.len() as u32,
                    }
                }
                None => {
                    let frame = compress(&raw, self.level).map_err(WriteError::from)?;
                    let at = keep + tail.len() as u64;
                    record::write(
                        &mut tail,
                        Kind::OverviewBlock,
                        &[&record::overview_head(shift, k), &frame],
                    );
                    OverviewRow {
                        offset: at + (record::PAYLOAD_AT + record::OVERVIEW_HEAD_LEN) as u64,
                        length: frame.len() as u32,
                        decoded_length: raw.len() as u32,
                    }
                }
            };
            overview_rows.push((shift, k, row));
        }
        overview_rows.sort_by_key(|&(shift, k, _)| (shift, k));

        let mut digests = Vec::with_capacity(self.channels.len());
        let mut channels = Vec::with_capacity(self.channels.len());
        for (state, declared) in self.channels.into_iter().zip(&declaration.channels) {
            let data = match (state, &declared.data) {
                (
                    Channel::Digital {
                        hasher,
                        edges,
                        gaps,
                    },
                    DeclaredKind::Digital { initial },
                ) => {
                    // A gap the recording ends inside: the level is not known from its `from`.
                    let kept = gaps
                        .iter()
                        .position(|g| g.to >= length)
                        .unwrap_or(gaps.len());
                    let end = gaps.get(kept).map_or(length, |g| g.from);
                    let gaps = gaps[..kept].to_vec();
                    digests.push(hasher.finish_digital(end, &gaps));
                    ChannelKind::Digital {
                        initial: *initial,
                        end,
                        edges,
                        gaps,
                    }
                }
                (
                    Channel::Analog {
                        hasher,
                        count,
                        runs,
                    },
                    DeclaredKind::Analog {
                        unit,
                        period,
                        coding,
                    },
                ) => {
                    let runs: Vec<Break> = runs.into_iter().filter(|r| r.index < count).collect();
                    let first = runs.first().map_or(0, |r| r.tick);
                    let breaks = runs.get(1..).unwrap_or_default().to_vec();
                    digests.push(hasher.finish_analog(first, count, &breaks));
                    ChannelKind::Analog {
                        unit: unit.clone(),
                        first,
                        period: *period,
                        count,
                        breaks,
                        coding: *coding,
                    }
                }
                _ => unreachable!("channels follow the declaration"),
            };
            channels.push(ChannelHeader {
                name: declared.name.clone(),
                source_name: declared.source_name.clone(),
                data,
                shaping: declared.shaping.clone(),
            });
        }
        let header = finished_header(
            &declaration,
            length,
            channels,
            &digests,
            self.rows.len() as u32,
            &done.plan,
            BTreeMap::new(),
        );
        check_header(&header)?;
        let mut overview_raw = Vec::new();
        overview::write(&done.head, &mut overview_raw);
        let overview_frame = compress(&overview_raw, self.level).map_err(WriteError::from)?;
        let rows: Vec<OverviewRow> = overview_rows.into_iter().map(|(_, _, r)| r).collect();
        let footer = record::footer_bytes(&header, &self.rows, &rows, &overview_frame)
            .map_err(WriteError::FooterTooLarge)?;
        let at = keep + tail.len() as u64;
        let mut end = Vec::new();
        record::write_footer(&mut end, at, &footer);
        tail.extend_from_slice(&end);
        let mut generated: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for a in self.annotations {
            if let Some(line) = clip_annotation(a.annotation, length) {
                generated.entry(a.source).or_default().push(line);
            }
        }
        Ok(Repaired {
            keep,
            tail,
            header,
            generated,
        })
    }
}

/// Repairs a cut-off file held in memory.
pub fn repair(file: &[u8], level: i32) -> Result<Repaired, RepairError> {
    let mut r = Repairer::new(level);
    r.feed(file)?;
    r.finish()
}
