//! The drift between two sources that recorded the same periodic signal: each periodic digital
//! channel's rises fitted to a line on the recording's axis, and each pair of such channels from
//! two sources compared by their periods. A stated stretch is already in the axis, so what is
//! measured is what remains.

use std::sync::Arc;

use escapement_tickwave_format::decode::{BlockChannel, Index};
use escapement_tickwave_format::header::{ChannelKind, Header};

use crate::input::Input;
use crate::recording::{self, Error};

/// The most edges a channel has for the drift to read it: a marker or a slow clock. A denser
/// channel is not decoded for it.
pub const MAX_EDGES: u64 = 1 << 20;

/// Periods two channels' may differ by and still be taken for one signal's.
const SAME_PERIOD: f64 = 0.01;

/// A channel's rises fitted to `t = a + n × period`, `n` the rise's count of periods from the
/// first, so a missed rise leaves its period out rather than doubling one. In seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Periodic {
    pub channel: usize,
    pub source: u32,
    pub rises: usize,
    /// Periods from the first rise to the last.
    pub spanned: u64,
    pub period: f64,
    /// The period's standard error.
    pub period_se: f64,
    /// The rises' distance from the line, root mean square.
    pub rms: f64,
}

/// Channel `b`'s period over `a`'s, less 1, in parts per million: the stretch `a`'s source would
/// take to tick as `b`'s does, as `merge --stretch` states one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drift {
    pub a: Periodic,
    pub b: Periodic,
    pub ppm: f64,
    /// Its standard error, from both periods'.
    pub ppm_se: f64,
}

/// A line through rise times `t` (axis ticks): its period and standard error, residual rms and the
/// periods spanned, in axis ticks. None for fewer than 3 rises, or rises that are not periodic:
/// two in one period, or a residual past a quarter period.
fn fit(t: &[i64]) -> Option<(f64, f64, f64, u64)> {
    let n = t.len();
    if n < 3 {
        return None;
    }
    let y: Vec<f64> = t.iter().map(|&x| (x - t[0]) as f64).collect();
    let mut d: Vec<f64> = y.windows(2).map(|w| w[1] - w[0]).collect();
    d.sort_by(f64::total_cmp);
    let (mut a, mut p) = (0.0, d[d.len() / 2]);
    if p <= 0.0 {
        return None;
    }
    // Twice: the periods counted by the median interval, then by the line through them.
    let mut index: Vec<f64> = Vec::new();
    let mut sxx = 0.0;
    for _ in 0..2 {
        index = y.iter().map(|&v| ((v - a) / p).round()).collect();
        if index.windows(2).any(|w| w[1] <= w[0]) {
            return None;
        }
        let nf = n as f64;
        let (mx, my) = (index.iter().sum::<f64>() / nf, y.iter().sum::<f64>() / nf);
        sxx = index.iter().map(|x| (x - mx) * (x - mx)).sum();
        let sxy: f64 = index.iter().zip(&y).map(|(x, v)| (x - mx) * (v - my)).sum();
        p = sxy / sxx;
        a = my - p * mx;
    }
    let ss: f64 = index
        .iter()
        .zip(&y)
        .map(|(x, v)| (v - a - p * x).powi(2))
        .sum();
    let rms = (ss / n as f64).sqrt();
    if !(p > 0.0 && rms < p / 4.0) {
        return None;
    }
    let se = (ss / (n - 2) as f64 / sxx).sqrt();
    let spanned = (index[n - 1] - index[0]) as u64;
    Some((p, se, rms, spanned))
}

/// The drift between every two channels from different sources whose rises are periodic with
/// periods within 1 % of each other. `rises` holds each channel's rise times in axis ticks, None
/// for one not read.
pub fn drifts(header: &Header, rises: &[Option<Vec<i64>>]) -> Vec<Drift> {
    let seconds = header.timebase.den() as f64 / header.timebase.num() as f64;
    let periodic: Vec<Periodic> = rises
        .iter()
        .enumerate()
        .filter_map(|(c, r)| {
            let (p, se, rms, spanned) = fit(r.as_deref()?)?;
            Some(Periodic {
                channel: c,
                source: header.channels.get(c)?.source,
                rises: r.as_ref()?.len(),
                spanned,
                period: p * seconds,
                period_se: se * seconds,
                rms: rms * seconds,
            })
        })
        .collect();
    let mut out = Vec::new();
    for (i, a) in periodic.iter().enumerate() {
        for b in &periodic[i + 1..] {
            if a.source == b.source || (b.period / a.period - 1.0).abs() > SAME_PERIOD {
                continue;
            }
            let ratio = b.period / a.period;
            let relative =
                ((a.period_se / a.period).powi(2) + (b.period_se / b.period).powi(2)).sqrt();
            out.push(Drift {
                a: *a,
                b: *b,
                ppm: (ratio - 1.0) * 1e6,
                ppm_se: ratio * relative * 1e6,
            });
        }
    }
    out
}

/// The digital channels the drift would read: 3 to `MAX_EDGES` edges each, from at least two
/// sources. None where there are none such.
fn wanted(header: &Header) -> Option<Vec<bool>> {
    let wanted: Vec<bool> = header
        .channels
        .iter()
        .map(|c| matches!(c.data, ChannelKind::Digital { edges, .. } if (3..=MAX_EDGES).contains(&edges)))
        .collect();
    let mut sources = header
        .channels
        .iter()
        .zip(&wanted)
        .filter(|(_, w)| **w)
        .map(|(c, _)| c.source);
    let first = sources.next()?;
    sources.any(|s| s != first).then_some(wanted)
}

/// What measuring the drift would read: the channels it compares, and their blocks, which it
/// decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidates {
    pub channels: usize,
    pub blocks: usize,
    /// The blocks' bytes in the file.
    pub bytes: u64,
}

/// What `of_index` would read of the recording `index` places, from the index alone; None where
/// it would read nothing.
pub fn candidates(index: &Index) -> Option<Candidates> {
    let wanted = wanted(index.header())?;
    let rows = || {
        index
            .blocks
            .iter()
            .zip(&wanted)
            .filter(|(_, w)| **w)
            .flat_map(|(b, _)| b)
    };
    Some(Candidates {
        channels: wanted.iter().filter(|w| **w).count(),
        blocks: rows().count(),
        bytes: rows().map(|b| u64::from(b.row.length)).sum(),
    })
}

/// The drift in the recording `input` holds, `index` its index. Only a recording of several
/// sources has any, and only its digital channels of 3 to `MAX_EDGES` edges are read; with none
/// such from two sources, no block is.
pub fn of_index(input: &dyn Input, index: &Index) -> Result<Vec<Drift>, Error> {
    let footer = &index.footer;
    let header = &footer.header;
    let Some(wanted) = wanted(header) else {
        return Ok(Vec::new());
    };
    let mut rises: Vec<Option<Vec<i64>>> = vec![None; wanted.len()];
    for c in (0..wanted.len()).filter(|&c| wanted[c]) {
        let gaps = footer.gaps(c).expect("an index holds every list");
        // The channel's level as its edges go by, and its next gap whose end sets it.
        let mut next_gap = 0;
        let mut out = Vec::new();
        for b in &index.blocks[c] {
            let data = recording::block(input, footer, c, b)?;
            let BlockChannel::Digital { initial, edges, .. } = &data else {
                unreachable!("a digital channel's blocks decode as digital")
            };
            let source = data.source_ticks().unwrap_or(edges);
            let mut level = *initial;
            for (&k, &t) in source.iter().zip(edges) {
                while let Some(g) = gaps.get(next_gap).filter(|g| g.to < k) {
                    level = g.level;
                    next_gap += 1;
                }
                level = !level;
                if level {
                    out.push(t);
                }
            }
        }
        rises[c] = Some(out);
    }
    Ok(drifts(header, &rises))
}

/// `of_index` for the recording `input` holds, its index read first.
pub fn of_file(input: Arc<dyn Input>) -> Result<(Header, Vec<Drift>), Error> {
    let footer = recording::footer(&*input)?;
    let index = Index::read(footer, recording::fetch(&*input))?;
    let drifts = of_index(&*input, &index)?;
    Ok((index.footer.header, drifts))
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn a_missed_rise_leaves_its_period_out() {
        // Every 1,000 ticks from 0, rise 3 missed, each rise jittered by ±1.
        let t = [0, 1001, 1999, 4001, 4999, 6000, 7001];
        let (p, se, rms, spanned) = fit(&t).unwrap();
        assert!((p - 1000.0).abs() < 0.5, "{p}");
        assert!(se < 0.5 && rms < 1.5, "{se} {rms}");
        assert_eq!(spanned, 7);
    }

    #[test]
    fn rises_that_are_not_periodic_fit_no_line() {
        assert_eq!(fit(&[0, 1000]), None);
        // Two rises inside one period of the median interval.
        assert_eq!(fit(&[0, 1000, 1100, 2000, 3000]), None);
    }
}
