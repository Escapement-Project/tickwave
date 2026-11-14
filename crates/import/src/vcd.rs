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

use ::vcd::{Command, IdCode, Parser, TimescaleUnit, Value, VarType};
use escapement_tickwave_format::model::{
    Channel, ChannelData, Digital, Ratio, Recording, Source, SourceFormat,
};

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
    let header = read_header(&mut parser)?;
    let (quantity, unit) = header.timescale.ok_or(Error::NoTimescale)?;
    let timebase = reduce(unit.divisor(), u64::from(quantity)).ok_or_else(|| Error::Invalid {
        line: parser.line(),
        message: "$timescale of 0".to_owned(),
    })?;
    let Header {
        vars,
        comments,
        date,
        version,
        ..
    } = header;

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
        written: None,
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
    // A last timestamp with no change marks where the dump ends, as sigrok-cli writes one. One with
    // changes is inside the capture, which then runs a tick past it.
    let now = state.now.unwrap_or(start);
    let length = (now - start)
        .checked_add(u64::from(state.written == Some(now)))
        .and_then(|l| i64::try_from(l).ok())
        .ok_or_else(|| Error::Invalid {
            line: parser.line(),
            message: format!("{} ticks and one past them is too long", now - start),
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
                    source: 0,
                    data: ChannelData::Digital(Digital {
                        initial,
                        end: length,
                        count: edges.len() as u64,
                        edges: Box::new(edges.into_iter()),
                        gaps: Vec::new(),
                    }),
                    shaping: Default::default(),
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
    if let Some(date) = date {
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
            derived: Vec::new(),
            sources: vec![Source {
                files: vec![file_name.to_owned()],
                software: version,
                begin_s: Some(start as f64 * timebase.den() as f64 / timebase.num() as f64),
                extra,
                ..Source::new(SourceFormat::Vcd, timebase)
            }],
        },
        skipped,
    })
}

/// `num`/`den` in lowest terms; None when a term is zero.
fn reduce(num: u64, den: u64) -> Option<Ratio> {
    if num == 0 || den == 0 {
        return None;
    }
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

/// A `$var` as the reader takes it.
struct Var {
    var_type: VarType,
    size: u32,
    code: IdCode,
}

/// What the header declares: its variables by full name, in declaration order, and its comments
/// in every scope.
struct Header {
    timescale: Option<(u32, TimescaleUnit)>,
    date: Option<String>,
    version: Option<String>,
    vars: Vec<(String, Var)>,
    comments: Vec<String>,
}

/// The header, read command by command, as `Parser::parse_header` reads it but with no tree of
/// scopes: that one recurses for each scope inside another, so a deep enough nesting overflows
/// the stack, and dropping the tree it made does too.
fn read_header(parser: &mut Parser<impl BufRead>) -> Result<Header, Error> {
    let mut header = Header {
        timescale: None,
        date: None,
        version: None,
        vars: Vec::new(),
        comments: Vec::new(),
    };
    // The scope path, joined with `.`, and where each scope's own name starts in it.
    let mut path = String::new();
    let mut starts = Vec::new();
    loop {
        let command = parser.next().transpose()?;
        let line = parser.line();
        let refused = |message: &str| Error::Invalid {
            line,
            message: message.to_owned(),
        };
        let in_scope = !starts.is_empty();
        match command {
            Some(Command::Enddefinitions) if !in_scope => return Ok(header),
            Some(Command::Comment(c)) => header.comments.push(c),
            Some(Command::Date(d)) if !in_scope => header.date = Some(d),
            Some(Command::Version(v)) if !in_scope => header.version = Some(v),
            Some(Command::Timescale(q, u)) if !in_scope => header.timescale = Some((q, u)),
            Some(Command::ScopeDef(_, id)) => {
                starts.push(path.len());
                if in_scope {
                    path.push('.');
                }
                path.push_str(&id);
            }
            Some(Command::Upscope) => {
                let Some(at) = starts.pop() else {
                    return Err(refused("$upscope with no $scope open"));
                };
                path.truncate(at);
            }
            Some(Command::VarDef(var_type, size, code, reference, index)) => {
                let mut name = path.clone();
                if !name.is_empty() {
                    name.push('.');
                }
                name.push_str(&reference);
                if let Some(index) = index {
                    name.push_str(&index.to_string());
                }
                header.vars.push((
                    name,
                    Var {
                        var_type,
                        size,
                        code,
                    },
                ));
            }
            Some(other) => {
                let place = if in_scope { " in a $scope" } else { "" };
                return Err(refused(&format!("{other:?}{place} before $enddefinitions")));
            }
            None => {
                return Err(refused(if in_scope {
                    "the file ends with a $scope open"
                } else {
                    "the file ends before $enddefinitions"
                }));
            }
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
    /// The last time any value was set, changed or not.
    written: Option<u64>,
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
        if !self.touched.is_empty() {
            self.written = Some(now);
        }
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
