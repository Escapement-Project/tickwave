//! The encoder: a `model::Recording` in, a file out through any `Write`. It merges the channels'
//! items into tick order and hands them to a `Writer`, so a capture larger than memory streams
//! through.

use std::collections::BTreeMap;
use std::io::Write;

use crate::header::{AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, SourceHeader};
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
    #[error("{channel}: {promised} samples promised, {delivered} delivered")]
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
    /// The finest overview level's shift; None for the converter's, from the length.
    pub overview_shift: Option<u8>,
    /// Generated annotations to write, each with its generator's name; their times are ticks.
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
    /// The next item's tick: a gap's at its `from`, so it goes in before the edges after it.
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
    for (i, channel) in channels.into_iter().enumerate() {
        let coding = options.analog.get(i).copied().flatten();
        let data = match channel.data {
            ChannelData::Digital(d) => {
                if coding.is_some() {
                    return Err(EncodeError::CodingOnDigital {
                        channel: channel.name,
                    });
                }
                let mut edges = d.edges;
                let next = edges.next();
                let mut gaps = d.gaps.into_iter();
                let next_gap = gaps.next();
                streams.push(Stream::Digital {
                    edges,
                    next,
                    gaps,
                    next_gap,
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
    let declaration = Declaration {
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
                instrument: source.instrument,
                software: source.software,
                begin_s: source.begin_s,
                trigger_s: source.trigger_s,
                rate: source.rate,
                extra: source.extra,
                converter: source.converter.or_else(|| options.converter.clone()),
            })
            .collect(),
        title: options.title.clone(),
        meta: options.meta.clone(),
        overview_shift: options
            .overview_shift
            .unwrap_or_else(|| overview::converter_finest(length)),
    };
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
            .filter_map(|(i, s)| s.next_tick().map(|t| (t, i)))
            .min();
        let Some((tick, c)) = next else {
            break;
        };
        match &mut streams[c] {
            Stream::Digital {
                edges,
                next,
                gaps,
                next_gap,
            } => {
                if let Some(g) = *next_gap
                    && g.from == tick
                    && next.is_none_or(|t| t > g.from)
                {
                    writer.gap(c, g)?;
                    *next_gap = gaps.next();
                } else {
                    writer.edge(c, tick)?;
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
        if let Stream::Analog { index, count, .. } = stream
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

/// Encodes `recording` into memory.
pub fn encode(recording: Recording, options: &Options) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::new();
    encode_to(recording, options, &mut out)?;
    Ok(out)
}
