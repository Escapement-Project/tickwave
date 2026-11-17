//! A recording's stored view (ANNOTATIONS.md, "The stored view"): the hand-written file's
//! `[view]` table, the channels a view of the recording shows and the span it starts at where the
//! page or host gives neither, and the trace's colour.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::roles::{Colour, colour_field};
use crate::header::Header;
use crate::times::{Time, ticks};

/// The `[view]` table, held to the recording: its times in ticks.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct View {
    /// The channels shown, in this order; None for every channel.
    pub channels: Option<Vec<String>>,
    /// The span a view starts at; None for the capture's start.
    pub start: Option<i64>,
    /// None for the capture's end. Exclusive.
    pub end: Option<i64>,
    /// The trace's colour, under a page's own.
    pub colour: Option<Colour>,
}

/// `[view]` as the hand-written file gives it, held to `header`, or every problem with it.
pub(crate) fn view(v: &Value, header: &Header) -> Result<View, Vec<String>> {
    let Some(fields) = v.as_object() else {
        return Err(vec!["view: not a table".into()]);
    };
    let mut problems = Vec::new();
    if let Some(key) = fields
        .keys()
        .find(|k| !["channels", "start", "end", "colour"].contains(&k.as_str()))
    {
        problems.push(format!(
            "view: unknown field {key:?}; a view takes channels, start, end and colour"
        ));
    }
    let channels = match fields.get("channels") {
        None => None,
        Some(Value::Array(list)) => {
            let mut names: Vec<String> = Vec::new();
            for c in list {
                match c.as_str() {
                    Some(n) if names.iter().any(|m| m == n) => {
                        problems.push(format!("view: channel {n:?} is listed twice"));
                    }
                    Some(n) if !header.channels.iter().any(|h| h.name == n) => {
                        problems.push(format!("view: no channel {n:?}"));
                    }
                    Some(n) => names.push(n.to_owned()),
                    None => problems.push("view: a channel is a name".into()),
                }
            }
            if list.is_empty() {
                problems.push("view: channels lists none; leave it out for every channel".into());
            }
            Some(names)
        }
        Some(_) => {
            problems.push("view: channels is a list of names".into());
            None
        }
    };
    let time = |key: &str, problems: &mut Vec<String>| {
        let t = fields.get(key)?;
        match serde_json::from_value::<Time>(t.clone())
            .map_err(|e| e.to_string())
            .and_then(|t| ticks(&t, header.timebase).map_err(|e| e.to_string()))
        {
            Ok(t) => Some(t),
            Err(e) => {
                problems.push(format!("view: {key}: {e}"));
                None
            }
        }
    };
    let start = time("start", &mut problems);
    let end = time("end", &mut problems);
    let (from, to) = (start.unwrap_or(0), end.unwrap_or(header.length));
    if from < 0 || to > header.length || from >= header.length {
        problems.push(format!(
            "view: ticks {from} to {to} fall outside the capture, 0..{}",
            header.length
        ));
    } else if to <= from {
        problems.push("view: ends before it starts".into());
    }
    let colour = match fields.get("colour").map(colour_field) {
        None => None,
        Some(Ok(c)) => Some(c),
        Some(Err(problem)) => {
            problems.push(format!("view: {problem}"));
            None
        }
    };
    if problems.is_empty() {
        Ok(View {
            channels,
            start,
            end,
            colour,
        })
    } else {
        Err(problems)
    }
}
