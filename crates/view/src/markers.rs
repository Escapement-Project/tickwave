//! A recording's annotations as markers: in order of their start tick, then of their arrival,
//! with a cursor that names one in that order. A finished file's are read a range at a time, from
//! the annotation parts the range meets, until something asks for all of them.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use escapement_tickwave_format::annotations::{self, Annotation, Included};
use escapement_tickwave_format::decode::{DecodeError, Footer};
use escapement_tickwave_format::header::Header;
use escapement_tickwave_format::index::Table;
use escapement_tickwave_format::parts::{self, Part, PartRow};

use crate::bytes::read;
use crate::cache::{Entry, Key, list_size};
use crate::recording::{Error, Markers, Recording, bundled, marked, widen};

/// Where a marker sits in the order markers are listed: its start tick, then its arrival. A
/// finished file's arrival is its place in the sidecar's order: the hand-written file's
/// annotations by their place in it, then each generator file's lines, by the file's place, the
/// row of the part holding the line and the line's place in it. A live recording's is the same,
/// a part not yet written taking a row after the written ones', so its markers keep their order
/// once it is. It stays below 2^53, so a JavaScript number holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MarkerCursor {
    pub tick: i64,
    pub arrival: u64,
}

/// An annotation and its cursor. A host reads the annotation's `kind`, `label` and `values`.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub cursor: MarkerCursor,
    pub annotation: Annotation,
}

/// Markers in order from a cursor, at most a page of them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    pub entries: Vec<Marker>,
    /// The cursor to ask from for the next page; None when this page has the last.
    pub next: Option<MarkerCursor>,
    /// How many markers the interval holds in all.
    pub total: usize,
}

/// A part's lines number fewer than 2^19: it decodes to at most 4 MiB, and a line takes 12 bytes
/// at least, `{"start":0}` and its newline. A recording has at most 256 generators, 9 bits.
const LINE_BITS: u32 = 19;
const ROW_BITS: u32 = 25;

/// The arrival of line `line` of the part at row `row`, of the generator whose file is
/// `rank`th in the sidecar's order, from 1. Past 2^25 rows, which only a crafted file reaches,
/// markers share arrivals and so their order within a tick.
pub(crate) fn arrival(rank: usize, row: u64, line: usize) -> u64 {
    (rank as u64) << (ROW_BITS + LINE_BITS)
        | row.min((1 << ROW_BITS) - 1) << LINE_BITS
        | (line as u64).min((1 << LINE_BITS) - 1)
}

/// What reading a finished file's markers by range needs of its annotation bundle.
#[derive(Default)]
pub(crate) struct FileAnnotations {
    /// The bundle's files: the hand-written file, and each generator file's first line.
    files: BTreeMap<String, String>,
    /// Each generator's file's rank in the sidecar's order, from 1, by the generator's name;
    /// None where the file's first line breaks a rule, which leaves out every line of it.
    ranks: BTreeMap<String, Option<usize>>,
    /// The hand-written file's annotations in order, those but its refs and those naming a
    /// channel this version can't read.
    plain: Vec<Marker>,
}

/// An annotation part's lines as markers read them, each resolved alone.
pub(crate) struct PartMarkers {
    source: String,
    /// Each line, in order.
    lines: Vec<PartLine>,
}

enum PartLine {
    /// A line that starts at or past the length, left out, and why. It claims no id.
    Past(String),
    Read {
        /// Its id, where it reads.
        id: Option<String>,
        /// The annotation, or each rule it breaks.
        annotation: Result<Box<Annotation>, String>,
    },
}

impl PartMarkers {
    fn of(part: Part, header: &Header) -> Self {
        let length = header.length;
        let lines = part
            .lines()
            .map(|line| {
                let id = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(line)
                    .ok()
                    .and_then(|f| f.get("id")?.as_str().map(str::to_owned));
                let name = format!("{}/{}", part.source, id.as_deref().unwrap_or("(no id)"));
                // A decoded part's lines' spans read; `resolve_line` names what else is wrong.
                if let Ok((start, _)) = parts::line_span(line, header.timebase)
                    && start >= length
                {
                    return PartLine::Past(format!(
                        "{name}: starts at or past the length, {length}"
                    ));
                }
                PartLine::Read {
                    annotation: annotations::resolve_line(&part.source, line, header)
                        .map(Box::new)
                        .map_err(|problems| format!("{name}: {}", problems.join("; "))),
                    id,
                }
            })
            .collect();
        Self {
            source: part.source,
            lines,
        }
    }

    /// About the bytes it holds.
    fn size(&self, decoded: u32) -> usize {
        list_size(&self.lines) + 2 * decoded as usize
    }
}

/// The markers of some of a file's parts, each its cursor and its annotation in the part, and
/// what reading them left out.
#[derive(Default)]
struct Parts<'a> {
    markers: Vec<(MarkerCursor, &'a Annotation)>,
    /// Each line left out for the length or as the second of an id, in the order of the lines.
    joined: Vec<String>,
    /// Each line left out for a rule it breaks, in the order of the lines.
    broken: Vec<String>,
}

/// The markers of `m`, sorted, that start in `interval`, after `after` where it is given, at most
/// `limit` of them.
fn page(m: &[Marker], interval: Range<i64>, after: Option<MarkerCursor>, limit: usize) -> Page {
    let (at, next, total) = page_of(m, |x| x.cursor, interval, after, limit);
    Page {
        entries: m[at].to_vec(),
        next,
        total,
    }
}

/// `page` of markers held by reference, each cloned only where the page takes it.
fn page_of_refs(
    m: &[(MarkerCursor, &Annotation)],
    interval: Range<i64>,
    after: Option<MarkerCursor>,
    limit: usize,
) -> Page {
    let (at, next, total) = page_of(m, |x| x.0, interval, after, limit);
    Page {
        entries: m[at]
            .iter()
            .map(|&(cursor, a)| Marker {
                cursor,
                annotation: a.clone(),
            })
            .collect(),
        next,
        total,
    }
}

/// Where `page`'s markers lie in `m`, sorted by `cursor`; the cursor to ask from for the next
/// page, and how many the interval holds.
fn page_of<T>(
    m: &[T],
    cursor: impl Fn(&T) -> MarkerCursor,
    interval: Range<i64>,
    after: Option<MarkerCursor>,
    limit: usize,
) -> (Range<usize>, Option<MarkerCursor>, usize) {
    let lo = m.partition_point(|x| cursor(x).tick < interval.start);
    let hi = m.partition_point(|x| cursor(x).tick < interval.end).max(lo);
    let from = match after {
        Some(c) => m[lo..hi].partition_point(|x| cursor(x) <= c) + lo,
        None => lo,
    };
    let to = hi.min(from.saturating_add(limit));
    let next = (to < hi && to > from).then(|| cursor(&m[to - 1]));
    (from..to, next, hi - lo)
}

/// A writer's annotation, `annotation` one line of generator `source`'s file with its times in
/// ticks, resolved as the sidecar would resolve it: id `<source>/<id>`. A line with no id is named
/// by its place in its part.
#[cfg(feature = "live")]
pub(crate) fn generated(
    source: &str,
    annotation: &serde_json::Map<String, serde_json::Value>,
    line: usize,
) -> Annotation {
    use serde_json::Value;
    let text = |key: &str| {
        annotation
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let id = text("id").unwrap_or_else(|| line.to_string());
    Annotation {
        id: format!("{source}/{id}"),
        source: Some(source.to_owned()),
        target: None,
        start: annotation.get("start").and_then(Value::as_i64).unwrap_or(0),
        end: annotation.get("end").and_then(Value::as_i64),
        channels: annotation
            .get("channels")
            .and_then(Value::as_array)
            .map(|cs| {
                cs.iter()
                    .filter_map(|c| c.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        label: text("label"),
        kind: text("kind"),
        text: text("text"),
        paint: text("paint"),
        measure: annotation
            .get("measure")
            .and_then(|m| serde_json::from_value(m.clone()).ok()),
        values: annotation
            .get("values")
            .and_then(Value::as_object)
            .map(|v| {
                v.iter()
                    .filter_map(|(k, x)| Some((k.clone(), x.as_str()?.to_owned())))
                    .collect::<std::collections::BTreeMap<_, _>>()
            }),
    }
}

impl Recording {
    /// The markers that start in `interval`, in order, after `after` where it is given, at most
    /// `limit` of them; none where they don't read (a query asking for markers says why). A
    /// finished file's come from the annotation parts the interval meets until every marker is
    /// read: a `ref` in its hand-written file shows only then, and the second of an id shows
    /// where the part holding the first isn't read (FORMAT.md, "Annotations").
    pub fn markers(&self, interval: Range<i64>, after: Option<MarkerCursor>, limit: usize) -> Page {
        self.markers_in(interval, after, limit).unwrap_or_default()
    }

    /// The marker after `cursor` in order, wherever it starts. A finished file's markers are
    /// read whole.
    pub fn marker_after(&self, cursor: MarkerCursor) -> Option<Marker> {
        let m = self.read_markers().map_or(&[][..], |m| &m.list);
        let i = m.partition_point(|x| x.cursor <= cursor);
        m.get(i).cloned()
    }

    /// `markers`, or why they don't read.
    pub(crate) fn markers_in(
        &self,
        interval: Range<i64>,
        after: Option<MarkerCursor>,
        limit: usize,
    ) -> Result<Page, Error> {
        if let Some(m) = self.markers.get() {
            let m = m.as_ref().map_err(|e| Error::Annotations(e.clone()))?;
            return Ok(page(&m.list, interval, after, limit));
        }
        let file = self.file_annotations()?;
        let Some(f) = self.finished() else {
            return Ok(Page::default());
        };
        if interval.start >= interval.end {
            return Ok(Page::default());
        }
        let _span = tracing::debug_span!("read markers in range").entered();
        let footer = f.footer();
        let rows = footer
            .parts_meeting_at(interval.clone(), |offset, len| self.parts_page(offset, len))?;
        let lo = file
            .plain
            .partition_point(|m| m.cursor.tick < interval.start);
        let hi = file
            .plain
            .partition_point(|m| m.cursor.tick < interval.end)
            .max(lo);
        let parts = self.read_parts(&footer, &rows)?;
        let read = kept_lines(file, &parts)?;
        let mut list: Vec<(MarkerCursor, &Annotation)> = file.plain[lo..hi]
            .iter()
            .map(|m| (m.cursor, &m.annotation))
            .collect();
        list.extend(read.markers.into_iter().filter(|&(cursor, a)| {
            interval.contains(&cursor.tick) && self.unavailable_in(a).is_none()
        }));
        list.sort_unstable_by_key(|m| m.0);
        Ok(page_of_refs(&list, interval, after, limit))
    }

    /// A finished file's markers whole: every part's lines, and the hand-written file's
    /// annotations with its refs followed. What breaks a rule, starts at or past the length,
    /// repeats an id its generator gave already or names a channel this version can't read is
    /// left out and named (FORMAT.md, "Annotations" and "Unavailable channels").
    pub(crate) fn every_marker(&self) -> Result<Markers, Error> {
        let file = self.file_annotations()?;
        let Some(f) = self.finished() else {
            return Ok(Markers::default());
        };
        let footer = f.footer();
        let rows = footer.parts_meeting_at(i64::MIN..i64::MAX, |offset, len| {
            self.parts_page(offset, len)
        })?;
        let parts = self.read_parts(&footer, &rows)?;
        let read = kept_lines(file, &parts)?;
        let by_id: HashMap<&str, &Annotation> = read
            .markers
            .iter()
            .map(|&(_, a)| (a.id.as_str(), a))
            .collect();
        let (hand, included) = hand_and_included(&file.files)?;
        let apart = annotations::resolve_apart(hand, &included, &footer.header, &|target| {
            by_id.get(target).map(|a| (*a).clone())
        });
        let mut left_out = read.joined;
        left_out.extend(read.broken);
        left_out.extend(apart.problems);
        let mut list = hand_markers(apart.annotations);
        list.extend(read.markers.iter().map(|&(cursor, a)| Marker {
            cursor,
            annotation: a.clone(),
        }));
        self.keep_available(&mut list, &mut left_out);
        list.sort_unstable_by_key(|m| m.cursor);
        let mut markers = Markers::of(list);
        markers.left_out = left_out;
        Ok(markers)
    }

    /// The span a finished file's own markers cover, from its hand-written annotations and its
    /// parts table, no part read, so it holds whether or not they are read: from the earliest
    /// row's start to past the latest row's end, within the length. It covers the lines the
    /// markers leave out too. Other markers' span, a live recording's or a sidecar's.
    pub(crate) fn markers_span(&self) -> Option<Range<i64>> {
        if self.bundled.is_none() {
            return self.markers.get()?.as_ref().ok()?.span.clone();
        }
        let file = self.file_annotations().ok()?;
        let mut span = None;
        for m in &file.plain {
            widen(&mut span, marked(&m.annotation));
        }
        if let Some(f) = self.finished()
            && let Some((start, end)) = f.footer().parts_span()
        {
            widen(&mut span, start..end.saturating_add(1).min(self.length));
        }
        span
    }

    /// A finished file's bundle as reading markers needs it, read the first time: Err on every
    /// call while it doesn't read.
    fn file_annotations(&self) -> Result<&FileAnnotations, Error> {
        self.bundle
            .get_or_init(|| self.read_file_annotations().map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| Error::Annotations(e.clone()))
    }

    fn read_file_annotations(&self) -> Result<FileAnnotations, Error> {
        let Some(f) = self.finished() else {
            return Ok(FileAnnotations::default());
        };
        let _span = tracing::debug_span!("read the annotation bundle").entered();
        let footer = f.footer();
        let fetch = |offset: u64, len: u32| -> Result<Vec<u8>, Error> {
            Ok(read(&*self.bytes, offset, len as usize)?)
        };
        let Some(files) = footer.read_bundle(fetch)? else {
            return Ok(FileAnnotations::default());
        };
        let by_source = parts::generator_files(&files)?;
        let (hand, included) = hand_and_included(&files)?;
        // A ref resolves once every part is read.
        let apart = annotations::resolve_apart(hand, &included, &footer.header, &|_| None);
        let rank: BTreeMap<&str, Option<usize>> = apart
            .files
            .iter()
            .enumerate()
            .map(|(i, (name, kept))| (*name, kept.then_some(i + 1)))
            .collect();
        let ranks = by_source
            .into_iter()
            .map(|(source, name)| {
                let r = rank.get(name.as_str()).copied().flatten();
                (source, r)
            })
            .collect();
        let mut plain = hand_markers(apart.annotations);
        plain.retain(|m| self.unavailable_in(&m.annotation).is_none());
        plain.sort_unstable_by_key(|m| m.cursor);
        drop(footer);
        Ok(FileAnnotations {
            files,
            ranks,
            plain,
        })
    }

    /// The parts at `rows`, each with its row, in the order of the rows.
    fn read_parts(
        &self,
        footer: &Footer,
        rows: &[(u64, PartRow)],
    ) -> Result<Vec<(u64, Arc<PartMarkers>)>, Error> {
        rows.iter()
            .map(|(at, row)| Ok((*at, self.part(footer, row)?)))
            .collect()
    }

    /// The part at `row`, read and resolved the first time, then from the cache.
    fn part(&self, footer: &Footer, row: &PartRow) -> Result<Arc<PartMarkers>, Error> {
        let entry = self.cache.get_or_make(Key::Part(row.offset), || {
            let _span = tracing::debug_span!("read annotation part", offset = row.offset).entered();
            let frame = read(&*self.bytes, row.offset, row.length as usize)?;
            let part = PartMarkers::of(footer.decode_part(row, &frame)?, &footer.header);
            let size = part.size(row.decoded_length);
            Ok::<_, Error>((Entry::Part(Arc::new(part)), size))
        })?;
        match entry {
            Entry::Part(p) => Ok(p),
            _ => Err(DecodeError::Corrupt(
                "an annotation part is listed where another kind of record is".into(),
            )
            .into()),
        }
    }

    /// A page of the annotation parts table, through the cache.
    fn parts_page(&self, offset: u64, len: u32) -> Result<Vec<u8>, Error> {
        let entry = self
            .cache
            .get_or_make(Key::Page(0, Table::Parts, offset), || {
                let bytes = read(&*self.bytes, offset, len as usize)?;
                let size = list_size(&bytes);
                Ok::<_, Error>((Entry::Bytes(Arc::new(bytes)), size))
            })?;
        match entry {
            Entry::Bytes(b) if b.len() == len as usize => Ok(b.to_vec()),
            _ => Err(DecodeError::Corrupt(
                "an annotation parts page is listed where another kind of record is".into(),
            )
            .into()),
        }
    }

    /// Leaves out of `list` each marker that names a channel this version can't read, and says
    /// so in `left_out`.
    pub(crate) fn keep_available(&self, list: &mut Vec<Marker>, left_out: &mut Vec<String>) {
        list.retain(|m| match self.unavailable_in(&m.annotation) {
            Some(c) => {
                left_out.push(format!(
                    "{}: names channel {c:?}, which this version can't read",
                    m.annotation.id
                ));
                false
            }
            None => true,
        });
    }

    /// The first channel `a` names, in `channels` or its measure, that this version can't read.
    fn unavailable_in<'a>(&self, a: &'a Annotation) -> Option<&'a str> {
        if self.channels.iter().all(|c| c.unavailable.is_none()) {
            return None;
        }
        a.channels
            .iter()
            .map(String::as_str)
            .chain(a.measure.as_ref().map(|m| m.channel.as_str()))
            .find(|name| {
                self.channels
                    .iter()
                    .any(|c| c.unavailable.is_some() && c.name == *name)
            })
    }
}

/// The markers of `parts`, given in the order of their rows: each line resolved alone, and the
/// second of an id its generator gave already among them left out.
fn kept_lines<'a>(
    file: &FileAnnotations,
    parts: &'a [(u64, Arc<PartMarkers>)],
) -> Result<Parts<'a>, Error> {
    let mut out = Parts::default();
    let mut ids: HashSet<(&str, &str), foldhash::fast::RandomState> = HashSet::default();
    for (at, part) in parts {
        let Some(&rank) = file.ranks.get(&part.source) else {
            return Err(DecodeError::Corrupt(format!(
                "an annotation part of generator {:?}, which has no file in the bundle",
                part.source
            ))
            .into());
        };
        for (n, line) in part.lines.iter().enumerate() {
            let (id, annotation) = match line {
                PartLine::Past(why) => {
                    out.joined.push(why.clone());
                    continue;
                }
                PartLine::Read { id, annotation } => (id, annotation),
            };
            if let Some(id) = id
                && !ids.insert((&part.source, id))
            {
                out.joined.push(format!(
                    "{}/{id}: an id its generator has given already",
                    part.source
                ));
                continue;
            }
            let Some(rank) = rank else {
                // Its file's first line breaks a rule, which the bundle's problems name.
                continue;
            };
            match annotation {
                Ok(a) => out.markers.push((
                    MarkerCursor {
                        tick: a.start,
                        arrival: arrival(rank, *at, n),
                    },
                    a,
                )),
                Err(why) => out.broken.push(why.clone()),
            }
        }
    }
    Ok(out)
}

/// The hand-written file of a bundle and its generator files.
fn hand_and_included(files: &BTreeMap<String, String>) -> Result<(&str, Vec<Included<'_>>), Error> {
    bundled(files).ok_or_else(|| Error::Annotations("the bundle holds no hand-written file".into()))
}

/// Markers of the hand-written file's annotations, each arriving at its place in the file.
fn hand_markers(annotations: Vec<(usize, Annotation)>) -> Vec<Marker> {
    annotations
        .into_iter()
        .map(|(position, annotation)| Marker {
            cursor: MarkerCursor {
                tick: annotation.start,
                arrival: position as u64,
            },
            annotation,
        })
        .collect()
}
