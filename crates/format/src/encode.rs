//! The encoder: a `model::Recording` in, a whole file out.
//!
//! Blocks are cut by time across all channels. Items are taken in tick order; once a block's
//! decoded payload reaches the target size, the block ends before the next item whose tick is
//! later than the last one taken, so items sharing a tick stay together. Block i covers ticks
//! `[start_i, start_{i+1})`; the last block runs to the recording's end.
//!
//! A block's decoded payload: for each channel, a u32 section length, then the sections in channel
//! order. A digital section is the block's edges as LEB128 intervals, the first measured from the
//! block's start tick. An analog section is the block's samples in the channel's coding.

use std::collections::BTreeMap;

use rayon::prelude::*;

use crate::codec::put_uleb;
use crate::hash::{self, ChannelHasher};
use crate::header::{
    AnalogCoding, BlockRow, ChannelHeader, ChannelKind, Header, JSON_SAFE, LEVEL_BIT, MAJOR, MINOR,
    PREAMBLE_LEN, Preamble, SourceHeader,
};
use crate::model::{ChannelData, Edges, Ratio, Recording, Samples};
use crate::overview::{self, AnalogBuilder, DigitalBuilder, Level};

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("timebase {0}/{1} is not a ratio of positive integers")]
    Timebase(u64, u64),
    #[error("{what} {value} does not fit the header, which holds integers below 2^53")]
    TooLarge { what: &'static str, value: u64 },
    #[error("{channel}: ends at tick {end}, outside the recording's 0 to {length}")]
    End {
        channel: String,
        end: i64,
        length: i64,
    },
    #[error("{channel}: edge at tick {tick} does not follow tick {previous}")]
    Order {
        channel: String,
        tick: i64,
        previous: i64,
    },
    #[error("{channel}: edge at tick {tick} lies outside the channel's ticks 1 to {end}")]
    EdgeRange {
        channel: String,
        tick: i64,
        end: i64,
    },
    #[error("{channel}: {promised} samples promised, {delivered} delivered")]
    Count {
        channel: String,
        promised: u64,
        delivered: u64,
    },
    #[error("{channel}: samples run past the recording's end, tick {length}")]
    SampleRange { channel: String, length: i64 },
    #[error("{channel}: sample {index}, {value}, is not exactly a code of {coding:?}")]
    Coding {
        channel: String,
        index: u64,
        value: f32,
        coding: AnalogCoding,
    },
    #[error("{channel}: an analog coding was given for a digital channel")]
    CodingOnDigital { channel: String },
    #[error("the block size target must lie between 1 and 2^30 bytes")]
    BlockSize,
    #[error("the head is {0} bytes, more than a u32 offset reaches")]
    HeadTooLarge(usize),
    #[error("zstd: {0}")]
    Zstd(#[from] std::io::Error),
}

pub struct Options {
    /// The zstd level for blocks and the overview.
    pub level: i32,
    /// The decoded size a block aims for.
    pub block_bytes: usize,
    pub title: Option<String>,
    pub meta: BTreeMap<String, String>,
    pub converter: Option<String>,
    /// A coding for each analog channel, by channel index; absent or None stores f32.
    pub analog: Vec<Option<AnalogCoding>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            level: 19,
            block_bytes: 1 << 20,
            title: None,
            meta: BTreeMap::new(),
            converter: None,
            analog: Vec::new(),
        }
    }
}

/// One zstd frame with a content checksum, which the decoder requires.
fn compress(raw: &[u8], level: i32) -> std::io::Result<Vec<u8>> {
    let mut compressor = zstd::bulk::Compressor::new(level)?;
    compressor.set_parameter(zstd::stream::raw::CParameter::ChecksumFlag(true))?;
    compressor.compress(raw)
}

/// The code for `value` under `coding`, if one reproduces it exactly.
pub fn code_of(value: f32, coding: AnalogCoding) -> Option<u16> {
    let AnalogCoding::Codes {
        bits,
        scale,
        offset,
    } = coding
    else {
        return None;
    };
    let code = ((f64::from(value) - offset) / scale).round();
    if !(0.0..f64::from(1u32 << bits)).contains(&code) {
        return None;
    }
    let code = code as u16;
    let back = (offset + f64::from(code) * scale) as f32;
    (back.to_bits() == value.to_bits()).then_some(code)
}

struct DigitalStream {
    edges: Edges,
    next: Option<i64>,
    previous: i64,
    end: i64,
    level: bool,
    count: u64,
    hasher: ChannelHasher,
    overview: DigitalBuilder,
}

struct AnalogStream {
    samples: Box<dyn Iterator<Item = f32> + Send>,
    next: Option<f32>,
    index: u64,
    count: u64,
    first: i64,
    period: Ratio,
    coding: AnalogCoding,
    hasher: ChannelHasher,
    overview: AnalogBuilder,
}

enum Stream {
    Digital(DigitalStream),
    Analog(AnalogStream),
}

impl AnalogStream {
    fn tick(&self, index: u64) -> i64 {
        self.first
            + (u128::from(index) * u128::from(self.period.num) / u128::from(self.period.den)) as i64
    }
}

impl Stream {
    fn next_tick(&self) -> Option<i64> {
        match self {
            Stream::Digital(d) => d.next,
            Stream::Analog(a) => a.next.map(|_| a.tick(a.index)),
        }
    }
}

struct Open {
    start: i64,
    sections: Vec<Vec<u8>>,
    words: Vec<u32>,
    size: usize,
    last: i64,
}

fn check_safe(what: &'static str, value: u64) -> Result<(), EncodeError> {
    if value < JSON_SAFE {
        Ok(())
    } else {
        Err(EncodeError::TooLarge { what, value })
    }
}

pub fn encode(recording: Recording, options: &Options) -> Result<Vec<u8>, EncodeError> {
    let Recording {
        timebase,
        length,
        channels,
        source,
    } = recording;
    if timebase.num == 0 || timebase.den == 0 {
        return Err(EncodeError::Timebase(timebase.num, timebase.den));
    }
    if options.block_bytes == 0 || options.block_bytes > 1 << 30 {
        return Err(EncodeError::BlockSize);
    }
    check_safe("timebase numerator", timebase.num)?;
    check_safe("timebase denominator", timebase.den)?;
    check_safe("length", length.max(0) as u64)?;
    let shifts = overview::levels(length);
    let finest = shifts[0];

    let mut names = Vec::with_capacity(channels.len());
    let mut kinds = Vec::with_capacity(channels.len());
    let mut streams = Vec::with_capacity(channels.len());
    for (i, channel) in channels.into_iter().enumerate() {
        let coding = options.analog.get(i).copied().flatten();
        match channel.data {
            ChannelData::Digital(d) => {
                if coding.is_some() {
                    return Err(EncodeError::CodingOnDigital {
                        channel: channel.name,
                    });
                }
                if d.end < 0 || d.end > length {
                    return Err(EncodeError::End {
                        channel: channel.name,
                        end: d.end,
                        length,
                    });
                }
                let mut edges = d.edges;
                let next = edges.next();
                kinds.push(ChannelKind::Digital {
                    initial: d.initial,
                    end: d.end,
                    edges: 0,
                });
                streams.push(Stream::Digital(DigitalStream {
                    edges,
                    next,
                    previous: 0,
                    end: d.end,
                    level: d.initial,
                    count: 0,
                    hasher: ChannelHasher::digital(&channel.name, d.initial, d.end),
                    overview: DigitalBuilder::new(length, finest, d.initial),
                }));
            }
            ChannelData::Analog(a) => {
                let period = [a.period.num, a.period.den];
                if a.period.num == 0 || a.period.den == 0 {
                    return Err(EncodeError::Timebase(a.period.num, a.period.den));
                }
                check_safe("analog sample count", a.count)?;
                check_safe("analog period numerator", a.period.num)?;
                check_safe("analog period denominator", a.period.den)?;
                let coding = coding.unwrap_or(AnalogCoding::F32);
                let Samples::F32(mut samples) = a.samples;
                let next = samples.next();
                kinds.push(ChannelKind::Analog {
                    unit: a.unit.clone(),
                    first: a.first,
                    period,
                    count: a.count,
                    coding,
                });
                let stream = AnalogStream {
                    samples,
                    next,
                    index: 0,
                    count: a.count,
                    first: a.first,
                    period: a.period,
                    coding,
                    hasher: ChannelHasher::analog(&channel.name, &a.unit, a.first, period, a.count),
                    overview: AnalogBuilder::new(length, finest),
                };
                if a.first < 0 || (a.count > 0 && stream.tick(a.count - 1) > length) {
                    return Err(EncodeError::SampleRange {
                        channel: channel.name,
                        length,
                    });
                }
                streams.push(Stream::Analog(stream));
            }
        }
        names.push((channel.name, channel.source_name));
    }

    let new_block = |start: i64, streams: &[Stream]| Open {
        start,
        sections: vec![Vec::new(); streams.len()],
        words: streams
            .iter()
            .map(|s| match s {
                Stream::Digital(d) if d.level => LEVEL_BIT,
                _ => 0,
            })
            .collect(),
        size: 0,
        last: start,
    };
    let mut pending: Vec<(i64, Vec<u32>, Vec<u8>)> = Vec::new();
    let mut blocks: Vec<(BlockRow, Vec<u8>)> = Vec::new();
    let batch = 4 * rayon::current_num_threads().max(1);
    let flush = |pending: &mut Vec<(i64, Vec<u32>, Vec<u8>)>,
                 blocks: &mut Vec<(BlockRow, Vec<u8>)>|
     -> Result<(), EncodeError> {
        let compressed = pending
            .par_iter()
            .map(|(_, _, raw)| compress(raw, options.level))
            .collect::<Result<Vec<_>, _>>()?;
        for ((start, words, raw), frame) in pending.drain(..).zip(compressed) {
            blocks.push((
                BlockRow {
                    start,
                    offset: 0,
                    length: frame.len() as u32,
                    decoded_length: raw.len() as u32,
                    channels: words,
                },
                frame,
            ));
        }
        Ok(())
    };
    let planes: Vec<bool> = kinds
        .iter()
        .map(|k| {
            matches!(
                k,
                ChannelKind::Analog {
                    coding: AnalogCoding::Codes { .. },
                    ..
                }
            )
        })
        .collect();
    let close = |open: Open, pending: &mut Vec<(i64, Vec<u32>, Vec<u8>)>| {
        let mut raw = Vec::with_capacity(open.size + 4 * open.sections.len());
        for section in &open.sections {
            raw.extend_from_slice(&(section.len() as u32).to_le_bytes());
        }
        for (section, planes) in open.sections.iter().zip(&planes) {
            if *planes {
                // Codes were appended little-endian; store every low byte, then every high byte.
                raw.extend(section.iter().step_by(2));
                raw.extend(section.iter().skip(1).step_by(2));
            } else {
                raw.extend_from_slice(section);
            }
        }
        pending.push((open.start, open.words, raw));
    };

    let mut open = new_block(0, &streams);
    loop {
        let next = streams
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.next_tick().map(|t| (t, i)))
            .min();
        let Some((tick, i)) = next else {
            close(open, &mut pending);
            break;
        };
        if open.size >= options.block_bytes && tick > open.last {
            let next_open = new_block(tick, &streams);
            close(std::mem::replace(&mut open, next_open), &mut pending);
            if pending.len() >= batch {
                flush(&mut pending, &mut blocks)?;
            }
        }
        let name = &names[i].0;
        let section = &mut open.sections[i];
        let before = section.len();
        match &mut streams[i] {
            Stream::Digital(d) => {
                if tick < 1 || tick > d.end {
                    return Err(EncodeError::EdgeRange {
                        channel: name.clone(),
                        tick,
                        end: d.end,
                    });
                }
                if tick <= d.previous {
                    return Err(EncodeError::Order {
                        channel: name.clone(),
                        tick,
                        previous: d.previous,
                    });
                }
                let from = if open.words[i] & !LEVEL_BIT == 0 {
                    open.start
                } else {
                    d.previous
                };
                put_uleb(section, (tick - from) as u64);
                open.words[i] += 1;
                d.previous = tick;
                d.count += 1;
                d.level = !d.level;
                d.hasher.edge(tick);
                d.overview.edge(tick);
                d.next = d.edges.next();
            }
            Stream::Analog(a) => {
                let value = a.next.take().unwrap();
                if a.index >= a.count {
                    return Err(EncodeError::Count {
                        channel: name.clone(),
                        promised: a.count,
                        delivered: a.index + 1 + a.samples.by_ref().count() as u64,
                    });
                }
                match a.coding {
                    AnalogCoding::F32 => section.extend_from_slice(&value.to_le_bytes()),
                    coding @ AnalogCoding::Codes { .. } => {
                        let code = code_of(value, coding).ok_or_else(|| EncodeError::Coding {
                            channel: name.clone(),
                            index: a.index,
                            value,
                            coding,
                        })?;
                        section.extend_from_slice(&code.to_le_bytes());
                    }
                }
                open.words[i] += 1;
                a.hasher.sample(value);
                a.overview.sample(tick, value);
                a.index += 1;
                a.next = a.samples.next();
            }
        }
        open.size += section.len() - before;
        open.last = tick;
    }
    flush(&mut pending, &mut blocks)?;

    let mut digests = Vec::with_capacity(streams.len());
    let mut finest_channels = Vec::with_capacity(streams.len());
    for ((stream, kind), (name, _)) in streams.into_iter().zip(&mut kinds).zip(&names) {
        match stream {
            Stream::Digital(d) => {
                if let ChannelKind::Digital { edges, .. } = kind {
                    *edges = d.count;
                }
                check_safe("edge count", d.count)?;
                digests.push(d.hasher.finish());
                finest_channels.push(d.overview.finish());
            }
            Stream::Analog(a) => {
                if a.index != a.count {
                    return Err(EncodeError::Count {
                        channel: name.clone(),
                        promised: a.count,
                        delivered: a.index,
                    });
                }
                digests.push(a.hasher.finish());
                finest_channels.push(a.overview.finish());
            }
        }
    }
    let finest_level = Level {
        shift: finest,
        buckets: overview::buckets(length, finest),
        channels: finest_channels,
    };
    let mut levels = vec![];
    for &shift in &shifts[1..] {
        levels.push(overview::coarsen(&finest_level, shift, length));
    }
    levels.insert(0, finest_level);
    let mut overview_raw = Vec::new();
    overview::write(&levels, &mut overview_raw);
    let overview_frame = compress(&overview_raw, options.level)?;

    let timebase = [timebase.num, timebase.den];
    let header = Header {
        timebase,
        length,
        channels: names
            .into_iter()
            .zip(kinds)
            .map(|((name, source_name), data)| ChannelHeader {
                name,
                source_name,
                data,
            })
            .collect(),
        source: SourceHeader {
            format: source.format,
            files: source.files,
            instrument: source.instrument,
            software: source.software,
            begin_s: source.begin_s,
            extra: source.extra,
            converter: options.converter.clone(),
        },
        title: options.title.clone(),
        meta: options.meta.clone(),
        content_id: hash::content_id(timebase, length, &digests),
        blocks: blocks.len() as u32,
        overview: shifts,
        annotations: None,
    };
    let header_json = serde_json::to_vec(&header).expect("a header always serialises");
    let index_len = blocks.len() * BlockRow::size(header.channels.len());
    let head_len = PREAMBLE_LEN + header_json.len() + index_len + overview_frame.len();
    let head_len_u32 = u32::try_from(head_len).map_err(|_| EncodeError::HeadTooLarge(head_len))?;
    let mut offset = head_len as u64;
    let mut index = Vec::with_capacity(index_len);
    for (row, frame) in &mut blocks {
        row.offset = offset;
        offset += frame.len() as u64;
        row.write(&mut index);
    }
    let preamble = Preamble {
        major: MAJOR,
        minor: MINOR,
        header_len: header_json.len() as u32,
        index_len: index_len as u32,
        head_len: head_len_u32,
        file_len: offset,
    };
    let mut out = Vec::with_capacity(offset as usize);
    out.extend_from_slice(&preamble.to_bytes());
    out.extend_from_slice(&header_json);
    out.extend_from_slice(&index);
    out.extend_from_slice(&overview_frame);
    for (_, frame) in &blocks {
        out.extend_from_slice(frame);
    }
    debug_assert_eq!(out.len() as u64, offset);
    Ok(out)
}
