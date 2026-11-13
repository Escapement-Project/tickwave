//! What each pixel column of a row shows. Column c covers ticks [x0 + c × per, x0 + (c + 1) × per).
//! A port of web/src/render/columns.ts: the same f64 arithmetic, so a column takes the same
//! samples and edges as the web's does.

use std::ops::Range;

use crate::js;
use crate::runs::{Runs, index_at_or_after};

/// What a digital column shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum State {
    /// Outside the data drawn.
    #[default]
    None = 0,
    Low = 1,
    High = 2,
    /// More than one edge in the column, or the overview says it changes: both levels joined.
    Busy = 3,
    Rise = 4,
    Fall = 5,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DigitalColumns {
    pub state: Vec<State>,
    /// For `Rise` and `Fall`, where in the column the edge sits, from 0 to 1.
    pub at: Vec<f64>,
}

impl DigitalColumns {
    pub(crate) fn empty(count: usize) -> Self {
        Self {
            state: vec![State::None; count],
            at: vec![0.0; count],
        }
    }
}

/// The ticks a column grid covers; columns outside what is drawn get `State::None`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub x0: f64,
    pub per: f64,
    pub count: usize,
}

/// A digital channel's edges over a span: the level at its start, before any edge there, and
/// every edge in it, in order. Two can share a tick.
#[derive(Clone, Copy, Debug)]
pub struct Edges<'a> {
    pub level: bool,
    pub ticks: &'a [i64],
}

/// Buckets of `bucket` ticks, the first starting at `start`, a multiple of `bucket`.
#[derive(Clone, Copy, Debug)]
pub struct DigitalOverview<'a> {
    pub bucket: i64,
    pub start: i64,
    /// The level at each bucket's start.
    pub level: &'a [bool],
    /// Where the level changes inside the bucket.
    pub changes: &'a [bool],
}

#[derive(Clone, Copy, Debug)]
pub struct AnalogOverview<'a, T> {
    pub bucket: i64,
    pub start: i64,
    /// The smallest and largest value in each bucket; +∞ and −∞ in one with no sample.
    pub min: &'a [T],
    pub max: &'a [T],
}

/// A tile of an analog channel's samples.
#[derive(Clone, Copy, Debug)]
pub struct Samples<'a, T> {
    /// The channel's index of `values[0]`.
    pub first: u64,
    pub values: &'a [T],
    /// Each whole run of `SAMPLES_PER_RUN` samples' smallest and largest value, NaN left out,
    /// where the tile comes with them; a column reads them in place of the samples they cover.
    pub runs: Option<SampleRuns<'a>>,
}

#[derive(Clone, Copy, Debug)]
pub struct SampleRuns<'a> {
    pub min: &'a [f64],
    pub max: &'a [f64],
}

/// Samples a run of `SampleRuns` covers.
pub const SAMPLES_PER_RUN: usize = 64;

/// Sets runs `r0..r1` of `min` and `max` from `values`.
pub fn fill_runs<T: Copy + Into<f64>>(
    values: &[T],
    min: &mut [f64],
    max: &mut [f64],
    r0: usize,
    r1: usize,
) {
    for r in r0..r1 {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for &v in &values[r * SAMPLES_PER_RUN..(r + 1) * SAMPLES_PER_RUN] {
            let v = v.into();
            if v < lo {
                lo = v;
            }
            if v > hi {
                hi = v;
            }
        }
        min[r] = lo;
        max[r] = hi;
    }
}

fn column_range(g: &Grid, from: f64, to: f64) -> (usize, usize) {
    let c0 = js::max(0.0, ((from - g.x0) / g.per).floor());
    let c1 = js::min(g.count as f64, ((to - g.x0) / g.per).ceil());
    // A NaN bound draws nothing.
    if c0.is_nan() || c1.is_nan() {
        return (0, 0);
    }
    (c0 as usize, js::max(c0, c1) as usize)
}

/// The first index in `lo..` of `a` whose value is not less than `x`.
fn lower_bound(a: &[i64], x: f64, lo: usize) -> usize {
    lo + a[lo..].partition_point(|&t| (t as f64) < x)
}

/// Columns from exact edges fetched over [from, to). A column with no edge is flat, one with one
/// edge rises or falls where the edge is, one with more is busy.
pub fn digital_from_edges(g: &Grid, edges: &Edges, from: f64, to: f64) -> DigitalColumns {
    let mut cols = DigitalColumns::empty(g.count);
    digital_from_edges_into(g, edges, from, to, &mut cols);
    cols
}

/// `digital_from_edges` into `cols`, sized to `g`: it sets the columns [from, to) reaches, and
/// returns them, leaving the rest as they were.
pub(crate) fn digital_from_edges_into(
    g: &Grid,
    edges: &Edges,
    from: f64,
    to: f64,
    cols: &mut DigitalColumns,
) -> Range<usize> {
    let DigitalColumns { state, at } = cols;
    let t = edges.ticks;
    let (c0, c1) = column_range(g, from, to);
    let col_start = |c: usize| js::max(from, g.x0 + c as f64 * g.per);
    let mut i = lower_bound(t, col_start(c0), 0);
    let mut level = edges.level ^ (i % 2 == 1);
    for c in c0..c1 {
        let end = js::min(to, g.x0 + (c + 1) as f64 * g.per);
        let j = lower_bound(t, end, i);
        let n = j - i;
        at[c] = 0.0;
        state[c] = match n {
            0 if level => State::High,
            0 => State::Low,
            1 => {
                at[c] = (t[i] as f64 - (g.x0 + c as f64 * g.per)) / g.per;
                if level { State::Fall } else { State::Rise }
            }
            _ => State::Busy,
        };
        level ^= n % 2 == 1;
        i = j;
    }
    c0..c1
}

/// Columns from an overview's buckets, whatever their width against a column's. A column any
/// changing bucket overlaps is busy: the overview cannot say where in the bucket the change was,
/// or how many.
pub fn digital_from_overview(g: &Grid, ov: &DigitalOverview, from: f64, to: f64) -> DigitalColumns {
    let mut cols = DigitalColumns::empty(g.count);
    digital_from_overview_into(g, ov, from, to, &mut cols);
    cols
}

/// `digital_from_overview` into `cols`, as `digital_from_edges_into`.
pub(crate) fn digital_from_overview_into(
    g: &Grid,
    ov: &DigitalOverview,
    from: f64,
    to: f64,
    cols: &mut DigitalColumns,
) -> Range<usize> {
    let n = ov.level.len();
    let (start, bucket) = (ov.start as f64, ov.bucket as f64);
    let lo = js::max(from, start);
    let hi = js::min(to, start + n as f64 * bucket);
    let (c0, c1) = column_range(g, lo, hi);
    cols.at[c0..c1].fill(0.0);
    for (c, state) in cols.state.iter_mut().enumerate().take(c1).skip(c0) {
        let s = js::max(lo, g.x0 + c as f64 * g.per);
        let e = js::min(hi, g.x0 + (c + 1) as f64 * g.per);
        let k0 = js::max(0.0, ((s - start) / bucket).floor()) as usize;
        let k1 = js::min(n as f64, ((e - start) / bucket).ceil()) as usize;
        let busy = (k0..k1).any(|k| ov.changes.get(k).copied().unwrap_or(false));
        // Past the buckets reads as low, as the web's out-of-range read does.
        *state = if busy {
            State::Busy
        } else if ov.level.get(k0).copied().unwrap_or(false) {
            State::High
        } else {
            State::Low
        };
    }
    c0..c1
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnalogColumns {
    /// The smallest and largest value a column's trace passes through. A column with none holds
    /// +∞ and −∞, as an empty overview bucket does.
    pub min: Vec<f64>,
    pub max: Vec<f64>,
}

impl AnalogColumns {
    pub(crate) fn empty(count: usize) -> Self {
        Self {
            min: vec![f64::INFINITY; count],
            max: vec![f64::NEG_INFINITY; count],
        }
    }
}

/// Joins neighbouring columns: the trace passes through the gap between their ranges.
pub fn connect(cols: &mut AnalogColumns) {
    let n = cols.min.len();
    connect_over(cols, 0..n);
}

/// `connect` over the columns `range` alone.
pub(crate) fn connect_over(cols: &mut AnalogColumns, range: Range<usize>) {
    let AnalogColumns { min, max } = cols;
    for c in range.start + 1..range.end {
        if min[c] > max[c] || min[c - 1] > max[c - 1] {
            continue;
        }
        if max[c - 1] < min[c] {
            min[c] = max[c - 1];
        } else if min[c - 1] > max[c] {
            max[c] = min[c - 1];
        }
    }
}

/// `lo` down to `a` and `hi` up to `b`, where they are past them; a NaN moves neither.
fn widen(lo: &mut f64, hi: &mut f64, a: f64, b: f64) {
    if a < *lo {
        *lo = a;
    }
    if b > *hi {
        *hi = b;
    }
}

/// The tile's own index of its first sample at or after `tick`.
fn tile_index<T>(s: &Samples<T>, runs: &Runs, tick: f64) -> usize {
    let end = s.first + s.values.len() as u64;
    index_at_or_after(runs, tick)
        .min(end)
        .saturating_sub(s.first) as usize
}

/// Each column's range from the samples in it. For several samples a column.
pub fn analog_from_samples<T: Copy + Into<f64>>(
    g: &Grid,
    s: &Samples<T>,
    channel: &Runs,
    from: f64,
    to: f64,
) -> AnalogColumns {
    let mut cols = AnalogColumns::empty(g.count);
    widen_from_samples(g, s, channel, from, to, &mut cols);
    connect(&mut cols);
    cols
}

/// `analog_from_samples`'s ranges, widening `cols`' rather than replacing them, and not joined.
pub(crate) fn widen_from_samples<T: Copy + Into<f64>>(
    g: &Grid,
    s: &Samples<T>,
    channel: &Runs,
    from: f64,
    to: f64,
    cols: &mut AnalogColumns,
) {
    let v = s.values;
    let (c0, c1) = column_range(g, from, to);
    let at = |tick: f64| tile_index(s, channel, tick);
    let mut i = at(js::max(from, g.x0 + c0 as f64 * g.per));
    for c in c0..c1 {
        let j = i.max(at(js::min(to, g.x0 + (c + 1) as f64 * g.per)));
        // NaN samples are left out, as the overview leaves them out: a comparison with NaN is
        // false.
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        let mut k = i;
        let (r0, r1) = (i.div_ceil(SAMPLES_PER_RUN), j / SAMPLES_PER_RUN);
        // Without runs a column reads every sample: they give the same range.
        if let Some(runs) = s.runs
            && r0 < r1
        {
            for &x in &v[k..r0 * SAMPLES_PER_RUN] {
                widen(&mut lo, &mut hi, x.into(), x.into());
            }
            for r in r0..r1 {
                // A run the tile's runs don't hold counts for nothing, as the web's read of one
                // past the end does.
                if let (Some(&a), Some(&b)) = (runs.min.get(r), runs.max.get(r)) {
                    widen(&mut lo, &mut hi, a, b);
                }
            }
            k = r1 * SAMPLES_PER_RUN;
        }
        for &x in &v[k..j] {
            widen(&mut lo, &mut hi, x.into(), x.into());
        }
        widen(&mut cols.min[c], &mut cols.max[c], lo, hi);
        i = j;
    }
}

/// The samples a line through a tile's samples draws over a grid, `k0..k1` in the tile's own
/// indices: those in [from, to) that lie within `pad` ticks of the grid, and out to one that is
/// not NaN past each side, so the line leaves the grid on both.
pub fn line_samples<T: Copy + Into<f64>>(
    g: &Grid,
    s: &Samples<T>,
    channel: &Runs,
    from: f64,
    to: f64,
    pad: f64,
) -> (usize, usize) {
    let v = s.values;
    let nan = |k: usize| v[k].into().is_nan();
    let at = |tick: f64| tile_index(s, channel, tick);
    let (lo, hi) = (at(from), at(to));
    if hi <= lo {
        return (lo, lo);
    }
    let mut k0 = lo.max(at(g.x0 - pad).saturating_sub(1)).min(hi - 1);
    while k0 > lo && nan(k0) {
        k0 -= 1;
    }
    let mut k1 = hi
        .min(at(g.x0 + g.count as f64 * g.per + pad) + 1)
        .max(lo + 1);
    while k1 < hi && nan(k1 - 1) {
        k1 += 1;
    }
    (k0, k1)
}

/// Each column's range from the overview buckets that overlap it.
pub fn analog_from_overview<T: Copy + Into<f64>>(
    g: &Grid,
    ov: &AnalogOverview<T>,
    from: f64,
    to: f64,
) -> AnalogColumns {
    let mut cols = AnalogColumns::empty(g.count);
    widen_from_overview(g, ov, from, to, &mut cols);
    connect(&mut cols);
    cols
}

/// `analog_from_overview`'s ranges, widening `cols`' rather than replacing them, and not joined.
pub(crate) fn widen_from_overview<T: Copy + Into<f64>>(
    g: &Grid,
    ov: &AnalogOverview<T>,
    from: f64,
    to: f64,
    cols: &mut AnalogColumns,
) {
    let n = ov.min.len();
    let (start, bucket) = (ov.start as f64, ov.bucket as f64);
    let lo = js::max(from, start);
    let hi = js::min(to, start + n as f64 * bucket);
    let (c0, c1) = column_range(g, lo, hi);
    for c in c0..c1 {
        let s = js::max(lo, g.x0 + c as f64 * g.per);
        let e = js::min(hi, g.x0 + (c + 1) as f64 * g.per);
        let k0 = js::max(0.0, ((s - start) / bucket).floor()) as usize;
        let k1 = js::min(n as f64, ((e - start) / bucket).ceil()) as usize;
        for k in k0..k1 {
            // A bucket past the maxima reads as no maximum, as the web's read past the end does.
            let b = ov.max.get(k).map_or(f64::NEG_INFINITY, |&b| b.into());
            widen(&mut cols.min[c], &mut cols.max[c], ov.min[k].into(), b);
        }
    }
}
