//! The encoder: a `model::Recording` in, a file out through any `Write`. It merges the channels'
//! items into axis order and hands them to a `Writer`, each in its source's ticks, so a capture
//! larger than memory streams through.

use std::collections::BTreeMap;
use std::io::Write;

use crate::decode::overview_room;
use crate::header::{AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, SourceHeader};
use crate::mapping::{self, Mapping, Spacing};
use crate::model::{Break, ChannelData, Edges, Gap, Recording, Sample, Samples, run_sample_tick};
use crate::overview;
use crate::samples::coding_name;
pub use crate::writer::{
    Bundle, Finish, Finished, WriteError, WriteOptions, Writer, code_of, generator_file,
};

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error("{channel}: {promised} items promised, {delivered} delivered")]
    Count {
        channel: String,
        promised: u64,
        delivered: u64,
    },
    #[error("{channel}: an analog coding was given for a digital channel")]
    CodingOnDigital { channel: String },
    #[error("{channel}: its samples are {samples}, which a {coding} coding does not store")]
    CodingMismatch {
        channel: String,
        samples: String,
        coding: String,
    },
    #[error("writing: {0}")]
    Io(#[from] std::io::Error),
}

pub struct Options {
    /// The zstd level for blocks and the overview.
    pub level: i32,
    /// The decoded size a block aims for.
    pub block_bytes: usize,
    /// How many blocks compress together; None for four a thread.
    pub batch: Option<usize>,
    pub title: Option<String>,
    pub meta: BTreeMap<String, String>,
    /// The `converter` of every source that doesn't state one.
    pub converter: Option<String>,
    /// A coding for each analog channel, by channel index. Absent or None stores the samples as
    /// their type has them; f32 samples can also go in `codes`.
    pub analog: Vec<Option<AnalogCoding>>,
    /// The finest overview level's shift; None for the one the data's density gives
    /// (`mapping::finest_shift`).
    pub overview_shift: Option<u8>,
    /// Generated annotations to write, each with its generator's name; their times are axis
    /// ticks.
    pub annotations: Vec<(String, serde_json::Map<String, serde_json::Value>)>,
    /// Bundle the annotations into the footer rather than handing them back.
    pub bundle: Option<Bundle>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            level: 19,
            block_bytes: 1 << 20,
            batch: None,
            title: None,
            meta: BTreeMap::new(),
            converter: None,
            analog: Vec::new(),
            overview_shift: None,
            annotations: Vec::new(),
            bundle: None,
        }
    }
}

enum Stream {
    Digital {
        edges: Edges,
        next: Option<i64>,
        gaps: std::vec::IntoIter<Gap>,
        next_gap: Option<Gap>,
        /// Edges handed on, and how many the channel promised.
        index: u64,
        count: u64,
    },
    Analog {
        samples: Box<dyn Iterator<Item = Sample> + Send>,
        next: Option<Sample>,
        index: u64,
        count: u64,
        first: i64,
        period: crate::model::Ratio,
        breaks: Vec<Break>,
    },
}

impl Stream {
    /// The next item's source tick: a gap's at its `from`, so it goes in before the edges after
    /// it.
    fn next_tick(&self) -> Option<i64> {
        match self {
            Stream::Digital { next, next_gap, .. } => match (next, next_gap) {
                (Some(t), Some(g)) => Some((*t).min(g.from)),
                (t, g) => t.or(g.map(|g| g.from)),
            },
            Stream::Analog {
                next,
                index,
                first,
                period,
                breaks,
                ..
            } => next.map(|_| run_sample_tick(*first, *period, breaks, *index)),
        }
    }
}

/// Where the next item of a stream lies on the axis. One that maps nowhere goes first, for the
/// writer to refuse.
fn axis_of(map: &Mapping, tick: i64) -> i64 {
    map.axis(tick).unwrap_or(i64::MIN)
}

fn drain(writer: &mut Writer, out: &mut impl Write) -> std::io::Result<()> {
    if !writer.output().is_empty() {
        out.write_all(writer.output())?;
        let n = writer.output().len();
        writer.consume(n);
    }
    Ok(())
}

/// Encodes `recording` into `out`, and returns what the writer finished with: its header and
/// generated annotations (a recording from a capture has none).
pub fn encode_to(
    recording: Recording,
    options: &Options,
    mut out: impl Write,
) -> Result<Finished, EncodeError> {
    let Recording {
        timebase,
        length,
        channels,
        derived,
        sources,
    } = recording;
    let mut declared = Vec::with_capacity(channels.len());
    let mut streams = Vec::with_capacity(channels.len());
    let mut names = Vec::with_capacity(channels.len());
    let mut ends = Vec::new();
    // Each channel's items, a digital one's as its iterator promises them, and a digital
    // channel's end: what the overview's density needs.
    let mut items = Vec::with_capacity(channels.len());
    for (i, channel) in channels.into_iter().enumerate() {
        let coding = options.analog.get(i).copied().flatten();
        let data = match channel.data {
            ChannelData::Digital(d) => {
                if coding.is_some() {
                    return Err(EncodeError::CodingOnDigital {
                        channel: channel.name,
                    });
                }
                items.push((d.count, d.end));
                let mut edges = d.edges;
                let next = edges.next();
                let mut gaps = d.gaps.into_iter();
                let next_gap = gaps.next();
                streams.push(Stream::Digital {
                    edges,
                    next,
                    gaps,
                    next_gap,
                    index: 0,
                    count: d.count,
                });
                ends.push(d.end);
                DeclaredKind::Digital { initial: d.initial }
            }
            ChannelData::Analog(a) => {
                let natural = match &a.samples {
                    Samples::F32(_) => AnalogCoding::F32,
                    Samples::F64(_) => AnalogCoding::F64,
                    Samples::U64(int, _) | Samples::I64(int, _) => AnalogCoding::Int(*int),
                };
                let coding = match (coding, natural) {
                    (None, natural) => natural,
                    (Some(c @ AnalogCoding::Codes { .. }), AnalogCoding::F32) => c,
                    (Some(c), natural) if c == natural => c,
                    (Some(c), natural) => {
                        return Err(EncodeError::CodingMismatch {
                            channel: channel.name,
                            samples: coding_name(natural),
                            coding: coding_name(c),
                        });
                    }
                };
                items.push((a.count, 0));
                let mut samples = a.samples.into_samples();
                let next = samples.next();
                streams.push(Stream::Analog {
                    samples,
                    next,
                    index: 0,
                    count: a.count,
                    first: a.first,
                    period: a.period,
                    breaks: a.breaks,
                });
                DeclaredKind::Analog {
                    unit: a.unit,
                    period: a.period,
                    coding,
                    significant: a.significant,
                }
            }
        };
        names.push(channel.name.clone());
        declared.push(DeclaredChannel {
            name: channel.name,
            source_name: channel.source_name,
            source: channel.source,
            data,
            shaping: channel.shaping,
        });
    }
    let mut declaration = Declaration {
        timebase,
        channels: declared,
        derived,
        sources: sources
            .into_iter()
            .map(|source| SourceHeader {
                format: source.format,
                files: source.files,
                timebase: source.timebase,
                origin: source.origin,
                align: source.align,
                stretch: source.stretch,
                instrument: source.instrument,
                software: source.software,
                begin_s: source.begin_s,
                trigger_s: source.trigger_s,
                rate: source.rate,
                extra: source.extra,
                converter: source.converter.or_else(|| options.converter.clone()),
                title: source.title,
                meta: source.meta,
            })
            .collect(),
        title: options.title.clone(),
        meta: options.meta.clone(),
        overview_shift: 0,
    };
    // A channel whose source has no `m` maps nowhere: the writer refuses its declaration.
    let maps: Vec<Mapping> = (0..declaration.channels.len())
        .map(|c| declaration.mapping(c).unwrap_or(Mapping::IDENTITY))
        .collect();
    declaration.overview_shift = options.overview_shift.unwrap_or_else(|| {
        finest_kept(
            length,
            mapping::finest_shift(densest(&declaration, &maps, &items)),
        )
    });
    let batch = options
        .batch
        .unwrap_or_else(|| 4 * rayon::current_num_threads().max(1));
    let mut writer = Writer::new(
        declaration,
        WriteOptions {
            level: options.level,
            block_bytes: options.block_bytes,
            batch,
        },
    )?;
    for (source, annotation) in &options.annotations {
        writer.annotation(source, annotation.clone())?;
    }
    drain(&mut writer, &mut out)?;
    loop {
        let next = streams
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.next_tick().map(|t| (axis_of(&maps[i], t), t, i)))
            .min_by_key(|&(axis, _, i)| (axis, i));
        let Some((_, tick, c)) = next else {
            break;
        };
        match &mut streams[c] {
            Stream::Digital {
                edges,
                next,
                gaps,
                next_gap,
                index,
                count,
            } => {
                if let Some(g) = *next_gap
                    && g.from == tick
                    && next.is_none_or(|t| t > g.from)
                {
                    writer.gap(c, g)?;
                    *next_gap = gaps.next();
                } else {
                    if *index >= *count {
                        return Err(EncodeError::Count {
                            channel: names[c].clone(),
                            promised: *count,
                            delivered: *index + 1 + edges.by_ref().count() as u64,
                        });
                    }
                    writer.edge(c, tick)?;
                    *index += 1;
                    *next = edges.next();
                }
            }
            Stream::Analog {
                samples,
                next,
                index,
                count,
                first,
                breaks,
                ..
            } => {
                let value = next.take().unwrap();
                if *index >= *count {
                    return Err(EncodeError::Count {
                        channel: names[c].clone(),
                        promised: *count,
                        delivered: *index + 1 + samples.by_ref().count() as u64,
                    });
                }
                if *index == 0 {
                    writer.run(c, *first, 0)?;
                } else if let Ok(b) = breaks.binary_search_by_key(index, |b| b.index) {
                    writer.run(c, breaks[b].tick, breaks[b].lost)?;
                }
                writer.sample(c, value)?;
                *index += 1;
                *next = samples.next();
            }
        }
        if writer.output().len() >= 1 << 20 {
            drain(&mut writer, &mut out)?;
        }
    }
    for (stream, name) in streams.iter().zip(&names) {
        if let Stream::Analog { index, count, .. } | Stream::Digital { index, count, .. } = stream
            && index != count
        {
            return Err(EncodeError::Count {
                channel: name.clone(),
                promised: *count,
                delivered: *index,
            });
        }
    }
    drain(&mut writer, &mut out)?;
    let finished = writer.finish(
        length,
        &ends,
        Finish {
            bundle: options.bundle.clone(),
            ..Finish::default()
        },
    )?;
    out.write_all(&finished.tail)?;
    out.flush()?;
    Ok(finished)
}

/// The densest channel's spacing in axis ticks (FORMAT.md, "Overview"): an analog channel's
/// period times its source's `m`, a digital one's span from its start to its end over its edges.
/// `items` holds each channel's count, which the encoder holds it to, and a digital channel's
/// end. A channel with no item does not count.
fn densest(declaration: &Declaration, maps: &[Mapping], items: &[(u64, i64)]) -> Option<Spacing> {
    let spacings = declaration
        .channels
        .iter()
        .zip(items)
        .enumerate()
        .filter(|(_, (_, (count, _)))| *count > 0)
        .filter_map(|(c, (channel, &(count, end)))| match &channel.data {
            DeclaredKind::Analog { period, .. } => {
                let m = declaration.m(channel.source as usize)?;
                Some(Spacing::of_samples(*period, m))
            }
            DeclaredKind::Digital { .. } => {
                let start = declaration.start(c)?;
                let span = maps[c].axis(end)?.checked_sub(maps[c].axis(start)?)?;
                Spacing::of_edges(u64::try_from(span).ok()?, count)
            }
        });
    Spacing::closest(spacings)
}

/// The finest overview level for a recording of `length`: the densest channel's, `dense`, or the
/// least coarser one whose overview blocks tickwave keeps (`decode::MAX_OVERVIEW_BLOCKS`). The
/// length is known here; a live writer, which learns it as it goes, refuses past the bound
/// instead.
fn finest_kept(length: i64, dense: u8) -> u8 {
    (dense..overview::MAX_SHIFT)
        .find(|&shift| overview_room(length, shift).is_ok())
        .unwrap_or(overview::MAX_SHIFT)
}

/// Encodes `recording` into memory.
pub fn encode(recording: Recording, options: &Options) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::new();
    encode_to(recording, options, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where a recording's overview passes 2^20 blocks, counted by hand from FORMAT.md's levels:
    /// at 2^10-tick buckets, 4,123,223,130,112 ticks is 4,026,585,088 buckets, 983,053 blocks,
    /// and the levels at 2^14, 2^18 and 2^22 add 61,441, 3,841 and 241: 1,048,576. A tick more
    /// starts block 983,054. Ten hours at 1 GHz needs 1,072,883 blocks at 2^13 alone, and
    /// 572,197 in all at 2^14. The longest axis, 2^63 ticks, needs 2^20 at 2^31 before its
    /// coarser levels, and 559,232 in all at 2^32.
    #[test]
    fn a_recording_past_the_overview_bound_takes_the_least_coarser_finest_level() {
        const KEPT: i64 = 4_123_223_130_112;
        assert_eq!(finest_kept(KEPT, 10), 10);
        assert_eq!(finest_kept(KEPT + 1, 10), 11);
        assert_eq!(finest_kept(KEPT + 1, 12), 12);
        assert_eq!(finest_kept(36_000_000_000_000, 10), 14);
        assert_eq!(finest_kept(i64::MAX, 0), 32);
    }
}
