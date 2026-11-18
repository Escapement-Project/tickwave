//! The host facing decoders that do anything: a played decoder emits what the fuzz input says,
//! turn by turn, alone on a wire and with another stacked on it, while the host steps, keeps
//! checkpoints, derives and retires windows, seeks, replaces runs and has a turn held out. A run
//! must end without a panic, and the host must stay inside the bounds its options set, whatever
//! the decoders emit; a refusal is the host doing its job.

use arbitrary::Arbitrary;
use escapement_tickwave_decode::{
    AnalysisId, Cause, Decoder, Definition, Delivery, Element, ElementType, Exchange, FieldId,
    FieldType, Host, HostOptions, Id, InputSpec, Keeping, KindDecl, KindId, LinkDecl, LinkId,
    LinkTo, Mode, Need, Observation, Observed, Pause, Reason, Ref, Request, Role, Schema, SeqId,
    Source, SourceError, Span, Status, Subscription, Tick, Turn, Value, Window, WindowId, field,
    name, seq,
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

/// A value a played decoder sets, any kind against any field.
#[derive(Arbitrary, Clone, Debug)]
pub enum Val {
    Phase,
    Bool(bool),
    Word(u64, u8),
    Name(u16),
    Text(String),
    Bits(Vec<u8>, u32),
    Unknown(u16, Option<u16>),
}

impl Val {
    fn value(&self) -> Value {
        match self {
            Val::Phase => Value::Phase,
            Val::Bool(b) => Value::Bool(*b),
            Val::Word(value, bits) => Value::Word {
                value: *value,
                bits: *bits,
            },
            Val::Name(n) => Value::Name(*n),
            Val::Text(t) => Value::Text(t.clone()),
            Val::Bits(bytes, bits) => Value::Bits {
                bytes: bytes.clone(),
                bits: *bits,
            },
            Val::Unknown(reason, needs) => Value::Unknown {
                reason: Reason(*reason),
                needs: needs.map(Need),
            },
        }
    }
}

/// One thing a played decoder does in its turn. Ticks are offsets from where it has read to;
/// items are named by their place among those it has opened, or received from upstream.
#[derive(Arbitrary, Clone, Debug)]
pub enum Act {
    /// An item of the first kind opened and closed after the last pulse: output a stacked
    /// decoder takes, and new each time a script repeats.
    Pulse {
        gap: u8,
        len: u8,
    },
    Open {
        kind: u8,
        anchor: i8,
        parent: Option<u8>,
        start: i8,
    },
    Set {
        item: u8,
        field: u8,
        value: Val,
        span: Option<(i8, u8)>,
        driver: Option<u8>,
    },
    Extend {
        item: u8,
        seq: u8,
        offset: u8,
        elements: Vec<(u8, i8, u8)>,
    },
    Link {
        item: u8,
        link: u8,
        other: u8,
        upstream: bool,
    },
    Close {
        item: u8,
        end: i8,
        cause: Option<u8>,
    },
    Frontier(i8),
    Lost(i8, u8),
    Request(i8, u8),
    /// Takes the reply, if one is ready, and reads on from its end.
    Reply,
    /// Takes up to this many deliveries, declaring the losses among them again where `again`.
    Receive {
        most: u8,
        again: bool,
    },
    Finish(i8),
    /// Ends the turn waiting for input, though the script has more.
    Wait,
}

/// A decoder that plays its script, a turn's acts a turn, asking for another while it has more,
/// and plays it again `repeat` times.
#[derive(Clone)]
struct Played {
    script: Vec<Vec<Act>>,
    repeat: u8,
    turns: VecDeque<Vec<Act>>,
    /// Where the last pulse ended.
    pulsed: Tick,
    opened: Vec<Id>,
    upstream: Vec<Id>,
    at: Tick,
}

impl Played {
    fn new(script: Vec<Vec<Act>>, repeat: u8, local: Option<Tick>) -> Self {
        Self {
            turns: script.clone().into(),
            script,
            repeat,
            pulsed: local.unwrap_or(0),
            opened: vec![],
            upstream: vec![],
            at: local.unwrap_or(0),
        }
    }

    fn item(&self, k: u8) -> Id {
        (self
            .opened
            .get(usize::from(k) % self.opened.len().max(1))
            .copied())
        .unwrap_or(Id::new(KindId(0), self.at))
    }

    fn t(&self, off: i8) -> Tick {
        self.at.saturating_add(Tick::from(off))
    }
}

impl Decoder for Played {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        if self.turns.is_empty() && self.repeat > 0 {
            self.repeat -= 1;
            self.turns = self.script.clone().into();
        }
        let Some(acts) = self.turns.pop_front() else {
            return Pause::Input;
        };
        let mut pause = match self.turns.is_empty() && self.repeat == 0 {
            true => Pause::Input,
            false => Pause::Work,
        };
        for act in acts {
            match act {
                Act::Pulse { gap, len } => {
                    let start = self.pulsed.saturating_add(Tick::from(gap));
                    let end = start.saturating_add(Tick::from(len));
                    let id = Id::new(KindId(0), start);
                    ex.out.open(id, None, start);
                    ex.out.close(id, end, Status::Complete);
                    self.pulsed = end.saturating_add(1);
                }
                Act::Open {
                    kind,
                    anchor,
                    parent,
                    start,
                } => {
                    // Now and then a kind the schema does not have.
                    let kind = if kind == u8::MAX {
                        9
                    } else {
                        u16::from(kind % 2)
                    };
                    let id = Id::new(KindId(kind), self.t(anchor));
                    let parent = parent.map(|p| self.item(p));
                    ex.out.open(id, parent, self.t(start));
                    self.opened.push(id);
                }
                Act::Set {
                    item,
                    field,
                    value,
                    span,
                    driver,
                } => {
                    let (id, f, v) = (
                        self.item(item),
                        FieldId(u16::from(field % 8)),
                        value.value(),
                    );
                    match span {
                        Some((a, len)) => {
                            let start = self.t(a);
                            let span = Span {
                                start,
                                end: start.saturating_add(Tick::from(len)),
                            };
                            let role = driver.map(|r| Role(u16::from(r % 3)));
                            ex.out.part(id, f, v, span, role);
                        }
                        None => ex.out.set(id, f, v),
                    }
                }
                Act::Extend {
                    item,
                    seq,
                    offset,
                    elements,
                } => {
                    let els: Vec<Element> = (elements.iter())
                        .map(|&(v, at, len)| {
                            let at = self.t(at);
                            let cell = Span {
                                start: at,
                                end: at.saturating_add(Tick::from(len)),
                            };
                            Element::new(v % 4, Observation::Wire(at), cell)
                        })
                        .collect();
                    let s = SeqId(u16::from(seq % 3));
                    ex.out.extend(self.item(item), s, u32::from(offset), &els);
                }
                Act::Link {
                    item,
                    link,
                    other,
                    upstream,
                } => {
                    let other = match (upstream, self.upstream.is_empty()) {
                        (true, false) => {
                            Ref::Upstream(self.upstream[usize::from(other) % self.upstream.len()])
                        }
                        _ => Ref::Own(self.item(other)),
                    };
                    ex.out
                        .link(self.item(item), LinkId(u16::from(link % 3)), other);
                }
                Act::Close { item, end, cause } => {
                    let status = match cause {
                        Some(c) => Status::Incomplete(Cause(u16::from(c % 2))),
                        None => Status::Complete,
                    };
                    ex.out.close(self.item(item), self.t(end), status);
                }
                Act::Frontier(t) => ex.frontier(self.t(t)),
                Act::Lost(from, len) => {
                    let from = self.t(from);
                    ex.lost(from, from.saturating_add(Tick::from(len)));
                }
                Act::Request(from, len) => {
                    let from = self.t(from);
                    let upto = from.saturating_add(Tick::from(len));
                    ex.request(Request::Window { from, upto });
                }
                Act::Reply => {
                    if let Some(w) = ex.reply() {
                        self.at = w.upto;
                    }
                }
                Act::Receive { most, again } => {
                    for _ in 0..most {
                        let Some(d) = ex.receive() else { break };
                        match d {
                            Delivery::Item { item, .. } => self.upstream.push(item.id),
                            Delivery::Piece { piece, .. } => self.upstream.push(*piece.op().id()),
                            Delivery::Lost { span, .. } if again => ex.lost(span.start, span.end),
                            Delivery::Frontier { start, .. } => self.at = self.at.max(start),
                            _ => {}
                        }
                    }
                    self.upstream.truncate(64);
                }
                Act::Finish(t) => {
                    ex.finish(self.t(t));
                    pause = Pause::Finished;
                }
                Act::Wait => pause = Pause::Input,
            }
        }
        pause
    }

    fn save(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(self.clone()))
    }
}

/// Two kinds, the second a child of the first, with a field and an element of every type, links
/// to its own kinds and, stacked, to the upstream's.
fn schema(stacked: bool) -> Schema {
    let mut links = vec![LinkDecl::new("next", vec![LinkTo::Own(name("a"))])];
    if stacked {
        links.push(LinkDecl::new("from", vec![LinkTo::Upstream(name("a"))]));
    }
    Schema::new(vec![
        KindDecl::new("a")
            .with_lanes(2)
            .with_fields(vec![
                field("w", FieldType::Word { bits: 8 }),
                field("n", FieldType::Name(vec![name("x"), name("y")])),
                field("b", FieldType::Bool),
                field("s", FieldType::Bits { max: 16 }),
                field("t", FieldType::Text { max: 8 }),
                field("p", FieldType::Phase),
            ])
            .with_sequences(vec![
                seq("bits", ElementType::Bit),
                seq("names", ElementType::Name(vec![name("u"), name("v")])),
            ])
            .with_links(links),
        KindDecl::new("b").with_parents(["a"]).with_row(1),
    ])
    .with_roles(["probe", "target"])
    .with_reasons(["a reason"])
    .with_needs(["a need"])
    .with_causes(["a cause", "another"])
    .with_open_at_most(4)
}

/// One line with an edge every `every` ticks up to `end`, cut at `most` ticks a read.
#[derive(Clone)]
struct Ticks {
    every: Tick,
    end: Tick,
}

impl Source for Ticks {
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
        w.events.push(Observed::Start {
            port: 0,
            level: (from > 0 && from <= self.end).then_some(false),
        });
        let first = (from.max(1) + self.every - 1) / self.every * self.every;
        let mut t = first;
        let mut taken = 0;
        while t < upto.min(self.end) {
            if taken == most.max(1) {
                w.upto = t;
                break;
            }
            let level = (t / self.every) % 2 == 1;
            w.events.push(Observed::Edge {
                at: t,
                port: 0,
                level,
            });
            taken += 1;
            t += self.every;
        }
        w.end = (self.end < w.upto).then_some(self.end);
        Ok(Some(w))
    }

    fn binding(&self) -> u64 {
        escapement_tickwave_decode::fold([self.every as u64, self.end as u64])
    }

    fn fork(&self) -> Option<Box<dyn Source>> {
        Some(Box::new(self.clone()))
    }
}

/// What the host does between the decoders' turns.
#[derive(Arbitrary, Debug)]
pub enum Control {
    Step(u8),
    Keep(u8),
    Derive(i16, u8),
    Retire(u8),
    Seek(i16),
    Replace(Option<i16>),
    Interrupt,
    /// Takes the wire decoder's turn out, as a worker would.
    Hold,
    /// Runs the held turn and hands it back.
    Return,
}

#[derive(Arbitrary, Debug)]
pub struct HostCase {
    pub wire: Vec<Vec<Act>>,
    /// How many times more each decoder plays its script.
    pub repeat: u8,
    /// A decoder stacked on the wire's, on pieces or completed items, and its script.
    pub stacked: Option<(bool, Vec<Vec<Act>>)>,
    /// Turn deliveries, inbox, recent ids, open items, window turns, window ticks, batch ops.
    pub bounds: [u8; 7],
    pub every: u8,
    pub end: u16,
    pub control: Vec<Control>,
}

fn definition(stacked: bool) -> Arc<Definition> {
    Arc::new(Definition::new(if stacked { "s" } else { "w" }, 1, schema(stacked)).with_ports(["l"]))
}

/// The bounds the host promises, after anything: no run holds more items open than the schema
/// allows, no inbox grows past its bound by more than one batch's deliveries, and a forward run,
/// keeping nothing, holds no more losses than its unsettled bound and one batch's.
fn check(h: &Host, o: &HostOptions, analyses: &[AnalysisId], windows: &[WindowId]) {
    for &a in analyses {
        let lost = h.lost(a).len();
        let most = o.recent_at_most + o.batch_ops;
        assert!(lost <= most, "{lost} losses held, past {most}");
        let mut stores = vec![h.store(a)];
        stores.extend(
            windows
                .iter()
                .filter_map(|&w| h.window_run(w, a))
                .map(|r| r.store()),
        );
        for s in stores {
            assert!(s.open_count() <= 4, "{} open", s.open_count());
        }
    }
    let inbox = h.stats().peak_inbox;
    let most = o.inbox_at_most + o.batch_ops + 2;
    assert!(inbox <= most, "an inbox held {inbox}, past {most}");
}

pub fn run(case: &HostCase) {
    reach(case);
}

/// Runs a case: what the host did, and how many windows it derived.
pub fn reach(case: &HostCase) -> (escapement_tickwave_decode::Stats, usize) {
    let mut o = HostOptions::default();
    let b = case.bounds.map(usize::from);
    o.turn_deliveries = b[0] % 64 + 1;
    o.inbox_at_most = b[1] % 64 + 1;
    o.recent_at_most = b[2] + 1;
    o.open_at_most = 4 + b[3] % 8;
    o.window_turns = b[4] % 8;
    o.window_ticks = b[5] % 64 + 1;
    o.batch_ops = b[6] + 1;
    o.log = Some(64);
    let mut h = Host::new(o);
    let source = Ticks {
        every: Tick::from(case.every).max(1),
        end: Tick::from(case.end),
    };
    let (script, repeat) = (case.wire.clone(), case.repeat);
    let factory =
        move |local| Box::new(Played::new(script.clone(), repeat, local)) as Box<dyn Decoder>;
    let input = InputSpec::Wire {
        source: Box::new(source),
        local: None,
    };
    let Ok(wire) = h.add(
        name("w"),
        definition(false),
        input,
        Box::new(factory),
        vec![],
    ) else {
        return (h.stats(), 0);
    };
    let mut derived = 0;
    let mut analyses = vec![wire];
    if let Some((pieces, script)) = &case.stacked {
        let script = script.clone();
        let factory =
            move |local| Box::new(Played::new(script.clone(), repeat, local)) as Box<dyn Decoder>;
        let sub = match pieces {
            true => Subscription::new(wire, Mode::Pieces)
                .with_kinds(["a", "b"])
                .with_sequences(["bits"]),
            false => Subscription::new(wire, Mode::Completed).with_kinds(["a", "b"]),
        };
        let input = InputSpec::Upstream(sub);
        if let Ok(s) = h.add(
            name("s"),
            definition(true),
            input,
            Box::new(factory),
            vec![],
        ) {
            analyses.push(s);
        }
    }
    let mut windows: Vec<WindowId> = vec![];
    let mut held: Option<Turn> = None;
    for c in &case.control {
        match c {
            Control::Step(n) => {
                for _ in 0..*n {
                    let ran = h.step();
                    check(&h, &o, &analyses, &windows);
                    if ran == Ok(false) {
                        break;
                    }
                }
            }
            Control::Keep(every) => {
                let _ = h.keep_checkpoints(wire, Keeping::new(Tick::from(*every) + 1, 1 << 16));
            }
            Control::Derive(start, len) => {
                let start = Tick::from(*start);
                let window = Span {
                    start,
                    end: start + Tick::from(*len) + 1,
                };
                if let Ok(d) = h.derive(wire, window) {
                    windows.push(d.window);
                    derived += 1;
                }
            }
            Control::Retire(k) => {
                if !windows.is_empty() {
                    let w = windows.remove(usize::from(*k) % windows.len());
                    h.retire(w);
                }
            }
            Control::Seek(t) => {
                let _ = h.seek(wire, Tick::from(*t));
            }
            Control::Replace(local) => {
                h.replace(wire, local.map(Tick::from));
            }
            Control::Interrupt => h.interrupt(wire).store(true, Relaxed),
            Control::Hold => {
                if held.is_none() {
                    held = h.begin_turn(wire).ok();
                }
            }
            Control::Return => {
                if let Some(mut t) = held.take() {
                    t.run();
                    let _ = h.end_turn(t);
                }
            }
        }
        check(&h, &o, &analyses, &windows);
    }
    if let Some(t) = held.take() {
        let _ = h.end_turn(t);
    }
    for _ in 0..4096 {
        if h.step() == Ok(false) {
            break;
        }
    }
    check(&h, &o, &analyses, &windows);
    (h.stats(), derived)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stacked decoder that never takes its deliveries holds its upstream back.
    #[test]
    fn a_full_inbox_holds_the_upstream() {
        let case = HostCase {
            wire: vec![vec![Act::Pulse { gap: 1, len: 1 }]],
            repeat: 9,
            stacked: Some((true, vec![])),
            bounds: [0, 0, 255, 0, 0, 0, 3],
            every: 1,
            end: 100,
            control: vec![Control::Step(255)],
        };
        let (s, _) = reach(&case);
        assert_eq!(s.admitted, 1, "one batch, then the inbox is full");
    }

    /// What the corpus reaches: `cargo test --lib -- --ignored --nocapture reach`.
    #[test]
    #[ignore = "reads fuzz/corpus/host"]
    fn reach_of_the_corpus() {
        let (mut n, mut admitted, mut delivered, mut stale, mut derived) = (0, 0, 0, 0, 0);
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/host");
        for e in std::fs::read_dir(dir).unwrap() {
            let bytes = std::fs::read(e.unwrap().path()).unwrap();
            let mut u = arbitrary::Unstructured::new(&bytes);
            let Ok(case) = HostCase::arbitrary(&mut u) else {
                continue;
            };
            let (s, d) = reach(&case);
            n += 1;
            admitted += s.admitted;
            delivered += s.delivered;
            stale += s.stale;
            derived += d;
        }
        println!(
            "{n} cases: {admitted} batches admitted, {delivered} deliveries, {stale} stale, \
             {derived} windows derived"
        );
    }
}
