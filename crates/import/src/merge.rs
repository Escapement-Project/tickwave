//! Merging recordings onto one time axis (plan/merge-2026-11-12.md; FORMAT.md "Times",
//! "Sources", "Moved and trimmed channels"). The merged recording ticks at the least common
//! multiple of its inputs' rates. Input j's tick `t` of channel c lies at merged tick
//! `F_j t + shift_c`, `shift_c = A_j − base + offset_m(c)`: its factor, its alignment, the
//! merged recording's start and the merge's own offset for the channel. The header states all of
//! it, so `expected` rebuilds the merge from the header and its inputs alone.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use escapement_tickwave_format::annotations::{self, Time};
use escapement_tickwave_format::encode::{Bundle, EncodeError, Options, encode_to};
use escapement_tickwave_format::header::{AnalogCoding, Derived, Header, JSON_SAFE, factor};
use escapement_tickwave_format::model::{
    Analog, Channel, ChannelData, Kept, Offset, Ratio, Recording, Shaping, Source,
};
use escapement_tickwave_format::verify::{VerifyError, verify_limited};

use crate::convert::{Pass, Progress, counted};
use crate::shape::{self, AnalogCut, Keep, Map, Span, exact, grid, off_grid, rounded};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("a merge takes at least two recordings; {0} given")]
    TooFew(usize),
    #[error("settings for input {given}, but {inputs} inputs")]
    Settings { given: usize, inputs: usize },
    #[error(transparent)]
    Read(#[from] crate::recording::Error),
    #[error("reading {0}: {1}")]
    Io(String, std::io::Error),
    #[error("the inputs tick {rates}: their least common multiple is past 2^53")]
    Timebase { rates: String },
    #[error("input {input} ticks {rate}: each of its ticks is {factor} merged ticks, past 2^53")]
    Factor {
        input: usize,
        rate: String,
        factor: u128,
    },
    #[error("input {input} ({stem}) has no channel {name:?}; it has {has:?}")]
    NoChannel {
        input: usize,
        stem: String,
        name: String,
        has: Vec<String>,
    },
    #[error("input {input} ({stem}): channel {name:?} is taken twice")]
    TakenTwice {
        input: usize,
        stem: String,
        name: String,
    },
    #[error("input {input} ({stem}): {name:?} is the name of another channel of the merge")]
    Clash {
        input: usize,
        stem: String,
        name: String,
    },
    #[error(
        "input {input} ({stem}): its source {index} ({format}) keeps no channel; take at least one \
         of its channels"
    )]
    SourceLeftOut {
        input: usize,
        stem: String,
        index: usize,
        format: String,
    },
    #[error("{what} for {name:?}: the merge has no channel by that name; it has {has:?}")]
    NoOutput {
        what: &'static str,
        name: String,
        has: Vec<String>,
    },
    #[error("{name:?} is given {what} twice")]
    Twice { what: &'static str, name: String },
    #[error("a unit for {0}: it is digital")]
    UnitOnDigital(String),
    #[error("{name}: {problem}")]
    Time { name: String, problem: String },
    #[error(
        "{name}: {time:?} is {ticks:.3} merged ticks, more than 0.01 of a merged tick off the grid"
    )]
    OffGrid {
        name: String,
        time: String,
        ticks: f64,
    },
    #[error("{name}: {ticks} ticks is past 2^53")]
    Range { name: String, ticks: i128 },
    #[error("{name}: kept {span}: its from must come before its to")]
    Backwards { name: String, span: Span },
    #[error(
        "{name}: kept {span}, which leaves nothing of the ticks its input holds, {from} to {to}"
    )]
    Outside {
        name: String,
        span: Span,
        from: i64,
        to: i64,
    },
    #[error(
        "{name}: kept {span}; its samples keep their ticks only from one a multiple of {q} into \
         its run, and none in that span is"
    )]
    NoGridSample { name: String, span: Span, q: u64 },
    #[error(
        "{name}: its input states a {kind:?} offset, which the merge's own offset can't add to"
    )]
    OffsetKind { name: String, kind: String },
    #[error(
        "{name}: its run before sample {index} ends at merged tick {last}, after the run that \
         starts there at {tick}: the merged rate puts the input's sample before the break later \
         than the break"
    )]
    BreakBeforeSample {
        name: String,
        index: u64,
        last: i64,
        tick: i64,
    },
    #[error("{name}: its period times {factor} is past 2^53")]
    Period { name: String, factor: u64 },
    #[error("the merged recording would run to tick {0}, past 2^53")]
    Length(i128),
    #[error("the recordings given hold {given} sources, and the merged recording {stated}")]
    SourceCount { given: usize, stated: usize },
    #[error(
        "input {input}'s source {index} is {given}, but the merged recording's source {at} is \
         {stated}: the recordings are given in another order than the merge's, or another \
         recording"
    )]
    Order {
        input: usize,
        index: usize,
        at: usize,
        given: String,
        stated: String,
    },
    #[error(
        "the merged recording ticks {stated}, not {lcm}, the least common multiple of the \
         recordings given"
    )]
    NotLcm { stated: String, lcm: String },
    #[error(
        "input {input}: its sources' origins or aligns state different moves, {0:?} and {1:?}",
        .moves.0, .moves.1
    )]
    Disagree {
        input: usize,
        moves: ((i128, i128), (i128, i128)),
    },
    #[error("channel {name}: input {input} has no channel {source_name:?} of its source {index}")]
    NoInputChannel {
        name: String,
        input: usize,
        index: usize,
        source_name: String,
    },
    #[error(
        "channel {name}: input {input} has more than one channel {source_name:?} of its source \
         {index}"
    )]
    Ambiguous {
        name: String,
        input: usize,
        index: usize,
        source_name: String,
    },
    #[error(
        "channel {name}: its kept span {from} to {to} is not a whole number of its input's ticks"
    )]
    NotWhole { name: String, from: i64, to: i64 },
    #[error("channel {name} states no kept span, which a merged recording states on every channel")]
    NoKept { name: String },
    #[error(
        "derived channel {name}: its input states no product of its channels {a} and {b} in {unit:?}"
    )]
    NoDerived {
        name: String,
        a: String,
        b: String,
        unit: String,
    },
    #[error("derived channel {name}: its operands {a} and {b} are moved or cut apart")]
    DerivedApart { name: String, a: String, b: String },
    #[error("encoding: {0}")]
    Encode(#[from] EncodeError),
    #[error("the merged recording does not map back to its inputs: {0}")]
    Verify(#[from] VerifyError),
}

/// A recording to merge, as `recording::read` gives it, with what the model does not hold.
pub struct Input {
    pub recording: Recording,
    /// Its file's name without the extension: it names a clashing channel or generator apart.
    pub stem: String,
    /// The sidecar it carries, file name to text: its header's bundle, or the sidecar beside it.
    pub bundled: Option<BTreeMap<String, String>>,
    /// Each channel's coding, by index, so the merge stores the samples as the input does: None
    /// for a digital channel.
    pub codings: Vec<Option<AnalogCoding>>,
}

impl Input {
    /// The recording `input` holds. The sidecar is its bundle; the caller may give it one found
    /// beside the file instead.
    pub fn read(
        input: Arc<dyn crate::input::Input>,
        stem: impl Into<String>,
    ) -> Result<Self, Error> {
        let (header, recording) = crate::recording::read(input)?;
        Ok(Self {
            recording,
            stem: stem.into(),
            bundled: header.annotations,
            codings: header.channels.iter().map(|c| c.coding()).collect(),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct InputSettings {
    /// A time at the merged rate, positive later: a whole number of merged ticks, or a decimal
    /// with a unit, within 0.01 of a merged tick.
    pub align: Option<String>,
    /// The channels to take, by the input's names, in order, each renamed where a name is given.
    /// None given takes every channel under its own name.
    pub channels: Vec<(String, Option<String>)>,
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// By input, in order; an input past the end takes the defaults.
    pub inputs: Vec<InputSettings>,
    /// Output channels to move, by name, each by a time at the merged rate.
    pub offsets: Vec<(String, String)>,
    /// Output channels to keep part of, by name, each end in the channel's own input time.
    pub keeps: Vec<Keep>,
    /// Analog output channels' units, by name.
    pub units: Vec<(String, String)>,
    pub title: Option<String>,
    pub meta: BTreeMap<String, String>,
    pub level: i32,
    pub block_kib: u32,
    /// The merged file's name without its extension, which names the generator files it bundles.
    pub stem: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            inputs: Vec::new(),
            offsets: Vec::new(),
            keeps: Vec::new(),
            units: Vec::new(),
            title: None,
            meta: BTreeMap::new(),
            level: 19,
            block_kib: 1024,
            stem: "merged".into(),
        }
    }
}

/// Something the merge did that its settings didn't say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// A derived channel not carried: an operand is not taken, or its operands are moved or cut
    /// apart, which would leave them no longer sharing their samples.
    DerivedLeftOut {
        input: usize,
        stem: String,
        name: String,
        why: String,
    },
    /// A generator renamed, since an earlier input's took its name.
    GeneratorRenamed {
        input: usize,
        from: String,
        to: String,
    },
    /// Hand-written annotations not carried: their times are the input's.
    HandWrittenLeftOut {
        input: usize,
        stem: String,
        count: usize,
    },
    /// A sidecar file that doesn't read, left out.
    Unread {
        input: usize,
        stem: String,
        file: String,
    },
    /// Generated annotations left out: on no channel the merge takes, before the merged
    /// recording's start or past its end, or not reading.
    AnnotationsLeftOut {
        input: usize,
        stem: String,
        count: usize,
        why: &'static str,
    },
}

impl Note {
    /// Whether it loses something a reader may miss.
    pub fn warns(&self) -> bool {
        !matches!(self, Note::GeneratorRenamed { .. })
    }
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Note::DerivedLeftOut {
                input,
                stem,
                name,
                why,
            } => write!(
                f,
                "input {input} ({stem}): derived channel {name} left out: {why}"
            ),
            Note::GeneratorRenamed { input, from, to } => write!(
                f,
                "input {input}: generator {from} renamed {to}, since an earlier input's has its name"
            ),
            Note::HandWrittenLeftOut { input, stem, count } => write!(
                f,
                "input {input} ({stem}): {count} hand-written annotation{} left out, since their \
                 times are the input's",
                if *count == 1 { "" } else { "s" }
            ),
            Note::Unread { input, stem, file } => write!(
                f,
                "input {input} ({stem}): its sidecar's {file} does not read, and is left out"
            ),
            Note::AnnotationsLeftOut {
                input,
                stem,
                count,
                why,
            } => write!(
                f,
                "input {input} ({stem}): {count} generated annotation{} left out: {why}",
                if *count == 1 { "" } else { "s" }
            ),
        }
    }
}

pub struct Merged {
    pub header: Header,
    pub notes: Vec<Note>,
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn reduced(r: Ratio) -> (u128, u128) {
    let (n, d) = (u128::from(r.num()), u128::from(r.den()));
    let g = gcd(n, d);
    (n / g, d / g)
}

fn rate(r: Ratio) -> String {
    format!("{}/{}", r.num(), r.den())
}

/// The least common multiple of `rates`: their reduced numerators' lcm over their reduced
/// denominators' gcd.
pub fn merged_timebase(rates: &[Ratio]) -> Result<Ratio, Error> {
    let too_big = || Error::Timebase {
        rates: rates
            .iter()
            .map(|&r| rate(r))
            .collect::<Vec<_>>()
            .join(", "),
    };
    let (mut num, mut den) = reduced(rates[0]);
    for &r in &rates[1..] {
        let (n, d) = reduced(r);
        num = num.checked_mul(n / gcd(num, n)).ok_or_else(too_big)?;
        den = gcd(den, d);
    }
    if num >= u128::from(JSON_SAFE) {
        return Err(too_big());
    }
    Ok(Ratio::new(num as u64, den as u64).expect("positive terms"))
}

/// A time at `timebase` in whole ticks, within 0.01 of one.
fn on_grid(name: &str, text: &str, timebase: Ratio) -> Result<i64, Error> {
    let time = |problem| Error::Time {
        name: name.to_owned(),
        problem,
    };
    let e = exact(text, timebase).map_err(time)?;
    let ticks = rounded(text, &e).map_err(time)?;
    if off_grid(&e, ticks) {
        let t = e.numerator as f64 / e.denominator as f64;
        return Err(Error::OffGrid {
            name: name.to_owned(),
            time: text.to_owned(),
            ticks: if e.negative { -t } else { t },
        });
    }
    if ticks.unsigned_abs() >= JSON_SAFE {
        return Err(Error::Range {
            name: name.to_owned(),
            ticks: i128::from(ticks),
        });
    }
    Ok(ticks)
}

/// Where input j's ticks go: tick `t` at `factor × t + moved`, `moved = align − base`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InputMap {
    factor: u64,
    align: i64,
    moved: i64,
}

/// One merged channel: where its data comes from, how it is cut and moved, and what the header
/// states of it.
#[derive(Clone, Debug)]
struct ChannelMap {
    input: usize,
    channel: usize,
    name: String,
    source: u32,
    /// The input-tick cut `from..to`.
    from: i64,
    to: i64,
    /// An analog channel's samples skipped, then kept.
    samples: Option<(u64, u64)>,
    shift: i64,
    shaping: Shaping,
    unit: Option<String>,
}

struct Layout {
    timebase: Ratio,
    length: i64,
    inputs: Vec<InputMap>,
    channels: Vec<ChannelMap>,
    derived: Vec<Derived>,
}

/// An input channel's own span: its `kept`, or the whole of its input.
fn own_span(c: &Channel, length: i64) -> (i64, i64) {
    c.shaping.kept.map_or((0, length), |k| (k.from, k.to))
}

/// `ticks` as an i64 within 2^53 either side of 0, as FORMAT.md holds every tick.
fn within(name: impl FnOnce() -> String, ticks: i128) -> Result<i64, Error> {
    if ticks.unsigned_abs() < u128::from(JSON_SAFE) {
        Ok(ticks as i64)
    } else {
        Err(Error::Range {
            name: name(),
            ticks,
        })
    }
}

/// What the header states of channel `name` moved by `shift`, cut at `from..to` of its input's
/// ticks, with its input's offset scaled by `factor` and the merge's own `offset_m` added. Every
/// tick the channel's data maps to lies in its kept span, so the span's bounds bound them.
fn statements(
    name: &str,
    input_channel: &Channel,
    factor: u64,
    shift: i64,
    from: i64,
    to: i64,
    offset_m: i64,
) -> Result<Shaping, Error> {
    let input_offset = input_channel.shaping.offset.as_ref();
    let ticks = within(
        || format!("{name}'s offset"),
        i128::from(factor) * i128::from(input_offset.map_or(0, |o| o.ticks)) + i128::from(offset_m),
    )?;
    let at = |t: i64| i128::from(factor) * i128::from(t) + i128::from(shift);
    let end = at(to);
    if end >= i128::from(JSON_SAFE) {
        return Err(Error::Length(end));
    }
    Ok(Shaping {
        offset: (ticks != 0).then(|| Offset {
            ticks,
            kind: input_offset.map_or_else(|| Offset::DELAY.to_owned(), |o| o.kind.clone()),
        }),
        kept: Some(Kept {
            from: at(from) as i64,
            to: end as i64,
        }),
    })
}

/// Checks a merged analog channel against the reader's rules the merged rate can break: its
/// period within 2^53, and each run's last sample at or before the next run's tick.
fn check_analog(
    name: &str,
    a: &Analog,
    factor: u64,
    shift: i64,
    (skip, keep): (u64, u64),
) -> Result<(), Error> {
    let period = shape::scaled_period(a.period, factor).ok_or_else(|| Error::Period {
        name: name.to_owned(),
        factor,
    })?;
    let map = Map { factor, shift };
    let tick_in =
        |i| escapement_tickwave_format::model::run_sample_tick(a.first, a.period, &a.breaks, i);
    let end = skip + keep;
    // The runs kept, from the cut: (first index, merged tick).
    let mut runs = vec![(skip, map.tick(tick_in(skip)))];
    runs.extend(
        a.breaks
            .iter()
            .filter(|b| b.index > skip && b.index < end)
            .map(|b| (b.index, map.tick(b.tick))),
    );
    for pair in runs.windows(2) {
        let ((start, tick), (next, next_tick)) = (pair[0], pair[1]);
        let n = next - start;
        let last = escapement_tickwave_format::model::sample_tick(tick, period, n - 1);
        if last > next_tick {
            return Err(Error::BreakBeforeSample {
                name: name.to_owned(),
                index: next - skip,
                last,
                tick: next_tick,
            });
        }
    }
    Ok(())
}

/// `name` made unique against `taken`: `<stem>/<name>`, then `<stem>-2/<name>`, and so on, as
/// web/src/merged-source.ts names them.
fn unique(name: &str, stem: &str, taken: &BTreeSet<String>) -> String {
    let mut out = name.to_owned();
    let mut n = 1;
    while taken.contains(&out) {
        out = if n == 1 {
            format!("{stem}/{name}")
        } else {
            format!("{stem}-{n}/{name}")
        };
        n += 1;
    }
    out
}

/// A generator's name made unique against `taken`: `<stem>-<name>`, then `<stem>-2-<name>`,
/// whitespace in the stem written `_` (ANNOTATIONS.md, "Merging").
fn unique_generator(name: &str, stem: &str, taken: &BTreeSet<String>) -> String {
    let stem: String = stem
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    let mut out = name.to_owned();
    let mut n = 1;
    while taken.contains(&out) {
        out = if n == 1 {
            format!("{stem}-{name}")
        } else {
            format!("{stem}-{n}-{name}")
        };
        n += 1;
    }
    out
}

/// The merge's plan from its settings: the layout, and what it notes.
fn plan(inputs: &[Input], settings: &Settings) -> Result<(Layout, Vec<Note>), Error> {
    if inputs.len() < 2 {
        return Err(Error::TooFew(inputs.len()));
    }
    if settings.inputs.len() > inputs.len() {
        return Err(Error::Settings {
            given: settings.inputs.len(),
            inputs: inputs.len(),
        });
    }
    let defaults = InputSettings::default();
    let input_settings = |j: usize| settings.inputs.get(j).unwrap_or(&defaults);
    let timebase = merged_timebase(
        &inputs
            .iter()
            .map(|i| i.recording.timebase)
            .collect::<Vec<_>>(),
    )?;
    let mut notes = Vec::new();

    // Each output channel: (input, input channel, name), named in order, an input's derived
    // channels after its stored ones.
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut chosen: Vec<(usize, usize, String)> = Vec::new();
    let mut derived = Vec::new();
    let mut factors = Vec::with_capacity(inputs.len());
    let mut aligns = Vec::with_capacity(inputs.len());
    for (j, input) in inputs.iter().enumerate() {
        let number = j + 1;
        let r = &input.recording;
        let f = factor(timebase, r.timebase).expect("the lcm is a multiple of every rate");
        if f >= u128::from(JSON_SAFE) {
            return Err(Error::Factor {
                input: number,
                rate: rate(r.timebase),
                factor: f,
            });
        }
        factors.push(f as u64);
        let s = input_settings(j);
        aligns.push(match &s.align {
            Some(t) => on_grid(&format!("input {number}'s alignment"), t, timebase)?,
            None => 0,
        });
        let wanted: Vec<(usize, Option<&str>)> = if s.channels.is_empty() {
            (0..r.channels.len()).map(|c| (c, None)).collect()
        } else {
            let mut seen = BTreeSet::new();
            let mut wanted = Vec::with_capacity(s.channels.len());
            for (name, new) in &s.channels {
                let c = r
                    .channels
                    .iter()
                    .position(|c| c.name == *name)
                    .ok_or_else(|| Error::NoChannel {
                        input: number,
                        stem: input.stem.clone(),
                        name: name.clone(),
                        has: r.channels.iter().map(|c| c.name.clone()).collect(),
                    })?;
                if !seen.insert(c) {
                    return Err(Error::TakenTwice {
                        input: number,
                        stem: input.stem.clone(),
                        name: name.clone(),
                    });
                }
                wanted.push((c, new.as_deref()));
            }
            wanted
        };
        let mut names_in: BTreeMap<&str, String> = BTreeMap::new();
        for (c, new) in wanted {
            let name = match new {
                Some(new) => {
                    if taken.contains(new) {
                        return Err(Error::Clash {
                            input: number,
                            stem: input.stem.clone(),
                            name: new.to_owned(),
                        });
                    }
                    new.to_owned()
                }
                None => unique(&r.channels[c].name, &input.stem, &taken),
            };
            taken.insert(name.clone());
            names_in.insert(&r.channels[c].name, name.clone());
            chosen.push((j, c, name));
        }
        for (s, source) in r.sources.iter().enumerate() {
            if !chosen
                .iter()
                .any(|&(i, c, _)| i == j && r.channels[c].source as usize == s)
            {
                return Err(Error::SourceLeftOut {
                    input: number,
                    stem: input.stem.clone(),
                    index: s,
                    format: source.format.to_string(),
                });
            }
        }
        for d in &r.derived {
            match (
                names_in.get(d.product[0].as_str()),
                names_in.get(d.product[1].as_str()),
            ) {
                (Some(a), Some(b)) => {
                    let name = unique(&d.name, &input.stem, &taken);
                    taken.insert(name.clone());
                    derived.push((
                        j,
                        Derived {
                            name,
                            unit: d.unit.clone(),
                            product: [a.clone(), b.clone()],
                            significant: d.significant,
                        },
                        d.clone(),
                    ));
                }
                (a, _) => notes.push(Note::DerivedLeftOut {
                    input: number,
                    stem: input.stem.clone(),
                    name: d.name.clone(),
                    why: format!(
                        "its operand {} is not taken",
                        if a.is_none() {
                            &d.product[0]
                        } else {
                            &d.product[1]
                        }
                    ),
                }),
            }
        }
    }
    let has = || chosen.iter().map(|(_, _, n)| n.clone()).collect::<Vec<_>>();
    let find = |what: &'static str, name: &str| {
        chosen
            .iter()
            .position(|(_, _, n)| n == name)
            .ok_or_else(|| Error::NoOutput {
                what,
                name: name.to_owned(),
                has: has(),
            })
    };
    let mut offsets = vec![None; chosen.len()];
    for (name, time) in &settings.offsets {
        let c = find("an offset", name)?;
        if offsets[c].is_some() {
            return Err(Error::Twice {
                what: "an offset",
                name: name.clone(),
            });
        }
        offsets[c] = Some(on_grid(name, time, timebase)?);
    }
    let mut keeps = vec![None; chosen.len()];
    for (name, from, to) in &settings.keeps {
        let c = find("a keep", name)?;
        if keeps[c].is_some() {
            return Err(Error::Twice {
                what: "a keep",
                name: name.clone(),
            });
        }
        let tb = inputs[chosen[c].0].recording.timebase;
        let tick = |time: &Option<String>| {
            time.as_deref()
                .map(|t| exact(t, tb).and_then(|e| rounded(t, &e)))
                .transpose()
                .map_err(|problem| Error::Time {
                    name: name.clone(),
                    problem,
                })
        };
        keeps[c] = Some(Span {
            from: tick(from)?,
            to: tick(to)?,
        });
    }
    let mut units = vec![None; chosen.len()];
    for (name, unit) in &settings.units {
        let c = find("a unit", name)?;
        let (j, ic, _) = &chosen[c];
        if !matches!(
            inputs[*j].recording.channels[*ic].data,
            ChannelData::Analog(_)
        ) {
            return Err(Error::UnitOnDigital(name.clone()));
        }
        units[c] = Some(unit.clone());
    }

    // Each channel's cut, in its input's ticks, and where it starts on the merged axis.
    struct Cut {
        from: i64,
        to: i64,
        samples: Option<(u64, u64)>,
        offset_m: i64,
    }
    let mut cuts = Vec::with_capacity(chosen.len());
    for (c, (j, ic, name)) in chosen.iter().enumerate() {
        let r = &inputs[*j].recording;
        let channel = &r.channels[*ic];
        let (own_from, own_to) = own_span(channel, r.length);
        let span = keeps[c].unwrap_or(Span {
            from: None,
            to: None,
        });
        if let Span {
            from: Some(f),
            to: Some(t),
        } = span
            && f >= t
        {
            return Err(Error::Backwards {
                name: name.clone(),
                span,
            });
        }
        let from = span.from.map_or(own_from, |f| f.max(own_from));
        let to = span.to.map_or(own_to, |t| t.min(own_to));
        if from >= to {
            return Err(Error::Outside {
                name: name.clone(),
                span,
                from: own_from,
                to: own_to,
            });
        }
        let offset_m = offsets[c].unwrap_or(0);
        if offset_m != 0
            && let Some(o) = &channel.shaping.offset
            && o.kind != Offset::DELAY
        {
            return Err(Error::OffsetKind {
                name: name.clone(),
                kind: o.kind.clone(),
            });
        }
        let (from, samples) = match &channel.data {
            ChannelData::Digital(_) => (from, None),
            ChannelData::Analog(a) => {
                let cut = AnalogCut::new(a, (from > 0).then_some(from), to).ok_or_else(|| {
                    Error::NoGridSample {
                        name: name.clone(),
                        span,
                        q: grid(a.period),
                    }
                })?;
                (cut.from.unwrap_or(from), Some((cut.skip, cut.keep)))
            }
        };
        cuts.push(Cut {
            from,
            to,
            samples,
            offset_m,
        });
    }
    // The recording starts at the earliest channel's start, after its move and its alignment.
    let base = chosen
        .iter()
        .zip(&cuts)
        .map(|((j, _, _), cut)| {
            i128::from(factors[*j]) * i128::from(cut.from)
                + i128::from(aligns[*j])
                + i128::from(cut.offset_m)
        })
        .min()
        .expect("at least one channel");
    let mut input_maps = Vec::with_capacity(inputs.len());
    for (j, (&factor, &align)) in factors.iter().zip(&aligns).enumerate() {
        let number = j + 1;
        let moved = within(
            || format!("input {number}'s move"),
            i128::from(align) - base,
        )?;
        let map = InputMap {
            factor,
            align,
            moved,
        };
        for (s, source) in inputs[j].recording.sources.iter().enumerate() {
            let (origin, align) = source_ticks(source, map);
            within(|| format!("input {number}'s source {s}'s origin"), origin)?;
            within(|| format!("input {number}'s source {s}'s align"), align)?;
        }
        input_maps.push(map);
    }
    let source_offsets = source_offsets(inputs.iter().map(|i| &i.recording));
    let mut channels = Vec::with_capacity(chosen.len());
    for (c, ((j, ic, name), cut)) in chosen.into_iter().zip(cuts).enumerate() {
        let map = input_maps[j];
        let input_channel = &inputs[j].recording.channels[ic];
        channels.push(finish_channel(
            ChannelMap {
                input: j,
                channel: ic,
                name,
                source: source_offsets[j] + input_channel.source,
                from: cut.from,
                to: cut.to,
                samples: cut.samples,
                shift: map.moved + cut.offset_m,
                shaping: Shaping::default(),
                unit: units[c].clone(),
            },
            input_channel,
            map.factor,
            cut.offset_m,
        )?);
    }
    // A derived channel's operands must still share their samples: cut and moved alike.
    let mut kept_derived = Vec::new();
    for (j, d, input_derived) in derived {
        let operand = |name: &str| {
            channels
                .iter()
                .find(|c| c.name == name)
                .expect("a taken operand")
        };
        let (a, b) = (operand(&d.product[0]), operand(&d.product[1]));
        if (a.from, a.to, a.samples, a.shift) == (b.from, b.to, b.samples, b.shift) {
            kept_derived.push(d);
        } else {
            notes.push(Note::DerivedLeftOut {
                input: j + 1,
                stem: inputs[j].stem.clone(),
                name: input_derived.name,
                why: format!(
                    "its operands {} and {} are moved or cut apart",
                    input_derived.product[0], input_derived.product[1]
                ),
            });
        }
    }
    let length = layout_length(&channels);
    Ok((
        Layout {
            timebase,
            length,
            inputs: input_maps,
            channels,
            derived: kept_derived,
        },
        notes,
    ))
}

/// A source's origin and align once its input is mapped by `map`.
fn source_ticks(s: &Source, map: InputMap) -> (i128, i128) {
    let f = i128::from(map.factor);
    (
        f * i128::from(s.origin) + i128::from(map.moved),
        f * i128::from(s.align.unwrap_or(0)) + i128::from(map.align),
    )
}

/// Each input's first source's index in the merged recording's `sources`.
fn source_offsets<'a>(inputs: impl Iterator<Item = &'a Recording>) -> Vec<u32> {
    let mut at = 0;
    inputs
        .map(|r| {
            let first = at;
            at += r.sources.len() as u32;
            first
        })
        .collect()
}

/// A channel's statements, checked against what the merged rate can break.
fn finish_channel(
    mut c: ChannelMap,
    input_channel: &Channel,
    factor: u64,
    offset_m: i64,
) -> Result<ChannelMap, Error> {
    c.shaping = statements(
        &c.name,
        input_channel,
        factor,
        c.shift,
        c.from,
        c.to,
        offset_m,
    )?;
    if let (ChannelData::Analog(a), Some(samples)) = (&input_channel.data, c.samples) {
        check_analog(&c.name, a, factor, c.shift, samples)?;
    }
    Ok(c)
}

/// The merged recording's length: the latest kept span's end, which `statements` holds within
/// 2^53.
fn layout_length(channels: &[ChannelMap]) -> i64 {
    channels
        .iter()
        .filter_map(|c| c.shaping.kept.map(|k| k.to))
        .max()
        .unwrap_or(1)
}

/// The merged recording `layout` describes, from its inputs.
fn apply(layout: &Layout, inputs: Vec<Recording>) -> Recording {
    let mut sources = Vec::new();
    let mut channels_in: Vec<Vec<Option<Channel>>> = Vec::with_capacity(inputs.len());
    for (r, &map) in inputs.into_iter().zip(&layout.inputs) {
        sources.extend(r.sources.into_iter().map(|s| {
            let (origin, align) = source_ticks(&s, map);
            Source {
                origin: origin as i64,
                align: Some(align as i64).filter(|&a| a != 0),
                ..s
            }
        }));
        channels_in.push(r.channels.into_iter().map(Some).collect());
    }
    let channels = layout
        .channels
        .iter()
        .map(|c| {
            let channel = channels_in[c.input][c.channel]
                .take()
                .expect("each input channel is taken once");
            let map = Map {
                factor: layout.inputs[c.input].factor,
                shift: c.shift,
            };
            let data = match channel.data {
                ChannelData::Digital(d) => {
                    ChannelData::Digital(shape::digital(d, c.from, c.to, map))
                }
                ChannelData::Analog(a) => {
                    let (skip, keep) = c.samples.expect("an analog channel's cut");
                    let mut a = shape::analog(a, skip, keep, map);
                    if let Some(unit) = &c.unit {
                        a.unit = unit.clone();
                    }
                    ChannelData::Analog(a)
                }
            };
            Channel {
                name: c.name.clone(),
                source_name: channel.source_name,
                source: c.source,
                data,
                shaping: c.shaping.clone(),
            }
        })
        .collect();
    Recording {
        timebase: layout.timebase,
        length: layout.length,
        channels,
        derived: layout.derived.clone(),
        sources,
    }
}

/// An annotation's time at `timebase`, in ticks.
fn time_ticks(value: &serde_json::Value, timebase: Ratio) -> Option<i64> {
    let time: Time = serde_json::from_value(value.clone()).ok()?;
    annotations::ticks(&time, timebase).ok()
}

/// Each input's generated annotations, carried: times mapped, generators renamed apart, channels
/// renamed and dropped. Hand-written annotations are counted, not carried.
#[allow(clippy::type_complexity)]
fn carried(
    inputs: &[Input],
    layout: &Layout,
    notes: &mut Vec<Note>,
) -> Vec<(String, serde_json::Map<String, serde_json::Value>)> {
    let mut out = Vec::new();
    let mut generators: BTreeSet<String> = BTreeSet::new();
    for (j, input) in inputs.iter().enumerate() {
        let number = j + 1;
        let Some(files) = &input.bundled else {
            continue;
        };
        let map = layout.inputs[j];
        let names: BTreeMap<&str, &str> = layout
            .channels
            .iter()
            .filter(|c| c.input == j)
            .map(|c| {
                (
                    input.recording.channels[c.channel].name.as_str(),
                    c.name.as_str(),
                )
            })
            .collect();
        let all: Vec<serde_json::Value> = layout
            .channels
            .iter()
            .filter(|c| c.input == j)
            .map(|c| c.name.clone().into())
            .collect();
        let tb = input.recording.timebase;
        let (mut no_channel, mut before, mut after, mut unread) = (0, 0, 0, 0);
        for (file, text) in files {
            let Some(source) = annotations::generator_source(text) else {
                match annotations::hand_written_count(text) {
                    Some(0) => {}
                    Some(count) => notes.push(Note::HandWrittenLeftOut {
                        input: number,
                        stem: input.stem.clone(),
                        count,
                    }),
                    None => notes.push(Note::Unread {
                        input: number,
                        stem: input.stem.clone(),
                        file: file.clone(),
                    }),
                }
                continue;
            };
            let name = unique_generator(&source, &input.stem, &generators);
            generators.insert(name.clone());
            if name != source {
                notes.push(Note::GeneratorRenamed {
                    input: number,
                    from: source.clone(),
                    to: name.clone(),
                });
            }
            for line in text.lines().filter(|l| !l.trim().is_empty()).skip(1) {
                let Ok(serde_json::Value::Object(mut a)) = serde_json::from_str(line) else {
                    unread += 1;
                    continue;
                };
                // In i128: a time far past the merged recording's end is left out below, not
                // wrapped round into it.
                let mapped = |key: &str, a: &serde_json::Map<String, serde_json::Value>| {
                    a.get(key).map(|v| {
                        time_ticks(v, tb)
                            .map(|t| i128::from(map.factor) * i128::from(t) + i128::from(map.moved))
                    })
                };
                let (Some(Some(mut start)), end) = (mapped("start", &a), mapped("end", &a)) else {
                    unread += 1;
                    continue;
                };
                let end = match end {
                    None => None,
                    Some(Some(e)) => Some(e),
                    Some(None) => {
                        unread += 1;
                        continue;
                    }
                };
                // Before the merged recording's start: a span that reaches into it starts at 0,
                // as one that runs past its end ends there.
                if start < 0 {
                    match end {
                        Some(e) if e > 0 => start = 0,
                        _ => {
                            before += 1;
                            continue;
                        }
                    }
                }
                if start >= i128::from(layout.length) {
                    after += 1;
                    continue;
                }
                a.insert("start".into(), (start as i64).into());
                if let Some(e) = end {
                    let e = e.min(i128::from(layout.length));
                    a.insert("end".into(), (e as i64).into());
                }
                match a.get("channels").and_then(|c| c.as_array()) {
                    Some(list) if !list.is_empty() => {
                        let kept: Vec<serde_json::Value> = list
                            .iter()
                            .filter_map(|n| n.as_str().and_then(|n| names.get(n)))
                            .map(|&n| n.into())
                            .collect();
                        if kept.is_empty() {
                            no_channel += 1;
                            continue;
                        }
                        a.insert("channels".into(), kept.into());
                    }
                    _ => {
                        a.insert("channels".into(), all.clone().into());
                    }
                }
                if let Some(serde_json::Value::Object(m)) = a.get_mut("measure") {
                    match m
                        .get("channel")
                        .and_then(|c| c.as_str())
                        .and_then(|c| names.get(c))
                    {
                        Some(&n) => {
                            m.insert("channel".into(), n.into());
                        }
                        None => {
                            a.remove("measure");
                        }
                    }
                }
                out.push((name.clone(), a));
            }
        }
        for (count, why) in [
            (no_channel, "none of their channels is taken"),
            (before, "they lie before the merged recording's start"),
            (after, "they lie past the merged recording's end"),
            (unread, "they do not read"),
        ] {
            if count > 0 {
                notes.push(Note::AnnotationsLeftOut {
                    input: number,
                    stem: input.stem.clone(),
                    count,
                    why,
                });
            }
        }
    }
    out
}

/// Merges what `open` gives into `out`, from its start, and checks what it wrote by rebuilding
/// it from its header and its inputs, opened again. `open` is called once for each pass.
pub fn merge<F: Read + Write + Seek>(
    open: &dyn Fn() -> Result<Vec<Input>, Error>,
    settings: &Settings,
    max: usize,
    progress: &dyn Progress,
    out: &mut F,
) -> Result<Merged, Error> {
    let inputs = open()?;
    let (layout, mut notes) = plan(&inputs, settings)?;
    let annotations = carried(&inputs, &layout, &mut notes);
    let analog = layout
        .channels
        .iter()
        .map(|c| inputs[c.input].codings.get(c.channel).copied().flatten())
        .collect();
    let options = Options {
        level: settings.level,
        block_bytes: settings.block_kib as usize * 1024,
        title: settings.title.clone(),
        meta: settings.meta.clone(),
        converter: Some(format!(
            "tickwave {} merge (zstd {}, blocks of {} KiB)",
            env!("CARGO_PKG_VERSION"),
            settings.level,
            settings.block_kib
        )),
        analog,
        bundle: (!annotations.is_empty()).then(|| Bundle::Generated {
            stem: settings.stem.clone(),
        }),
        annotations,
        ..Options::default()
    };
    let recording = apply(&layout, inputs.into_iter().map(|i| i.recording).collect());
    let io = |e| Error::Encode(EncodeError::Io(e));
    out.seek(SeekFrom::Start(0)).map_err(io)?;
    let finished = encode_to(
        counted(recording, progress, Pass::Encode),
        &options,
        std::io::BufWriter::with_capacity(1 << 20, &mut *out),
    )?;
    progress.end(Pass::Encode);
    let against = open()?.into_iter().map(|i| i.recording).collect();
    let expected = expected(&finished.header, against)?;
    out.seek(SeekFrom::Start(0)).map_err(io)?;
    verify_limited(
        std::io::BufReader::with_capacity(1 << 20, &mut *out),
        counted(expected, progress, Pass::Verify),
        max,
    )?;
    progress.end(Pass::Verify);
    Ok(Merged {
        header: finished.header,
        notes,
    })
}

/// A source as the order check names it.
fn describe(s: &Source) -> String {
    format!(
        "{} [{}]{}",
        s.format,
        s.files.join(", "),
        s.begin_s
            .map_or(String::new(), |b| format!(", its tick 0 at {b} s"))
    )
}

/// The recording `header` describes, rebuilt from the recordings it merged from the header's
/// statements alone: each source's origin gives its input's move, its align the alignment, and
/// each channel's offset and kept span its own. Each of `against` covers as many of the header's
/// sources as it has, in order. A unit is a claim, as a name is: it comes from the header.
pub fn expected(header: &Header, against: Vec<Recording>) -> Result<Recording, Error> {
    let given: usize = against.iter().map(|r| r.sources.len()).sum();
    if given != header.sources.len() {
        return Err(Error::SourceCount {
            given,
            stated: header.sources.len(),
        });
    }
    let offsets = source_offsets(against.iter());
    // Each recording's sources are the header's run for it: a swapped order reads as one.
    for (j, (r, &at)) in against.iter().zip(&offsets).enumerate() {
        for (s, source) in r.sources.iter().enumerate() {
            let stated = &header.sources[at as usize + s];
            if (&source.format, &source.files, source.begin_s)
                != (&stated.format, &stated.files, stated.begin_s)
            {
                let stated = Source {
                    format: stated.format.clone(),
                    files: stated.files.clone(),
                    begin_s: stated.begin_s,
                    ..Source::new(stated.format.clone(), stated.timebase)
                };
                return Err(Error::Order {
                    input: j + 1,
                    index: s,
                    at: at as usize + s,
                    given: describe(source),
                    stated: describe(&stated),
                });
            }
        }
    }
    let lcm = merged_timebase(&against.iter().map(|r| r.timebase).collect::<Vec<_>>())?;
    if reduced(lcm) != reduced(header.timebase) {
        return Err(Error::NotLcm {
            stated: rate(header.timebase),
            lcm: rate(lcm),
        });
    }
    let timebase = header.timebase;
    let mut inputs = Vec::with_capacity(against.len());
    for (j, (r, &at)) in against.iter().zip(&offsets).enumerate() {
        let number = j + 1;
        let f = factor(timebase, r.timebase).expect("the lcm is a multiple of every rate");
        if f >= u128::from(JSON_SAFE) {
            return Err(Error::Factor {
                input: number,
                rate: rate(r.timebase),
                factor: f,
            });
        }
        let mut moves = r.sources.iter().enumerate().map(|(s, source)| {
            let stated = &header.sources[at as usize + s];
            let f = f as i128;
            let moved = i128::from(stated.origin) - f * i128::from(source.origin);
            let align =
                i128::from(stated.align.unwrap_or(0)) - f * i128::from(source.align.unwrap_or(0));
            (moved, align)
        });
        let first = moves.next().expect("a recording has a source");
        if let Some(other) = moves.find(|&m| m != first) {
            return Err(Error::Disagree {
                input: number,
                moves: (first, other),
            });
        }
        // No check that `align − moved` gives every input one base: where a recording is the
        // capture behind a moved or cut conversion, its move also holds the conversion's origin.
        let moved = within(|| format!("input {number}'s move"), first.0)?;
        let align = within(|| format!("input {number}'s align"), first.1)?;
        inputs.push(InputMap {
            factor: f as u64,
            align,
            moved,
        });
    }
    let mut channels = Vec::with_capacity(header.channels.len());
    for c in &header.channels {
        let j = offsets
            .iter()
            .rposition(|&at| at <= c.source)
            .expect("the first input's sources start at 0");
        let s = (c.source - offsets[j]) as usize;
        let r = &against[j];
        let mut found = r
            .channels
            .iter()
            .enumerate()
            .filter(|(_, ic)| ic.source as usize == s && ic.source_name == c.source_name);
        let (ic, input_channel) = found.next().ok_or_else(|| Error::NoInputChannel {
            name: c.name.clone(),
            input: j + 1,
            index: s,
            source_name: c.source_name.clone(),
        })?;
        if found.next().is_some() {
            return Err(Error::Ambiguous {
                name: c.name.clone(),
                input: j + 1,
                index: s,
                source_name: c.source_name.clone(),
            });
        }
        let map = inputs[j];
        let offset_m = within(
            || format!("{}'s offset", c.name),
            i128::from(c.shaping.offset.as_ref().map_or(0, |o| o.ticks))
                - i128::from(map.factor)
                    * i128::from(input_channel.shaping.offset.as_ref().map_or(0, |o| o.ticks)),
        )?;
        let shift = map.moved + offset_m;
        let kept = c.shaping.kept.ok_or_else(|| Error::NoKept {
            name: c.name.clone(),
        })?;
        let f = map.factor as i64;
        let (from, to) = (kept.from - shift, kept.to - shift);
        if from % f != 0 || to % f != 0 {
            return Err(Error::NotWhole {
                name: c.name.clone(),
                from: kept.from,
                to: kept.to,
            });
        }
        let (from, to) = (from / f, to / f);
        let span = Span {
            from: Some(from),
            to: Some(to),
        };
        let samples = match &input_channel.data {
            ChannelData::Digital(_) => None,
            ChannelData::Analog(a) => {
                let cut = AnalogCut::new(a, (from > 0).then_some(from), to).ok_or_else(|| {
                    Error::NoGridSample {
                        name: c.name.clone(),
                        span,
                        q: grid(a.period),
                    }
                })?;
                Some((cut.skip, cut.keep))
            }
        };
        let unit = match &c.data {
            escapement_tickwave_format::header::ChannelKind::Analog { unit, .. } => {
                Some(unit.clone())
            }
            escapement_tickwave_format::header::ChannelKind::Digital { .. } => None,
        };
        channels.push(finish_channel(
            ChannelMap {
                input: j,
                channel: ic,
                name: c.name.clone(),
                source: c.source,
                from,
                to,
                samples,
                shift,
                shaping: Shaping::default(),
                unit,
            },
            input_channel,
            map.factor,
            offset_m,
        )?);
    }
    // Each derived channel is one of its input's, over the channels the header takes from it.
    for d in &header.derived {
        let operand = |name: &str| {
            channels
                .iter()
                .find(|c| c.name == name)
                .map(|c| (c.input, against[c.input].channels[c.channel].name.as_str()))
        };
        let layout_of = |name: &str| {
            channels
                .iter()
                .find(|c| c.name == name)
                .map(|c| (c.from, c.to, c.samples, c.shift))
        };
        if layout_of(&d.product[0]) != layout_of(&d.product[1]) {
            return Err(Error::DerivedApart {
                name: d.name.clone(),
                a: d.product[0].clone(),
                b: d.product[1].clone(),
            });
        }
        let found = match (operand(&d.product[0]), operand(&d.product[1])) {
            (Some((ja, a)), Some((jb, b))) if ja == jb => against[ja]
                .derived
                .iter()
                .any(|e| e.product == [a.to_owned(), b.to_owned()] && e.unit == d.unit),
            _ => false,
        };
        if !found {
            return Err(Error::NoDerived {
                name: d.name.clone(),
                a: d.product[0].clone(),
                b: d.product[1].clone(),
                unit: d.unit.clone(),
            });
        }
    }
    let layout = Layout {
        timebase,
        length: layout_length(&channels),
        inputs,
        channels,
        derived: header.derived.clone(),
    };
    Ok(apply(&layout, against))
}
