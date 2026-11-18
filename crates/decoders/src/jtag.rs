//! JTAG (IEEE 1149.1): the TAP controller's state followed from TMS at each TCK rise, and the IR
//! and DR scans it shifts, with their TDI and TDO bits.
//!
//! A state held for more than one clock is an item. States passed in one clock join a path item
//! from the held state before them to the one after, at most `PATH_AT_MOST` long; Shift and
//! Test-Logic-Reset are items even for one clock. The state is unknown from the start, and again
//! after lost input, until TMS forces it. A scan's bits are emitted in chunks as they shift; its
//! first 128 bits are a field once it passes them, and the whole of a scan of at most 256 bits
//! once it closes.

use escapement_tickwave_decode::{
    Cause, Decoder, Definition, Element, ElementType, Exchange, Factory, FieldId, FieldType, Id,
    KindDecl, KindId, MAX_BITS, Need, Observation, Observed, Out, Pause, Reason, Request, Schema,
    SeqId, Span, Status, Tick, Value, Window, field, name, seq,
};
use std::sync::Arc;

pub const TCK: u8 = 0;
pub const TMS: u8 = 1;
pub const TDI: u8 = 2;
pub const TDO: u8 = 3;

pub const STATE: KindId = KindId(0);
pub const PATH: KindId = KindId(1);
pub const IR_SCAN: KindId = KindId(2);
pub const DR_SCAN: KindId = KindId(3);

/// `STATE`'s field.
pub const TAP_STATE: FieldId = FieldId(0);
/// `PATH`'s fields and sequence.
pub const FROM: FieldId = FieldId(0);
pub const TO: FieldId = FieldId(1);
pub const CLOCKS: FieldId = FieldId(2);
pub const VIA: SeqId = SeqId(0);
/// A scan's fields and sequences.
pub const TDI_VALUE: FieldId = FieldId(0);
pub const TDO_VALUE: FieldId = FieldId(1);
pub const TDI_HEAD: FieldId = FieldId(2);
pub const TDO_HEAD: FieldId = FieldId(3);
pub const LENGTH: FieldId = FieldId(4);
pub const TDI_BITS: SeqId = SeqId(0);
pub const TDO_BITS: SeqId = SeqId(1);

pub const NO_TDO: Reason = Reason(0);
pub const TWO_WIRE: Reason = Reason(1);
pub const MID_SESSION: Reason = Reason(2);
pub const LOST_STATE: Reason = Reason(3);
pub const NO_STATE_BEFORE: Reason = Reason(4);
pub const NEEDS_TDO: Need = Need(0);

pub const GAP_CUT: Cause = Cause(0);
pub const ENDS_MID_SCAN: Cause = Cause(1);

pub const STATES: [&str; 16] = [
    "Test-Logic-Reset",
    "Run-Test/Idle",
    "Select-DR",
    "Capture-DR",
    "Shift-DR",
    "Exit1-DR",
    "Pause-DR",
    "Exit2-DR",
    "Update-DR",
    "Select-IR",
    "Capture-IR",
    "Shift-IR",
    "Exit1-IR",
    "Pause-IR",
    "Exit2-IR",
    "Update-IR",
];
/// The next state for TMS low and high.
const NEXT: [[u8; 2]; 16] = [
    [1, 0],
    [1, 2],
    [3, 9],
    [4, 5],
    [4, 5],
    [6, 8],
    [6, 7],
    [4, 8],
    [1, 2],
    [10, 0],
    [11, 12],
    [11, 12],
    [13, 15],
    [13, 14],
    [11, 15],
    [1, 2],
];
const RESET: u8 = 0;
const SHIFT_DR: u8 = 4;
const UPDATE_DR: u8 = 8;
const SHIFT_IR: u8 = 11;
const UPDATE_IR: u8 = 15;
const ALL: u16 = 0xFFFF;

pub fn schema() -> Schema {
    let state = || FieldType::Name(STATES.map(name).to_vec());
    let scan = |kind: &'static str| {
        KindDecl::new(kind)
            .with_fields(vec![
                field("tdi", FieldType::Bits { max: MAX_BITS }),
                field("tdo", FieldType::Bits { max: MAX_BITS }),
                field("tdi_head", FieldType::Bits { max: 128 }),
                field("tdo_head", FieldType::Bits { max: 128 }),
                field("length", FieldType::Word { bits: 32 }),
            ])
            .with_sequences(vec![
                seq("tdi", ElementType::Bit),
                seq("tdo", ElementType::Bit),
            ])
    };
    Schema::new(vec![
        KindDecl::new("state").with_fields(vec![field("state", state())]),
        KindDecl::new("path")
            .with_fields(vec![
                field("from", state()),
                field("to", state()),
                field("clocks", FieldType::Word { bits: 32 }),
            ])
            .with_sequences(vec![seq(
                "via",
                ElementType::Name(STATES.map(name).to_vec()),
            )]),
        scan("ir-scan"),
        scan("dr-scan"),
    ])
    .with_reasons([
        "the recording has no TDO channel",
        "cJTAG's Standard Protocol carries only TMS on its two wires",
        "the recording starts mid-session, before anything forces Test-Logic-Reset",
        "the TAP state is lost in a recording gap",
        "no state was decoded before it",
    ])
    .with_needs(["a TDO channel"])
    .with_causes([
        "a recording gap interrupts the scan",
        "the recording ends mid-scan",
    ])
    .with_open_at_most(2)
}

pub fn definition() -> Arc<Definition> {
    Arc::new(Definition::new("jtag", 1, schema()).with_ports(["TCK", "TMS", "TDI", "TDO"]))
}

/// The states possible after one TCK rise with `tms`, from a set of possible states.
pub fn step(set: u16, tms: bool) -> u16 {
    NEXT.iter()
        .enumerate()
        .filter(|(s, _)| set & (1 << s) != 0)
        .fold(0, |n, (_, next)| n | 1 << next[usize::from(tms)])
}

pub(crate) fn single(set: u16) -> Option<u8> {
    (set.count_ones() == 1).then(|| set.trailing_zeros() as u8)
}

fn pack(bits: &[bool]) -> Value {
    let bytes = bits
        .chunks(8)
        .map(|c| (c.iter().enumerate()).fold(0u8, |b, (k, &v)| b | u8::from(v) << k))
        .collect();
    Value::Bits {
        bytes,
        bits: bits.len() as u32,
    }
}

const fn unknown(reason: Reason) -> Value {
    Value::Unknown {
        reason,
        needs: None,
    }
}

/// One TCK rise with the levels sampled at it, and the cell a view draws for its bit.
#[derive(Clone, Copy, Debug)]
pub struct Clocked {
    pub rise: Tick,
    pub cell: Span,
    pub tms: bool,
    /// None where the clock carries no TDI: cJTAG's Standard Protocol on two wires.
    pub tdi: Option<bool>,
    pub tdo: Option<bool>,
    /// The upstream elements the values were decoded from; None where they are the wire's levels
    /// at `rise`.
    pub from: Option<Inputs>,
}

#[derive(Clone, Copy, Debug)]
pub struct Inputs {
    pub tms: Observation,
    pub tdi: Option<Observation>,
    pub tdo: Option<Observation>,
}

impl Clocked {
    fn tms_at(&self) -> Observation {
        self.from.map_or(Observation::Wire(self.rise), |f| f.tms)
    }
    fn tdi_at(&self) -> Observation {
        (self.from.and_then(|f| f.tdi)).unwrap_or(Observation::Wire(self.rise))
    }
    fn tdo_at(&self) -> Observation {
        (self.from.and_then(|f| f.tdo)).unwrap_or(Observation::Wire(self.rise))
    }
}

/// The most elements a scan holds before emitting them.
const CHUNK: usize = 64;

#[derive(Clone, Debug)]
struct Scan {
    id: Id,
    /// Bits shifted.
    n: u32,
    latest: Tick,
    /// Elements not yet emitted, and how many each sequence has had in all.
    tdi: Vec<Element>,
    tdo: Vec<Element>,
    tdi_n: u32,
    tdo_n: u32,
    /// The first `MAX_BITS` bits, unknown ones as low.
    head_tdi: Vec<bool>,
    head_tdo: Vec<bool>,
    /// Whether the scan's clocks carry TDI and TDO, taken from its first clock.
    tdi_known: bool,
    tdo_known: bool,
}

/// The current state, None while ambiguous, from the rise that entered it, and its item once
/// opened. A state's item opens when the state lasts a second clock, at once for Shift and
/// Test-Logic-Reset, and at once for an unknown state.
#[derive(Clone, Debug)]
struct Run {
    state: Option<u8>,
    start: Tick,
    /// The TMS observation that entered the state.
    enter: Observation,
    id: Option<Id>,
}

/// The most one-clock states a path holds before it is emitted, so TMS cycling through one-clock
/// states cannot grow the decoder's state.
pub const PATH_AT_MOST: usize = 64;

/// The TAP controller and its scans, clock by clock, whatever the clocks are decoded from.
#[derive(Clone, Debug)]
pub struct Tap {
    set: u16,
    run: Run,
    /// One-clock states since the last held one, as `VIA` elements.
    path: Vec<Element>,
    /// The held state before `path`.
    from: Option<Value>,
    scan: Option<Scan>,
    has_tdo: bool,
    /// Why the state is unknown when it is.
    lost: Reason,
}

impl Tap {
    /// The bytes its path and the scan under way hold.
    pub fn heap_bytes(&self) -> usize {
        let scan = self.scan.as_ref().map_or(0, |s| {
            crate::held::<Element>(s.tdi.capacity() + s.tdo.capacity())
                + crate::held::<bool>(s.head_tdi.capacity() + s.head_tdo.capacity())
        });
        crate::held::<Element>(self.path.capacity()) + scan
    }
}

impl Tap {
    /// The TAP from `start`, where its state is not known. `begin` opens that state's item.
    pub fn new(start: Tick, has_tdo: bool) -> Self {
        Self {
            set: ALL,
            run: Run {
                state: None,
                start,
                enter: Observation::Wire(start),
                id: None,
            },
            path: vec![],
            from: None,
            scan: None,
            has_tdo,
            lost: MID_SESSION,
        }
    }

    /// Opens the unknown state's item at the start. Called once, before any clock.
    pub fn begin(&mut self, out: &mut Out) {
        self.open_run(out);
    }

    /// The earliest a new item or element can start, given that no clock still to come has a cell
    /// starting before `next`: the current state's item while it has not opened, the pending
    /// path, or a scan's bits not yet emitted.
    pub fn start_frontier(&self, next: Tick) -> Tick {
        let mut f = next;
        if self.run.id.is_none() {
            f = f.min(self.run.start);
        }
        if let Some(p) = self.path.first() {
            f = f.min(p.cell.start);
        }
        if let Some(sc) = &self.scan {
            for e in sc.tdi.first().into_iter().chain(sc.tdo.first()) {
                f = f.min(e.cell.start);
            }
        }
        f
    }

    fn value(&self, state: Option<u8>) -> Value {
        match state {
            Some(s) => Value::Name(s.into()),
            None => unknown(self.lost),
        }
    }

    /// Emits the one-clock states since the last held state as one path, ending at `to`.
    fn flush_path(&mut self, to: Value, out: &mut Out) {
        let (Some(first), Some(last)) = (self.path.first(), self.path.last()) else {
            return;
        };
        let (start, end) = (first.cell.start, last.cell.end);
        let id = Id::new(PATH, start);
        out.open(id, None, start);
        let from = self.from.clone().unwrap_or(unknown(NO_STATE_BEFORE));
        out.set(id, FROM, from);
        out.set(id, TO, to);
        out.extend(id, VIA, 0, &self.path);
        let clocks = self.path.len() as u64;
        out.set(
            id,
            CLOCKS,
            Value::Word {
                value: clocks,
                bits: 32,
            },
        );
        out.close(id, end, Status::Complete);
        self.path.clear();
    }

    /// Opens the current state's item, after the path that led to it.
    fn open_run(&mut self, out: &mut Out) {
        if self.run.id.is_some() {
            return;
        }
        let v = self.value(self.run.state);
        self.flush_path(v.clone(), out);
        let id = Id::new(STATE, self.run.start);
        out.open(id, None, self.run.start);
        out.set(id, TAP_STATE, v);
        self.run.id = Some(id);
    }

    /// Ends the current state at `at`: its item closes, or a one-clock state joins the path.
    fn end_run(&mut self, at: Tick, out: &mut Out) {
        if let Some(id) = self.run.id.take() {
            out.close(id, at, Status::Complete);
            self.from = Some(self.value(self.run.state));
        } else if let Some(s) = self.run.state {
            let cell = Span {
                start: self.run.start,
                end: at,
            };
            self.path.push(Element::new(s, self.run.enter, cell));
        }
    }

    fn flush(sc: &mut Scan, out: &mut Out) {
        if !sc.tdi.is_empty() {
            let at = sc.tdi_n.saturating_sub(sc.tdi.len() as u32);
            out.extend(sc.id, TDI_BITS, at, &sc.tdi);
            sc.tdi.clear();
        }
        if !sc.tdo.is_empty() {
            let at = sc.tdo_n.saturating_sub(sc.tdo.len() as u32);
            out.extend(sc.id, TDO_BITS, at, &sc.tdo);
            sc.tdo.clear();
        }
    }

    fn close_scan(&mut self, end: Tick, status: Status, out: &mut Out) {
        let Some(mut sc) = self.scan.take() else {
            return;
        };
        Self::flush(&mut sc, out);
        let n = u64::from(sc.n);
        out.set(sc.id, LENGTH, Value::Word { value: n, bits: 32 });
        if sc.n <= MAX_BITS {
            if sc.tdi_known {
                out.set(sc.id, TDI_VALUE, pack(&sc.head_tdi));
            }
            if sc.tdo_known {
                out.set(sc.id, TDO_VALUE, pack(&sc.head_tdo));
            }
        }
        out.close(sc.id, end.max(sc.latest), status);
    }

    fn open_scan(&mut self, ir: bool, c: &Clocked, out: &mut Out) {
        let id = Id::new(if ir { IR_SCAN } else { DR_SCAN }, c.rise);
        out.open(id, None, c.cell.start);
        let (tdi_known, tdo_known) = (c.tdi.is_some(), self.has_tdo && c.tdo.is_some());
        if !tdi_known {
            out.set(id, TDI_VALUE, unknown(TWO_WIRE));
        }
        if !self.has_tdo {
            let v = Value::Unknown {
                reason: NO_TDO,
                needs: Some(NEEDS_TDO),
            };
            out.set(id, TDO_VALUE, v);
        } else if !tdo_known {
            out.set(id, TDO_VALUE, unknown(TWO_WIRE));
        }
        self.scan = Some(Scan {
            id,
            n: 0,
            latest: c.cell.start,
            tdi: Vec::with_capacity(CHUNK),
            tdo: Vec::with_capacity(CHUNK),
            tdi_n: 0,
            tdo_n: 0,
            head_tdi: vec![],
            head_tdo: vec![],
            tdi_known,
            tdo_known,
        });
    }

    fn shift(sc: &mut Scan, c: &Clocked, out: &mut Out) {
        if let (Some(v), true) = (c.tdi, sc.tdi_known) {
            sc.tdi.push(Element::new(u8::from(v), c.tdi_at(), c.cell));
            sc.tdi_n = sc.tdi_n.saturating_add(1);
        }
        if let (Some(v), true) = (c.tdo, sc.tdo_known) {
            sc.tdo.push(Element::new(u8::from(v), c.tdo_at(), c.cell));
            sc.tdo_n = sc.tdo_n.saturating_add(1);
        }
        if sc.head_tdi.len() < MAX_BITS as usize {
            sc.head_tdi.push(c.tdi.unwrap_or(false));
            sc.head_tdo.push(c.tdo.unwrap_or(false));
        }
        sc.n = sc.n.saturating_add(1);
        sc.latest = sc.latest.max(c.cell.end);
        if sc.n == 129 {
            if let (true, Some(h)) = (sc.tdi_known, sc.head_tdi.get(..128)) {
                out.set(sc.id, TDI_HEAD, pack(h));
            }
            if let (true, Some(h)) = (sc.tdo_known, sc.head_tdo.get(..128)) {
                out.set(sc.id, TDO_HEAD, pack(h));
            }
        }
        if sc.tdi.len() >= CHUNK || sc.tdo.len() >= CHUNK {
            Self::flush(sc, out);
        }
    }

    pub fn clock(&mut self, c: &Clocked, out: &mut Out) {
        let before = self.set;
        if let Some(s @ (SHIFT_DR | SHIFT_IR)) = single(before) {
            if self.scan.is_none() {
                self.open_scan(s == SHIFT_IR, c, out);
            }
            if let Some(sc) = self.scan.as_mut() {
                Self::shift(sc, c, out);
            }
        }
        let after = step(before, c.tms);
        let changed = single(before) != single(after);
        let acts = matches!(self.run.state, Some(SHIFT_DR | SHIFT_IR | RESET));
        if !changed || acts {
            self.open_run(out);
        }
        if changed {
            self.end_run(c.rise, out);
            self.run = Run {
                state: single(after),
                start: c.rise,
                enter: c.tms_at(),
                id: None,
            };
            if self.path.len() >= PATH_AT_MOST {
                let last = self.path.last().map(|p| Value::Name(p.value.into()));
                self.flush_path(self.value(single(after)), out);
                self.from = last;
            }
        }
        self.set = after;
        if matches!(single(after), Some(UPDATE_DR | UPDATE_IR)) {
            self.close_scan(c.rise, Status::Complete, out);
        }
    }

    /// Input lost from `from` to `to`: a scan it cuts closes incomplete, and the state is unknown
    /// from `to`.
    pub fn gap(&mut self, from: Tick, to: Tick, out: &mut Out) {
        self.close_scan(from, Status::Incomplete(GAP_CUT), out);
        self.open_run(out);
        self.end_run(from, out);
        self.set = ALL;
        self.lost = LOST_STATE;
        self.run = Run {
            state: None,
            start: to,
            enter: Observation::Wire(to),
            id: None,
        };
        self.open_run(out);
    }

    pub fn finish(&mut self, end: Tick, out: &mut Out) {
        self.close_scan(end, Status::Incomplete(ENDS_MID_SCAN), out);
        self.open_run(out);
        self.end_run(end, out);
    }
}

/// A rise waiting for the fall after it, which ends its cell: the fall before it in TCK's known
/// stretch, and the levels sampled.
#[derive(Clone, Copy, Debug)]
struct Rise {
    at: Tick,
    fall: Option<Tick>,
    tms: bool,
    tdi: bool,
    tdo: Option<bool>,
}

/// JTAG under the host: TCK, TMS, TDI and TDO in windows of `window` ticks. TMS, TDI and TDO are
/// sampled at each TCK rise: a change at the rise's own tick counts after it. Without TDO, port 3
/// is not read and TDO values are unknown. A stretch where a line it reads is unknown is lost
/// input.
///
/// A clock's cell runs from the fall before its rise to the fall after it, each side at most twice
/// the other, and never before where input became known; so a clock is decoded once its next fall
/// is read.
#[derive(Clone, Debug)]
pub struct Jtag {
    tap: Tap,
    started: bool,
    has_tdo: bool,
    next: Tick,
    window: Tick,
    /// Each port's level, None where it is not known.
    level: [Option<bool>; 4],
    /// Each port's level before its last change, and where that change was.
    before: [Option<(Tick, Option<bool>)>; 4],
    /// TCK's last fall in its current known stretch.
    fall: Option<Tick>,
    rise: Option<Rise>,
    /// Where input became known last: the run's start or a gap's end.
    floor: Tick,
    /// No clock still to come has a cell starting before this.
    bound: Tick,
    /// Where input became unknown, while it is.
    gap: Option<Tick>,
    /// A window an interrupted turn left part read, and the next observation in it.
    held: Option<(Window, usize)>,
    /// Whether TDO is sampled at the fall after each rise rather than at the rise.
    tdo_at_fall: bool,
}

impl Jtag {
    pub fn new(local: Option<Tick>, has_tdo: bool, window: Tick) -> Self {
        let at = local.unwrap_or(0);
        Self {
            tap: Tap::new(at, has_tdo),
            started: false,
            has_tdo,
            next: at,
            window: window.max(1),
            level: [None; 4],
            before: [None; 4],
            fall: None,
            rise: None,
            floor: at,
            bound: at,
            gap: None,
            held: None,
            tdo_at_fall: false,
        }
    }

    /// TDO sampled at the fall after each rise: a probe at a high TCK rate can read TDO late, and
    /// a recording of it then shows TDO changing near the rise. A clock with no fall after it, at
    /// a gap or the end, keeps the level at its rise.
    pub fn tdo_at_fall(mut self, on: bool) -> Self {
        self.tdo_at_fall = on;
        self
    }

    pub fn factory(has_tdo: bool, window: Tick) -> Factory {
        Self::factory_with(has_tdo, window, false)
    }

    /// `factory`, with TDO sampled at TCK's fall where `tdo_at_fall`.
    pub fn factory_with(has_tdo: bool, window: Tick, tdo_at_fall: bool) -> Factory {
        Box::new(move |local| Box::new(Self::new(local, has_tdo, window).tdo_at_fall(tdo_at_fall)))
    }

    fn port(&self, port: u8) -> Option<usize> {
        (port < 3 || (port == TDO && self.has_tdo)).then_some(usize::from(port))
    }

    fn known(&self) -> bool {
        let read = if self.has_tdo { 4 } else { 3 };
        self.level.iter().take(read).all(Option::is_some)
    }

    /// Port `p`'s level just before `at`.
    fn sampled(&self, p: u8, at: Tick) -> Option<bool> {
        let p = usize::from(p);
        match self.before.get(p).copied().flatten() {
            Some((t, before)) if t == at => before,
            _ => self.level.get(p).copied().flatten(),
        }
    }

    /// Where the next item can start at the earliest, with input read up to `upto`.
    fn frontier(&self, upto: Tick) -> Tick {
        let mut next = self.bound.min(upto);
        if let Some(r) = &self.rise {
            next = next.min(r.fall.unwrap_or(self.floor).max(self.floor));
        }
        if let Some(g) = self.gap {
            next = next.min(g);
        }
        self.tap.start_frontier(next)
    }

    /// Decodes a rise, its cell ended by `fall` and by `end` where they are known.
    fn emit(&mut self, r: Rise, fall: Option<Tick>, end: Option<Tick>, out: &mut Out) {
        let at = r.at;
        let l = r.fall.map(|f| at.saturating_sub(f));
        let h = fall.map(|f| f.saturating_sub(at));
        let (lo, hi) = match (l, h) {
            (Some(l), Some(h)) => (l.min(h.saturating_mul(2)), h.min(l.saturating_mul(2))),
            (Some(l), None) => (l, l),
            (None, Some(h)) => (h, h),
            (None, None) => (1, 1),
        };
        let hi = at.saturating_add(hi);
        let cell = Span {
            start: at.saturating_sub(lo).max(self.floor),
            end: end.map_or(hi, |e| hi.min(e)),
        };
        let c = Clocked {
            rise: at,
            cell,
            tms: r.tms,
            tdi: Some(r.tdi),
            tdo: r.tdo,
            from: None,
        };
        self.tap.clock(&c, out);
    }

    fn on_rise(&mut self, at: Tick, out: &mut Out) {
        if let Some(r) = self.rise.take() {
            self.emit(r, None, None, out);
        }
        let (Some(tms), Some(tdi)) = (self.sampled(TMS, at), self.sampled(TDI, at)) else {
            return;
        };
        let tdo = match (self.has_tdo, self.sampled(TDO, at)) {
            (false, _) => None,
            (true, Some(v)) => Some(v),
            (true, None) => return,
        };
        self.rise = Some(Rise {
            at,
            fall: self.fall,
            tms,
            tdi,
            tdo,
        });
    }

    fn on_fall(&mut self, at: Tick, out: &mut Out) {
        self.fall = Some(at);
        self.bound = self.bound.max(at);
        if let Some(mut r) = self.rise.take() {
            if self.tdo_at_fall && self.has_tdo {
                r.tdo = self.sampled(TDO, at).or(r.tdo);
            }
            self.emit(r, Some(at), None, out);
        }
    }

    /// Input became known again at `at`, after a gap.
    fn regain(&mut self, at: Tick, ex: &mut Exchange<'_>) {
        let Some(from) = self.gap.take() else {
            return;
        };
        if let Some(r) = self.rise.take() {
            self.emit(r, None, None, &mut ex.out);
        }
        if from < at {
            self.tap.gap(from, at, &mut ex.out);
            ex.lost(from, at);
        }
        self.floor = at;
        self.bound = self.bound.max(at);
    }

    fn observe(&mut self, e: Observed, w: &Window, ex: &mut Exchange<'_>) {
        match e {
            Observed::Start { port, level } => {
                let Some(p) = self.port(port) else {
                    return;
                };
                self.level[p] = level;
                if level.is_none() {
                    self.gap.get_or_insert(w.from);
                }
            }
            Observed::Edge { at, port, level } => {
                let Some(p) = self.port(port) else {
                    return;
                };
                let was = self.level[p].replace(level);
                self.before[p] = Some((at, was));
                if port == TCK {
                    match (was, level) {
                        (Some(false), true) => self.on_rise(at, &mut ex.out),
                        (Some(true), false) => self.on_fall(at, &mut ex.out),
                        _ => {}
                    }
                }
            }
            Observed::Known { at, port, level } => {
                let Some(p) = self.port(port) else {
                    return;
                };
                let was = self.level[p].replace(level);
                self.before[p] = Some((at, was));
                if self.known() {
                    self.regain(at, ex);
                }
            }
            Observed::Unknown { at, port } => {
                let Some(p) = self.port(port) else {
                    return;
                };
                let was = self.level[p].take();
                self.before[p] = Some((at, was));
                if w.end == Some(at) {
                    return;
                }
                if port == TCK {
                    if let Some(r) = self.rise.take() {
                        self.emit(r, None, None, &mut ex.out);
                    }
                    self.fall = None;
                }
                self.gap.get_or_insert(at);
            }
            _ => {}
        }
    }

    fn ask(&self, ex: &mut Exchange<'_>) {
        ex.request(Request::Window {
            from: self.next,
            upto: self.next.saturating_add(self.window),
        });
    }

    fn finish(&mut self, end: Tick, ex: &mut Exchange<'_>) {
        if let Some(r) = self.rise.take() {
            self.emit(r, None, Some(end), &mut ex.out);
        }
        if let Some(from) = self.gap.take()
            && from < end
        {
            self.tap.gap(from, end, &mut ex.out);
            ex.lost(from, end);
        }
        self.tap.finish(end, &mut ex.out);
        ex.finish(end);
    }
}

impl Decoder for Jtag {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        if !std::mem::replace(&mut self.started, true) {
            self.tap.begin(&mut ex.out);
        }
        let (w, first) = match self.held.take() {
            Some(held) => held,
            None => match ex.reply() {
                Some(w) => (w, 0),
                None => {
                    self.ask(ex);
                    return Pause::Input;
                }
            },
        };
        for k in first..w.events.len() {
            let Some(&e) = w.events.get(k) else {
                break;
            };
            if k > first && ex.interrupted() {
                ex.frontier(self.frontier(e.at().unwrap_or(w.from)));
                self.held = Some((w, k));
                return Pause::Work;
            }
            self.observe(e, &w, ex);
        }
        if let Some(end) = w.end {
            self.finish(end, ex);
            return Pause::Finished;
        }
        ex.frontier(self.frontier(w.upto));
        self.next = w.upto;
        self.ask(ex);
        Pause::Input
    }

    fn save(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(self.clone()))
    }

    fn heap_bytes(&self) -> usize {
        self.tap.heap_bytes() + crate::window_bytes(&self.held)
    }

    fn settings(&self) -> u64 {
        u64::from(self.has_tdo) | u64::from(self.tdo_at_fall) << 1
    }
}

/// The TCK edge TDO is sampled at, as `choose_tdo_edge` read it from the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct EdgeChoice {
    /// Whether TDO is sampled at TCK's fall after each rise.
    pub at_fall: bool,
    /// Of the first `scans` IR scans decoded each way, how many captured TDO ending in binary 01
    /// sampled at the rise, and at the fall.
    pub rise: u32,
    pub fall: u32,
    pub scans: u32,
}

/// Counts the first so many IR scans it receives, and those whose captured TDO ends in binary 01.
struct IrCaptures(Arc<std::sync::Mutex<(u32, u32)>>, u32);

impl Decoder for IrCaptures {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        use escapement_tickwave_decode::Delivery;
        while let Some(d) = ex.receive() {
            match d {
                Delivery::Item { item, .. } if item.id.kind == IR_SCAN => {
                    let ok = matches!(
                        item.field(TDO_VALUE).map(|f| &f.value),
                        Some(Value::Bits { bytes, .. }) if bytes.first().is_some_and(|b| b & 3 == 1)
                    );
                    if let Ok(mut n) = self.0.lock()
                        && n.0 < self.1
                    {
                        n.0 += 1;
                        n.1 += u32::from(ok);
                    }
                }
                Delivery::Lost { span, .. } => ex.lost(span.start, span.end),
                Delivery::Frontier { start, .. } => ex.frontier(start),
                Delivery::End { end, .. } => {
                    ex.finish(end);
                    return Pause::Finished;
                }
                _ => {}
            }
        }
        if ex.more() { Pause::Work } else { Pause::Input }
    }
}

/// Decodes the first `k` IR scans of the wires `source` gives twice, TDO sampled at TCK's rise
/// and at its fall, and picks the edge whose IR captures end in binary 01, as IEEE 1149.1 has
/// every IR capture do. A tie goes to the rise. A host labels the choice as read from the wire,
/// and a setting given by hand overrides it. The first `k` decide, so where the edge that reads
/// TDO depends on the TCK rate and the rate changes later in the capture, they may not tell.
pub fn choose_tdo_edge(
    source: impl Fn() -> Box<dyn escapement_tickwave_decode::Source>,
    window: Tick,
    k: u32,
) -> Result<EdgeChoice, escapement_tickwave_decode::BindError> {
    use escapement_tickwave_decode::{Host, InputSpec, Mode, Subscription};
    // Each edge in a host of its own, stopped at its kth IR scan: in one host, one chain would
    // run to the end before the other began.
    let mut counted = [(0u32, 0u32); 2];
    for (slot, at_fall) in counted.iter_mut().zip([false, true]) {
        let mut h = Host::default();
        let j = h.add(
            name("jtag"),
            definition(),
            InputSpec::Wire {
                source: source(),
                local: None,
            },
            Jtag::factory_with(true, window, at_fall),
            vec![],
        )?;
        let seen = Arc::new(std::sync::Mutex::new((0u32, 0u32)));
        let s = seen.clone();
        h.add(
            name("captures"),
            Arc::new(Definition::new("ir-captures", 1, Schema::new(vec![]))),
            InputSpec::Upstream(Subscription::new(j, Mode::Completed).with_kinds(["ir-scan"])),
            Box::new(move |_| Box::new(IrCaptures(s.clone(), k))),
            vec![],
        )?;
        let read = || seen.lock().map_or((0, 0), |n| *n);
        // A refusal stops the probe where it is; what it counted so far still decides.
        let _ = h.run_until(|_| read().0 >= k);
        *slot = read();
    }
    let [(rs, rise), (fs, fall)] = counted;
    let scans = rs.min(fs);
    Ok(EdgeChoice {
        at_fall: fall > rise,
        rise,
        fall,
        scans,
    })
}
