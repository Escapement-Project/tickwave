//! The encoder: a `model::Recording` in, a file out through any `Write`. It merges the channels'
//! items into axis order and hands them to a `Writer`, each in its source's ticks and promising
//! its tick, so a capture larger than memory streams through.

use std::collections::BTreeMap;
use std::io::Write;

use crate::header::{AnalogCoding, Declaration, DeclaredChannel, DeclaredKind, SourceHeader};
use crate::mapping::{self, Mapping, Spacing};
use crate::model::{Break, ChannelData, Edges, Gap, Grid, Recording, Sample, Samples};
use crate::samples::coding_name;
pub use crate::writer::{
    Bundle, Compression, Finish, Finished, WriteError, WriteOptions, Writer, code_of,
    generator_file,
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
    /// The zstd level for blocks and the strips.
    pub level: i32,
    /// The decoded size a channel's block aims for.
    pub block_bytes: usize,
    /// Where frames are compressed: by default other threads, for throughput, and the caller's
    /// in WASM, which has none to give.
    pub compression: Compression,
    pub title: Option<String>,
    pub meta: BTreeMap<String, String>,
    /// The `converter` of every source that doesn't state one.
    pub converter: Option<String>,
    /// A coding for each analog channel, by channel index. Absent or None stores the samples as
    /// their type has them; f32 samples can also go in `codes`.
    pub analog: Vec<Option<AnalogCoding>>,
    /// Every channel's finest overview level's shift; None for the one each channel's own
    /// density gives (`mapping::finest_shift`).
    pub overview_shift: Option<u8>,
    /// Generated annotations to write, each with its generator's name; their times are axis
    /// ticks.
    pub annotations: Vec<(String, serde_json::Map<String, serde_json::Value>)>,
    /// How to bundle the annotations (`Finish::bundle`).
    pub bundle: Option<Bundle>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            level: 19,
            block_bytes: 1 << 20,
            compression: if cfg!(target_family = "wasm") {
                Compression::Spread
            } else {
                Compression::Offload
            },
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
        grid: Grid,
        breaks: Vec<Break>,
        lost_before: u64,
    },
}

impl Stream {
    /// Where the next item lies on the axis, and a digital item's source tick: a gap's at its
    /// `from`, so it goes in before the edges after it. One that maps nowhere goes first, for
    /// the writer to refuse.
    fn next_item(&self, map: &Mapping) -> Option<(i64, i64)> {
        match self {
            Stream::Digital { next, next_gap, .. } => {
                let t = match (next, next_gap) {
                    (Some(t), Some(g)) => (*t).min(g.from),
                    (t, g) => t.or(g.map(|g| g.from))?,
                };
                Some((map.axis(t).unwrap_or(i64::MIN), t))
            }
            Stream::Analog {
                next,
                index,
                first,
                grid,
                breaks,
                ..
            } => next.map(|_| {
                let place = grid.sample_place(*first, breaks, *index);
                (map.axis_place(place, grid.d()).unwrap_or(i64::MIN), 0)
            }),
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
    let mut lost_after = BTreeMap::new();
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
                if a.lost_after > 0 {
                    lost_after.insert(i, a.lost_after);
                }
                let mut samples = a.samples.into_samples();
                let next = samples.next();
                // A grid past 2^16 is refused with the declaration.
                let grid = Grid::new(a.period, a.phase).unwrap_or(Grid::WHOLE);
                streams.push(Stream::Analog {
                    samples,
                    next,
                    index: 0,
                    count: a.count,
                    first: a.first,
                    grid,
                    breaks: a.breaks,
                    lost_before: a.lost_before,
                });
                DeclaredKind::Analog {
                    unit: a.unit,
                    period: a.period,
                    coding,
                    significant: a.significant,
                    phase: a.phase,
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
            overview_shift: None,
            requires: Vec::new(),
            codec: None,
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
        requires: Vec::new(),
    };
    // A channel whose source has no `m` maps nowhere: the writer refuses its declaration.
    let maps: Vec<Mapping> = (0..declaration.channels.len())
        .map(|c| declaration.mapping(c).unwrap_or(Mapping::IDENTITY))
        .collect();
    match options.overview_shift {
        Some(shift) => declaration.overview_shift = shift,
        None => {
            // Each channel's own shift; the recording's is the densest's, and a channel states
            // its own where it differs.
            let shifts: Vec<u8> = (0..declaration.channels.len())
                .map(|c| mapping::finest_shift(spacing(&declaration, &maps, &items, c)))
                .collect();
            declaration.overview_shift = shifts.iter().copied().min().unwrap_or(62);
            for (channel, shift) in declaration.channels.iter_mut().zip(shifts) {
                channel.overview_shift = (shift != declaration.overview_shift).then_some(shift);
            }
        }
    }
    let mut writer = Writer::new(
        declaration,
        WriteOptions {
            level: options.level,
            block_bytes: options.block_bytes,
            promise_with_items: true,
            compression: options.compression,
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
            .filter_map(|(i, s)| s.next_item(&maps[i]).map(|(axis, t)| (axis, t, i)))
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
                lost_before,
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
                    writer.run(c, *first, *lost_before)?;
                } else if let Ok(b) = breaks.binary_search_by_key(index, |b| b.index) {
                    let b = breaks[b];
                    writer.run_at(c, b.tick, b.phase, b.lost)?;
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
            lost_after,
            ..Finish::default()
        },
    )?;
    out.write_all(&finished.tail)?;
    out.flush()?;
    Ok(finished)
}

/// Channel `c`'s spacing in axis ticks (FORMAT.md, "Overview"): an analog channel's period times
/// its source's `m`, a digital one's span from its start to its end over its edges. `items`
/// holds each channel's count, which the encoder holds it to, and a digital channel's end. None
/// for a channel with no item.
pub fn spacing(
    declaration: &Declaration,
    maps: &[Mapping],
    items: &[(u64, i64)],
    c: usize,
) -> Option<Spacing> {
    let channel = declaration.channels.get(c)?;
    let &(count, end) = items.get(c)?;
    if count == 0 {
        return None;
    }
    match &channel.data {
        DeclaredKind::Analog { period, .. } => {
            let m = declaration.m(channel.source as usize)?;
            Some(Spacing::of_samples(*period, m))
        }
        DeclaredKind::Digital { .. } => {
            let start = declaration.start(c)?;
            let span = maps[c].axis(end)?.checked_sub(maps[c].axis(start)?)?;
            Spacing::of_edges(u64::try_from(span).ok()?.max(1), count)
        }
    }
}

/// Encodes `recording` into memory.
pub fn encode(recording: Recording, options: &Options) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::new();
    encode_to(recording, options, &mut out)?;
    Ok(out)
}
