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
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::model::Ratio;

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
    /// Ticks a second.
    pub timebase: Ratio,
    /// Ticks from tick 0 to the capture's end.
    pub length: i64,
    pub channels: Vec<ChannelHeader>,
    pub source: SourceHeader,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, String>,
    pub content_id: ContentId,
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
#[serde(try_from = "RawChannel")]
pub struct ChannelHeader {
    pub name: String,
    pub source_name: String,
    #[serde(flatten)]
    pub data: ChannelKind,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
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
        /// Ticks from one sample to the next.
        period: Ratio,
        count: u64,
        coding: AnalogCoding,
    },
}

/// How a block stores an analog channel's samples.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AnalogCoding {
    /// The source's f32 values, little-endian.
    F32,
    /// u16 codes in two byte planes: every code's low byte, then every high byte. A value is
    /// `(offset + code × scale) as f32`, which reproduces the source's value exactly. The
    /// converter only picks this after checking every sample.
    Codes { bits: u8, scale: f64, offset: f64 },
}

/// The value a code stands for: an f64 multiply, an f64 add, then rounding to the nearest f32. A
/// reader must compute exactly this: a fused multiply-add rounds once instead of twice and can give
/// another f32. Every reader and writer here goes through this one.
#[inline]
pub fn code_value(code: u16, scale: f64, offset: f64) -> f32 {
    (offset + f64::from(code) * scale) as f32
}

impl AnalogCoding {
    pub fn sample_len(self) -> usize {
        match self {
            Self::F32 => 4,
            Self::Codes { .. } => 2,
        }
    }
}

/// A channel as the JSON holds it, every kind's fields inline and optional. Deriving
/// `Deserialize` for the flattened, internally tagged form instead makes serde buffer each channel
/// before parsing it, which costs the browser's decoder a large share of its size.
#[derive(Deserialize)]
struct RawChannel {
    name: String,
    source_name: String,
    kind: RawKind,
    initial: Option<bool>,
    end: Option<i64>,
    edges: Option<u64>,
    unit: Option<String>,
    first: Option<i64>,
    period: Option<Ratio>,
    count: Option<u64>,
    coding: Option<RawCoding>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawKind {
    Digital,
    Analog,
}

#[derive(Deserialize)]
struct RawCoding {
    #[serde(rename = "type")]
    kind: RawCodingKind,
    bits: Option<u8>,
    scale: Option<f64>,
    offset: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawCodingKind {
    F32,
    Codes,
}

fn need<T>(value: Option<T>, field: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("missing field `{field}`"))
}

impl TryFrom<RawChannel> for ChannelHeader {
    type Error = String;

    fn try_from(raw: RawChannel) -> Result<Self, String> {
        let data = match raw.kind {
            RawKind::Digital => ChannelKind::Digital {
                initial: need(raw.initial, "initial")?,
                end: need(raw.end, "end")?,
                edges: need(raw.edges, "edges")?,
            },
            RawKind::Analog => {
                let coding = need(raw.coding, "coding")?;
                ChannelKind::Analog {
                    unit: need(raw.unit, "unit")?,
                    first: need(raw.first, "first")?,
                    period: need(raw.period, "period")?,
                    count: need(raw.count, "count")?,
                    coding: match coding.kind {
                        RawCodingKind::F32 => AnalogCoding::F32,
                        RawCodingKind::Codes => AnalogCoding::Codes {
                            bits: need(coding.bits, "bits")?,
                            scale: need(coding.scale, "scale")?,
                            offset: need(coding.offset, "offset")?,
                        },
                    },
                }
            }
        };
        Ok(Self {
            name: raw.name,
            source_name: raw.source_name,
            data,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceHeader {
    pub format: SourceFormat,
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub software: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub begin_s: Option<f64>,
    /// The trigger's time in the source's own seconds, as `begin_s` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<RateOrigin>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
    /// The converter's name, version and arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub converter: Option<String>,
}

impl SourceHeader {
    /// A source of `format` that states nothing else.
    pub fn new(format: SourceFormat) -> Self {
        Self {
            format,
            files: Vec::new(),
            instrument: None,
            software: None,
            begin_s: None,
            trigger_s: None,
            rate: None,
            extra: BTreeMap::new(),
            converter: None,
        }
    }
}

/// The kind of capture a recording was converted from. It reads and writes as its name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum SourceFormat {
    /// `saleae-binary-0`: Saleae Logic 2's binary export, version 0.
    SaleaeBinary0,
    /// `saleae-binary-1`: the same, version 1.
    SaleaeBinary1,
    /// `sigrok-sr`: a sigrok session file.
    SigrokSr,
    /// `vcd`: a value change dump.
    Vcd,
    /// A name this version doesn't know, as a newer converter may write.
    Other(String),
}

impl SourceFormat {
    pub fn as_str(&self) -> &str {
        match self {
            SourceFormat::SaleaeBinary0 => "saleae-binary-0",
            SourceFormat::SaleaeBinary1 => "saleae-binary-1",
            SourceFormat::SigrokSr => "sigrok-sr",
            SourceFormat::Vcd => "vcd",
            SourceFormat::Other(name) => name,
        }
    }
}

impl From<String> for SourceFormat {
    fn from(name: String) -> Self {
        match name.as_str() {
            "saleae-binary-0" => SourceFormat::SaleaeBinary0,
            "saleae-binary-1" => SourceFormat::SaleaeBinary1,
            "sigrok-sr" => SourceFormat::SigrokSr,
            "vcd" => SourceFormat::Vcd,
            _ => SourceFormat::Other(name),
        }
    }
}

impl From<SourceFormat> for String {
    fn from(format: SourceFormat) -> Self {
        match format {
            SourceFormat::Other(name) => name,
            known => known.as_str().to_owned(),
        }
    }
}

impl fmt::Display for SourceFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A recording's content id: `blake3:` and the hex BLAKE3 digest of its decoded content (see
/// `hash`). It reads and writes in that form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentId(blake3::Hash);

impl ContentId {
    pub fn new(digest: blake3::Hash) -> Self {
        Self(digest)
    }
}

impl FromStr for ContentId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        s.strip_prefix("blake3:")
            .and_then(|hex| blake3::Hash::from_hex(hex).ok())
            .map(Self)
            .ok_or_else(|| format!("{s:?} is not a content id: blake3: and 64 hex digits"))
    }
}

impl TryFrom<String> for ContentId {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        s.parse()
    }
}

impl From<ContentId> for String {
    fn from(id: ContentId) -> Self {
        id.to_string()
    }
}

impl fmt::Display for ContentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "blake3:{}", self.0.to_hex())
    }
}

/// How the converter came by the sample rate, where the source doesn't record it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RateOrigin {
    /// The lowest rate on whose grid all the source's edges sit.
    Inferred,
    /// Given to the converter.
    Given,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<ChannelHeader, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn a_ratio_reads_and_writes_as_two_positive_integers() {
        let r = Ratio::new(3, 2).unwrap();
        assert_eq!(serde_json::to_string(&r).unwrap(), "[3,2]");
        assert_eq!(serde_json::from_str::<Ratio>("[3,2]").unwrap(), r);
        for bad in ["[0,1]", "[1,0]", "[1]", "[1,2,3]"] {
            assert!(serde_json::from_str::<Ratio>(bad).is_err(), "{bad}");
        }
        assert_eq!(Ratio::new(1, 0), None);
    }

    #[test]
    fn a_source_format_reads_and_writes_as_its_name_and_keeps_one_it_does_not_know() {
        for name in [
            "saleae-binary-0",
            "saleae-binary-1",
            "sigrok-sr",
            "vcd",
            "logic-99",
        ] {
            let json = format!("\"{name}\"");
            let format: SourceFormat = serde_json::from_str(&json).unwrap();
            assert_eq!(serde_json::to_string(&format).unwrap(), json);
        }
        let read = |json| serde_json::from_str::<SourceFormat>(json).unwrap();
        assert_eq!(read("\"vcd\""), SourceFormat::Vcd);
        assert_eq!(read("\"logic-99\""), SourceFormat::Other("logic-99".into()));
    }

    #[test]
    fn a_content_id_is_blake3_and_64_hex_digits() {
        let id = format!("blake3:{}", "0f".repeat(32));
        let parsed: ContentId = id.parse().unwrap();
        assert_eq!(parsed.to_string(), id);
        assert_eq!(serde_json::to_string(&parsed).unwrap(), format!("\"{id}\""));
        let hex = "0f".repeat(32);
        for bad in [
            "blake3:00ff".to_owned(),
            hex.clone(),
            format!("sha256:{hex}"),
            format!("blake3:{}", "zz".repeat(32)),
        ] {
            assert!(bad.parse::<ContentId>().is_err(), "{bad}");
        }
    }

    #[test]
    fn channels_parse_as_the_encoder_writes_them() {
        let analog = |coding| ChannelKind::Analog {
            unit: "V".into(),
            first: 2,
            period: Ratio::new(3, 2).unwrap(),
            count: 9,
            coding,
        };
        for data in [
            ChannelKind::Digital {
                initial: true,
                end: 7,
                edges: 3,
            },
            analog(AnalogCoding::F32),
            analog(AnalogCoding::Codes {
                bits: 12,
                scale: 0.001,
                offset: -1.5,
            }),
        ] {
            let channel = ChannelHeader {
                name: "a".into(),
                source_name: "b".into(),
                data,
            };
            let json = serde_json::to_string(&channel).unwrap();
            assert_eq!(parse(&json).unwrap(), channel, "{json}");
        }
    }

    #[test]
    fn a_channel_missing_a_field_of_its_kind_is_refused() {
        let named = r#""name":"a","source_name":"b""#;
        let analog = r#""kind":"analog","unit":"V","first":0,"period":[1,1],"count":1"#;
        for (json, problem) in [
            (
                format!(r#"{{{named},"kind":"digital","initial":true,"end":7}}"#),
                "missing field `edges`",
            ),
            (format!("{{{named},{analog}}}"), "missing field `coding`"),
            (
                format!(
                    r#"{{{named},{analog},"coding":{{"type":"codes","bits":12,"scale":1.0}}}}"#
                ),
                "missing field `offset`",
            ),
            (
                format!(r#"{{{named},"kind":"logic"}}"#),
                "unknown variant `logic`",
            ),
        ] {
            let err = parse(&json).unwrap_err().to_string();
            assert!(err.contains(problem), "{json}: {err}");
        }
    }
}
