//! Wires for the decoder targets: lines built from a structured input, served as windows through
//! the host to each reference decoder stack. A run must end without a panic and without the host
//! refusing a batch, since a refusal is a decoder breaking a rule.

use arbitrary::Arbitrary;
use escapement_tickwave_decode::{
    AnalysisId, Host, InputSpec, Observed, Source, SourceError, Span, Tick, Window, name,
};
use escapement_tickwave_decoders::{adiv5, cjtag, jtag, swd};
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

/// The lines as a source, its observations at one tick in port order or in reverse.
struct Wires {
    lines: Vec<Line>,
    end: Tick,
    reversed: bool,
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
fn wires(c: &Case, ports: usize) -> (Box<Wires>, Tick, Option<Tick>) {
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
    let (source, window, local) = wires(c, 2);
    let mut h = Host::default();
    let Ok(factory) = swd::Swd::factory(window, c.setting % 4 + 1) else {
        return;
    };
    let s = h
        .add(
            name("swd"),
            swd::definition(),
            InputSpec::Wire { source, local },
            factory,
            all(),
        )
        .unwrap();
    let a = h
        .add(
            name("adi"),
            adiv5::adi_definition(),
            InputSpec::Upstream(adiv5::adi_subscription(s)),
            adiv5::Adi::factory(),
            all(),
        )
        .unwrap();
    h.add(
        name("mem"),
        adiv5::mem_definition(),
        InputSpec::Upstream(adiv5::mem_subscription(a)),
        adiv5::Mem::factory(),
        all(),
    )
    .unwrap();
    run(h, s);
}

pub fn jtag(c: &Case) {
    let (source, window, local) = wires(c, 4);
    let mut h = Host::default();
    let j = h
        .add(
            name("jtag"),
            jtag::definition(),
            InputSpec::Wire { source, local },
            jtag::Jtag::factory_with(c.setting & 1 != 0, window, c.setting & 2 != 0),
            all(),
        )
        .unwrap();
    run(h, j);
}

pub fn cjtag(c: &Case) {
    let (source, window, local) = wires(c, 2);
    let mut h = Host::default();
    let k = h
        .add(
            name("cjtag"),
            cjtag::definition(),
            InputSpec::Wire { source, local },
            cjtag::Cjtag::factory(c.setting & 1 != 0, window),
            all(),
        )
        .unwrap();
    h.add(
        name("jtag"),
        cjtag::jtag_definition(),
        InputSpec::Upstream(cjtag::jtag_subscription(k)),
        cjtag::JtagOnCjtag::factory(),
        all(),
    )
    .unwrap();
    run(h, k);
}
