//! Merging recordings onto one time axis (plan/merge-2026-11-12.md; FORMAT.md "Times",
//! "Sources", "Moved and trimmed channels"). The merged recording ticks at the least common
//! multiple of its sources' rates, or at the fastest of them where the multiple puts a term past
//! 2^53. Every channel keeps its source's ticks; a merge restates each source's placement. Input
//! j's axis tick `x` lies at merged tick `A_j − base + floor(x × s_j × R / R_j)`: its alignment,
//! the merged recording's start, and its stretch. The header states all of it, so `expected`
//! rebuilds the merge from the header and its inputs alone.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Seek, SeekFrom, Write};
use std::sync::Arc;

use escapement_tickwave_format::annotations::{self, ExactTicks, Time};
use escapement_tickwave_format::encode::{Bundle, EncodeError, Options, encode_to};
use escapement_tickwave_format::header::{AnalogCoding, ChannelKind, Derived, Header, JSON_SAFE};
use escapement_tickwave_format::mapping::{Mapping, axis_ratio};
use escapement_tickwave_format::model::{
    Channel, ChannelData, Kept, Offset, Ratio, Recording, Shaping, Source,
};

use crate::convert::{Pass, Progress, counted};
use crate::recording::{Check, Opened};
use crate::shape::{self, AnalogCut, Keep, Span, exact, off_grid, rounded};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("a merge takes at least two recordings; {0} given")]
    TooFew(usize),
    #[error("none of the {0} recordings holds a channel")]
    NoSource(usize),
    #[error("settings for input {given}, but {inputs} inputs")]
    Settings { given: usize, inputs: usize },
    #[error(transparent)]
    Read(#[from] crate::recording::Error),
    #[error("{stem}: {why}")]
    Rewrite {
        stem: String,
        why: escapement_tickwave_format::record::Rewrite,
    },
    #[error("reading {0}: {1}")]
    Io(String, std::io::Error),
    #[error("the inputs tick {rates}: their least common multiple is past 2^53")]
    Timebase { rates: String },
    #[error(
        "input {input}{stretch}: a tick of it, or of a source of it, would be a ratio of merged \
         ticks with a term past 2^53"
    )]
    Stretch {
        input: usize,
        /// The stretch, as ", stretched a/b", where there is one.
        stretch: String,
    },
    #[error("input {input} is given a stretch and two marks, which give one each")]
    TwoStretches { input: usize },
    /// The inputs' generators declare one paint role differently, or a declaration breaks a rule
    /// (ANNOTATIONS.md, "Roles"): every problem, joined.
    #[error("the inputs' paint roles: {0}")]
    Roles(String),
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
        "{name}: {time:?} is {ticks:.3} {grid} ticks, more than 0.01 of a {grid} tick off the grid"
    )]
    OffGrid {
        name: String,
        time: String,
        ticks: f64,
        /// Whose ticks: "merged" or "source".
        grid: &'static str,
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
        "{name}: kept {span}; its first kept sample's phase gives a coarser grid than a later \
         run's phase needs, which a channel can't state"
    )]
    GridChanges { name: String, span: Span },
    #[error(
        "{name}: its input states a {kind:?} offset, which the merge's own offset can't add to"
    )]
    OffsetKind { name: String, kind: String },
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
        "the merged recording ticks {stated}, neither {lcm}, the least common multiple of the \
         recordings' rates, nor {fastest}, the fastest of them"
    )]
    NotLcm {
        stated: String,
        lcm: String,
        fastest: String,
    },
    #[error(
        "input {input}: its sources' origins or aligns state different moves, {0:?} and {1:?}",
        .moves.0, .moves.1
    )]
    Disagree {
        input: usize,
        moves: ((i128, i128), (i128, i128)),
    },
    #[error("channel {name} is of kind {kind}, which this version doesn't know")]
    UnknownKind { name: String, kind: String },
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
}

/// A recording to merge, as `recording::open` gives it, with what the model does not hold.
pub struct Input {
    pub recording: Recording,
    /// Its file's name without the extension: it names a clashing channel or generator apart.
    pub stem: String,
    /// The sidecar it carries, file name to text: its header's bundle, or the sidecar beside it.
    pub bundled: Option<BTreeMap<String, String>>,
    /// Each channel's coding, by index, so the merge stores the samples as the input does: None
    /// for a digital channel.
    pub codings: Vec<Option<AnalogCoding>>,
    check: Check,
}

impl Input {
    /// The recording `input` holds, with no block decoded: the merge decodes the channels it
    /// takes before it writes. The sidecar is its bundle; the caller may give it one found
    /// beside the file instead.
    pub fn read(
        input: Arc<dyn crate::input::Input>,
        stem: impl Into<String>,
    ) -> Result<Self, Error> {
        let stem = stem.into();
        if let Some(why) = crate::recording::rewrite_check(&*input, "merging")? {
            return Err(Error::Rewrite { stem, why });
        }
        let Opened {
            header,
            bundled,
            mut recording,
            check,
        } = crate::recording::open(input)?;
        // Its title and meta go with its sources, which the merged recording's own don't keep. A
        // source that came from a merge already has its own recording's.
        for s in &mut recording.sources {
            if s.title.is_none() && s.meta.is_empty() {
                s.title.clone_from(&header.title);
                s.meta.clone_from(&header.meta);
            }
        }
        Ok(Self {
            recording,
            stem,
            bundled,
            codings: header.channels.iter().map(|c| c.coding()).collect(),
            check,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct InputSettings {
    /// A move: a time at the merged rate, positive later, a whole number of merged ticks or a
    /// decimal with a unit, within 0.01 of a merged tick. Or two marks, `x@y,x@y`: each an event's
    /// time in the input's own time and the merged time it takes, which give a move and a
    /// stretch.
    pub align: Option<String>,
    /// A stretch of the input's ticks: a ratio `a/b`, or a change such as `+20ppm`.
    pub stretch: Option<String>,
    /// The channels to take, by the input's names, in order, each renamed where a name is given.
    /// None given takes every channel under its own name.
    pub channels: Vec<(String, Option<String>)>,
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// By input, in order; an input past the end takes the defaults.
    pub inputs: Vec<InputSettings>,
    /// Output channels to move, by name, each by a time in its source's own time, on its source's
    /// grid.
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
    /// Generated annotations carried into the merged recording's bundle.
    AnnotationsCarried {
        input: usize,
        stem: String,
        count: usize,
    },
    /// Generated annotations left out: on no channel the merge takes, before the merged
    /// recording's start or past its end, or not reading.
    AnnotationsLeftOut {
        input: usize,
        stem: String,
        count: usize,
        why: &'static str,
    },
    /// Generated annotations carried with keys or `measure`s this version doesn't know, which a
    /// later minor version of their generator's file may add, each named as
    /// `annotations::Lines::carrying` names it.
    KeysCarried {
        input: usize,
        stem: String,
        generator: String,
        keys: Vec<String>,
    },
}

impl Note {
    /// Whether it loses something a reader may miss.
    pub fn warns(&self) -> bool {
        !matches!(
            self,
            Note::GeneratorRenamed { .. }
                | Note::AnnotationsCarried { .. }
                | Note::KeysCarried { .. }
        )
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
            Note::AnnotationsCarried { input, stem, count } => write!(
                f,
                "input {input} ({stem}): {count} generated annotation{} carried",
                if *count == 1 { "" } else { "s" }
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
            Note::KeysCarried {
                input,
                stem,
                generator,
                keys,
            } => write!(
                f,
                "input {input} ({stem}): generator {generator}'s annotations carried what this \
                 version doesn't know, {}",
                keys.join(", ")
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

/// A stretch as a refusal names it: nothing for none.
fn stretched(s: Ratio) -> String {
    if s.num() == s.den() {
        String::new()
    } else {
        format!(", stretched {}", rate(s))
    }
}

/// A ratio of two u128 terms in lowest terms, as a `Ratio` where both fit below 2^53.
fn ratio_of(num: u128, den: u128) -> Option<Ratio> {
    let g = gcd(num, den).max(1);
    let (num, den) = (num / g, den / g);
    let safe = |v: u128| v < u128::from(JSON_SAFE);
    (safe(num) && safe(den))
        .then(|| Ratio::new(num as u64, den as u64))
        .flatten()
}

/// The least common multiple of `rates`, one rate at least: their reduced numerators' lcm over
/// their reduced denominators' gcd, the slowest rate at which each one's tick is a whole number
/// of ticks.
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

/// The fastest of `rates`, one rate at least.
pub fn fastest_rate(rates: &[Ratio]) -> Ratio {
    let faster = |a: Ratio, b: Ratio| {
        u128::from(a.num()) * u128::from(b.den()) > u128::from(b.num()) * u128::from(a.den())
    };
    rates
        .iter()
        .copied()
        .reduce(|a, b| if faster(b, a) { b } else { a })
        .expect("at least one rate")
}

/// A time at `timebase` in whole ticks, within 0.01 of one; `grid` names whose ticks they are.
fn on_grid(name: &str, text: &str, timebase: Ratio, grid: &'static str) -> Result<i64, Error> {
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
            grid,
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

/// A stretch as `--stretch` takes it: a ratio `a/b`, or a change in parts per million, signed:
/// `+20ppm` is 1,000,020 / 1,000,000. Above 1 lengthens the input's ticks.
pub fn parse_stretch(text: &str) -> Result<Ratio, String> {
    let text = text.trim();
    let bad = || format!("{text:?}: a stretch is a ratio a/b, or a change such as +20ppm");
    if let Some((a, b)) = text.split_once('/') {
        let (a, b) = (a.trim().parse::<u64>(), b.trim().parse::<u64>());
        let (Ok(a), Ok(b)) = (a, b) else {
            return Err(bad());
        };
        return ratio_of(u128::from(a), u128::from(b))
            .filter(|_| a > 0 && b > 0)
            .ok_or_else(bad);
    }
    let number = text.strip_suffix("ppm").ok_or_else(bad)?.trim();
    let (negative, digits) = match number.as_bytes().first() {
        Some(b'-') => (true, &number[1..]),
        Some(b'+') => (false, &number[1..]),
        _ => (false, number),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
        || fraction.len() > 12
    {
        return Err(bad());
    }
    let scale = 10u128.pow(fraction.len() as u32);
    let ppm = format!("{whole}{fraction}")
        .parse::<u128>()
        .map_err(|_| bad())?;
    let one = 1_000_000 * scale;
    let num = if negative {
        one.checked_sub(ppm).filter(|&n| n > 0).ok_or_else(bad)?
    } else {
        one.checked_add(ppm).ok_or_else(bad)?
    };
    ratio_of(num, one).ok_or_else(bad)
}

/// An input's alignment as `--align` gives it: a move at the merged rate, or two marks, each a
/// time in the input's own time and the time it takes on the merged axis, which give a move and a
/// stretch.
enum Alignment {
    Move(String),
    /// Each mark's input time and merged time, in exact seconds.
    Marks([(ExactTicks, ExactTicks); 2]),
}

/// An exact time in seconds as a signed ratio of its numerator and its positive denominator.
fn signed(t: &ExactTicks) -> (i128, i128) {
    (
        if t.negative {
            -t.numerator
        } else {
            t.numerator
        },
        t.denominator,
    )
}

impl Alignment {
    fn parse(name: &str, text: &str) -> Result<Self, Error> {
        let time = |problem| Error::Time {
            name: name.to_owned(),
            problem,
        };
        let Some((a, b)) = text.split_once(',') else {
            return Ok(Self::Move(text.to_owned()));
        };
        let mark = |m: &str| -> Result<(ExactTicks, ExactTicks), Error> {
            let (x, y) = m.split_once('@').ok_or_else(|| {
                time(format!(
                    "{m:?}: a mark is the input's time and the merged time, as 1.5ms@2.25ms"
                ))
            })?;
            let seconds = |t: &str| exact(t, Ratio::ONE).map_err(time);
            Ok((seconds(x)?, seconds(y)?))
        };
        Ok(Self::Marks([mark(a)?, mark(b)?]))
    }
}

/// Where input j's axis lies on the merged axis: its tick `x` at merged tick
/// `moved + floor(x × q)`, `q = stretch × R / R_j`, with `moved = align − base`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InputMap {
    q: Ratio,
    stretch: Ratio,
    align: i64,
    moved: i64,
}

impl InputMap {
    /// Input tick `x` on the merged axis, in i128: a time far outside is left out, not wrapped.
    fn tick(&self, x: i64) -> i128 {
        i128::from(self.moved) + scaled(x, self.q)
    }
}

/// `floor(x × q)`.
fn scaled(x: i64, q: Ratio) -> i128 {
    (i128::from(x) * i128::from(q.num())).div_euclid(i128::from(q.den()))
}

/// One merged channel: where its data comes from, how it is cut and moved, in its source's ticks,
/// and what the header states of it.
#[derive(Clone, Debug)]
struct ChannelMap {
    input: usize,
    channel: usize,
    name: String,
    source: u32,
    /// The source-tick cut `from..to`, and whether it cuts the channel's own span.
    from: i64,
    to: i64,
    cut: bool,
    /// An analog channel's samples skipped, then kept.
    samples: Option<(u64, u64)>,
    /// A cut digital channel's edges kept, once counted.
    edges: Option<u64>,
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

/// An input channel's mapping on its input's axis, from its input's model.
fn input_mapping(r: &Recording, c: &Channel) -> Option<Mapping> {
    let source = r.sources.get(c.source as usize)?;
    let m = axis_ratio(
        source.stretch.unwrap_or(Ratio::ONE),
        r.timebase,
        source.timebase,
    )?;
    Some(Mapping::new(
        source.origin,
        c.shaping.offset.as_ref().map_or(0, |o| o.ticks),
        m,
    ))
}

/// An input channel's own span, in its source's ticks: its `kept`, or, where it states none, from
/// the source tick at its input's start to the one at its input's end.
fn own_span(r: &Recording, c: &Channel, map: &Mapping) -> (i64, i64) {
    c.shaping.kept.map_or_else(
        || (map.first_at(0), map.first_at(r.length)),
        |k| (k.from, k.to),
    )
}

/// `stretch × more`, in lowest terms, where both terms stay below 2^53.
fn compose(stretch: Option<Ratio>, more: Ratio) -> Option<Ratio> {
    let s = stretch.unwrap_or(Ratio::ONE);
    ratio_of(
        u128::from(s.num()) * u128::from(more.num()),
        u128::from(s.den()) * u128::from(more.den()),
    )
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

/// What a merge takes of its inputs, before it places them on an axis: the channels, renamed,
/// the derived channels, and each setting resolved to its channel.
struct Chosen {
    /// Each output channel: input, input channel, name.
    channels: Vec<(usize, usize, String)>,
    /// Each derived channel carried: its input, the output's, and the input's own.
    derived: Vec<(usize, Derived, Derived)>,
    /// Each output channel's own move, in its source's ticks.
    offsets: Vec<Option<i64>>,
    /// Each output channel's kept span, in its input's ticks.
    keeps: Vec<Option<Span>>,
    units: Vec<Option<String>>,
    /// Each input's stretch as given, and its alignment.
    stretches: Vec<Option<Ratio>>,
    aligns: Vec<Option<Alignment>>,
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
    let (chosen, mut notes) = choose(inputs, settings)?;
    // FORMAT.md, "Times": the least common multiple of the sources' rates, where every term it
    // puts on the axis stays below 2^53; otherwise the fastest of them.
    let rates: Vec<Ratio> = inputs
        .iter()
        .flat_map(|i| i.recording.sources.iter().map(|s| s.timebase))
        .collect();
    // Recordings that hold no channel hold no source: nothing to tick at.
    if rates.is_empty() {
        return Err(Error::NoSource(inputs.len()));
    }
    let fastest = fastest_rate(&rates);
    // Only a tick, a ratio or the length past 2^53 moves the merge to the fastest rate; any other
    // refusal stands.
    let past_2_53 = |e: &Error| {
        matches!(
            e,
            Error::Stretch { .. } | Error::Length(_) | Error::Range { .. }
        )
    };
    let layout = match merged_timebase(&rates) {
        Ok(lcm) => match place(inputs, &chosen, lcm) {
            Err(e) if past_2_53(&e) && reduced(lcm) != reduced(fastest) => {
                place(inputs, &chosen, fastest)
            }
            placed => placed,
        },
        Err(_) => place(inputs, &chosen, fastest),
    }?;
    notes.extend(derived_notes(inputs, &chosen, &layout));
    Ok((layout, notes))
}

/// The channels a merge takes and the settings resolved to them.
fn choose(inputs: &[Input], settings: &Settings) -> Result<(Chosen, Vec<Note>), Error> {
    let defaults = InputSettings::default();
    let input_settings = |j: usize| settings.inputs.get(j).unwrap_or(&defaults);
    let mut notes = Vec::new();

    // Each output channel: (input, input channel, name), named in order, an input's derived
    // channels after its stored ones.
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut chosen: Vec<(usize, usize, String)> = Vec::new();
    let mut derived = Vec::new();
    let mut stretches = Vec::with_capacity(inputs.len());
    let mut aligns = Vec::with_capacity(inputs.len());
    for (j, input) in inputs.iter().enumerate() {
        let number = j + 1;
        let r = &input.recording;
        let s = input_settings(j);
        let name = format!("input {number}'s alignment");
        let align = s
            .align
            .as_deref()
            .map(|a| Alignment::parse(&name, a))
            .transpose()?;
        let stretch = s
            .stretch
            .as_deref()
            .map(|t| {
                parse_stretch(t).map_err(|problem| Error::Time {
                    name: format!("input {number}'s stretch"),
                    problem,
                })
            })
            .transpose()?;
        if stretch.is_some() && matches!(align, Some(Alignment::Marks(_))) {
            return Err(Error::TwoStretches { input: number });
        }
        stretches.push(stretch);
        aligns.push(align);
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
            let renamed: Vec<Option<&String>> = d
                .operands
                .iter()
                .map(|o| names_in.get(o.as_str()))
                .collect();
            match renamed.iter().position(Option::is_none) {
                None => {
                    let name = unique(&d.name, &input.stem, &taken);
                    taken.insert(name.clone());
                    derived.push((
                        j,
                        Derived {
                            name,
                            operands: renamed.into_iter().flatten().cloned().collect(),
                            ..d.clone()
                        },
                        d.clone(),
                    ));
                }
                Some(i) => notes.push(Note::DerivedLeftOut {
                    input: number,
                    stem: input.stem.clone(),
                    name: d.name.clone(),
                    why: format!("its operand {} is not taken", d.operands[i]),
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
    // A channel's own move is in its source's ticks: on its source's grid.
    let mut offsets = vec![None; chosen.len()];
    for (name, time) in &settings.offsets {
        let c = find("an offset", name)?;
        if offsets[c].is_some() {
            return Err(Error::Twice {
                what: "an offset",
                name: name.clone(),
            });
        }
        let (j, ic, _) = &chosen[c];
        let r = &inputs[*j].recording;
        let source_rate = r.sources[r.channels[*ic].source as usize].timebase;
        offsets[c] = Some(on_grid(name, time, source_rate, "source")?);
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
    Ok((
        Chosen {
            channels: chosen,
            derived,
            offsets,
            keeps,
            units,
            stretches,
            aligns,
        },
        notes,
    ))
}

/// An input's stretch on an axis at `timebase`, and its alignment there in merged ticks: as
/// given, or from its two marks.
fn input_placement(
    number: usize,
    input: &Recording,
    stretch: Option<Ratio>,
    align: Option<&Alignment>,
    timebase: Ratio,
) -> Result<(Ratio, i64), Error> {
    let name = format!("input {number}'s alignment");
    let fits = |s: Ratio| {
        axis_ratio(s, timebase, input.timebase).is_some()
            && input.sources.iter().all(|source| {
                compose(source.stretch, s)
                    .and_then(|st| axis_ratio(st, timebase, source.timebase))
                    .is_some()
            })
    };
    let unplaceable = |s: Ratio| Error::Stretch {
        input: number,
        stretch: stretched(s),
    };
    match align {
        None => {
            let s = stretch.unwrap_or(Ratio::ONE);
            if !fits(s) {
                return Err(unplaceable(s));
            }
            Ok((s, 0))
        }
        Some(Alignment::Move(text)) => {
            let s = stretch.unwrap_or(Ratio::ONE);
            if !fits(s) {
                return Err(unplaceable(s));
            }
            Ok((s, on_grid(&name, text, timebase, "merged")?))
        }
        Some(Alignment::Marks([(x1, y1), (x2, y2)])) => {
            let too_far = || Error::Time {
                name: name.clone(),
                problem: "the marks lie too far apart, or too close, to give a stretch".into(),
            };
            let [x1, y1, x2, y2] = [x1, y1, x2, y2].map(Q::of);
            // Δx and Δy, in exact seconds.
            let (dx, dy) = (
                x2.sub(x1).ok_or_else(too_far)?,
                y2.sub(y1).ok_or_else(too_far)?,
            );
            if dx.n == 0 || dy.n == 0 || (dx.n > 0) != (dy.n > 0) {
                return Err(Error::Time {
                    name,
                    problem: "the marks must lie apart, and in the same order, in both times"
                        .into(),
                });
            }
            let (dx, dy) = (dx.abs().ok_or_else(too_far)?, dy.abs().ok_or_else(too_far)?);
            // Each mark's x is good to one of the input's ticks, its coarsest source's for a merged
            // input, and its y to one merged tick. So the stretch is any slope through both marks
            // within those, from (Δy − δy) / (Δx + δx) to (Δy + δy) / (Δx − δx), and takes the
            // simplest ratio there.
            let tick = |rate: Ratio, stretch: Option<Ratio>| {
                let s = stretch.unwrap_or(Ratio::ONE);
                Q::new(
                    i128::from(s.num()).checked_mul(i128::from(rate.den()))?,
                    i128::from(s.den()).checked_mul(i128::from(rate.num()))?,
                )
            };
            let dx_tick = input
                .sources
                .iter()
                .try_fold(tick(input.timebase, None).ok_or_else(too_far)?, |a, s| {
                    a.max(tick(s.timebase, s.stretch)?)
                })
                .ok_or_else(too_far)?;
            let dy_tick = tick(timebase, None).ok_or_else(too_far)?;
            let lo = dy.sub(dy_tick).zip(dx.add(dx_tick));
            let hi = dy.add(dy_tick).zip(dx.sub(dx_tick));
            let (n, d) = lo
                .zip(hi)
                .filter(|&((a, b), (c, d))| [a, b, c, d].iter().all(|q| q.n > 0))
                .and_then(|((a, b), (c, d))| simplest(a.div(b)?, c.div(d)?))
                .ok_or_else(too_far)?;
            let s = ratio_of(n, d)
                .filter(|&s| fits(s))
                .ok_or_else(|| Error::Time {
                    name: name.clone(),
                    problem: "the marks lie so far apart that no stretch within their error has \
                              terms below 2^53"
                        .into(),
                })?;
            // The input's tick 0 lies where its first mark's time, stretched, meets the merged
            // time: (y1 − x1 × s) × R merged ticks, to the nearest.
            let (r, s_q) = (Q::ratio(timebase), Q::ratio(s));
            let at = y1
                .mul(r)
                .zip(x1.mul(r).and_then(|x| x.mul(s_q)))
                .and_then(|(y, x)| y.sub(x))
                .ok_or_else(too_far)?;
            let ticks = ExactTicks {
                negative: at.n < 0,
                numerator: at.n.abs(),
                denominator: at.d,
            }
            .rounded()
            .ok_or_else(too_far)?;
            Ok((s, within(|| name.clone(), i128::from(ticks))?))
        }
    }
}

/// An exact fraction in lowest terms, its denominator positive, for the arithmetic two marks take:
/// each step reduced, so times to the picosecond and a stretch near 2^53 stay inside 128 bits.
#[derive(Clone, Copy, Debug)]
struct Q {
    n: i128,
    d: i128,
}

impl Q {
    fn new(n: i128, d: i128) -> Option<Self> {
        if d == 0 {
            return None;
        }
        let g = gcd(n.unsigned_abs(), d.unsigned_abs()).max(1) as i128;
        let sign = if d < 0 { -1 } else { 1 };
        Some(Self {
            n: sign * (n / g),
            d: sign * (d / g),
        })
    }

    /// An exact time in seconds.
    fn of(t: &ExactTicks) -> Self {
        let (n, d) = signed(t);
        Self::new(n, d).unwrap_or(Self { n: 0, d: 1 })
    }

    fn ratio(r: Ratio) -> Self {
        Self {
            n: i128::from(r.num()),
            d: i128::from(r.den()),
        }
        .reduce()
    }

    fn reduce(self) -> Self {
        Self::new(self.n, self.d).unwrap_or(self)
    }

    fn mul(self, o: Self) -> Option<Self> {
        // Cancelled crosswise first, so neither product holds a factor the result drops.
        let g1 = gcd(self.n.unsigned_abs(), o.d.unsigned_abs()).max(1) as i128;
        let g2 = gcd(o.n.unsigned_abs(), self.d.unsigned_abs()).max(1) as i128;
        Self::new(
            (self.n / g1).checked_mul(o.n / g2)?,
            (self.d / g2).checked_mul(o.d / g1)?,
        )
    }

    fn div(self, o: Self) -> Option<Self> {
        self.mul(Self::new(o.d, o.n)?)
    }

    fn sub(self, o: Self) -> Option<Self> {
        let g = gcd(self.d.unsigned_abs(), o.d.unsigned_abs()).max(1) as i128;
        let (a, b) = (self.d / g, o.d / g);
        Self::new(
            self.n.checked_mul(b)?.checked_sub(o.n.checked_mul(a)?)?,
            self.d.checked_mul(b)?,
        )
    }

    fn add(self, o: Self) -> Option<Self> {
        self.sub(Self::new(o.n.checked_neg()?, o.d)?)
    }

    fn abs(self) -> Option<Self> {
        Self::new(self.n.checked_abs()?, self.d)
    }

    fn max(self, o: Self) -> Option<Self> {
        Some(if self.sub(o)?.n < 0 { o } else { self })
    }
}

/// The ratio of least denominator in `[lo, hi]`, `0 < lo ≤ hi`, by continued fractions: where an
/// integer lies in the interval the least one ends the fraction; otherwise both ends share an
/// integer part `a`, and the ratio is `a + 1 / r`, with `r` the simplest in
/// `[1 / (hi − a), 1 / (lo − a)]`.
fn simplest(mut lo: Q, mut hi: Q) -> Option<(u128, u128)> {
    let (mut h0, mut h1, mut k0, mut k1) = (0u128, 1u128, 1u128, 0u128);
    loop {
        let a = lo.n.div_euclid(lo.d);
        let ceil = if lo.n.rem_euclid(lo.d) == 0 { a } else { a + 1 };
        let ends = ceil.checked_mul(hi.d)? <= hi.n;
        let t = u128::try_from(if ends { ceil } else { a }).ok()?;
        let (h2, k2) = (
            t.checked_mul(h1)?.checked_add(h0)?,
            t.checked_mul(k1)?.checked_add(k0)?,
        );
        if ends {
            return Some((h2, k2));
        }
        (h0, h1, k0, k1) = (h1, h2, k1, k2);
        let whole = Q::new(a, 1)?;
        (lo, hi) = (
            Q::new(1, 1)?.div(hi.sub(whole)?)?,
            Q::new(1, 1)?.div(lo.sub(whole)?)?,
        );
    }
}

/// The merge laid out on an axis at `timebase`: every input placed, every channel cut, the
/// recording's start and length.
fn place(inputs: &[Input], chosen: &Chosen, timebase: Ratio) -> Result<Layout, Error> {
    let mut placements = Vec::with_capacity(inputs.len());
    for (j, input) in inputs.iter().enumerate() {
        let (stretch, align) = input_placement(
            j + 1,
            &input.recording,
            chosen.stretches[j],
            chosen.aligns[j].as_ref(),
            timebase,
        )?;
        let q = axis_ratio(stretch, timebase, input.recording.timebase).ok_or(Error::Stretch {
            input: j + 1,
            stretch: stretched(stretch),
        })?;
        placements.push((stretch, q, align));
    }
    let source_offsets = source_offsets(inputs.iter().map(|i| &i.recording));

    // Each channel's cut in its source's ticks, and where its kept span would start were the
    // merged recording to start at merged tick 0.
    struct Cut {
        from: i64,
        to: i64,
        cut: bool,
        samples: Option<(u64, u64)>,
        offset: i64,
        m: Ratio,
        start: i128,
    }
    let mut cuts = Vec::with_capacity(chosen.channels.len());
    for (c, (j, ic, name)) in chosen.channels.iter().enumerate() {
        let r = &inputs[*j].recording;
        let channel = &r.channels[*ic];
        let source = &r.sources[channel.source as usize];
        let (stretch, q, align) = placements[*j];
        let map = input_mapping(r, channel).ok_or_else(|| Error::Stretch {
            input: j + 1,
            stretch: stretched(stretch),
        })?;
        let (own_from, own_to) = own_span(r, channel, &map);
        let span = chosen.keeps[c].unwrap_or(Span {
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
        // A keep is in the input's ticks: its ends are the first source ticks at or past them.
        let from = span
            .from
            .map_or(own_from, |f| map.first_at(f).max(own_from));
        let to = span.to.map_or(own_to, |t| map.first_at(t).min(own_to));
        if from >= to {
            return Err(Error::Outside {
                name: name.clone(),
                span,
                from: own_from,
                to: own_to,
            });
        }
        let offset_m = chosen.offsets[c].unwrap_or(0);
        if offset_m != 0
            && let Some(o) = &channel.shaping.offset
            && o.kind != Offset::DELAY
        {
            return Err(Error::OffsetKind {
                name: name.clone(),
                kind: o.kind.clone(),
            });
        }
        let offset = within(
            || format!("{name}'s offset"),
            i128::from(map.offset()) + i128::from(offset_m),
        )?;
        let (from, samples) = match &channel.data {
            ChannelData::Digital(_) => (from, None),
            ChannelData::Analog(a) => {
                let cut =
                    AnalogCut::new(a, (from > own_from).then_some(from), to).ok_or_else(|| {
                        Error::GridChanges {
                            name: name.clone(),
                            span,
                        }
                    })?;
                (cut.from.unwrap_or(from), Some((cut.skip, cut.keep)))
            }
        };
        let m = compose(source.stretch, stretch)
            .and_then(|st| axis_ratio(st, timebase, source.timebase))
            .ok_or_else(|| Error::Stretch {
                input: j + 1,
                stretch: stretched(stretch),
            })?;
        let at = |k: i64| i128::from(align) + scaled(source.origin, q) + scaled(k + offset, m);
        cuts.push(Cut {
            from,
            to,
            cut: from > own_from || to < own_to,
            samples,
            offset,
            m,
            start: at(from),
        });
    }
    // The recording starts at the earliest channel's start, after its move and its alignment.
    let base = cuts
        .iter()
        .map(|c| c.start)
        .min()
        .expect("at least one channel");
    let mut inputs_mapped = Vec::with_capacity(inputs.len());
    for (j, &(stretch, q, align)) in placements.iter().enumerate() {
        let number = j + 1;
        let moved = within(
            || format!("input {number}'s move"),
            i128::from(align) - base,
        )?;
        let map = InputMap {
            q,
            stretch,
            align,
            moved,
        };
        for (s, source) in inputs[j].recording.sources.iter().enumerate() {
            let (origin, align) = source_ticks(source, map);
            within(|| format!("input {number}'s source {s}'s origin"), origin)?;
            within(|| format!("input {number}'s source {s}'s align"), align)?;
        }
        inputs_mapped.push(map);
    }
    let mut channels = Vec::with_capacity(chosen.channels.len());
    let mut length = 1i128;
    for (c, ((j, ic, name), cut)) in chosen.channels.iter().zip(cuts).enumerate() {
        let input_channel = &inputs[*j].recording.channels[*ic];
        let end = cut.start - base - scaled(cut.from + cut.offset, cut.m)
            + scaled(cut.to + cut.offset, cut.m);
        length = length.max(end);
        let kind = input_channel
            .shaping
            .offset
            .as_ref()
            .map_or_else(|| Offset::DELAY.to_owned(), |o| o.kind.clone());
        channels.push(ChannelMap {
            input: *j,
            channel: *ic,
            name: name.clone(),
            source: source_offsets[*j] + input_channel.source,
            from: cut.from,
            to: cut.to,
            cut: cut.cut,
            samples: cut.samples,
            edges: None,
            shaping: Shaping {
                offset: (cut.offset != 0).then_some(Offset {
                    ticks: cut.offset,
                    kind,
                }),
                kept: Some(Kept {
                    from: cut.from,
                    to: cut.to,
                }),
            },
            unit: chosen.units[c].clone(),
        });
    }
    if length >= i128::from(JSON_SAFE) {
        return Err(Error::Length(length));
    }
    // A derived channel's operands must still share their samples: cut and moved alike.
    let mut derived = Vec::new();
    for (_, d, _) in &chosen.derived {
        let layout_of = |name: &str| {
            channels.iter().find(|c| c.name == name).map(|c| {
                (
                    c.from,
                    c.to,
                    c.samples,
                    c.shaping.offset.as_ref().map(|o| o.ticks),
                )
            })
        };
        let first = d.operands.first().map(|o| layout_of(o));
        if d.operands.iter().all(|o| Some(layout_of(o)) == first) {
            derived.push(d.clone());
        }
    }
    Ok(Layout {
        timebase,
        length: length as i64,
        inputs: inputs_mapped,
        channels,
        derived,
    })
}

/// The derived channels `choose` carried that `place` left out, as notes: their operands are
/// moved or cut apart.
fn derived_notes(inputs: &[Input], chosen: &Chosen, layout: &Layout) -> Vec<Note> {
    chosen
        .derived
        .iter()
        .filter(|(_, d, _)| !layout.derived.iter().any(|e| e.name == d.name))
        .map(|(j, _, input_derived)| Note::DerivedLeftOut {
            input: j + 1,
            stem: inputs[*j].stem.clone(),
            name: input_derived.name.clone(),
            why: format!(
                "its operands {} are moved or cut apart",
                input_derived.operands.join(" and ")
            ),
        })
        .collect()
}

/// A source's origin and align once its input is placed by `map` (FORMAT.md, "Sources"): a value
/// `x` on the input's axis becomes `floor(x × q)`, the origin then moved by the input's alignment
/// and the merged recording's start, the align by the alignment.
fn source_ticks(s: &Source, map: InputMap) -> (i128, i128) {
    (
        i128::from(map.moved) + scaled(s.origin, map.q),
        i128::from(map.align) + scaled(s.align.unwrap_or(0), map.q),
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

/// Reads `inputs`, the recordings the layout was made from, for each cut digital channel's edges
/// in its kept span: the count the encoder holds the channel to.
fn count_cut_edges(layout: &mut Layout, inputs: Vec<Input>) -> Result<(), Error> {
    for c in layout
        .channels
        .iter()
        .filter(|c| c.cut && c.samples.is_none())
    {
        inputs[c.input].check.channel(c.channel)?;
    }
    let mut channels: Vec<Vec<Option<Channel>>> = inputs
        .into_iter()
        .map(|i| i.recording.channels.into_iter().map(Some).collect())
        .collect();
    for c in layout.channels.iter_mut().filter(|c| c.cut) {
        if let Some(Channel {
            data: ChannelData::Digital(d),
            ..
        }) = channels[c.input][c.channel].take()
        {
            let span = c.from..c.to;
            c.edges = Some(d.edges.filter(|t| span.contains(t)).count() as u64);
        }
    }
    Ok(())
}

/// The merged recording `layout` describes, from its inputs: each channel keeps its source's
/// ticks, cut where its span cuts it.
fn apply(layout: &Layout, inputs: Vec<Recording>) -> Recording {
    let mut sources = Vec::new();
    let mut channels_in: Vec<Vec<Option<Channel>>> = Vec::with_capacity(inputs.len());
    for (r, &map) in inputs.into_iter().zip(&layout.inputs) {
        sources.extend(r.sources.into_iter().map(|s| {
            let (origin, align) = source_ticks(&s, map);
            let stretch = compose(s.stretch, map.stretch).filter(|st| st.num() != st.den());
            Source {
                origin: origin as i64,
                align: Some(align as i64).filter(|&a| a != 0),
                stretch,
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
            let data = match channel.data {
                ChannelData::Digital(d) if c.cut => {
                    let count = c
                        .edges
                        .expect("a merge counts what it cuts before it applies");
                    ChannelData::Digital(shape::digital(d, c.from, c.to, count))
                }
                ChannelData::Analog(a) => {
                    let (skip, keep) = c.samples.expect("an analog channel's cut");
                    let mut a = if c.cut {
                        shape::analog(a, skip, keep)
                    } else {
                        a
                    };
                    if let Some(unit) = &c.unit {
                        a.unit = unit.clone();
                    }
                    ChannelData::Analog(a)
                }
                data => data,
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

/// Generated annotations, each with its generator's name.
type Annotations = Vec<(String, serde_json::Map<String, serde_json::Value>)>;

/// The roles each carried generator's file declares in its first line, by the generator's name in
/// the merge, with the file as a problem names it.
type Roles = Vec<(String, String, Vec<serde_json::Value>)>;

/// Each carried generator's file's first line's keys, by the generator's name in the merge.
type Heads = Vec<(String, serde_json::Map<String, serde_json::Value>)>;

/// Each input's generated annotations, carried: times mapped, generators renamed apart, channels
/// renamed and dropped, and the roles their files declare, and their files' first lines. Hand-written
/// annotations are counted, not carried, nor the roles and view their file declares.
fn carried(
    inputs: &[Input],
    layout: &Layout,
    notes: &mut Vec<Note>,
) -> (Annotations, Roles, Heads) {
    let mut out = Vec::new();
    let mut roles = Vec::new();
    let mut heads = Vec::new();
    let mut generators: BTreeSet<String> = BTreeSet::new();
    // The writer refuses a line that would not resolve: such a line is left out here instead.
    // What a later minor version adds to a line of its file is carried, and named.
    let mut lines = annotations::Lines::new(layout.channels.iter().map(|c| {
        let data = &inputs[c.input].recording.channels[c.channel].data;
        (c.name.clone(), matches!(data, ChannelData::Digital(_)))
    }));
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
        let (mut no_channel, mut before, mut after, mut unread, mut broken) = (0, 0, 0, 0, 0);
        let carried_before = out.len();
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
            roles.push((
                name.clone(),
                format!("input {number} ({}): {file}", input.stem),
                annotations::generator_roles(text),
            ));
            if let Some(keys) = annotations::generator_head_keys(text) {
                heads.push((name.clone(), keys));
            }
            let later = annotations::later_generator(text);
            let mut keys = annotations::Carried::new();
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
                    a.get(key).map(|v| time_ticks(v, tb).map(|t| map.tick(t)))
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
                let taken = match later {
                    true => lines.take_carried(&name, &a),
                    false => lines.take(&name, &a),
                };
                match taken {
                    Ok(carried) => keys.extend(carried),
                    Err(_) => {
                        broken += 1;
                        continue;
                    }
                }
                out.push((name.clone(), a));
            }
            if !keys.is_empty() {
                notes.push(Note::KeysCarried {
                    input: number,
                    stem: input.stem.clone(),
                    generator: name,
                    keys: keys.into_iter().collect(),
                });
            }
        }
        if out.len() > carried_before {
            notes.push(Note::AnnotationsCarried {
                input: number,
                stem: input.stem.clone(),
                count: out.len() - carried_before,
            });
        }
        for (count, why) in [
            (no_channel, "none of their channels is taken"),
            (before, "they lie before the merged recording's start"),
            (after, "they lie past the merged recording's end"),
            (unread, "they do not read"),
            (
                broken,
                "they break a rule ANNOTATIONS.md gives a generator's line",
            ),
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
    (out, roles, heads)
}

/// Merges what `open` gives into `out`, from its start. It decodes only the channels it takes.
/// `open` is called once, and once more where a cut digital channel's edges need counting first.
/// `tickwave verify` checks a merged recording against its inputs, through `expected`.
pub fn merge<F: Write + Seek>(
    open: &dyn Fn() -> Result<Vec<Input>, Error>,
    settings: &Settings,
    progress: &dyn Progress,
    out: &mut F,
) -> Result<Merged, Error> {
    let inputs = open()?;
    let (mut layout, mut notes) = plan(&inputs, settings)?;
    let (annotations, roles, heads) = carried(&inputs, &layout, &mut notes);
    // One name declared differently by two inputs' generators would leave the merged sidecar
    // refused by every reader.
    let problems =
        annotations::roles::declared(roles.iter().map(|(_, file, r)| (file.as_str(), &r[..]))).1;
    if !problems.is_empty() {
        return Err(Error::Roles(problems.join("; ")));
    }
    // A cut digital channel's edges kept are counted before the declaration is written.
    if layout.channels.iter().any(|c| c.cut && c.samples.is_none()) {
        count_cut_edges(&mut layout, open()?)?;
    }
    // Each channel taken is decoded first, so a damaged block is refused as itself before
    // anything is written, not later as a channel short of its count.
    for c in &layout.channels {
        inputs[c.input].check.channel(c.channel)?;
    }
    let analog = layout
        .channels
        .iter()
        .map(|c| inputs[c.input].codings.get(c.channel).copied().flatten())
        .collect();
    let options = {
        let mut o = Options::default();
        o.level = settings.level;
        o.block_bytes = settings.block_kib as usize * 1024;
        o.title = settings.title.clone();
        o.meta = settings.meta.clone();
        o.converter = Some(format!(
            "tickwave {} merge (zstd {}, blocks of {} KiB)",
            env!("CARGO_PKG_VERSION"),
            settings.level,
            settings.block_kib
        ));
        o.analog = analog;
        o.bundle = (!annotations.is_empty()).then(|| Bundle::Generated {
            stem: settings.stem.clone(),
        });
        o.annotations = annotations;
        o.carried = true;
        o.heads = heads;
        o
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
/// statements alone: each source's stretch over its input's gives the merge's stretch of that
/// input, its origin the input's move, its align the alignment, each by the rule FORMAT.md's
/// "Sources" gives, and each channel's offset and kept span its own. Each of `against` covers as
/// many of the header's sources as it has, in order. A unit is a claim, as a name is: it comes
/// from the header, and so does each digital channel's count, which the check holds the edges to.
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
    // The axis a merge of these recordings ticks at: the sources' least common multiple, or the
    // fastest of them where the multiple puts a term past 2^53.
    let rates: Vec<Ratio> = against
        .iter()
        .flat_map(|r| r.sources.iter().map(|s| s.timebase))
        .collect();
    if rates.is_empty() {
        return Err(Error::NoSource(against.len()));
    }
    let timebase = header.timebase;
    let fastest = fastest_rate(&rates);
    let lcm = merged_timebase(&rates).ok();
    if lcm.is_none_or(|l| reduced(l) != reduced(timebase)) && reduced(fastest) != reduced(timebase)
    {
        return Err(Error::NotLcm {
            stated: rate(timebase),
            lcm: lcm.map_or_else(|| "past 2^53".to_owned(), rate),
            fastest: rate(fastest),
        });
    }
    let mut inputs = Vec::with_capacity(against.len());
    for (j, (r, &at)) in against.iter().zip(&offsets).enumerate() {
        let number = j + 1;
        let mut placements = r.sources.iter().enumerate().map(|(s, source)| {
            let stated = &header.sources[at as usize + s];
            // stated = source × stretch: the merge's stretch of the input.
            let (sn, sd) = reduced(stated.stretch.unwrap_or(Ratio::ONE));
            let (on, od) = reduced(source.stretch.unwrap_or(Ratio::ONE));
            let stretch = ratio_of(sn * od, sd * on)?;
            let q = axis_ratio(stretch, timebase, r.timebase)?;
            let moved = i128::from(stated.origin) - scaled(source.origin, q);
            let align =
                i128::from(stated.align.unwrap_or(0)) - scaled(source.align.unwrap_or(0), q);
            Some((stretch, q, moved, align))
        });
        let unplaced = || Error::Stretch {
            input: number,
            stretch: String::new(),
        };
        let first = placements.next().flatten().ok_or_else(unplaced)?;
        for other in placements {
            let other = other.ok_or_else(unplaced)?;
            if other != first {
                return Err(Error::Disagree {
                    input: number,
                    moves: ((first.2, first.3), (other.2, other.3)),
                });
            }
        }
        let (stretch, q, moved, align) = first;
        inputs.push(InputMap {
            q,
            stretch,
            align: within(|| format!("input {number}'s align"), align)?,
            moved: within(|| format!("input {number}'s move"), moved)?,
        });
    }
    let mut channels = Vec::with_capacity(header.channels.len());
    let mut length = 1i128;
    // Each input channel goes into the merged recording once: `apply` takes it.
    let mut taken: Vec<Vec<bool>> = against
        .iter()
        .map(|r| vec![false; r.channels.len()])
        .collect();
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
        if std::mem::replace(&mut taken[j][ic], true) {
            return Err(Error::TakenTwice {
                input: j + 1,
                stem: describe(&r.sources[s]),
                name: input_channel.name.clone(),
            });
        }
        let kept = c.shaping.kept.ok_or_else(|| Error::NoKept {
            name: c.name.clone(),
        })?;
        let (from, to) = (kept.from, kept.to);
        let span = Span {
            from: Some(from),
            to: Some(to),
        };
        let own = input_mapping(r, input_channel)
            .map(|map| own_span(r, input_channel, &map))
            .ok_or_else(|| Error::Stretch {
                input: j + 1,
                stretch: String::new(),
            })?;
        let samples = match &input_channel.data {
            ChannelData::Digital(_) => None,
            ChannelData::Analog(a) => {
                let cut =
                    AnalogCut::new(a, (from > own.0).then_some(from), to).ok_or_else(|| {
                        Error::GridChanges {
                            name: c.name.clone(),
                            span,
                        }
                    })?;
                Some((cut.skip, cut.keep))
            }
        };
        let (unit, edges) = match &c.data {
            ChannelKind::Analog { unit, .. } => (Some(unit.clone()), None),
            ChannelKind::Digital { edges, .. } => (None, Some(*edges)),
            ChannelKind::Other { kind } => {
                return Err(Error::UnknownKind {
                    name: c.name.clone(),
                    kind: kind.clone(),
                });
            }
        };
        // A digital input channel takes its count from the header, which states none for one it
        // calls analog.
        if unit.is_some() && matches!(input_channel.data, ChannelData::Digital(_)) {
            return Err(Error::UnitOnDigital(c.name.clone()));
        }
        // The channel's end on the merged axis, by the header's own mapping.
        let end = header
            .mapping(channels.len())
            .and_then(|m| m.axis(to))
            .unwrap_or(i64::MAX);
        length = length.max(i128::from(end));
        channels.push(ChannelMap {
            input: j,
            channel: ic,
            name: c.name.clone(),
            source: c.source,
            from,
            to,
            cut: from > own.0 || to < own.1,
            samples,
            edges,
            shaping: c.shaping.clone(),
            unit,
        });
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
            channels.iter().find(|c| c.name == name).map(|c| {
                (
                    c.from,
                    c.to,
                    c.samples,
                    c.shaping.offset.as_ref().map(|o| o.ticks),
                )
            })
        };
        let pair = |i: usize| d.operands.get(i).cloned().unwrap_or_default();
        let first = d.operands.first().map(|o| layout_of(o));
        if !d.operands.iter().all(|o| Some(layout_of(o)) == first) {
            return Err(Error::DerivedApart {
                name: d.name.clone(),
                a: pair(0),
                b: pair(1),
            });
        }
        let operands: Option<Vec<(usize, &str)>> = d.operands.iter().map(|o| operand(o)).collect();
        let found = match operands {
            Some(ops) if !ops.is_empty() && ops.iter().all(|(j, _)| *j == ops[0].0) => {
                against[ops[0].0].derived.iter().any(|e| {
                    e.op == d.op
                        && e.params == d.params
                        && e.unit == d.unit
                        && e.operands
                            .iter()
                            .map(String::as_str)
                            .eq(ops.iter().map(|o| o.1))
                })
            }
            _ => false,
        };
        if !found {
            return Err(Error::NoDerived {
                name: d.name.clone(),
                a: pair(0),
                b: pair(1),
                unit: d.unit.clone(),
            });
        }
    }
    if length >= i128::from(JSON_SAFE) {
        return Err(Error::Length(length));
    }
    let layout = Layout {
        timebase,
        length: length as i64,
        inputs,
        channels,
        derived: header.derived.iter().map(|d| d.declared()).collect(),
    };
    Ok(apply(&layout, against))
}
