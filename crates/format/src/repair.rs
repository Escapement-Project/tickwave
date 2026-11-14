//! Repair: a footer and a trailer for a file cut off before them, by a crash, a power cut or a
//! full disk. The records up to the first that is not whole stay as they are.

use std::collections::{BTreeMap, HashMap};

use crate::decode::{
    BlockChannel, DecodeError, MAX_DECODED, check_header, corrupt, gaps_ending_in, inflate_limited,
    level_after,
};
use crate::hash::ChannelHasher;
use crate::header::{
    BlockRow, ChannelHeader, ChannelKind, Declaration, DeclaredKind, Header, LEVEL_BIT,
    OverviewRow, Words,
};
use crate::mapping::Mapping;
use crate::model::{Break, Gap};
use crate::overview::{self, Builder, Settled};
use crate::reader::{Ending, Event, Reader};
use crate::record::{self, BlockHead, Kind};
use crate::writer::{
    Bundle, OVERVIEW_BLOCK_LEVEL, WriteError, assemble, bundled_files, compress, finished_header,
    generated,
};

/// A cut-off file's length (FORMAT.md, "Repairing a cut-off file"): the later of the last
/// block's end, 0 with no block, and one past the latest start among the annotation records.
pub fn repaired_length(last_end: i64, latest_annotation: Option<i64>) -> i64 {
    latest_annotation.map_or(last_end, |start| last_end.max(start.saturating_add(1)))
}

/// The latest start among annotation records, with `record` taken in.
pub(crate) fn latest_start(latest: Option<i64>, record: &record::Annotation) -> Option<i64> {
    match record.annotation.get("start").and_then(|s| s.as_i64()) {
        Some(start) => Some(latest.map_or(start, |l| l.max(start))),
        None => latest,
    }
}

/// A digital channel a file with no block declares high at its start: no block carries that level,
/// and with no block the channel is never known, so its `initial` would have to be false.
pub(crate) fn high_without_block(d: &Declaration) -> Option<&str> {
    d.channels.iter().find_map(|c| match c.data {
        DeclaredKind::Digital { initial: true } => Some(c.name.as_str()),
        _ => None,
    })
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
    #[error(
        "the file holds no block, and channel {0} is declared high at tick 0: no block carries \
         that level, so it isn't repaired"
    )]
    HighWithoutBlock(String),
    #[error("an overview block in the file differs from the one its content gives")]
    Overview,
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
        /// Each gap's `to` on the axis.
        gap_ends: Vec<i64>,
        /// The level after the last block: its start's, through its edges and gap ends.
        level: bool,
        map: Mapping,
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
    words: Words,
    settled: Vec<Settled>,
    /// The overview blocks the file holds: their frames' offsets and bytes.
    present: HashMap<(u8, u64), (u64, Vec<u8>)>,
    annotations: Vec<record::Annotation>,
    /// The latest start among the annotation records.
    latest_annotation: Option<i64>,
    bundle: Option<Bundle>,
    /// A record that does not hold together: a derived channel's operands that don't pair.
    error: Option<String>,
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
            words: Words::default(),
            settled: Vec::new(),
            present: HashMap::new(),
            annotations: Vec::new(),
            latest_annotation: None,
            bundle: None,
            error: None,
        }
    }

    /// Bundles the annotations into the footer, as the writer would have, rather than handing
    /// them back as generator files alone.
    pub fn bundle(&mut self, bundle: Bundle) {
        self.bundle = Some(bundle);
    }

    /// The file's next bytes, from its start.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), RepairError> {
        self.reader.feed(bytes);
        while let Some(event) = self.reader.next_event()? {
            if self.error.is_none() {
                self.take(event);
            }
        }
        match &self.error {
            Some(e) => Err(RepairError::Decode(corrupt(e.clone()))),
            None => Ok(()),
        }
    }

    fn take(&mut self, event: Event) {
        match event {
            Event::Declaration(d) => {
                self.builder = Some(Builder::new(d.overview_shift, &d.channels, &d.derived));
                let mut channels = Vec::with_capacity(d.channels.len());
                for (c, channel) in d.channels.iter().enumerate() {
                    let (Some(hasher), Some(map)) = (ChannelHasher::declared(&d, c), d.mapping(c))
                    else {
                        self.error
                            .get_or_insert(format!("channel {c} has no mapping to the axis"));
                        return;
                    };
                    channels.push(match &channel.data {
                        DeclaredKind::Digital { initial } => Channel::Digital {
                            hasher,
                            edges: 0,
                            gaps: Vec::new(),
                            gap_ends: Vec::new(),
                            level: *initial,
                            map,
                        },
                        DeclaredKind::Analog { .. } => Channel::Analog {
                            hasher,
                            count: 0,
                            runs: Vec::new(),
                        },
                    });
                }
                self.channels = channels;
                self.words = Words::new(d.channels.len());
                self.declaration = Some(*d);
            }
            Event::Gap { channel, gap } => {
                if let Channel::Digital {
                    gaps,
                    gap_ends,
                    map,
                    ..
                } = &mut self.channels[channel]
                {
                    // The reader holds a gap's ends inside the axis.
                    let to = map.axis(gap.to).unwrap_or(i64::MAX);
                    gaps.push(gap);
                    gap_ends.push(to);
                    self.builder.as_mut().unwrap().gap(channel, gap, to);
                }
            }
            Event::Run { channel, run } => {
                if let Channel::Analog { runs, .. } = &mut self.channels[channel] {
                    runs.push(run);
                }
            }
            Event::Block(block) => {
                let builder = self.builder.as_mut().unwrap();
                for (c, got) in block.channels.iter().enumerate() {
                    let source = got.source_ticks().unwrap_or_default();
                    match (got, &mut self.channels[c]) {
                        (
                            BlockChannel::Digital { edges, initial, .. },
                            Channel::Digital {
                                hasher,
                                edges: n,
                                gaps,
                                gap_ends,
                                level,
                                ..
                            },
                        ) => {
                            for (&k, &tick) in source.iter().zip(edges) {
                                hasher.edge(k);
                                builder.edge(c, k, tick);
                                *n += 1;
                            }
                            *level = level_after(
                                *initial,
                                source,
                                gaps_ending_in(gaps, gap_ends, block.head.start, block.head.end),
                            );
                        }
                        (
                            BlockChannel::Analog {
                                values,
                                first_index,
                            },
                            Channel::Analog { hasher, count, .. },
                        ) => {
                            for (index, value) in (*first_index..).zip(values.iter()) {
                                let Some(tick) = self.reader.sample_axis_tick(c, index) else {
                                    self.error.get_or_insert(format!(
                                        "channel {c}: sample {index} has no place on the axis"
                                    ));
                                    return;
                                };
                                hasher.sample(value);
                                if let Err(e) = builder.sample(c, tick, value) {
                                    self.error.get_or_insert(e.to_string());
                                }
                                *count += 1;
                            }
                        }
                        _ => unreachable!("blocks follow the declaration"),
                    }
                }
                if let Err(e) = builder.settle(block.head.end) {
                    self.error.get_or_insert(e.to_string());
                }
                self.settled.extend(builder.take_settled());
                self.rows.push(BlockRow {
                    start: block.head.start,
                    offset: block.frame_offset,
                    length: block.frame_len,
                    decoded_length: block.head.decoded_length,
                });
                self.words.push(&block.head.words);
            }
            Event::OverviewBlock {
                shift,
                k,
                frame_offset,
                frame,
            } => {
                self.present.insert((shift, k), (frame_offset, frame));
            }
            Event::Annotation(a) => {
                self.latest_annotation = latest_start(self.latest_annotation, &a);
                self.annotations.push(a);
            }
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
        if self.rows.is_empty() && self.annotations.is_empty() {
            return Err(RepairError::NoBlock);
        }
        if self.rows.is_empty()
            && let Some(name) = high_without_block(&declaration)
        {
            return Err(RepairError::HighWithoutBlock(name.to_owned()));
        }
        let last_end = self.reader.next_start();
        let length = repaired_length(last_end, self.latest_annotation);
        if length < 1 {
            return Err(RepairError::NoBlock);
        }
        let mut tail = Vec::new();
        // Marks past the last block: an empty block runs to them.
        if length > last_end {
            let words: Vec<u32> = self
                .channels
                .iter()
                .map(|c| match c {
                    Channel::Digital { level: true, .. } => LEVEL_BIT,
                    _ => 0,
                })
                .collect();
            let raw = assemble(&vec![Vec::new(); words.len()], &vec![None; words.len()]);
            let frame = compress(&raw, self.level).map_err(WriteError::from)?;
            let head = BlockHead {
                start: last_end,
                end: length,
                decoded_length: raw.len() as u32,
                words,
            };
            record::write(&mut tail, Kind::Block, &[&head.to_bytes(), &frame]);
            self.rows.push(BlockRow {
                start: last_end,
                offset: keep + (record::PAYLOAD_AT + BlockHead::len(head.words.len())) as u64,
                length: frame.len() as u32,
                decoded_length: raw.len() as u32,
            });
            self.words.push(&head.words);
        }
        let (done, last) = builder
            .finish(length)
            .map_err(|e| RepairError::Decode(corrupt(e.to_string())))?;
        let mut overview_rows = Vec::new();
        for Settled { shift, k, level } in std::mem::take(&mut self.settled).into_iter().chain(last)
        {
            let mut raw = Vec::new();
            overview::write_level(&level, &mut raw);
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
                    let frame = compress(&raw, OVERVIEW_BLOCK_LEVEL).map_err(WriteError::from)?;
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
                        map,
                        ..
                    },
                    DeclaredKind::Digital { initial },
                ) => {
                    // Known to the source tick at the last block's end; a gap that reaches it ends
                    // what is known at its `from`, unless that lies past the end too: a recorder
                    // can learn of a loss before it writes the block the loss starts in.
                    let last = map.first_at(last_end);
                    let kept = gaps.iter().position(|g| g.to >= last).unwrap_or(gaps.len());
                    let end = gaps.get(kept).map_or(last, |g| g.from.min(last));
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
                        significant,
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
                        significant: *significant,
                    }
                }
                _ => unreachable!("channels follow the declaration"),
            };
            channels.push(ChannelHeader {
                name: declared.name.clone(),
                source_name: declared.source_name.clone(),
                source: declared.source,
                data,
                shaping: declared.shaping.clone(),
            });
        }
        let mut header = finished_header(
            &declaration,
            length,
            channels,
            &digests,
            self.rows.len() as u32,
            &done.plan,
            Vec::new(),
        );
        let generated = generated(&self.annotations, length);
        header.annotations = match self.bundle {
            None => None,
            Some(Bundle::Generated { stem }) => {
                Some(bundled_files(&stem, header.content_id, &generated))
            }
            Some(Bundle::Files(files)) => Some(files),
        };
        check_header(&header)?;
        let mut overview_raw = Vec::new();
        overview::write(&done.head, &mut overview_raw);
        let overview_frame = compress(&overview_raw, self.level).map_err(WriteError::from)?;
        let rows: Vec<OverviewRow> = overview_rows.into_iter().map(|(_, _, r)| r).collect();
        let footer = record::footer_bytes(&header, &self.rows, &self.words, &rows, &overview_frame)
            .map_err(WriteError::FooterTooLarge)?;
        let at = keep + tail.len() as u64;
        let mut end = Vec::new();
        record::write_footer(&mut end, at, &footer);
        tail.extend_from_slice(&end);
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
