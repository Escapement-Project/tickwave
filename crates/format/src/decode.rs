//! The random-access decoder. It does no I/O: a caller fetches the file's end, then the bytes of
//! the blocks it needs, by HTTP Range in the browser or from a file natively, and hands them in.

use std::ops::Range;

use crate::codec::get_uleb;
use crate::header::{
    AnalogCoding, BlockRow, ChannelHeader, ChannelKind, Declaration, DeclaredChannel, DeclaredKind,
    Header, JSON_SAFE, Kept, LEVEL_BIT, MAGIC, MAJOR, OverviewRow, PREAMBLE_LEN, TRAILER_LEN,
    Trailer, Words,
};
use crate::mapping::Mapping;
use crate::model::{Break, Gap, Ratio, run_sample_tick};
use crate::overview::{self, Column, Level};
use crate::record::{self, Kind};
use crate::samples::{self, Product, Values};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("not a tickwave recording")]
    NotTickwave,
    #[error(
        "no trailer at the end: a tickwave recording cut off or still being written, or not one at all"
    )]
    Unfinished,
    #[error("format version {major}.{minor}: this reader reads major version {MAJOR}")]
    Version { major: u16, minor: u16 },
    #[error("the footer starts at byte {0}: read the file from there to its end")]
    NeedFrom(u64),
    #[error("header: {0}")]
    Header(#[from] serde_json::Error),
    #[error("{0}")]
    Corrupt(String),
    #[error("zstd: {0}")]
    Zstd(String),
}

pub(crate) fn corrupt(what: impl Into<String>) -> DecodeError {
    DecodeError::Corrupt(what.into())
}

/// How many bytes from its end a reader reads first: enough for most footers.
pub const FIRST_READ: usize = 32 << 10;

/// An empty buffer with room for `len` decoded bytes, at most `max`. The length comes from the
/// file, so a reader refuses one it is not set to hold, or cannot allocate, rather than aborting:
/// in WASM an abort ends every recording on the page.
pub fn buffer(len: usize, max: usize) -> Result<Vec<u8>, DecodeError> {
    let too_big = || {
        corrupt(format!(
            "it decodes to {len} bytes, more than this reader holds ({max})"
        ))
    };
    if len > max {
        return Err(too_big());
    }
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| too_big())?;
    Ok(out)
}

/// The most a reader decodes from one frame unless it is set otherwise: a block, an overview block
/// or the footer's overview. The format sets no maximum, and a file can state up to 4 GiB.
pub const MAX_DECODED: usize = 64 << 20;

/// The most overview blocks tickwave writes, repairs or checks for one recording: their rows
/// take 16 MiB of the footer's index, a quarter of what an in-order reader holds of one record
/// (`MAX_DECODED`). Settling them costs a span about a third of a second natively; the format
/// would allow four times as many, at four times the cost.
pub(crate) const MAX_OVERVIEW_BLOCKS: u64 = (MAX_DECODED / OverviewRow::SIZE / 4) as u64;

/// A recording reaching axis tick `end`, its finest overview level `finest`, whose overview
/// blocks pass `MAX_OVERVIEW_BLOCKS`.
#[derive(Debug)]
pub(crate) struct OverviewPast {
    pub end: i64,
    pub finest: u8,
    pub blocks: u64,
}

impl std::fmt::Display for OverviewPast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            end,
            finest,
            blocks,
        } = self;
        write!(
            f,
            "its finest overview level, of 2^{finest}-tick buckets, needs {blocks} overview blocks \
             to reach tick {end}, more than tickwave keeps ({MAX_OVERVIEW_BLOCKS})"
        )
    }
}

/// Whether tickwave keeps the overview of a recording reaching axis tick `end`, its finest
/// overview level `finest`: the overview blocks the format sets for it are no more than
/// `MAX_OVERVIEW_BLOCKS`. Settling the overview costs time and memory for each of those blocks,
/// so whatever settles it, for a span a file or a caller states, is held to this first.
pub(crate) fn overview_room(end: i64, finest: u8) -> Result<(), OverviewPast> {
    let plan = overview::plan(end, finest);
    let blocks: u64 = plan
        .blocks
        .iter()
        .map(|&shift| overview::block_count(end, shift))
        .sum();
    if blocks > MAX_OVERVIEW_BLOCKS {
        return Err(OverviewPast {
            end,
            finest,
            blocks,
        });
    }
    Ok(())
}

/// Decompresses exactly one zstd frame, which must fill `frame`, come to exactly `len` bytes and
/// carry a checksum that matches. A skippable frame, a frame that names a dictionary, or any byte
/// after the frame is refused, and so is a `len` past `MAX_DECODED`.
pub fn inflate(frame: &[u8], len: usize) -> Result<Vec<u8>, DecodeError> {
    inflate_limited(frame, len, MAX_DECODED)
}

/// `inflate`, holding at most `max` decoded bytes. libzstd checks the frame's checksum.
pub fn inflate_limited(frame: &[u8], len: usize, max: usize) -> Result<Vec<u8>, DecodeError> {
    use zstd::zstd_safe;
    let mut out = buffer(len, max)?;
    // RFC 8878: the magic number, then the frame header descriptor, whose bit 2 says the frame
    // ends in a checksum and whose bits 0-1 give the size of a dictionary id. libzstd would read
    // a skippable frame as one holding nothing, and frames without a checksum.
    if frame.get(..4) != Some(&0xFD2F_B528u32.to_le_bytes()[..]) {
        return Err(corrupt("the bytes are not one zstd frame"));
    }
    let Some(&descriptor) = frame.get(4) else {
        return Err(corrupt("a frame carries no checksum"));
    };
    if descriptor & 0b100 == 0 {
        return Err(corrupt("a frame carries no checksum"));
    }
    if descriptor & 0b11 != 0 {
        return Err(corrupt("a frame names a dictionary"));
    }
    match zstd_safe::find_frame_compressed_size(frame) {
        Ok(n) if n == frame.len() => {}
        Ok(_) => return Err(corrupt("the bytes hold more than one frame")),
        Err(code) => return Err(DecodeError::Zstd(zstd_safe::get_error_name(code).into())),
    }
    zstd::bulk::Decompressor::new()
        .and_then(|mut d| d.decompress_to_buffer(frame, &mut out))
        .map_err(|e| DecodeError::Zstd(e.to_string()))?;
    if out.len() != len {
        return Err(corrupt(format!(
            "a frame decoded to {} bytes, not {len}",
            out.len()
        )));
    }
    Ok(out)
}

/// One overview block, decoded: buckets `first..first + level.buckets` of its level.
#[derive(Debug)]
pub struct OverviewBlock {
    pub first: u64,
    pub level: Level,
}

#[derive(Debug, PartialEq)]
pub enum BlockChannel {
    Digital {
        /// The level at the block's start.
        initial: bool,
        /// The edges' axis ticks, in order.
        edges: Vec<i64>,
        /// The edges' source ticks, as the file stores them; None where the channel's mapping
        /// is the identity, so they are `edges`.
        source: Option<Vec<i64>>,
    },
    Analog {
        /// The channel's index of the block's first sample.
        first_index: u64,
        values: Values,
    },
}

impl BlockChannel {
    /// An analog channel's samples; None for a digital one.
    pub fn values(&self) -> Option<&Values> {
        match self {
            Self::Analog { values, .. } => Some(values),
            Self::Digital { .. } => None,
        }
    }

    /// A digital channel's edges in its source's ticks; None for an analog one.
    pub fn source_ticks(&self) -> Option<&[i64]> {
        match self {
            Self::Digital { edges, source, .. } => Some(source.as_deref().unwrap_or(edges)),
            Self::Analog { .. } => None,
        }
    }
}

/// A derived channel's samples in a block, from its operands' there: `a` and `b` index the
/// block's channels, and `product` is the rule (`overview::products` gives each derived
/// channel's). The browser and the native views take derived samples from here, so both follow
/// one rule.
pub fn derived_values(
    channels: &[BlockChannel],
    (a, b, product): (usize, usize, Product),
) -> Result<Values, String> {
    let (
        Some(BlockChannel::Analog {
            first_index: fa,
            values: va,
        }),
        Some(BlockChannel::Analog {
            first_index: fb,
            values: vb,
        }),
    ) = (channels.get(a), channels.get(b))
    else {
        return Err("a derived channel's operands are analog channels".into());
    };
    if fa != fb {
        return Err(format!(
            "its operands' samples start at index {fa} and {fb} in the block"
        ));
    }
    samples::product_values(product, va, vb)
}

#[derive(Debug, PartialEq)]
pub struct Block {
    pub start: i64,
    /// Exclusive: the next block's start, or the length for the last block.
    pub end: i64,
    pub channels: Vec<BlockChannel>,
}

/// The place of the last of a run's `n` samples, the first at `tick`, in source ticks, in u128 so
/// that an absurd count or period cannot wrap into a tick inside the capture. `tick` is not
/// negative.
fn run_last(tick: i64, period: Ratio, n: u64) -> u128 {
    tick as u128
        + u128::from(n.saturating_sub(1)) * u128::from(period.num()) / u128::from(period.den())
}

/// A source tick a channel names: in `0..2^53` (FORMAT.md, "Times").
fn source_tick(k: i64) -> bool {
    (0..JSON_SAFE as i64).contains(&k)
}

/// Whether source tick `k` lies in `0..2^53` and maps into `0..length`.
fn maps_inside(map: &Mapping, k: i64, length: i64) -> bool {
    source_tick(k) && map.axis(k).is_some_and(|t| (0..length).contains(&t))
}

/// Whether source tick `k` lies in `0..2^53` and maps into `0..=length`: a channel's end, or a
/// kept span's.
fn maps_up_to(map: &Mapping, k: i64, length: i64) -> bool {
    source_tick(k) && map.axis(k).is_some_and(|t| (0..=length).contains(&t))
}

/// Two rates of the same value, however they are written.
fn same_rate(a: Ratio, b: Ratio) -> bool {
    u128::from(a.num()) * u128::from(b.den()) == u128::from(b.num()) * u128::from(a.den())
}

/// What a declaration states, which the footer's header repeats: FORMAT.md's rules on its sources,
/// codings, derived channels and kept spans, checked before anything relies on them.
pub(crate) fn check_declaration(d: &Declaration) -> Result<(), DecodeError> {
    // The page reads the header with JSON.parse: past 2^53 it would read another number.
    let safe = |v: u64| v < JSON_SAFE;
    let within = |v: i64| v.unsigned_abs() < JSON_SAFE;
    let terms = |r: Ratio| safe(r.num()) && safe(r.den());
    if !terms(d.timebase) {
        return Err(corrupt("the timebase has a term past 2^53"));
    }
    if d.overview_shift > overview::MAX_SHIFT {
        return Err(corrupt("the finest overview level's shift is past 62"));
    }
    for (s, source) in d.sources.iter().enumerate() {
        if !terms(source.timebase) {
            return Err(corrupt(format!(
                "source {s}'s timebase has a term past 2^53"
            )));
        }
        if source.stretch.is_some_and(|r| !terms(r)) {
            return Err(corrupt(format!(
                "source {s}'s stretch has a term past 2^53"
            )));
        }
        if d.m(s).is_none() {
            return Err(corrupt(format!(
                "source {s}'s m, the axis ticks in one of its ticks, has a term past 2^53 in \
                 lowest terms"
            )));
        }
        if !within(source.origin) || source.align.is_some_and(|a| !within(a)) {
            return Err(corrupt(format!(
                "source {s}'s origin or align is past 2^53"
            )));
        }
        // JSON holds no NaN or infinity: a writer would write it as null.
        if [source.begin_s, source.trigger_s]
            .iter()
            .flatten()
            .any(|t| !t.is_finite())
        {
            return Err(corrupt(format!(
                "source {s}'s begin_s or trigger_s is not a finite number"
            )));
        }
        if !d.channels.iter().any(|c| c.source as usize == s) {
            return Err(corrupt(format!("source {s} is no channel's source")));
        }
    }
    if let [only] = d.sources.as_slice()
        && !same_rate(d.timebase, only.timebase)
    {
        return Err(corrupt(format!(
            "the recording has one source, ticking {}/{} times a second, and ticks {}/{} itself: \
             a recording of one source ticks at its rate",
            only.timebase.num(),
            only.timebase.den(),
            d.timebase.num(),
            d.timebase.den()
        )));
    }
    for (i, c) in d.channels.iter().enumerate() {
        let name = &c.name;
        if d.channels[..i].iter().any(|e| e.name == *name) {
            return Err(corrupt(format!(
                "channel {name}: another channel has its name"
            )));
        }
        if c.source as usize >= d.sources.len() {
            return Err(corrupt(format!(
                "channel {name}'s source is {}, but the recording lists {} sources",
                c.source,
                d.sources.len()
            )));
        }
        if let Some(offset) = &c.shaping.offset
            && !within(offset.ticks)
        {
            return Err(corrupt(format!("channel {name}'s offset is past 2^53")));
        }
        let DeclaredKind::Analog {
            period,
            coding,
            significant,
            ..
        } = &c.data
        else {
            continue;
        };
        if *significant == Some(0) {
            return Err(corrupt(format!(
                "channel {name} states 0 significant figures; it states at least 1"
            )));
        }
        if !terms(*period) {
            return Err(corrupt(format!("channel {name}'s period is past 2^53")));
        }
        match coding {
            AnalogCoding::F32 | AnalogCoding::F64 => {}
            AnalogCoding::Codes {
                bits,
                scale,
                offset,
            } => {
                if !(1..=16).contains(bits) {
                    return Err(corrupt(format!(
                        "channel {name}'s codes are {bits} bits; a codes coding holds 1 to 16"
                    )));
                }
                if !scale.is_finite() || !offset.is_finite() {
                    return Err(corrupt(format!(
                        "channel {name}'s codes scale or offset is not a finite number"
                    )));
                }
            }
            AnalogCoding::Int(int) => {
                if !(1..=64).contains(&int.bits) {
                    return Err(corrupt(format!(
                        "channel {name}'s codes are {} bits; an int coding holds 1 to 64",
                        int.bits
                    )));
                }
                if int.scale.is_some_and(|s| s.num() == 0) {
                    return Err(corrupt(format!("channel {name}'s scale is zero")));
                }
                for (what, ratio) in [("scale", int.scale), ("offset", int.offset)] {
                    if ratio.is_some_and(|r| !within(r.num()) || !within(r.den())) {
                        return Err(corrupt(format!(
                            "channel {name}'s {what} has a term past 2^53"
                        )));
                    }
                }
            }
        }
    }
    for (i, derived) in d.derived.iter().enumerate() {
        let name = &derived.name;
        if d.channels.iter().any(|c| c.name == *name)
            || d.derived[..i].iter().any(|e| e.name == *name)
        {
            return Err(corrupt(format!(
                "derived channel {name}: another channel has its name"
            )));
        }
        if derived.significant == Some(0) {
            return Err(corrupt(format!(
                "derived channel {name} states 0 significant figures; it states at least 1"
            )));
        }
        let mut operands = Vec::with_capacity(2);
        for operand in &derived.product {
            let Some(c) = d
                .channels
                .iter()
                .find(|c| c.name == *operand && c.coding().is_some())
            else {
                return Err(corrupt(format!(
                    "derived channel {name}: no analog channel {operand}"
                )));
            };
            operands.push(c);
        }
        let (a, b) = (operands[0], operands[1]);
        let period = |c: &DeclaredChannel| match &c.data {
            DeclaredKind::Analog { period, .. } => Some(*period),
            DeclaredKind::Digital { .. } => None,
        };
        let moved = |c: &DeclaredChannel| c.shaping.offset.as_ref().map_or(0, |o| o.ticks);
        if a.source != b.source || period(a) != period(b) || moved(a) != moved(b) {
            return Err(corrupt(format!(
                "derived channel {name}: {} and {} come from different sources, or have \
                 different periods or moves",
                a.name, b.name
            )));
        }
    }
    // Once anything is moved or cut, or there are several sources, or a source is stretched,
    // every channel states its kept span, and the recording starts where the earliest one does.
    let stretched = d
        .sources
        .iter()
        .any(|s| s.stretch.is_some_and(|r| r.num() != r.den()));
    if d.channels.iter().any(|c| c.shaping.kept.is_some()) || d.sources.len() > 1 || stretched {
        if let Some(c) = d.channels.iter().find(|c| c.shaping.kept.is_none()) {
            return Err(corrupt(if d.sources.len() > 1 {
                format!(
                    "channel {} states no kept span, but the recording has several sources: \
                     a recording of several sources states one on every channel",
                    c.name
                )
            } else if stretched {
                format!(
                    "channel {} states no kept span, but its source is stretched: a recording \
                     with a stretched source states one on every channel",
                    c.name
                )
            } else {
                format!(
                    "channel {} states no kept span, but other channels do: a recording states \
                     one on every channel or on none",
                    c.name
                )
            }));
        }
        let starts_at_zero =
            (0..d.channels.len()).any(|c| match (d.mapping(c), d.channels[c].shaping.kept) {
                (Some(map), Some(kept)) => map.axis(kept.from) == Some(0),
                _ => false,
            });
        if !starts_at_zero {
            return Err(corrupt(
                "no channel's kept span starts at the axis's tick 0, where the recording starts",
            ));
        }
    } else {
        if let Some(c) = d.channels.iter().find(|c| c.shaping.offset.is_some()) {
            return Err(corrupt(format!(
                "channel {} is moved, but no channel states a kept span: a recording with a \
                 channel moved states one on every channel",
                c.name
            )));
        }
        if let [only] = d.sources.as_slice()
            && only.origin > 0
        {
            return Err(corrupt(format!(
                "the recording states no kept span, so it starts where its source's data does, \
                 but its source's origin is {}, after the axis's tick 0",
                only.origin
            )));
        }
    }
    Ok(())
}

/// What the header promises about the capture's extent, checked before anything relies on it.
pub(crate) fn check_header(h: &Header) -> Result<(), DecodeError> {
    if !(1..JSON_SAFE as i64).contains(&h.length) {
        return Err(corrupt("the length covers no tick or is past 2^53"));
    }
    check_declaration(&h.declaration())?;
    let length = h.length;
    let safe = |v: u64| v < JSON_SAFE;
    for (c, ch) in h.channels.iter().enumerate() {
        let name = &ch.name;
        let unmapped = || corrupt(format!("channel {name} has no mapping to the axis"));
        let map = h.mapping(c).ok_or_else(unmapped)?;
        if let Some(Kept { from, to }) = ch.shaping.kept
            && (from >= to || !maps_inside(&map, from, length) || !maps_up_to(&map, to, length))
        {
            return Err(corrupt(format!(
                "channel {name} is kept from source tick {from} to {to}, which is empty or maps \
                 outside the axis's 0 to {length}"
            )));
        }
        match &ch.data {
            ChannelKind::Digital {
                initial,
                end,
                edges,
                gaps,
            } => {
                let start = h.start(c).ok_or_else(unmapped)?;
                if !source_tick(start) {
                    return Err(corrupt(format!(
                        "channel {name} starts at source tick {start}, outside 0 to 2^53"
                    )));
                }
                if *end < start || !maps_up_to(&map, *end, length) {
                    return Err(corrupt(format!("channel {name} ends outside the capture")));
                }
                if *end == start && *initial {
                    return Err(corrupt(format!(
                        "channel {name} is never known but has a high initial level"
                    )));
                }
                if !safe(*edges) {
                    return Err(corrupt(format!("channel {name}'s edge count is past 2^53")));
                }
                let mut after = start - 1;
                for g in gaps {
                    if g.from <= after
                        || g.to <= g.from
                        || g.to >= *end
                        || !maps_inside(&map, g.from, length)
                        || !maps_inside(&map, g.to, length)
                    {
                        return Err(corrupt(format!(
                            "channel {name}: a gap from {} to {} is out of place",
                            g.from, g.to
                        )));
                    }
                    if g.from == start && *initial {
                        return Err(corrupt(format!(
                            "channel {name} has a gap from its start but a high initial level"
                        )));
                    }
                    after = g.to;
                }
            }
            ChannelKind::Analog {
                first,
                period,
                count,
                breaks,
                ..
            } => {
                if !safe(*count) {
                    return Err(corrupt(format!("channel {name}'s count is past 2^53")));
                }
                let outside =
                    || corrupt(format!("channel {name}'s samples lie outside the capture"));
                if *count == 0 {
                    // With no sample, `first` places nothing.
                    if !breaks.is_empty() {
                        return Err(corrupt(format!("channel {name}: a break is out of place")));
                    }
                } else {
                    if !maps_inside(&map, *first, length) {
                        return Err(outside());
                    }
                    // Each run's last sample: at or before the next run's tick, and inside the
                    // capture.
                    let mut run = Break {
                        index: 0,
                        tick: *first,
                        lost: 0,
                    };
                    for b in breaks {
                        if b.index <= run.index
                            || b.index >= *count
                            || !maps_inside(&map, b.tick, length)
                        {
                            return Err(corrupt(format!(
                                "channel {name}: a break is out of place"
                            )));
                        }
                        if !safe(b.lost) {
                            return Err(corrupt(format!(
                                "channel {name}: a break's lost count is past 2^53"
                            )));
                        }
                        if run_last(run.tick, *period, b.index - run.index) > b.tick as u128 {
                            return Err(outside());
                        }
                        run = *b;
                    }
                    let last = run_last(run.tick, *period, count - run.index);
                    if last >= u128::from(JSON_SAFE) || !maps_inside(&map, last as i64, length) {
                        return Err(outside());
                    }
                }
            }
        }
        check_shaping(ch)?;
    }
    // A derived channel's sample i sits where its operands' do.
    for d in &h.derived {
        let extent = |operand: &String| {
            h.channels
                .iter()
                .find(|c| c.name == *operand)
                .and_then(|c| match &c.data {
                    ChannelKind::Analog {
                        first,
                        count,
                        breaks,
                        ..
                    } => Some((*first, *count, breaks)),
                    ChannelKind::Digital { .. } => None,
                })
        };
        if extent(&d.product[0]) != extent(&d.product[1]) {
            return Err(corrupt(format!(
                "derived channel {}: {} and {} differ in first, count or breaks",
                d.name, d.product[0], d.product[1]
            )));
        }
    }
    Ok(())
}

/// A moved or trimmed channel's statements, against its data: FORMAT.md's moved and trimmed
/// channels. `check_header` has checked the data itself, and the kept span's own bounds.
fn check_shaping(c: &ChannelHeader) -> Result<(), DecodeError> {
    let name = &c.name;
    let Some(Kept { from, to }) = c.shaping.kept else {
        return Ok(());
    };
    match &c.data {
        ChannelKind::Digital { end, .. } => {
            // The channel starts at `from`, and `check_header` held its edges and gaps to that.
            if *end > to {
                return Err(corrupt(format!(
                    "channel {name} is known to {end}, past where it is kept to, {to}"
                )));
            }
        }
        ChannelKind::Analog {
            first,
            period,
            count,
            breaks,
            ..
        } => {
            if *count == 0 {
                return Ok(());
            }
            let run = breaks.last().copied().unwrap_or(Break {
                index: 0,
                tick: *first,
                lost: 0,
            });
            let last = run_last(run.tick, *period, count - run.index);
            if *first < from || last >= to as u128 {
                // As an i64: formatting a u128 costs the browser's decoder more than this check.
                let last = i64::try_from(last).unwrap_or(i64::MAX);
                return Err(corrupt(format!(
                    "channel {name} is kept from {from} to {to}, but its samples run from {first} to {last}"
                )));
            }
        }
    }
    Ok(())
}

/// A channel's item count from its index word: a digital word keeps the level in its top bit, and
/// an analog word is all count.
pub(crate) fn item_count(word: u32, digital: bool) -> u32 {
    if digital { word & !LEVEL_BIT } else { word }
}

/// How to read one channel's section of a block.
pub(crate) enum Section {
    /// Every edge lies in `start..end` of the channel's source ticks, `end` `i64::MAX` while it
    /// is not known, and `map` places it on the axis.
    Digital { start: i64, end: i64, map: Mapping },
    Analog {
        coding: AnalogCoding,
        first_index: u64,
    },
}

/// A block's decoded payload, split into its channels' items.
pub(crate) fn decode_sections(
    raw: &[u8],
    sections: &[Section],
    words: &[u32],
    start: i64,
    end: i64,
) -> Result<Vec<BlockChannel>, String> {
    let channels = sections.len();
    let lengths: Vec<usize> = raw
        .get(..4 * channels)
        .ok_or("the block is too short for its section table")?
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&c| u32::from_le_bytes(c) as usize)
        .collect();
    // In u64: on a 32-bit usize the lengths could wrap to the block's size.
    if 4 * channels as u64 + lengths.iter().map(|&l| l as u64).sum::<u64>() != raw.len() as u64 {
        return Err("the block's sections do not fill it".into());
    }
    let mut pos = 4 * channels;
    let mut out = Vec::with_capacity(channels);
    for ((kind, len), &word) in sections.iter().zip(lengths).zip(words) {
        let section = &raw[pos..pos + len];
        pos += len;
        out.push(match kind {
            Section::Digital {
                start: from,
                end: to,
                map,
            } => {
                let (edges, source) = digital_edges(
                    section,
                    item_count(word, true) as usize,
                    map,
                    *from..*to,
                    start..end,
                )?;
                BlockChannel::Digital {
                    initial: word & LEVEL_BIT != 0,
                    edges,
                    source,
                }
            }
            Section::Analog {
                coding,
                first_index,
            } => BlockChannel::Analog {
                first_index: *first_index,
                values: analog_values(section, word as usize, *coding)?,
            },
        });
    }
    Ok(out)
}

/// The level after a block's edges and the gap ends in it, from `level` at its start. No edge
/// lies at a gap's `to`.
pub(crate) fn level_after(level: bool, edges: &[i64], gap_ends: &[Gap]) -> bool {
    let mut level = level;
    let mut e = 0;
    for g in gap_ends {
        while e < edges.len() && edges[e] < g.to {
            level = !level;
            e += 1;
        }
        level = g.level;
    }
    level ^ ((edges.len() - e) % 2 == 1)
}

/// The first edge of `edges` that lies inside one of `gaps`.
pub(crate) fn edge_in_gap(edges: &[i64], gaps: &[Gap]) -> Option<i64> {
    edges.iter().copied().find(|&t| {
        let i = gaps.partition_point(|g| g.from <= t);
        i > 0 && t <= gaps[i - 1].to
    })
}

/// The gaps of `gaps` whose `to` lies in `start..end` of the axis, where `ends` holds each gap's
/// `to` mapped to the axis.
pub(crate) fn gaps_ending_in<'a>(gaps: &'a [Gap], ends: &[i64], start: i64, end: i64) -> &'a [Gap] {
    let a = ends.partition_point(|&t| t < start).min(gaps.len());
    let b = ends.partition_point(|&t| t < end).min(gaps.len()).max(a);
    &gaps[a..b]
}

/// Each gap's `to` mapped to the axis; `i64::MAX` for one that maps nowhere, which a checked
/// header has none of.
pub(crate) fn gap_ends(gaps: &[Gap], map: &Mapping) -> Vec<i64> {
    gaps.iter()
        .map(|g| map.axis(g.to).unwrap_or(i64::MAX))
        .collect()
}

/// A finished file's footer: the header, the index and the overview. Parsed from the file's last
/// bytes, it says where every block is.
#[derive(Debug)]
pub struct Footer {
    pub trailer: Trailer,
    pub header: Header,
    pub rows: Vec<BlockRow>,
    /// Each block's channel words, beside its row.
    pub words: Words,
    pub overview: Vec<Level>,
    /// For each level in `header.overview_blocks`, its overview blocks' places in the file.
    pub overview_rows: Vec<Vec<OverviewRow>>,
    /// The footer's overview, as the file holds it: one zstd frame.
    pub overview_frame: Vec<u8>,
    /// Each derived channel's operands, by channel index, and its rule.
    pub products: Vec<(usize, usize, Product)>,
    /// For each analog channel, the index of its first sample in each block.
    first_sample: Vec<Vec<u64>>,
    columns: Vec<Column>,
    /// Each channel's mapping to the axis.
    maps: Vec<Mapping>,
    /// Each channel's start, in its source's ticks.
    channel_starts: Vec<i64>,
    /// Each digital channel's gaps' ends on the axis; empty for an analog channel.
    gap_ends: Vec<Vec<i64>>,
}

impl Footer {
    /// Parses the footer from the file's last bytes, `tail`, of a file `file_len` long.
    /// `NeedFrom` says where the footer starts when `tail` holds less of it.
    pub fn parse(tail: &[u8], file_len: u64) -> Result<Self, DecodeError> {
        Self::parse_with(tail, file_len, inflate)
    }

    /// Parses a whole file's footer.
    pub fn parse_file(file: &[u8]) -> Result<Self, DecodeError> {
        Self::parse_file_limited(file, MAX_DECODED)
    }

    /// `parse_file`, holding at most `max` decoded bytes a frame.
    pub fn parse_file_limited(file: &[u8], max: usize) -> Result<Self, DecodeError> {
        if !file.starts_with(&MAGIC) {
            return Err(DecodeError::NotTickwave);
        }
        Self::parse_with(file, file.len() as u64, |f, len| {
            inflate_limited(f, len, max)
        })
    }

    /// `parse` with another zstd decoder, which must do what this module's `inflate` does.
    pub fn parse_with(
        tail: &[u8],
        file_len: u64,
        inflate: impl Fn(&[u8], usize) -> Result<Vec<u8>, DecodeError>,
    ) -> Result<Self, DecodeError> {
        let tail_start = file_len
            .checked_sub(tail.len() as u64)
            .ok_or_else(|| corrupt("more bytes than the file holds"))?;
        if file_len < (PREAMBLE_LEN + TRAILER_LEN) as u64 {
            return Err(DecodeError::NotTickwave);
        }
        if tail.len() < TRAILER_LEN {
            return Err(DecodeError::NeedFrom(file_len - TRAILER_LEN as u64));
        }
        let trailer =
            Trailer::parse(&tail[tail.len() - TRAILER_LEN..]).ok_or(DecodeError::Unfinished)?;
        if trailer.major != MAJOR {
            return Err(DecodeError::Version {
                major: trailer.major,
                minor: trailer.minor,
            });
        }
        let footer_end = trailer
            .footer_offset
            .checked_add(u64::from(trailer.footer_len));
        if trailer.footer_offset < PREAMBLE_LEN as u64
            || footer_end != Some(file_len - TRAILER_LEN as u64)
        {
            return Err(corrupt(
                "the trailer's footer does not end where the trailer starts",
            ));
        }
        if trailer.footer_offset < tail_start {
            return Err(DecodeError::NeedFrom(trailer.footer_offset));
        }
        let at = (trailer.footer_offset - tail_start) as usize;
        let bytes = &tail[at..tail.len() - TRAILER_LEN];
        let payload = match record::next(bytes, usize::MAX) {
            record::Next::Record { kind, payload, len }
                if kind == Kind::Footer as u8 && len == bytes.len() =>
            {
                payload
            }
            _ => return Err(corrupt("the footer record is not whole")),
        };
        let u32_at = |at: usize| {
            payload
                .get(at..at + 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        };
        let (Some(header_len), Some(index_len)) = (u32_at(0), u32_at(4)) else {
            return Err(corrupt("the footer is too short for its lengths"));
        };
        let header_end = record::FOOTER_HEAD_LEN as u64 + u64::from(header_len);
        let index_end = header_end + u64::from(index_len);
        if index_end > payload.len() as u64 {
            return Err(corrupt("the footer's lengths do not fit it"));
        }
        let (header_end, index_end) = (header_end as usize, index_end as usize);
        let header: Header = serde_json::from_slice(&payload[record::FOOTER_HEAD_LEN..header_end])?;
        check_header(&header)?;
        // The levels follow from the length and the finest shift. Holding the header to them
        // bounds every level's size, and their count, before anything is allocated for them.
        let plan = overview::plan(header.length, header.overview_shift);
        if header.overview != plan.head
            || !header
                .overview_blocks
                .iter()
                .map(|l| l.shift)
                .eq(plan.blocks.iter().copied())
        {
            return Err(corrupt(
                "the overview levels are not the ones the format sets for this length",
            ));
        }
        for level in &header.overview_blocks {
            if u64::from(level.blocks) != overview::block_count(header.length, level.shift) {
                return Err(corrupt(format!(
                    "overview level 2^{} has {} blocks, not the {} its buckets need",
                    level.shift,
                    level.blocks,
                    overview::block_count(header.length, level.shift)
                )));
            }
        }
        let channels = header.channels.len();
        let row_size = BlockRow::size(channels);
        let overview_count: u64 = header
            .overview_blocks
            .iter()
            .map(|l| u64::from(l.blocks))
            .sum();
        // In u64: a corrupt header's counts must not wrap a 32-bit usize into a plausible length.
        if u64::from(index_len)
            != u64::from(header.blocks) * row_size as u64
                + overview_count * OverviewRow::SIZE as u64
        {
            return Err(corrupt("the index does not hold one row a block"));
        }
        let data_len = header.blocks as usize * row_size;
        let mut words = Words::with_capacity(channels, header.blocks as usize);
        let rows: Vec<BlockRow> = payload[header_end..header_end + data_len]
            .chunks_exact(row_size)
            .map(|row| BlockRow::parse(row, &mut words))
            .collect();
        let inside = |offset: u64, len: u32| {
            offset >= PREAMBLE_LEN as u64
                && offset
                    .checked_add(u64::from(len))
                    .is_some_and(|e| e <= trailer.footer_offset)
        };
        for (i, row) in rows.iter().enumerate() {
            let after_last =
                i == 0 || (row.start > rows[i - 1].start && row.offset > rows[i - 1].offset);
            if !after_last || row.start >= header.length || !inside(row.offset, row.length) {
                return Err(corrupt(format!("block {i}'s row is out of place")));
            }
        }
        if rows.first().is_none_or(|r| r.start != 0) {
            return Err(corrupt("the first block does not start at tick 0"));
        }
        let mut overview_rows = Vec::with_capacity(header.overview_blocks.len());
        let mut pos = header_end + data_len;
        for level in &header.overview_blocks {
            let mut level_rows = Vec::with_capacity(level.blocks as usize);
            for _ in 0..level.blocks {
                let row = OverviewRow::parse(&payload[pos..pos + OverviewRow::SIZE]);
                pos += OverviewRow::SIZE;
                if !inside(row.offset, row.length) {
                    return Err(corrupt("an overview block's row is out of place"));
                }
                level_rows.push(row);
            }
            overview_rows.push(level_rows);
        }
        let declaration = header.declaration();
        let columns = overview::columns(&declaration.channels, &declaration.derived);
        let products = overview::products(&declaration.channels, &declaration.derived);
        let overview_len: u64 = header
            .overview
            .iter()
            .map(|&shift| overview::decoded_len(overview::buckets(header.length, shift), &columns))
            .sum();
        let overview_len = usize::try_from(overview_len).map_err(|_| {
            corrupt(format!(
                "the overview decodes to {overview_len} bytes, more than this reader can hold"
            ))
        })?;
        let overview_frame = payload[index_end..].to_vec();
        let raw = inflate(&overview_frame, overview_len)?;
        let overview = overview::parse(&raw, &header.overview, header.length, &columns)
            .ok_or_else(|| corrupt("the overview does not match its levels"))?;
        let end_of = |i: usize| rows.get(i + 1).map_or(header.length, |r| r.start);
        let unmapped = || corrupt("a channel has no mapping to the axis");
        let maps = header.mappings().ok_or_else(unmapped)?;
        let channel_starts = (0..channels)
            .map(|c| header.start(c).ok_or_else(unmapped))
            .collect::<Result<Vec<i64>, _>>()?;
        let gap_ends: Vec<Vec<i64>> = header
            .channels
            .iter()
            .zip(&maps)
            .map(|(c, map)| match &c.data {
                ChannelKind::Digital { gaps, .. } => gap_ends(gaps, map),
                ChannelKind::Analog { .. } => Vec::new(),
            })
            .collect();
        let mut first_sample = vec![Vec::new(); channels];
        for (c, kind) in header.channels.iter().enumerate() {
            let name = &kind.name;
            let mut total = 0u64;
            let mut starts = Vec::with_capacity(rows.len());
            // A digital channel's level at each block's start follows from `initial`, the edge
            // counts before it and the gap ends. Without decoding, a block holding a gap end says
            // nothing of the next one's level; `decode_block` checks that one.
            let mut level = match &kind.data {
                ChannelKind::Digital { initial, .. } => Some(*initial),
                ChannelKind::Analog { .. } => None,
            };
            for (i, row) in rows.iter().enumerate() {
                let word = words.of(i)[c];
                if let (Some(l), ChannelKind::Digital { gaps, .. }) = (&mut level, &kind.data) {
                    let bit = word & LEVEL_BIT != 0;
                    let known = i == 0
                        || gaps_ending_in(gaps, &gap_ends[c], rows[i - 1].start, row.start)
                            .is_empty();
                    if known && bit != *l {
                        return Err(corrupt(format!(
                            "channel {name}: block {i} starts at the wrong level"
                        )));
                    }
                    *l = bit ^ (word & 1 != 0);
                }
                starts.push(total);
                total += u64::from(item_count(word, level.is_some()));
            }
            let promised = match kind.data {
                ChannelKind::Digital { edges, .. } => edges,
                ChannelKind::Analog { count, .. } => count,
            };
            if total != promised {
                return Err(corrupt(format!(
                    "channel {name}: the index counts {total} items, the header {promised}"
                )));
            }
            if let ChannelKind::Analog {
                first,
                period,
                breaks,
                ..
            } = &kind.data
            {
                // A view fetches blocks by tick, so a sample in a block that does not cover its
                // tick would never be drawn.
                let map = maps[c];
                for (i, row) in rows.iter().enumerate() {
                    let (from, to) = (starts[i], starts.get(i + 1).copied().unwrap_or(total));
                    let tick = |index| map.axis(run_sample_tick(*first, *period, breaks, index));
                    if to > from
                        && (tick(from).is_none_or(|t| t < row.start)
                            || tick(to - 1).is_none_or(|t| t >= end_of(i)))
                    {
                        return Err(corrupt(format!(
                            "channel {name}: block {i}'s samples lie outside its ticks"
                        )));
                    }
                }
                first_sample[c] = starts;
            }
        }
        Ok(Self {
            trailer,
            header,
            rows,
            words,
            overview,
            overview_rows,
            overview_frame,
            products,
            first_sample,
            columns,
            maps,
            channel_starts,
            gap_ends,
        })
    }

    /// Derived channel `d`'s samples in `block`, one this footer decoded.
    pub fn derived(&self, block: &Block, d: usize) -> Result<Values, DecodeError> {
        let operands = *self
            .products
            .get(d)
            .ok_or_else(|| corrupt(format!("no derived channel {d}")))?;
        derived_values(&block.channels, operands).map_err(|what| {
            corrupt(format!(
                "derived channel {}: {what}",
                self.header.derived[d].name
            ))
        })
    }

    /// Channel `c`'s mapping from its source's ticks to the axis.
    pub fn mapping(&self, c: usize) -> Option<Mapping> {
        self.maps.get(c).copied()
    }

    /// The axis tick of sample `index` of analog channel `c`.
    pub fn sample_axis_tick(&self, c: usize, index: u64) -> Option<i64> {
        self.mapping(c)?.axis(self.sample_tick(c, index)?)
    }

    /// The source tick of sample `index` of analog channel `c`: its place.
    pub fn sample_tick(&self, c: usize, index: u64) -> Option<i64> {
        match &self.header.channels.get(c)?.data {
            ChannelKind::Analog {
                first,
                period,
                breaks,
                ..
            } => Some(run_sample_tick(*first, *period, breaks, index)),
            ChannelKind::Digital { .. } => None,
        }
    }

    /// Where overview block `k` of `header.overview_blocks[level]` sits: offset and length.
    pub fn overview_block_location(&self, level: usize, k: usize) -> (u64, u32) {
        let row = self.overview_rows[level][k];
        (row.offset, row.length)
    }

    /// The overview blocks of `header.overview_blocks[level]` holding any tick in `[start, end)`.
    pub fn overview_blocks_covering(&self, level: usize, start: i64, end: i64) -> Range<usize> {
        let shift = self.header.overview_blocks[level].shift;
        let last = self.header.length.max(1) - 1;
        // Every tick in 0..length falls in a block, so clamping keeps the range inside the level.
        let block =
            |tick: i64| ((tick.clamp(0, last) as u64 >> shift) / overview::BLOCK_BUCKETS) as usize;
        let first = block(start);
        first..block(end.max(start.saturating_add(1)) - 1) + 1
    }

    /// Decodes overview block `k` of `header.overview_blocks[level]` from exactly its bytes.
    pub fn decode_overview_block(
        &self,
        level: usize,
        k: usize,
        frame: &[u8],
    ) -> Result<OverviewBlock, DecodeError> {
        self.decode_overview_block_with(level, k, frame, inflate)
    }

    /// `decode_overview_block` with another zstd decoder, which must do what this module's
    /// `inflate` does.
    pub fn decode_overview_block_with(
        &self,
        level: usize,
        k: usize,
        frame: &[u8],
        inflate: impl Fn(&[u8], usize) -> Result<Vec<u8>, DecodeError>,
    ) -> Result<OverviewBlock, DecodeError> {
        let row = *self
            .overview_rows
            .get(level)
            .and_then(|rows| rows.get(k))
            .ok_or_else(|| corrupt(format!("no overview block {k} of level {level}")))?;
        if frame.len() != row.length as usize {
            return Err(corrupt(format!(
                "overview block {k} is {} bytes, the index says {}",
                frame.len(),
                row.length
            )));
        }
        let shift = self.header.overview_blocks[level].shift;
        let (first, n) = overview::block_buckets(self.header.length, shift, k as u64);
        if u64::from(row.decoded_length) != overview::decoded_len(n, &self.columns) {
            return Err(corrupt(format!(
                "overview block {k}'s decoded length is wrong"
            )));
        }
        let raw = inflate(frame, row.decoded_length as usize)?;
        let level = overview::parse_block(&raw, shift, n, &self.columns)
            .ok_or_else(|| corrupt(format!("overview block {k} does not parse")))?;
        Ok(OverviewBlock { first, level })
    }

    /// Where block `i` ends: the next block's start, or the length.
    pub fn block_end(&self, i: usize) -> i64 {
        self.rows.get(i + 1).map_or(self.header.length, |r| r.start)
    }

    /// The blocks holding any tick in `[start, end)`.
    pub fn blocks_covering(&self, start: i64, end: i64) -> Range<usize> {
        let first = self.rows.partition_point(|r| r.start <= start).max(1) - 1;
        let last = self.rows.partition_point(|r| r.start < end);
        first..last.max(first + 1).min(self.rows.len())
    }

    /// Where block `i`'s zstd frame sits in the file: offset and length.
    pub fn block_location(&self, i: usize) -> (u64, u32) {
        (self.rows[i].offset, self.rows[i].length)
    }

    /// Decodes block `i` from exactly its frame's bytes.
    pub fn decode_block(&self, i: usize, frame: &[u8]) -> Result<Block, DecodeError> {
        self.decode_block_with(i, frame, inflate)
    }

    /// `decode_block` with another zstd decoder. `inflate` must do what this module's `inflate`
    /// does: decode exactly `len` bytes and refuse a frame whose checksum is missing or wrong.
    pub fn decode_block_with(
        &self,
        i: usize,
        frame: &[u8],
        inflate: impl Fn(&[u8], usize) -> Result<Vec<u8>, DecodeError>,
    ) -> Result<Block, DecodeError> {
        let row = self
            .rows
            .get(i)
            .ok_or_else(|| corrupt(format!("no block {i}")))?;
        if frame.len() != row.length as usize {
            return Err(corrupt(format!(
                "block {i} is {} bytes, the index says {}",
                frame.len(),
                row.length
            )));
        }
        let raw = inflate(frame, row.decoded_length as usize)?;
        let (start, end) = (row.start, self.block_end(i));
        let sections: Vec<Section> = self
            .header
            .channels
            .iter()
            .enumerate()
            .map(|(c, kind)| match &kind.data {
                ChannelKind::Digital { end, .. } => Section::Digital {
                    start: self.channel_starts[c],
                    end: *end,
                    map: self.maps[c],
                },
                ChannelKind::Analog { coding, .. } => Section::Analog {
                    coding: *coding,
                    first_index: self.first_sample[c][i],
                },
            })
            .collect();
        let channels = decode_sections(&raw, &sections, self.words.of(i), start, end)
            .map_err(|what| corrupt(format!("block {i}: {what}")))?;
        for (c, (got, kind)) in channels.iter().zip(&self.header.channels).enumerate() {
            if let (BlockChannel::Digital { initial, .. }, ChannelKind::Digital { gaps, .. }) =
                (got, &kind.data)
            {
                let bad =
                    |what: String| corrupt(format!("block {i}, channel {}: {what}", kind.name));
                let edges = got.source_ticks().unwrap_or_default();
                if let Some(t) = edge_in_gap(edges, gaps) {
                    return Err(bad(format!("an edge at source tick {t} lies inside a gap")));
                }
                if i + 1 < self.rows.len() {
                    let ending = gaps_ending_in(gaps, &self.gap_ends[c], start, end);
                    let after = level_after(*initial, edges, ending);
                    if after != (self.words.of(i + 1)[c] & LEVEL_BIT != 0) {
                        return Err(bad("the next block starts at the wrong level".into()));
                    }
                }
            }
        }
        Ok(Block {
            start,
            end,
            channels,
        })
    }
}

// The two section decoders are kept out of line so that each loop compiles on its own: while they
// shared a function, a change to the analog loop moved the digital loop's speed by 5-7 %.

/// A digital section's `count` edges (FORMAT.md, "Blocks"): nothing for none; otherwise ULEB128
/// `step`, the first edge's source tick, then each later interval over `step`. Every edge lies in
/// `channel` of its source's ticks, and `map` puts it in `block` of the axis. The axis ticks, and
/// the source ticks unless `map` is the identity.
#[inline(never)]
fn digital_edges(
    section: &[u8],
    count: usize,
    map: &Mapping,
    channel: Range<i64>,
    block: Range<i64>,
) -> Result<(Vec<i64>, Option<Vec<i64>>), String> {
    if count == 0 {
        if !section.is_empty() {
            return Err("bytes in the section of a channel with no edge in the block".into());
        }
        return Ok((Vec::new(), None));
    }
    // Every number takes a byte at least: the step, then one an edge.
    if count >= section.len() {
        return Err("the section is too short for its edges".into());
    }
    let mut at = 0;
    let step = get_uleb(section, &mut at).ok_or("the section's step is cut short")?;
    if step == 0 {
        return Err("the section's step is 0".into());
    }
    let first = get_uleb(section, &mut at).ok_or("the first edge is cut short")?;
    let limit = JSON_SAFE;
    if first >= limit {
        return Err("an edge lies past source tick 2^53".into());
    }
    let mut ticks = Vec::with_capacity(count);
    let mut k = first;
    ticks.push(k as i64);
    let lo = first as i64;
    let past = || "an edge lies past source tick 2^53";
    if step == 1 {
        // Most sections: no multiply on the per-edge path.
        for _ in 1..count {
            let interval = get_uleb(section, &mut at).ok_or("an interval is cut short")?;
            k = k
                .checked_add(interval)
                .filter(|&k| k < limit)
                .ok_or_else(past)?;
            ticks.push(k as i64);
        }
    } else {
        for _ in 1..count {
            let interval = get_uleb(section, &mut at).ok_or("an interval is cut short")?;
            k = interval
                .checked_mul(step)
                .and_then(|d| k.checked_add(d))
                .filter(|&k| k < limit)
                .ok_or_else(past)?;
            ticks.push(k as i64);
        }
    }
    if at != section.len() {
        return Err("bytes follow the last edge".into());
    }
    let hi = k as i64;
    if lo < channel.start || hi >= channel.end {
        return Err("an edge lies outside its channel's start and end".into());
    }
    if map.is_identity() {
        if lo < block.start || hi >= block.end {
            return Err("an edge lies outside the block".into());
        }
        return Ok((ticks, None));
    }
    let mut edges = Vec::with_capacity(count);
    for &k in &ticks {
        match map.axis(k) {
            Some(t) if block.contains(&t) => edges.push(t),
            _ => return Err("an edge lies outside the block".into()),
        }
    }
    Ok((edges, Some(ticks)))
}

/// An analog section's `count` samples in `coding`.
#[inline(never)]
fn analog_values(section: &[u8], count: usize, coding: AnalogCoding) -> Result<Values, String> {
    samples::decode(section, count, coding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::{ChannelHeader, SourceFormat, SourceHeader};
    use crate::model::Ratio;

    fn header(breaks: Vec<Break>) -> Header {
        Header {
            timebase: Ratio::ONE,
            length: 100,
            channels: vec![ChannelHeader {
                name: "V".into(),
                source_name: "v".into(),
                source: 0,
                data: ChannelKind::Analog {
                    unit: String::new(),
                    first: 0,
                    period: Ratio::integer(10).unwrap(),
                    count: 5,
                    breaks,
                    coding: AnalogCoding::F32,
                    significant: None,
                },
                shaping: Default::default(),
            }],
            derived: Vec::new(),
            sources: vec![SourceHeader::new(SourceFormat::Vcd, Ratio::ONE)],
            title: None,
            meta: Default::default(),
            content_id: format!("blake3:{}", "0".repeat(64)).parse().unwrap(),
            blocks: 1,
            overview_shift: 12,
            overview: vec![12],
            overview_blocks: vec![],
            annotations: None,
        }
    }

    #[test]
    fn an_edge_at_either_end_of_a_gap_lies_in_it() {
        let gaps = [
            Gap {
                from: 5,
                to: 10,
                level: true,
            },
            Gap {
                from: 20,
                to: 30,
                level: false,
            },
        ];
        assert_eq!(edge_in_gap(&[4, 11, 19, 31], &gaps), None);
        for t in [5, 7, 10, 20, 30] {
            assert_eq!(edge_in_gap(&[1, t], &gaps), Some(t), "{t}");
        }
    }

    #[test]
    fn a_break_must_lie_inside_the_samples_and_the_capture() {
        let b = |index, tick| Break {
            index,
            tick,
            lost: 0,
        };
        assert!(check_header(&header(vec![])).is_ok());
        assert!(check_header(&header(vec![b(2, 50)])).is_ok());
        for (breaks, why) in [
            (
                vec![b(5, i64::MAX)],
                "an index at the count, with the largest tick",
            ),
            (vec![b(5, 60)], "an index at the count"),
            (vec![b(0, 60)], "index 0, which is the first run's"),
            (vec![b(3, 50), b(2, 70)], "indexes out of order"),
            (vec![b(2, 5)], "before the sample ahead of it, at 10"),
            (vec![b(2, 100)], "a tick at the length"),
            (
                vec![Break {
                    lost: 1 << 53,
                    ..b(2, 50)
                }],
                "a lost count past 2^53",
            ),
            (
                vec![b(2, 80)],
                "a run that ends past the length: 80, 90, 100",
            ),
        ] {
            assert!(check_header(&header(breaks)).is_err(), "{why}");
        }
    }

    fn uleb(values: &[u64]) -> Vec<u8> {
        let mut out = Vec::new();
        for &v in values {
            crate::codec::put_uleb(&mut out, v);
        }
        out
    }

    /// A digital section's numbers, as `digital_edges` reads them: the step, the first edge's
    /// source tick, then each later interval over the step.
    #[test]
    fn a_digital_section_reads_its_step_and_refuses_what_breaks_it() {
        let map = Mapping::new(1000, -3, Ratio::new(500_617, 4_000).unwrap());
        let read = |values: &[u64], count: usize, block: Range<i64>| {
            digital_edges(&uleb(values), count, &map, 0..20_000, block)
        };
        // Step 8: ticks 16, 16, 40, 64; on the axis 1000 + floor((k - 3) × 125.15425).
        let (axis, source) = read(&[8, 16, 0, 3, 3], 4, 0..10_000).unwrap();
        assert_eq!(source, Some(vec![16, 16, 40, 64]));
        assert_eq!(axis, [2627, 2627, 5630, 8634]);
        // The identity keeps one list: the edges are the source ticks.
        let (axis, source) =
            digital_edges(&uleb(&[8, 16, 0, 3]), 3, &Mapping::IDENTITY, 0..100, 0..100).unwrap();
        assert_eq!((axis, source), (vec![16, 16, 40], None));
        // No edge: no byte.
        assert_eq!(read(&[], 0, 0..10).unwrap(), (Vec::new(), None));
        for (values, count, block, why) in [
            (&[0u64, 16][..], 1, 0..10_000, "the section's step is 0"),
            (&[1, 16, 1], 1, 0..10_000, "bytes follow the last edge"),
            (&[1, 16], 2, 0..10_000, "the section is too short"),
            (&[1], 1, 0..10_000, "the section is too short"),
            // The first edge maps to 2627, before a block from 2628.
            (
                &[8, 16, 0],
                2,
                2628..10_000,
                "an edge lies outside the block",
            ),
            // The last maps to 8634, at the end of a block to 8634.
            (
                &[8, 16, 0, 3, 3],
                4,
                0..8634,
                "an edge lies outside the block",
            ),
            (&[1, 1 << 53], 1, 0..10_000, "past source tick 2^53"),
            (&[u64::MAX, 1, 1, 1], 3, 0..10_000, "past source tick 2^53"),
            (
                &[1, 20_000],
                1,
                0..i64::MAX,
                "outside its channel's start and end",
            ),
        ] {
            let err = read(values, count, block).expect_err(why);
            assert!(err.contains(why), "{values:?}: {err}");
        }
        // Bytes in the section of a channel with no edge in the block.
        let err = read(&[1], 0, 0..10).expect_err("refused");
        assert!(err.contains("no edge in the block"), "{err}");
        // An edge before the channel's start.
        let err = digital_edges(&uleb(&[1, 4]), 1, &map, 5..100, 0..10_000).expect_err("refused");
        assert!(err.contains("outside its channel's start and end"), "{err}");
    }
}
