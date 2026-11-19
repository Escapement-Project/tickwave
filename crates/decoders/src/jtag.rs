//! JTAG (IEEE 1149.1): the TAP controller's state followed from TMS at each TCK rise, and the IR
//! and DR scans it shifts, with their TDI and TDO bits.
//!
//! A state held for more than one clock is an item. States passed in one clock join a path item
//! from the held state before them to the one after, at most `PATH_AT_MOST` long; Shift and
//! Test-Logic-Reset are items even for one clock. The state is unknown from the start, and again
//! after lost input, until TMS forces it. A scan's bits are emitted in chunks as they shift; its
//! first 128 bits are a field once it passes them, and the whole of a scan of at most 256 bits
//! once it closes.
//!
//! JTAG reads the wire, or cJTAG's output as its input `clocks` (see `cjtag`). TDO is an optional
//! port: bound without it, a scan's TDO is unknown. Where TDO is read, at TCK's rise, at the fall
//! after it, or at whichever of them the IR scans' captures read, is a setting, `tdo_edge`.
//!
//! A chain of several TAPs is cut into each TAP's part where the setting `chain` gives each TAP's
//! IR length, the TAP nearest TDO first. An IR scan as long as the chain's IRs has an
//! `instruction` child for each TAP, the bits shifted in being its new instruction. A DR scan has
//! a `data` child for the one TAP whose instruction is not BYPASS (all ones) where every other's
//! is, its DR being the scan less a bit for each TAP in BYPASS; the TAPs nearer TDO shift their
//! bits out first. A scan that can't be cut says why in its `split` field: an IR scan of another
//! length shifts some other chain, as a chain whose TAPs a router adds or removes does, and no
//! TAP's instruction is then known until an IR scan of the chain's length; nor is it from the
//! start, after lost input or after Test-Logic-Reset, which loads IDCODE or BYPASS.

use crate::cjtag::JtagOnCjtag;
use escapement_tickwave_decode::{
    Cause, Decoder, Definition, Element, ElementType, Exchange, Factory, FieldId, FieldType, Id,
    InputDecl, KindDecl, KindId, Label, MAX_BITS, Mode, Need, Observation, Observed, Out, Pause,
    Reason, Request, Schema, SeqId, Setting, SettingDecl, SettingId, Span, Status, Tick,
    UpstreamDecl, Value, Window, field, name, seq,
};
use std::ops::Range;
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
/// Why a scan isn't cut into its TAPs' parts, where the chain is set.
pub const SPLIT: FieldId = FieldId(5);
/// A TAP's part of a scan, its instruction or its data.
pub const INSTRUCTION: KindId = KindId(4);
pub const DATA: KindId = KindId(5);
/// A part's fields: its TAP's place in the chain from TDO, its length and its bits.
pub const TAP_INDEX: FieldId = FieldId(0);
pub const PART_LENGTH: FieldId = FieldId(1);
pub const PART_TDI: FieldId = FieldId(2);
pub const PART_TDO: FieldId = FieldId(3);

pub const NO_TDO: Reason = Reason(0);
pub const TWO_WIRE: Reason = Reason(1);
pub const MID_SESSION: Reason = Reason(2);
pub const LOST_STATE: Reason = Reason(3);
pub const NO_STATE_BEFORE: Reason = Reason(4);
pub const SPLIT_LENGTH: Reason = Reason(5);
pub const SPLIT_BYPASS: Reason = Reason(6);
pub const SPLIT_UNKNOWN: Reason = Reason(7);
pub const SPLIT_LONG: Reason = Reason(8);
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
    let scan = |kind: &'static str, head: &'static str| {
        KindDecl::new(kind)
            .with_fields(vec![
                field("tdi", FieldType::Bits { max: MAX_BITS }),
                field("tdo", FieldType::Bits { max: MAX_BITS }),
                field("tdi_head", FieldType::Bits { max: 128 }),
                field("tdo_head", FieldType::Bits { max: 128 }),
                field("length", FieldType::Word { bits: 32 }),
                field("split", FieldType::Bool),
            ])
            .with_sequences(vec![
                seq("tdi", ElementType::Bit),
                seq("tdo", ElementType::Bit),
            ])
            .with_labels(vec![
                Label::default().text(head).text(" ").field(TDI_HEAD),
                Label::default().text(head),
            ])
    };
    // A TAP's part of a scan lies within the scan, in a row under the states.
    let part = |kind: &'static str, head: &'static str, parent: &'static str| {
        KindDecl::new(kind)
            .with_row(2)
            .with_parents([parent])
            .with_fields(vec![
                field("tap", FieldType::Word { bits: 8 }),
                field("length", FieldType::Word { bits: 32 }),
                field("tdi", FieldType::Bits { max: MAX_BITS }),
                field("tdo", FieldType::Bits { max: MAX_BITS }),
            ])
            .with_labels(vec![
                Label::default()
                    .text("TAP ")
                    .field(TAP_INDEX)
                    .text(" ")
                    .text(head)
                    .text(" ")
                    .field(PART_TDI),
                Label::default().text(head).text(" ").field(PART_TDI),
                Label::default().text(head),
            ])
    };
    // A scan spans the states it shifts in, so the states draw in a row under the scans.
    Schema::new(vec![
        KindDecl::new("state")
            .with_row(1)
            .with_fields(vec![field("state", state())])
            .with_labels(vec![Label::default().field(TAP_STATE)]),
        KindDecl::new("path")
            .with_row(1)
            .with_fields(vec![
                field("from", state()),
                field("to", state()),
                field("clocks", FieldType::Word { bits: 32 }),
            ])
            .with_sequences(vec![seq(
                "via",
                ElementType::Name(STATES.map(name).to_vec()),
            )])
            .with_labels(vec![
                Label::default().field(FROM).text(" to ").field(TO),
                Label::default().field(TO),
            ]),
        scan("ir-scan", "IR"),
        scan("dr-scan", "DR"),
        part("instruction", "IR", "ir-scan"),
        part("data", "DR", "dr-scan"),
    ])
    .with_reasons([
        "the recording has no TDO channel",
        "cJTAG's Standard Protocol carries only TMS on its two wires",
        "the recording starts mid-session, before anything forces Test-Logic-Reset",
        "the TAP state is lost in a recording gap",
        "no state was decoded before it",
        "its length differs from the chain's",
        "more than one TAP is out of BYPASS, so no TAP's DR length is known",
        "a TAP's instruction is not known: none since the start, lost input, Test-Logic-Reset or an IR scan of another chain",
        "its TAP's part runs past the bits a scan keeps",
    ])
    .with_needs(["a TDO channel"])
    .with_causes([
        "a recording gap interrupts the scan",
        "the recording ends mid-scan",
    ])
    .with_open_at_most(3)
}

/// JTAG's inputs: the wire, and cJTAG's output.
pub const FROM_WIRE: usize = 0;
pub const FROM_CJTAG: usize = 1;

/// Where TDO is read: `TdoEdge`'s choices, by their names.
pub const TDO_EDGE: SettingId = SettingId(0);
/// Each TAP's IR length, the TAP nearest TDO first; none, the default, leaves scans whole.
pub const CHAIN: SettingId = SettingId(1);
/// The most TAPs a chain names, and the longest IR: together no more bits than a scan keeps.
pub const CHAIN_AT_MOST: usize = 8;
pub const IR_AT_MOST: i64 = 32;

/// The edge of TCK each clock's TDO is read at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TdoEdge {
    /// At the rise, where IEEE 1149.1 has the probe sample it.
    Rise,
    /// At the fall after the rise: a target at a high TCK rate can drive TDO late, and a recording
    /// then shows TDO changing near the rise. A clock with no fall after it, at a gap or the end,
    /// keeps the level at its rise.
    Fall,
    /// At the fall, moved by the IR captures. The fall reads TDO wherever its delay after the fall
    /// that drives it is under one TCK period, and the rise only where it is under half of one, so
    /// a faster TCK cannot make the rise right where the fall is wrong. The fall reads the next
    /// bit instead where TDO is recorded changing at or just before that fall, as skew between a
    /// probe's channels can show it. Either way TDO then changes only between a rise and the
    /// fall after it, and the rise reads each value one clock after the fall does. An IR scan
    /// whose bits show that, and whose first two read 1 then 0, as every IR capture's do, only at
    /// the other edge, counts toward moving the edge there; `MOVED_BY` of them in a row move it, for the
    /// last and every scan after. One alone does not: an IR scan can shift out a TDO no TAP
    /// drives, as a probe's switch from cJTAG to JTAG does.
    IrCapture,
}

impl TdoEdge {
    const NAMES: [&'static str; 3] = ["rise", "fall", "ir-capture"];

    /// The edge a binding's choice names: its index into `NAMES`.
    fn chosen(index: Option<usize>) -> Self {
        match index {
            Some(0) => Self::Rise,
            Some(1) => Self::Fall,
            _ => Self::IrCapture,
        }
    }
}

/// IR scans in a row that read only at the other edge before `TdoEdge::IrCapture` moves to it.
pub const MOVED_BY: u8 = 2;

/// The chain setting, as a binding gives it: each TAP's IR length, the TAP nearest TDO first.
pub fn chain(lengths: &[u8]) -> (&'static str, Setting) {
    (
        "chain",
        Setting::List(lengths.iter().map(|&l| i64::from(l)).collect()),
    )
}

/// The TDO edge setting, as a binding gives it.
pub fn tdo_edge(edge: TdoEdge) -> (&'static str, Setting) {
    let n = match edge {
        TdoEdge::Rise => 0,
        TdoEdge::Fall => 1,
        TdoEdge::IrCapture => 2,
    };
    ("tdo_edge", Setting::Name(name(TdoEdge::NAMES[n])))
}

pub fn definition() -> Arc<Definition> {
    Arc::new(
        Definition::new("jtag", 1, schema())
            .with_ports(["TCK", "TMS", "TDI"])
            .with_optional_ports(["TDO"])
            .with_inputs([
                InputDecl::Wire,
                // The Standard Protocol's TMS bits, an escape's among them, and the Scan Packets,
                // as pieces: a packet's TMS or nTDI element says where its clock was sampled.
                UpstreamDecl::new("clocks", Mode::Pieces)
                    .from(["cjtag"])
                    .with_kinds(["jscan", "escape", "packet"])
                    .with_sequences(["tms", "ntdi"])
                    .into(),
            ])
            .with_settings(vec![
                SettingDecl::choice("tdo_edge", TdoEdge::NAMES, 2).for_inputs([FROM_WIRE]),
                SettingDecl::list("chain", 1..=IR_AT_MOST, CHAIN_AT_MOST, vec![]),
            ]),
    )
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

pub(crate) fn pack(bits: &[bool]) -> Value {
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
    /// TDO read again at the fall after the rise, and where; None where the wire gives no such
    /// fall, or where the values come from upstream.
    pub tdo_fall: Option<(Tick, bool)>,
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

    /// TDO and where it was read: at the fall after the rise where `fall` and there is one, else
    /// at the rise or upstream.
    fn tdo_read(&self, fall: bool) -> Option<(bool, Observation)> {
        match (fall, self.tdo_fall) {
            (true, Some((at, v))) => Some((v, Observation::Wire(at))),
            _ => self.tdo.map(|v| (v, self.tdo_at())),
        }
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
    /// An IR scan's TDO bits read at the other edge until it is counted, where the IR captures
    /// move the edge (`Tap::follow_capture`).
    other: Option<Vec<(bool, Observation)>>,
    /// The first `MAX_BITS` bits' cells, where the chain is set, for its TAPs' parts.
    cells: Option<Vec<Span>>,
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
    edge: TdoEdge,
    reading: Reading,
    /// Each TAP's IR length, the TAP nearest TDO first; empty, scans aren't cut.
    chain: Vec<u8>,
    /// Each TAP's instruction as the last IR scan of the chain shifted it in, None where not known.
    instructions: Vec<Option<Vec<bool>>>,
}

/// Where a `Tap` reads TDO: as its edge fixes it, or as the IR captures last moved it.
#[derive(Clone, Copy, Debug)]
struct Reading {
    at_fall: bool,
    /// IR scans in a row that read only at the other edge.
    votes: u8,
}

impl Tap {
    /// The bytes its path and the scan under way hold.
    pub fn heap_bytes(&self) -> usize {
        let scan = self.scan.as_ref().map_or(0, |s| {
            crate::held::<Element>(s.tdi.capacity() + s.tdo.capacity())
                + crate::held::<bool>(s.head_tdi.capacity() + s.head_tdo.capacity())
                + crate::held::<(bool, Observation)>(s.other.as_ref().map_or(0, Vec::capacity))
                + crate::held::<Span>(s.cells.as_ref().map_or(0, Vec::capacity))
        });
        let instructions = (self.instructions.iter().flatten())
            .map(|i| crate::held::<bool>(i.capacity()))
            .sum::<usize>();
        crate::held::<Element>(self.path.capacity()) + scan + instructions
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
            edge: TdoEdge::Rise,
            reading: Reading {
                at_fall: false,
                votes: 0,
            },
            chain: vec![],
            instructions: vec![],
        }
    }

    /// Scans cut into the parts of the TAPs whose IR lengths `chain` gives, nearest TDO first.
    pub fn chain(mut self, chain: Vec<u8>) -> Self {
        self.instructions = vec![None; chain.len()];
        self.chain = chain;
        self
    }

    /// TDO read at `edge`, where a clock gives TDO at its fall as well as at its rise.
    pub fn tdo_edge(mut self, edge: TdoEdge) -> Self {
        self.edge = edge;
        self.reading.at_fall = edge != TdoEdge::Rise;
        self
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
            // Its TAPs' parts open as it closes, from its first bit.
            if let Some(first) = sc.cells.as_ref().and_then(|c| c.first()) {
                f = f.min(first.start);
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

    fn flush(sc: &mut Scan, reading: &mut Reading, out: &mut Out) {
        Self::follow_capture(sc, reading);
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
        Self::flush(&mut sc, &mut self.reading, out);
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
        if !self.chain.is_empty() {
            let split = match (status, sc.id.kind == IR_SCAN) {
                (Status::Complete, true) => self.split_ir(&sc, out),
                (Status::Complete, false) => self.split_dr(&sc, out),
                // A scan cut short updates no register, and its cause says why it is whole.
                _ => Ok(()),
            };
            if let Err(reason) = split {
                out.set(sc.id, SPLIT, unknown(reason));
            }
        }
        out.close(sc.id, end.max(sc.latest), status);
    }

    /// An IR scan of the chain's length cut into each TAP's instruction, which each TAP holds
    /// from then. One of another length shifts another chain: no instruction is known after it.
    fn split_ir(&mut self, sc: &Scan, out: &mut Out) -> Result<(), Reason> {
        let total: usize = self.chain.iter().map(|&l| usize::from(l)).sum();
        if sc.n as usize != total || !sc.tdi_known {
            self.instructions.fill(None);
            return Err(if sc.tdi_known {
                SPLIT_LENGTH
            } else {
                SPLIT_UNKNOWN
            });
        }
        let mut at = 0;
        for (k, &len) in self.chain.iter().enumerate() {
            let bits = at..at + usize::from(len);
            self.part(sc, INSTRUCTION, k, bits.clone(), out);
            self.instructions[k] = Some(sc.head_tdi[bits.clone()].to_vec());
            at = bits.end;
        }
        Ok(())
    }

    /// A DR scan's part for the one TAP out of BYPASS, where every other is in it: each TAP in
    /// BYPASS holds one bit, and those nearer TDO shift theirs first.
    fn split_dr(&self, sc: &Scan, out: &mut Out) -> Result<(), Reason> {
        let mut out_of_bypass = None;
        for (k, i) in self.instructions.iter().enumerate() {
            let i = i.as_ref().ok_or(SPLIT_UNKNOWN)?;
            if i.iter().all(|&b| b) {
                continue;
            }
            if out_of_bypass.replace(k).is_some() {
                return Err(SPLIT_BYPASS);
            }
        }
        let Some(k) = out_of_bypass else {
            return Ok(());
        };
        let bypass = self.chain.len() - 1;
        let n = sc.n as usize;
        if n <= bypass {
            return Err(SPLIT_LENGTH);
        }
        let bits = k..k + n - bypass;
        if bits.end > sc.head_tdi.len() {
            return Err(SPLIT_LONG);
        }
        self.part(sc, DATA, k, bits, out);
        Ok(())
    }

    /// TAP `tap`'s part of scan `sc`, bits `bits` of it, as a child of the scan.
    fn part(&self, sc: &Scan, kind: KindId, tap: usize, bits: Range<usize>, out: &mut Out) {
        let cells = sc.cells.as_deref().unwrap_or(&[]);
        let (Some(first), Some(last)) = (cells.get(bits.start), cells.get(bits.end - 1)) else {
            return;
        };
        let id = Id::new(kind, first.start);
        out.open(id, Some(sc.id), first.start);
        let word = |value: usize, bits: u8| Value::Word {
            value: value as u64,
            bits,
        };
        out.set(id, TAP_INDEX, word(tap, 8));
        out.set(id, PART_LENGTH, word(bits.len(), 32));
        if sc.tdi_known {
            out.set(id, PART_TDI, pack(&sc.head_tdi[bits.clone()]));
        }
        let tdo = match (self.has_tdo, sc.tdo_known) {
            (false, _) => Value::Unknown {
                reason: NO_TDO,
                needs: Some(NEEDS_TDO),
            },
            (true, false) => unknown(TWO_WIRE),
            (true, true) => pack(&sc.head_tdo[bits]),
        };
        out.set(id, PART_TDO, tdo);
        out.close(id, last.end, Status::Complete);
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
            other: (ir && tdo_known && self.edge == TdoEdge::IrCapture).then(Vec::new),
            cells: (!self.chain.is_empty()).then(Vec::new),
        });
    }

    fn shift(sc: &mut Scan, c: &Clocked, reading: &mut Reading, out: &mut Out) {
        if let (Some(v), true) = (c.tdi, sc.tdi_known) {
            sc.tdi.push(Element::new(u8::from(v), c.tdi_at(), c.cell));
            sc.tdi_n = sc.tdi_n.saturating_add(1);
        }
        let tdo = c.tdo_read(reading.at_fall);
        if let (Some((v, at)), true) = (tdo, sc.tdo_known) {
            sc.tdo.push(Element::new(u8::from(v), at, c.cell));
            sc.tdo_n = sc.tdo_n.saturating_add(1);
            if let (Some(other), Some(o)) = (&mut sc.other, c.tdo_read(!reading.at_fall)) {
                other.push(o);
            }
        }
        if sc.head_tdi.len() < MAX_BITS as usize {
            sc.head_tdi.push(c.tdi.unwrap_or(false));
            sc.head_tdo.push(tdo.is_some_and(|(v, _)| v));
            if let Some(cells) = &mut sc.cells {
                cells.push(c.cell);
            }
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
            Self::flush(sc, reading, out);
        }
    }

    /// Counts an IR scan whose held bits show it read at the wrong edge (`TdoEdge::IrCapture`),
    /// and at `MOVED_BY` in a row moves the edge and reads its bits there; any other IR scan
    /// starts the count again. Called before the scan's first bits are emitted.
    fn follow_capture(sc: &mut Scan, reading: &mut Reading) {
        let Some(other) = sc.other.take() else {
            return;
        };
        let here: Vec<bool> = sc.tdo.iter().map(|e| e.value == 1).collect();
        let there: Vec<bool> = other.iter().map(|&(v, _)| v).collect();
        let reads = |v: &[bool]| v.len() >= 2 && v[0] && !v[1];
        let (rise, fall) = if reading.at_fall {
            (&there, &here)
        } else {
            (&here, &there)
        };
        // `there` reading 1 then 0 holds at least two bits, so both do past the length check.
        let wrong = !reads(&here)
            && reads(&there)
            && rise.len() == fall.len()
            && rise[1..] == fall[..fall.len() - 1];
        reading.votes = if wrong {
            reading.votes.saturating_add(1)
        } else {
            0
        };
        if reading.votes < MOVED_BY {
            return;
        }
        for ((e, (v, at)), h) in sc.tdo.iter_mut().zip(other).zip(&mut sc.head_tdo) {
            *e = Element::new(u8::from(v), at, e.cell);
            *h = v;
        }
        *reading = Reading {
            at_fall: !reading.at_fall,
            votes: 0,
        };
    }

    pub fn clock(&mut self, c: &Clocked, out: &mut Out) {
        let before = self.set;
        if let Some(s @ (SHIFT_DR | SHIFT_IR)) = single(before) {
            if self.scan.is_none() {
                self.open_scan(s == SHIFT_IR, c, out);
            }
            if let Some(sc) = self.scan.as_mut() {
                Self::shift(sc, c, &mut self.reading, out);
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
        if single(after) == Some(RESET) {
            self.instructions.fill(None);
        }
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
        self.instructions.fill(None);
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
    tdo_fall: Option<(Tick, bool)>,
}

/// JTAG under the host: TCK, TMS, TDI and TDO in windows of `window` ticks. TMS and TDI are sampled
/// at each TCK rise, and TDO at the edge `tdo_edge` gives: a change at an edge's own tick counts
/// after it. Without TDO, port 3
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
        }
    }

    /// TDO read at `edge`; at the rise by default.
    pub fn tdo_edge(mut self, edge: TdoEdge) -> Self {
        self.tap = self.tap.tdo_edge(edge);
        self
    }

    /// Makes runs of JTAG: on the wire in windows of `window` ticks, reading TDO where it has a
    /// channel, at the edge bound; on cJTAG from its clocks.
    pub fn factory(window: Tick) -> Factory {
        Box::new(move |b, local| {
            // Bound, each length is from 1 to `IR_AT_MOST`.
            let chain = (b.list(CHAIN).unwrap_or(&[]).iter())
                .map(|&l| l as u8)
                .collect();
            if b.input() == FROM_CJTAG {
                return Box::new(JtagOnCjtag::new(local, chain));
            }
            let edge = TdoEdge::chosen(b.choice(TDO_EDGE));
            let mut jtag = Self::new(local, b.port(TDO), window).tdo_edge(edge);
            jtag.tap = jtag.tap.chain(chain);
            Box::new(jtag)
        })
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
            tdo_fall: r.tdo_fall,
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
            tdo_fall: None,
        });
    }

    fn on_fall(&mut self, at: Tick, out: &mut Out) {
        self.fall = Some(at);
        self.bound = self.bound.max(at);
        if let Some(mut r) = self.rise.take() {
            if self.has_tdo {
                r.tdo_fall = self.sampled(TDO, at).map(|v| (at, v));
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
}
