//! Sidecar annotations: a hand-written TOML file beside a recording, and one JSON Lines file for
//! each generator that the TOML includes. `ANNOTATIONS.md` specifies both.
//!
//! `resolve` turns them into one list and checks it against the recording's header. It reports
//! every problem it finds, not only the first, so a page's build can show them all at once.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::header::{ChannelKind, ContentId, Declaration, DeclaredKind, Header};
use crate::model::Ratio;
use crate::record::check_source;
pub use crate::times::{ExactTicks, Time, exact_ticks, ticks};

pub const FORMAT: &str = "tickwave-annotations/1";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measure {
    pub channel: String,
    pub what: MeasureKind,
}

/// What a measure reads from a channel's edges in a span (ANNOTATIONS.md, "Measures").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MeasureKind {
    Rate,
    Period,
    Duration,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: String,
    #[serde(rename = "ref")]
    target: Option<String>,
    start: Option<Time>,
    end: Option<Time>,
    channels: Option<Vec<String>>,
    label: Option<String>,
    kind: Option<String>,
    text: Option<String>,
    paint: Option<String>,
    measure: Option<Measure>,
    values: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HandFile {
    format: String,
    recording: Option<String>,
    recording_id: ContentId,
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    annotation: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratorHead {
    format: String,
    source: String,
    // Accepted for provenance; nothing reads it.
    #[allow(dead_code)]
    version: Option<String>,
    recording_id: ContentId,
}

/// One annotation, resolved: times in ticks, a `ref` followed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    /// The full id: a hand-written id, or `<source>/<id>` for a generated one.
    pub id: String,
    /// The generator's name; None for a hand-written annotation.
    pub source: Option<String>,
    /// For a hand-written alias, the full id of the generated annotation it names.
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub start: i64,
    /// None for a point.
    pub end: Option<i64>,
    /// Empty means every channel.
    #[serde(default)]
    pub channels: Vec<String>,
    pub label: Option<String>,
    pub kind: Option<String>,
    pub text: Option<String>,
    pub paint: Option<String>,
    pub measure: Option<Measure>,
    /// Figures the annotation states, by name: each a decimal and its unit, as `"37.5 MHz"`, or
    /// a decimal alone for a count, as `"17"`.
    pub values: Option<BTreeMap<String, String>>,
}

/// A generator's file, as handed to `resolve`: the name the TOML includes it by, and its text.
pub struct Included<'a> {
    pub name: &'a str,
    pub text: &'a str,
}

/// Why a `values` entry is refused, if it is: its name must be lower-case letters, digits and
/// `_`, starting with a letter, and its value a decimal, then one space and a unit where it has
/// one, as `"37.5 MHz"`, or `"17"` for a count. A chart compares units as text, so micro is
/// U+00B5 alone, as in times, never Greek mu U+03BC.
fn check_value(name: &str, value: &str) -> Option<String> {
    let mut chars = name.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase())
        || !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Some(format!(
            "values: name {name:?} is not lower-case letters, digits and '_', starting with a letter"
        ));
    }
    let decimal = |s: &str| {
        let digits = s.strip_prefix('-').unwrap_or(s);
        let (whole, fraction) = match digits.split_once('.') {
            Some((w, f)) => (w, Some(f)),
            None => (digits, None),
        };
        let all_digits = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
        all_digits(whole) && fraction.is_none_or(all_digits)
    };
    let fits = match value.split_once(' ') {
        Some((number, unit)) => {
            decimal(number) && !unit.is_empty() && !unit.chars().any(char::is_whitespace)
        }
        None => decimal(value),
    };
    if !fits {
        return Some(format!(
            "values: {name} = {value:?} is not a decimal with one space and a unit where it has \
             one, as \"37.5 MHz\" or \"17\""
        ));
    }
    value.contains('\u{3bc}').then(|| {
        format!("values: {name} = {value:?} writes micro as Greek mu (U+03BC); use µ (U+00B5)")
    })
}

/// The problems with one annotation's id and values.
fn field_problems(entry: &Entry, generated: bool) -> Vec<String> {
    let mut problems: Vec<String> = check_id(&entry.id, generated).into_iter().collect();
    for (name, value) in entry.values.iter().flatten() {
        problems.extend(check_value(name, value));
    }
    problems
}

const GENERATED_REF: &str = "only hand-written annotations take a ref";

/// The problems with where an annotation lies: its ticks against the recording's length, where
/// that is known, and its channels and measure against the recording's. `digital` says whether a
/// channel of that name is digital, None where there is none.
fn place_problems(
    start: i64,
    end: Option<i64>,
    channels: &[String],
    measure: Option<&Measure>,
    length: Option<i64>,
    digital: impl Fn(&str) -> Option<bool>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let last = end.unwrap_or(start);
    // A span's end is exclusive, so it may sit at the length; a point may not.
    match length {
        Some(length) if start < 0 || start >= length || last > length => problems.push(format!(
            "ticks {start} to {last} fall outside the capture, 0..{length}"
        )),
        None if start < 0 => problems.push(format!("tick {start} lies before the capture")),
        _ => {}
    }
    if last < start {
        problems.push("ends before it starts".to_owned());
    }
    for c in channels {
        if digital(c).is_none() {
            problems.push(format!("no channel {c:?}"));
        }
    }
    if let Some(m) = measure {
        match digital(&m.channel) {
            None => problems.push(format!("measure: no channel {:?}", m.channel)),
            Some(false) => problems.push(format!(
                "measure: {:?} is analog; measures are taken from edges",
                m.channel
            )),
            Some(true) => {}
        }
        if end.is_none() {
            problems.push("measure needs a span, not a point".to_owned());
        }
    }
    problems
}

/// Generators' lines, held to what `resolve` asks of each before the recording's length is known,
/// as a writer takes them: the line's fields and their types, times in whole ticks, and a place on
/// the recording's channels. Ids are not held to be unique, which would mean holding every one: a
/// reader keeps the first of an id its generator gives twice (FORMAT.md, "Annotations"). A reader
/// drops a line that starts at or past the length and ends a span there, which keeps the rest.
pub struct Lines {
    /// Each channel's name, and whether it is digital.
    channels: BTreeMap<String, bool>,
}

impl Lines {
    pub fn new(channels: impl IntoIterator<Item = (String, bool)>) -> Self {
        Self {
            channels: channels.into_iter().collect(),
        }
    }

    /// Lines on the channels `declaration` declares.
    pub fn declared(declaration: &Declaration) -> Self {
        Self::new(declaration.channels.iter().map(|c| {
            let digital = matches!(c.data, DeclaredKind::Digital { .. });
            (c.name.clone(), digital)
        }))
    }

    /// As `take`, for a line as its file's text gives it.
    pub fn take_line(&mut self, source: &str, line: &str) -> Result<(), String> {
        match serde_json::from_str(line) {
            Ok(fields) => self.take(source, &fields),
            Err(e) => Err(format!("annotation with no id: {e}")),
        }
    }

    /// As `take_line`, for a line of a generator's file rather than an annotation record: its
    /// times may be decimals with a unit, at `timebase`, as ANNOTATIONS.md allows a file's.
    pub fn take_file_line(
        &mut self,
        source: &str,
        line: &str,
        timebase: Ratio,
    ) -> Result<(), String> {
        match serde_json::from_str(line) {
            Ok(fields) => self.check(source, &fields, Some(timebase)),
            Err(e) => Err(format!("annotation with no id: {e}")),
        }
    }

    /// Takes generator `source`'s `line`, or says why it can't stand: `annotation "<id>": ...`,
    /// or `annotation with no id: ...`.
    pub fn take(
        &mut self,
        source: &str,
        line: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        self.check(source, line, None)
    }

    /// `take`, with times in ticks alone where `timebase` is None, or also as decimals with a
    /// unit at it.
    fn check(
        &mut self,
        source: &str,
        line: &serde_json::Map<String, serde_json::Value>,
        timebase: Option<Ratio>,
    ) -> Result<(), String> {
        let who = match line.get("id").and_then(|id| id.as_str()) {
            Some(id) => format!("annotation {id:?}"),
            None => "annotation with no id".to_owned(),
        };
        let refused = |problem: &str| Err(format!("{who}: {problem}"));
        if let Some(problem) = check_source(source) {
            return refused(&problem);
        }
        let entry: Entry = match serde_json::from_value(serde_json::Value::Object(line.clone())) {
            Ok(entry) => entry,
            Err(e) => return refused(&e.to_string()),
        };
        let tick = |key: &'static str| {
            line.get(key)
                .map(|t| match (t.as_i64(), t.as_str(), timebase) {
                    (Some(t), ..) => Ok(t),
                    (None, Some(text), Some(timebase)) => {
                        ticks(&Time::Text(text.to_owned()), timebase)
                            .map_err(|e| format!("{key}: {e}"))
                    }
                    _ => Err(format!(
                        "{key} is not a whole tick count: a writer's times are axis ticks"
                    )),
                })
        };
        let (start, end) = match (tick("start"), tick("end").transpose()) {
            (Some(Ok(start)), Ok(end)) => (start, end),
            (None, _) => return refused("no start"),
            (Some(Err(why)), _) | (_, Err(why)) => return refused(&why),
        };
        let mut problems = field_problems(&entry, true);
        if entry.target.is_some() {
            problems.push(GENERATED_REF.to_owned());
        }
        problems.extend(place_problems(
            start,
            end,
            entry.channels.as_deref().unwrap_or_default(),
            entry.measure.as_ref(),
            None,
            |c| self.channels.get(c).copied(),
        ));
        if !problems.is_empty() {
            return refused(&problems.join("; "));
        }
        Ok(())
    }
}

fn check_id(id: &str, generated: bool) -> Option<String> {
    if id.is_empty() {
        return Some("an empty id".into());
    }
    if id.chars().any(char::is_whitespace) {
        return Some(format!("id {id:?} holds whitespace"));
    }
    if !generated && id.contains('/') {
        return Some(format!(
            "hand-written id {id:?} holds a '/', which only generated ids carry"
        ));
    }
    None
}

/// The generator a sidecar file is from, when its first line is a generator's head: a file a
/// merge carries, where a hand-written file's annotations stay behind.
pub fn generator_source(text: &str) -> Option<String> {
    let first = text.lines().find(|l| !l.trim().is_empty())?;
    let head: GeneratorHead = json(first).ok()?;
    (head.format == FORMAT).then_some(head.source)
}

/// The files a hand-written file includes, in its order; None when its TOML doesn't read.
#[cfg(feature = "toml")]
pub fn include_order(hand: &str) -> Option<Vec<String>> {
    #[derive(Deserialize)]
    struct Include {
        #[serde(default)]
        include: Vec<String>,
    }
    toml::from_str::<Include>(hand).ok().map(|i| i.include)
}

/// How many annotations a hand-written file holds; None when its TOML doesn't read.
#[cfg(feature = "toml")]
pub fn hand_written_count(hand: &str) -> Option<usize> {
    #[derive(Deserialize)]
    struct Annotations {
        #[serde(default)]
        annotation: Vec<toml::Value>,
    }
    toml::from_str::<Annotations>(hand)
        .ok()
        .map(|a| a.annotation.len())
}

/// Resolves and checks a sidecar against the recording's header, and against the recording's
/// file name when the caller knows it. `hand` is the hand-written file's TOML. Returns the
/// annotations, or every problem found.
#[cfg(feature = "toml")]
pub fn resolve(
    hand: &str,
    included: &[Included<'_>],
    header: &Header,
    recording_file: Option<&str>,
) -> Result<Vec<Annotation>, Vec<String>> {
    match toml::from_str(hand) {
        Ok(file) => resolve_file(file, included, header, recording_file),
        Err(e) => Err(vec![format!("the TOML file: {e}")]),
    }
}

/// Parses JSON text as bytes, as the header is parsed: the WASM module then carries one of
/// serde_json's readers rather than two.
fn json<'a, T: Deserialize<'a>>(text: &'a str) -> serde_json::Result<T> {
    serde_json::from_slice(text.as_bytes())
}

/// As `resolve`, with the hand-written file's TOML already parsed and written out as JSON.
pub fn resolve_json(
    hand: &str,
    included: &[Included<'_>],
    header: &Header,
    recording_file: Option<&str>,
) -> Result<Vec<Annotation>, Vec<String>> {
    match json(hand) {
        Ok(file) => resolve_file(file, included, header, recording_file),
        // The position is in the JSON the page made from the TOML, not in any file its author has.
        Err(e) => {
            let text = e.to_string();
            let at = format!(" at line {} column {}", e.line(), e.column());
            Err(vec![format!(
                "the TOML file: {}",
                text.strip_suffix(&at).unwrap_or(&text)
            )])
        }
    }
}

/// Where an entry came from, as a problem names it.
#[derive(Clone, Copy)]
enum At<'a> {
    /// The TOML file's annotation, counted from 1.
    Hand(usize),
    /// An included file's line, counted from 1.
    Line(&'a str, usize),
}

impl std::fmt::Display for At<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            At::Hand(n) => write!(f, "annotation {n}"),
            At::Line(name, n) => write!(f, "{name}, line {n}"),
        }
    }
}

/// Whom a problem found while resolving belongs to: one annotation, by its place in sidecar
/// order, an included file's lines, or the sidecar as a whole.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner<'a> {
    Entry(usize),
    File(&'a str),
    Sidecar,
}

/// A sidecar resolved as far as it goes: each annotation that resolved, with its place in
/// sidecar order and its file, and every problem found, with whom it belongs to.
struct Found<'a> {
    annotations: Vec<(usize, At<'a>, Annotation)>,
    problems: Vec<(Owner<'a>, String)>,
}

impl Found<'_> {
    /// The annotations a lenient reader keeps, with their places: those but the ones that break a
    /// rule, or whose file's first line does. And why it leaves out each `ref` whose target it
    /// leaves out: a `ref` resolves only to an annotation the reader keeps.
    fn kept(&self) -> (Vec<(usize, &Annotation)>, Vec<String>) {
        let mut entries = BTreeSet::new();
        let mut files = BTreeSet::new();
        for (owner, _) in &self.problems {
            match owner {
                Owner::Entry(p) => {
                    entries.insert(*p);
                }
                Owner::File(name) => {
                    files.insert(*name);
                }
                Owner::Sidecar => {}
            }
        }
        let dropped = |p: usize, at: &At<'_>| {
            entries.contains(&p) || matches!(at, At::Line(file, _) if files.contains(file))
        };
        let kept_ids: BTreeSet<&str> = self
            .annotations
            .iter()
            .filter(|(p, at, a)| a.source.is_some() && !dropped(*p, at))
            .map(|(_, _, a)| a.id.as_str())
            .collect();
        let left_out: BTreeSet<&str> = self
            .annotations
            .iter()
            .filter(|(p, at, a)| a.source.is_some() && dropped(*p, at))
            .map(|(_, _, a)| a.id.as_str())
            .filter(|id| !kept_ids.contains(id))
            .collect();
        let mut problems = Vec::new();
        let kept = self
            .annotations
            .iter()
            .filter(|(p, at, a)| {
                if dropped(*p, at) {
                    return false;
                }
                match &a.target {
                    Some(t) if left_out.contains(t.as_str()) => {
                        problems.push(format!("{at}: ref {t:?} names an annotation left out"));
                        false
                    }
                    _ => true,
                }
            })
            .map(|(p, _, a)| (*p, a))
            .collect();
        (kept, problems)
    }
}

fn resolve_file(
    file: HandFile,
    included: &[Included<'_>],
    header: &Header,
    recording_file: Option<&str>,
) -> Result<Vec<Annotation>, Vec<String>> {
    let found = find(file, included, header, recording_file, None);
    if found.problems.is_empty() {
        Ok(found.annotations.into_iter().map(|(.., a)| a).collect())
    } else {
        Err(found.problems.into_iter().map(|(_, p)| p).collect())
    }
}

/// A sidecar resolved the way a reader shows one (FORMAT.md, "Annotations"): each annotation that
/// breaks a rule is left out, and so are an included file's lines where its first line does,
/// rather than the whole sidecar; the first of an id given twice stays. `problems` names each.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resolved {
    pub annotations: Vec<Annotation>,
    pub problems: Vec<String>,
}

/// One line of generator `source`'s file resolved alone, as `resolve` resolves it: its fields,
/// times and place held to the recording's `header`, or every rule it breaks. What only the whole
/// sidecar says, an id given twice or a `ref`, it can't check: a reader that reads a few parts
/// of a recording's annotations resolves their lines this way.
pub fn resolve_line(source: &str, line: &str, header: &Header) -> Result<Annotation, Vec<String>> {
    let entry: Entry = json(line).map_err(|e| vec![e.to_string()])?;
    let mut problems = field_problems(&entry, true);
    if entry.target.is_some() {
        problems.push(GENERATED_REF.to_owned());
    }
    let time = |t: &Option<Time>, key: &str| {
        t.as_ref()
            .map(|t| ticks(t, header.timebase).map_err(|e| format!("{key}: {e}")))
            .transpose()
    };
    let (start, end) = match (time(&entry.start, "start"), time(&entry.end, "end")) {
        (Ok(Some(start)), Ok(end)) => (start, end),
        (Ok(None), _) => return Err(vec!["no start".into()]),
        (Err(e), _) | (_, Err(e)) => return Err(vec![e]),
    };
    // Held to the length as a reader holds a part's lines (FORMAT.md, "Annotations").
    if start >= header.length {
        return Err(vec![format!(
            "starts at or past the length, {}",
            header.length
        )]);
    }
    let end = end.map(|e| e.min(header.length));
    let channels: BTreeMap<&str, bool> = header
        .channels
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                matches!(c.data, ChannelKind::Digital { .. }),
            )
        })
        .collect();
    let a = Annotation {
        id: format!("{source}/{}", entry.id),
        source: Some(source.to_owned()),
        target: None,
        start,
        end,
        channels: entry.channels.unwrap_or_default(),
        label: entry.label,
        kind: entry.kind,
        text: entry.text,
        paint: entry.paint,
        measure: entry.measure,
        values: entry.values,
    };
    problems.extend(place_problems(
        a.start,
        a.end,
        &a.channels,
        a.measure.as_ref(),
        Some(header.length),
        |c| channels.get(c).copied(),
    ));
    if problems.is_empty() {
        Ok(a)
    } else {
        Err(problems)
    }
}

/// A sidecar resolved apart from its generator files' lines, as a reader that reads those from a
/// recording's annotation parts resolves it, leaving out what breaks a rule as `resolve_lenient`
/// does: `included` the bundle's generator files, each its first line alone. Each `ref` is
/// followed through `targets`, the generated annotations by full id.
#[cfg(feature = "toml")]
pub fn resolve_apart<'a>(
    hand: &str,
    included: &[Included<'a>],
    header: &Header,
    targets: Targets<'_>,
) -> Apart<'a> {
    match toml::from_str(hand) {
        Ok(file) => apart(file, included, header, targets),
        Err(e) => Apart {
            problems: vec![format!("the TOML file: {e}")],
            ..Apart::default()
        },
    }
}

/// As `resolve_apart`, with the hand-written file's TOML already parsed and written out as JSON.
pub fn resolve_apart_json<'a>(
    hand: &str,
    included: &[Included<'a>],
    header: &Header,
    targets: Targets<'_>,
) -> Apart<'a> {
    match json(hand) {
        Ok(file) => apart(file, included, header, targets),
        Err(e) => {
            // The position is in the JSON the page made from the TOML, as in `resolve_json`.
            let text = e.to_string();
            let at = format!(" at line {} column {}", e.line(), e.column());
            Apart {
                problems: vec![format!(
                    "the TOML file: {}",
                    text.strip_suffix(&at).unwrap_or(&text)
                )],
                ..Apart::default()
            }
        }
    }
}

/// `included` in `include`'s order: those it doesn't list first, each in the order given, as a
/// stable sort by place in `include` would put them, without a sort's code for each caller.
fn in_include_order<'b, 'a>(
    include: &[String],
    included: &'b [Included<'a>],
) -> Vec<&'b Included<'a>> {
    let places: Vec<Option<usize>> = included
        .iter()
        .map(|inc| include.iter().position(|n| n == inc.name))
        .collect();
    let at = |place: Option<usize>| {
        included
            .iter()
            .zip(&places)
            .filter(move |(_, p)| **p == place)
            .map(|(inc, _)| inc)
    };
    let mut order: Vec<&Included<'a>> = at(None).collect();
    for k in 0..include.len() {
        order.extend(at(Some(k)));
    }
    order
}

fn apart<'a>(
    file: HandFile,
    included: &[Included<'a>],
    header: &Header,
    targets: Targets<'_>,
) -> Apart<'a> {
    let order = in_include_order(&file.include, included);
    let found = find(file, included, header, None, Some(targets));
    let (kept, left_out) = found.kept();
    let annotations = kept.into_iter().map(|(p, a)| (p, a.clone())).collect();
    let dropped = |owner: Owner<'_>| found.problems.iter().any(|(o, _)| *o == owner);
    let files = order
        .into_iter()
        .map(|inc| (inc.name, !dropped(Owner::File(inc.name))))
        .collect();
    let mut problems: Vec<String> = found.problems.into_iter().map(|(_, p)| p).collect();
    problems.extend(left_out);
    Apart {
        annotations,
        files,
        problems,
    }
}

/// `resolve_apart`'s answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Apart<'a> {
    /// The hand-written file's annotations kept, each with its place in it.
    pub annotations: Vec<(usize, Annotation)>,
    /// The included files in sidecar order, which puts their lines after the hand-written file's
    /// annotations, each with whether its lines are kept: not where its first line breaks a rule.
    pub files: Vec<(&'a str, bool)>,
    pub problems: Vec<String>,
}

/// As `resolve`, leaving out what breaks a rule rather than refusing the sidecar.
#[cfg(feature = "toml")]
pub fn resolve_lenient(
    hand: &str,
    included: &[Included<'_>],
    header: &Header,
    recording_file: Option<&str>,
) -> Resolved {
    match toml::from_str(hand) {
        Ok(file) => {
            let found = find(file, included, header, recording_file, None);
            let (kept, left_out) = found.kept();
            let annotations = kept.into_iter().map(|(_, a)| a.clone()).collect();
            let mut problems: Vec<String> = found.problems.into_iter().map(|(_, p)| p).collect();
            problems.extend(left_out);
            Resolved {
                annotations,
                problems,
            }
        }
        Err(e) => Resolved {
            annotations: Vec::new(),
            problems: vec![format!("the TOML file: {e}")],
        },
    }
}

/// The generated annotations a hand-written file's `ref` can name where its generator files are
/// read apart from it, as a reader of a few parts reads them: by full id.
pub type Targets<'t> = &'t dyn Fn(&str) -> Option<Annotation>;

fn find<'a>(
    file: HandFile,
    included: &[Included<'a>],
    header: &Header,
    recording_file: Option<&str>,
    elsewhere: Option<Targets<'_>>,
) -> Found<'a> {
    let mut problems: Vec<(Owner<'a>, String)> = Vec::new();
    if file.format != FORMAT {
        problems.push((
            Owner::Sidecar,
            format!(
                "the TOML file's format is {:?}, not {FORMAT:?}",
                file.format
            ),
        ));
    }
    if let (Some(named), Some(actual)) = (&file.recording, recording_file)
        && named != actual
    {
        problems.push((
            Owner::Sidecar,
            format!("the TOML file names {named:?}, not {actual:?}"),
        ));
    }
    if file.recording_id != header.content_id {
        problems.push((
            Owner::Sidecar,
            format!(
                "the TOML file is for {}, but the recording is {}",
                file.recording_id, header.content_id
            ),
        ));
    }
    for name in &file.include {
        if name.contains(['/', '\\']) || name == ".." || name == "." {
            problems.push((
                Owner::Sidecar,
                format!("{name}: an included file is named on its own, as it sits beside this one"),
            ));
        }
    }
    let listed: BTreeSet<&str> = file.include.iter().map(String::as_str).collect();
    let given: BTreeSet<&str> = included.iter().map(|i| i.name).collect();
    for name in listed.difference(&given) {
        problems.push((Owner::Sidecar, format!("{name}: included but not provided")));
    }
    for name in given.difference(&listed) {
        problems.push((Owner::Sidecar, format!("{name}: provided but not included")));
    }

    // (source, entry, where it came from)
    let mut entries: Vec<(Option<String>, Entry, At<'a>)> = Vec::new();
    for (i, entry) in file.annotation.into_iter().enumerate() {
        entries.push((None, entry, At::Hand(i + 1)));
    }
    // Sidecar order: the TOML file's annotations, then each included file's in `include` order.
    for inc in in_include_order(&file.include, included) {
        let mut lines = inc
            .text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty());
        let whole = Owner::File(inc.name);
        let Some((first_n, first)) = lines.next() else {
            problems.push((whole, format!("{}: empty", inc.name)));
            continue;
        };
        let head: GeneratorHead = match json(first) {
            Ok(head) => head,
            Err(e) => {
                problems.push((whole, format!("{}, line {}: {e}", inc.name, first_n + 1)));
                continue;
            }
        };
        if head.format != FORMAT {
            problems.push((
                whole,
                format!("{}: format {:?}, not {FORMAT:?}", inc.name, head.format),
            ));
        }
        if head.recording_id != header.content_id {
            problems.push((
                whole,
                format!(
                    "{}: generated for {}, but the recording is {}",
                    inc.name, head.recording_id, header.content_id
                ),
            ));
        }
        if let Some(problem) = check_source(&head.source) {
            problems.push((whole, format!("{}: {problem}", inc.name)));
        }
        for (n, line) in lines {
            match json::<Entry>(line) {
                Ok(entry) => {
                    entries.push((Some(head.source.clone()), entry, At::Line(inc.name, n + 1)))
                }
                // A line that doesn't read is no annotation: nothing of it is left to drop.
                Err(e) => problems.push((
                    Owner::Entry(usize::MAX),
                    format!("{}, line {}: {e}", inc.name, n + 1),
                )),
            }
        }
    }

    let channels: BTreeMap<&str, bool> = header
        .channels
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                matches!(c.data, ChannelKind::Digital { .. }),
            )
        })
        .collect();
    let mut seen = BTreeSet::new();
    // (position in sidecar order, where it came from, annotation)
    let mut plain = Vec::with_capacity(entries.len());
    let mut refs = Vec::new();
    for (position, (source, entry, at)) in entries.into_iter().enumerate() {
        let id = match &source {
            Some(s) => format!("{s}/{}", entry.id),
            None => entry.id.clone(),
        };
        let own = Owner::Entry(position);
        for problem in field_problems(&entry, source.is_some()) {
            problems.push((own, format!("{at}: {problem}")));
        }
        if !seen.insert(id.clone()) {
            problems.push((own, format!("{at}: id {id:?} is used twice")));
        }
        if let Some(target) = entry.target.clone() {
            if source.is_some() {
                problems.push((own, format!("{at}: {GENERATED_REF}")));
            }
            if entry.start.is_some() || entry.end.is_some() {
                problems.push((
                    own,
                    format!("{at}: a ref takes its span from its target; drop start and end"),
                ));
            }
            refs.push((position, id, target, entry, at));
            continue;
        }
        let Some(start) = &entry.start else {
            problems.push((own, format!("{at}: no start")));
            continue;
        };
        let start = match ticks(start, header.timebase) {
            Ok(t) => t,
            Err(e) => {
                problems.push((own, format!("{at}: start: {e}")));
                continue;
            }
        };
        let end = match entry
            .end
            .as_ref()
            .map(|e| ticks(e, header.timebase))
            .transpose()
        {
            Ok(e) => e,
            Err(e) => {
                problems.push((own, format!("{at}: end: {e}")));
                continue;
            }
        };
        plain.push((
            position,
            at,
            Annotation {
                id,
                source,
                target: None,
                start,
                end,
                channels: entry.channels.unwrap_or_default(),
                label: entry.label,
                kind: entry.kind,
                text: entry.text,
                paint: entry.paint,
                measure: entry.measure,
                values: entry.values,
            },
        ));
    }
    // A ref names a generated annotation (ANNOTATIONS.md): a name for a decoder's find, not a
    // second name for a hand-written one.
    let by_id: BTreeMap<&str, &Annotation> = if refs.is_empty() {
        BTreeMap::new()
    } else {
        // The first of an id, which a reader keeps.
        let mut by_id = BTreeMap::new();
        for (_, _, a) in plain.iter().filter(|(_, _, a)| a.source.is_some()) {
            by_id.entry(a.id.as_str()).or_insert(a);
        }
        by_id
    };
    let mut followed = Vec::with_capacity(refs.len());
    for (position, id, target, entry, at) in refs {
        let found = elsewhere.and_then(|f| f(&target));
        let Some(t) = by_id.get(target.as_str()).copied().or(found.as_ref()) else {
            let hand = plain.iter().any(|(_, _, a)| a.id == target);
            problems.push((
                Owner::Entry(position),
                if hand {
                    format!(
                        "{at}: ref {target:?} names a hand-written annotation, not a generated one"
                    )
                } else {
                    format!("{at}: ref {target:?} does not resolve")
                },
            ));
            continue;
        };
        followed.push((
            position,
            at,
            Annotation {
                id,
                source: None,
                target: Some(target),
                start: t.start,
                end: t.end,
                channels: entry.channels.unwrap_or_else(|| t.channels.clone()),
                label: entry.label.or_else(|| t.label.clone()),
                kind: entry.kind.or_else(|| t.kind.clone()),
                text: entry.text.or_else(|| t.text.clone()),
                paint: entry.paint.or_else(|| t.paint.clone()),
                measure: entry.measure.or_else(|| t.measure.clone()),
                values: entry.values.or_else(|| t.values.clone()),
            },
        ));
    }
    plain.extend(followed);

    for (position, at, a) in &plain {
        let place = place_problems(
            a.start,
            a.end,
            &a.channels,
            a.measure.as_ref(),
            Some(header.length),
            |c| channels.get(c).copied(),
        );
        problems.extend(
            place
                .into_iter()
                .map(|p| (Owner::Entry(*position), format!("{at}: {p}"))),
        );
    }
    plain.sort_by_key(|(position, ..)| *position);
    Found {
        annotations: plain,
        problems,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GS: Ratio = Ratio::whole(std::num::NonZeroU64::new(1_000_000_000).unwrap());

    fn text(s: &str) -> Time {
        Time::Text(s.into())
    }

    #[test]
    fn decimal_times_convert_exactly() {
        // By hand: 438.208327 ms at 1 ns a tick is 438,208,327 ticks.
        assert_eq!(ticks(&text("438.208327 ms"), GS), Ok(438_208_327));
        // 0.1 ms is 100,000 ticks; a float route gives 99,999.99... and truncation would be wrong.
        assert_eq!(ticks(&text("0.1ms"), GS), Ok(100_000));
        assert_eq!(ticks(&text("1.5 us"), GS), Ok(1_500));
        assert_eq!(ticks(&text("1.5 µs"), GS), Ok(1_500));
        assert_eq!(ticks(&text("2 s"), GS), Ok(2_000_000_000));
        // Half a tick rounds away from zero; less than half rounds down.
        assert_eq!(ticks(&text("0.5 ns"), GS), Ok(1));
        assert_eq!(ticks(&text("0.49 ns"), GS), Ok(0));
        assert_eq!(ticks(&text("-0.5 ns"), GS), Ok(-1));
        // At 100 kHz a tick is 10 us: 15 us is 1.5 ticks, so 2; 14 us is 1.4, so 1.
        assert_eq!(
            ticks(&text("15 us"), Ratio::new(100_000, 1).unwrap()),
            Ok(2)
        );
        assert_eq!(
            ticks(&text("14 us"), Ratio::new(100_000, 1).unwrap()),
            Ok(1)
        );
        // A ratio timebase: 3 ticks a microsecond as 3,000,000/1 and as 6,000,000/2 agree.
        assert_eq!(
            ticks(&text("7 us"), Ratio::new(6_000_000, 2).unwrap()),
            Ok(21)
        );
        assert_eq!(ticks(&Time::Ticks(42), GS), Ok(42));
    }

    #[test]
    fn bad_times_are_refused() {
        for bad in ["12", "12 minutes", "ms", "1.2.3 ms", "1e3 ns", "--1 ns"] {
            assert!(ticks(&text(bad), GS).is_err(), "{bad}");
        }
    }
}
