//! Sidecar annotations: a hand-written TOML file beside a recording, and one JSON Lines file for
//! each generator that the TOML includes. `ANNOTATIONS.md` specifies both.
//!
//! `resolve` turns them into one list and checks it against the recording's header. It reports
//! every problem it finds, not only the first, so a page's build can show them all at once.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::header::{ChannelKind, Header};

pub const FORMAT: &str = "tickwave-annotations/1";

/// A time as a file writes it: a tick count, or a decimal with a unit.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Time {
    Ticks(i64),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Measure {
    pub channel: String,
    /// `rate`, `period` or `duration`.
    pub what: String,
}

const MEASURES: [&str; 3] = ["rate", "period", "duration"];

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
    recording_id: String,
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
    recording_id: String,
}

/// One annotation, resolved: times in ticks, a `ref` followed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Annotation {
    /// The full id: a hand-written id, or `<source>/<id>` for a generated one.
    pub id: String,
    /// The generator's name; None for a hand-written annotation.
    pub source: Option<String>,
    /// For a hand-written alias, the full id of the generated annotation it names.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub start: i64,
    /// None for a point.
    pub end: Option<i64>,
    /// Empty means every channel.
    pub channels: Vec<String>,
    pub label: Option<String>,
    pub kind: Option<String>,
    pub text: Option<String>,
    pub paint: Option<String>,
    pub measure: Option<Measure>,
    /// Figures the annotation states, by name: each a decimal and a unit, as `"37.5 MHz"`.
    pub values: Option<BTreeMap<String, String>>,
}

/// A generator's file, as handed to `resolve`: the name the TOML includes it by, and its text.
pub struct Included<'a> {
    pub name: &'a str,
    pub text: &'a str,
}

/// Converts a time to ticks, rounding to the nearest tick (halves away from zero). A text time is
/// a decimal and a unit (`s`, `ms`, `us`, `µs`, `ns`, `ps`), with or without a space between.
/// The arithmetic is exact: `0.1 ms` at 1 GS/s is 100,000 ticks, not 99,999.
pub fn ticks(time: &Time, timebase: [u64; 2]) -> Result<i64, String> {
    let text = match time {
        Time::Ticks(t) => return Ok(*t),
        Time::Text(s) => s.trim(),
    };
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .ok_or_else(|| format!("{text:?}: no unit"))?;
    let (number, unit) = (text[..split].trim(), text[split..].trim());
    let unit_exp: u32 = match unit {
        "s" => 0,
        "ms" => 3,
        "us" | "µs" => 6,
        "ns" => 9,
        "ps" => 12,
        _ => return Err(format!("{text:?}: unknown unit {unit:?}")),
    };
    let (negative, digits) = match number.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, number.strip_prefix('+').unwrap_or(number)),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole
            .chars()
            .chain(fraction.chars())
            .all(|c| c.is_ascii_digit())
    {
        return Err(format!("{text:?}: not a decimal"));
    }
    let too_big = || format!("{text:?}: out of range");
    let mantissa: i128 = format!("{whole}{fraction}")
        .parse()
        .map_err(|_| too_big())?;
    // seconds = mantissa / 10^(fraction digits + unit exponent); ticks = seconds × num / den.
    let scale_exp = fraction.len() as u32 + unit_exp;
    let denominator = 10i128
        .checked_pow(scale_exp)
        .and_then(|p| p.checked_mul(i128::from(timebase[1])))
        .ok_or_else(too_big)?;
    let numerator = mantissa
        .checked_mul(i128::from(timebase[0]))
        .ok_or_else(too_big)?;
    let rounded = (2 * numerator + denominator) / (2 * denominator);
    let value = i64::try_from(rounded).map_err(|_| too_big())?;
    Ok(if negative { -value } else { value })
}

/// Why a `values` entry is refused, if it is: its name must be lower-case letters, digits and
/// `_`, starting with a letter, and its value a decimal, one space and a unit, as `"37.5 MHz"`. A
/// chart compares units as text, so micro is U+00B5 alone, as in times, never Greek mu U+03BC.
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
    match value.split_once(' ') {
        Some((number, unit))
            if decimal(number) && !unit.is_empty() && !unit.chars().any(char::is_whitespace) =>
        {
            None
        }
        _ => Some(format!(
            "values: {name} = {value:?} is not a decimal and a unit, as \"37.5 MHz\""
        )),
    }
    .or_else(|| {
        value.contains('\u{3bc}').then(|| {
            format!("values: {name} = {value:?} writes micro as Greek mu (U+03BC); use µ (U+00B5)")
        })
    })
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

/// As `resolve`, with the hand-written file's TOML already parsed and written out as JSON.
pub fn resolve_json(
    hand: &str,
    included: &[Included<'_>],
    header: &Header,
    recording_file: Option<&str>,
) -> Result<Vec<Annotation>, Vec<String>> {
    match serde_json::from_str(hand) {
        Ok(file) => resolve_file(file, included, header, recording_file),
        Err(e) => Err(vec![format!("the TOML file: {e}")]),
    }
}

fn resolve_file(
    file: HandFile,
    included: &[Included<'_>],
    header: &Header,
    recording_file: Option<&str>,
) -> Result<Vec<Annotation>, Vec<String>> {
    let mut problems = Vec::new();
    if file.format != FORMAT {
        problems.push(format!(
            "the TOML file's format is {:?}, not {FORMAT:?}",
            file.format
        ));
    }
    if let (Some(named), Some(actual)) = (&file.recording, recording_file)
        && named != actual
    {
        problems.push(format!("the TOML file names {named:?}, not {actual:?}"));
    }
    if file.recording_id != header.content_id {
        problems.push(format!(
            "the TOML file is for {}, but the recording is {}",
            file.recording_id, header.content_id
        ));
    }
    let listed: BTreeSet<&str> = file.include.iter().map(String::as_str).collect();
    let given: BTreeSet<&str> = included.iter().map(|i| i.name).collect();
    for name in listed.difference(&given) {
        problems.push(format!("{name}: included but not provided"));
    }
    for name in given.difference(&listed) {
        problems.push(format!("{name}: provided but not included"));
    }

    // (source, entry, where it came from)
    let mut entries: Vec<(Option<String>, Entry, String)> = Vec::new();
    for (i, entry) in file.annotation.into_iter().enumerate() {
        entries.push((None, entry, format!("annotation {}", i + 1)));
    }
    // Sidecar order: the TOML file's annotations, then each included file's in `include` order.
    let mut by_include: Vec<&Included<'_>> = included.iter().collect();
    by_include.sort_by_key(|inc| file.include.iter().position(|n| n == inc.name));
    for inc in by_include {
        let mut lines = inc
            .text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty());
        let Some((_, first)) = lines.next() else {
            problems.push(format!("{}: empty", inc.name));
            continue;
        };
        let head: GeneratorHead = match serde_json::from_str(first) {
            Ok(head) => head,
            Err(e) => {
                problems.push(format!("{}, line 1: {e}", inc.name));
                continue;
            }
        };
        if head.format != FORMAT {
            problems.push(format!(
                "{}: format {:?}, not {FORMAT:?}",
                inc.name, head.format
            ));
        }
        if head.recording_id != header.content_id {
            problems.push(format!(
                "{}: generated for {}, but the recording is {}",
                inc.name, head.recording_id, header.content_id
            ));
        }
        if let Some(problem) = check_id(&head.source, false) {
            problems.push(format!("{}: source name: {problem}", inc.name));
        }
        for (n, line) in lines {
            match serde_json::from_str::<Entry>(line) {
                Ok(entry) => entries.push((
                    Some(head.source.clone()),
                    entry,
                    format!("{}, line {}", inc.name, n + 1),
                )),
                Err(e) => problems.push(format!("{}, line {}: {e}", inc.name, n + 1)),
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
        if let Some(problem) = check_id(&entry.id, source.is_some()) {
            problems.push(format!("{at}: {problem}"));
        }
        for (name, value) in entry.values.iter().flatten() {
            if let Some(problem) = check_value(name, value) {
                problems.push(format!("{at}: {problem}"));
            }
        }
        if !seen.insert(id.clone()) {
            problems.push(format!("{at}: id {id:?} is used twice"));
        }
        if let Some(target) = entry.target.clone() {
            if source.is_some() {
                problems.push(format!("{at}: only hand-written annotations take a ref"));
            }
            if entry.start.is_some() || entry.end.is_some() {
                problems.push(format!(
                    "{at}: a ref takes its span from its target; drop start and end"
                ));
            }
            refs.push((position, id, target, entry, at));
            continue;
        }
        let Some(start) = &entry.start else {
            problems.push(format!("{at}: no start"));
            continue;
        };
        let start = match ticks(start, header.timebase) {
            Ok(t) => t,
            Err(e) => {
                problems.push(format!("{at}: start: {e}"));
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
                problems.push(format!("{at}: end: {e}"));
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
    let by_id: BTreeMap<String, Annotation> = plain
        .iter()
        .map(|(_, _, a)| (a.id.clone(), a.clone()))
        .collect();
    for (position, id, target, entry, at) in refs {
        let Some(t) = by_id.get(&target) else {
            problems.push(format!("{at}: ref {target:?} does not resolve"));
            continue;
        };
        plain.push((
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

    for (_, at, a) in &plain {
        let end = a.end.unwrap_or(a.start);
        if a.start < 0 || end > header.length {
            problems.push(format!(
                "{at}: ticks {} to {end} fall outside the capture, 0 to {}",
                a.start, header.length
            ));
        }
        if end < a.start {
            problems.push(format!("{at}: ends before it starts"));
        }
        for c in &a.channels {
            if !channels.contains_key(c.as_str()) {
                problems.push(format!("{at}: no channel {c:?}"));
            }
        }
        if let Some(m) = &a.measure {
            match channels.get(m.channel.as_str()) {
                None => problems.push(format!("{at}: measure: no channel {:?}", m.channel)),
                Some(false) => problems.push(format!(
                    "{at}: measure: {:?} is analog; measures are taken from edges",
                    m.channel
                )),
                Some(true) => {}
            }
            if !MEASURES.contains(&m.what.as_str()) {
                problems.push(format!(
                    "{at}: measure: {:?} is not one of {MEASURES:?}",
                    m.what
                ));
            }
            if a.end.is_none() {
                problems.push(format!("{at}: measure needs a span, not a point"));
            }
        }
    }
    if problems.is_empty() {
        plain.sort_by_key(|(position, ..)| *position);
        Ok(plain.into_iter().map(|(.., a)| a).collect())
    } else {
        Err(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GS: [u64; 2] = [1_000_000_000, 1];

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
        assert_eq!(ticks(&text("15 us"), [100_000, 1]), Ok(2));
        assert_eq!(ticks(&text("14 us"), [100_000, 1]), Ok(1));
        // A ratio timebase: 3 ticks a microsecond as 3,000,000/1 and as 6,000,000/2 agree.
        assert_eq!(ticks(&text("7 us"), [6_000_000, 2]), Ok(21));
        assert_eq!(ticks(&Time::Ticks(42), GS), Ok(42));
    }

    #[test]
    fn bad_times_are_refused() {
        for bad in ["12", "12 minutes", "ms", "1.2.3 ms", "1e3 ns", "--1 ns"] {
            assert!(ticks(&text(bad), GS).is_err(), "{bad}");
        }
    }
}
