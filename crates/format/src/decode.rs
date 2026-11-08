//! The decoder. It does no I/O: a caller fetches the head, then the bytes of the blocks it needs,
//! by HTTP Range in the browser or from a file natively, and hands them in.

use std::ops::Range;

use ruzstd::decoding::FrameDecoder;

use crate::codec::get_uleb;
use crate::hash::{self, ChannelHasher};
use crate::header::{
    AnalogCoding, BlockRow, ChannelKind, Header, LEVEL_BIT, MAGIC, MAJOR, PREAMBLE_LEN, Preamble,
};
use crate::model::{ChannelData, Ratio, Recording, Samples};
use crate::overview::{self, Level};

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
    /// For each analog channel, the index of its first sample in each block.
    first_sample: Vec<Vec<u64>>,
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
        let channels = header.channels.len();
        let row_size = BlockRow::size(channels);
        if preamble.index_len as usize != header.blocks as usize * row_size {
            return Err(corrupt("the index does not hold one row a block"));
        }
        let rows: Vec<BlockRow> = bytes[header_end..index_end]
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
            .map(|&shift| {
                let n = overview::buckets(header.length, shift) as usize;
                analog
                    .iter()
                    .map(|&a| if a { 8 * n } else { 2 * n.div_ceil(8) })
                    .sum::<usize>()
            })
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
            first_sample,
        })
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
    enum Want {
        Digital(Box<dyn Iterator<Item = i64> + Send>, bool),
        Analog(Box<dyn Iterator<Item = f32> + Send>),
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
                wants.push(Want::Digital(d.edges, d.initial));
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
                wants.push(Want::Analog(samples));
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
                (BlockChannel::Digital { initial, edges }, Want::Digital(want, level)) => {
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
                        *level = !*level;
                    }
                }
                (BlockChannel::Analog { values, .. }, Want::Analog(want)) => {
                    for value in values {
                        if want.next().map(f32::to_bits) != Some(value.to_bits()) {
                            return Err(mismatch(format!(
                                "channel {name}, block {i}: a sample differs"
                            )));
                        }
                        hashers[c].sample(value);
                    }
                }
                _ => unreachable!("decode_block follows the header's kinds"),
            }
        }
    }
    for (c, want) in wants.iter_mut().enumerate() {
        let left = match want {
            Want::Digital(it, _) => it.next().is_some(),
            Want::Analog(it) => it.next().is_some(),
        };
        if left {
            return Err(mismatch(format!(
                "channel {}: the source holds more than the file",
                h.channels[c].name
            )));
        }
    }
    let digests: Vec<_> = hashers.into_iter().map(ChannelHasher::finish).collect();
    if hash::content_id(h.timebase, h.length, &digests) != h.content_id {
        return Err(mismatch("the content id does not match the content"));
    }
    Ok(())
}
