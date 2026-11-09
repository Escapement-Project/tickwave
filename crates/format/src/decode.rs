//! The decoder. It does no I/O: a caller fetches the head, then the bytes of the blocks it needs,
//! by HTTP Range in the browser or from a file natively, and hands them in.

use std::ops::Range;

use ruzstd::decoding::FrameDecoder;

use crate::codec::get_uleb;
use crate::hash::{self, ChannelHasher};
use crate::header::{
    AnalogCoding, BlockRow, ChannelKind, Header, JSON_SAFE, LEVEL_BIT, MAGIC, MAJOR, OverviewRow,
    PREAMBLE_LEN, Preamble, code_value,
};
use crate::model::{ChannelData, Ratio, Recording, Samples, sample_tick};
use crate::overview::{self, AnalogBuilder, DigitalBuilder, Level};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("not a tickwave recording")]
    NotTickwave,
    #[error("format version {major}.{minor}: this reader reads major version {MAJOR}")]
    Version { major: u16, minor: u16 },
    #[error("the head needs {0} bytes")]
    NeedMore(usize),
    #[error("header: {0}")]
    Header(#[from] serde_json::Error),
    #[error("{0}")]
    Corrupt(String),
    #[error("zstd: {0}")]
    Zstd(String),
}

fn corrupt(what: impl Into<String>) -> DecodeError {
    DecodeError::Corrupt(what.into())
}

/// A file whose length is not the one its preamble needs.
pub fn cut_short(len: usize, needs: u64) -> DecodeError {
    corrupt(format!(
        "the file is {len} bytes, where its preamble needs {needs}"
    ))
}

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
/// or the head's overview. The format sets no maximum, and a file can state up to 4 GiB.
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

pub struct Head {
    pub preamble: Preamble,
    pub header: Header,
    pub rows: Vec<BlockRow>,
    pub overview: Vec<Level>,
    /// For each level in `header.overview_blocks`, its overview blocks' places in the file.
    pub overview_rows: Vec<Vec<OverviewRow>>,
    /// For each analog channel, the index of its first sample in each block.
    first_sample: Vec<Vec<u64>>,
    analog: Vec<bool>,
}

/// One overview block, decoded: buckets `first..first + level.buckets` of its level.
pub struct OverviewBlock {
    pub first: u64,
    pub level: Level,
}

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

pub struct Block {
    pub start: i64,
    /// Exclusive; `i64::MAX` for the last block.
    pub end: i64,
    pub channels: Vec<BlockChannel>,
}

/// What the header promises about the capture's extent, checked before anything relies on it.
pub(crate) fn check_header(h: &Header) -> Result<(), DecodeError> {
    if !(0..JSON_SAFE as i64).contains(&h.length) {
        return Err(corrupt("the length is negative or past 2^53"));
    }
    if h.timebase.contains(&0) {
        return Err(corrupt("the timebase has a zero term"));
    }
    // The page reads the header with JSON.parse: past 2^53 it would read another number.
    let safe = |v: u64| v < JSON_SAFE;
    if !h.timebase.iter().all(|&t| safe(t)) {
        return Err(corrupt("the timebase has a term past 2^53"));
    }
    for c in &h.channels {
        let name = &c.name;
        match &c.data {
            ChannelKind::Digital { end, edges, .. } => {
                if !(0..=h.length).contains(end) {
                    return Err(corrupt(format!("channel {name} ends outside the capture")));
                }
                if !safe(*edges) {
                    return Err(corrupt(format!("channel {name}'s edge count is past 2^53")));
                }
            }
            ChannelKind::Analog {
                first,
                period,
                count,
                coding,
                ..
            } => {
                if period.contains(&0) {
                    return Err(corrupt(format!("channel {name}'s period has a zero term")));
                }
                if !period.iter().all(|&t| safe(t)) || !safe(*count) {
                    return Err(corrupt(format!(
                        "channel {name}'s period or count is past 2^53"
                    )));
                }
                // In u128: an absurd count or period must not wrap into a tick inside the capture.
                let last = u128::from(count.saturating_sub(1)) * u128::from(period[0])
                    / u128::from(period[1]);
                if *first < 0 || *first > h.length || *first as u128 + last > h.length as u128 {
                    return Err(corrupt(format!(
                        "channel {name}'s samples lie outside the capture"
                    )));
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

impl Head {
    /// Parses the head from the file's first bytes. `NeedMore` says how many bytes the head
    /// takes when `bytes` holds fewer.
    pub fn parse(bytes: &[u8]) -> Result<Self, DecodeError> {
        Self::parse_with(bytes, inflate)
    }

    /// `parse` with another zstd decoder, which must do what this module's `inflate` does.
    /// Parses a whole file's head. A file shorter or longer than its preamble says is refused, so
    /// its blocks can be cut from it as the index places them.
    pub fn parse_file(file: &[u8]) -> Result<Self, DecodeError> {
        Self::parse_file_limited(file, MAX_DECODED)
    }

    /// `parse_file`, holding at most `max` decoded bytes a frame.
    pub fn parse_file_limited(file: &[u8], max: usize) -> Result<Self, DecodeError> {
        let head =
            Self::parse_with(file, |f, len| inflate_limited(f, len, max)).map_err(|e| match e {
                DecodeError::NeedMore(n) => cut_short(file.len(), n as u64),
                e => e,
            })?;
        if file.len() as u64 != head.preamble.file_len {
            return Err(cut_short(file.len(), head.preamble.file_len));
        }
        Ok(head)
    }

    pub fn parse_with(
        bytes: &[u8],
        inflate: impl Fn(&[u8], usize) -> Result<Vec<u8>, DecodeError>,
    ) -> Result<Self, DecodeError> {
        if bytes.len() < PREAMBLE_LEN {
            return if MAGIC.starts_with(&bytes[..bytes.len().min(8)]) {
                Err(DecodeError::NeedMore(PREAMBLE_LEN))
            } else {
                Err(DecodeError::NotTickwave)
            };
        }
        let preamble = Preamble::parse(bytes).ok_or(DecodeError::NotTickwave)?;
        if preamble.major != MAJOR {
            return Err(DecodeError::Version {
                major: preamble.major,
                minor: preamble.minor,
            });
        }
        // In u64: on a 32-bit usize, a corrupt length would wrap into a plausible one. Checked before
        // asking for the head, so a reader never fetches the bytes a corrupt preamble names.
        let header_end = PREAMBLE_LEN as u64 + u64::from(preamble.header_len);
        let index_end = header_end + u64::from(preamble.index_len);
        if index_end > u64::from(preamble.head_len)
            || u64::from(preamble.head_len) > preamble.file_len
        {
            return Err(corrupt("the preamble's lengths do not fit together"));
        }
        let head_len = preamble.head_len as usize;
        if bytes.len() < head_len {
            return Err(DecodeError::NeedMore(head_len));
        }
        // Each is at most head_len, which the bytes in hand hold.
        let (header_end, index_end) = (header_end as usize, index_end as usize);
        let header: Header = serde_json::from_slice(&bytes[PREAMBLE_LEN..header_end])?;
        check_header(&header)?;
        // The levels follow from the length alone. Holding the header to them bounds every level's
        // size, and their count, before anything is allocated for them.
        let plan = overview::plan(header.length);
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
        if u64::from(preamble.index_len)
            != u64::from(header.blocks) * row_size as u64
                + overview_count * OverviewRow::SIZE as u64
        {
            return Err(corrupt("the index does not hold one row a block"));
        }
        let data_len = header.blocks as usize * row_size;
        let rows: Vec<BlockRow> = bytes[header_end..header_end + data_len]
            .chunks_exact(row_size)
            .map(|row| BlockRow::parse(row, channels))
            .collect();
        let mut expect_offset = head_len as u64;
        for (i, row) in rows.iter().enumerate() {
            if row.offset != expect_offset
                || (i > 0 && row.start <= rows[i - 1].start)
                || row.start > header.length
            {
                return Err(corrupt(format!("block {i}'s row is out of place")));
            }
            expect_offset += u64::from(row.length);
        }
        let mut overview_rows = Vec::with_capacity(header.overview_blocks.len());
        let mut pos = header_end + data_len;
        for level in &header.overview_blocks {
            let mut level_rows = Vec::with_capacity(level.blocks as usize);
            for _ in 0..level.blocks {
                let row = OverviewRow::parse(&bytes[pos..pos + OverviewRow::SIZE]);
                pos += OverviewRow::SIZE;
                if row.offset != expect_offset {
                    return Err(corrupt("an overview block's row is out of place"));
                }
                expect_offset += u64::from(row.length);
                level_rows.push(row);
            }
            overview_rows.push(level_rows);
        }
        if expect_offset != preamble.file_len || rows.first().is_some_and(|r| r.start != 0) {
            return Err(corrupt("the blocks do not fill the file"));
        }
        if header.overview.is_empty() {
            return Err(corrupt("the header lists no overview level"));
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
        let raw = inflate(&bytes[index_end..head_len], overview_len)?;
        let overview = overview::parse(&raw, &header.overview, header.length, &analog)
            .ok_or_else(|| corrupt("the overview does not match its levels"))?;
        let mut first_sample = vec![Vec::new(); channels];
        for (c, kind) in header.channels.iter().enumerate() {
            let mut total = 0u64;
            let mut starts = Vec::with_capacity(rows.len());
            // A digital channel's level at each block's start follows from `initial` and the edge
            // counts before it; the decoder trusts the index's bit, so check it here.
            let mut level = match kind.data {
                ChannelKind::Digital { initial, .. } => Some(initial),
                ChannelKind::Analog { .. } => None,
            };
            for (i, row) in rows.iter().enumerate() {
                let word = row.channels[c];
                if let Some(l) = &mut level {
                    if (word & LEVEL_BIT != 0) != *l {
                        return Err(corrupt(format!(
                            "channel {}: block {i} starts at the wrong level",
                            kind.name
                        )));
                    }
                    *l ^= word & 1 != 0;
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
                    "channel {}: the index counts {total} items, the header {promised}",
                    kind.name
                )));
            }
            if let ChannelKind::Analog { first, period, .. } = &kind.data {
                // A view fetches blocks by tick, so a sample in a block that does not cover its
                // tick would never be drawn.
                let period = Ratio {
                    num: period[0],
                    den: period[1],
                };
                for (i, row) in rows.iter().enumerate() {
                    let (from, to) = (starts[i], starts.get(i + 1).copied().unwrap_or(total));
                    let end = rows.get(i + 1).map_or(i64::MAX, |r| r.start);
                    if to > from
                        && (sample_tick(*first, period, from) < row.start
                            || sample_tick(*first, period, to - 1) >= end)
                    {
                        return Err(corrupt(format!(
                            "channel {}: block {i}'s samples lie outside its ticks",
                            kind.name
                        )));
                    }
                }
                first_sample[c] = starts;
            }
        }
        Ok(Self {
            preamble,
            header,
            rows,
            overview,
            overview_rows,
            first_sample,
            analog,
        })
    }

    /// Where overview block `k` of `header.overview_blocks[level]` sits: offset and length.
    pub fn overview_block_location(&self, level: usize, k: usize) -> (u64, u32) {
        let row = self.overview_rows[level][k];
        (row.offset, row.length)
    }

    /// The overview blocks of `header.overview_blocks[level]` holding any tick in `[start, end)`.
    pub fn overview_blocks_covering(&self, level: usize, start: i64, end: i64) -> Range<usize> {
        let shift = self.header.overview_blocks[level].shift;
        let length = self.header.length.max(0);
        // Every tick in 0..=length falls in a block, so clamping keeps the range inside the level.
        let block = |tick: i64| {
            ((tick.clamp(0, length) as u64 >> shift) / overview::BLOCK_BUCKETS) as usize
        };
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

    pub fn block_end(&self, i: usize) -> i64 {
        self.rows.get(i + 1).map_or(i64::MAX, |r| r.start)
    }

    /// The blocks holding any tick in `[start, end)`.
    pub fn blocks_covering(&self, start: i64, end: i64) -> Range<usize> {
        let first = self.rows.partition_point(|r| r.start <= start).max(1) - 1;
        let last = self.rows.partition_point(|r| r.start < end);
        first..last.max(first + 1).min(self.rows.len())
    }

    /// Where block `i` sits in the file: offset and length.
    pub fn block_location(&self, i: usize) -> (u64, u32) {
        (self.rows[i].offset, self.rows[i].length)
    }

    /// Decodes block `i` from exactly its bytes.
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
        let channels = self.header.channels.len();
        let lengths: Vec<usize> = raw
            .get(..4 * channels)
            .ok_or_else(|| corrupt(format!("block {i} is too short for its section table")))?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&c| u32::from_le_bytes(c) as usize)
            .collect();
        // In u64: on a 32-bit usize the lengths could wrap to the block's size.
        if 4 * channels as u64 + lengths.iter().map(|&l| l as u64).sum::<u64>() != raw.len() as u64
        {
            return Err(corrupt(format!("block {i}'s sections do not fill it")));
        }
        let mut pos = 4 * channels;
        let mut out = Vec::with_capacity(channels);
        for (c, (kind, len)) in self.header.channels.iter().zip(lengths).enumerate() {
            let section = &raw[pos..pos + len];
            pos += len;
            let word = row.channels[c];
            let count = item_count(word, matches!(kind.data, ChannelKind::Digital { .. })) as usize;
            let bad = |what: &str| corrupt(format!("block {i}, channel {}: {what}", kind.name));
            out.push(match &kind.data {
                ChannelKind::Digital {
                    end: channel_end, ..
                } => BlockChannel::Digital {
                    initial: word & LEVEL_BIT != 0,
                    edges: digital_edges(section, count, start, end, *channel_end)
                        .map_err(|what| bad(&what))?,
                },
                ChannelKind::Analog { coding, .. } => BlockChannel::Analog {
                    first_index: self.first_sample[c][i],
                    values: analog_values(section, count, *coding).map_err(|what| bad(&what))?,
                },
            });
        }
        Ok(Block {
            start,
            end,
            channels: out,
        })
    }
}

/// A channel's item count from its index word: a digital word keeps the level in its top bit, and
/// an analog word is all count.
fn item_count(word: u32, digital: bool) -> u32 {
    if digital { word & !LEVEL_BIT } else { word }
}

// The two section decoders are kept out of line so that each loop compiles on its own: while they
// shared a function, a change to the analog loop moved the digital loop's speed by 5-7 %.

/// A digital section's `count` edges: LEB128 intervals, the first from the block's `start`. Every
/// edge lies in `1..=channel_end` and before the block's `end`, and no two share a tick.
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
    for k in 0..count {
        let interval = get_uleb(section, &mut at).ok_or("an interval is cut short")?;
        if k > 0 && interval == 0 {
            return Err("two edges share a tick".into());
        }
        tick = i64::try_from(interval)
            .ok()
            .and_then(|iv| tick.checked_add(iv))
            .ok_or("an interval overflows")?;
        if tick >= end || tick > channel_end || tick < 1 {
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

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("{0}")]
    Mismatch(String),
}

fn mismatch(what: impl Into<String>) -> VerifyError {
    VerifyError::Mismatch(what.into())
}

/// Checks that `file` holds exactly what `source` holds: the header's description of every
/// channel, every edge tick and every sample's bits, and a content id that matches its content.
pub fn verify(file: &[u8], source: Recording) -> Result<(), VerifyError> {
    verify_limited(file, source, MAX_DECODED)
}

/// `verify`, holding at most `max` decoded bytes a frame.
pub fn verify_limited(file: &[u8], source: Recording, max: usize) -> Result<(), VerifyError> {
    let head = Head::parse_file_limited(file, max)?;
    let inflate = |f: &[u8], len| inflate_limited(f, len, max);
    let h = &head.header;
    let Ratio { num, den } = source.timebase;
    if h.timebase != [num, den] || h.length != source.length {
        return Err(mismatch("the timebase or the length differs"));
    }
    if h.channels.len() != source.channels.len() {
        return Err(mismatch("the channel count differs"));
    }
    // The levels are the plan's: Head::parse refuses any others.
    let plan = overview::plan(h.length);
    let finest = plan.finest();
    enum Want {
        Digital(Box<dyn Iterator<Item = i64> + Send>, bool, DigitalBuilder),
        Analog(
            Box<dyn Iterator<Item = f32> + Send>,
            AnalogBuilder,
            i64,
            Ratio,
        ),
    }
    let mut wants = Vec::new();
    let mut hashers = Vec::new();
    for (header, channel) in h.channels.iter().zip(source.channels) {
        let name = &channel.name;
        if header.name != *name || header.source_name != channel.source_name {
            return Err(mismatch(format!("channel {name}: the names differ")));
        }
        match (&header.data, channel.data) {
            (ChannelKind::Digital { initial, end, .. }, ChannelData::Digital(d)) => {
                if *initial != d.initial || *end != d.end {
                    return Err(mismatch(format!("channel {name}: initial or end differs")));
                }
                hashers.push(ChannelHasher::digital(name, d.initial, d.end));
                let builder = DigitalBuilder::new(h.length, finest, d.initial);
                wants.push(Want::Digital(d.edges, d.initial, builder));
            }
            (
                ChannelKind::Analog {
                    unit,
                    first,
                    period,
                    count,
                    ..
                },
                ChannelData::Analog(a),
            ) => {
                if *unit != a.unit
                    || *first != a.first
                    || *period != [a.period.num, a.period.den]
                    || *count != a.count
                {
                    return Err(mismatch(format!("channel {name}: the sampling differs")));
                }
                hashers.push(ChannelHasher::analog(name, unit, *first, *period, *count));
                let Samples::F32(samples) = a.samples;
                let builder = AnalogBuilder::new(h.length, finest);
                wants.push(Want::Analog(samples, builder, a.first, a.period));
            }
            _ => return Err(mismatch(format!("channel {name}: the kind differs"))),
        }
    }
    for i in 0..head.rows.len() {
        let (offset, len) = head.block_location(i);
        let frame = &file[offset as usize..offset as usize + len as usize];
        let block = head.decode_block_with(i, frame, inflate)?;
        for (c, got) in block.channels.into_iter().enumerate() {
            let name = &h.channels[c].name;
            match (got, &mut wants[c]) {
                (BlockChannel::Digital { initial, edges }, Want::Digital(want, level, ov)) => {
                    if initial != *level {
                        return Err(mismatch(format!(
                            "channel {name}, block {i}: the starting level differs"
                        )));
                    }
                    for tick in edges {
                        if want.next() != Some(tick) {
                            return Err(mismatch(format!(
                                "channel {name}, block {i}: edge at tick {tick} differs"
                            )));
                        }
                        hashers[c].edge(tick);
                        ov.edge(tick);
                        *level = !*level;
                    }
                }
                (
                    BlockChannel::Analog {
                        values,
                        first_index,
                    },
                    Want::Analog(want, ov, first, period),
                ) => {
                    for (index, value) in (first_index..).zip(values) {
                        if want.next().map(f32::to_bits) != Some(value.to_bits()) {
                            return Err(mismatch(format!(
                                "channel {name}, block {i}: a sample differs"
                            )));
                        }
                        hashers[c].sample(value);
                        ov.sample(sample_tick(*first, *period, index), value);
                    }
                }
                _ => unreachable!("decode_block follows the header's kinds"),
            }
        }
    }
    let mut finest_channels = Vec::with_capacity(wants.len());
    for (c, want) in wants.into_iter().enumerate() {
        let (left, channel) = match want {
            Want::Digital(mut it, _, ov) => (it.next().is_some(), ov.finish()),
            Want::Analog(mut it, ov, ..) => (it.next().is_some(), ov.finish()),
        };
        if left {
            return Err(mismatch(format!(
                "channel {}: the source holds more than the file",
                h.channels[c].name
            )));
        }
        finest_channels.push(channel);
    }
    let digests: Vec<_> = hashers.into_iter().map(ChannelHasher::finish).collect();
    if hash::content_id(h.timebase, h.length, &digests) != h.content_id {
        return Err(mismatch("the content id does not match the content"));
    }
    let finest = Level {
        shift: finest,
        buckets: overview::buckets(h.length, finest),
        channels: finest_channels,
    };
    let level_at = |shift: u8| {
        if shift == finest.shift {
            finest.clone()
        } else {
            overview::coarsen(&finest, shift, h.length)
        }
    };
    for (stored, &shift) in head.overview.iter().zip(&plan.head) {
        if *stored != level_at(shift) {
            return Err(mismatch(format!(
                "the head's overview level 2^{shift} differs from the content"
            )));
        }
    }
    for (l, &shift) in plan.blocks.iter().enumerate() {
        let level = level_at(shift);
        for k in 0..head.overview_rows[l].len() {
            let (offset, len) = head.overview_block_location(l, k);
            let frame = &file[offset as usize..offset as usize + len as usize];
            let block = head.decode_overview_block_with(l, k, frame, inflate)?;
            if block.level != level.slice(block.first, block.level.buckets) {
                return Err(mismatch(format!(
                    "overview level 2^{shift}, block {k}: differs from the content"
                )));
            }
        }
    }
    Ok(())
}
