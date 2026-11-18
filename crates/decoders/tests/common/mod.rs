//! Synthetic wires for the decoders' tests: lines with edges and gaps served as windows, and an
//! SWD encoder.

#![allow(dead_code)]

#[path = "../../examples/check/rows.rs"]
mod rows;

use escapement_tickwave_decode::{Host, Observed, Source, SourceError, Span, Tick, Window};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// One line: its level from tick 0, its edges, and its gaps.
#[derive(Clone, Debug, Default)]
pub struct Line {
    pub initial: bool,
    pub edges: Vec<Tick>,
    pub gaps: Vec<Span>,
}

impl Line {
    fn in_gap(&self, t: Tick) -> bool {
        self.gaps.iter().any(|g| g.start <= t && t < g.end)
    }

    /// The level at tick `t`, after anything at it; None in a gap or before tick 0.
    pub fn level_at(&self, t: Tick) -> Option<bool> {
        if t < 0 || self.in_gap(t) {
            return None;
        }
        let n = self.edges.partition_point(|&e| e <= t);
        Some(self.initial ^ (n % 2 == 1))
    }
}

/// Lines served as windows on one timeline that ends at `end`. Observations at one tick come in
/// port order, or in reverse where `reversed`, since a decoder may rely on neither.
#[derive(Clone, Debug)]
pub struct Wires {
    pub lines: Vec<Line>,
    pub end: Tick,
    pub reversed: bool,
}

impl Source for Wires {
    fn window(
        &mut self,
        from: Tick,
        upto: Tick,
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
                if inside(e) && !line.in_gap(e) && e > 0 {
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
        for (at, port, what, level) in points {
            w.events.push(match (what, level) {
                (0, Some(level)) => Observed::Known { at, port, level },
                (1, Some(level)) => Observed::Edge { at, port, level },
                _ => Observed::Unknown { at, port },
            });
        }
        w.end = (self.end < upto).then_some(self.end);
        Ok(Some(w))
    }
    fn fork(&self) -> Option<Box<dyn Source>> {
        Some(Box::new(self.clone()))
    }
}

/// An SWD transfer to encode.
#[derive(Clone, Copy, Debug)]
pub struct T {
    pub ap: bool,
    pub read: bool,
    pub addr: u8,
    /// 1 OK, 2 WAIT, 4 FAULT.
    pub ack: u8,
    pub data: u32,
}

pub fn dp_write(addr: u8, data: u32) -> T {
    T {
        ap: false,
        read: false,
        addr,
        ack: 1,
        data,
    }
}
pub fn dp_read(addr: u8, data: u32) -> T {
    T {
        ap: false,
        read: true,
        addr,
        ack: 1,
        data,
    }
}
pub fn ap_write(addr: u8, data: u32) -> T {
    T {
        ap: true,
        read: false,
        addr,
        ack: 1,
        data,
    }
}
pub fn ap_read(addr: u8, data: u32) -> T {
    T {
        ap: true,
        read: true,
        addr,
        ack: 1,
        data,
    }
}

/// The bits of each transfer, sampled one a clock: request, turnaround, acknowledge, and for OK
/// the data and its parity with the second turnaround where the direction says.
pub fn bits(transfers: &[T]) -> (Vec<bool>, Vec<usize>) {
    let mut out = vec![false; 8];
    let mut starts = vec![];
    for t in transfers {
        starts.push(out.len());
        let a2 = t.addr & 4 != 0;
        let a3 = t.addr & 8 != 0;
        let parity = t.ap ^ t.read ^ a2 ^ a3;
        out.extend([true, t.ap, t.read, a2, a3, parity, false, true]);
        out.push(true);
        out.extend((0..3).map(|k| t.ack >> k & 1 != 0));
        if t.ack == 1 {
            let data: Vec<bool> = (0..32).map(|k| t.data >> k & 1 != 0).collect();
            let p = t.data.count_ones() % 2 == 1;
            if !t.read {
                out.push(true);
            }
            out.extend(data);
            out.push(p);
            if t.read {
                out.push(true);
            }
        } else {
            out.push(true);
        }
        out.extend([false, false]);
    }
    out.extend([false; 8]);
    (out, starts)
}

/// SWCLK and SWDIO for `bits`, one bit every `period` ticks: SWDIO set at each fall, sampled at
/// each rise half a period later.
pub fn wires(bits: &[bool], period: Tick) -> Wires {
    let half = period / 2;
    let mut clock = Line::default();
    let mut data = Line::default();
    let mut level = false;
    for (k, &b) in bits.iter().enumerate() {
        let at = 10 + k as Tick * period;
        if b != level {
            data.edges.push(at);
            level = b;
        }
        clock.edges.push(at + half);
        clock.edges.push(at + period);
    }
    Wires {
        lines: vec![clock, data],
        end: 10 + bits.len() as Tick * period + period,
        reversed: false,
    }
}

/// The tick of bit `k`'s rise.
pub fn rise(k: usize, period: Tick) -> Tick {
    10 + k as Tick * period + period / 2
}

/// Asserts that every row of analysis `a`'s items holds no more at once than its lanes.
pub fn fits_its_rows(h: &Host, a: escapement_tickwave_decode::AnalysisId) {
    let schema = &h.definition(a).schema;
    let items = h.store(a).items.values();
    let spans = items.map(|i| (i.id.kind, i.start, i.end.unwrap_or(i.start)));
    for (row, (most, lanes)) in rows::most_at_once(schema, spans).into_iter().enumerate() {
        assert!(most <= lanes, "row {row}: {most} at once in {lanes} lanes");
    }
}
