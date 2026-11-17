//! Paint roles as a sidecar declares them (ANNOTATIONS.md, "Roles"): the hand-written file's
//! `[[role]]` tables and a generator's `role` list in its file's first line, held to the schema's
//! bounds, and one name's declarations in several files held to agree.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The most roles one file declares, and the most a sidecar's files declare between them.
pub const ROLES_AT_MOST: usize = 64;
/// The longest role name, in bytes.
pub const NAME_BYTES: usize = 32;
/// The longest label, in bytes.
pub const LABEL_BYTES: usize = 200;
/// The most numbers in a dash.
pub const DASH_NUMBERS: usize = 8;
/// The greatest weight, in CSS pixels or egui points.
pub const WEIGHT_AT_MOST: f64 = 4.0;
/// The greatest dash or gap, in line widths.
pub const DASH_AT_MOST: f64 = 64.0;
/// The least a dash's numbers sum to, in line widths: a pattern repeats at most once a line
/// width, so a renderer cutting a row into dashes cuts a bounded number of pieces.
pub const DASH_PERIOD_AT_LEAST: f64 = 1.0;

/// A role's colour on the page's day face and on its night face, each `#rrggbb` in lower case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Colour {
    pub day: String,
    pub night: String,
}

/// A role as a sidecar declares it, with its defaults: a label left out is the name, a weight
/// left out 0. A colour or a dash left out is None, for a renderer's fallback.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Role {
    /// The value a `paint` gives.
    pub name: String,
    /// The key's and the readout's text.
    pub label: String,
    pub colour: Option<Colour>,
    /// Added to the trace's line weight, in CSS pixels (egui points), 0 to 4.
    pub weight: f64,
    /// (dash, gap) pairs, in the role's line widths, weight included; None for a solid line.
    pub dash: Option<Vec<f64>>,
}

/// Whether `weight` is one a role may add: 0 to `WEIGHT_AT_MOST`. A host's own weight for a role is
/// held to it too, as a page's is.
pub fn weight_fits(weight: f64) -> bool {
    (0.0..=WEIGHT_AT_MOST).contains(&weight)
}

/// Whether `numbers` make a dash: (dash, gap) pairs, at most `DASH_NUMBERS`, each 0 to
/// `DASH_AT_MOST`, together at least `DASH_PERIOD_AT_LEAST`. A host's own dash is held to it too.
pub fn dash_fits(numbers: &[f64]) -> bool {
    !numbers.is_empty()
        && numbers.len().is_multiple_of(2)
        && numbers.len() <= DASH_NUMBERS
        && numbers.iter().all(|x| (0.0..=DASH_AT_MOST).contains(x))
        && numbers.iter().sum::<f64>() >= DASH_PERIOD_AT_LEAST
}

/// Why `name` can't name a role, if it can't: lower-case ASCII letters, digits and `_`, starting
/// with a letter, at most `NAME_BYTES`. No hyphen, so a page's `--tickwave-role-<name>-weight` and
/// a role's colour variable never read as each other.
pub fn check_name(name: &str) -> Option<String> {
    let mut chars = name.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase())
        || !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Some(format!(
            "role {name:?}: a name is lower-case letters, digits and '_', starting with a letter"
        ));
    }
    (name.len() > NAME_BYTES)
        .then(|| format!("role {name:?}: a name is at most {NAME_BYTES} bytes"))
}

/// `#rrggbb`, in lower case; None for anything else.
fn colour(text: &str) -> Option<String> {
    let hex = text.strip_prefix('#')?;
    (hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| text.to_ascii_lowercase())
}

/// A `colour` field: one `#rrggbb` for both faces, or a [day, night] pair of them.
pub(crate) fn colour_field(v: &Value) -> Result<Colour, String> {
    let one = |v: &Value| v.as_str().and_then(colour);
    match v {
        Value::Array(pair) if pair.len() == 2 => match (one(&pair[0]), one(&pair[1])) {
            (Some(day), Some(night)) => Ok(Colour { day, night }),
            _ => Err("colour: each of a pair is \"#rrggbb\"".into()),
        },
        c => match one(c) {
            Some(both) => Ok(Colour {
                day: both.clone(),
                night: both,
            }),
            None => Err("colour is \"#rrggbb\", or a [day, night] pair of them".into()),
        },
    }
}

/// One declaration, as a file's TOML or JSON gives it, held to the schema; the problem names the
/// role, or its place in `file` where it has no name.
fn role(v: &Value, file: &str, place: usize) -> Result<Role, String> {
    let Some(fields) = v.as_object() else {
        return Err(format!("{file}: role {place} is not a table"));
    };
    let name = match fields.get("name") {
        Some(Value::String(n)) => n.clone(),
        Some(_) => return Err(format!("{file}: role {place}: name is not text")),
        None => return Err(format!("{file}: role {place} has no name")),
    };
    let who = |problem: String| format!("{file}: role {name:?}: {problem}");
    if let Some(problem) = check_name(&name) {
        return Err(format!("{file}: {problem}"));
    }
    if let Some(key) = fields
        .keys()
        .find(|k| !["name", "label", "colour", "weight", "dash"].contains(&k.as_str()))
    {
        return Err(who(format!(
            "unknown field {key:?}; a role takes name, label, colour, weight and dash"
        )));
    }
    let label = match fields.get("label") {
        None => name.clone(),
        Some(Value::String(l)) if l.len() <= LABEL_BYTES => l.clone(),
        Some(Value::String(_)) => {
            return Err(who(format!("label is longer than {LABEL_BYTES} bytes")));
        }
        Some(_) => return Err(who("label is not text".into())),
    };
    let colour = match fields.get("colour").map(colour_field) {
        None => None,
        Some(Ok(c)) => Some(c),
        Some(Err(problem)) => return Err(who(problem)),
    };
    let weight = match fields.get("weight") {
        None => 0.0,
        Some(w) => match w.as_f64() {
            Some(w) if weight_fits(w) => w,
            _ => {
                return Err(who(format!(
                    "weight is a number from 0 to {WEIGHT_AT_MOST}"
                )));
            }
        },
    };
    let dash = match fields.get("dash") {
        None => None,
        Some(Value::Array(d)) => {
            let numbers: Option<Vec<f64>> = d.iter().map(Value::as_f64).collect();
            match numbers {
                Some(n) if dash_fits(&n) => Some(n),
                _ => {
                    return Err(who(format!(
                        "dash is (dash, gap) pairs, at most {DASH_NUMBERS} numbers, each from 0 \
                         to {DASH_AT_MOST} line widths, together at least {DASH_PERIOD_AT_LEAST}"
                    )));
                }
            }
        }
        Some(_) => return Err(who("dash is a list of numbers".into())),
    };
    Ok(Role {
        name,
        label,
        colour,
        weight,
        dash,
    })
}

/// The roles `files` declare, each a file's name and its role tables, in sidecar order: the
/// first declaration of each name, in order, and every problem. A file declaring more than
/// `ROLES_AT_MOST`, a name twice in a file, or one name differently in two files is a problem
/// naming the role; so is a declaration the schema refuses, which is left out. Declarations of
/// one name agree where each field does once its defaults are filled in. A file's tables past the
/// bound are not read, and once the files declare more names than it between them the rest are
/// not collected, so a crafted sidecar costs no more than one at the bound.
pub fn declared<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a [Value])>,
) -> (Vec<Role>, Vec<String>) {
    let mut firsts: Vec<Role> = Vec::new();
    // Each name's place in `firsts`, and the file that declared it there, keyed by the tables'
    // own text.
    let mut places: BTreeMap<&'a str, (usize, &'a str)> = BTreeMap::new();
    let mut problems = Vec::new();
    let mut full = false;
    for (file, tables) in files {
        if tables.len() > ROLES_AT_MOST {
            problems.push(format!(
                "{file}: declares {} roles, more than {ROLES_AT_MOST}",
                tables.len()
            ));
        }
        let mut here: BTreeSet<&'a str> = BTreeSet::new();
        for (place, v) in tables.iter().take(ROLES_AT_MOST).enumerate() {
            let role = match role(v, file, place + 1) {
                Ok(r) => r,
                Err(problem) => {
                    problems.push(problem);
                    continue;
                }
            };
            // `role` read it from the table.
            let name = v["name"].as_str().unwrap_or_default();
            if !here.insert(name) {
                problems.push(format!("{file}: role {:?} is declared twice", role.name));
                continue;
            }
            match places.get(name) {
                Some(&(at, first)) if firsts[at] != role => problems.push(format!(
                    "role {:?}: {first} and {file} declare it differently ({})",
                    role.name,
                    differences(&firsts[at], &role)
                )),
                Some(_) => {}
                None if firsts.len() == ROLES_AT_MOST => {
                    if !full {
                        problems.push(format!(
                            "the sidecar's files declare more than {ROLES_AT_MOST} roles"
                        ));
                        full = true;
                    }
                }
                None => {
                    places.insert(name, (firsts.len(), file));
                    firsts.push(role);
                }
            }
        }
    }
    (firsts, problems)
}

/// The names `files` declare, as `declared` reads their tables: a declaration the schema refuses
/// still names its role, so a paint naming it is not undeclared as well.
pub(crate) fn names<'a>(files: impl IntoIterator<Item = &'a [Value]>) -> BTreeSet<&'a str> {
    files
        .into_iter()
        .flat_map(|tables| tables.iter().take(ROLES_AT_MOST))
        .filter_map(|v| v.get("name")?.as_str())
        .collect()
}

/// The fields two declarations of one name differ in.
fn differences(a: &Role, b: &Role) -> String {
    let mut fields = Vec::new();
    if a.label != b.label {
        fields.push("label");
    }
    if a.colour != b.colour {
        fields.push("colour");
    }
    if a.weight != b.weight {
        fields.push("weight");
    }
    if a.dash != b.dash {
        fields.push("dash");
    }
    fields.join(", ")
}
