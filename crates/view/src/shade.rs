//! How a digital column that holds edges reads, the same at every zoom and in every recording.
//! Edges that stand apart are strokes, and so is a pulse or a burst too short to have a stretch to
//! shade. Where edges crowd, a run's band, between the two levels' rails, takes an opacity from its
//! edge rate, edges a second, on one fixed log scale, and each rail a part of the band's opacity by
//! its level's share of the time. A column with an edge spent time at both levels, so both rails
//! show at least `RAIL_FLOOR` of the band there, and the band itself at least `FLOOR`: a rare edge
//! never reads as empty. Both renderers draw from `looks`, held to `testdata/view/shade.json` with
//! the web's `render/shade.ts`.

use std::ops::Range;
use std::sync::LazyLock;

use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::overview::DENSITY_STEPS;

use crate::columns::{DigitalColumns, State};

/// The opacity of a band at `RATE_FLOOR` edges a second and below.
pub const FLOOR: f64 = 0.2;
/// The opacity of a band at `RATE_TOP` edges a second and above: short of full ink, so the
/// busiest band still reads as a fill.
pub const TOP: f64 = 0.6;
/// Edges a second at the floor.
pub const RATE_FLOOR: f64 = 1.0;
/// Edges a second at the top: a clock of 50 MHz.
pub const RATE_TOP: f64 = 1e8;
/// The least part of the band's opacity a rail takes where the columns about it hold an edge.
pub const RAIL_FLOOR: f64 = 0.2;

/// The smallest edge count whose density is at least `d`, from 1; None past what a u64 counts.
fn least(d: u32) -> Option<u64> {
    let (l, r) = ((d - 1) / 4, (d - 1) % 4);
    if l > 63 {
        return None;
    }
    Some(match r {
        0 => 1 << l,
        // The smallest e in [2^l, 2^(l + 1)) with `e << (63 − l)` at least the step.
        r => ((DENSITY_STEPS[r as usize - 1] - 1) >> (63 - l)) + 1,
    })
}

/// The smallest and largest edge count density `d` stands for, None for 0. A step no count
/// reaches, as 2, 3 and 4 between one edge and two, stands for the next count up alone.
pub fn counts(d: u8) -> Option<(u64, u64)> {
    if d == 0 {
        return None;
    }
    let lo = least(u32::from(d))?;
    let hi = least(u32::from(d) + 1).map_or(u64::MAX, |next| next - 1);
    Some((lo, hi.max(lo)))
}

/// The edge count density `d` stands for: the geometric mean of the least and most it stands
/// for, so a count it stands for lies within `sqrt(most / least) − 1` of it, under 9.1 % at any
/// density, and exactly where it stands for one count, as for one to seven edges.
pub fn count(d: u8) -> f64 {
    COUNTS[usize::from(d)]
}

/// Each density's count, worked once: a query reads one for every bucket a column meets.
static COUNTS: LazyLock<[f64; 256]> = LazyLock::new(|| {
    std::array::from_fn(|d| counts(d as u8).map_or(0.0, |(lo, hi)| (lo as f64 * hi as f64).sqrt()))
});

/// Edges a second: `edges` over `ticks` of the axis, which runs `timebase` ticks a second.
pub fn rate(edges: f64, ticks: f64, timebase: Ratio) -> f64 {
    edges * timebase.num() as f64 / (ticks * timebase.den() as f64)
}

/// A band's opacity at `rate` edges a second: none at no edge, `FLOOR` up to `RATE_FLOOR`, `TOP`
/// from `RATE_TOP`, and linear in the rate's logarithm between.
pub fn band(rate: f64) -> f64 {
    if rate.is_nan() || rate <= 0.0 {
        return 0.0;
    }
    let along = (rate / RATE_FLOOR).log2() / (RATE_TOP / RATE_FLOOR).log2();
    FLOOR + (TOP - FLOOR) * along.clamp(0.0, 1.0)
}

/// The opacity of a lone edge's stroke, `width` device pixels wide and `spacing` from the nearest
/// edge, centre to centre, where `band` is the opacity of a band at the rate that spacing stands
/// for, an edge every `spacing`: the stroke covers its stretch of band as darkly as that band
/// would, up to full ink. So where edges come close enough to join into a run's band, their
/// strokes read as that band, and edges further apart reach full ink. An edge with none beside it
/// takes full ink.
pub fn stroke(band: f64, spacing: f64, width: f64) -> f64 {
    if !spacing.is_finite() {
        return 1.0;
    }
    (band * spacing / width).min(1.0)
}

/// The high rail's opacity and the low's, for columns whose known time is `high` high, as a share
/// from 0 to 1, NaN where none of it is known, in a run whose band takes opacity `band`. With
/// `edges`, each rail takes the band's opacity times its level's share over the larger share, at
/// least `RAIL_FLOOR` of it: an even wave's rails both at the band's, 75 % high the high rail at it
/// and the low at a third of it. Without, each takes the band's opacity times its share, so a level
/// held inside a run reads in the band's tone, not as a line at full ink.
pub fn rails(high: f64, edges: bool, band: f64) -> (f64, f64) {
    if high.is_nan() {
        return (0.0, 0.0);
    }
    let (h, l) = (high.clamp(0.0, 1.0), (1.0 - high).clamp(0.0, 1.0));
    match edges {
        true => {
            let larger = h.max(l);
            let rail = |share: f64| band * (share / larger).max(RAIL_FLOOR);
            (rail(h), rail(l))
        }
        false => (band * h, band * l),
    }
}

/// The share of its time a bucket of duty `duty` spends high, as `(stands for, least, most)`:
/// duty `d` stands for `d / 14`, rounded from any share at least `(2d − 1) / 28` and below
/// `(2d + 1) / 28`, held to 0..=1. None for 15, a bucket outside the channel's kept span.
pub fn duty_share(duty: u8) -> Option<(f64, f64, f64)> {
    match duty {
        0..=14 => {
            let d = f64::from(duty);
            Some((
                d / 14.0,
                ((2.0 * d - 1.0) / 28.0).max(0.0),
                ((2.0 * d + 1.0) / 28.0).min(1.0),
            ))
        }
        _ => None,
    }
}

/// How a column of a digital row draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Look {
    /// Its level as the line: a column with no edge, or nothing drawn.
    Flat,
    /// A lone edge where it sits: a stroke across the band a line's width wide, at this opacity.
    Stroke(f64),
    /// A busy column alone, or a short run: one stroke across the band a line's width wide at
    /// `alpha`, centred `at` columns from the row's first column. Each of its columns holds the same
    /// `Pulse`. The line comes in at the level before its first column and leaves at the level the
    /// column after its last starts at.
    Pulse { alpha: f64, at: f64 },
    /// In a run: the band between the levels at `band`, and each level's rail at its opacity.
    Shade { band: f64, high: f64, low: f64 },
    /// A level held briefly between two runs: no band, and its line at this opacity, the larger
    /// of the two runs' bands.
    Held(f64),
}

/// The columns either side of one in a run whose time its rails take: a centred window of five
/// device pixels, the odd width nearest four, held to the run.
pub const RAIL_REACH: usize = 2;

/// Each of `shown`'s columns' look, on a grid of a device pixel a column `per` ticks wide, on an
/// axis of `timebase` ticks a second, the line `width` device pixels wide.
///
/// Edges whose strokes come within a device pixel of each other, or of a busy column, join in a
/// run with the columns between them; a column with no data ends a run. So two changing columns
/// join when their centres are under `width + 1` apart, the join distance.
///
/// A run whose first and last changing columns are under the join distance apart is short: any two
/// of its edges would join on their own, so it has no stretch for a band to describe, and a shift
/// of the column grid could have put a busy column alone in its place. It is a `Pulse` at its
/// middle, as a busy column alone is at its own. A lone edge is a `Stroke`. Each takes the opacity
/// `stroke` gives the spacing from its centre to its nearest neighbour's (a pulse's middle, any
/// other changing column's own centre), at the rate of its own edges, at least one, over that
/// spacing: full ink where it stands apart. A pulse's spacing counts no pulse beside it, so pulses
/// near each other, rare flags close together, each take full ink at every zoom.
///
/// A longer run's band takes the rate of its edges over its columns, and each of its columns'
/// rails `rails` of the share of time high over the `RAIL_REACH` columns either side of it within
/// the run. Where two longer runs have under the join distance of bare space between them, and no
/// column without data, the level held between keeps its gap but is `Held` at the larger of their
/// bands: at full ink it would draw as a speck darker than the runs either side.
pub fn looks(
    cols: &DigitalColumns,
    shown: Range<usize>,
    width: f64,
    per: f64,
    timebase: Ratio,
) -> Vec<Look> {
    let mut looks = vec![Look::Flat; cols.state.len()];
    // Each changing column's centre, in columns, and how far its paint reaches either side of it.
    let changing: Vec<(usize, f64, f64)> = shown
        .filter_map(|k| match cols.state[k] {
            State::Rise | State::Fall => Some((k, k as f64 + cols.at[k], width / 2.0)),
            State::Busy => Some((k, k as f64 + 0.5, 0.5 + (width - 1.0) / 2.0)),
            _ => None,
        })
        .collect();
    // The bare space between their paint.
    let gap = |i: usize, j: usize| {
        let ((_, a, ra), (_, b, rb)) = (changing[i], changing[j]);
        b - rb - (a + ra)
    };
    // Whether a column with no data lies between changing columns i and j.
    let cut = |i: usize, j: usize| cols.state[changing[i].0..changing[j].0].contains(&State::None);
    let run = |a: usize, b: usize, of: &[f64]| of[a..=b].iter().sum::<f64>();
    // Each run as its first and last changing column; where each changing column sits as a
    // neighbour, a short run's at its middle, as it draws, any other's at its own centre; and
    // which are in pulses.
    let mut runs: Vec<(usize, usize, bool)> = Vec::new();
    let mut place: Vec<f64> = changing.iter().map(|&(_, c, _)| c).collect();
    let mut pulse = vec![false; changing.len()];
    let mut i = 0;
    while i < changing.len() {
        let mut last = i;
        while last + 1 < changing.len() && gap(last, last + 1) < 1.0 && !cut(last, last + 1) {
            last += 1;
        }
        let short = changing[last].1 - changing[i].1 < width + 1.0;
        if short {
            place[i..=last].fill((changing[i].1 + changing[last].1) / 2.0);
            pulse[i..=last].fill(last > i || cols.state[changing[i].0] == State::Busy);
        }
        runs.push((i, last, short));
        i = last + 1;
    }
    // The longer run just before, as its last changing column and its band.
    let mut before: Option<(usize, f64)> = None;
    for (i, last, short) in runs {
        let (k0, k1) = (changing[i].0, changing[last].0);
        if short {
            before = None;
            let centre = place[i];
            let mut spacing = f64::INFINITY;
            if i > 0 && !(pulse[i] && pulse[i - 1]) {
                spacing = spacing.min(centre - place[i - 1]);
            }
            if last + 1 < changing.len() && !(pulse[i] && pulse[last + 1]) {
                spacing = spacing.min(place[last + 1] - centre);
            }
            // A busy column changes level at least once, though an overview's may hold no edge
            // (a gap's end).
            let edges = run(k0, k1, &cols.edges).max(1.0);
            let alpha = stroke(band(rate(edges, spacing * per, timebase)), spacing, width);
            match pulse[i] {
                true => looks[k0..=k1].fill(Look::Pulse { alpha, at: centre }),
                false => looks[k0] = Look::Stroke(alpha),
            }
        } else {
            let band = band(rate(
                run(k0, k1, &cols.edges),
                (k1 - k0 + 1) as f64 * per,
                timebase,
            ));
            for (k, look) in looks.iter_mut().enumerate().take(k1 + 1).skip(k0) {
                let (a, b) = (
                    k.saturating_sub(RAIL_REACH).max(k0),
                    (k + RAIL_REACH).min(k1),
                );
                let known = run(a, b, &cols.known);
                let share = match known > 0.0 {
                    true => run(a, b, &cols.high) / known,
                    false => f64::NAN,
                };
                let (high, low) = rails(share, run(a, b, &cols.edges) > 0.0, band);
                *look = Look::Shade { band, high, low };
            }
            if let Some((end, was)) = before
                && gap(end, i) < width + 1.0
                && !cut(end, i)
            {
                looks[changing[end].0 + 1..k0].fill(Look::Held(was.max(band)));
            }
            before = Some((last, band));
        }
    }
    looks
}

#[cfg(test)]
mod tests {
    use super::*;
    use escapement_tickwave_format::overview::density;

    #[test]
    fn a_densitys_counts_are_those_the_format_gives_it() {
        for d in 1..=212u8 {
            let (lo, hi) = counts(d).unwrap();
            assert!(
                density(lo - 1) < d,
                "density {d}: {} below its least",
                lo - 1
            );
            if density(lo) == d {
                assert_eq!(density(hi), d, "density {d}: its most, {hi}");
                assert!(density(hi + 1) > d, "density {d}: {} past its most", hi + 1);
            } else {
                // A step no count reaches: the next count up, alone.
                assert!(density(lo) > d && hi == lo, "density {d}: {lo}..={hi}");
            }
        }
        assert_eq!(counts(0), None);
    }

    #[test]
    fn a_count_lies_within_an_eighth_of_a_doubling_of_what_its_density_stands_for() {
        let bound = 2f64.powf(0.125) - 1.0;
        let mut worst = 0f64;
        for d in 1..=212u8 {
            let (lo, hi) = counts(d).unwrap();
            let c = count(d);
            // The least and most are the farthest from it; a few between, as a check.
            for e in [lo, hi, lo + (hi - lo) / 3, lo + (hi - lo) / 2] {
                let err = (e as f64 / c - 1.0).abs();
                worst = worst.max(err);
                assert!(err <= bound + 1e-12, "density {d}: {e} against {c}");
            }
        }
        // The bound is met, not merely kept under: at the densest steps.
        assert!(worst > bound - 1e-6, "{worst}");
    }
}
