//! The decoder. It does no I/O: a caller fetches the head, then the bytes of the blocks it needs,
//! by HTTP Range in the browser or from a file natively, and hands them in.

use std::ops::Range;

use ruzstd::decoding::FrameDecoder;

use crate::codec::get_uleb;
use crate::hash::{self, ChannelHasher};
use crate::header::{
    AnalogCoding, BlockRow, ChannelKind, Header, LEVEL_BIT, MAGIC, MAJOR, OverviewRow,
    PREAMBLE_LEN, Preamble,
};
use crate::model::{ChannelData, Ratio, Recording, Samples, sample_tick};
use crate::overview::{self, AnalogBuilder, DigitalBuilder, Level};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("not a chrono recording")]
    NotChrono,
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

/// Decompresses one zstd frame that must come to exactly `len` bytes and carry a checksum that
/// matches.
pub fn inflate(frame: &[u8], len: usize) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(len);
    let mut decoder = FrameDecoder::new();
    decoder
        .decode_all_to_vec(frame, &mut out)
        .map_err(|e| DecodeError::Zstd(e.to_string()))?;
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

impl Head {
    /// Parses the head from the file's first bytes. `NeedMore` says how many bytes the head
    /// takes when `bytes` holds fewer.
    pub fn parse(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.len() < PREAMBLE_LEN {
            return if MAGIC.starts_with(&bytes[..bytes.len().min(8)]) {
                Err(DecodeError::NeedMore(PREAMBLE_LEN))
            } else {
                Err(DecodeError::NotChrono)
            };
        }
        let preamble = Preamble::parse(bytes).ok_or(DecodeError::NotChrono)?;
        if preamble.major != MAJOR {
            return Err(DecodeError::Version {
                major: preamble.major,
                minor: preamble.minor,
            });
        }
        let head_len = preamble.head_len as usize;
        if bytes.len() < head_len {
            return Err(DecodeError::NeedMore(head_len));
        }
        let header_end = PREAMBLE_LEN + preamble.header_len as usize;
        let index_end = header_end + preamble.index_len as usize;
        if index_end > head_len || head_len as u64 > preamble.file_len {
            return Err(corrupt("the preamble's lengths do not fit together"));
        }
        let header: Header = serde_json::from_slice(&bytes[PREAMBLE_LEN..header_end])?;
        // Bounds every level's size before anything is allocated for it, whatever the header says.
        let too_fine =
            |shift: u8, limit: u64| shift > 62 || overview::buckets(header.length, shift) > limit;
        if header
            .overview
            .iter()
            .any(|&s| too_fine(s, overview::HEAD_BUCKETS))
            || header
                .overview_blocks
                .iter()
                .any(|l| too_fine(l.shift, overview::BLOCK_LEVEL_BUCKETS))
        {
            return Err(corrupt(
                "an overview level has more buckets than the format allows",
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
            if row.offset != expect_offset || (i > 0 && row.start <= rows[i - 1].start) {
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
        let overview_len: usize = header
            .overview
            .iter()
            .map(|&shift| overview::decoded_len(overview::buckets(header.length, shift), &analog))
            .sum();
        let raw = inflate(&bytes[index_end..head_len], overview_len)?;
        let overview = overview::parse(&raw, &header.overview, header.length, &analog)
            .ok_or_else(|| corrupt("the overview does not match its levels"))?;
        let mut first_sample = vec![Vec::new(); channels];
        for (c, kind) in header.channels.iter().enumerate() {
            let mut total = 0u64;
            let mut starts = Vec::with_capacity(rows.len());
            for row in &rows {
                starts.push(total);
                total += u64::from(row.channels[c] & !LEVEL_BIT);
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
            if analog[c] {
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
        let row = self.overview_rows[level][k];
        if frame.len() != row.length as usize {
            return Err(corrupt(format!(
                "overview block {k} is {} bytes, the index says {}",
                frame.len(),
                row.length
            )));
        }
        let shift = self.header.overview_blocks[level].shift;
        let (first, n) = overview::block_buckets(self.header.length, shift, k as u64);
        if row.decoded_length as usize != overview::decoded_len(n, &self.analog) {
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
        let row = &self.rows[i];
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
        if 4 * channels + lengths.iter().sum::<usize>() != raw.len() {
            return Err(corrupt(format!("block {i}'s sections do not fill it")));
        }
        let mut pos = 4 * channels;
        let mut out = Vec::with_capacity(channels);
        for (c, (kind, len)) in self.header.channels.iter().zip(lengths).enumerate() {
            let section = &raw[pos..pos + len];
            pos += len;
            let word = row.channels[c];
            let count = (word & !LEVEL_BIT) as usize;
            let bad = |what: &str| corrupt(format!("block {i}, channel {}: {what}", kind.name));
            out.push(match &kind.data {
                ChannelKind::Digital {
                    end: channel_end, ..
                } => {
                    let mut edges = Vec::with_capacity(count);
                    let mut at = 0;
                    let mut tick = start;
                    for k in 0..count {
                        let interval = get_uleb(section, &mut at)
                            .ok_or_else(|| bad("an interval is cut short"))?;
                        if k > 0 && interval == 0 {
                            return Err(bad("two edges share a tick"));
                        }
                        tick = i64::try_from(interval)
                            .ok()
                            .and_then(|iv| tick.checked_add(iv))
                            .ok_or_else(|| bad("an interval overflows"))?;
                        if tick >= end || tick > *channel_end || tick < 1 {
                            return Err(bad("an edge lies outside the block"));
                        }
                        edges.push(tick);
                    }
                    if at != section.len() {
                        return Err(bad("bytes follow the last edge"));
                    }
                    BlockChannel::Digital {
                        initial: word & LEVEL_BIT != 0,
                        edges,
                    }
                }
                ChannelKind::Analog { coding, .. } => {
                    if section.len() != count * coding.sample_len() {
                        return Err(bad("the section does not hold its samples"));
                    }
                    let values = match *coding {
                        AnalogCoding::F32 => section
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|&b| f32::from_le_bytes(b))
                            .collect(),
                        AnalogCoding::Codes { scale, offset, .. } => {
                            let (low, high) = section.split_at(count);
                            low.iter()
                                .zip(high)
                                .map(|(&lo, &hi)| {
                                    let code = u16::from_le_bytes([lo, hi]);
                                    (offset + f64::from(code) * scale) as f32
                                })
                                .collect()
                        }
                    };
                    BlockChannel::Analog {
                        first_index: self.first_sample[c][i],
                        values,
                    }
                }
            });
        }
        Ok(Block {
            start,
            end,
            channels: out,
        })
    }
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
    let head = Head::parse(file)?;
    let h = &head.header;
    let Ratio { num, den } = source.timebase;
    if h.timebase != [num, den] || h.length != source.length {
        return Err(mismatch("the timebase or the length differs"));
    }
    if h.channels.len() != source.channels.len() {
        return Err(mismatch("the channel count differs"));
    }
    let plan = overview::plan(h.length);
    let listed: Vec<u8> = h.overview_blocks.iter().map(|l| l.shift).collect();
    if h.overview != plan.head || listed != plan.blocks {
        return Err(mismatch(
            "the overview levels are not the ones the format sets for this length",
        ));
    }
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
        let block = head.decode_block(i, frame)?;
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
            let block = head.decode_overview_block(l, k, frame)?;
            if block.level != level.slice(block.first, block.level.buckets) {
                return Err(mismatch(format!(
                    "overview level 2^{shift}, block {k}: differs from the content"
                )));
            }
        }
    }
    Ok(())
}
