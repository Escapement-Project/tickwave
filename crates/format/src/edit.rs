//! Edits of a sidecar's hand-written TOML file, one `[[annotation]]` table at a time, for the
//! annotation editor. Every byte outside the table an edit touches is copied from the old text,
//! so comments, blank lines, key order and spacing elsewhere survive.
//!
//! A table's text runs from the comments and blank lines above its header to the next table's,
//! so those lines go with it when it moves or is removed. Tables under it, such as
//! `[annotation.values]`, go with it too. Comments after the last table stay at the file's end.
//!
//! An edit checks no meaning: channels, times and ids are the resolver's to check.

use std::ops::Range;

use serde::Deserialize;
use toml_edit::{Array, ArrayOfTables, Document, DocumentMut, InlineTable, Item, Table, Value};

use crate::annotations::{FORMAT, Time};

/// The fields the editor manages, in the order a new table writes them: ANNOTATIONS.md's, where
/// `ref` follows `id`.
const MANAGED: [&str; 10] = [
    "id", "ref", "start", "end", "channels", "label", "kind", "text", "paint", "measure",
];

/// One edit, as the page sends it in JSON. An index is a position in the file's
/// `[[annotation]]` array.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
pub enum Op {
    /// A new file, from empty text.
    Start {
        recording: String,
        recording_id: String,
    },
    /// A new table, before the one at `index`, or after the last when `index` is their count.
    Insert {
        index: usize,
        fields: Fields,
    },
    /// Sets the managed fields given and removes those not given. Other keys stay.
    Replace {
        index: usize,
        fields: Fields,
    },
    Remove {
        index: usize,
    },
    /// The table at `from` goes to `to`, its index afterwards.
    Move {
        from: usize,
        to: usize,
    },
}

/// An annotation's managed fields. A null is the same as an absent field.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fields {
    pub id: Option<String>,
    pub start: Option<Time>,
    pub end: Option<Time>,
    pub channels: Option<Vec<String>>,
    pub label: Option<String>,
    pub kind: Option<String>,
    pub text: Option<String>,
    pub paint: Option<String>,
    pub measure: Option<MeasureField>,
    #[serde(rename = "ref")]
    pub target: Option<String>,
}

/// `measure`, unchecked: the resolver says whether `what` is a measure.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasureField {
    pub channel: String,
    pub what: String,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);

fn time(t: &Time) -> Value {
    match t {
        Time::Ticks(n) => Value::from(*n),
        Time::Text(s) => Value::from(s.as_str()),
    }
}

impl Fields {
    /// The fields given, in `MANAGED` order. Each value is named here beside its key, so the order
    /// lives in `MANAGED` alone.
    fn values(&self) -> Vec<(&'static str, Value)> {
        let string = |s: &Option<String>| s.as_deref().map(Value::from);
        let mut given = [
            ("id", string(&self.id)),
            ("ref", string(&self.target)),
            ("start", self.start.as_ref().map(time)),
            ("end", self.end.as_ref().map(time)),
            (
                "channels",
                self.channels
                    .as_ref()
                    .map(|c| Value::Array(c.iter().map(String::as_str).collect::<Array>())),
            ),
            ("label", string(&self.label)),
            ("kind", string(&self.kind)),
            ("text", string(&self.text)),
            ("paint", string(&self.paint)),
            (
                "measure",
                self.measure.as_ref().map(|m| {
                    let mut t = InlineTable::new();
                    t.insert("channel", m.channel.as_str().into());
                    t.insert("what", m.what.as_str().into());
                    Value::InlineTable(t)
                }),
            ),
        ];
        MANAGED
            .into_iter()
            .filter_map(|k| {
                let (_, v) = given.iter_mut().find(|(g, _)| *g == k)?;
                Some((k, v.take()?))
            })
            .collect()
    }
}

/// Applies `op`, given as JSON, to the hand-written file's `text`.
pub fn edit_json(text: &str, op: &str) -> Result<String, Error> {
    let op: Op = serde_json::from_str(op).map_err(|e| Error(format!("the edit: {e}")))?;
    edit(text, &op)
}

/// Applies `op` to the hand-written file's `text`.
pub fn edit(text: &str, op: &Op) -> Result<String, Error> {
    if let Op::Start {
        recording,
        recording_id,
    } = op
    {
        if !text.trim().is_empty() {
            return Err(Error(
                "start makes a new file, and this one is not empty".into(),
            ));
        }
        let mut doc = DocumentMut::new();
        doc.insert("format", FORMAT.into());
        doc.insert("recording", recording.as_str().into());
        doc.insert("recording_id", recording_id.as_str().into());
        return Ok(doc.to_string());
    }
    let parsed = Document::parse(text).map_err(|e| Error(e.to_string()))?;
    let layout = Layout::of(&parsed, text)?;
    let n = layout.groups.len();
    let has = match n {
        1 => "the file has 1 annotation".to_owned(),
        n => format!("the file has {n} annotations"),
    };
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let in_range = |index: usize| {
        if index < n {
            Ok(())
        } else {
            Err(Error(format!("index {index} is out of range: {has}")))
        }
    };
    let splice = Splice {
        text,
        layout: &layout,
        eol,
    };
    match op {
        Op::Start { .. } => unreachable!("handled above"),
        Op::Insert { index, fields } => {
            if *index > n {
                return Err(Error(format!(
                    "index {index} is out of range: {has}, so an insert takes 0 to {n}"
                )));
            }
            let at = match index {
                i if *i < n => Anchor::Before(*i),
                0 => Anchor::End,
                _ => Anchor::After(n - 1),
            };
            let before = splice.before(at);
            let new = new_table(fields, !text[..before].trim().is_empty());
            Ok(splice.apply(None, Some((at, newlines(new, eol)))))
        }
        Op::Replace { index, fields } => {
            in_range(*index)?;
            let group = &layout.groups[*index];
            let mut doc = parsed.into_mut();
            let key = doc
                .as_table()
                .key("annotation")
                .expect("the layout found the annotations")
                .clone();
            let table = doc["annotation"]
                .as_array_of_tables_mut()
                .and_then(|a| a.get_mut(*index))
                .expect("the layout found the annotation");
            set(table, fields);
            // The header's line is kept as written; what follows it is rendered again.
            let mut table = table.clone();
            table.decor_mut().clear();
            let mut one = DocumentMut::new();
            let mut array = ArrayOfTables::new();
            array.push(table);
            one.as_table_mut()
                .insert_formatted(&key, Item::ArrayOfTables(array));
            let rendered = one.to_string();
            let body = rendered.split_once('\n').map_or("", |(_, body)| body);
            let start = group.ranges[0].start;
            let line_end = text[group.header.end..]
                .find('\n')
                .map_or(text.len(), |i| group.header.end + i + 1);
            let mut region = text[start..line_end].to_owned();
            if !body.is_empty() && !region.ends_with('\n') {
                region.push_str(eol);
            }
            region += &newlines(body.to_owned(), eol);
            Ok(splice.apply(Some(*index), Some((Anchor::Before(*index), region))))
        }
        Op::Remove { index } => {
            in_range(*index)?;
            Ok(splice.apply(Some(*index), None))
        }
        Op::Move { from, to } => {
            in_range(*from)?;
            in_range(*to)?;
            if from == to {
                return Ok(text.to_owned());
            }
            let mut moved: String = layout.groups[*from]
                .ranges
                .iter()
                .map(|r| &text[r.clone()])
                .collect();
            if !moved.ends_with('\n') {
                moved.push_str(eol);
            }
            // The tables between `from` and `to` shift by one, so `to` names a table that stays.
            let at = if to < from {
                Anchor::Before(*to)
            } else {
                Anchor::After(*to)
            };
            Ok(splice.apply(Some(*from), Some((at, moved))))
        }
    }
}

/// Where the tables' text lies.
struct Layout {
    /// Each table with a header, by where its text starts, with the annotation it belongs to.
    tables: Vec<(usize, Option<usize>)>,
    /// Each annotation's text, in document order: its own table first.
    groups: Vec<Group>,
    /// Where the tables' text ends: the comments and blank lines after it close the file.
    end: usize,
}

struct Group {
    ranges: Vec<Range<usize>>,
    /// The `[[annotation]]` header's own span.
    header: Range<usize>,
}

impl Layout {
    fn of(doc: &Document<&str>, text: &str) -> Result<Self, Error> {
        let root = doc.as_table();
        let mut tables = Vec::new();
        let mut headers = Vec::new();
        for (key, item) in root.iter() {
            if key != "annotation" {
                tables_under(item, None, text, &mut tables);
                continue;
            }
            let array = match item {
                Item::ArrayOfTables(array) => array,
                Item::Value(Value::Array(_)) => {
                    return Err(Error(
                        "`annotation` is an inline array, `annotation = [...]`; the editor edits \
                         `[[annotation]]` tables only"
                            .into(),
                    ));
                }
                _ => {
                    return Err(Error(
                        "`annotation` is not an array of `[[annotation]]` tables, which is what \
                         the editor edits"
                            .into(),
                    ));
                }
            };
            for (i, table) in array.iter().enumerate() {
                let start =
                    header_start(table, text).expect("an array of tables' table has a header");
                tables.push((start, Some(i)));
                headers.push(table.span().expect("a parsed table has a span"));
                for (_, item) in table.iter() {
                    tables_under(item, Some(i), text, &mut tables);
                }
            }
        }
        tables.sort_by_key(|&(start, _)| start);
        let end = doc.trailing().span().map_or(text.len(), |s| s.start);
        let mut groups: Vec<Group> = headers
            .into_iter()
            .map(|header| Group {
                ranges: Vec::new(),
                header,
            })
            .collect();
        for (k, &(start, group)) in tables.iter().enumerate() {
            let stop = tables.get(k + 1).map_or(end, |&(next, _)| next);
            if let Some(g) = group {
                groups[g].ranges.push(start..stop);
            }
        }
        Ok(Self {
            tables,
            groups,
            end,
        })
    }
}

/// Where a table's text starts, if it has a header: at the comments and blank lines above it.
fn header_start(table: &Table, text: &str) -> Option<usize> {
    let span = table.span()?;
    if table.is_dotted() || span.is_empty() || !text[span.start..].starts_with('[') {
        return None;
    }
    Some(
        table
            .decor()
            .prefix()
            .and_then(|p| p.span())
            .map_or(span.start, |p| p.start),
    )
}

fn tables_under(
    item: &Item,
    group: Option<usize>,
    text: &str,
    out: &mut Vec<(usize, Option<usize>)>,
) {
    let tables: Vec<&Table> = match item {
        Item::Table(t) => vec![t],
        Item::ArrayOfTables(a) => a.iter().collect(),
        _ => return,
    };
    for table in tables {
        if let Some(start) = header_start(table, text) {
            out.push((start, group));
        }
        for (_, item) in table.iter() {
            tables_under(item, group, text, out);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Anchor {
    Before(usize),
    After(usize),
    /// After every table, before the comments that close the file.
    End,
}

struct Splice<'a> {
    text: &'a str,
    layout: &'a Layout,
    eol: &'a str,
}

impl Splice<'_> {
    /// Where `at` lies in the old text.
    fn before(&self, at: Anchor) -> usize {
        let groups = &self.layout.groups;
        match at {
            Anchor::Before(g) => groups[g].ranges[0].start,
            Anchor::After(g) => groups[g].ranges.last().expect("a table").end,
            Anchor::End => self.layout.end,
        }
    }

    /// The old text without annotation `skip`, with `put` placed at its anchor.
    fn apply(&self, skip: Option<usize>, put: Option<(Anchor, String)>) -> String {
        let text = self.text;
        let tables = &self.layout.tables;
        let mut out = String::with_capacity(text.len() + 256);
        let place = |out: &mut String, at: Anchor| {
            if let Some((anchor, new)) = &put
                && *anchor == at
            {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push_str(self.eol);
                }
                out.push_str(new);
            }
        };
        let end = self.layout.end;
        out.push_str(&text[..tables.first().map_or(end, |&(start, _)| start)]);
        for (k, &(start, group)) in tables.iter().enumerate() {
            let stop = tables.get(k + 1).map_or(end, |&(next, _)| next);
            let ranges = group.map(|g| &self.layout.groups[g].ranges);
            if let (Some(g), Some(r)) = (group, ranges)
                && r[0].start == start
            {
                place(&mut out, Anchor::Before(g));
            }
            if group.is_none() || group != skip {
                out.push_str(&text[start..stop]);
            }
            if let (Some(g), Some(r)) = (group, ranges)
                && r.last().is_some_and(|r| r.end == stop)
            {
                place(&mut out, Anchor::After(g));
            }
        }
        place(&mut out, Anchor::End);
        out.push_str(&text[end..]);
        out
    }
}

/// A new table holding `fields`, laid out as ANNOTATIONS.md's example, after a blank line if
/// `blank`.
fn new_table(fields: &Fields, blank: bool) -> String {
    let mut table = Table::new();
    for (key, value) in fields.values() {
        table.insert(key, Item::Value(value));
    }
    table.decor_mut().set_prefix(if blank { "\n" } else { "" });
    table.decor_mut().set_suffix("");
    let mut array = ArrayOfTables::new();
    array.push(table);
    let mut doc = DocumentMut::new();
    doc.insert("annotation", Item::ArrayOfTables(array));
    doc.to_string()
}

/// `text` with its line ends written as `eol`.
fn newlines(text: String, eol: &str) -> String {
    if eol == "\n" {
        text
    } else {
        text.replace("\r\n", "\n").replace('\n', eol)
    }
}

/// Sets the managed fields `fields` gives in `table`, and removes the others. A value that
/// doesn't change keeps how it is written; one that changes keeps the spacing and comment around
/// it. A new key goes after the managed keys that come before it in `MANAGED`.
fn set(table: &mut Table, fields: &Fields) {
    let given = fields.values();
    let rank = |k: &str| MANAGED.iter().position(|m| *m == k);
    for (mine, key) in MANAGED.into_iter().enumerate() {
        let Some((_, value)) = given.iter().find(|(k, _)| *k == key) else {
            table.remove(key);
            continue;
        };
        if let Some(item) = table.get_mut(key) {
            if same(item, value) {
                continue;
            }
            let mut value = value.clone();
            let was_value = match item.as_value() {
                Some(old) => {
                    *value.decor_mut() = old.decor().clone();
                    true
                }
                None => false,
            };
            *item = Item::Value(value);
            // A sub-table or dotted keys become one line: the key's spacing was the header's.
            if !was_value {
                table.key_mut(key).expect("just set").fmt();
            }
            continue;
        }
        let at = table
            .iter()
            .enumerate()
            .filter(|(_, (k, _))| rank(k).is_some_and(|r| r < mine))
            .map(|(i, _)| i + 1)
            .last()
            .unwrap_or(0);
        let later: Vec<String> = table.iter().skip(at).map(|(k, _)| k.to_owned()).collect();
        let moved: Vec<_> = later.iter().filter_map(|k| table.remove_entry(k)).collect();
        table.insert(key, Item::Value(value.clone()));
        for (k, item) in moved {
            table.insert_formatted(&k, item);
        }
    }
}

/// Whether `old` holds the value `new`, however it is written.
fn same(old: &Item, new: &Value) -> bool {
    match (old.as_value(), new) {
        (Some(old), new) => same_value(old, new),
        (None, Value::InlineTable(new)) => old.as_table_like().is_some_and(|t| {
            t.len() == new.len()
                && new
                    .iter()
                    .all(|(k, v)| t.get(k).is_some_and(|o| same(o, v)))
        }),
        _ => false,
    }
}

fn same_value(old: &Value, new: &Value) -> bool {
    match (old, new) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Integer(a), Value::Integer(b)) => a.value() == b.value(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same_value(a, b))
        }
        (Value::InlineTable(a), Value::InlineTable(b)) => {
            a.len() == b.len()
                && b.iter()
                    .all(|(k, v)| a.get(k).is_some_and(|a| same_value(a, v)))
        }
        _ => false,
    }
}
