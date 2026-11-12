//! Checks a file against the source it was converted from, reading the file in order: every edge
//! and sample, every gap and run, the overview and the content id.

use std::collections::VecDeque;
use std::io::Read;

use crate::decode::{BlockChannel, DecodeError, MAX_DECODED, inflate_limited};
use crate::hash::{self, ChannelHasher};
use crate::header::{ChannelKind, DeclaredKind, Header};
use crate::model::{Break, ChannelData, Edges, Gap, Ratio, Recording, Samples, run_sample_tick};
use crate::overview::{self, Builder, Level, Settled};
use crate::reader::{Ending, Event, Reader};

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
        hasher: ChannelHasher,
    },
    Analog {
        samples: Box<dyn Iterator<Item = f32> + Send>,
        unit: String,
        first: i64,
        period: Ratio,
        count: u64,
        breaks: Vec<Break>,
        hasher: ChannelHasher,
    },
}

/// Checks that `file` holds exactly what `source` holds, and returns its header.
pub fn verify(file: impl Read, source: Recording) -> Result<Header, VerifyError> {
    verify_limited(file, source, MAX_DECODED)
}

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
        source: _,
    } = source;
    let mut names = Vec::with_capacity(channels.len());
    let mut shapings = Vec::with_capacity(channels.len());
    let mut wants: Vec<Want> = Vec::with_capacity(channels.len());
    for channel in channels {
        names.push((channel.name.clone(), channel.source_name.clone()));
        wants.push(match channel.data {
            ChannelData::Digital(d) => Want::Digital {
                hasher: ChannelHasher::digital(&channel.name, d.initial, &channel.shaping),
                edges: d.edges,
                initial: d.initial,
                end: d.end,
                gaps: d.gaps,
                next_gap: 0,
                count: 0,
            },
            ChannelData::Analog(a) => {
                let Samples::F32(samples) = a.samples;
                Want::Analog {
                    hasher: ChannelHasher::analog(
                        &channel.name,
                        &a.unit,
                        a.period,
                        &channel.shaping,
                    ),
                    samples,
                    unit: a.unit,
                    first: a.first,
                    period: a.period,
                    count: a.count,
                    breaks: a.breaks,
                }
            }
        });
        shapings.push(channel.shaping);
    }
    let mut reader = Reader::new(max);
    let mut builder: Option<Builder> = None;
    let mut expected: VecDeque<Settled> = VecDeque::new();
    let mut head: Option<Vec<Level>> = None;
    let mut analog = Vec::new();
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
                analog = Vec::with_capacity(wants.len());
                for (((declared, want), (name, source_name)), shaping) in
                    d.channels.iter().zip(&wants).zip(&names).zip(&shapings)
                {
                    if declared.name != *name || declared.source_name != *source_name {
                        return Err(mismatch(format!("channel {name}: the names differ")));
                    }
                    if declared.shaping != *shaping {
                        return Err(mismatch(format!(
                            "channel {name}: the offset or the kept span differs"
                        )));
                    }
                    match (&declared.data, want) {
                        (DeclaredKind::Digital { initial }, Want::Digital { initial: i, .. })
                            if initial == i =>
                        {
                            analog.push(false);
                        }
                        (
                            DeclaredKind::Analog { unit, period, .. },
                            Want::Analog {
                                unit: u, period: p, ..
                            },
                        ) if unit == u && period == p => analog.push(true),
                        _ => {
                            return Err(mismatch(format!(
                                "channel {name}: the kind, initial level or sampling differs"
                            )));
                        }
                    }
                }
                builder = Some(Builder::new(d.overview_shift, &d.channels));
            }
            Event::Gap { channel: c, gap } => {
                let Want::Digital { gaps, next_gap, .. } = &mut wants[c] else {
                    unreachable!("the reader checks a gap's channel")
                };
                if gaps.get(*next_gap) != Some(&gap) {
                    return Err(mismatch(format!("channel {}: a gap differs", names[c].0)));
                }
                *next_gap += 1;
                builder.as_mut().unwrap().gap(c, gap);
            }
            Event::Run {
                channel: c,
                index,
                tick,
            } => {
                let Want::Analog { first, breaks, .. } = &wants[c] else {
                    unreachable!("the reader checks a run's channel")
                };
                let fits = if index == 0 {
                    tick == *first
                } else {
                    breaks.contains(&Break { index, tick })
                };
                if !fits {
                    return Err(mismatch(format!("channel {}: a run differs", names[c].0)));
                }
            }
            Event::Block(block) => {
                let b = builder.as_mut().unwrap();
                for (c, got) in block.channels.into_iter().enumerate() {
                    let name = &names[c].0;
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
                            for tick in edges {
                                if want.next() != Some(tick) {
                                    return Err(mismatch(format!(
                                        "channel {name}: edge at tick {tick} differs"
                                    )));
                                }
                                hasher.edge(tick);
                                b.edge(c, tick);
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
                                period,
                                breaks,
                                hasher,
                                ..
                            },
                        ) => {
                            for (index, value) in (first_index..).zip(values) {
                                if samples.next().map(f32::to_bits) != Some(value.to_bits()) {
                                    return Err(mismatch(format!(
                                        "channel {name}: sample {index} differs"
                                    )));
                                }
                                hasher.sample(value);
                                b.sample(c, run_sample_tick(*first, *period, breaks, index), value);
                            }
                        }
                        _ => unreachable!("blocks follow the declaration"),
                    }
                }
                b.settle(block.head.end);
                expected.extend(b.take_settled());
                if block.head.end == length {
                    let (done, last) = builder.take().unwrap().finish(length);
                    expected.extend(last);
                    head = Some(done.head);
                }
            }
            Event::OverviewBlock {
                shift, k, frame, ..
            } => {
                let want = expected
                    .pop_front()
                    .ok_or_else(|| mismatch("an overview block the content does not settle"))?;
                if (want.shift, want.k) != (shift, k) {
                    return Err(mismatch(format!(
                        "overview block {k} of level 2^{shift} is out of place"
                    )));
                }
                let n = want.level.buckets;
                let raw = inflate_limited(&frame, overview::decoded_len(n, &analog) as usize, max)?;
                let got = overview::parse_block(&raw, shift, n, &analog)
                    .ok_or_else(|| mismatch(format!("overview block {k} does not parse")))?;
                if got != want.level {
                    return Err(mismatch(format!(
                        "overview level 2^{shift}, block {k}: differs from the content"
                    )));
                }
            }
            Event::Annotation(_) | Event::Skipped { .. } => {}
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
    if !expected.is_empty() {
        return Err(mismatch(
            "the file lacks overview blocks the content settles",
        ));
    }
    if head.as_ref() != Some(&footer.overview) {
        return Err(mismatch("the footer's overview differs from the content"));
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
                    ..
                },
            ) => {
                if end != *e || count != *n || gaps != *g || next_gap != gaps.len() {
                    return Err(mismatch(format!(
                        "channel {name}: the end, count or gaps differ"
                    )));
                }
                (edges.next().is_some(), hasher.finish_digital(end, &gaps))
            }
            (
                Want::Analog {
                    mut samples,
                    first,
                    count,
                    breaks,
                    hasher,
                    ..
                },
                ChannelKind::Analog {
                    first: f,
                    count: n,
                    breaks: b,
                    ..
                },
            ) => {
                let first = if count == 0 { 0 } else { first };
                if first != *f || count != *n || breaks != *b {
                    return Err(mismatch(format!(
                        "channel {name}: the first tick, count or breaks differ"
                    )));
                }
                (
                    samples.next().is_some(),
                    hasher.finish_analog(first, count, &breaks),
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
    if hash::content_id(timebase, length, &digests) != h.content_id {
        return Err(mismatch("the content id does not match the content"));
    }
    Ok(footer.header)
}
