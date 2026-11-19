//! Annotation parts (FORMAT.md, "Annotations" and "Index"): a run of one generator's lines in one
//! zstd frame, and the recording's table of them. A part's row, and each directory entry above the
//! rows, says which times its annotations reach, so a reader fetches only the parts a view meets.
//! `Footer::parts_meeting` finds them, `Footer::decode_part` reads one, and `Footer::read_sidecar`
//! joins the sidecar's files back from the bundle and every part.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::{ControlFlow, Range};

use serde::{Deserialize, de};

use crate::decode::{DecodeError, Footer, corrupt, inflate_limited};
use crate::index::Page;
use crate::model::Ratio;
use crate::record::{MAX_GENERATORS, MAX_PART, named};
use crate::times::{Time, ticks};

/// A part row, 32 bytes: where the part's frame lies, and the span its annotations cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PartRow {
    /// The offset of its zstd frame in the file, the frame's length and its decoded length.
    pub offset: u64,
    pub length: u32,
    pub decoded_length: u32,
    /// The earliest axis tick any of its annotations starts at.
    pub start: i64,
    /// The latest any of them ends at, a point ending where it starts.
    pub end: i64,
}

impl PartRow {
    pub const SIZE: usize = 32;

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.extend_from_slice(&self.length.to_le_bytes());
        out.extend_from_slice(&self.decoded_length.to_le_bytes());
        out.extend_from_slice(&self.start.to_le_bytes());
        out.extend_from_slice(&self.end.to_le_bytes());
    }

    /// `bytes` is exactly one row.
    pub fn parse(bytes: &[u8]) -> Self {
        Self {
            offset: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            length: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            decoded_length: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
            start: i64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            end: i64::from_le_bytes(bytes[24..32].try_into().unwrap()),
        }
    }

    /// Whether its span meets ticks `span`: it starts before the span's end and ends at or after
    /// its start.
    pub fn meets(&self, span: &Range<i64>) -> bool {
        self.start < span.end && self.end >= span.start
    }
}

/// A part, decoded: a generator's name and a run of its file's lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub source: String,
    /// The lines, each ending with its newline.
    pub lines: String,
}

impl Part {
    /// What its frame decodes to: the name's length as a u16, the name, then the lines.
    pub fn to_raw(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 + self.source.len() + self.lines.len());
        out.extend_from_slice(&(self.source.len() as u16).to_le_bytes());
        out.extend_from_slice(self.source.as_bytes());
        out.extend_from_slice(self.lines.as_bytes());
        out
    }

    /// A part from what its frame decodes to, or why a reader refuses it: a generator name
    /// ANNOTATIONS.md doesn't allow, or a last line without its newline. `span` refuses one with
    /// no line.
    pub fn parse_raw(raw: &[u8]) -> Result<Self, String> {
        let (source, lines) = named(raw)?;
        let lines = std::str::from_utf8(lines).map_err(|_| "its lines are not UTF-8")?;
        if !lines.is_empty() && !lines.ends_with('\n') {
            return Err("its last line does not end with a newline".into());
        }
        Ok(Self {
            source,
            lines: lines.to_owned(),
        })
    }

    /// Its lines, without their newlines.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.split_terminator('\n')
    }

    /// The span its annotations cover, as its row states it: the earliest start and the latest
    /// end, a point ending where it starts. Each line's times are ticks, or decimals with a unit
    /// at `timebase`. Err where a line is not a JSON object with a start, or ends before it
    /// starts, as an annotation record may not.
    pub fn span(&self, timebase: Ratio) -> Result<(i64, i64), String> {
        let mut span: Option<(i64, i64)> = None;
        for (n, line) in self.lines().enumerate() {
            let (start, end) =
                line_span(line, timebase).map_err(|e| format!("line {}: {e}", n + 1))?;
            if end < start {
                return Err(format!(
                    "line {}: ends at {end}, before it starts at {start}",
                    n + 1
                ));
            }
            span = Some(span.map_or((start, end), |(s, e)| (s.min(start), e.max(end))));
        }
        span.ok_or_else(|| "it holds no line".into())
    }
}

/// One generator line's span as a part's row can state it: in `0..2^53`, its start at most its
/// end. Err for a line no part can hold.
pub fn placed_span(line: &str, timebase: Ratio) -> Result<(i64, i64), String> {
    let (start, end) = line_span(line, timebase)?;
    if !crate::index::span_in_range(start, end.max(0) as u64) || end < start {
        return Err(format!(
            "ticks {start} to {end} lie outside 0 to 2^53, or end before they start"
        ));
    }
    Ok((start, end))
}

/// An annotation record's line's start and end, as `line_span` gives a line's, where each is a
/// JSON integer, axis ticks, as tickwave's writer writes a record's (FORMAT.md, "Annotations"). A
/// time with a unit is a generator file's, not a record's.
pub fn record_span(line: &str) -> Result<(i64, i64), String> {
    span_of(line, None)
}

/// One generator line's start and end in ticks, a point's end at its start.
pub fn line_span(line: &str, timebase: Ratio) -> Result<(i64, i64), String> {
    span_of(line, Some(timebase))
}

/// `line_span`, with times in ticks alone where `timebase` is None.
fn span_of(line: &str, timebase: Option<Ratio>) -> Result<(i64, i64), String> {
    let fields: SpanFields = serde_json::from_str(line).map_err(|_| "not a JSON object")?;
    if let Some(key) = fields.twice {
        return Err(format!("names key {key:?} twice"));
    }
    let time = |key: &str, field: Option<Result<Time, String>>| {
        field
            .map(|t| {
                t.and_then(|t| match timebase {
                    Some(timebase) => ticks(&t, timebase),
                    None => match t {
                        Time::Ticks(t) => Ok(t),
                        Time::Text(_) => Err("not a whole tick count: a record's times are axis \
                                              ticks"
                            .to_owned()),
                    },
                })
                .map_err(|e| format!("{key}: {e}"))
            })
            .transpose()
    };
    let start = time("start", fields.start)?.ok_or("no start")?;
    let end = time("end", fields.end)?.unwrap_or(start);
    Ok((start, end))
}

/// A line's `start` and `end`, each the last of its key's, read as a map of `serde_json::Value`s
/// would read the line, without building one: every value parsed whole, and a JSON object whose
/// values don't all parse refused.
#[derive(Default)]
struct SpanFields {
    start: Option<Result<Time, String>>,
    end: Option<Result<Time, String>>,
    /// The first key an object in the line names twice.
    twice: Option<String>,
}

impl<'de> Deserialize<'de> for SpanFields {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Fields;
        impl<'de> de::Visitor<'de> for Fields {
            type Value = SpanFields;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<SpanFields, A::Error> {
                let mut out = SpanFields::default();
                let mut keys = BTreeSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "start" => out.start = Some(map.next_value::<TimeField>()?.0),
                        "end" => out.end = Some(map.next_value::<TimeField>()?.0),
                        _ => {
                            let AnyValue(inner) = map.next_value()?;
                            out.twice = out.twice.or(inner);
                        }
                    }
                    if keys.contains(&key) {
                        out.twice.get_or_insert(key);
                    } else {
                        keys.insert(key);
                    }
                }
                Ok(out)
            }
        }
        d.deserialize_map(Fields)
    }
}

/// A `start` or `end`: its time, or why `Time` refuses the value, in the words `Time` gives it
/// read from a `serde_json::Value`.
struct TimeField(Result<Time, String>);

impl TimeField {
    fn of<'de, D: de::Deserializer<'de, Error = serde_json::Error>>(d: D) -> Self {
        Self(Time::deserialize(d).map_err(|e| e.to_string()))
    }
}

impl<'de> Deserialize<'de> for TimeField {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::value::{
            BoolDeserializer, F64Deserializer, I64Deserializer, MapDeserializer, SeqDeserializer,
            StrDeserializer, U64Deserializer, UnitDeserializer,
        };
        struct Field;
        impl<'de> de::Visitor<'de> for Field {
            type Value = TimeField;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_bool<E>(self, v: bool) -> Result<TimeField, E> {
                Ok(TimeField::of(BoolDeserializer::new(v)))
            }
            fn visit_i64<E>(self, v: i64) -> Result<TimeField, E> {
                Ok(TimeField::of(I64Deserializer::new(v)))
            }
            fn visit_u64<E>(self, v: u64) -> Result<TimeField, E> {
                Ok(TimeField::of(U64Deserializer::new(v)))
            }
            fn visit_f64<E>(self, v: f64) -> Result<TimeField, E> {
                Ok(TimeField::of(F64Deserializer::new(v)))
            }
            fn visit_str<E>(self, v: &str) -> Result<TimeField, E> {
                Ok(TimeField::of(StrDeserializer::new(v)))
            }
            fn visit_unit<E>(self) -> Result<TimeField, E> {
                Ok(TimeField::of(UnitDeserializer::new()))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<TimeField, A::Error> {
                while seq.next_element::<AnyValue>()?.is_some() {}
                let none = std::iter::empty::<u64>();
                Ok(TimeField::of(SeqDeserializer::new(none)))
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<TimeField, A::Error> {
                while map.next_entry::<String, AnyValue>()?.is_some() {}
                let none = std::iter::empty::<(u64, u64)>();
                Ok(TimeField::of(MapDeserializer::new(none)))
            }
        }
        d.deserialize_any(Field)
    }
}

/// Any JSON value, parsed whole and dropped, and the first key an object in it names twice,
/// which a `serde_json::Value` would take the last of. Not `IgnoredAny`: serde_json skips that
/// without checking a number's range, where a `serde_json::Value` refuses one past an f64's.
struct AnyValue(Option<String>);

/// `AnyValue`'s visitor.
struct Any;

impl<'de> de::Visitor<'de> for Any {
    type Value = AnyValue;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E>(self, _: bool) -> Result<AnyValue, E> {
        Ok(AnyValue(None))
    }
    fn visit_i64<E>(self, _: i64) -> Result<AnyValue, E> {
        Ok(AnyValue(None))
    }
    fn visit_u64<E>(self, _: u64) -> Result<AnyValue, E> {
        Ok(AnyValue(None))
    }
    fn visit_f64<E>(self, _: f64) -> Result<AnyValue, E> {
        Ok(AnyValue(None))
    }
    fn visit_str<E>(self, _: &str) -> Result<AnyValue, E> {
        Ok(AnyValue(None))
    }
    fn visit_unit<E>(self) -> Result<AnyValue, E> {
        Ok(AnyValue(None))
    }
    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<AnyValue, A::Error> {
        let mut twice = None;
        while let Some(AnyValue(inner)) = seq.next_element()? {
            twice = twice.or(inner);
        }
        Ok(AnyValue(twice))
    }
    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<AnyValue, A::Error> {
        let mut keys = BTreeSet::new();
        let mut twice = None;
        while let Some((key, AnyValue(inner))) = map.next_entry::<String, AnyValue>()? {
            if keys.contains(&key) {
                twice.get_or_insert(key);
            } else {
                keys.insert(key);
            }
            twice = twice.or(inner);
        }
        Ok(AnyValue(twice))
    }
}

impl<'de> Deserialize<'de> for AnyValue {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(Any)
    }
}

/// A JSON object, and the first key an object in it names twice.
struct Object(AnyValue);

impl<'de> Deserialize<'de> for Object {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_map(Any).map(Object)
    }
}

/// An annotation line, which is one JSON object: Err where it is none, and the first key an
/// object in it names twice, which a reader refuses as it refuses one in the header.
pub(crate) fn line_object(line: &str) -> Result<Option<String>, serde_json::Error> {
    serde_json::from_str::<Object>(line).map(|Object(AnyValue(twice))| twice)
}

/// The first key an object in `json` names twice, where `json` is a JSON object: a header, a
/// declaration or a bundle's object of files. Err where it doesn't parse whole, a number past an
/// f64's range included, which the typed parse of an unknown key would skip.
pub(crate) fn key_twice(json: &[u8]) -> Result<Option<String>, serde_json::Error> {
    serde_json::from_slice::<Object>(json).map(|Object(AnyValue(twice))| twice)
}

/// The sidecar's files whole, as `Footer::read_sidecar` joins them, and what it left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sidecar {
    /// Each file's name and text: the hand-written file, and each generator's file with every
    /// line its parts hold.
    pub files: BTreeMap<String, String>,
    /// Each annotation left out, and why: one that starts at or past the length, or a second of
    /// an id its generator has given already.
    pub left_out: Vec<String>,
}

/// Each generator's file in a bundle's `files`, the file's name by the generator's: those whose
/// first line names a generator. A bundle that holds two files of one generator, or files of more
/// generators than a recording has, is refused.
pub fn generator_files(
    files: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, DecodeError> {
    let mut by_source: BTreeMap<String, String> = BTreeMap::new();
    for (name, text) in files {
        if let Some(source) = head_source(text)
            && by_source.insert(source.clone(), name.clone()).is_some()
        {
            return Err(corrupt(format!(
                "the annotation bundle holds two files of generator {source:?}"
            )));
        }
    }
    if by_source.len() > MAX_GENERATORS {
        return Err(corrupt(format!(
            "the annotation bundle holds {} generators' files, more than the {MAX_GENERATORS} a \
             recording has",
            by_source.len()
        )));
    }
    Ok(by_source)
}

/// `Footer::decode_part` for a recording of `timebase`.
pub(crate) fn decode_part(
    row: &PartRow,
    frame: &[u8],
    timebase: Ratio,
) -> Result<Part, DecodeError> {
    if frame.len() != row.length as usize {
        return Err(corrupt(format!(
            "an annotation part is {} bytes, its row says {}",
            frame.len(),
            row.length
        )));
    }
    let raw = inflate_limited(frame, row.decoded_length as usize, MAX_PART)?;
    let part = Part::parse_raw(&raw)
        .map_err(|e| corrupt(format!("the annotation part at {}: {e}", row.offset)))?;
    let span = part
        .span(timebase)
        .map_err(|e| corrupt(format!("the annotation part at {}: {e}", row.offset)))?;
    if span != (row.start, row.end) {
        return Err(corrupt(format!(
            "the annotation part at {} covers ticks {} to {}, its row says {} to {}",
            row.offset, span.0, span.1, row.start, row.end
        )));
    }
    Ok(part)
}

/// The generator a bundled file's first line names, where it is a generator's head.
pub(crate) fn head_source(text: &str) -> Option<String> {
    let first = text.lines().next()?;
    let fields: serde_json::Map<String, serde_json::Value> = serde_json::from_str(first).ok()?;
    fields.get("format")?;
    Some(fields.get("source")?.as_str()?.to_owned())
}

/// `line` held to the recording's `length` (FORMAT.md, "Annotations"): None for an annotation
/// that starts at or past it; a span that ends past it ends there. A line that needs no cut keeps
/// its bytes.
pub(crate) fn clipped(line: &str, length: i64, timebase: Ratio) -> Option<String> {
    let Ok((start, end)) = line_span(line, timebase) else {
        // A line whose times don't read is the resolver's to report.
        return Some(line.to_owned());
    };
    if start >= length {
        return None;
    }
    if end <= length {
        return Some(line.to_owned());
    }
    let mut fields: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(line).expect("line_span read it");
    fields.insert("end".into(), length.into());
    Some(serde_json::to_string(&fields).expect("a map serialises"))
}

impl Footer {
    /// The rows of the annotation parts whose span meets ticks `span`, in the order the parts
    /// were written. Only the table's pages whose span meets it are fetched, through `fetch`,
    /// and each is checked against the entry that lists it. None of the parts is fetched.
    pub fn parts_meeting<E: From<DecodeError>>(
        &self,
        span: Range<i64>,
        fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Vec<PartRow>, E> {
        Ok(self
            .parts_meeting_at(span, fetch)?
            .into_iter()
            .map(|(_, r)| r)
            .collect())
    }

    /// `parts_meeting`, each row with its place among the rows, the order the parts were
    /// written in.
    pub fn parts_meeting_at<E: From<DecodeError>>(
        &self,
        span: Range<i64>,
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Vec<(u64, PartRow)>, E> {
        let Some(table) = &self.parts else {
            return Ok(Vec::new());
        };
        let mut rows = Vec::new();
        table.visit_in(span.clone(), &mut fetch, |at, page| {
            if let Page::Parts(page) = page {
                let first = at.j * crate::index::PAGE as u64;
                rows.extend((first..).zip(page).filter(|(_, r)| r.meets(&span)));
            }
            Ok(ControlFlow::Continue(()))
        })?;
        Ok(rows)
    }

    /// The span the annotation parts' rows cover, from the top index alone: the earliest start
    /// and the latest end. Rows may pass the length, which a reader cuts them at. None without
    /// parts.
    pub fn parts_span(&self) -> Option<(i64, i64)> {
        let top = self.parts.as_ref()?.levels.last()?;
        let start = top.iter().map(|e| e.first).min()?;
        let end = top.iter().map(|e| e.number).max()?;
        Some((start, i64::try_from(end).ok()?))
    }

    /// Every annotation part's row, in the order the parts were written.
    pub fn part_rows<E: From<DecodeError>>(
        &self,
        fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Vec<PartRow>, E> {
        self.parts_meeting(i64::MIN..i64::MAX, fetch)
    }

    /// The part `row` places, from exactly its frame's bytes. It must decode to the length the
    /// row states, within the format's limit, and its lines must cover the span the row states.
    pub fn decode_part(&self, row: &PartRow, frame: &[u8]) -> Result<Part, DecodeError> {
        decode_part(row, frame, self.header.timebase)
    }

    /// The sidecar's files whole, joined from the annotation bundle and every part through
    /// `fetch`; None without a bundle. Each generator file is its bundled first line, then the
    /// lines of each of its generator's parts in the order of their rows. An annotation that
    /// starts at or past the length is left out and a span that ends past it ends there, and a
    /// second annotation of an id its generator has given already is left out, so the files pass
    /// ANNOTATIONS.md's checks; `left_out` says which. A part whose generator has no file in the
    /// bundle is refused.
    pub fn read_sidecar<E: From<DecodeError>>(
        &self,
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
    ) -> Result<Option<Sidecar>, E> {
        let Some(mut files) = self.read_bundle(&mut fetch)? else {
            return Ok(None);
        };
        let by_source = generator_files(&files)?;
        let length = self.header.length;
        let timebase = self.header.timebase;
        let mut left_out = Vec::new();
        let mut ids: BTreeSet<(String, String)> = BTreeSet::new();
        for row in self.part_rows(&mut fetch)? {
            let frame = fetch(row.offset, row.length)?;
            let part = self.decode_part(&row, &frame)?;
            let Some(name) = by_source.get(&part.source) else {
                return Err(corrupt(format!(
                    "an annotation part of generator {:?}, which has no file in the bundle",
                    part.source
                ))
                .into());
            };
            let text = files.get_mut(name).expect("a bundled file");
            for line in part.lines() {
                let id = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(line)
                    .ok()
                    .and_then(|f| f.get("id")?.as_str().map(str::to_owned));
                let Some(line) = clipped(line, length, timebase) else {
                    left_out.push(format!(
                        "{}/{}: starts at or past the length, {length}",
                        part.source,
                        id.as_deref().unwrap_or("(no id)")
                    ));
                    continue;
                };
                if let Some(id) = id
                    && !ids.insert((part.source.clone(), id.clone()))
                {
                    left_out.push(format!(
                        "{}/{id}: an id its generator has given already",
                        part.source
                    ));
                    continue;
                }
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&line);
                text.push('\n');
            }
        }
        Ok(Some(Sidecar { files, left_out }))
    }

    /// Each generator's lines through `each`, as its generator and the line, from every part in
    /// the order of their rows, one part held at a time. An annotation that starts at or past the
    /// length is left out and a span that ends past it ends there, as in `read_sidecar`; unlike
    /// it, this needs no bundle and keeps the second annotation of an id.
    pub fn each_generated_line<E: From<DecodeError>>(
        &self,
        mut fetch: impl FnMut(u64, u32) -> Result<Vec<u8>, E>,
        mut each: impl FnMut(&str, &str) -> Result<(), E>,
    ) -> Result<(), E> {
        let (length, timebase) = (self.header.length, self.header.timebase);
        for row in self.part_rows(&mut fetch)? {
            let frame = fetch(row.offset, row.length)?;
            let part = self.decode_part(&row, &frame)?;
            for line in part.lines() {
                if let Some(line) = clipped(line, length, timebase) {
                    each(&part.source, &line)?;
                }
            }
        }
        Ok(())
    }
}
