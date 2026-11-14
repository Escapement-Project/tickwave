//! Moving and trimming channels at conversion (FORMAT.md, "Moved and trimmed channels"). A plan
//! takes each channel's offset and kept span in the source's time and works out where the
//! recording starts. Applied to a recording as a pass opens it, it cuts each channel as its items
//! stream through; the items keep their source ticks, and the move is the channel's `offset`.

use std::fmt;

use escapement_tickwave_format::annotations::{self, ExactTicks};
use escapement_tickwave_format::header::{Header, JSON_SAFE};
use escapement_tickwave_format::model::{
    Analog, Break, Channel, ChannelData, Digital, Gap, Kept, Offset, Ratio, Recording, Shaping,
    Wrap, run_sample_tick,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{what} for {name:?}: the recording has no channel by that name; it has {has:?}")]
    NoChannel {
        what: &'static str,
        name: String,
        has: Vec<String>,
    },
    #[error("{name:?} is given {what} twice")]
    Twice { what: &'static str, name: String },
    #[error("{name}: {problem}")]
    Time { name: String, problem: String },
    #[error(
        "{name}: an offset of {time:?} is {ticks:.3} ticks, more than 0.01 of a tick off the grid"
    )]
    OffGrid {
        name: String,
        time: String,
        ticks: f64,
    },
    #[error("{name}: an offset of {ticks} ticks is past 2^53")]
    OffsetRange { name: String, ticks: i64 },
    #[error("{name}: kept {span}: its from must come before its to")]
    Backwards { name: String, span: Span },
    #[error("{name}: kept {span}, which leaves nothing of the capture's ticks 0 to {length}")]
    Outside {
        name: String,
        span: Span,
        length: i64,
    },
    #[error(
        "{name}: kept {span}; its samples keep their ticks only from one a multiple of {q} into \
         its run, and none in that span is"
    )]
    NoGridSample { name: String, span: Span, q: u64 },
    #[error(
        "the recording's channels start {base} ticks into the source, but its source states an \
         origin of {origin}, not {}",
        -base
    )]
    Origin { origin: i64, base: i64 },
    #[error("the recording has {0} sources; a plan shapes a recording of one")]
    Sources(usize),
}

/// A kept span as given, in ticks, for a message: an end not given is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub from: Option<i64>,
    pub to: Option<i64>,
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.from, self.to) {
            (Some(from), Some(to)) => write!(f, "from tick {from} to {to}"),
            (Some(from), None) => write!(f, "from tick {from} to the end"),
            (None, Some(to)) => write!(f, "from the start to tick {to}"),
            (None, None) => f.write_str("whole"),
        }
    }
}

/// A part of a channel to keep, by the channel's name: from and to, each a time in the channel's
/// own source time, before its offset. None is an open end.
pub type Keep = (String, Option<String>, Option<String>);

/// Checks a time's syntax: a whole tick count, or a decimal with a unit (ANNOTATIONS.md, "Times"),
/// either with a leading `-`.
pub fn check_time(text: &str) -> Result<(), String> {
    exact(text, Ratio::ONE).map(drop)
}

pub(crate) fn exact(text: &str, timebase: Ratio) -> Result<ExactTicks, String> {
    let text = text.trim();
    match text.parse::<i64>() {
        Ok(ticks) => Ok(ExactTicks {
            negative: ticks < 0,
            numerator: i128::from(ticks.unsigned_abs()),
            denominator: 1,
        }),
        Err(_) => annotations::exact_ticks(text, timebase),
    }
}

pub(crate) fn rounded(text: &str, exact: &ExactTicks) -> Result<i64, String> {
    exact
        .rounded()
        .ok_or_else(|| format!("{:?}: out of range", text.trim()))
}

/// Whether `exact` lies more than 0.01 of a tick from `tick`, its nearest: in whole numbers,
/// 100 × |numerator − |tick| × denominator| against the denominator.
pub(crate) fn off_grid(exact: &ExactTicks, tick: i64) -> bool {
    i128::from(tick.unsigned_abs())
        .checked_mul(exact.denominator)
        .and_then(|at| exact.numerator.checked_sub(at))
        .and_then(|off| off.unsigned_abs().checked_mul(100))
        .is_none_or(|off| off > exact.denominator.unsigned_abs())
}

/// Each channel's move and cut, and where the recording starts: everything a pass needs to shape
/// a recording as it opens it.
#[derive(Clone, Debug)]
pub struct Plan {
    channels: Vec<ChannelPlan>,
    /// The tick of the source's grid where the recording starts.
    base: i64,
    length: i64,
    source_length: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ChannelPlan {
    /// Ticks the channel moves, positive later.
    offset: i64,
    /// Where the channel is cut, in its own source ticks; None at an end not cut. An analog
    /// channel's `from` has moved to its first sample where that keeps the samples' ticks.
    from: Option<i64>,
    to: Option<i64>,
    /// An analog channel's samples kept: how many to skip, then how many to keep.
    samples: Option<(u64, u64)>,
    /// A cut digital channel's edges kept, once `Plan::count` has read them.
    edges: Option<u64>,
}

impl Plan {
    /// Resolves `offsets` and `keeps` against `recording`: its channels' names and the sample
    /// rate its timebase gives the times.
    pub fn new(
        recording: &Recording,
        offsets: &[(String, String)],
        keeps: &[Keep],
    ) -> Result<Self, Error> {
        let timebase = recording.timebase;
        let length = recording.length;
        let has = || recording.channels.iter().map(|c| c.name.clone()).collect();
        let find = |what, name: &str| {
            recording
                .channels
                .iter()
                .position(|c| c.name == name)
                .ok_or_else(|| Error::NoChannel {
                    what,
                    name: name.to_owned(),
                    has: has(),
                })
        };
        let time_error = |name: &str| {
            let name = name.to_owned();
            move |problem| Error::Time { name, problem }
        };
        let mut plans: Vec<ChannelPlan> = recording
            .channels
            .iter()
            .map(|c| ChannelPlan {
                samples: match &c.data {
                    ChannelData::Analog(a) => Some((0, a.count)),
                    ChannelData::Digital(_) => None,
                },
                ..ChannelPlan::default()
            })
            .collect();

        let mut seen = vec![false; plans.len()];
        for (name, time) in offsets {
            let c = find("an offset", name)?;
            if std::mem::replace(&mut seen[c], true) {
                return Err(Error::Twice {
                    what: "an offset",
                    name: name.clone(),
                });
            }
            let exact = exact(time, timebase).map_err(time_error(name))?;
            let ticks = rounded(time, &exact).map_err(time_error(name))?;
            if off_grid(&exact, ticks) {
                let ticks = exact.numerator as f64 / exact.denominator as f64;
                return Err(Error::OffGrid {
                    name: name.clone(),
                    time: time.clone(),
                    ticks: if exact.negative { -ticks } else { ticks },
                });
            }
            if ticks.unsigned_abs() >= JSON_SAFE {
                return Err(Error::OffsetRange {
                    name: name.clone(),
                    ticks,
                });
            }
            plans[c].offset = ticks;
        }

        let mut seen = vec![false; plans.len()];
        for (name, from, to) in keeps {
            let c = find("a keep", name)?;
            if std::mem::replace(&mut seen[c], true) {
                return Err(Error::Twice {
                    what: "a keep",
                    name: name.clone(),
                });
            }
            let tick = |time: &Option<String>| {
                time.as_deref()
                    .map(|t| exact(t, timebase).and_then(|e| rounded(t, &e)))
                    .transpose()
                    .map_err(time_error(name))
            };
            let span = Span {
                from: tick(from)?,
                to: tick(to)?,
            };
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
            // An end outside the capture cuts nothing there.
            let from = span.from.filter(|&f| f > 0);
            let to = span.to.filter(|&t| t < length);
            if from.unwrap_or(0) >= to.unwrap_or(length) {
                return Err(Error::Outside {
                    name: name.clone(),
                    span,
                    length,
                });
            }
            let plan = &mut plans[c];
            (plan.from, plan.to) = (from, to);
            if let ChannelData::Analog(a) = &recording.channels[c].data {
                let cut = AnalogCut::new(a, from, to.unwrap_or(length)).ok_or_else(|| {
                    Error::NoGridSample {
                        name: name.clone(),
                        span,
                        q: grid(a.period),
                    }
                })?;
                plan.from = cut.from;
                plan.samples = Some((cut.skip, cut.keep));
            }
        }

        // Each channel's data starts at its cut, or the source's start, and ends at its cut, or
        // the source's length.
        let base = plans
            .iter()
            .map(|p| p.from.unwrap_or(0) + p.offset)
            .min()
            .unwrap_or(0);
        let shaped_length = plans
            .iter()
            .map(|p| p.to.unwrap_or(length) + p.offset - base)
            .max()
            .unwrap_or(length);
        Ok(Self {
            channels: plans,
            base,
            length: shaped_length,
            source_length: length,
        })
    }

    /// The plan that made the recording `header` describes from `source`, read back from its
    /// channels' `offset` and `kept`, both in the source's ticks, and its source's `origin`
    /// (FORMAT.md, "Times"): what `tickwave verify` checks a shaped recording through.
    /// `source`'s channels carry the recording's names. Call `count` before applying it to a
    /// recording whose digital channels it cuts.
    pub fn of_header(source: &Recording, header: &Header) -> Result<Self, Error> {
        let [only] = header.sources.as_slice() else {
            return Err(Error::Sources(header.sources.len()));
        };
        let origin = only.origin;
        let mut offsets = Vec::new();
        let mut keeps = Vec::new();
        for c in &header.channels {
            let offset = c.shaping.offset.as_ref().map_or(0, |o| o.ticks);
            if offset != 0 {
                offsets.push((c.name.clone(), offset.to_string()));
            }
            if let Some(kept) = c.shaping.kept {
                keeps.push((
                    c.name.clone(),
                    Some(kept.from.to_string()),
                    Some(kept.to.to_string()),
                ));
            }
        }
        let plan = Self::new(source, &offsets, &keeps)?;
        if plan.base != -origin {
            return Err(Error::Origin {
                origin,
                base: plan.base,
            });
        }
        Ok(plan)
    }

    /// The tick of the source's grid where the recording starts: 0 unless a channel moved or was
    /// cut.
    pub fn base(&self) -> i64 {
        self.base
    }

    /// Whether the plan moves and cuts nothing.
    pub fn is_whole(&self) -> bool {
        self.channels
            .iter()
            .all(|p| p.offset == 0 && p.from.is_none() && p.to.is_none())
    }

    /// Whether the plan cuts a digital channel, whose edges kept `count` must read before the
    /// plan applies.
    pub fn cuts_edges(&self) -> bool {
        self.channels
            .iter()
            .any(|p| p.samples.is_none() && (p.from.is_some() || p.to.is_some()))
    }

    /// Reads `recording`, the source the plan was made from, for each cut digital channel's
    /// edges in its kept span: the count the encoder holds the channel to.
    pub fn count(&mut self, recording: Recording) {
        for (p, c) in self.channels.iter_mut().zip(recording.channels) {
            if let (ChannelData::Digital(d), None, true) =
                (c.data, p.samples, p.from.is_some() || p.to.is_some())
            {
                let (from, to) = (p.from.unwrap_or(i64::MIN), p.to.unwrap_or(i64::MAX));
                p.edges = Some(d.edges.filter(|t| (from..to).contains(t)).count() as u64);
            }
        }
    }

    /// `recording`, which must be the source the plan was made from, moved and cut: its items keep
    /// their source ticks, cut as they are read. A digital channel cut at its start reads its
    /// edges up to the cut here, for its level there. A plan that cuts a digital channel must have
    /// counted it.
    pub fn apply(&self, recording: Recording) -> Recording {
        if self.is_whole() {
            return recording;
        }
        assert_eq!(
            (recording.length, recording.channels.len()),
            (self.source_length, self.channels.len()),
            "a plan applies to the source it was made from"
        );
        let Recording {
            timebase,
            length,
            channels,
            derived,
            mut sources,
        } = recording;
        // The recording starts `base` ticks into the source's grid: the source's tick 0 lies
        // `base` before it. Its own time at its tick 0, `begin_s`, stays.
        for source in &mut sources {
            source.origin -= self.base;
        }
        let channels = channels
            .into_iter()
            .zip(&self.channels)
            .map(|(c, p)| p.apply(c, length))
            .collect();
        Recording {
            timebase,
            length: self.length,
            channels,
            derived,
            sources,
        }
    }
}

impl ChannelPlan {
    /// The channel moved and cut. Once anything in the recording is, every channel states its
    /// kept span, so a reader can tell the ticks outside a channel's source data from ticks the
    /// instrument lost. `length` is the source's.
    fn apply(&self, channel: Channel, length: i64) -> Channel {
        let cut = self.from.is_some() || self.to.is_some();
        let (from, to) = (self.from.unwrap_or(0), self.to.unwrap_or(length));
        let shaping = Shaping {
            offset: (self.offset != 0).then(|| Offset {
                ticks: self.offset,
                kind: Offset::DELAY.into(),
            }),
            kept: Some(Kept { from, to }),
        };
        let data = if !cut {
            channel.data
        } else {
            match channel.data {
                ChannelData::Digital(d) => {
                    let count = self
                        .edges
                        .expect("a plan counts what it cuts before it applies");
                    ChannelData::Digital(digital(d, from, to, count))
                }
                ChannelData::Analog(a) => {
                    let (skip, keep) = self.samples.expect("an analog channel's plan");
                    ChannelData::Analog(analog(a, skip, keep))
                }
            }
        };
        Channel {
            data,
            shaping,
            ..channel
        }
    }
}

/// A digital channel kept at source ticks `from..to`, `count` edges there (FORMAT.md, "Moved and
/// trimmed channels"): it starts at `from`, its level there the one just before it, and it keeps
/// every edge in its span, one at `from` included. A gap the cut lies in starts the channel with a
/// gap from `from`. Its items keep their ticks.
pub(crate) fn digital(d: Digital, from: i64, to: i64, count: u64) -> Digital {
    let Digital {
        initial,
        end,
        edges,
        gaps,
        ..
    } = d;
    let mut edges = edges.peekable();
    let mut level = initial;
    // The level just before `from`: every edge before it, and every gap end at or before it. No
    // edge lies in a gap's `from..=to`, so the edges before a gap all come before its `from`.
    let mut next = 0;
    for g in gaps.iter().take_while(|g| g.to <= from) {
        while edges.next_if(|&t| t < g.from).is_some() {
            level = !level;
        }
        level = g.level;
        next += 1;
    }
    while edges.next_if(|&t| t < from).is_some() {
        level = !level;
    }
    let rest = gaps[next..]
        .iter()
        .take_while(|g| g.from < to)
        .map(|g| Gap {
            from: g.from.max(from),
            to: g.to,
            level: g.level,
        });
    // A gap that reaches the end ends what is known at its `from`, as a writer has it.
    let mut known = end.min(to).max(from);
    let mut kept = Vec::new();
    for g in rest {
        if g.to >= known {
            known = known.min(g.from);
            break;
        }
        kept.push(g);
    }
    let unknown_at_start = known == from || kept.first().is_some_and(|g| g.from == from);
    Digital {
        initial: level && !unknown_at_start,
        end: known,
        edges: Box::new(edges.take_while(move |&t| t < to)),
        count,
        gaps: kept,
    }
}

/// An analog channel with its first `skip` samples left out and the next `keep` kept. Its
/// samples keep their places: the first kept one's place is its `first`, and its breaks keep
/// their ticks.
pub(crate) fn analog(a: Analog, skip: u64, keep: u64) -> Analog {
    let end = skip + keep;
    let first = if keep == 0 {
        0
    } else {
        run_sample_tick(a.first, a.period, &a.breaks, skip)
    };
    let breaks = a
        .breaks
        .iter()
        .filter(|b| b.index > skip && b.index < end)
        .map(|b| Break {
            index: b.index - skip,
            tick: b.tick,
            lost: b.lost,
        })
        .collect();
    /// Skips `0` samples, then keeps `1`.
    struct Cut(u64, u64);
    impl Wrap for Cut {
        fn wrap<T: Send + 'static>(
            self,
            mut samples: Box<dyn Iterator<Item = T> + Send>,
        ) -> Box<dyn Iterator<Item = T> + Send> {
            // Counted in u64: a usize is 32 bits in the browser.
            let Cut(mut to_skip, mut to_keep) = self;
            Box::new(std::iter::from_fn(move || {
                while to_skip > 0 {
                    to_skip -= 1;
                    samples.next()?;
                }
                to_keep = to_keep.checked_sub(1)?;
                samples.next()
            }))
        }
    }
    Analog {
        unit: a.unit,
        first,
        period: a.period,
        count: keep,
        breaks,
        significant: a.significant,
        samples: a.samples.map(Cut(skip, keep)),
    }
}

/// `q` of a period `p/q` in lowest terms: a run's samples sit on its grid every `q` samples.
pub(crate) fn grid(period: Ratio) -> u64 {
    let (mut a, mut b) = (period.num(), period.den());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    period.den() / a
}

/// Where an analog channel kept at ticks `from..to` keeps its samples.
pub(crate) struct AnalogCut {
    pub(crate) skip: u64,
    pub(crate) keep: u64,
    /// The cut's start: `from`, or the first sample kept where the grid moved it there.
    pub(crate) from: Option<i64>,
}

impl AnalogCut {
    /// None when the grid moves the first sample kept past every sample before `to`, which the
    /// span would then claim without holding.
    pub(crate) fn new(a: &Analog, from: Option<i64>, to: i64) -> Option<Self> {
        let runs = runs(a);
        let end = first_at(&runs, a.period, to).1;
        let Some(cut) = from else {
            return Some(Self {
                skip: 0,
                keep: end,
                from,
            });
        };
        let (run, at) = first_at(&runs, a.period, cut);
        if at >= end {
            return Some(Self {
                skip: at,
                keep: 0,
                from,
            });
        }
        let q = grid(a.period);
        let run_end = runs.iter().find(|r| r.0 > run).map_or(a.count, |r| r.0);
        let on_grid = (run + (at - run).div_ceil(q) * q).min(run_end);
        if on_grid == at {
            return Some(Self {
                skip: at,
                keep: end - at,
                from,
            });
        }
        // The samples from `at` to `on_grid` are left out: the cut moves past them.
        (on_grid < end).then(|| Self {
            skip: on_grid,
            keep: end - on_grid,
            from: Some(run_sample_tick(a.first, a.period, &a.breaks, on_grid)),
        })
    }
}

/// An analog channel's runs: each one's first index, its tick, and its sample count.
fn runs(a: &Analog) -> Vec<(u64, i64, u64)> {
    let starts: Vec<(u64, i64)> = std::iter::once((0, a.first))
        .chain(a.breaks.iter().map(|b| (b.index, b.tick)))
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(i, &(index, tick))| {
            let next = starts.get(i + 1).map_or(a.count, |s| s.0);
            (index, tick, next - index)
        })
        .collect()
}

/// The first sample at or after `tick`, and the index of the run that holds it; the count, and
/// the last run's index, when every sample lies before it. Ticks never fall from one sample to
/// the next.
fn first_at(runs: &[(u64, i64, u64)], period: Ratio, tick: i64) -> (u64, u64) {
    for &(index, start, len) in runs {
        // floor(k × num / den) ≥ tick − start holds from k = ceil((tick − start) × den / num).
        let k = match tick - start {
            ..=0 => 0,
            after => (after as u128 * u128::from(period.den())).div_ceil(u128::from(period.num())),
        };
        if k < u128::from(len) {
            return (index, index + k as u64);
        }
    }
    let count = runs.last().map_or(0, |r| r.0 + r.2);
    (runs.last().map_or(0, |r| r.0), count)
}
