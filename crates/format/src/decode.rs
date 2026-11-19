//! The random-access decoder. It does no I/O: a caller fetches the file's end, then the bytes a
//! view needs, by HTTP Range in the browser or from a file natively, and hands them in. The
//! footer holds the header, the top index and every channel's strip; a channel's pages, blocks
//! and overview blocks are fetched by the places the top index and its pages give.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::codec::get_uleb;
use crate::header::{
    AnalogCoding, BlockRow, ChannelHeader, ChannelKind, Declaration, DeclaredChannel, DeclaredKind,
    Header, JSON_SAFE, Kept, LEVEL_BIT, MAGIC, MAJOR, MAX_SIGNIFICANT, OverviewLevel, OverviewRow,
    PREAMBLE_LEN, TRAILER_LEN, Trailer,
};
use crate::index::{Entry, Located, PAGE, Page, TOP, Table, TableIndex, depth_of, pages_at};
use crate::mapping::Mapping;
use crate::model::{Break, Gap, Grid, Ratio};
use crate::overview::{self, BLOCK_BUCKETS, Column, Level};
use crate::record::{
    self, Kind, MAX_BLOCK, MAX_CHANNELS, MAX_DECLARATION, MAX_FOOTER, compress_bound,
};
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
    /// A part of the file within the format's limits that this reader can't hold: more than it
    /// is set to, or more than it can allocate.
    #[error("{0}")]
    Resource(String),
}

pub(crate) fn corrupt(what: impl Into<String>) -> DecodeError {
    DecodeError::Corrupt(what.into())
}

/// What a section decoder's message starts with where it can't allocate.
pub(crate) const CANT_HOLD: &str = "this reader can't hold";

/// The most a repair, a re-cut or verify holds of what it decoded or made while it can't yet tell
/// whether it stands or where it goes.
pub const HOLD: usize = 4 * MAX_DECODED;

/// How many bytes from its end a reader reads first: enough for most footers.
pub const FIRST_READ: usize = 32 << 10;

/// An empty buffer with room for `len` decoded bytes, at most `max`. The length comes from the
/// file, so a reader refuses one it is not set to hold, or cannot allocate, rather than aborting:
/// in WASM an abort ends every recording on the page.
pub fn buffer(len: usize, max: usize) -> Result<Vec<u8>, DecodeError> {
    if len > max {
        return Err(DecodeError::Resource(format!(
            "it decodes to {len} bytes, more than this reader holds ({max})"
        )));
    }
    let mut out = Vec::new();
    out.try_reserve_exact(len)
        .map_err(|_| DecodeError::Resource(format!("{CANT_HOLD} the {len} bytes it decodes to")))?;
    Ok(out)
}

/// The most a reader decodes from one frame unless it is set otherwise: the most any frame the
/// format allows decodes to, an annotation bundle's 64 MiB (FORMAT.md, "Limits").
pub const MAX_DECODED: usize = record::MAX_BUNDLE;

/// Decompresses exactly one zstd frame, which must fill `frame`, come to exactly `len` bytes and
/// carry a checksum that matches. A skippable frame, a frame that names a dictionary, or any byte
/// after the frame is refused, and so is a `len` past `MAX_DECODED`.
pub fn inflate(frame: &[u8], len: usize) -> Result<Vec<u8>, DecodeError> {
    inflate_limited(frame, len, MAX_DECODED)
}

/// `inflate`, holding at most `max` decoded bytes. libzstd checks the frame's checksum.
pub fn inflate_limited(frame: &[u8], len: usize, max: usize) -> Result<Vec<u8>, DecodeError> {
    use zstd::zstd_safe;
    if frame.len() > compress_bound(len) {
        return Err(corrupt(format!(
            "a frame of {} bytes for {len} decoded bytes, more than zstd's bound for them",
            frame.len()
        )));
    }
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

/// The annotation bundle record's files: a u32 decoded length, then one zstd frame of a JSON
/// object of strings. A generator's file that is not its first line and its newline, or whose
/// first line names a key twice, is refused.
pub fn parse_bundle(
    payload: &[u8],
    inflate: impl Fn(&[u8], usize) -> Result<Vec<u8>, DecodeError>,
) -> Result<BTreeMap<String, String>, DecodeError> {
    let (len, frame) = payload
        .split_first_chunk::<4>()
        .ok_or_else(|| corrupt("the annotation bundle is too short for its length"))?;
    let raw = inflate(frame, u32::from_le_bytes(*len) as usize)?;
    let files: BTreeMap<String, String> =
        serde_json::from_slice(&raw).map_err(|e| corrupt(format!("the annotation bundle: {e}")))?;
    // A generator's file holds its first line alone, with its newline: its lines are in its
    // parts.
    for (name, text) in &files {
        if crate::parts::head_source(text).is_none() {
            continue;
        }
        let Some((first, rest)) = text.split_once('\n') else {
            return Err(corrupt(format!(
                "the annotation bundle's {name:?}: its first line lacks its newline"
            )));
        };
        if !rest.is_empty() {
            return Err(corrupt(format!(
                "the annotation bundle's {name:?} holds more than its generator's first line"
            )));
        }
        if let Ok(Some(key)) = crate::parts::line_object(first) {
            return Err(corrupt(format!(
                "the annotation bundle's {name:?}: its first line names key {key:?} twice"
            )));
        }
    }
    Ok(files)
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

    /// Its items: edges or samples.
    pub fn len(&self) -> usize {
        match self {
            Self::Digital { edges, .. } => edges.len(),
            Self::Analog { values, .. } => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One channel's block, decoded: its items over `start..end` of the axis.
#[derive(Debug, PartialEq)]
pub struct Block {
    pub channel: usize,
    pub start: i64,
    /// Exclusive: the channel's next block's start, or the length for its last.
    pub end: i64,
    pub data: BlockChannel,
}

/// A derived channel's samples from its operands' samples `a` and `b`, which start at the same
/// index and hold as many: `product` is the rule (`overview::products` gives each derived
/// channel's). The browser and the native views take derived samples from here, so both follow
/// one rule.
pub fn derived_values(
    a: &BlockChannel,
    b: &BlockChannel,
    product: Product,
) -> Result<Values, String> {
    let (
        BlockChannel::Analog {
            first_index: fa,
            values: va,
        },
        BlockChannel::Analog {
            first_index: fb,
            values: vb,
        },
    ) = (a, b)
    else {
        return Err("a derived channel's operands are analog channels".into());
    };
    if fa != fb {
        return Err(format!(
            "its operands' samples start at index {fa} and {fb}"
        ));
    }
    samples::product_values(product, va, vb)
}

/// A source tick a channel names: in `0..2^53` (FORMAT.md, "Times").
fn source_tick(k: i64) -> bool {
    (0..JSON_SAFE as i64).contains(&k)
}

/// Whether a place of `units / d` source ticks lies at a source time in `0..2^53` and maps into
/// `0..length`: every sample's place.
fn place_inside(map: &Mapping, units: i128, d: u64, length: i64) -> bool {
    units >= 0
        && units.div_euclid(i128::from(d)) < i128::from(JSON_SAFE)
        && map
            .axis_place(units, d)
            .is_some_and(|t| (0..length).contains(&t))
}

/// Whether such a place maps into `0..=length`: the end of a loss after the last sample.
fn place_up_to(map: &Mapping, units: i128, d: u64, length: i64) -> bool {
    units >= 0
        && units.div_euclid(i128::from(d)) < i128::from(JSON_SAFE)
        && map
            .axis_place(units, d)
            .is_some_and(|t| (0..=length).contains(&t))
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
    // This version defines no feature a recording can require.
    if !d.requires.is_empty() {
        return Err(corrupt(format!(
            "the recording requires {}, which this reader doesn't know",
            d.requires.join(", ")
        )));
    }
    if d.channels.len() + d.derived.len() > MAX_CHANNELS {
        return Err(corrupt(format!(
            "{} channels and {} derived channels, more than the {MAX_CHANNELS} a recording holds",
            d.channels.len(),
            d.derived.len()
        )));
    }
    if !terms(d.timebase) {
        return Err(corrupt("the timebase has a term past 2^53"));
    }
    if !d.timebase.is_lowest() {
        return Err(corrupt("the timebase is not in lowest terms"));
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
        if !source.timebase.is_lowest() || source.stretch.is_some_and(|r| !r.is_lowest()) {
            return Err(corrupt(format!(
                "source {s}'s timebase or stretch is not in lowest terms"
            )));
        }
        let (half, two) = (Ratio::new(1, 2).unwrap(), Ratio::integer(2).unwrap());
        if source
            .stretch
            .is_some_and(|r| r.less_than(half) || two.less_than(r))
        {
            return Err(corrupt(format!(
                "source {s}'s stretch lies outside 1/2 to 2"
            )));
        }
        let Some(m) = d.m(s) else {
            return Err(corrupt(format!(
                "source {s}'s m, the axis ticks in one of its ticks, has a term past 2^53 in \
                 lowest terms"
            )));
        };
        if m.less_than(half) {
            return Err(corrupt(format!(
                "source {s}'s m, the axis ticks in one of its ticks, is below 1/2: more than two \
                 of its ticks would share an axis tick"
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
        if c.overview_shift.is_some_and(|s| s > overview::MAX_SHIFT) {
            return Err(corrupt(format!(
                "channel {name}'s finest overview level's shift is past 62"
            )));
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
        if let Some(n) = significant.filter(|&n| n > MAX_SIGNIFICANT) {
            return Err(corrupt(format!(
                "channel {name} states {n} significant figures; it states at most \
                 {MAX_SIGNIFICANT}"
            )));
        }
        if !terms(*period) {
            return Err(corrupt(format!("channel {name}'s period is past 2^53")));
        }
        if !period.is_lowest() || period.num() < period.den() {
            return Err(corrupt(format!(
                "channel {name}'s period is not in lowest terms, or is below one tick"
            )));
        }
        if let DeclaredKind::Analog { phase: Some(f), .. } = &c.data
            && (!f.is_lowest() || f.num() >= f.den())
        {
            return Err(corrupt(format!(
                "channel {name}'s phase is not in lowest terms, or not below one tick"
            )));
        }
        if c.grid().is_none() {
            return Err(corrupt(format!(
                "channel {name}'s grid, the least common multiple of its phase's and period's \
                 denominators, is 2^16 or more"
            )));
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
                    if ratio.is_some_and(|r| !r.is_lowest()) {
                        return Err(corrupt(format!(
                            "channel {name}'s {what} is not in lowest terms"
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
        if let Some(n) = derived.significant.filter(|&n| n > MAX_SIGNIFICANT) {
            return Err(corrupt(format!(
                "derived channel {name} states {n} significant figures; it states at most \
                 {MAX_SIGNIFICANT}"
            )));
        }
        // Checked of every derived channel, whether or not this version computes it.
        if derived.operands.is_empty() {
            return Err(corrupt(format!("derived channel {name} names no operand")));
        }
        let mut operands = Vec::with_capacity(derived.operands.len());
        for operand in &derived.operands {
            let Some(c) = d.channels.iter().find(|c| c.name == *operand) else {
                return Err(corrupt(format!(
                    "derived channel {name}: no channel {operand}"
                )));
            };
            operands.push(c);
        }
        // An operation this version doesn't know, or one given `params` or `requires`, makes
        // the channel unavailable, and so does an operand that is.
        if !derived.known() || operands.iter().any(|c| !c.available()) {
            continue;
        }
        let [a, b] = operands[..] else {
            return Err(corrupt(format!(
                "derived channel {name}: {} takes two operands, not {}",
                derived.op,
                operands.len()
            )));
        };
        if a.coding().is_none() || b.coding().is_none() {
            return Err(corrupt(format!(
                "derived channel {name}: its operands are analog channels"
            )));
        }
        let grid = |c: &DeclaredChannel| match &c.data {
            DeclaredKind::Analog { period, phase, .. } => Some((*period, *phase)),
            DeclaredKind::Digital { .. } => None,
        };
        let moved = |c: &DeclaredChannel| c.shaping.offset.as_ref().map_or(0, |o| o.ticks);
        if a.source != b.source || grid(a) != grid(b) || moved(a) != moved(b) {
            return Err(corrupt(format!(
                "derived channel {name}: {} and {} come from different sources, or have \
                 different periods, phases or moves",
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

/// A digital channel's gaps, against what its header states of it (FORMAT.md, "Gaps"): in order,
/// each inside the channel's start to its end, and none from its start unless its level there is
/// low.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_gaps(
    name: &str,
    gaps: &[Gap],
    start: i64,
    end: i64,
    initial: bool,
    map: &Mapping,
    length: i64,
) -> Result<(), DecodeError> {
    let mut after = start - 1;
    for g in gaps {
        if g.from <= after
            || g.to <= g.from
            || g.to >= end
            || !maps_inside(map, g.from, length)
            || !maps_inside(map, g.to, length)
        {
            return Err(corrupt(format!(
                "channel {name}: a gap from {} to {} is out of place",
                g.from, g.to
            )));
        }
        if g.from == start && initial {
            return Err(corrupt(format!(
                "channel {name} has a gap from its start but a high initial level"
            )));
        }
        after = g.to;
    }
    Ok(())
}

/// An analog channel's runs, against what its header states of it (FORMAT.md, "Gaps"): each
/// break in order, inside its samples, its phase on the grid, starting at or after the place of
/// the sample before it and, where it states lost samples, past place `index` on the run before
/// it; every sample and the losses at either end inside the capture. Its last run.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_runs(
    name: &str,
    grid: Grid,
    first: i64,
    count: u64,
    breaks: &[Break],
    lost_after: u64,
    map: &Mapping,
    length: i64,
) -> Result<Break, DecodeError> {
    let outside = || corrupt(format!("channel {name}'s samples lie outside the capture"));
    let d = grid.d();
    let mut run = grid.first_run(first, 0);
    if count == 0 {
        // With no sample, `first` places nothing.
        if !breaks.is_empty() {
            return Err(corrupt(format!("channel {name}: a break is out of place")));
        }
        return Ok(run);
    }
    if !source_tick(first) || !place_inside(map, grid.start(run), d, length) {
        return Err(outside());
    }
    for b in breaks {
        if b.index <= run.index
            || b.index >= count
            || u64::from(b.phase) >= d
            || !source_tick(b.tick)
            || !place_inside(map, grid.start(*b), d, length)
        {
            return Err(corrupt(format!("channel {name}: a break is out of place")));
        }
        if b.lost >= JSON_SAFE {
            return Err(corrupt(format!(
                "channel {name}: a break's lost count is past 2^53"
            )));
        }
        check_break(name, grid, run, *b)?;
        run = *b;
    }
    let last = grid.place(run, count - 1);
    if !place_inside(map, last, d, length) {
        return Err(outside());
    }
    if lost_after > 0 {
        let end = grid.place(run, count.saturating_add(lost_after));
        if !place_up_to(map, end, d, length) {
            return Err(corrupt(format!(
                "channel {name}: the {lost_after} samples lost after its last end past the length"
            )));
        }
    }
    Ok(run)
}

/// A break `b` after the run `run`: it starts at or after the place of the sample before it, and
/// where it states lost samples, past place `b.index` on `run`'s grid, so that part of a lost
/// sample lies before it.
pub(crate) fn check_break(name: &str, grid: Grid, run: Break, b: Break) -> Result<(), DecodeError> {
    let start = grid.start(b);
    if start < grid.place(run, b.index - 1) {
        return Err(corrupt(format!(
            "channel {name}: the run at sample {} starts before the sample before it",
            b.index
        )));
    }
    if b.lost > 0 && start <= grid.place(run, b.index) {
        return Err(corrupt(format!(
            "channel {name}: the run at sample {} states {} lost samples, but starts no later \
             than where the first of them would sit",
            b.index, b.lost
        )));
    }
    Ok(())
}

/// Where the samples lost before an analog channel's first start: the place of `-lost` on the
/// first run's grid, in `1/d` ticks; None below source tick 0.
pub fn lost_before_start(grid: Grid, first: i64, lost: u64) -> Option<i128> {
    let run = grid.first_run(first, 0);
    let back = i128::try_from(u128::from(lost).checked_mul(grid.step())?).ok()?;
    Some(grid.start(run).checked_sub(back)?).filter(|&u| u >= 0)
}

/// Where the samples lost after an analog channel's last end: the place of `count + lost` on the
/// last run's grid, in `1/d` ticks.
pub fn lost_after_end(grid: Grid, run: Break, count: u64, lost: u64) -> Option<i128> {
    Some(grid.place(run, count.checked_add(lost)?))
}

/// What the header promises about the capture's extent, checked before anything relies on it.
/// A channel's list in pages is checked once it is read (`Footer::load_list`).
pub(crate) fn check_header(h: &Header) -> Result<(), DecodeError> {
    if !(1..JSON_SAFE as i64).contains(&h.length) {
        return Err(corrupt("the length covers no tick or is past 2^53"));
    }
    let declaration = h.declaration();
    check_declaration(&declaration)?;
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
        if ch.blocks == 0 || ch.blocks > length as u64 {
            return Err(corrupt(format!(
                "channel {name} states {} blocks; its blocks tile the length, {length}",
                ch.blocks
            )));
        }
        let list = ch.data.list_len();
        if !safe(list) {
            return Err(corrupt(format!("channel {name}'s list is past 2^53")));
        }
        match &ch.data {
            ChannelKind::Digital {
                initial,
                end,
                edges,
                gaps,
                ..
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
                if *end == start && *edges > 0 {
                    return Err(corrupt(format!(
                        "channel {name} ends at its start, so it holds no edge, but states {edges}"
                    )));
                }
                if !safe(*edges) {
                    return Err(corrupt(format!("channel {name}'s edge count is past 2^53")));
                }
                if ch.available() {
                    check_gaps(name, gaps, start, *end, *initial, &map, length)?;
                }
            }
            ChannelKind::Analog {
                first,
                period,
                phase,
                count,
                breaks,
                break_count,
                lost_before,
                lost_after,
                ..
            } => {
                let grid = Grid::new(*period, *phase)
                    .ok_or_else(|| corrupt(format!("channel {name}'s grid is 2^16 or more")))?;
                if !safe(*count) || !safe(*lost_before) || !safe(*lost_after) {
                    return Err(corrupt(format!(
                        "channel {name}'s count or losses are past 2^53"
                    )));
                }
                if *count == 0 && (*lost_before > 0 || *lost_after > 0) {
                    return Err(corrupt(format!(
                        "channel {name} states samples lost at either end of none"
                    )));
                }
                if break_count.is_some_and(|n| n >= *count) {
                    return Err(corrupt(format!(
                        "channel {name} states more breaks than its samples hold"
                    )));
                }
                if *lost_before > 0 {
                    let start = lost_before_start(grid, *first, *lost_before);
                    if start.is_none_or(|u| map.axis_place(u, grid.d()).is_none_or(|t| t < 0)) {
                        return Err(corrupt(format!(
                            "channel {name}: the {lost_before} samples lost before its first \
                             start below tick 0"
                        )));
                    }
                }
                if !ch.available() {
                    // Its data is not this version's to check.
                } else if break_count.is_none() {
                    let run = check_runs(
                        name,
                        grid,
                        *first,
                        *count,
                        breaks,
                        *lost_after,
                        &map,
                        length,
                    )?;
                    check_shaping(ch, run)?;
                } else if *count > 0
                    && !place_inside(
                        &map,
                        grid.start(grid.first_run(*first, 0)),
                        grid.d(),
                        length,
                    )
                {
                    return Err(corrupt(format!(
                        "channel {name}'s samples lie outside the capture"
                    )));
                }
            }
        }
        if let ChannelKind::Digital { .. } = &ch.data
            && ch.available()
        {
            check_shaping(ch, Break::default())?;
        }
    }
    match h.annotation_parts {
        Some(0) => {
            return Err(corrupt(
                "the header states 0 annotation parts; a recording with none states no count",
            ));
        }
        Some(n) if n >= JSON_SAFE => {
            return Err(corrupt(format!(
                "the header states {n} annotation parts, past 2^53"
            )));
        }
        _ => {}
    }
    let overview_channels = h.channels.len() + h.derived.len();
    for o in 0..overview_channels {
        let shift = h
            .shift(o)
            .ok_or_else(|| corrupt("a derived channel names no channel"))?;
        let levels = h.levels(o).unwrap_or_default();
        let ladder = overview::ladder(length, shift);
        let name = match h.channels.get(o) {
            Some(c) => c.name.clone(),
            None => h.derived[o - h.channels.len()].name.clone(),
        };
        let fits = levels.len() + 1 == ladder.len()
            && levels.iter().zip(&ladder).all(|(l, &s)| {
                let blocks = overview::block_count(length, s);
                l.shift == s
                    && l.blocks == blocks
                    && l.rows >= 1
                    && l.rows <= blocks
                    && (blocks == 1 || l.rows >= 2)
            });
        if !fits {
            return Err(corrupt(format!(
                "channel {name}: the overview levels are not the ones the format sets for this \
                 length"
            )));
        }
    }
    // A derived channel's sample i sits where its operands' do: compared from what the header
    // holds, and of paged breaks where a reader fetches both lists.
    for (i, d) in h.derived.iter().enumerate() {
        if !h.available(h.channels.len() + i) {
            continue;
        }
        let extent = |operand: &String| {
            h.channels
                .iter()
                .find(|c| c.name == *operand)
                .and_then(|c| match &c.data {
                    ChannelKind::Analog {
                        first,
                        phase,
                        count,
                        breaks,
                        break_count,
                        lost_before,
                        lost_after,
                        ..
                    } => Some((
                        *first,
                        *phase,
                        *count,
                        breaks,
                        *break_count,
                        *lost_before,
                        *lost_after,
                    )),
                    ChannelKind::Digital { .. } => None,
                })
        };
        if extent(&d.operands[0]) != extent(&d.operands[1]) {
            return Err(corrupt(format!(
                "derived channel {}: {} and {} differ in first, phase, count, breaks or losses",
                d.name, d.operands[0], d.operands[1]
            )));
        }
    }
    Ok(())
}

/// A moved or trimmed channel's statements, against its data: FORMAT.md's moved and trimmed
/// channels. `check_header` has checked the data itself, and the kept span's own bounds; `run` is
/// an analog channel's last run.
fn check_shaping(c: &ChannelHeader, run: Break) -> Result<(), DecodeError> {
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
            phase,
            count,
            ..
        } => {
            if *count == 0 {
                return Ok(());
            }
            let Some(grid) = Grid::new(*period, *phase) else {
                return Err(corrupt(format!("channel {name}'s grid is 2^16 or more")));
            };
            let d = i128::from(grid.d());
            let last = grid.place(run, count - 1);
            if *first < from || last >= i128::from(to) * d {
                // As an i64: formatting an i128 costs the browser's decoder more than this check.
                let last = grid.floor(last);
                return Err(corrupt(format!(
                    "channel {name} is kept from {from} to {to}, but its samples run from {first} to {last}"
                )));
            }
        }
    }
    Ok(())
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

/// How to read one channel's block.
pub(crate) enum Section {
    /// Every edge lies in `start..end` of the channel's source ticks, `end` `i64::MAX` while it
    /// is not known, and `map` places it on the axis.
    Digital { start: i64, end: i64, map: Mapping },
    Analog {
        coding: AnalogCoding,
        first_index: u64,
    },
}

/// A block's decoded payload as its channel's items. `word` is the block's word. A section that
/// breaks the format is a `Corrupt` error whose message the caller places; one this reader can't
/// allocate for is a `Resource` error.
pub(crate) fn decode_section(
    raw: &[u8],
    section: &Section,
    word: u32,
    start: i64,
    end: i64,
) -> Result<BlockChannel, DecodeError> {
    Ok(match section {
        Section::Digital {
            start: from,
            end: to,
            map,
        } => {
            let (edges, source) = digital_edges(
                raw,
                (word & !LEVEL_BIT) as usize,
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
            values: analog_values(raw, word as usize, *coding).map_err(|e| {
                if e.starts_with(CANT_HOLD) {
                    DecodeError::Resource(e)
                } else {
                    DecodeError::Corrupt(e)
                }
            })?,
        },
    })
}

/// A section decoder's error with `what` placed before its message, where the format is broken.
pub(crate) fn placed(e: DecodeError, what: impl FnOnce() -> String) -> DecodeError {
    match e {
        DecodeError::Corrupt(m) => corrupt(format!("{}: {m}", what())),
        other => other,
    }
}

/// A block as a channel's index places it: its row, where it ends, the index of its first item,
/// and the next block's word, which says the level a digital block leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockRef {
    pub start: i64,
    pub end: i64,
    pub first_index: u64,
    pub row: BlockRow,
    pub next_word: Option<u32>,
}

/// An overview row as its level's index places it: blocks `k..k + n`, each the frame's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelRef {
    pub k: u64,
    pub row: OverviewRow,
}

/// One overview block, decoded: buckets `first..first + level.buckets` of its level, and the `n`
/// blocks it stands for, each the same buckets.
#[derive(Debug)]
pub struct OverviewBlock {
    pub first: u64,
    pub n: u32,
    pub level: Level,
}

/// A channel's gaps or breaks: all of them, or those a span of its ticks needs. For a span, a
/// channel's breaks are the last at or before its start, the one before that, and every one
/// after it through the first at or past its end; its gaps are the last two starting at or
/// before its start and every one starting inside it.
#[derive(Clone, Debug, PartialEq)]
pub enum List {
    Gaps(Vec<Gap>),
    Breaks(Vec<Break>),
}

/// What the footer says of one channel or derived channel.
#[derive(Clone, Debug)]
pub struct ChannelIndex {
    /// Its finest overview level's shift.
    pub finest: u8,
    /// Whether this version reads it (FORMAT.md, "Unavailable channels"). An unavailable
    /// channel's tables are listed, so a reader finds its way past them, but nothing of its
    /// blocks, overview or strip is read, and its `column` says nothing.
    pub available: bool,
    pub column: Column,
    /// Its strip's shift: its coarsest level's.
    pub strip_shift: u8,
    strip: Range<usize>,
    strip_decoded: u32,
    /// Its blocks; None for a derived channel.
    pub blocks: Option<TableIndex>,
    /// Each level of its overview in overview blocks, finest first.
    pub levels: Vec<(OverviewLevel, TableIndex)>,
    /// Its gaps or breaks, where they are in list pages.
    pub list: Option<TableIndex>,
}

impl ChannelIndex {
    /// Its strip's frame's bytes in the footer, and what they decode to.
    pub fn strip_size(&self) -> (usize, u32) {
        (self.strip.len(), self.strip_decoded)
    }
}

/// A finished file's footer: the header, the top index and the strips. Parsed from the file's
/// last bytes, it says where every channel's pages are.
#[derive(Clone, Debug)]
pub struct Footer {
    pub trailer: Trailer,
    pub header: Header,
    /// The annotation bundle record's offset and length, framing included, where there is one.
    pub bundle: Option<(u64, u32)>,
    /// The annotation parts table, where the header states `annotation_parts`.
    pub parts: Option<TableIndex>,
    /// Each channel's, then each derived channel's.
    pub channels: Vec<ChannelIndex>,
    /// Each derived channel's operands, by channel index, and its rule; None for one this
    /// version can't compute, which is unavailable.
    pub products: Vec<Option<(usize, usize, Product)>>,
    strips: Vec<u8>,
    /// Each channel's mapping to the axis.
    maps: Vec<Mapping>,
    /// Each channel's start, in its source's ticks.
    channel_starts: Vec<i64>,
    /// Each channel's gaps or breaks, inline from the header or once read from its pages.
    lists: Vec<Option<List>>,
    /// Each digital channel's gaps' ends on the axis, once its gaps are known.
    gap_ends: Vec<Vec<i64>>,
}

/// Reads one table's entry in the top index: its depth, its entry count and its entries.
fn top_table(
    cursor: &mut &[u8],
    channel: usize,
    table: Table,
    rows: u64,
    footer_offset: u64,
) -> Result<TableIndex, DecodeError> {
    let bad = |what: &str| {
        corrupt(format!(
            "the top index's {} of channel {channel}: {what}",
            crate::index::table_name(table)
        ))
    };
    let [depth, e] = *cursor
        .split_first_chunk::<2>()
        .ok_or_else(|| bad("cut short"))?
        .0;
    *cursor = &cursor[2..];
    if depth != depth_of(rows) || u64::from(e) != pages_at(rows, depth) || usize::from(e) > TOP {
        return Err(bad(&format!(
            "depth {depth} and {e} entries, where its {rows} entries give depth {} and {}",
            depth_of(rows),
            pages_at(rows, depth_of(rows))
        )));
    }
    let bytes = cursor
        .get(..usize::from(e) * Entry::SIZE)
        .ok_or_else(|| bad("cut short"))?;
    *cursor = &cursor[bytes.len()..];
    let (entries, _) = bytes.as_chunks::<{ Entry::SIZE }>();
    let entries: Vec<Entry> = entries.iter().map(|e| Entry::parse(e)).collect();
    let smallest = record::FRAMING + crate::index::PAGE_HEAD;
    for (i, en) in entries.iter().enumerate() {
        let inside = en.offset >= PREAMBLE_LEN as u64
            && (en.length as usize) >= smallest
            && en
                .offset
                .checked_add(u64::from(en.length))
                .is_some_and(|end| end <= footer_offset);
        // A parts entry's keys are the span under it, which keeps no order across entries.
        let after = i == 0 || {
            let p = entries[i - 1];
            p.offset < en.offset
                && (table.spans() || (p.first <= en.first && p.number <= en.number))
        };
        let span = !table.spans() || crate::index::span_in_range(en.first, en.number);
        if !inside || !after || !span {
            return Err(bad("an entry is out of place"));
        }
    }
    let first = entries[0];
    let starts = match table {
        Table::Blocks | Table::Level(_) => (first.first, first.number) == (0, 0),
        Table::Gaps => first.number == 0,
        Table::Breaks | Table::Parts => true,
    };
    if !starts {
        return Err(bad("its first entry does not start the table"));
    }
    Ok(TableIndex::finished(
        channel as u32,
        table,
        rows,
        depth,
        entries,
    ))
}

impl Footer {
    /// Parses a whole file's footer.
    pub fn parse_file(file: &[u8]) -> Result<Self, DecodeError> {
        if !file.starts_with(&MAGIC) {
            return Err(DecodeError::NotTickwave);
        }
        Self::parse(file, file.len() as u64)
    }

    /// Parses the footer from the file's last bytes, `tail`, of a file `file_len` long.
    /// `NeedFrom` says where the footer starts when `tail` holds less of it. The strips are not
    /// decoded until asked for, so no zstd decoder runs here.
    pub fn parse(tail: &[u8], file_len: u64) -> Result<Self, DecodeError> {
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
        if trailer.footer_len as usize > MAX_FOOTER {
            return Err(corrupt(format!(
                "the footer is {} bytes, more than the {MAX_FOOTER} a footer holds",
                trailer.footer_len
            )));
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
        if header_len as usize > MAX_DECLARATION {
            return Err(corrupt(format!(
                "the footer's header is {header_len} bytes, more than the {MAX_DECLARATION} a \
                 header holds"
            )));
        }
        let json = &payload[record::FOOTER_HEAD_LEN..header_end];
        if let Some(key) = crate::parts::key_twice(json) {
            return Err(corrupt(format!(
                "the footer's header names key {key:?} twice"
            )));
        }
        let header: Header = serde_json::from_slice(json)?;
        check_header(&header)?;
        let declaration = header.declaration();
        let columns = overview::columns(&declaration.channels, &declaration.derived);
        let products = overview::rules(&declaration.channels, &declaration.derived);
        let footer_offset = trailer.footer_offset;

        let mut cursor = &payload[header_end..index_end];
        let (Some(bundle_at), Some(bundle_len)) = (
            cursor
                .get(..8)
                .map(|b| u64::from_le_bytes(b.try_into().unwrap())),
            cursor
                .get(8..12)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap())),
        ) else {
            return Err(corrupt(
                "the top index is too short for the annotation bundle's place",
            ));
        };
        cursor = &cursor[12..];
        let bundle = match (bundle_at, bundle_len) {
            (0, 0) => None,
            (at, len)
                if at >= PREAMBLE_LEN as u64
                    && (len as usize) > record::FRAMING + 4
                    && at.checked_add(u64::from(len)) == Some(footer_offset) =>
            {
                Some((at, len))
            }
            _ => {
                return Err(corrupt(
                    "the annotation bundle does not lie just before the footer",
                ));
            }
        };
        let parts = match header.annotation_parts {
            Some(_) if bundle.is_none() => {
                return Err(corrupt(
                    "the header states annotation parts, and the file holds no annotation bundle",
                ));
            }
            Some(n) => Some(top_table(&mut cursor, 0, Table::Parts, n, footer_offset)?),
            None => None,
        };
        let channels_n = header.channels.len();
        let mut channels = Vec::with_capacity(columns.len());
        let mut strip_at = index_end;
        for (o, &column) in columns.iter().enumerate() {
            let (Some(frame_len), Some(decoded)) = (
                cursor
                    .get(..4)
                    .map(|b| u32::from_le_bytes(b.try_into().unwrap())),
                cursor
                    .get(4..8)
                    .map(|b| u32::from_le_bytes(b.try_into().unwrap())),
            ) else {
                return Err(corrupt(format!(
                    "the top index is cut short at channel {o}"
                )));
            };
            cursor = &cursor[8..];
            let finest = header.shift(o).expect("a checked header's channel");
            let ladder = overview::ladder(header.length, finest);
            let strip_shift = *ladder.last().expect("a ladder has a strip");
            let available = header.available(o);
            // An unavailable channel's strip has a layout this version can't know.
            let want = column.decoded_len(overview::buckets(header.length, strip_shift));
            if available && u64::from(decoded) != want {
                return Err(corrupt(format!(
                    "channel {o}'s strip decodes to {decoded} bytes, where its {} buckets take {want}",
                    overview::buckets(header.length, strip_shift)
                )));
            }
            if decoded as usize > record::MAX_STRIP
                || frame_len as usize > compress_bound(decoded as usize)
            {
                return Err(corrupt(format!(
                    "channel {o}'s strip is {frame_len} bytes for {decoded}, past the format's \
                     limits"
                )));
            }
            let strip_end = strip_at
                .checked_add(frame_len as usize)
                .filter(|&e| e <= payload.len())
                .ok_or_else(|| corrupt(format!("channel {o}'s strip runs past the footer")))?;
            let blocks = match header.channels.get(o) {
                Some(c) => Some(top_table(
                    &mut cursor,
                    o,
                    Table::Blocks,
                    c.blocks,
                    footer_offset,
                )?),
                None => None,
            };
            let levels_stated = header.levels(o).unwrap_or_default();
            let mut levels = Vec::with_capacity(levels_stated.len());
            for l in levels_stated {
                let t = top_table(&mut cursor, o, Table::Level(l.shift), l.rows, footer_offset)?;
                levels.push((*l, t));
            }
            let list = match header.channels.get(o).map(|c| &c.data) {
                Some(ChannelKind::Digital {
                    gap_count: Some(n), ..
                }) => Some(top_table(&mut cursor, o, Table::Gaps, *n, footer_offset)?),
                Some(ChannelKind::Analog {
                    break_count: Some(n),
                    ..
                }) => Some(top_table(&mut cursor, o, Table::Breaks, *n, footer_offset)?),
                _ => None,
            };
            channels.push(ChannelIndex {
                finest,
                available,
                column,
                strip_shift,
                strip: strip_at..strip_end,
                strip_decoded: decoded,
                blocks,
                levels,
                list,
            });
            strip_at = strip_end;
        }
        if !cursor.is_empty() {
            return Err(corrupt("the top index holds bytes past its last table"));
        }
        if strip_at != payload.len() {
            return Err(corrupt("the strips don't end where the footer does"));
        }
        let strips = payload[index_end..].to_vec();
        let base = index_end;
        for c in &mut channels {
            c.strip = c.strip.start - base..c.strip.end - base;
        }
        let unmapped = || corrupt("a channel has no mapping to the axis");
        let maps = header.mappings().ok_or_else(unmapped)?;
        let channel_starts = (0..channels_n)
            .map(|c| header.start(c).ok_or_else(unmapped))
            .collect::<Result<Vec<i64>, _>>()?;
        let mut lists = Vec::with_capacity(channels_n);
        let mut ends = Vec::with_capacity(channels_n);
        for (c, ch) in header.channels.iter().enumerate() {
            match &ch.data {
                ChannelKind::Digital {
                    gaps, gap_count, ..
                } => {
                    lists.push(gap_count.is_none().then(|| List::Gaps(gaps.clone())));
                    ends.push(gap_ends(gaps, &maps[c]));
                }
                ChannelKind::Analog {
                    breaks,
                    break_count,
                    ..
                } => {
                    lists.push(break_count.is_none().then(|| List::Breaks(breaks.clone())));
                    ends.push(Vec::new());
                }
            }
        }
        Ok(Self {
            trailer,
            header,
            bundle,
            parts,
            channels,
            products,
            strips,
            maps,
            channel_starts,
            lists,
            gap_ends: ends,
        })
    }

    /// The channels and derived channels the footer indexes.
    pub fn overview_channels(&self) -> usize {
        self.channels.len()
    }

    /// Whether this version reads overview channel `o`: false for one it shows as unavailable,
    /// and for no such channel.
    pub fn available(&self, o: usize) -> bool {
        self.channels.get(o).is_some_and(|c| c.available)
    }

    /// An error naming overview channel `o` where it is unavailable, or there is no such channel.
    pub fn check_available(&self, o: usize) -> Result<(), DecodeError> {
        if self.available(o) {
            return Ok(());
        }
        let name = match self.header.channels.get(o) {
            Some(c) => c.name.clone(),
            None => self
                .header
                .derived
                .get(o.wrapping_sub(self.header.channels.len()))
                .map_or_else(|| format!("{o}"), |d| d.name.clone()),
        };
        Err(corrupt(format!(
            "channel {name} is unavailable to this reader: it needs a feature, codec or \
             operation this version doesn't know, or is derived from one that does"
        )))
    }

    /// Analog channel `c`'s grid; None for a digital channel, or no such channel.
    pub fn grid(&self, c: usize) -> Option<Grid> {
        match &self.header.channels.get(c)?.data {
            ChannelKind::Analog { period, phase, .. } => Grid::new(*period, *phase),
            ChannelKind::Digital { .. } => None,
        }
    }

    /// Overview channel `o`'s strip: its coarsest level, every bucket.
    pub fn strip(&self, o: usize) -> Result<Level, DecodeError> {
        self.strip_with(o, inflate)
    }

    /// `strip` with another zstd decoder, which must do what this module's `inflate` does.
    pub fn strip_with(
        &self,
        o: usize,
        inflate: impl Fn(&[u8], usize) -> Result<Vec<u8>, DecodeError>,
    ) -> Result<Level, DecodeError> {
        let raw = self.strip_bytes(o, inflate)?;
        let c = &self.channels[o];
        let n = overview::buckets(self.header.length, c.strip_shift);
        Level::parse(&raw, c.strip_shift, n, c.column)
            .ok_or_else(|| corrupt(format!("channel {o}'s strip does not parse")))
    }

    /// Overview channel `o`'s strip as its frame decodes, before `strip` parses it: parsing
    /// takes any minimum above its maximum as an empty bucket, where the format gives one set
    /// of bytes.
    pub fn strip_bytes(
        &self,
        o: usize,
        inflate: impl Fn(&[u8], usize) -> Result<Vec<u8>, DecodeError>,
    ) -> Result<Vec<u8>, DecodeError> {
        let c = self
            .channels
            .get(o)
            .ok_or_else(|| corrupt(format!("no channel {o}")))?;
        self.check_available(o)?;
        inflate(&self.strips[c.strip.clone()], c.strip_decoded as usize)
    }

    /// Channel `c`'s gaps, inline or once read; None for an analog channel, or one whose gaps
    /// are in pages not yet read (`load_list`).
    pub fn gaps(&self, c: usize) -> Option<&[Gap]> {
        match self.lists.get(c)? {
            Some(List::Gaps(g)) => Some(g),
            _ => None,
        }
    }

    /// Channel `c`'s breaks, as `gaps`.
    pub fn breaks(&self, c: usize) -> Option<&[Break]> {
        match self.lists.get(c)? {
            Some(List::Breaks(b)) => Some(b),
            _ => None,
        }
    }

    /// Whether channel `c`'s list is known: inline, or read from its pages.
    pub fn list_known(&self, c: usize) -> bool {
        self.lists.get(c).is_some_and(Option::is_some)
    }

    /// Reads every channel's list that is in pages, through `fetch`.
    pub fn load_lists<E: From<DecodeError>>(
        &mut self,
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<(), E> {
        for c in 0..self.header.channels.len() {
            self.load_list(c, &mut fetch)?;
        }
        Ok(())
    }

    /// Reads channel `c`'s gaps or breaks from its list pages, through `fetch`, and checks them
    /// against its header. Nothing to read for a list in the header.
    pub fn load_list<E: From<DecodeError>>(
        &mut self,
        c: usize,
        fetch: &mut impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<(), E> {
        if self.list_known(c) {
            return Ok(());
        }
        let Some(table) = self.channels.get(c).and_then(|ch| ch.list.clone()) else {
            return Err(corrupt(format!("no channel {c}")).into());
        };
        let pages = table.read_all(fetch)?;
        let h = &self.header;
        let ch = &h.channels[c];
        let name = &ch.name;
        let map = self.maps[c];
        let list = match &ch.data {
            ChannelKind::Digital { initial, end, .. } => {
                let gaps: Vec<Gap> = pages
                    .into_iter()
                    .flat_map(|p| match p {
                        Page::Gaps(g) => g,
                        _ => unreachable!("a gap list's pages hold gaps"),
                    })
                    .collect();
                check_gaps(
                    name,
                    &gaps,
                    self.channel_starts[c],
                    *end,
                    *initial,
                    &map,
                    h.length,
                )?;
                self.gap_ends[c] = gap_ends(&gaps, &map);
                List::Gaps(gaps)
            }
            ChannelKind::Analog {
                first,
                period,
                phase,
                count,
                lost_after,
                ..
            } => {
                let breaks: Vec<Break> = pages
                    .into_iter()
                    .flat_map(|p| match p {
                        Page::Breaks(b) => b,
                        _ => unreachable!("a break list's pages hold breaks"),
                    })
                    .collect();
                let grid = Grid::new(*period, *phase).expect("a checked header's grid");
                let run = check_runs(
                    name,
                    grid,
                    *first,
                    *count,
                    &breaks,
                    *lost_after,
                    &map,
                    h.length,
                )?;
                check_shaping(ch, run)?;
                // A derived channel's operands share their breaks: checked where both lists are
                // read, never by reading one only to check it.
                for (i, d) in h.derived.iter().enumerate() {
                    if self.products.get(i).is_none_or(Option::is_none) {
                        continue;
                    }
                    let partner = if d.operands[0] == *name {
                        &d.operands[1]
                    } else if d.operands[1] == *name {
                        &d.operands[0]
                    } else {
                        continue;
                    };
                    let p = h.channels.iter().position(|x| x.name == *partner);
                    if let Some(theirs) = p.and_then(|p| self.breaks(p))
                        && theirs != breaks.as_slice()
                    {
                        return Err(corrupt(format!(
                            "derived channel {}: {} and {} differ in breaks",
                            d.name, d.operands[0], d.operands[1]
                        ))
                        .into());
                    }
                }
                List::Breaks(breaks)
            }
        };
        self.lists[c] = Some(list);
        Ok(())
    }

    /// Channel `c`'s blocks, every page of its index read through `fetch`, each checked against
    /// the header: they tile `0..length` in order and hold the items it counts.
    pub fn block_refs<E: From<DecodeError>>(
        &self,
        c: usize,
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Vec<BlockRef>, E> {
        let ch = self
            .header
            .channels
            .get(c)
            .ok_or_else(|| corrupt(format!("no channel {c}")))?;
        let table = self.channels[c]
            .blocks
            .as_ref()
            .expect("a channel has blocks");
        let digital = matches!(ch.data, ChannelKind::Digital { .. });
        let name = &ch.name;
        let mut rows: Vec<(u64, BlockRow)> = Vec::new();
        let mut items = 0u64;
        for page in table.read_all(&mut fetch)? {
            let Page::Blocks { number, rows: page } = page else {
                unreachable!("a block table's pages hold blocks")
            };
            if number != items || rows.last().is_some_and(|(_, r)| r.start >= page[0].start) {
                return Err(corrupt(format!(
                    "channel {name}: its index pages don't follow on from each other"
                ))
                .into());
            }
            for row in page {
                rows.push((items, row));
                items += u64::from(row.items(digital));
            }
        }
        let promised = match ch.data {
            ChannelKind::Digital { edges, .. } => edges,
            ChannelKind::Analog { count, .. } => count,
        };
        if rows.len() as u64 != ch.blocks || items != promised {
            return Err(corrupt(format!(
                "channel {name}: the index holds {} blocks of {items} items, the header {} of {promised}",
                rows.len(),
                ch.blocks
            ))
            .into());
        }
        let length = self.header.length;
        if rows.last().is_some_and(|(_, r)| r.start >= length) {
            return Err(corrupt(format!("channel {name}: a block starts past the length")).into());
        }
        let mut out = Vec::with_capacity(rows.len());
        for (i, &(first_index, row)) in rows.iter().enumerate() {
            let next = rows.get(i + 1).map(|(_, r)| *r);
            out.push(BlockRef {
                start: row.start,
                end: next.map_or(length, |r| r.start),
                first_index,
                row,
                next_word: next.map(|r| r.word),
            });
        }
        self.check_blocks(c, &out, 0, None)?;
        Ok(out)
    }

    /// Channel `c`'s blocks on the page `at` locates in its block table, `page` that page as
    /// `TableIndex::visit` or `parse_page` gives it: each as `block_refs` gives it, but the last
    /// with no `next_word`, since the next block is on the next page. They are checked to tile
    /// to where the next page starts or, for the last page, to the length and the channel's
    /// count, so the pages a reader reads one at a time tile as the whole table does. And they
    /// are checked as `block_refs` checks them against the channel's gaps or breaks: `list`
    /// where it is given, which must hold every one the page's ticks need (see `List`), else the
    /// footer's where it holds them. One thing goes unchecked: the level a page's last block
    /// leaves, which only the next page's first block states.
    pub fn block_page(
        &self,
        c: usize,
        at: &Located,
        page: &Page,
        list: Option<&List>,
    ) -> Result<Vec<BlockRef>, DecodeError> {
        let ch = self
            .header
            .channels
            .get(c)
            .ok_or_else(|| corrupt(format!("no channel {c}")))?;
        let Page::Blocks { number, rows } = page else {
            return Err(corrupt(format!(
                "channel {c}: a block page holds no blocks"
            )));
        };
        let name = &ch.name;
        let digital = matches!(ch.data, ChannelKind::Digital { .. });
        let promised = match ch.data {
            ChannelKind::Digital { edges, .. } => edges,
            ChannelKind::Analog { count, .. } => count,
        };
        let (end, items_to) = at.next.unwrap_or((self.header.length, promised));
        let mut out = Vec::with_capacity(rows.len());
        let mut items = *number;
        for (i, &row) in rows.iter().enumerate() {
            let next = rows.get(i + 1);
            out.push(BlockRef {
                start: row.start,
                end: next.map_or(end, |r| r.start),
                first_index: items,
                row,
                next_word: next.map(|r| r.word),
            });
            items = items
                .checked_add(u64::from(row.items(digital)))
                .ok_or_else(|| corrupt(format!("channel {name}: its items pass 2^64")))?;
        }
        if items != items_to || rows.last().is_none_or(|r| r.start >= end) {
            return Err(corrupt(format!(
                "channel {name}: index page {} doesn't end where the next starts",
                at.j
            )));
        }
        self.check_blocks(c, &out, at.j.saturating_mul(PAGE as u64), list)?;
        Ok(out)
    }

    /// One page of channel `c`'s gaps or breaks, the page `at` locates in its list's table,
    /// checked as `load_list` checks the whole list: each entry against the header and the one
    /// before it, the last against where the next page starts, the first page's first against
    /// the channel's start, and the last page's last run against the capture's end. So a reader
    /// can take a list a page at a time. Not checked: that a derived channel's operands' pages
    /// agree, which `load_list` checks of their whole lists.
    pub fn list_page(&self, c: usize, at: &Located, page: &Page) -> Result<List, DecodeError> {
        let ch = self
            .header
            .channels
            .get(c)
            .ok_or_else(|| corrupt(format!("no channel {c}")))?;
        let name = &ch.name;
        let map = self.maps[c];
        let length = self.header.length;
        let out_of_place = || {
            corrupt(format!(
                "channel {name}: list page {} is out of place",
                at.j
            ))
        };
        match (&ch.data, page) {
            (ChannelKind::Digital { initial, end, .. }, Page::Gaps(gaps)) => {
                // As `check_gaps`: each gap after the one before, the first page's first after
                // the channel's start; a later page's first was held to its `from` by the page
                // before.
                let start = self.channel_starts[c];
                let mut after = if at.j == 0 { start - 1 } else { i64::MIN };
                for g in gaps {
                    if g.from <= after
                        || g.from < start
                        || g.to <= g.from
                        || g.to >= *end
                        || !maps_inside(&map, g.from, length)
                        || !maps_inside(&map, g.to, length)
                    {
                        return Err(out_of_place());
                    }
                    if g.from == start && *initial {
                        return Err(corrupt(format!(
                            "channel {name} has a gap from its start but a high initial level"
                        )));
                    }
                    after = g.to;
                }
                if let (Some(last), Some((next, _))) = (gaps.last(), at.next)
                    && last.to >= next
                {
                    return Err(out_of_place());
                }
                Ok(List::Gaps(gaps.clone()))
            }
            (
                ChannelKind::Analog {
                    first,
                    period,
                    phase,
                    count,
                    lost_after,
                    ..
                },
                Page::Breaks(breaks),
            ) => {
                let outside =
                    || corrupt(format!("channel {name}'s samples lie outside the capture"));
                let grid = Grid::new(*period, *phase).expect("a checked header's grid");
                let d = grid.d();
                let mut run = (at.j == 0).then_some(grid.first_run(*first, 0));
                if *count == 0 {
                    return Err(out_of_place());
                }
                for b in breaks {
                    if run.is_some_and(|r| b.index <= r.index)
                        || b.index >= *count
                        || u64::from(b.phase) >= d
                        || !source_tick(b.tick)
                        || !place_inside(&map, grid.start(*b), d, length)
                        || b.lost >= JSON_SAFE
                    {
                        return Err(out_of_place());
                    }
                    if let Some(r) = run {
                        check_break(name, grid, r, *b)?;
                    }
                    run = Some(*b);
                }
                let Some(last) = run else {
                    return Err(out_of_place());
                };
                match at.next {
                    Some((tick, index)) => {
                        // The next page's first break, of which its directory entry gives the
                        // tick and index: it starts at or after the place of the sample before
                        // it, whatever its phase.
                        if index <= last.index
                            || grid.place(last, index - 1) >= (i128::from(tick) + 1) * d as i128
                        {
                            return Err(out_of_place());
                        }
                    }
                    None => {
                        // The last run, as `check_runs` ends.
                        if !place_inside(&map, grid.place(last, count - 1), d, length) {
                            return Err(outside());
                        }
                        if *lost_after > 0 {
                            let to = grid.place(last, count.saturating_add(*lost_after));
                            if !place_up_to(&map, to, d, length) {
                                return Err(outside());
                            }
                        }
                        check_shaping(ch, last)?;
                    }
                }
                Ok(List::Breaks(breaks.clone()))
            }
            _ => Err(out_of_place()),
        }
    }

    /// What a channel's blocks say without being decoded, against its gaps or runs where they
    /// are known: a digital channel's level at each block's start, and an analog channel's
    /// samples inside each block's ticks. Each row's frame lies within the format's limits, so
    /// nothing past them is fetched. `blocks` are the channel's from block `from` on; the gaps or
    /// breaks are `list` where it is given, else the footer's.
    fn check_blocks(
        &self,
        c: usize,
        blocks: &[BlockRef],
        from: u64,
        list: Option<&List>,
    ) -> Result<(), DecodeError> {
        let ch = &self.header.channels[c];
        let name = &ch.name;
        if let Some(b) = blocks.iter().find(|b| {
            b.row.decoded_length as usize > MAX_BLOCK
                || b.row.length as usize > compress_bound(b.row.decoded_length as usize)
        }) {
            return Err(corrupt(format!(
                "channel {name}: the block at tick {} is {} bytes for {}, past the format's \
                 limits",
                b.start, b.row.length, b.row.decoded_length
            )));
        }
        // Each item takes a byte at least, so a block holds at most as many as it decodes to.
        let digital = matches!(ch.data, ChannelKind::Digital { .. });
        if let Some(b) = blocks
            .iter()
            .find(|b| b.row.items(digital) as usize > MAX_BLOCK)
        {
            return Err(corrupt(format!(
                "channel {name}: the block at tick {} holds {} items, more than the {MAX_BLOCK} a \
                 block holds",
                b.start,
                b.row.items(digital)
            )));
        }
        if !ch.available() {
            return Ok(());
        }
        let given_ends;
        let (gaps, ends) = match list {
            Some(List::Gaps(g)) => {
                given_ends = gap_ends(g, &self.maps[c]);
                (Some(&g[..]), &given_ends[..])
            }
            _ => (self.gaps(c), &self.gap_ends[c][..]),
        };
        let breaks = match list {
            Some(List::Breaks(b)) => Some(&b[..]),
            _ => self.breaks(c),
        };
        match &ch.data {
            ChannelKind::Digital { initial, .. } => {
                if from == 0
                    && (blocks[0].row.word & LEVEL_BIT != 0 && !initial
                        || blocks[0].row.word & LEVEL_BIT == 0 && *initial)
                {
                    return Err(corrupt(format!(
                        "channel {name}: block 0 starts at the wrong level"
                    )));
                }
                let Some(gaps) = gaps else {
                    return Ok(());
                };
                for (i, w) in (from..).zip(blocks.windows(2)) {
                    let ending = gaps_ending_in(gaps, ends, w[0].start, w[0].end);
                    let level = w[0].row.word & LEVEL_BIT != 0;
                    let flips = w[0].row.word & 1 != 0;
                    if ending.is_empty() && (w[1].row.word & LEVEL_BIT != 0) != (level ^ flips) {
                        return Err(corrupt(format!(
                            "channel {name}: block {} starts at the wrong level",
                            i + 1
                        )));
                    }
                }
            }
            ChannelKind::Analog { .. } => {
                let Some(breaks) = breaks else {
                    return Ok(());
                };
                for b in blocks {
                    self.samples_inside(c, b, breaks)?;
                }
            }
        }
        Ok(())
    }

    /// Overview channel `o`'s level `li` (of `channels[o].levels`), every page of its index read
    /// through `fetch`: each row with the first block it stands for.
    pub fn level_refs<E: From<DecodeError>>(
        &self,
        o: usize,
        li: usize,
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Vec<LevelRef>, E> {
        let ch = self
            .channels
            .get(o)
            .ok_or_else(|| corrupt(format!("no channel {o}")))?;
        let (level, table) = ch
            .levels
            .get(li)
            .ok_or_else(|| corrupt(format!("channel {o} has no overview level {li} in blocks")))?;
        let bad = || {
            corrupt(format!(
                "channel {o}: overview level 2^{}'s rows don't stand for its {} blocks",
                level.shift, level.blocks
            ))
        };
        let mut out = Vec::new();
        let mut k = 0u64;
        for page in table.read_all(&mut fetch)? {
            let Page::Level { k: first, rows } = page else {
                unreachable!("a level's pages hold its rows")
            };
            if first != k {
                return Err(bad().into());
            }
            for row in rows {
                let (_, n) = overview::block_buckets(self.header.length, level.shift, k);
                let want = ch.column.decoded_len(n);
                if n == 0
                    || (ch.available && u64::from(row.decoded_length) != want)
                    || row.length as usize > compress_bound(row.decoded_length as usize)
                    || row.decoded_length as usize > record::MAX_OVERVIEW_BLOCK
                {
                    return Err(bad().into());
                }
                out.push(LevelRef { k, row });
                k = k.checked_add(u64::from(row.n)).ok_or_else(bad)?;
            }
        }
        if k != level.blocks
            || out.len() as u64 != level.rows
            || out.last().is_none_or(|r| r.row.n != 1)
        {
            return Err(bad().into());
        }
        Ok(out)
    }

    /// Overview channel `o`'s level `li`'s rows on the page `at` locates in its table, `page` that
    /// page: each with the first block it stands for, checked as `level_refs` checks them,
    /// against where the next page starts or, for the last page, the level's blocks.
    pub fn level_page(
        &self,
        o: usize,
        li: usize,
        at: &Located,
        page: &Page,
    ) -> Result<Vec<LevelRef>, DecodeError> {
        let ch = self
            .channels
            .get(o)
            .ok_or_else(|| corrupt(format!("no channel {o}")))?;
        let (level, _) = ch
            .levels
            .get(li)
            .ok_or_else(|| corrupt(format!("channel {o} has no overview level {li} in blocks")))?;
        let bad = || {
            corrupt(format!(
                "channel {o}: overview level 2^{}'s page {} doesn't stand for its blocks",
                level.shift, at.j
            ))
        };
        let Page::Level { k: first, rows } = page else {
            return Err(bad());
        };
        let blocks_to = match at.next {
            Some((k, _)) => u64::try_from(k).map_err(|_| bad())?,
            None => level.blocks,
        };
        let mut out = Vec::with_capacity(rows.len());
        let mut k = *first;
        for &row in rows {
            let (_, n) = overview::block_buckets(self.header.length, level.shift, k);
            if n == 0
                || (ch.available && u64::from(row.decoded_length) != ch.column.decoded_len(n))
                || row.length as usize > compress_bound(row.decoded_length as usize)
                || row.decoded_length as usize > record::MAX_OVERVIEW_BLOCK
            {
                return Err(bad());
            }
            out.push(LevelRef { k, row });
            k = k.checked_add(u64::from(row.n)).ok_or_else(bad)?;
        }
        let last = at.next.is_none() && out.last().is_none_or(|r| r.row.n != 1);
        if k != blocks_to || last {
            return Err(bad());
        }
        Ok(out)
    }

    /// Decodes overview channel `o`'s level `li` row `r` from exactly its frame's bytes.
    pub fn decode_overview(
        &self,
        o: usize,
        li: usize,
        r: &LevelRef,
        frame: &[u8],
    ) -> Result<OverviewBlock, DecodeError> {
        self.decode_overview_with(o, li, r, frame, inflate)
    }

    /// `decode_overview` with another zstd decoder, which must do what this module's `inflate`
    /// does.
    pub fn decode_overview_with(
        &self,
        o: usize,
        li: usize,
        r: &LevelRef,
        frame: &[u8],
        inflate: impl Fn(&[u8], usize) -> Result<Vec<u8>, DecodeError>,
    ) -> Result<OverviewBlock, DecodeError> {
        let ch = self
            .channels
            .get(o)
            .ok_or_else(|| corrupt(format!("no channel {o}")))?;
        let (level, _) = ch
            .levels
            .get(li)
            .ok_or_else(|| corrupt(format!("channel {o} has no overview level {li} in blocks")))?;
        self.check_available(o)?;
        decode_overview_frame(
            ch.column,
            level.shift,
            self.header.length,
            r,
            frame,
            inflate,
        )
    }

    /// Where overview channel `o`'s level `li`'s blocks holding any tick in `[start, end)` are:
    /// the range of `level_refs`'s rows.
    pub fn rows_covering(refs: &[LevelRef], shift: u8, start: i64, end: i64) -> Range<usize> {
        let block = |t: i64| (t.max(0) as u64 >> shift) / BLOCK_BUCKETS;
        let (a, b) = (block(start), block(end.max(start.saturating_add(1)) - 1));
        let first = refs.partition_point(|r| r.k + u64::from(r.row.n) <= a);
        let last = refs.partition_point(|r| r.k <= b);
        first..last.max(first)
    }

    /// The blocks of `refs` holding any tick in `[start, end)`.
    pub fn blocks_covering(refs: &[BlockRef], start: i64, end: i64) -> Range<usize> {
        let first = refs.partition_point(|r| r.end <= start);
        let last = refs.partition_point(|r| r.start < end).max(first);
        first..last
    }

    /// Decodes channel `c`'s block `b` from exactly its frame's bytes.
    pub fn decode_block(&self, c: usize, b: &BlockRef, frame: &[u8]) -> Result<Block, DecodeError> {
        self.decode_block_with(c, b, frame, inflate)
    }

    /// `decode_block` with another zstd decoder. `inflate` must do what this module's `inflate`
    /// does: decode exactly `len` bytes and refuse a frame whose checksum is missing or wrong.
    pub fn decode_block_with(
        &self,
        c: usize,
        b: &BlockRef,
        frame: &[u8],
        inflate: impl Fn(&[u8], usize) -> Result<Vec<u8>, DecodeError>,
    ) -> Result<Block, DecodeError> {
        let gaps = self.gaps(c).map(|g| (g, &self.gap_ends[c][..]));
        self.decode_checked(c, b, frame, inflate, gaps)
    }

    /// `decode_block` checked against `list`, the channel's gaps or breaks that the block's
    /// ticks need (see `List`), which the footer may not hold: a digital block's edges outside
    /// its gaps and the level it leaves, as `decode_block` checks them, and an analog block's
    /// samples inside its ticks, as `block_refs` checks them.
    pub fn decode_block_in(
        &self,
        c: usize,
        b: &BlockRef,
        frame: &[u8],
        list: &List,
    ) -> Result<Block, DecodeError> {
        self.decode_block_in_with(c, b, frame, list, inflate)
    }

    /// `decode_block_in` with another zstd decoder, as `decode_block_with`.
    pub fn decode_block_in_with(
        &self,
        c: usize,
        b: &BlockRef,
        frame: &[u8],
        list: &List,
        inflate: impl Fn(&[u8], usize) -> Result<Vec<u8>, DecodeError>,
    ) -> Result<Block, DecodeError> {
        let map = self.maps.get(c).copied().unwrap_or(Mapping::IDENTITY);
        match list {
            List::Gaps(gaps) => {
                let ends = gap_ends(gaps, &map);
                self.decode_checked(c, b, frame, inflate, Some((gaps, &ends)))
            }
            List::Breaks(breaks) => {
                let block = self.decode_checked(c, b, frame, inflate, None)?;
                self.samples_inside(c, b, breaks)?;
                Ok(block)
            }
        }
    }

    /// Whether block `b` of analog channel `c` holds its samples inside its ticks, by `breaks`.
    fn samples_inside(&self, c: usize, b: &BlockRef, breaks: &[Break]) -> Result<(), DecodeError> {
        let ch = &self.header.channels[c];
        let ChannelKind::Analog { first, .. } = &ch.data else {
            return Ok(());
        };
        let n = u64::from(b.row.word);
        if n == 0 {
            return Ok(());
        }
        let map = self.maps[c];
        let grid = self.grid(c).expect("a checked header's grid");
        let tick = |index| map.axis_place(grid.sample_place(*first, breaks, index), grid.d());
        if tick(b.first_index).is_none_or(|t| t < b.start)
            || tick(b.first_index + n - 1).is_none_or(|t| t >= b.end)
        {
            return Err(corrupt(format!(
                "channel {}: the block at tick {}: its samples lie outside its ticks",
                ch.name, b.start
            )));
        }
        Ok(())
    }

    fn decode_checked(
        &self,
        c: usize,
        b: &BlockRef,
        frame: &[u8],
        inflate: impl Fn(&[u8], usize) -> Result<Vec<u8>, DecodeError>,
        gaps: Option<(&[Gap], &[i64])>,
    ) -> Result<Block, DecodeError> {
        let ch = self
            .header
            .channels
            .get(c)
            .ok_or_else(|| corrupt(format!("no channel {c}")))?;
        self.check_available(c)?;
        if b.row.decoded_length as usize > MAX_BLOCK {
            return Err(corrupt(format!(
                "channel {}: a block decodes to {} bytes, more than the {MAX_BLOCK} a block holds",
                ch.name, b.row.decoded_length
            )));
        }
        if frame.len() != b.row.length as usize {
            return Err(corrupt(format!(
                "channel {}: a block is {} bytes, the index says {}",
                ch.name,
                frame.len(),
                b.row.length
            )));
        }
        let raw = inflate(frame, b.row.decoded_length as usize)?;
        let section = match &ch.data {
            ChannelKind::Digital { end, .. } => Section::Digital {
                start: self.channel_starts[c],
                end: *end,
                map: self.maps[c],
            },
            ChannelKind::Analog { coding, .. } => Section::Analog {
                coding: *coding,
                first_index: b.first_index,
            },
        };
        let bad = |what: String| {
            corrupt(format!(
                "channel {}, the block at tick {}: {what}",
                ch.name, b.start
            ))
        };
        let data = decode_section(&raw, &section, b.row.word, b.start, b.end).map_err(|e| {
            placed(e, || {
                format!("channel {}, the block at tick {}", ch.name, b.start)
            })
        })?;
        if let (BlockChannel::Digital { initial, .. }, Some((gaps, ends))) = (&data, gaps) {
            let edges = data.source_ticks().unwrap_or_default();
            if let Some(t) = edge_in_gap(edges, gaps) {
                return Err(bad(format!("an edge at source tick {t} lies inside a gap")));
            }
            if let Some(next) = b.next_word {
                let ending = gaps_ending_in(gaps, ends, b.start, b.end);
                if level_after(*initial, edges, ending) != (next & LEVEL_BIT != 0) {
                    return Err(bad("the next block starts at the wrong level".into()));
                }
            }
        }
        Ok(Block {
            channel: c,
            start: b.start,
            end: b.end,
            data,
        })
    }

    /// The annotation bundle's files, its record read through `fetch`; None without one.
    pub fn read_bundle<E: From<DecodeError>>(
        &self,
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Option<BTreeMap<String, String>>, E> {
        let Some((at, len)) = self.bundle else {
            return Ok(None);
        };
        let bytes = fetch(at, len)?;
        let payload = match record::next(&bytes, bytes.len()) {
            record::Next::Record { kind, payload, len }
                if kind == Kind::Bundle as u8 && len == bytes.len() =>
            {
                payload
            }
            _ => return Err(corrupt("the annotation bundle record is not whole").into()),
        };
        Ok(Some(parse_bundle(payload, inflate)?))
    }

    /// Channel `c`'s mapping from its source's ticks to the axis.
    pub fn mapping(&self, c: usize) -> Option<Mapping> {
        self.maps.get(c).copied()
    }

    /// Channel `c`'s start, in its source's ticks.
    pub fn channel_start(&self, c: usize) -> Option<i64> {
        self.channel_starts.get(c).copied()
    }

    /// The axis tick of sample `index` of analog channel `c`, once its breaks are known: the
    /// tick its place maps to.
    pub fn sample_axis_tick(&self, c: usize, index: u64) -> Option<i64> {
        let grid = self.grid(c)?;
        self.mapping(c)?
            .axis_place(self.sample_place(c, index)?, grid.d())
    }

    /// The whole source tick at or before sample `index`'s place, of analog channel `c`, once its
    /// breaks are known.
    pub fn sample_tick(&self, c: usize, index: u64) -> Option<i64> {
        Some(self.grid(c)?.floor(self.sample_place(c, index)?))
    }

    /// Sample `index`'s place, in `1/d` source ticks of analog channel `c`'s grid, once its
    /// breaks are known.
    pub fn sample_place(&self, c: usize, index: u64) -> Option<i128> {
        match &self.header.channels.get(c)?.data {
            ChannelKind::Analog { first, .. } => {
                Some(self.grid(c)?.sample_place(*first, self.breaks(c)?, index))
            }
            ChannelKind::Digital { .. } => None,
        }
    }
}

/// A finished file's whole index, read once: its footer with every list read, and every
/// channel's blocks and every level's rows. What a reader that wants every block starts from.
#[derive(Debug)]
pub struct Index {
    pub footer: Footer,
    /// Each channel's blocks.
    pub blocks: Vec<Vec<BlockRef>>,
    /// Each overview channel's levels in overview blocks, finest first, each its rows.
    pub levels: Vec<Vec<Vec<LevelRef>>>,
}

impl Index {
    /// `footer`'s file's index, its pages read through `fetch`.
    pub fn read<E: From<DecodeError>>(
        mut footer: Footer,
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Self, E> {
        footer.load_lists(&mut fetch)?;
        let blocks = (0..footer.header.channels.len())
            .map(|c| footer.block_refs(c, &mut fetch))
            .collect::<Result<Vec<_>, E>>()?;
        let levels = (0..footer.channels.len())
            .map(|o| {
                (0..footer.channels[o].levels.len())
                    .map(|li| footer.level_refs(o, li, &mut fetch))
                    .collect::<Result<Vec<_>, E>>()
            })
            .collect::<Result<Vec<_>, E>>()?;
        Ok(Self {
            footer,
            blocks,
            levels,
        })
    }

    /// A whole file's index, the file in memory.
    pub fn of_file(file: &[u8]) -> Result<Self, DecodeError> {
        Self::read(Footer::parse_file(file)?, from_memory(file))
    }

    pub fn header(&self) -> &Header {
        &self.footer.header
    }

    /// Channel `c`'s block `i`'s frame in `file`, decoded.
    pub fn block_in(&self, file: &[u8], c: usize, i: usize) -> Result<Block, DecodeError> {
        let b = self
            .blocks
            .get(c)
            .and_then(|b| b.get(i))
            .ok_or_else(|| corrupt(format!("no block {i} of channel {c}")))?;
        let frame = from_memory(file)(b.row.offset, b.row.length)?;
        self.footer.decode_block(c, b, &frame)
    }

    /// Every block of channel `c` in `file`, decoded, in order.
    pub fn channel_blocks(&self, file: &[u8], c: usize) -> Result<Vec<Block>, DecodeError> {
        (0..self.blocks.get(c).map_or(0, Vec::len))
            .map(|i| self.block_in(file, c, i))
            .collect()
    }

    /// Overview channel `o`'s level `li`'s row `r`'s frame in `file`, decoded.
    pub fn overview_in(
        &self,
        file: &[u8],
        o: usize,
        li: usize,
        r: usize,
    ) -> Result<OverviewBlock, DecodeError> {
        let row = self
            .levels
            .get(o)
            .and_then(|l| l.get(li))
            .and_then(|l| l.get(r))
            .ok_or_else(|| corrupt(format!("no row {r} of channel {o}'s level {li}")))?;
        let frame = from_memory(file)(row.row.offset, row.row.length)?;
        self.footer.decode_overview(o, li, row, &frame)
    }

    /// Overview channel `o`'s level `li` from `file`, every bucket, its runs spelled out.
    pub fn level_in(&self, file: &[u8], o: usize, li: usize) -> Result<Level, DecodeError> {
        let (level, _) = &self.footer.channels[o].levels[li];
        let mut out = Level::empty(level.shift, self.footer.channels[o].column);
        for r in 0..self.levels[o][li].len() {
            let b = self.overview_in(file, o, li, r)?;
            for _ in 0..b.n {
                out.append(&b.level);
            }
        }
        Ok(out)
    }
}

/// An overview block record's frame for row `r` of a level of `shift` in a recording of
/// `length`, decoded: its buckets, the size its block holds.
pub(crate) fn decode_overview_frame(
    column: Column,
    shift: u8,
    length: i64,
    r: &LevelRef,
    frame: &[u8],
    inflate: impl Fn(&[u8], usize) -> Result<Vec<u8>, DecodeError>,
) -> Result<OverviewBlock, DecodeError> {
    if frame.len() != r.row.length as usize {
        return Err(corrupt(format!(
            "overview block {} is {} bytes, the index says {}",
            r.k,
            frame.len(),
            r.row.length
        )));
    }
    let (first, n) = overview::block_buckets(length, shift, r.k);
    if u64::from(r.row.decoded_length) != column.decoded_len(n) || n == 0 {
        return Err(corrupt(format!(
            "overview block {}'s decoded length is wrong",
            r.k
        )));
    }
    let raw = inflate(frame, r.row.decoded_length as usize)?;
    let level = Level::parse(&raw, shift, n, column)
        .ok_or_else(|| corrupt(format!("overview block {} does not parse", r.k)))?;
    Ok(OverviewBlock {
        first,
        n: r.row.n,
        level,
    })
}

/// Reads `[offset, offset + len)` of a file held in memory, refusing a range past its end.
pub fn from_memory(file: &[u8]) -> impl FnMut(u64, u32) -> Result<Vec<u8>, DecodeError> + '_ {
    move |offset, len| {
        usize::try_from(offset)
            .ok()
            .and_then(|o| file.get(o..o.checked_add(len as usize)?))
            .map(<[u8]>::to_vec)
            .ok_or_else(|| corrupt(format!("{len} bytes at {offset} lie past the file's end")))
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
) -> Result<(Vec<i64>, Option<Vec<i64>>), DecodeError> {
    digital_edges_in(section, count, map, channel, block).map_err(|e| match e {
        Err(what) => DecodeError::Corrupt(what.to_owned()),
        Ok(r) => r,
    })
}

/// `try_reserve_exact` of `n` ticks, as a `Resource` error where it fails.
fn ticks(n: usize) -> Result<Vec<i64>, Result<DecodeError, &'static str>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| Ok(DecodeError::Resource(format!("{CANT_HOLD} {n} edges"))))?;
    Ok(v)
}

/// `digital_edges`, a format error as `Err(Err(message))`.
#[allow(clippy::type_complexity)]
fn digital_edges_in(
    section: &[u8],
    count: usize,
    map: &Mapping,
    channel: Range<i64>,
    block: Range<i64>,
) -> Result<(Vec<i64>, Option<Vec<i64>>), Result<DecodeError, &'static str>> {
    if count == 0 {
        if !section.is_empty() {
            return Err(Err(
                "bytes in the section of a channel with no edge in the block",
            ));
        }
        return Ok((Vec::new(), None));
    }
    // Every number takes a byte at least: the step, then one an edge.
    if count >= section.len() {
        return Err(Err("the section is too short for its edges"));
    }
    let mut at = 0;
    let step = get_uleb(section, &mut at).ok_or(Err("the section's step is cut short"))?;
    if step == 0 {
        return Err(Err("the section's step is 0"));
    }
    let first = get_uleb(section, &mut at).ok_or(Err("the first edge is cut short"))?;
    let limit = JSON_SAFE;
    const PAST: &str = "an edge lies past source tick 2^53";
    let past = || Err(PAST);
    if first >= limit {
        return Err(past());
    }
    let mut out = ticks(count)?;
    let mut k = first;
    out.push(k as i64);
    let lo = first as i64;
    let cut = || Err("an interval is cut short");
    if step == 1 {
        // Most sections: no multiply on the per-edge path.
        for _ in 1..count {
            let interval = get_uleb(section, &mut at).ok_or_else(cut)?;
            k = k
                .checked_add(interval)
                .filter(|&k| k < limit)
                .ok_or_else(past)?;
            out.push(k as i64);
        }
    } else {
        for _ in 1..count {
            let interval = get_uleb(section, &mut at).ok_or_else(cut)?;
            k = interval
                .checked_mul(step)
                .and_then(|d| k.checked_add(d))
                .filter(|&k| k < limit)
                .ok_or_else(past)?;
            out.push(k as i64);
        }
    }
    if at != section.len() {
        return Err(Err("bytes follow the last edge"));
    }
    let hi = k as i64;
    if lo < channel.start || hi >= channel.end {
        return Err(Err("an edge lies outside its channel's start and end"));
    }
    if map.is_identity() {
        if lo < block.start || hi >= block.end {
            return Err(Err("an edge lies outside the block"));
        }
        return Ok((out, None));
    }
    let mut edges = ticks(count)?;
    for &k in &out {
        match map.axis(k) {
            Some(t) if block.contains(&t) => edges.push(t),
            _ => return Err(Err("an edge lies outside the block")),
        }
    }
    Ok((edges, Some(out)))
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

    fn header(breaks: Vec<Break>) -> Header {
        losses(breaks, 0, 0)
    }

    fn losses(breaks: Vec<Break>, lost_before: u64, lost_after: u64) -> Header {
        Header {
            timebase: Ratio::ONE,
            length: 100,
            channels: vec![ChannelHeader {
                name: "V".into(),
                source_name: "v".into(),
                source: 0,
                data: ChannelKind::Analog {
                    unit: String::new(),
                    first: 20,
                    period: Ratio::integer(10).unwrap(),
                    count: 5,
                    breaks,
                    break_count: None,
                    coding: AnalogCoding::F32,
                    significant: None,
                    lost_before,
                    lost_after,
                    phase: None,
                },
                shaping: Default::default(),
                overview_shift: None,
                blocks: 1,
                overview_blocks: vec![],
                codec: None,
                requires: Vec::new(),
            }],
            derived: Vec::new(),
            sources: vec![SourceHeader::new(SourceFormat::Vcd, Ratio::ONE)],
            title: None,
            meta: Default::default(),
            content_id: format!("blake3:{}", "0".repeat(64)).parse().unwrap(),
            overview_shift: 12,
            annotation_parts: None,
            requires: Vec::new(),
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

    fn b(index: u64, tick: i64) -> Break {
        Break {
            index,
            tick,
            lost: 0,
            phase: 0,
        }
    }

    #[test]
    fn a_break_must_lie_inside_the_samples_and_the_capture() {
        // Samples at 20, 30, 40, 50, 60 without a break.
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
            (vec![b(2, 25)], "before the sample ahead of it, at 30"),
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

    /// FORMAT.md, "Gaps": samples lost before the first sit at places -lost_before to -1 of the
    /// first run's grid, those after the last at places count to count + lost_after - 1 of the
    /// last run's; the first may not start below tick 0, the second may not end past the length.
    #[test]
    fn a_loss_at_either_end_lies_inside_the_capture() {
        // The first sample at 20, every 10 ticks: two lost before start at 0, three at -10.
        assert!(check_header(&losses(vec![], 2, 0)).is_ok());
        let err = check_header(&losses(vec![], 3, 0)).unwrap_err().to_string();
        assert!(err.contains("start below tick 0"), "{err}");
        // The last sample at 60: places 5, 6, 7 end at place 8, 100, the length; 4 end at 110.
        assert!(check_header(&losses(vec![], 0, 3)).is_ok());
        let err = check_header(&losses(vec![], 0, 4)).unwrap_err().to_string();
        assert!(err.contains("end past the length"), "{err}");
        // On the last run's grid: a run at 70 from sample 3 puts sample 4 at 80, and places 5
        // and 6 end at 100.
        assert!(check_header(&losses(vec![b(3, 70)], 0, 1)).is_ok());
        assert!(check_header(&losses(vec![b(3, 70)], 0, 2)).is_err());
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
            let err = read(values, count, block).expect_err(why).to_string();
            assert!(err.contains(why), "{values:?}: {err}");
        }
        // Bytes in the section of a channel with no edge in the block.
        let err = read(&[1], 0, 0..10).expect_err("refused").to_string();
        assert!(err.contains("no edge in the block"), "{err}");
        // An edge before the channel's start.
        let err = digital_edges(&uleb(&[1, 4]), 1, &map, 5..100, 0..10_000)
            .expect_err("refused")
            .to_string();
        assert!(err.contains("outside its channel's start and end"), "{err}");
    }
}
