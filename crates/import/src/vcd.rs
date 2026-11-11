//! VCD (value change dump) files, parsed by the `vcd` crate.
//!
//! One tick is one `$timescale` unit. Tick 0 is the first timestamp, or time 0 when values change
//! before any timestamp; the recording ends at the last timestamp. Each 1-bit net or reg becomes a
//! digital channel named by its scope path, joined with `.`. Everything else is skipped and
//! listed with the reason.
//!
//! The model is binary, so a channel whose level is x, z or unset over any span is skipped too:
//! that includes the leading x a simulator dumps before a signal is driven. An x that a 0 or 1
//! replaces at the same time never takes effect, since changes at one time collapse to the net
//! change.
//!
//! The whole file is parsed when it is opened, and each channel's edges are held in memory.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use ::vcd::{Command, IdCode, Parser, ScopeItem, Value, Var, VarType};
use escapement_tickwave_format::model::{Channel, ChannelData, Digital, Ratio, Recording, Source};

pub use crate::sigrok::{Import, Skipped};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("line {line}: {message}")]
    Invalid { line: u64, message: String },
    #[error("no $timescale, so the time resolution is unknown")]
    NoTimescale,
}

/// Reads a VCD file.
pub fn read(path: &Path) -> Result<Import, Error> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    read_from(BufReader::new(File::open(path)?), &name)
}

/// Reads VCD text. `file_name` goes into the recording's source.
pub fn read_from(reader: impl BufRead, file_name: &str) -> Result<Import, Error> {
    let mut parser = Parser::new(reader);
    let header = parser.parse_header()?;
    let (quantity, unit) = header.timescale.ok_or(Error::NoTimescale)?;
    if quantity == 0 {
        return Err(Error::Invalid {
            line: parser.line(),
            message: "$timescale of 0".to_owned(),
        });
    }
    let timebase = reduce(unit.divisor(), u64::from(quantity));

    let mut vars = Vec::new();
    let mut comments = Vec::new();
    collect(&header.items, &mut Vec::new(), &mut vars, &mut comments);

    let mut skipped = Vec::new();
    let mut signals: Vec<Signal> = Vec::new();
    let mut codes: HashMap<IdCode, Code> = HashMap::new();
    // (name, signal) for each digital channel, in declaration order.
    let mut digital = Vec::new();
    for (name, var) in vars {
        if let Some(reason) = unread(&var) {
            skipped.push(Skipped { name, reason });
            codes.entry(var.code).or_insert(Code::Skipped);
            continue;
        }
        let slot = match codes.get(&var.code) {
            Some(&Code::Digital(slot)) => slot,
            _ => {
                signals.push(Signal::default());
                codes.insert(var.code, Code::Digital(signals.len() - 1));
                signals.len() - 1
            }
        };
        digital.push((name, slot));
    }

    let mut state = State {
        signals,
        touched: Vec::new(),
        start: None,
        now: None,
    };
    while let Some(command) = parser.next().transpose()? {
        let line = parser.line();
        let invalid = |message: String| Error::Invalid { line, message };
        match command {
            Command::Timestamp(t) => state.timestamp(t).map_err(invalid)?,
            Command::ChangeScalar(code, value) => match codes.get(&code) {
                Some(&Code::Digital(slot)) => state.change(slot, value),
                Some(Code::Skipped) => {}
                None => return Err(invalid(format!("change to undeclared id `{code}`"))),
            },
            Command::ChangeVector(code, vector) => match codes.get(&code) {
                Some(&Code::Digital(slot)) => {
                    let mut bits = vector.iter();
                    match (bits.next(), bits.next()) {
                        (Some(value), None) => state.change(slot, value),
                        _ => {
                            return Err(invalid(format!(
                                "{}-bit value for 1-bit id `{code}`",
                                vector.len()
                            )));
                        }
                    }
                }
                Some(Code::Skipped) => {}
                None => return Err(invalid(format!("change to undeclared id `{code}`"))),
            },
            Command::ChangeReal(code, _) | Command::ChangeString(code, _) => {
                match codes.get(&code) {
                    Some(Code::Skipped) => {}
                    Some(Code::Digital(_)) => {
                        return Err(invalid(format!("non-binary value for 1-bit id `{code}`")));
                    }
                    None => return Err(invalid(format!("change to undeclared id `{code}`"))),
                }
            }
            Command::Begin(_) | Command::End(_) | Command::Comment(_) => {}
            other => return Err(invalid(format!("{other:?} after $enddefinitions"))),
        }
    }
    state.commit();

    let start = state.start.unwrap_or(0);
    let length = state.now.unwrap_or(start) - start;
    let length = i64::try_from(length).map_err(|_| Error::Invalid {
        line: parser.line(),
        message: format!("{length} ticks is too long"),
    })?;

    let mut channels = Vec::new();
    for (name, slot) in digital {
        let signal = &state.signals[slot];
        match (&signal.unknown, signal.initial) {
            (None, Some(initial)) => {
                let edges: Vec<i64> = signal.edges.iter().map(|&t| (t - start) as i64).collect();
                channels.push(Channel {
                    name: name.clone(),
                    source_name: name,
                    data: ChannelData::Digital(Digital {
                        initial,
                        end: length,
                        edges: Box::new(edges.into_iter()),
                    }),
                });
            }
            (Some(unknown), _) => skipped.push(Skipped {
                name,
                reason: unknown.describe(start),
            }),
            (None, None) => skipped.push(Skipped {
                name,
                reason: "never given a value".to_owned(),
            }),
        }
    }

    let mut extra = BTreeMap::new();
    if let Some(date) = header.date {
        extra.insert("date".to_owned(), date);
    }
    if !comments.is_empty() {
        extra.insert("comment".to_owned(), comments.join("\n"));
    }
    extra.insert("timescale".to_owned(), format!("{quantity} {unit}"));
    if start != 0 {
        extra.insert("start time".to_owned(), start.to_string());
    }

    Ok(Import {
        recording: Recording {
            timebase,
            length,
            channels,
            source: Source {
                format: "vcd".to_owned(),
                files: vec![file_name.to_owned()],
                instrument: None,
                software: header.version,
                begin_s: Some(start as f64 * timebase.den as f64 / timebase.num as f64),
                rate: None,
                extra,
            },
        },
        skipped,
    })
}

fn reduce(num: u64, den: u64) -> Ratio {
    let (mut a, mut b) = (num, den);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    Ratio::new(num / a, den / a)
}

/// Why a variable is not read as a digital channel, or `None` if it is.
fn unread(var: &Var) -> Option<String> {
    match var.var_type {
        VarType::Wire
        | VarType::Reg
        | VarType::Tri
        | VarType::TriAnd
        | VarType::TriOr
        | VarType::TriReg
        | VarType::Tri0
        | VarType::Tri1
        | VarType::WAnd
        | VarType::WOr
        | VarType::Supply0
        | VarType::Supply1 => (var.size != 1).then(|| {
            format!(
                "{}-bit {}: only 1-bit signals are read",
                var.size, var.var_type
            )
        }),
        other => Some(format!("{other}: only 1-bit nets and regs are read")),
    }
}

/// Flattens the scope tree into (full name, variable), in declaration order.
fn collect(
    items: &[ScopeItem],
    path: &mut Vec<String>,
    vars: &mut Vec<(String, Var)>,
    comments: &mut Vec<String>,
) {
    for item in items {
        match item {
            ScopeItem::Scope(scope) => {
                path.push(scope.identifier.clone());
                collect(&scope.items, path, vars, comments);
                path.pop();
            }
            ScopeItem::Var(var) => {
                let mut name = path.join(".");
                if !name.is_empty() {
                    name.push('.');
                }
                name.push_str(&var.reference);
                if let Some(index) = var.index {
                    name.push_str(&index.to_string());
                }
                vars.push((name, var.clone()));
            }
            ScopeItem::Comment(comment) => comments.push(comment.clone()),
            _ => {}
        }
    }
}

enum Code {
    Digital(usize),
    Skipped,
}

/// Why a signal's level is not known everywhere: the first time it was x or z, or had no value.
struct Unknown {
    time: u64,
    value: Option<Value>,
}

impl Unknown {
    fn describe(&self, start: u64) -> String {
        let tick = self.time - start;
        match self.value {
            Some(value) => format!("{value} at tick {tick}; the model holds only 0 and 1"),
            None => format!("no value until tick {tick}"),
        }
    }
}

#[derive(Default)]
struct Signal {
    /// The level after the last committed time.
    level: Option<bool>,
    /// The value set at the current time, not yet committed.
    pending: Option<Value>,
    touched: bool,
    initial: Option<bool>,
    unknown: Option<Unknown>,
    /// Absolute times of level changes.
    edges: Vec<u64>,
}

struct State {
    signals: Vec<Signal>,
    touched: Vec<usize>,
    start: Option<u64>,
    now: Option<u64>,
}

impl State {
    fn timestamp(&mut self, t: u64) -> Result<(), String> {
        match self.now {
            None => {
                self.start = Some(t);
                self.now = Some(t);
            }
            Some(now) if t < now => return Err(format!("time {t} after time {now}")),
            Some(now) if t > now => {
                self.commit();
                self.now = Some(t);
            }
            Some(_) => {}
        }
        Ok(())
    }

    fn change(&mut self, slot: usize, value: Value) {
        if self.now.is_none() {
            self.start = Some(0);
            self.now = Some(0);
        }
        let signal = &mut self.signals[slot];
        signal.pending = Some(value);
        if !signal.touched {
            signal.touched = true;
            self.touched.push(slot);
        }
    }

    /// Applies the net change of every signal set at the current time.
    fn commit(&mut self) {
        let (Some(now), Some(start)) = (self.now, self.start) else {
            return;
        };
        for slot in self.touched.drain(..) {
            let signal = &mut self.signals[slot];
            signal.touched = false;
            let Some(value) = signal.pending.take() else {
                continue;
            };
            if signal.unknown.is_some() {
                continue;
            }
            let level = match value {
                Value::V0 => false,
                Value::V1 => true,
                _ => {
                    signal.unknown = Some(Unknown {
                        time: now,
                        value: Some(value),
                    });
                    signal.edges = Vec::new();
                    continue;
                }
            };
            match signal.level {
                None if now == start => signal.initial = Some(level),
                None => {
                    signal.unknown = Some(Unknown {
                        time: now,
                        value: None,
                    });
                    continue;
                }
                Some(previous) if previous != level => signal.edges.push(now),
                Some(_) => {}
            }
            signal.level = Some(level);
        }
    }
}
