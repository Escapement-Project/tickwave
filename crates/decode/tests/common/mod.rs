//! Synthetic analyses for the host's tests: a scripted decoder that does exactly what a test asks
//! turn by turn, and a small pulse protocol on one line with an analysis stacked on it.

#![allow(dead_code)]

use escapement_tickwave_decode::{
    AnalysisId, Decoder, Definition, Delivery, Element, ElementType, Exchange, FieldId, FieldType,
    Host, HostOptions, Id, InputSpec, KindDecl, KindId, LinkDecl, LinkTo, Mode, Observation,
    Observed, Op, Pause, Ref, Request, Schema, SeqId, Source, SourceError, Span, Status,
    Subscription, Tick, Value, Window, field, name, seq,
};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
    },
};

pub const FRAME: KindId = KindId(0);
pub const CHILD: KindId = KindId(1);
pub const V: FieldId = FieldId(0);
pub const N: FieldId = FieldId(1);
pub const P: FieldId = FieldId(2);
pub const B: FieldId = FieldId(3);
pub const BITS: SeqId = SeqId(0);

pub fn toy_schema() -> Schema {
    Schema::new(vec![
        KindDecl::new("frame")
            .with_fields(vec![
                field("v", FieldType::Word { bits: 8 }),
                field("n", FieldType::Name(vec![name("a"), name("b")])),
                field("p", FieldType::Phase),
                field("b", FieldType::Bits { max: 16 }),
            ])
            .with_sequences(vec![seq("bits", ElementType::Bit)])
            .with_links(vec![LinkDecl::new(
                "next",
                vec![LinkTo::Own(name("frame"))],
            )]),
        KindDecl::new("child").with_parents(["frame"]),
    ])
    .with_roles(["probe"])
    .with_reasons(["lost"])
    .with_needs(["SELECT"])
    .with_causes(["cut"])
    .with_open_at_most(4)
}

pub fn toy() -> Arc<Definition> {
    Arc::new(Definition::new("toy", 1, toy_schema()).with_ports(["line"]))
}

pub fn all() -> Vec<Span> {
    vec![Span {
        start: Tick::MIN,
        end: Tick::MAX,
    }]
}

pub fn frame(at: Tick) -> Id {
    Id::new(FRAME, at)
}

pub fn set(id: Id, field: FieldId, value: Value) -> Op {
    Op::Set {
        id,
        field,
        value,
        span: None,
        driver: None,
    }
}

pub fn open(id: Id, start: Tick) -> Op {
    Op::Open {
        id,
        parent: None,
        start,
    }
}

pub fn close(id: Id, end: Tick) -> Op {
    Op::Close {
        id,
        end,
        status: Status::Complete,
    }
}

/// A source with nothing on it, which counts its reads.
#[derive(Default, Clone)]
pub struct Nothing(pub Arc<AtomicUsize>);

impl Source for Nothing {
    fn window(
        &mut self,
        from: Tick,
        upto: Tick,
        _: usize,
        cancel: &AtomicBool,
    ) -> Result<Option<Window>, SourceError> {
        if cancel.load(Relaxed) {
            return Ok(None);
        }
        self.0.fetch_add(1, Relaxed);
        Ok(Some(Window::new(from, upto)))
    }

    fn binding(&self) -> u64 {
        0
    }

    fn ports(&self) -> Vec<bool> {
        vec![true]
    }
}

/// What a scripted decoder does in a turn.
#[derive(Clone, Debug)]
pub enum Act {
    Emit(Op),
    Frontier(Tick),
    Lost(Tick, Tick),
    Request(Tick, Tick),
    TakeReply,
    /// Takes every delivery and declares each loss again.
    Drain,
    /// Takes every delivery and declares nothing.
    Swallow,
    Finish(Tick),
}

/// Takes every delivery and declares nothing.
pub struct Swallow;

impl Decoder for Swallow {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        while ex.receive().is_some() {}
        Pause::Input
    }
}

/// Does one list of acts a turn, then waits.
#[derive(Clone, Default)]
pub struct Script(pub VecDeque<Vec<Act>>);

impl Decoder for Script {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        let Some(acts) = self.0.pop_front() else {
            return Pause::Input;
        };
        let mut pause = if self.0.is_empty() {
            Pause::Input
        } else {
            Pause::Work
        };
        for act in acts {
            match act {
                Act::Emit(op) => ex.out.ops.push(op),
                Act::Frontier(t) => ex.frontier(t),
                Act::Lost(a, b) => ex.lost(a, b),
                Act::Request(from, upto) => ex.request(Request::Window { from, upto }),
                Act::TakeReply => {
                    ex.reply();
                }
                Act::Drain => {
                    while let Some(d) = ex.receive() {
                        if let Delivery::Lost { span, .. } = d {
                            ex.lost(span.start, span.end);
                        }
                    }
                }
                Act::Swallow => while ex.receive().is_some() {},
                Act::Finish(end) => {
                    ex.finish(end);
                    pause = Pause::Finished;
                }
            }
        }
        pause
    }

    fn save(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(self.clone()))
    }
}

pub fn script(turns: Vec<Vec<Act>>) -> Script {
    Script(turns.into())
}

// ---- Pulses: a line's high pulses, and pairs of them stacked on it ----------------------------

pub const PULSE: KindId = KindId(0);
pub const WIDTH: FieldId = FieldId(0);
pub const LEVELS: SeqId = SeqId(0);
pub const PAIR: KindId = KindId(0);
pub const FIRST: escapement_tickwave_decode::LinkId = escapement_tickwave_decode::LinkId(0);
pub const CUT: escapement_tickwave_decode::Cause = escapement_tickwave_decode::Cause(0);

pub fn pulses_schema() -> Schema {
    Schema::new(vec![
        KindDecl::new("pulse")
            .with_fields(vec![field("width", FieldType::Word { bits: 32 })])
            .with_sequences(vec![seq("levels", ElementType::Bit)]),
    ])
    .with_causes(["a gap cuts it"])
}

pub fn pairs_schema() -> Schema {
    Schema::new(vec![
        KindDecl::new("pair")
            .with_fields(vec![field("width", FieldType::Word { bits: 32 })])
            .with_links(vec![LinkDecl::new(
                "first",
                vec![LinkTo::Upstream(name("pulse"))],
            )]),
    ])
}

pub fn definition(n: &'static str, schema: Schema) -> Arc<Definition> {
    Arc::new(Definition::new(n, 1, schema).with_ports(["line"]))
}

/// A line's edges and gaps, served in windows.
#[derive(Clone)]
pub struct Line {
    pub initial: bool,
    pub edges: Vec<Tick>,
    pub gaps: Vec<Span>,
    pub end: Tick,
}

impl Line {
    fn level_at(&self, t: Tick) -> Option<bool> {
        if self.gaps.iter().any(|g| g.start <= t && t < g.end) || t >= self.end {
            return None;
        }
        let n = self.edges.partition_point(|&e| e <= t);
        Some(self.initial ^ (n % 2 == 1))
    }
}

impl Source for Line {
    fn window(
        &mut self,
        from: Tick,
        mut upto: Tick,
        most: usize,
        cancel: &AtomicBool,
    ) -> Result<Option<Window>, SourceError> {
        if cancel.load(Relaxed) {
            return Ok(None);
        }
        let mut events = vec![Observed::Start {
            port: 0,
            level: self.level_at(from.max(0)),
        }];
        let mut points = vec![];
        for &e in &self.edges {
            if from <= e && e < upto {
                points.push((e, 1, self.level_at(e)));
            }
        }
        for g in &self.gaps {
            if from <= g.start && g.start < upto {
                points.push((g.start, 0, None));
            }
            if from <= g.end && g.end < upto {
                points.push((g.end, 2, self.level_at(g.end)));
            }
        }
        if from <= self.end && self.end < upto {
            points.push((self.end, 0, None));
        }
        points.sort();
        if let Some(t) = cut_at(&points, |p| p.0, most) {
            points.retain(|p| p.0 < t);
            upto = t;
        }
        for (at, what, level) in points {
            events.push(match (what, level) {
                (1, Some(level)) => Observed::Edge { at, port: 0, level },
                (2, Some(level)) => Observed::Known { at, port: 0, level },
                _ => Observed::Unknown { at, port: 0 },
            });
        }
        let mut w = Window::new(from, upto);
        w.events = events;
        w.end = (self.end < upto).then_some(self.end);
        Ok(Some(w))
    }

    fn binding(&self) -> u64 {
        let edges = self.edges.iter().map(|&e| e as u64);
        escapement_tickwave_decode::fold([self.end as u64].into_iter().chain(edges))
    }

    fn ports(&self) -> Vec<bool> {
        vec![true]
    }

    fn fork(&self) -> Option<Box<dyn Source>> {
        Some(Box::new(self.clone()))
    }
}

/// Where a window of `points`, sorted by tick, ends to hold at most `most` ticks of them and at
/// least one: at the first tick past them, or None where all fit.
pub fn cut_at<T>(points: &[T], tick: impl Fn(&T) -> Tick, most: usize) -> Option<Tick> {
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

/// Each high pulse as an item with its width and its levels, one window of `step` ticks a turn.
#[derive(Clone)]
pub struct Pulses {
    pub at: Tick,
    pub step: Tick,
    /// How far past its fall a pulse ends, as SWD's last cell lies past its last rise.
    pub tail: Tick,
    pub level: Option<bool>,
    pub open: Option<Id>,
    pub levels: u32,
    pub gap: Option<Tick>,
}

impl Pulses {
    pub fn new(local: Option<Tick>, step: Tick) -> Self {
        Self {
            at: local.unwrap_or(0),
            step,
            tail: 0,
            level: None,
            open: None,
            levels: 0,
            gap: None,
        }
    }

    fn cut(&mut self, ex: &mut Exchange<'_>, at: Tick) {
        if let Some(id) = self.open.take() {
            ex.out.close(id, at, Status::Incomplete(CUT));
        }
    }
}

impl Decoder for Pulses {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        let Some(w) = ex.reply() else {
            ex.request(Request::Window {
                from: self.at,
                upto: self.at + self.step,
            });
            return Pause::Input;
        };
        for e in &w.events {
            match *e {
                Observed::Start { level, .. } => {
                    if self.level.is_none() {
                        self.level = level;
                    }
                }
                Observed::Edge { at, level, .. } => {
                    if level {
                        let id = Id::new(PULSE, at);
                        ex.out.open(id, None, at);
                        self.open = Some(id);
                        self.levels = 0;
                    } else if let Some(id) = self.open.take() {
                        let width = u64::try_from(at - id.anchor).unwrap_or(0);
                        ex.out.set(
                            id,
                            WIDTH,
                            Value::Word {
                                value: width,
                                bits: 32,
                            },
                        );
                        ex.out.close(id, at + self.tail, Status::Complete);
                    }
                    if let Some(id) = self.open {
                        let cell = Span {
                            start: at,
                            end: at + 1,
                        };
                        let el = Element::new(u8::from(level), Observation::Wire(at), cell);
                        ex.out.extend(id, LEVELS, self.levels, &[el]);
                        self.levels += 1;
                    }
                    self.level = Some(level);
                }
                Observed::Unknown { at, .. } => {
                    self.cut(ex, at);
                    self.level = None;
                    self.gap = Some(at);
                }
                Observed::Known { at, level, .. } => {
                    if let Some(from) = self.gap.take() {
                        ex.lost(from, at);
                    }
                    self.level = Some(level);
                }
                _ => {}
            }
        }
        self.at = w.upto;
        let frontier = self.open.map_or(w.upto, |id| id.anchor);
        ex.frontier(frontier);
        if let Some(end) = w.end {
            self.cut(ex, end);
            ex.finish(end);
            return Pause::Finished;
        }
        ex.request(Request::Window {
            from: self.at,
            upto: self.at + self.step,
        });
        Pause::Input
    }

    fn save(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(self.clone()))
    }
}

/// Every two completed pulses as a pair, linked to the first; declares every loss again.
#[derive(Clone, Default)]
pub struct Pairs {
    pub first: Option<(Id, Tick, u64)>,
}

impl Decoder for Pairs {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        while let Some(d) = ex.receive() {
            match d {
                Delivery::Item { item, .. } => {
                    if item.status != Status::Complete {
                        self.first = None;
                        continue;
                    }
                    let width = match item.field(WIDTH).map(|f| &f.value) {
                        Some(Value::Word { value, .. }) => *value,
                        _ => 0,
                    };
                    match self.first.take() {
                        None => self.first = Some((item.id, item.start, width)),
                        Some((first, start, w)) => {
                            let id = Id::new(PAIR, first.anchor);
                            ex.out.open(id, None, start);
                            let value = (w + width).min(u64::from(u32::MAX));
                            ex.out.set(id, WIDTH, Value::Word { value, bits: 32 });
                            ex.out.link(id, FIRST, Ref::Upstream(first));
                            ex.out.close(id, item.end, Status::Complete);
                        }
                    }
                }
                Delivery::Lost { span, .. } => {
                    self.first = None;
                    ex.lost(span.start, span.end);
                }
                Delivery::Frontier { start, .. } => {
                    let f = self.first.map_or(start, |(_, s, _)| s.min(start));
                    ex.frontier(f);
                }
                Delivery::End { end, .. } => {
                    ex.finish(end);
                    return Pause::Finished;
                }
                _ => {}
            }
        }
        if ex.more() { Pause::Work } else { Pause::Input }
    }

    fn save(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(self.clone()))
    }
}

/// A host with pulses on `line` read in windows of `step`, and pairs stacked on them.
pub fn pulses_host(
    line: Line,
    step: Tick,
    local: Option<Tick>,
    options: HostOptions,
) -> (Host, [AnalysisId; 2]) {
    pulses_host_tail(line, step, local, options, 0)
}

/// As `pulses_host`, each pulse ending `tail` past its fall.
pub fn pulses_host_tail(
    line: Line,
    step: Tick,
    local: Option<Tick>,
    options: HostOptions,
    tail: Tick,
) -> (Host, [AnalysisId; 2]) {
    let mut h = Host::new(options);
    let p = h
        .add(
            name("pulses"),
            definition("pulses", pulses_schema()),
            InputSpec::Wire {
                source: Box::new(line),
                local,
            },
            Box::new(move |_, local| {
                let mut p = Pulses::new(local, step);
                p.tail = tail;
                Box::new(p)
            }),
            &[],
            all(),
        )
        .unwrap();
    let q = h
        .add(
            name("pairs"),
            definition("pairs", pairs_schema()),
            InputSpec::Upstream(Subscription::new(p, Mode::Completed).with_kinds(["pulse"])),
            Box::new(|_, _| Box::new(Pairs::default())),
            &[],
            all(),
        )
        .unwrap();
    (h, [p, q])
}

/// A line of `n` pulses, each `width` wide, every `period`, with gaps where given.
pub fn line(n: i64, width: Tick, period: Tick, gaps: Vec<Span>) -> Line {
    let mut edges = vec![];
    for k in 0..n {
        let rise = 10 + k * period;
        if gaps.iter().any(|g| g.start <= rise + width && rise < g.end) {
            continue;
        }
        edges.push(rise);
        edges.push(rise + width);
    }
    Line {
        initial: false,
        edges,
        gaps,
        end: 10 + n * period,
    }
}

pub fn run_all(h: &mut Host) {
    h.run_until(|_| false).unwrap();
}
