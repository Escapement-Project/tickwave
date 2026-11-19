//! Wires for the decoder targets: lines built from a structured input, served as windows through
//! the host to each reference decoder stack. A run must end without a panic and without the host
//! refusing a batch, since a refusal is a decoder breaking a rule.

use arbitrary::Arbitrary;
use escapement_tickwave_decode::{
    AnalysisId, Host, InputSpec, Observed, Source, SourceError, Span, Tick, Window, name,
};
use escapement_tickwave_decoders::{adiv5, cjtag, jtag, jtagdp, swd};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// One line: its level from tick 0, the steps between its edges, and its gaps, each an offset
/// from the last gap's end and a length.
#[derive(Arbitrary, Debug)]
pub struct LineIn {
    pub initial: bool,
    pub steps: Vec<u8>,
    pub gaps: Vec<(u8, u8)>,
}

#[derive(Arbitrary, Debug)]
pub struct Case {
    pub lines: Vec<LineIn>,
    pub end: u16,
    /// A window of this many ticks; 0 for one larger than the input.
    pub window: u16,
    pub local: Option<u16>,
    pub reversed: bool,
    pub setting: u8,
}

struct Line {
    initial: bool,
    edges: Vec<Tick>,
    gaps: Vec<Span>,
}

impl Line {
    fn from(l: &LineIn) -> Self {
        let mut at = 0;
        let edges = (l.steps.iter())
            .map(|&s| {
                at += Tick::from(s) + 1;
                at
            })
            .collect();
        let mut after = 0;
        let gaps = (l.gaps.iter())
            .map(|&(off, len)| {
                let start = after + Tick::from(off);
                let end = start + Tick::from(len) + 1;
                after = end + 1;
                Span { start, end }
            })
            .collect();
        Self {
            initial: l.initial,
            edges,
            gaps,
        }
    }

    fn in_gap(&self, t: Tick) -> bool {
        self.gaps.iter().any(|g| g.start <= t && t < g.end)
    }

    fn level_at(&self, t: Tick) -> Option<bool> {
        if t < 0 || self.in_gap(t) {
            return None;
        }
        let n = self.edges.partition_point(|&e| e <= t);
        Some(self.initial ^ (n % 2 == 1))
    }
}

/// The lines as a source, its observations at one tick in port order or in reverse, and the line
/// of port `without` not read.
struct Wires {
    lines: Vec<Line>,
    end: Tick,
    reversed: bool,
    without: Option<u8>,
}

impl Source for Wires {
    fn window(
        &mut self,
        from: Tick,
        upto: Tick,
        most: usize,
        cancel: &AtomicBool,
    ) -> Result<Option<Window>, SourceError> {
        if cancel.load(Relaxed) {
            return Ok(None);
        }
        let mut w = Window::new(from, upto);
        let mut points = vec![];
        for (port, line) in self.lines.iter().enumerate() {
            let port = port as u8;
            if self.without == Some(port) {
                continue;
            }
            w.events.push(Observed::Start {
                port,
                level: line.level_at(from - 1).filter(|_| from - 1 < self.end),
            });
            let inside = |t: Tick| from <= t && t < upto && t < self.end;
            if inside(0) && !line.in_gap(0) {
                points.push((0, port, 0, Some(line.initial)));
            }
            for &e in &line.edges {
                if inside(e) && !line.in_gap(e) {
                    points.push((e, port, 1, line.level_at(e)));
                }
            }
            for g in &line.gaps {
                if inside(g.start) {
                    points.push((g.start, port, 2, None));
                }
                if inside(g.end) {
                    points.push((g.end, port, 0, line.level_at(g.end)));
                }
            }
            if from <= self.end && self.end < upto {
                points.push((self.end, port, 2, None));
            }
        }
        if self.reversed {
            points.sort_by_key(|&(at, port, what, _)| (at, std::cmp::Reverse(port), what));
        } else {
            points.sort_by_key(|&(at, port, what, _)| (at, port, what));
        }
        if let Some(t) = cut_at(&points, |p| p.0, most) {
            points.retain(|p| p.0 < t);
            w.upto = t;
        }
        for (at, port, what, level) in points {
            w.events.push(match (what, level) {
                (0, Some(level)) => Observed::Known { at, port, level },
                (1, Some(level)) => Observed::Edge { at, port, level },
                _ => Observed::Unknown { at, port },
            });
        }
        w.end = (self.end < w.upto).then_some(self.end);
        Ok(Some(w))
    }

    fn binding(&self) -> u64 {
        let edges = (self.lines.iter()).flat_map(|l| l.edges.iter().map(|&e| e as u64));
        escapement_tickwave_decode::fold([self.end as u64].into_iter().chain(edges))
    }

    fn ports(&self) -> Vec<bool> {
        (0..self.lines.len())
            .map(|p| self.without != Some(p as u8))
            .collect()
    }
}

/// Where a window of `points`, sorted by tick, ends to hold at most `most` ticks of them and at
/// least one: at the first tick past them, or None where all fit.
fn cut_at<T>(points: &[T], tick: impl Fn(&T) -> Tick, most: usize) -> Option<Tick> {
    let (mut ticks, mut last) = (0, None);
    for t in points.iter().map(tick) {
        if last != Some(t) {
            if ticks == most.max(1) {
                return Some(t);
            }
            ticks += 1;
            last = Some(t);
        }
    }
    None
}

fn all() -> Vec<Span> {
    vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]
}

/// The case's wires for `ports` lines, with the window and local start it asks for.
fn wires(c: &Case, ports: usize, without: Option<u8>) -> (Box<Wires>, Tick, Option<Tick>) {
    let mut lines: Vec<Line> = c.lines.iter().take(ports).map(Line::from).collect();
    while lines.len() < ports {
        lines.push(Line {
            initial: false,
            edges: vec![],
            gaps: vec![],
        });
    }
    let end = Tick::from(c.end) + 1;
    let window = if c.window == 0 {
        1 << 40
    } else {
        Tick::from(c.window)
    };
    let w = Wires {
        lines,
        end,
        reversed: c.reversed,
        without,
    };
    (Box::new(w), window, c.local.map(Tick::from))
}

/// Runs the host to the end, which must finish what it read without refusing a batch.
fn run(mut h: Host, root: AnalysisId) {
    if let Err(why) = h.run_until(|_| false) {
        panic!("refused: {why:?}");
    }
    assert!(
        h.finished(root).is_some(),
        "the wire analysis did not finish"
    );
}

pub fn swd(c: &Case) {
    let (source, window, local) = wires(c, 2, None);
    let mut h = Host::default();
    let s = h
        .add(
            name("swd"),
            swd::definition(),
            InputSpec::Wire { source, local },
            swd::Swd::factory(window),
            &[
                swd::turnaround(c.setting % 4 + 1),
                swd::orundetect(c.setting & 4 != 0),
            ],
            all(),
        )
        .unwrap();
    let a = h
        .add(
            name("adi"),
            adiv5::adi_definition(),
            InputSpec::Upstream(s),
            adiv5::Adi::factory(),
            &[],
            all(),
        )
        .unwrap();
    h.add(
        name("mem"),
        adiv5::mem_definition(),
        InputSpec::Upstream(a),
        adiv5::Mem::factory(),
        &[],
        all(),
    )
    .unwrap();
    run(h, s);
}

/// Chains a JTAG case is cut by: none, one TAP, a few short ones, and two with a 4-bit TAP
/// nearest TDO, which JTAG-DP and ADIv5 are stacked on.
const CHAINS: [&[u8]; 6] = [&[], &[1], &[2, 1], &[1, 1, 1], &[4], &[4, 1]];

pub fn jtag(c: &Case) {
    let has_tdo = c.setting & 1 != 0;
    let chain = CHAINS[usize::from(c.setting >> 4) % CHAINS.len()];
    let (source, window, local) = wires(c, 4, (!has_tdo).then_some(jtag::TDO));
    let mut h = Host::default();
    let j = h
        .add(
            name("jtag"),
            jtag::definition(),
            InputSpec::Wire { source, local },
            jtag::Jtag::factory(window),
            &[
                jtag::tdo_edge(
                    [jtag::TdoEdge::Rise, jtag::TdoEdge::Fall, jtag::TdoEdge::IrCapture]
                        [usize::from(c.setting >> 1) % 3],
                ),
                // Short chains, which random IR scans fit now and then.
                jtag::chain(chain),
            ],
            all(),
        )
        .unwrap();
    if chain.first() == Some(&4) {
        let dp = h
            .add(
                name("jtag-dp"),
                jtagdp::definition(),
                InputSpec::Upstream(j),
                jtagdp::JtagDp::factory(),
                &[],
                all(),
            )
            .unwrap();
        h.add(
            name("adiv5"),
            adiv5::adi_definition(),
            InputSpec::Upstream(dp),
            adiv5::Adi::factory(),
            &[],
            all(),
        )
        .unwrap();
    }
    run(h, j);
}

pub fn cjtag(c: &Case) {
    let (source, window, local) = wires(c, 2, None);
    let mut h = Host::default();
    let k = h
        .add(
            name("cjtag"),
            cjtag::definition(),
            InputSpec::Wire { source, local },
            cjtag::Cjtag::factory(window),
            &[
                cjtag::assume_qualified(c.setting & 1 != 0),
                cjtag::start_up(c.setting & 2 != 0),
            ],
            all(),
        )
        .unwrap();
    h.add(
        name("jtag"),
        jtag::definition(),
        InputSpec::Upstream(k),
        jtag::Jtag::factory(1 << 16),
        &[],
        all(),
    )
    .unwrap();
    run(h, k);
}
