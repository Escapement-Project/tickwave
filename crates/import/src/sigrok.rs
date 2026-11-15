//! sigrok session files (`.sr`, srzip versions 1 and 2), read as libsigrok reads them
//! (`src/session_file.c`, `src/session_driver.c`) and as its srzip output writes them
//! (`src/output/srzip.c`).
//!
//! A zip archive: `version`, an INI `metadata` member with `[global]` and `[device 1]`, the logic
//! stream as `<capturefile>` or chunks `<capturefile>-1`, `-2`, ..., and each analog channel as
//! `analog-1-<index>` or chunks `analog-1-<index>-1`, .... Every stream is read whole on its own;
//! logic and analog chunks need not hold the same samples. A logic sample is `unitsize` bytes,
//! probe N in bit N-1 counting from the first byte's least significant bit. Analog values are f32
//! in the writer's byte order, which the file does not record; this reader takes little-endian.
//!
//! The archive is held in memory, read through once when opened to check every member, and
//! decompressed again by each channel's iterator.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Cursor, Read};
use std::path::Path;
use std::sync::Arc;

use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Digital, Ratio, Recording, Samples, Source, SourceFormat,
};
use flate2::read::DeflateDecoder;
use ini::{Ini, ParseOption};
use zip::{CompressionMethod, ZipArchive};

/// A recording read from a source, and what the reader left out of it.
pub struct Import {
    pub recording: Recording,
    pub skipped: Vec<Skipped>,
}

/// A channel or signal the reader left out, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("not a readable zip archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("member `{member}`: {source}")]
    Member { member: String, source: io::Error },
    #[error("{0}")]
    Invalid(String),
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

const VALIDATED: &str = "member checked when the file was opened";

/// Samples decoded at a time by an analog channel's iterator.
const BLOCK: usize = 1 << 16;

/// Logic stream bytes a digital channel's iterator reads at a time.
const CHUNK: usize = 1 << 12;

/// The most a `version` or `metadata` member decompresses to.
const SMALL: u64 = 1 << 20;

/// Reads a session file.
pub fn read(path: &Path) -> Result<Import, Error> {
    let bytes = std::fs::read(path)?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    read_bytes(bytes, &name)
}

/// Reads a session file held in memory. `file_name` goes into the recording's source.
pub fn read_bytes(bytes: impl Into<Arc<[u8]>>, file_name: &str) -> Result<Import, Error> {
    let data: Arc<[u8]> = bytes.into();
    let mut archive = ZipArchive::new(Cursor::new(Arc::clone(&data)))?;
    // Refuses repeated names, overlapping members and local headers missing from the directory.
    archive.validate_header_consistency()?;

    let mut members = HashMap::new();
    for index in 0..archive.len() {
        let name = archive
            .name_for_index(index)
            .ok_or_else(|| invalid(format!("zip entry {index} has no name")))??
            .into_owned();
        members.insert(name, index);
    }
    let mut used = HashSet::new();

    let version = small_member(&mut archive, &members, "version")?;
    used.insert("version".to_owned());
    let version = String::from_utf8_lossy(&version);
    if !matches!(version.trim(), "1" | "2") {
        return Err(invalid(format!(
            "session file version {:?}: libsigrok reads versions 1 and 2",
            version.trim()
        )));
    }

    let metadata = small_member(&mut archive, &members, "metadata")?;
    used.insert("metadata".to_owned());
    let metadata = String::from_utf8(metadata)
        .map_err(|_| invalid("metadata is not UTF-8, which libsigrok's GKeyFile requires"))?;
    let meta = Metadata::parse(&metadata)?;

    let rate = meta
        .samplerate
        .ok_or_else(|| invalid("metadata has no samplerate"))?;
    let timebase = Ratio::integer(rate).ok_or_else(|| invalid("a samplerate of 0 Hz"))?;
    let logic_stream = match &meta.logic {
        Some(logic) => {
            let indices = stream_members(&members, &logic.capturefile)?;
            let mut scan = LogicScan::new(logic);
            let stream = check_stream(&mut archive, &indices, logic.unitsize, |bytes| {
                scan.feed(bytes)
            })?;
            used.extend(stream.names.iter().cloned());
            Some((stream, scan))
        }
        None => None,
    };
    let mut analog = Vec::new();
    for channel in &meta.analog {
        let base = format!("analog-1-{}", channel.index);
        let indices = stream_members(&members, &base)?;
        let stream = check_stream(&mut archive, &indices, 4, |_| {})?;
        used.extend(stream.names.iter().cloned());
        analog.push(stream);
    }

    let mut extra = meta.extra;
    let mut unread: Vec<_> = members
        .keys()
        .filter(|name| !used.contains(*name))
        .cloned()
        .collect();
    unread.sort();
    for name in unread {
        let size = archive.by_index_raw(members[&name])?.size();
        extra.insert(format!("member {name}"), format!("{size} bytes, not read"));
    }

    let mut channels = Vec::new();
    let mut skipped = Vec::new();
    let mut length = 0i64;
    if let (Some(logic), Some((stream, scan))) = (&meta.logic, logic_stream) {
        let unit = logic.unitsize;
        let samples = stream.bytes / unit as u64;
        let end = ticks(samples)?;
        length = length.max(end);
        let stream = Arc::new(stream);
        for part in scan.parts {
            match part {
                Part::Named {
                    bit,
                    first,
                    changes,
                    ..
                } => {
                    let name = &logic.names[&(bit as u32 + 1)];
                    channels.push(Channel {
                        name: name.clone(),
                        source_name: name.clone(),
                        source: 0,
                        data: ChannelData::Digital(Digital {
                            initial: first,
                            end,
                            gaps: Vec::new(),
                            count: changes,
                            edges: Box::new(BitEdges {
                                reader: StreamReader::new(&data, &stream),
                                unit: unit as u64,
                                mask: 1 << (bit % 8),
                                level: first,
                                tick: 0,
                                remaining: samples,
                                left: stream.bytes,
                                buf: vec![0; (CHUNK as u64).min(stream.bytes) as usize],
                                at: 0,
                                len: 0,
                                next: (bit / 8) as u64,
                            }),
                        }),
                        shaping: Default::default(),
                    });
                }
                Part::Unnamed {
                    from,
                    to,
                    set,
                    clear,
                } => {
                    if let Some(s) = unnamed(from, to, set, clear, logic.total as usize) {
                        skipped.push(s);
                    }
                }
            }
        }
    }
    for (channel, stream) in meta.analog.iter().zip(analog) {
        let count = stream.bytes / 4;
        let Some(name) = &channel.name else {
            skipped.push(Skipped {
                name: format!("analog{}", channel.index),
                reason: format!(
                    "no analog{} key, so libsigrok treats it as disabled",
                    channel.index
                ),
            });
            continue;
        };
        length = length.max(ticks(count)?);
        channels.push(Channel {
            name: name.clone(),
            source_name: name.clone(),
            source: 0,
            data: ChannelData::Analog(Analog {
                // srzip records no unit, and libsigrok labels every analog channel volts.
                unit: String::new(),
                first: 0,
                period: Ratio::ONE,
                count,
                breaks: Vec::new(),
                significant: None,
                samples: Samples::F32(Box::new(Values {
                    reader: StreamReader::new(&data, &Arc::new(stream)),
                    remaining: count,
                    buf: vec![0; BLOCK * 4],
                    pos: 0,
                    len: 0,
                })),
                lost_before: 0,
                lost_after: 0,
            }),
            shaping: Default::default(),
        });
    }

    Ok(Import {
        recording: Recording {
            timebase,
            // n samples cover ticks 0..n. A capture with none still covers tick 0, where nothing
            // is known.
            length: length.max(1),
            channels,
            derived: Vec::new(),
            sources: vec![Source {
                files: vec![file_name.to_owned()],
                extra,
                ..Source::new(SourceFormat::SigrokSr, timebase)
            }],
        },
        skipped,
    })
}

/// What a span of bits no probe names reports: a disabled probe always, a bit past the probes
/// only where it is set.
fn unnamed(from: usize, to: usize, set: bool, clear: bool, total: usize) -> Option<Skipped> {
    let bits = if set && clear {
        "toggles"
    } else if set {
        "is always 1"
    } else {
        "is always 0"
    };
    match (from < total, to - from) {
        (true, 1) => Some(Skipped {
            name: format!("probe{}", from + 1),
            reason: format!(
                "no probe{} key, so libsigrok treats it as disabled; its bit {bits}",
                from + 1
            ),
        }),
        (true, _) => {
            let some = if set && clear {
                "some are set and some clear".to_owned()
            } else {
                format!("they are always {}", u8::from(set))
            };
            Some(Skipped {
                name: format!("probe{} to probe{to}", from + 1),
                reason: format!(
                    "no probe{} to probe{to} keys, so libsigrok treats them as disabled; {some}",
                    from + 1
                ),
            })
        }
        (false, 1) => set.then(|| Skipped {
            name: format!("bit {from}"),
            reason: format!(
                "beyond the {total} probes the metadata declares, but it {}",
                if clear { "toggles" } else { "is set" }
            ),
        }),
        (false, _) => set.then(|| Skipped {
            name: format!("bits {from} to {}", to - 1),
            reason: format!("beyond the {total} probes the metadata declares, but some are set"),
        }),
    }
}

fn ticks(samples: u64) -> Result<i64, Error> {
    i64::try_from(samples).map_err(|_| invalid(format!("{samples} samples is too many")))
}

fn small_member(
    archive: &mut ZipArchive<Cursor<Arc<[u8]>>>,
    members: &HashMap<String, usize>,
    name: &str,
) -> Result<Vec<u8>, Error> {
    let index = *members
        .get(name)
        .ok_or_else(|| invalid(format!("no `{name}` member: not a sigrok session file")))?;
    let file = archive.by_index(index)?;
    let mut bytes = Vec::new();
    // Deflate can make a few bytes many: what it holds is read only as far as the bound.
    file.take(SMALL + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| Error::Member {
            member: name.to_owned(),
            source,
        })?;
    if bytes.len() as u64 > SMALL {
        return Err(invalid(format!(
            "member `{name}` is more than {SMALL} bytes"
        )));
    }
    Ok(bytes)
}

/// The members holding one stream, in order: `base` alone, or `base-1`, `base-2`, ... up to the
/// first missing number. libsigrok stops at that gap and never reads past it, so a chunk beyond
/// one is refused rather than dropped.
fn stream_members(members: &HashMap<String, usize>, base: &str) -> Result<Vec<usize>, Error> {
    let chunk_number = |name: &str| -> Option<u64> {
        let digits = name.strip_prefix(base)?.strip_prefix('-')?;
        (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .then(|| digits.parse().ok())
            .flatten()
    };
    let chunks = members
        .keys()
        .filter(|name| chunk_number(name).is_some())
        .count();
    if let Some(&index) = members.get(base) {
        if chunks > 0 {
            return Err(invalid(format!(
                "both `{base}` and chunks `{base}-N`: libsigrok reads only `{base}`"
            )));
        }
        return Ok(vec![index]);
    }
    let mut indices = Vec::new();
    while let Some(&index) = members.get(&format!("{base}-{}", indices.len() + 1)) {
        indices.push(index);
    }
    if indices.is_empty() {
        return Err(invalid(format!("no `{base}` or `{base}-1` member")));
    }
    if indices.len() != chunks {
        return Err(invalid(format!(
            "`{base}` chunks stop at {}, but later ones exist, which libsigrok would not read",
            indices.len()
        )));
    }
    Ok(indices)
}

/// Where a member's stored bytes sit in the archive.
#[derive(Clone, Copy)]
struct Member {
    start: usize,
    compressed: usize,
    deflated: bool,
}

struct Stream {
    names: Vec<String>,
    members: Vec<Member>,
    /// Decompressed bytes across all members.
    bytes: u64,
}

/// Reads every member of a stream once: each must be stored or deflated, decompress with a
/// matching CRC, and hold whole units of `unit` bytes.
fn check_stream(
    archive: &mut ZipArchive<Cursor<Arc<[u8]>>>,
    indices: &[usize],
    unit: usize,
    mut visit: impl FnMut(&[u8]),
) -> Result<Stream, Error> {
    let mut stream = Stream {
        names: Vec::new(),
        members: Vec::new(),
        bytes: 0,
    };
    let mut buf = vec![0; 1 << 16];
    for &index in indices {
        let mut file = archive.by_index(index)?;
        let name = file.name()?.into_owned();
        let fail = |message: String| invalid(format!("member `{name}`: {message}"));
        if file.encrypted() {
            return Err(fail("encrypted".to_owned()));
        }
        let deflated = match file.compression() {
            CompressionMethod::Stored => false,
            CompressionMethod::Deflated => true,
            other => return Err(fail(format!("compression {other} is not supported"))),
        };
        if file.size() % unit as u64 != 0 {
            return Err(fail(format!(
                "{} bytes is not a whole number of {unit}-byte samples",
                file.size()
            )));
        }
        let start = file
            .data_start()
            .ok_or_else(|| fail("data offset unknown".to_owned()))?;
        let member = Member {
            start: usize::try_from(start).map_err(|_| fail("offset too large".to_owned()))?,
            compressed: usize::try_from(file.compressed_size())
                .map_err(|_| fail("too large".to_owned()))?,
            deflated,
        };
        let expected = file.size();
        let mut read = 0u64;
        loop {
            let n = file.read(&mut buf).map_err(|source| Error::Member {
                member: name.clone(),
                source,
            })?;
            if n == 0 {
                break;
            }
            visit(&buf[..n]);
            read += n as u64;
        }
        if read != expected {
            return Err(fail(format!(
                "decompressed to {read} bytes, but the archive says {expected}"
            )));
        }
        stream.bytes += read;
        stream.names.push(name);
        stream.members.push(member);
    }
    Ok(stream)
}

/// A sample's bits, in order, as the logic scan keeps them: a probe the metadata names, or a span
/// it names none in. Every named probe costs a metadata line, and the run of bits between two is
/// a part a bit when it is `SPLIT` bits or fewer and one part when longer, so nothing here is
/// sized by `unitsize`.
enum Part {
    Named {
        bit: usize,
        /// Its level in the first sample, and in the last read.
        first: bool,
        level: bool,
        changes: u64,
    },
    /// Bits `from..to`: whether any is ever set, and ever clear.
    Unnamed {
        from: usize,
        to: usize,
        set: bool,
        clear: bool,
    },
}

impl Part {
    fn end(&self) -> usize {
        match *self {
            Part::Named { bit, .. } => bit + 1,
            Part::Unnamed { to, .. } => to,
        }
    }
}

/// A run of unnamed bits up to this long is reported a bit at a time, as libsigrok's disabled
/// probes are; a longer one as one span.
const SPLIT: usize = 8;

/// The logic stream read once through: each named probe's first level and changes, and which
/// unnamed bits are ever set or clear.
struct LogicScan {
    unit: usize,
    /// The byte of its sample the next byte of the stream is.
    pos: usize,
    /// Whether a whole sample has been read.
    past_first: bool,
    parts: Vec<Part>,
    /// The part the bit at `pos * 8` lies in.
    cursor: usize,
}

impl LogicScan {
    fn new(logic: &Logic) -> Self {
        let mut parts = Vec::new();
        let span = |parts: &mut Vec<Part>, from: usize, to: usize| {
            let unnamed = |from, to| Part::Unnamed {
                from,
                to,
                set: false,
                clear: false,
            };
            if to - from <= SPLIT {
                parts.extend((from..to).map(|b| unnamed(b, b + 1)));
            } else {
                parts.push(unnamed(from, to));
            }
        };
        let total = logic.total as usize;
        let mut at = 0;
        for &probe in logic.names.keys() {
            let bit = probe as usize - 1;
            span(&mut parts, at, bit);
            parts.push(Part::Named {
                bit,
                first: false,
                level: false,
                changes: 0,
            });
            at = bit + 1;
        }
        span(&mut parts, at, total);
        span(&mut parts, total, logic.unitsize * 8);
        Self {
            unit: logic.unitsize,
            pos: 0,
            past_first: false,
            parts,
            cursor: 0,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            let lo = self.pos * 8;
            while self.parts[self.cursor].end() <= lo {
                self.cursor += 1;
            }
            let mut i = self.cursor;
            loop {
                match &mut self.parts[i] {
                    Part::Named {
                        bit,
                        first,
                        level,
                        changes,
                    } => {
                        let now = b >> (*bit - lo) & 1 == 1;
                        if !self.past_first {
                            *first = now;
                        } else if now != *level {
                            *changes += 1;
                        }
                        *level = now;
                    }
                    Part::Unnamed {
                        from,
                        to,
                        set,
                        clear,
                    } => {
                        let (a, z) = ((*from).max(lo) - lo, (*to).min(lo + 8) - lo);
                        let mask = (0xffu16 << a & 0xffu16 >> (8 - z)) as u8;
                        *set |= b & mask != 0;
                        *clear |= b & mask != mask;
                    }
                }
                if self.parts[i].end() >= lo + 8 {
                    break;
                }
                i += 1;
            }
            self.cursor = i;
            self.pos += 1;
            if self.pos == self.unit {
                self.pos = 0;
                self.cursor = 0;
                self.past_first = true;
            }
        }
    }
}

/// A member's stored bytes.
struct Raw {
    data: Arc<[u8]>,
    pos: usize,
    end: usize,
}

impl Read for Raw {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.end - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// A stream's members decompressed one after another.
struct StreamReader {
    data: Arc<[u8]>,
    stream: Arc<Stream>,
    next: usize,
    current: Option<Box<dyn Read + Send>>,
}

impl StreamReader {
    fn new(data: &Arc<[u8]>, stream: &Arc<Stream>) -> Self {
        Self {
            data: Arc::clone(data),
            stream: Arc::clone(stream),
            next: 0,
            current: None,
        }
    }
}

impl Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if let Some(current) = &mut self.current {
                let n = current.read(buf)?;
                if n > 0 || buf.is_empty() {
                    return Ok(n);
                }
                self.current = None;
            }
            let Some(member) = self.stream.members.get(self.next) else {
                return Ok(0);
            };
            self.next += 1;
            let raw = Raw {
                data: Arc::clone(&self.data),
                pos: member.start,
                end: member.start + member.compressed,
            };
            self.current = Some(if member.deflated {
                Box::new(DeflateDecoder::new(raw))
            } else {
                Box::new(raw)
            });
        }
    }
}

/// One bit of every logic sample, read from the stream a chunk at a time: the byte holding it is
/// all a sample needs, however wide the sample.
struct BitEdges {
    reader: StreamReader,
    unit: u64,
    mask: u8,
    level: bool,
    /// The tick of the next sample.
    tick: i64,
    /// Samples not yet looked at.
    remaining: u64,
    /// Stream bytes not yet read.
    left: u64,
    buf: Vec<u8>,
    /// The stream offset of `buf[0]`, and the bytes it holds.
    at: u64,
    len: usize,
    /// The stream offset of the next sample's byte.
    next: u64,
}

impl Iterator for BitEdges {
    type Item = i64;

    fn next(&mut self) -> Option<i64> {
        while self.remaining > 0 {
            while self.next >= self.at + self.len as u64 {
                self.at += self.len as u64;
                let n = (self.buf.len() as u64).min(self.left) as usize;
                self.reader.read_exact(&mut self.buf[..n]).expect(VALIDATED);
                self.left -= n as u64;
                self.len = n;
            }
            let level = self.buf[(self.next - self.at) as usize] & self.mask != 0;
            let tick = self.tick;
            self.next += self.unit;
            self.tick += 1;
            self.remaining -= 1;
            if level != self.level {
                self.level = level;
                return Some(tick);
            }
        }
        None
    }
}

struct Values {
    reader: StreamReader,
    remaining: u64,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
}

impl Iterator for Values {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pos == self.len {
            if self.remaining == 0 {
                return None;
            }
            let n = self.remaining.min(BLOCK as u64) as usize;
            self.reader
                .read_exact(&mut self.buf[..n * 4])
                .expect(VALIDATED);
            self.remaining -= n as u64;
            self.pos = 0;
            self.len = n;
        }
        let at = self.pos * 4;
        self.pos += 1;
        Some(f32::from_le_bytes(
            self.buf[at..at + 4].try_into().expect("4 bytes"),
        ))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = (self.len - self.pos) as u64 + self.remaining;
        let left = usize::try_from(left).unwrap_or(usize::MAX);
        (left, Some(left))
    }
}

struct Logic {
    capturefile: String,
    unitsize: usize,
    total: u32,
    names: BTreeMap<u32, String>,
}

struct AnalogChannel {
    /// The number in its `analogN` key and its member names.
    index: u32,
    name: Option<String>,
}

struct Metadata {
    samplerate: Option<u64>,
    logic: Option<Logic>,
    analog: Vec<AnalogChannel>,
    extra: BTreeMap<String, String>,
}

impl Metadata {
    fn parse(text: &str) -> Result<Self, Error> {
        // GKeyFile has no quoting, and its escapes differ from rust-ini's: take values raw and
        // unescape them here.
        let ini = Ini::load_from_str_opt(
            text,
            ParseOption {
                enabled_quote: false,
                enabled_escape: false,
                ..ParseOption::default()
            },
        )
        .map_err(|e| invalid(format!("metadata: {e}")))?;

        let mut extra = BTreeMap::new();
        let mut device = None;
        let mut sections = HashSet::new();
        for (section, properties) in &ini {
            let Some(section) = section else {
                if properties.is_empty() {
                    continue;
                }
                return Err(invalid("metadata has keys before its first [section]"));
            };
            if !sections.insert(section) {
                return Err(invalid(format!("metadata repeats [{section}]")));
            }
            let mut keys = HashSet::new();
            let mut pairs = Vec::new();
            for (key, value) in properties {
                if !keys.insert(key) {
                    return Err(invalid(format!("metadata repeats `{key}` in [{section}]")));
                }
                pairs.push((key.to_owned(), unescape(value)?));
            }
            match section {
                "global" => {
                    for (key, value) in pairs {
                        extra.insert(key, value);
                    }
                }
                "device 1" => device = Some(pairs),
                other => {
                    return Err(invalid(format!(
                        "metadata section [{other}]: only [global] and [device 1] are read"
                    )));
                }
            }
        }
        let device = device.ok_or_else(|| invalid("metadata has no [device 1]"))?;

        let mut samplerate = None;
        let mut capturefile = None;
        let mut total_probes = None;
        let mut total_analog = None;
        let mut unitsize = None;
        let mut probes = BTreeMap::new();
        let mut analogs = BTreeMap::new();
        for (key, value) in device {
            match key.as_str() {
                "samplerate" => {
                    samplerate = Some(
                        parse_samplerate(&value)
                            .map_err(|e| invalid(format!("samplerate {value:?}: {e}")))?,
                    );
                }
                "capturefile" => capturefile = Some(value),
                "total probes" => total_probes = Some(count(&key, &value)?),
                "total analog" => total_analog = Some(count(&key, &value)?),
                "unitsize" => unitsize = Some(count(&key, &value)?),
                // libsigrok refuses the file on any other key starting with these.
                _ if key.starts_with("probe") => {
                    probes.insert(numbered(&key, "probe")?, value);
                }
                _ if key.starts_with("analog") => {
                    analogs.insert(numbered(&key, "analog")?, value);
                }
                _ => {
                    if extra.insert(key.clone(), value).is_some() {
                        return Err(invalid(format!(
                            "metadata has `{key}` in both [global] and [device 1]"
                        )));
                    }
                }
            }
        }

        let total = total_probes.unwrap_or(0);
        let logic = match capturefile {
            Some(capturefile) => {
                let total = total_probes
                    .ok_or_else(|| invalid("metadata has a capturefile but no total probes"))?;
                let unitsize = unitsize
                    .ok_or_else(|| invalid("metadata has a capturefile but no unitsize"))?;
                if unitsize == 0 || u64::from(unitsize) * 8 < u64::from(total) {
                    return Err(invalid(format!(
                        "unitsize {unitsize} does not hold {total} probes"
                    )));
                }
                if let Some((&n, _)) = probes.iter().find(|&(&n, _)| n > total) {
                    return Err(invalid(format!("probe{n}, but total probes is {total}")));
                }
                Some(Logic {
                    capturefile,
                    unitsize: unitsize as usize,
                    total,
                    names: probes,
                })
            }
            None => {
                if let Some(n) = probes.keys().next() {
                    return Err(invalid(format!(
                        "probe{n}, but no capturefile holds logic data"
                    )));
                }
                None
            }
        };

        // Analog channels number on from the logic ones, as libsigrok's reader assumes.
        let analog_count = total_analog.unwrap_or(0);
        let first = total + 1;
        let last = u64::from(total) + u64::from(analog_count);
        if let Some(&n) = analogs.keys().find(|&&n| n < first || u64::from(n) > last) {
            return Err(invalid(format!(
                "analog{n}, but analog channels are numbered {first} to {last}"
            )));
        }
        let analog = (0..analog_count)
            .map(|i| {
                let index = first + i;
                AnalogChannel {
                    index,
                    name: analogs.remove(&index),
                }
            })
            .collect();

        Ok(Self {
            samplerate,
            logic,
            analog,
            extra,
        })
    }
}

fn count(key: &str, value: &str) -> Result<u32, Error> {
    value
        .trim()
        .parse()
        .map_err(|_| invalid(format!("`{key}` is {value:?}, not a count")))
}

fn numbered(key: &str, prefix: &str) -> Result<u32, Error> {
    let digits = &key[prefix.len()..];
    match digits.parse::<u32>() {
        Ok(n) if n > 0 && digits.bytes().all(|b| b.is_ascii_digit()) => Ok(n),
        _ => Err(invalid(format!(
            "metadata key `{key}`: libsigrok expects {prefix}N with N from 1"
        ))),
    }
}

/// GKeyFile's value escapes.
fn unescape(value: &str) -> Result<String, Error> {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next() {
            Some('s') => ' ',
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some('\\') => '\\',
            other => {
                return Err(invalid(format!(
                    "metadata value {value:?}: invalid escape {other:?}"
                )));
            }
        });
    }
    Ok(out)
}

/// A samplerate as libsigrok's `sr_samplerate_string` writes it ("100 kHz", "31.5 kHz") and
/// `sr_parse_sizestring` reads it, in whole hertz.
///
/// Where libsigrok's parser misreads, this refuses rather than agree or disagree silently: it
/// drops a fraction written without a space before the prefix ("1.5kHz" is 1 Hz to libsigrok),
/// truncates a fraction of a hertz, and reads `m` and `p` as mega and peta.
pub fn parse_samplerate(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let int_len = text.bytes().take_while(u8::is_ascii_digit).count();
    if int_len == 0 {
        return Err("no number".to_owned());
    }
    let (int, rest) = text.split_at(int_len);
    let (frac, rest) = match rest.strip_prefix('.') {
        Some(rest) => {
            let len = rest.bytes().take_while(u8::is_ascii_digit).count();
            let (frac, rest) = rest.split_at(len);
            if !rest.starts_with(' ') && !rest.trim_start().is_empty() {
                return Err(
                    "a fraction needs a space before the unit, or libsigrok drops it".to_owned(),
                );
            }
            (frac, rest)
        }
        None => ("", rest),
    };
    let rest = rest.trim_start_matches(' ');
    let (exponent, unit) = match rest.chars().next() {
        Some('k' | 'K') => (3, &rest[1..]),
        Some('M') => (6, &rest[1..]),
        Some('g' | 'G') => (9, &rest[1..]),
        Some('t' | 'T') => (12, &rest[1..]),
        Some('P') => (15, &rest[1..]),
        Some('e' | 'E') => (18, &rest[1..]),
        Some('m' | 'p') => {
            return Err(
                "libsigrok reads `m` as mega and `p` as peta; refusing the ambiguity".into(),
            );
        }
        _ => (0, rest),
    };
    if !(unit.is_empty() || unit.eq_ignore_ascii_case("hz")) {
        return Err(format!("unit {unit:?} is not Hz"));
    }
    let frac = frac.trim_end_matches('0');
    let digits: u128 = format!("{int}{frac}")
        .parse()
        .map_err(|_| "too large".to_owned())?;
    let scale = exponent - frac.len() as i32;
    let hz = if scale >= 0 {
        10u128
            .checked_pow(scale as u32)
            .and_then(|p| digits.checked_mul(p))
    } else {
        // Past 38 fraction digits the power passes u128, and no such rate is a whole number of hertz.
        match 10u128.checked_pow((-scale) as u32) {
            Some(p) if digits.is_multiple_of(p) => Some(digits / p),
            _ => return Err("not a whole number of hertz".to_owned()),
        }
    };
    match hz.map(u64::try_from) {
        Some(Ok(0)) => Err("zero".to_owned()),
        Some(Ok(hz)) => Ok(hz),
        _ => Err("too large".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_samplerate;

    #[test]
    fn samplerates() {
        for (text, hz) in [
            ("1 Hz", 1),
            ("100 Hz", 100),
            ("1 kHz", 1_000),
            ("100 kHz", 100_000),
            ("31.5 kHz", 31_500),
            ("1.000001 GHz", 1_000_001_000),
            ("24 MHz", 24_000_000),
            ("2.50 MHz", 2_500_000),
            ("1 EHz", 1_000_000_000_000_000_000),
            ("18446744073709551615 Hz", u64::MAX),
            ("100k", 100_000),
            ("100 khz", 100_000),
            ("1.5 kHz", 1_500),
            (" 10 MHz ", 10_000_000),
        ] {
            assert_eq!(parse_samplerate(text), Ok(hz), "{text:?}");
        }
    }

    #[test]
    fn samplerates_refused() {
        for text in [
            "",
            "kHz",
            "0 Hz",
            "1.5 Hz",
            "1.0005 kHz",
            "1.5kHz",
            "1 mHz",
            "1 pHz",
            "18446744073709551616 Hz",
            "19 EHz",
            "-1 Hz",
            "1 kHz extra",
            "1 k Hz",
            "1 s",
        ] {
            assert!(parse_samplerate(text).is_err(), "{text:?} was accepted");
        }
    }
}
