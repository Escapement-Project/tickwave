//! A view's question of a recording and its answer: each channel's columns over the window and a
//! width either side, single samples once there are few enough, the overview strip, the
//! statistics and the markers.

use std::ops::Range;
use std::sync::atomic::AtomicBool;

use escapement_tickwave_format::model::sample_support;

use crate::columns::{
    self, AnalogOverview, DigitalColumns, DigitalOverview, Edges, Grid, Samples, State,
};
use crate::markers::{Marker, MarkerCursor, Page};
use crate::recording::{Error, Place, Recording};
use crate::runs::lost_spans_in;
use crate::spans::known_spans;
use crate::statistics::Statistics;

/// What a view shows and asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// On screen, in ticks.
    pub window: Range<i64>,
    /// Device pixels across the plot.
    pub columns: u32,
    /// Drawn: analog traces and digital lanes, by channel index.
    pub channels: Vec<usize>,
    /// (analog, digital): an analog channel's single samples are flagged where the digital one
    /// is high at their tick.
    pub flags: Vec<(usize, usize)>,
    /// The overview strip: a channel, and its columns over the extent.
    pub strip: Option<(usize, u32)>,
    /// Intervals to measure: the window's, the selection's.
    pub statistics: Vec<Range<i64>>,
    pub markers: MarkerQuery,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkerQuery {
    /// At most this many of the window's markers.
    pub limit: usize,
    /// A page of the list of every marker: from after a cursor, at most so many.
    pub list: Option<(Option<MarkerCursor>, usize)>,
    /// The marker after this one.
    pub successor_of: Option<MarkerCursor>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarkerAnswer {
    /// The markers that start in the window.
    pub window: Page,
    pub list: Option<Page>,
    /// The cursor asked about, and the marker after it.
    pub successor: Option<(MarkerCursor, Option<Marker>)>,
}

/// Each column's range of an analog channel, and how it draws.
#[derive(Clone, Debug, PartialEq)]
pub struct AnalogColumns {
    /// The smallest and largest value the trace passes through in each column; +∞ and −∞ where
    /// it passes through none.
    pub min: Vec<f64>,
    pub max: Vec<f64>,
    /// Whether the trace comes into the column from the one before it: false across a break,
    /// and where either holds nothing.
    pub joined: Vec<bool>,
    /// Whether the column touches a span the instrument lost.
    pub pale: Vec<bool>,
}

/// One sample, drawn on its own once a view holds few enough.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub tick: i64,
    /// Where its time ends: the next sample's tick inside a run.
    pub end: i64,
    pub value: f64,
    /// The query's flag channel is high at its tick.
    pub flagged: bool,
    /// A line joins it to the sample before: false for a run's first sample and the first
    /// point.
    pub joined: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Drawn {
    Analog {
        columns: AnalogColumns,
        /// Every sample `around` holds, and one either side, when it holds no more than there
        /// are columns.
        samples: Option<Vec<Point>>,
        /// The spans the instrument lost, shaded. A pause is a break with no span.
        lost: Vec<Range<i64>>,
        /// The smallest and largest value over the window's columns, or its samples where there
        /// are any.
        fit: Option<(f64, f64)>,
    },
    Digital {
        columns: DigitalColumns,
        /// Where the level wasn't known, shaded.
        gaps: Vec<Range<i64>>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct StripAnswer {
    pub extent: Range<i64>,
    /// Over the extent, the strip's columns of it.
    pub grid: Grid,
    pub channel: Drawn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub window: Range<i64>,
    /// The window and one width either side: what the columns cover, so a pan has columns to
    /// draw while its own answer is on the way.
    pub around: Range<i64>,
    /// Over `around`, three times the query's columns.
    pub grid: Grid,
    /// In the query's order.
    pub channels: Vec<Drawn>,
    pub strip: Option<StripAnswer>,
    pub statistics: Vec<Statistics>,
    pub markers: MarkerAnswer,
    pub extent: Option<Range<i64>>,
}

/// The columns of `g` that any tick of [from, to) falls in.
fn columns_over(g: &Grid, from: i64, to: i64) -> Range<usize> {
    let c0 = ((from as f64 - g.x0) / g.per).floor().max(0.0);
    let c1 = ((to as f64 - g.x0) / g.per).ceil().min(g.count as f64);
    if c1 <= c0 || c1.is_nan() || c0.is_nan() {
        return 0..0;
    }
    c0 as usize..c1 as usize
}

/// The level a digital column starts at and ends at, and the edges it holds: what two pieces of
/// one column, side by side, combine by.
fn ends(s: State) -> Option<(bool, bool, u8)> {
    Some(match s {
        State::Low => (false, false, 0),
        State::High => (true, true, 0),
        State::Rise => (false, true, 1),
        State::Fall => (true, false, 1),
        State::Busy | State::None => return None,
    })
}

/// A column's state from its left part, `a`, and its right part, `b`, each with where its one
/// edge sits. Where the right part doesn't start at the level the left ends at, or either is
/// busy, the column is busy.
fn merge(a: (State, f64), b: (State, f64)) -> (State, f64) {
    match (a.0, b.0) {
        (State::None, _) => b,
        (_, State::None) => a,
        _ => match (ends(a.0), ends(b.0)) {
            (Some((start, mid, n)), Some((mid2, _, m))) if mid == mid2 => match n + m {
                0 => (if start { State::High } else { State::Low }, 0.0),
                1 => (
                    if start { State::Fall } else { State::Rise },
                    if n == 1 { a.1 } else { b.1 },
                ),
                _ => (State::Busy, 0.0),
            },
            _ => (State::Busy, 0.0),
        },
    }
}

impl Recording {
    /// The view `q` asks for. Cancel is checked between blocks; a cancelled query returns
    /// `Error::Cancelled`.
    #[tracing::instrument(level = "debug", name = "query", skip_all, fields(columns = q.columns, ticks = q.window.end - q.window.start))]
    pub fn query(&self, q: &Query, cancel: &AtomicBool) -> Result<Answer, Error> {
        let width = (q.window.end - q.window.start).max(1);
        let window = q.window.start..q.window.start + width;
        let around = window.start - width..window.end + width;
        let columns = q.columns.max(1) as usize;
        let grid = Grid {
            x0: around.start as f64,
            per: width as f64 / columns as f64,
            count: 3 * columns,
        };
        let mut channels = Vec::with_capacity(q.channels.len());
        for &c in &q.channels {
            let flag = q.flags.iter().find(|f| f.0 == c).map(|f| f.1);
            channels.push(self.drawn(c, &grid, &around, Some(&window), flag, cancel)?);
        }
        let strip = match (q.strip, self.extent()) {
            (Some((c, n)), Some(extent)) => {
                let _span = tracing::debug_span!("strip").entered();
                let n = n.max(1) as usize;
                let grid = Grid {
                    x0: extent.start as f64,
                    per: (extent.end - extent.start) as f64 / n as f64,
                    count: n,
                };
                let channel = self.drawn(c, &grid, &extent, None, None, cancel)?;
                Some(StripAnswer {
                    extent,
                    grid,
                    channel,
                })
            }
            _ => None,
        };
        let statistics = q
            .statistics
            .iter()
            .map(|r| self.statistics(r.clone(), cancel))
            .collect::<Result<Vec<_>, _>>()?;
        let markers = MarkerAnswer {
            window: self.markers(window.clone(), None, q.markers.limit),
            list: q
                .markers
                .list
                .map(|(after, limit)| self.markers(i64::MIN..i64::MAX, after, limit)),
            successor: q.markers.successor_of.map(|c| (c, self.marker_after(c))),
        };
        Ok(Answer {
            window,
            around,
            grid,
            channels,
            strip,
            statistics,
            markers,
            extent: self.extent(),
        })
    }

    /// Channel `c` over `range` on `g`. With a `window`, an analog channel's single samples
    /// where there are few enough, and its fit over the window.
    #[tracing::instrument(level = "debug", name = "channel", skip_all, fields(channel = c))]
    fn drawn(
        &self,
        c: usize,
        g: &Grid,
        range: &Range<i64>,
        window: Option<&Range<i64>>,
        flag: Option<usize>,
        cancel: &AtomicBool,
    ) -> Result<Drawn, Error> {
        let place = self.place(c)?;
        let spans = known_spans(&place.known(), range.start, range.end);
        let level = self.level_for(g.per);
        match place {
            Place::Digital { gaps, .. } => {
                let mut cols = DigitalColumns::empty(g.count);
                let mut piece = DigitalColumns::empty(g.count);
                for span in &spans {
                    self.digital_span(c, g, span.clone(), level, &mut cols, &mut piece, cancel)?;
                }
                // The gaps `range` reaches: they lie in order and apart.
                let first = gaps.partition_point(|x| x.to <= range.start);
                let gaps = gaps[first..]
                    .iter()
                    .take_while(|x| x.from < range.end)
                    .map(|x| x.from..x.to)
                    .collect();
                Ok(Drawn::Digital {
                    columns: cols,
                    gaps,
                })
            }
            Place::Analog { .. } => {
                let runs = place.runs().expect("analog");
                let mut cols = AnalogColumns {
                    min: vec![f64::INFINITY; g.count],
                    max: vec![f64::NEG_INFINITY; g.count],
                    joined: vec![false; g.count],
                    pale: vec![false; g.count],
                };
                let mut part = columns::AnalogColumns::empty(g.count);
                for span in &spans {
                    // The columns the span reaches: its trace joins nothing outside them.
                    let ks = columns_over(g, span.start, span.end);
                    part.min[ks.clone()].fill(f64::INFINITY);
                    part.max[ks.clone()].fill(f64::NEG_INFINITY);
                    self.analog_span(c, g, span.clone(), level, &mut part, cancel)?;
                    columns::connect_over(&mut part, ks.clone());
                    let full = |k: usize| part.min[k] <= part.max[k];
                    for k in ks.clone() {
                        if full(k) {
                            cols.min[k] = cols.min[k].min(part.min[k]);
                            cols.max[k] = cols.max[k].max(part.max[k]);
                            if k > ks.start && full(k - 1) {
                                cols.joined[k] = true;
                            }
                        }
                    }
                }
                let lost: Vec<Range<i64>> = lost_spans_in(&runs, range.clone())
                    .into_iter()
                    .filter(|l| l.to > l.from)
                    .map(|l| l.from..l.to)
                    .collect();
                for l in &lost {
                    for k in columns_over(g, l.start, l.end) {
                        cols.pale[k] = true;
                    }
                }
                let samples = match window {
                    Some(_) => self.points(c, g, range, flag, cancel)?,
                    None => None,
                };
                let fit = window.and_then(|w| match &samples {
                    Some(points) => {
                        let shown = points
                            .iter()
                            .filter(|p| p.end > w.start && p.tick < w.end && !p.value.is_nan());
                        shown.fold(None, |f: Option<(f64, f64)>, p| {
                            Some(f.map_or((p.value, p.value), |(lo, hi)| {
                                (lo.min(p.value), hi.max(p.value))
                            }))
                        })
                    }
                    None => columns_over(g, w.start, w.end)
                        .filter(|&k| cols.min[k] <= cols.max[k])
                        .fold(None, |f, k| {
                            Some(
                                f.map_or((cols.min[k], cols.max[k]), |(lo, hi): (f64, f64)| {
                                    (lo.min(cols.min[k]), hi.max(cols.max[k]))
                                }),
                            )
                        }),
                });
                Ok(Drawn::Analog {
                    columns: cols,
                    samples,
                    lost,
                    fit,
                })
            }
        }
    }

    /// Widens `part` with analog channel `c`'s ranges over [from, to): from level `level` where
    /// it holds the span, each finer level for what it doesn't, and the samples past the finest.
    fn analog_span(
        &self,
        c: usize,
        g: &Grid,
        span: Range<i64>,
        level: Option<usize>,
        part: &mut columns::AnalogColumns,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        let mut at = span.start;
        let mut level = level;
        while at < span.end {
            match level {
                Some(l) => {
                    let shift = self.levels[l].shift;
                    let covered = self.levels[l].end();
                    let to = span.end.min(covered);
                    if to > at {
                        let (k0, k1) = (at >> shift, ((to - 1) >> shift) + 1);
                        let b = self.analog_buckets(c, l, k0 as u64, k1 as u64, cancel)?;
                        let ov = AnalogOverview {
                            bucket: 1 << shift,
                            start: k0 << shift,
                            min: &b.min,
                            max: &b.max,
                        };
                        columns::widen_from_overview(g, &ov, at as f64, to as f64, part);
                        at = to;
                    }
                    level = l.checked_sub(1);
                }
                None => {
                    let runs = self.place(c)?.runs().expect("analog");
                    let (i0, i1) = (self.index_at(c, at)?, self.index_at(c, span.end)?);
                    let values = self.values(c, i0, i1, cancel)?;
                    let s = Samples {
                        first: i0,
                        values: &values,
                        runs: None,
                    };
                    columns::widen_from_samples(g, &s, &runs, at as f64, span.end as f64, part);
                    at = span.end;
                }
            }
        }
        Ok(())
    }

    /// Sets digital channel `c`'s columns over [from, to), as `analog_span` does: each piece's
    /// columns combined with what the pieces before it left. `piece` is room for one piece's
    /// columns, as wide as `cols`.
    #[expect(
        clippy::too_many_arguments,
        reason = "a recording's span, its grid and two buffers"
    )]
    fn digital_span(
        &self,
        c: usize,
        g: &Grid,
        span: Range<i64>,
        level: Option<usize>,
        cols: &mut DigitalColumns,
        piece: &mut DigitalColumns,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        let mut at = span.start;
        let mut level = level;
        // Only the columns the piece reaches: the others hold another piece's, or nothing.
        let put = |reached: Range<usize>, piece: &DigitalColumns, cols: &mut DigitalColumns| {
            for k in reached {
                let (s, a) = merge((cols.state[k], cols.at[k]), (piece.state[k], piece.at[k]));
                cols.state[k] = s;
                cols.at[k] = a;
            }
        };
        while at < span.end {
            match level {
                Some(l) => {
                    let shift = self.levels[l].shift;
                    let covered = self.levels[l].end();
                    let to = span.end.min(covered);
                    if to > at {
                        let (k0, k1) = (at >> shift, ((to - 1) >> shift) + 1);
                        let (levels, changes) =
                            self.digital_buckets(c, l, k0 as u64, k1 as u64, cancel)?;
                        let ov = DigitalOverview {
                            bucket: 1 << shift,
                            start: k0 << shift,
                            level: &levels,
                            changes: &changes,
                        };
                        let reached = columns::digital_from_overview_into(
                            g, &ov, at as f64, to as f64, piece,
                        );
                        put(reached, piece, cols);
                        at = to;
                    }
                    level = l.checked_sub(1);
                }
                None => {
                    let (initial, ticks) = self.edges(c, at, span.end, cancel)?;
                    let edges = Edges {
                        level: initial,
                        ticks: &ticks,
                    };
                    let reached = columns::digital_from_edges_into(
                        g,
                        &edges,
                        at as f64,
                        span.end as f64,
                        piece,
                    );
                    put(reached, piece, cols);
                    at = span.end;
                }
            }
        }
        Ok(())
    }

    /// Channel `c`'s samples over `range` and one either side, when `range` holds no more than
    /// `g` has columns.
    fn points(
        &self,
        c: usize,
        g: &Grid,
        range: &Range<i64>,
        flag: Option<usize>,
        cancel: &AtomicBool,
    ) -> Result<Option<Vec<Point>>, Error> {
        let place = self.place(c)?;
        let (Some(runs), Place::Analog { count, .. }) = (place.runs(), place) else {
            return Ok(None);
        };
        let (i0, i1) = (self.index_at(c, range.start)?, self.index_at(c, range.end)?);
        if i1 - i0 > g.count as u64 {
            return Ok(None);
        }
        let lo = i0.saturating_sub(1);
        let hi = (i1 + 1).min(*count);
        let values = self.values(c, lo, hi, cancel)?;
        let ticks: Vec<i64> = (lo..hi).map(|i| runs.tick(i)).collect();
        let flagged = match (flag, ticks.first(), ticks.last()) {
            (Some(f), Some(&a), Some(&b)) => self.levels_at(f, &ticks, a, b + 1, cancel)?,
            _ => vec![false; ticks.len()],
        };
        Ok(Some(
            (lo..hi)
                .enumerate()
                .map(|(k, i)| Point {
                    tick: ticks[k],
                    end: sample_support(
                        runs.first,
                        runs.period,
                        runs.breaks,
                        *count,
                        self.length,
                        i,
                    )
                    .expect("a sample")
                    .end,
                    value: values[k],
                    flagged: flagged[k],
                    joined: i > lo && runs.breaks.binary_search_by_key(&i, |b| b.index).is_err(),
                })
                .collect(),
        ))
    }

    /// Whether digital channel `c` is high at each of `ticks`, all in [from, to) and in order;
    /// false where its level isn't known.
    fn levels_at(
        &self,
        c: usize,
        ticks: &[i64],
        from: i64,
        to: i64,
        cancel: &AtomicBool,
    ) -> Result<Vec<bool>, Error> {
        let Place::Digital {
            start, end, gaps, ..
        } = self.place(c)?
        else {
            return Err(Error::NoChannel(c));
        };
        let (level, edges) = self.edges(c, from, to, cancel)?;
        // The level from `from`, flipped at each edge, and set by each gap's end it passes.
        let mut out = Vec::with_capacity(ticks.len());
        let (mut l, mut e) = (level, 0);
        let mut g = gaps.partition_point(|x| x.to <= from);
        for &t in ticks {
            while e < edges.len() || g < gaps.len() {
                let edge = edges.get(e).copied().filter(|&x| x <= t);
                let gap_end = gaps.get(g).map(|x| x.to).filter(|&x| x <= t);
                match (edge, gap_end) {
                    (Some(x), Some(y)) if y < x => {
                        l = gaps[g].level;
                        g += 1;
                    }
                    (Some(_), _) => {
                        l = !l;
                        e += 1;
                    }
                    (None, Some(_)) => {
                        l = gaps[g].level;
                        g += 1;
                    }
                    (None, None) => break,
                }
            }
            // Gaps lie in order and apart: the one that could hold `t` is the last starting at
            // or before it.
            let holding = gaps
                .partition_point(|x| x.from <= t)
                .checked_sub(1)
                .is_some_and(|i| t < gaps[i].to);
            let known = t >= *start && t < *end && !holding;
            out.push(known && l);
        }
        Ok(out)
    }
}
