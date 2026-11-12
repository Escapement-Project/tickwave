//! The random-access decoder. It does no I/O: a caller fetches the file's end, then the bytes of
//! the blocks it needs, by HTTP Range in the browser or from a file natively, and hands them in.

use std::ops::Range;

use ruzstd::decoding::FrameDecoder;

use crate::codec::get_uleb;
use crate::header::{
    AnalogCoding, BlockRow, ChannelKind, Header, JSON_SAFE, LEVEL_BIT, MAGIC, MAJOR, OverviewRow,
    PREAMBLE_LEN, TRAILER_LEN, Trailer, code_value,
};
use crate::model::{Break, Gap, run_sample_tick};
use crate::overview::{self, Level};
use crate::record::{self, Kind};

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

/// Decompresses exactly one zstd frame, which must fill `frame`, come to exactly `len` bytes and
/// carry a checksum that matches. A skippable frame, a frame that names a dictionary, or any byte
/// after the frame is refused, and so is a `len` past `MAX_DECODED`.
pub fn inflate(frame: &[u8], len: usize) -> Result<Vec<u8>, DecodeError> {
    inflate_limited(frame, len, MAX_DECODED)
}

/// `inflate`, holding at most `max` decoded bytes.
pub fn inflate_limited(frame: &[u8], len: usize, max: usize) -> Result<Vec<u8>, DecodeError> {
    let mut out = buffer(len, max)?;
    let mut decoder = FrameDecoder::new();
    decoder
        .decode_all_to_vec(frame, &mut out)
        .map_err(|e| DecodeError::Zstd(e.to_string()))?;
    // decode_all reads every frame in its input, and steps over skippable ones, but the checksum
    // getters describe only the last frame: a valid frame after a corrupted one would hide it. The
    // last frame's byte count covers the whole input only when it is the only frame.
    if decoder.bytes_read_from_source() != frame.len() as u64 {
        return Err(corrupt("the bytes hold more than one frame"));
    }
    match (
        decoder.get_checksum_from_data(),
        decoder.get_calculated_checksum(),
    ) {
        (Some(stored), Some(computed)) if stored == computed => {}
        (Some(_), Some(_)) => return Err(DecodeError::Zstd("checksum mismatch".into())),
        _ => return Err(corrupt("a frame carries no checksum")),
    }
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
        edges: Vec<i64>,
    },
    Analog {
        /// The channel's index of the block's first sample.
        first_index: u64,
        values: Vec<f32>,
    },
}

#[derive(Debug, PartialEq)]
pub struct Block {
    pub start: i64,
    /// Exclusive: the next block's start, or the length for the last block.
    pub end: i64,
    pub channels: Vec<BlockChannel>,
}

/// The tick of the last of a run's `n` samples, the first at `tick`, in u128 so that an absurd
/// count or period cannot wrap into a tick inside the capture.
fn run_last(tick: i64, period: crate::model::Ratio, n: u64) -> u128 {
    tick as u128
        + u128::from(n.saturating_sub(1)) * u128::from(period.num()) / u128::from(period.den())
}

/// What the header promises about the capture's extent, checked before anything relies on it.
pub(crate) fn check_header(h: &Header) -> Result<(), DecodeError> {
    if !(1..JSON_SAFE as i64).contains(&h.length) {
        return Err(corrupt("the length covers no tick or is past 2^53"));
    }
    // The page reads the header with JSON.parse: past 2^53 it would read another number.
    let safe = |v: u64| v < JSON_SAFE;
    if !safe(h.timebase.num()) || !safe(h.timebase.den()) {
        return Err(corrupt("the timebase has a term past 2^53"));
    }
    if h.overview_shift > overview::MAX_SHIFT {
        return Err(corrupt("the finest overview level's shift is past 62"));
    }
    for c in &h.channels {
        let name = &c.name;
        match &c.data {
            ChannelKind::Digital {
                initial,
                end,
                edges,
                gaps,
            } => {
                if !(0..=h.length).contains(end) {
                    return Err(corrupt(format!("channel {name} ends outside the capture")));
                }
                if *end == 0 && *initial {
                    return Err(corrupt(format!(
                        "channel {name} is never known but has a high initial level"
                    )));
                }
                if !safe(*edges) {
                    return Err(corrupt(format!("channel {name}'s edge count is past 2^53")));
                }
                let mut after = -1;
                for g in gaps {
                    if g.from <= after || g.to <= g.from || g.to >= *end {
                        return Err(corrupt(format!(
                            "channel {name}: a gap from {} to {} is out of place",
                            g.from, g.to
                        )));
                    }
                    if g.from == 0 && *initial {
                        return Err(corrupt(format!(
                            "channel {name} has a gap from tick 0 but a high initial level"
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
                coding,
                ..
            } => {
                if !safe(period.num()) || !safe(period.den()) || !safe(*count) {
                    return Err(corrupt(format!(
                        "channel {name}'s period or count is past 2^53"
                    )));
                }
                let outside =
                    || corrupt(format!("channel {name}'s samples lie outside the capture"));
                if *first < 0 || *first >= h.length {
                    return Err(outside());
                }
                // Each run's last sample: before the next run's tick, and inside the capture.
                let mut run = Break {
                    index: 0,
                    tick: *first,
                };
                for b in breaks {
                    if b.index <= run.index || b.index >= *count || b.tick < 0 || b.tick >= h.length
                    {
                        return Err(corrupt(format!("channel {name}: a break is out of place")));
                    }
                    let last = run_last(run.tick, *period, b.index - run.index);
                    if last >= h.length as u128 || last > b.tick as u128 {
                        return Err(outside());
                    }
                    run = *b;
                }
                if *count > run.index
                    && run_last(run.tick, *period, count - run.index) >= h.length as u128
                {
                    return Err(outside());
                }
                if let AnalogCoding::Codes { bits, .. } = coding
                    && !(1..=16).contains(bits)
                {
                    return Err(corrupt(format!("channel {name}'s codes are {bits} bits")));
                }
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
    /// Every edge lies before `end`, the channel's end, or `i64::MAX` while it is not known.
    Digital { end: i64 },
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
            Section::Digital { end: channel_end } => BlockChannel::Digital {
                initial: word & LEVEL_BIT != 0,
                edges: digital_edges(
                    section,
                    item_count(word, true) as usize,
                    start,
                    end,
                    *channel_end,
                )?,
            },
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

/// The gaps of `gaps` whose `to` lies in `start..end`.
pub(crate) fn gaps_ending_in(gaps: &[Gap], start: i64, end: i64) -> &[Gap] {
    let a = gaps.partition_point(|g| g.to < start);
    let b = gaps.partition_point(|g| g.to < end);
    &gaps[a..b]
}

/// A finished file's footer: the header, the index and the overview. Parsed from the file's last
/// bytes, it says where every block is.
#[derive(Debug)]
pub struct Footer {
    pub trailer: Trailer,
    pub header: Header,
    pub rows: Vec<BlockRow>,
    pub overview: Vec<Level>,
    /// For each level in `header.overview_blocks`, its overview blocks' places in the file.
    pub overview_rows: Vec<Vec<OverviewRow>>,
    /// The footer's overview, as the file holds it: one zstd frame.
    pub overview_frame: Vec<u8>,
    /// For each analog channel, the index of its first sample in each block.
    first_sample: Vec<Vec<u64>>,
    analog: Vec<bool>,
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
        let rows: Vec<BlockRow> = payload[header_end..header_end + data_len]
            .chunks_exact(row_size)
            .map(|row| BlockRow::parse(row, channels))
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
        let analog: Vec<bool> = header
            .channels
            .iter()
            .map(|c| matches!(c.data, ChannelKind::Analog { .. }))
            .collect();
        let overview_len: u64 = header
            .overview
            .iter()
            .map(|&shift| overview::decoded_len(overview::buckets(header.length, shift), &analog))
            .sum();
        let overview_len = usize::try_from(overview_len).map_err(|_| {
            corrupt(format!(
                "the overview decodes to {overview_len} bytes, more than this reader can hold"
            ))
        })?;
        let overview_frame = payload[index_end..].to_vec();
        let raw = inflate(&overview_frame, overview_len)?;
        let overview = overview::parse(&raw, &header.overview, header.length, &analog)
            .ok_or_else(|| corrupt("the overview does not match its levels"))?;
        let end_of = |i: usize| rows.get(i + 1).map_or(header.length, |r| r.start);
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
                let word = row.channels[c];
                if let (Some(l), ChannelKind::Digital { gaps, .. }) = (&mut level, &kind.data) {
                    let bit = word & LEVEL_BIT != 0;
                    let known =
                        i == 0 || gaps_ending_in(gaps, rows[i - 1].start, row.start).is_empty();
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
                for (i, row) in rows.iter().enumerate() {
                    let (from, to) = (starts[i], starts.get(i + 1).copied().unwrap_or(total));
                    let tick = |index| run_sample_tick(*first, *period, breaks, index);
                    if to > from && (tick(from) < row.start || tick(to - 1) >= end_of(i)) {
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
            overview,
            overview_rows,
            overview_frame,
            first_sample,
            analog,
        })
    }

    /// The tick of sample `index` of analog channel `c`.
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
        if u64::from(row.decoded_length) != overview::decoded_len(n, &self.analog) {
            return Err(corrupt(format!(
                "overview block {k}'s decoded length is wrong"
            )));
        }
        let raw = inflate(frame, row.decoded_length as usize)?;
        let level = overview::parse_block(&raw, shift, n, &self.analog)
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
                ChannelKind::Digital { end, .. } => Section::Digital { end: *end },
                ChannelKind::Analog { coding, .. } => Section::Analog {
                    coding: *coding,
                    first_index: self.first_sample[c][i],
                },
            })
            .collect();
        let channels = decode_sections(&raw, &sections, &row.channels, start, end)
            .map_err(|what| corrupt(format!("block {i}: {what}")))?;
        for (c, (got, kind)) in channels.iter().zip(&self.header.channels).enumerate() {
            if let (BlockChannel::Digital { initial, edges }, ChannelKind::Digital { gaps, .. }) =
                (got, &kind.data)
            {
                let bad =
                    |what: String| corrupt(format!("block {i}, channel {}: {what}", kind.name));
                if let Some(t) = edge_in_gap(edges, gaps) {
                    return Err(bad(format!("an edge at tick {t} lies inside a gap")));
                }
                if let Some(next) = self.rows.get(i + 1) {
                    let after = level_after(*initial, edges, gaps_ending_in(gaps, start, end));
                    if after != (next.channels[c] & LEVEL_BIT != 0) {
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

/// A digital section's `count` edges: LEB128 intervals, the first from the block's `start`. Every
/// edge lies in `1..channel_end` and before the block's `end`.
#[inline(never)]
fn digital_edges(
    section: &[u8],
    count: usize,
    start: i64,
    end: i64,
    channel_end: i64,
) -> Result<Vec<i64>, String> {
    if count > section.len() {
        return Err("the section is too short for its edges".into());
    }
    let mut edges = Vec::with_capacity(count);
    let mut at = 0;
    let mut tick = start;
    for _ in 0..count {
        let interval = get_uleb(section, &mut at).ok_or("an interval is cut short")?;
        tick = i64::try_from(interval)
            .ok()
            .and_then(|iv| tick.checked_add(iv))
            .ok_or("an interval overflows")?;
        if tick >= end || tick >= channel_end || tick < 1 {
            return Err("an edge lies outside the block".into());
        }
        edges.push(tick);
    }
    if at != section.len() {
        return Err("bytes follow the last edge".into());
    }
    Ok(edges)
}

/// An analog section's `count` values in `coding`.
#[inline(never)]
fn analog_values(section: &[u8], count: usize, coding: AnalogCoding) -> Result<Vec<f32>, String> {
    if count.checked_mul(coding.sample_len()) != Some(section.len()) {
        return Err("the section does not hold its samples".into());
    }
    Ok(match coding {
        AnalogCoding::F32 => section
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| f32::from_le_bytes(b))
            .collect(),
        AnalogCoding::Codes {
            bits,
            scale,
            offset,
        } => {
            let (low, high) = section.split_at(count);
            let code = |(&lo, &hi): (&u8, &u8)| u16::from_le_bytes([lo, hi]);
            // One OR over every code, apart from the conversion, so both loops stay vectorised.
            let all = low.iter().zip(high).fold(0, |a, c| a | code(c));
            if u32::from(all) >> bits != 0 {
                let wide = low
                    .iter()
                    .zip(high)
                    .map(code)
                    .find(|&c| u32::from(c) >> bits != 0)
                    .unwrap_or_default();
                return Err(format!("code {wide} is wider than {bits} bits"));
            }
            low.iter()
                .zip(high)
                .map(|c| code_value(code(c), scale, offset))
                .collect()
        }
    })
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
                data: ChannelKind::Analog {
                    unit: String::new(),
                    first: 0,
                    period: Ratio::integer(10).unwrap(),
                    count: 5,
                    breaks,
                    coding: AnalogCoding::F32,
                },
            }],
            source: SourceHeader::new(SourceFormat::Vcd),
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
        let b = |index, tick| Break { index, tick };
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
                vec![b(2, 80)],
                "a run that ends past the length: 80, 90, 100",
            ),
        ] {
            assert!(check_header(&header(breaks)).is_err(), "{why}");
        }
    }
}
