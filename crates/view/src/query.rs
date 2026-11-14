//! A view's question of a recording and its answer: each channel's columns over the window and a
//! width either side, single samples once there are few enough, the overview strip, the
//! statistics and the markers.

use std::ops::Range;
use std::sync::atomic::AtomicBool;

use crate::columns::{
    self, AnalogOverview, DigitalColumns, DigitalOverview, Edges, Grid, Samples, State,
};
use crate::markers::{Marker, MarkerCursor, Page};
use crate::recording::{Error, Place, Recording, Revision};
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
    /// Columns at fixed ticks, for a window that follows a recording as it is written: each
    /// ⌈width / columns⌉ ticks from tick 0, the window's end moved on to the first column end at
    /// or after it, and `around` from two windows of columns before the last multiple of
    /// `columns` columns at or before that end, to a window after it. A later window as wide
    /// keeps the columns, which `Recording::extend` reuses. Without it, `around` is the window
    /// and a width either side, in `3 × columns` columns.
    pub aligned: bool,
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
    /// The window and at least a width either side: what the columns cover, so a pan has
    /// columns to draw while its own answer is on the way.
    pub around: Range<i64>,
    /// Over `around`: three times the query's columns, or up to four for `Query::aligned`.
    pub grid: Grid,
    /// In the query's order.
    pub channels: Vec<Drawn>,
    pub strip: Option<StripAnswer>,
    pub statistics: Vec<Statistics>,
    pub markers: MarkerAnswer,
    pub extent: Option<Range<i64>>,
    pub revision: Revision,
}

/// A query's window, `around`, and the grid over it.
struct Layout {
    window: Range<i64>,
    around: Range<i64>,
    grid: Grid,
    aligned: bool,
}

/// Ticks this far from 0 and the sums of a few of them are whole in an f64, so an aligned grid's
/// column edges come out in f64 as they do in integers.
const EXACT: i128 = 1 << 52;

fn layout(q: &Query) -> Layout {
    let width = q.window.end.saturating_sub(q.window.start).max(1);
    let columns = q.columns.max(1) as usize;
    let end = q.window.start.saturating_add(width);
    if q.aligned
        && let Some(l) = aligned(end, width, columns)
    {
        return l;
    }
    let window = q.window.start..end;
    let around = window.start.saturating_sub(width)..window.end.saturating_add(width);
    Layout {
        grid: Grid {
            x0: around.start as f64,
            per: width as f64 / columns as f64,
            count: 3 * columns,
        },
        window,
        around,
        aligned: false,
    }
}

/// `Query::aligned`'s layout for a window `width` ticks wide ending at `end`, worked in
/// integers; None where it reaches past `EXACT`.
fn aligned(end: i64, width: i64, columns: usize) -> Option<Layout> {
    let (w, c) = (i128::from(width), columns as i128);
    let per = (w + c - 1) / c;
    // The column the window ends at, ⌈end / per⌉, and the first of `around`'s.
    let last = -(-i128::from(end)).div_euclid(per);
    let first = (last.div_euclid(c) - 2) * c;
    let (from, to) = (first * per, (last + c) * per);
    if from < -EXACT || to > EXACT {
        return None;
    }
    let end = last * per;
    Some(Layout {
        window: (end - w) as i64..end as i64,
        around: from as i64..to as i64,
        grid: Grid {
            x0: from as f64,
            per: per as f64,
            count: (last + c - first) as usize,
        },
        aligned: true,
    })
}

/// What an extend keeps of one channel: the held answer's columns before `from`, on the same
/// grid.
#[derive(Clone, Copy)]
struct Keep<'a> {
    held: &'a Drawn,
    from: usize,
}

impl Drawn {
    /// Whether each of its column lists holds `count` columns.
    fn holds(&self, count: usize) -> bool {
        match self {
            Self::Analog { columns: c, .. } => {
                [c.min.len(), c.max.len(), c.joined.len(), c.pale.len()]
                    .iter()
                    .all(|&n| n == count)
            }
            Self::Digital { columns: c, .. } => c.state.len() == count && c.at.len() == count,
        }
    }
}

/// Where column `k` of an aligned grid starts.
fn start_of(g: &Grid, k: usize) -> i64 {
    (g.x0 + k as f64 * g.per) as i64
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
        self.answer(q, layout(q), &[], cancel)
    }

    /// What `query` answers, worked out from `held`, this recording's answer to `asked`: the
    /// columns before the first tick any snapshot since changed are kept, and the rest is worked
    /// out as `query` would, statistics whole. Both queries are aligned, and `query` asks what
    /// `asked` did, its window moved later by at most its width; their statistics may differ.
    /// None where it can't, for a whole query instead: `query` asks anything else, `held` is
    /// another recording's or older than the changes it keeps, or `around`'s start moved, since
    /// a column's range is joined to the one before it back to there.
    #[tracing::instrument(level = "debug", name = "extend", skip_all, fields(columns = query.columns, ticks = query.window.end - query.window.start))]
    pub fn extend(
        &self,
        held: &Answer,
        asked: &Query,
        query: &Query,
        cancel: &AtomicBool,
    ) -> Result<Option<Answer>, Error> {
        let same = Query {
            window: query.window.clone(),
            statistics: query.statistics.clone(),
            ..asked.clone()
        } == *query;
        let (before, now) = (layout(asked), layout(query));
        let width = before.window.end - before.window.start;
        let moved = now.window.start - before.window.start;
        if !same
            || !before.aligned
            || !now.aligned
            || (held.window.clone(), held.around.clone(), held.grid)
                != (before.window.clone(), before.around.clone(), before.grid)
            || now.window.end - now.window.start != width
            || !(0..=width).contains(&moved)
            || now.around.start != before.around.start
            || held.channels.len() != query.channels.len()
        {
            return Ok(None);
        }
        let Some(changed) = self.changed_since(held.revision) else {
            return Ok(None);
        };
        let mut keep = Vec::with_capacity(query.channels.len());
        for (&c, drawn) in query.channels.iter().zip(&held.channels) {
            let kind = matches!(
                (self.place(c)?, drawn),
                (Place::Digital { .. }, Drawn::Digital { .. })
                    | (Place::Analog { .. }, Drawn::Analog { .. })
            );
            if !kind || !drawn.holds(before.grid.count) {
                return Ok(None);
            }
            // The column holding the first tick that changed is worked out again, and those after.
            let first = i128::from(changed.from(c, now.grid.per)) - i128::from(now.around.start);
            let column = first.div_euclid(now.grid.per as i128);
            keep.push(Keep {
                held: drawn,
                from: column.clamp(0, before.grid.count.min(now.grid.count) as i128) as usize,
            });
        }
        self.answer(query, now, &keep, cancel).map(Some)
    }

    /// The answer to `q` over `layout`, each channel's columns from `keep`'s where it has one.
    fn answer(
        &self,
        q: &Query,
        layout: Layout,
        keep: &[Keep<'_>],
        cancel: &AtomicBool,
    ) -> Result<Answer, Error> {
        let Layout {
            window,
            around,
            grid,
            ..
        } = layout;
        let mut channels = Vec::with_capacity(q.channels.len());
        for (i, &c) in q.channels.iter().enumerate() {
            let flag = q.flags.iter().find(|f| f.0 == c).map(|f| f.1);
            let keep = keep.get(i).copied();
            channels.push(self.drawn(c, &grid, &around, Some(&window), flag, keep, cancel)?);
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
                let channel = self.drawn(c, &grid, &extent, None, None, None, cancel)?;
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
            revision: self.revision,
        })
    }

    /// Channel `c` over `range` on `g`. With a `window`, an analog channel's single samples
    /// where there are few enough, and its fit over the window. With `keep`, on an aligned grid,
    /// the columns before its `from` are the held ones, and only data from there on is read for
    /// the columns.
    #[expect(
        clippy::too_many_arguments,
        reason = "a channel, its grid and range, what it draws with, and what it keeps"
    )]
    #[tracing::instrument(level = "debug", name = "channel", skip_all, fields(channel = c))]
    fn drawn(
        &self,
        c: usize,
        g: &Grid,
        range: &Range<i64>,
        window: Option<&Range<i64>>,
        flag: Option<usize>,
        keep: Option<Keep<'_>>,
        cancel: &AtomicBool,
    ) -> Result<Drawn, Error> {
        let place = self.place(c)?;
        let spans = known_spans(&place.known(), range.start, range.end);
        let level = self.level_for(g.per);
        match place {
            Place::Digital { gaps, .. } => {
                let mut cols = DigitalColumns::empty(g.count);
                let mut piece = DigitalColumns::empty(g.count);
                // A column's state is its own ticks' alone.
                let from = match keep {
                    Some(Keep {
                        held: Drawn::Digital { columns: h, .. },
                        from,
                    }) => {
                        cols.state[..from].copy_from_slice(&h.state[..from]);
                        cols.at[..from].copy_from_slice(&h.at[..from]);
                        from
                    }
                    _ => 0,
                };
                for span in &spans {
                    let span = match from {
                        0 => span.clone(),
                        k => span.start.max(start_of(g, k))..span.end,
                    };
                    if span.end > span.start {
                        self.digital_span(c, g, span, level, &mut cols, &mut piece, cancel)?;
                    }
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
                // The columns each span reaches: its trace joins nothing outside them.
                let reach: Vec<Range<usize>> = spans
                    .iter()
                    .map(|s| columns_over(g, s.start, s.end))
                    .collect();
                // A span crossing into `from` joins its range there to its own in the column
                // before, which the held one is where no other span reaches that column: back
                // off while the crossing span starts in it.
                let mut from = match keep {
                    Some(Keep {
                        held: Drawn::Analog { .. },
                        from,
                    }) => from,
                    _ => 0,
                };
                while let Some(ks) = reach.iter().find(|ks| ks.start < from && ks.end > from)
                    && ks.start + 1 == from
                {
                    from -= 1;
                }
                if let Some(Keep {
                    held: Drawn::Analog { columns: h, .. },
                    ..
                }) = keep
                {
                    cols.min[..from].copy_from_slice(&h.min[..from]);
                    cols.max[..from].copy_from_slice(&h.max[..from]);
                    cols.joined[..from].copy_from_slice(&h.joined[..from]);
                }
                for (span, ks) in spans.iter().zip(&reach) {
                    if ks.end <= from {
                        continue;
                    }
                    let crossing = ks.start < from;
                    let (span, ks_new, joins) = if crossing {
                        part.min[from - 1] = cols.min[from - 1];
                        part.max[from - 1] = cols.max[from - 1];
                        (start_of(g, from)..span.end, from..ks.end, from - 1..ks.end)
                    } else {
                        (span.clone(), ks.clone(), ks.clone())
                    };
                    part.min[ks_new.clone()].fill(f64::INFINITY);
                    part.max[ks_new.clone()].fill(f64::NEG_INFINITY);
                    self.analog_span(c, g, span, level, &mut part, cancel)?;
                    columns::connect_over(&mut part, joins);
                    let full = |k: usize| part.min[k] <= part.max[k];
                    for k in ks_new {
                        if full(k) {
                            cols.min[k] = cols.min[k].min(part.min[k]);
                            cols.max[k] = cols.max[k].max(part.max[k]);
                            if k > ks.start && full(k - 1) {
                                cols.joined[k] = true;
                            }
                        }
                    }
                }
                // Shaded whole, kept columns too: a break's lost span can reach back before the
                // first tick a snapshot records as changed, to its run's last sample.
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
                    end: runs.support(i, self.length).expect("a sample").end,
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
