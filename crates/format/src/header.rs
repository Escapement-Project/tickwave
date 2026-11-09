//! The head of a file: preamble, JSON header, binary index. The overview follows them inside the
//! head (see `overview`).
//!
//! Layout, little-endian:
//!
//! ```text
//! preamble  32 bytes   magic, u16 major, u16 minor, u32 header length, u32 index length,
//!                      u32 head length, u64 file length
//! header    JSON       Header
//! index     binary     one row a block: i64 start tick, u64 offset, u32 length, u32 decoded
//!                      length, then a u32 a channel (digital: edge count, level at the block's
//!                      start in bit 31; analog: sample count); then one row an overview block:
//!                      u64 offset, u32 length, u32 decoded length
//! overview  zstd       the rest of the head
//! blocks    zstd       one frame a block
//! overview  zstd       one frame an overview block, for levels too fine for the head
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const MAGIC: [u8; 8] = *b"\x89TKW\r\n\x1a\n";
pub const MAJOR: u16 = 1;
pub const MINOR: u16 = 0;
pub const PREAMBLE_LEN: usize = 32;
pub const ROW_FIXED: usize = 24;
pub const LEVEL_BIT: u32 = 1 << 31;

/// Every integer in the header stays below this, so a JavaScript reader parses it exactly.
pub const JSON_SAFE: u64 = 1 << 53;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preamble {
    pub major: u16,
    pub minor: u16,
    pub header_len: u32,
    pub index_len: u32,
    pub head_len: u32,
    pub file_len: u64,
}

impl Preamble {
    pub fn to_bytes(self) -> [u8; PREAMBLE_LEN] {
        let mut out = [0; PREAMBLE_LEN];
        out[..8].copy_from_slice(&MAGIC);
        out[8..10].copy_from_slice(&self.major.to_le_bytes());
        out[10..12].copy_from_slice(&self.minor.to_le_bytes());
        out[12..16].copy_from_slice(&self.header_len.to_le_bytes());
        out[16..20].copy_from_slice(&self.index_len.to_le_bytes());
        out[20..24].copy_from_slice(&self.head_len.to_le_bytes());
        out[24..32].copy_from_slice(&self.file_len.to_le_bytes());
        out
    }

    /// None unless `bytes` starts with the magic and holds a whole preamble.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < PREAMBLE_LEN || bytes[..8] != MAGIC {
            return None;
        }
        let u16_at = |at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        Some(Self {
            major: u16_at(8),
            minor: u16_at(10),
            header_len: u32_at(12),
            index_len: u32_at(16),
            head_len: u32_at(20),
            file_len: u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Header {
    /// Ticks a second, `[num, den]`.
    pub timebase: [u64; 2],
    /// Ticks from tick 0 to the capture's end.
    pub length: i64,
    pub channels: Vec<ChannelHeader>,
    pub source: SourceHeader,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, String>,
    /// `blake3:` and the hex digest of the decoded content (see `hash`).
    pub content_id: String,
    pub blocks: u32,
    /// The overview levels in the head, as bucket widths of 2^shift ticks, finest first.
    pub overview: Vec<u8>,
    /// The finer overview levels kept in overview blocks after the data blocks, finest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overview_blocks: Vec<OverviewLevel>,
    /// The sidecar's files by name, the hand-written TOML file and what it includes; in a
    /// bundled file only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverviewLevel {
    pub shift: u8,
    pub blocks: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelHeader {
    pub name: String,
    pub source_name: String,
    #[serde(flatten)]
    pub data: ChannelKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ChannelKind {
    Digital {
        initial: bool,
        end: i64,
        edges: u64,
    },
    Analog {
        unit: String,
        first: i64,
        /// Ticks from one sample to the next, `[num, den]`.
        period: [u64; 2],
        count: u64,
        coding: AnalogCoding,
    },
}

/// How a block stores an analog channel's samples.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AnalogCoding {
    /// The source's f32 values, little-endian.
    F32,
    /// u16 codes in two byte planes: every code's low byte, then every high byte. A value is
    /// `(offset + code × scale) as f32`, which reproduces the source's value exactly. The
    /// converter only picks this after checking every sample.
    Codes { bits: u8, scale: f64, offset: f64 },
}

impl AnalogCoding {
    pub fn sample_len(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::Codes { .. } => 2,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SourceHeader {
    pub format: String,
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub software: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub begin_s: Option<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
    /// The converter's name, version and arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub converter: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockRow {
    pub start: i64,
    pub offset: u64,
    pub length: u32,
    pub decoded_length: u32,
    /// A word a channel: digital, the edge count with the level at the block's start in
    /// `LEVEL_BIT`; analog, the sample count.
    pub channels: Vec<u32>,
}

impl BlockRow {
    pub fn size(channels: usize) -> usize {
        ROW_FIXED + 4 * channels
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.start.to_le_bytes());
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.length.to_le_bytes());
        out.extend_from_slice(&self.decoded_length.to_le_bytes());
        for word in &self.channels {
            out.extend_from_slice(&word.to_le_bytes());
        }
    }

    /// `bytes` is exactly one row.
    pub fn parse(bytes: &[u8], channels: usize) -> Self {
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        Self {
            start: i64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            offset: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            length: u32_at(16),
            decoded_length: u32_at(20),
            channels: (0..channels).map(|c| u32_at(ROW_FIXED + 4 * c)).collect(),
        }
    }
}

/// An overview block's place in the file. The index holds these after the data blocks' rows, by
/// level in the header's order, then in time order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverviewRow {
    pub offset: u64,
    pub length: u32,
    pub decoded_length: u32,
}

impl OverviewRow {
    pub const SIZE: usize = 16;

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.length.to_le_bytes());
        out.extend_from_slice(&self.decoded_length.to_le_bytes());
    }

    /// `bytes` is exactly one row.
    pub fn parse(bytes: &[u8]) -> Self {
        Self {
            offset: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            length: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            decoded_length: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
        }
    }
}
