//! How a digital column that holds edges reads, the same at every zoom and in every recording.
//! Edges that stand apart are strokes. Where they crowd, a run's band, between the two levels'
//! rails, takes an opacity from its edge rate, edges a second, on one fixed log scale, and each
//! rail the share of the time at its level. A column with an edge spent time at both levels, so
//! both rails show at least the floor there, and so does the band: a rare edge never reads as
//! empty. Both renderers draw from `looks`, held to `testdata/view/shade.json` with the web's
//! `render/shade.ts`.

use std::ops::Range;
use std::sync::LazyLock;

use escapement_tickwave_format::model::Ratio;
use escapement_tickwave_format::overview::DENSITY_STEPS;

use crate::columns::{DigitalColumns, State};

/// The opacity of a band at `RATE_FLOOR` edges a second and below, and the least a rail of a
/// column with an edge takes.
pub const FLOOR: f64 = 0.2;
/// Edges a second at the floor.
pub const RATE_FLOOR: f64 = 1.0;
/// Edges a second at full opacity: a clock of 50 MHz.
pub const RATE_FULL: f64 = 1e8;

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

/// A band's opacity at `rate` edges a second: none at no edge, `FLOOR` up to `RATE_FLOOR`, full
/// from `RATE_FULL`, and linear in the rate's logarithm between.
pub fn band(rate: f64) -> f64 {
    if rate.is_nan() || rate <= 0.0 {
        return 0.0;
    }
    let along = (rate / RATE_FLOOR).log2() / (RATE_FULL / RATE_FLOOR).log2();
    FLOOR + (1.0 - FLOOR) * along.clamp(0.0, 1.0)
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

/// The high rail's opacity and the low's, for a column whose known time is `high` high, as a
/// share from 0 to 1, NaN where none of it is known. With `edges`, each is at least `FLOOR`.
pub fn rails(high: f64, edges: bool) -> (f64, f64) {
    if high.is_nan() {
        return (0.0, 0.0);
    }
    let (h, l) = (high.clamp(0.0, 1.0), (1.0 - high).clamp(0.0, 1.0));
    match edges {
        true => (h.max(FLOOR), l.max(FLOOR)),
        false => (h, l),
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
    /// A stroke across the band a line's width wide, at this opacity: a lone edge where it sits,
    /// or a busy column alone at its middle.
    Stroke(f64),
    /// In a run: the band between the levels at `band`, and each level's rail at its opacity.
    Shade { band: f64, high: f64, low: f64 },
}

/// The columns either side of one in a run whose time its rails take: a centred window of five
/// device pixels, the odd width nearest four, held to the run.
pub const RAIL_REACH: usize = 2;

/// Each of `shown`'s columns' look, on a grid of a device pixel a column `per` ticks wide, on an
/// axis of `timebase` ticks a second, the line `width` device pixels wide.
///
/// Edges whose strokes come within a device pixel of each other, or of a busy column, join in a
/// run with the columns between them; a column with no data ends a run. A run's band takes the
/// rate of its edges over its columns, and each of its columns' rails the share of time high over
/// the `RAIL_REACH` columns either side of it within the run, at least the floor where those hold
/// an edge. A lone edge, or a busy column with no other changing column within a device pixel, is
/// a stroke at the opacity `stroke` gives the spacing to its nearest neighbour, at the rate of its
/// own edges, at least one, over that spacing.
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
    // From centre to centre, and the bare space between their paint.
    let gap = |i: usize, j: usize| {
        let ((_, a, ra), (_, b, rb)) = (changing[i], changing[j]);
        (b - a, b - rb - (a + ra))
    };
    let mut i = 0;
    while i < changing.len() {
        let mut last = i;
        while last + 1 < changing.len()
            && gap(last, last + 1).1 < 1.0
            && !cols.state[changing[last].0..changing[last + 1].0].contains(&State::None)
        {
            last += 1;
        }
        let (k0, k1) = (changing[i].0, changing[last].0);
        if last == i {
            let spacing = [
                i.checked_sub(1).map(|j| gap(j, i)),
                (i + 1 < changing.len()).then(|| gap(i, i + 1)),
            ]
            .iter()
            .flatten()
            .map(|&(s, _)| s)
            .fold(f64::INFINITY, f64::min);
            // A busy column changes level at least once, though an overview's may hold no edge
            // (a gap's end).
            let edges = cols.edges[k0].max(1.0);
            let band = band(rate(edges, spacing * per, timebase));
            looks[k0] = Look::Stroke(stroke(band, spacing, width));
        } else {
            let run = |a: usize, b: usize, of: &[f64]| of[a..=b].iter().sum::<f64>();
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
                let (high, low) = rails(share, run(a, b, &cols.edges) > 0.0);
                *look = Look::Shade { band, high, low };
            }
        }
        i = last + 1;
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
