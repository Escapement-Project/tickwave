//! cJTAG (IEEE 1149.7, TAP.7) on TCKC and TMSC: escapes, the Standard Protocol's TMS bits,
//! control levels and commands, Check Packets, scan formats, and the OScan1, 2, 5 and 6 Scan
//! Packets with their elements; and JTAG stacked on it, a clock per Standard Protocol bit and per
//! Scan Packet that advances the TAP, each value naming the cJTAG element it came from.
//!
//! A bit is TMSC's level when TCKC falls, and its TMSC edges while TCKC is high make an escape.
//! Activation is followed by command, as the project's probe does it, or by a Selection Sequence
//! after a selection escape or a Selection Alert. A controller Offline at start-up (the `start_up`
//! setting, until a CNFG0 read's DAS bit says) waits for one after each reset escape. Offline
//! stretches after a deselection escape or a Deselection Alert are marked. Test-Logic-Reset brings
//! back the default scan format and the Standard Protocol (table 11-8).
//!
//! A three-part command's CR scan is named by its command and operand (table 9-4), with its TDI and
//! TDO bits and the register value they carry, where the Advanced Protocol puts them on TMSC; where
//! CGM sends it to the EPU bypass bit instead, it is named so. Those scan formats are Star-2
//! operation (13.1), so a string has no other controllers' bits around it (table 9-11 with m and n
//! zero). What is not decoded (ready and delay elements, transport packets, other scan formats)
//! ends what it interrupts, incomplete, with that as the cause; framing resumes at the next reset
//! or selection escape.
//!
//! Whether the target drives the TDO elements is not yet read from the wire. A setting,
//! `assume_qualified`, assumes it; off, their driver is unknown, for a reason.

mod tap7;

use crate::jtag::{self, Clocked, Inputs, Tap};
use escapement_tickwave_decode::{
    AnalysisId, Cause, Decoder, Definition, Delivery, Element, ElementType, Exchange, Factory,
    FieldId, FieldType, Id, KindDecl, KindId, Label, Mode, Observation, Observed, Op, Out, Pause,
    Reason, Request, Role, Schema, SeqId, Setting, SettingDecl, SettingId, Span, Status,
    Subscription, Tick, Value, Window, field, name, seq,
};
use std::sync::Arc;
use tap7::{
    Cr, Driver, Escape, Ev, MNEMONICS, Offline, Outcome, Path, Response, Selection, Ssd, SsdKind,
    Tap7, format_name,
};

pub const TCKC: u8 = 0;
pub const TMSC: u8 = 1;

pub const ESCAPE: KindId = KindId(0);
pub const JSCAN: KindId = KindId(1);
pub const LEVEL: KindId = KindId(2);
pub const CHECK: KindId = KindId(3);
pub const COMMAND: KindId = KindId(4);
pub const FORMAT: KindId = KindId(5);
pub const PACKET: KindId = KindId(6);
pub const SELECTION: KindId = KindId(7);
pub const OFFLINE: KindId = KindId(8);
pub const CR_SCAN: KindId = KindId(9);
pub const ALERT: KindId = KindId(10);
pub const SSD: KindId = KindId(11);

/// `ESCAPE`'s fields and sequences: in the Standard Protocol a selection or deselection escape's
/// bit is a TMS bit.
pub const TOGGLES: FieldId = FieldId(0);
pub const MEANING: FieldId = FieldId(1);
pub const RESPONSE: FieldId = FieldId(2);
pub const TOGGLE_BITS: SeqId = SeqId(0);
pub const ESCAPE_TMS: SeqId = SeqId(1);
/// `JSCAN`'s field and sequence.
pub const PROTOCOL: FieldId = FieldId(0);
pub const JSCAN_TMS: SeqId = SeqId(0);
/// `LEVEL`'s fields.
pub const CONTROL_LEVEL: FieldId = FieldId(0);
pub const LEVEL_USE: FieldId = FieldId(1);
/// `CHECK`'s field and sequence.
pub const RESULT: FieldId = FieldId(0);
pub const CHECK_BITS: SeqId = SeqId(0);
/// `COMMAND`'s fields.
pub const OPCODE: FieldId = FieldId(0);
pub const OPERAND: FieldId = FieldId(1);
pub const EFFECT: FieldId = FieldId(2);
pub const SHIFTS: FieldId = FieldId(3);
/// `FORMAT`'s fields.
pub const SCAN_FORMAT: FieldId = FieldId(0);
pub const SAMPLING: FieldId = FieldId(1);
/// `PACKET`'s fields and sequences.
pub const PACKET_FORMAT: FieldId = FieldId(0);
pub const NTDI: FieldId = FieldId(1);
pub const PACKET_TMS: FieldId = FieldId(2);
pub const PAD: FieldId = FieldId(3);
pub const PACKET_TDO: FieldId = FieldId(4);
pub const PACKET_TDI: FieldId = FieldId(5);
pub const ADVANCE: FieldId = FieldId(6);
pub const DRIVER: FieldId = FieldId(7);
pub const NTDI_BITS: SeqId = SeqId(0);
pub const TMS_BITS: SeqId = SeqId(1);
pub const PAD_BITS: SeqId = SeqId(2);
pub const TDO_BITS: SeqId = SeqId(3);
/// `SELECTION`'s fields and sequence.
pub const BRANCH: FieldId = FieldId(0);
pub const PARKING: FieldId = FieldId(1);
pub const PROTECT: FieldId = FieldId(2);
pub const FORM: FieldId = FieldId(3);
pub const SELECTED_FORMAT: FieldId = FieldId(4);
pub const READY: FieldId = FieldId(5);
pub const DELAY: FieldId = FieldId(6);
pub const TRANSPORT_STATES: FieldId = FieldId(7);
pub const OUTCOME: FieldId = FieldId(8);
pub const SELECTION_BITS: SeqId = SeqId(0);
/// `OFFLINE`'s field.
pub const OFFLINE_BY: FieldId = FieldId(0);
/// `CR_SCAN`'s fields.
pub const REGISTER: FieldId = FieldId(0);
pub const CR_LENGTH: FieldId = FieldId(1);
pub const CR_TDI: FieldId = FieldId(2);
pub const CR_TDO: FieldId = FieldId(3);
pub const CR_VALUE: FieldId = FieldId(4);
pub const CR_MEANING: FieldId = FieldId(5);
/// `ALERT`'s field: which alert, by its index in `ALERTS`.
pub const ALERT_IS: FieldId = FieldId(0);
/// `SSD`'s fields.
pub const DIRECTIVE: FieldId = FieldId(0);
pub const SSD_CID: FieldId = FieldId(1);
pub const SSD_TCA: FieldId = FieldId(2);
pub const SSD_OUTCOME: FieldId = FieldId(3);
pub const SSD_STATE: FieldId = FieldId(4);

pub const PROBE: Role = Role(0);
pub const TARGET: Role = Role(1);

pub const UNQUALIFIED: Reason = Reason(0);
pub const PROTECTED: Reason = Reason(1);
pub const NOT_ON_TMSC: Reason = Reason(2);
pub const CGM_UNKNOWN: Reason = Reason(3);
pub const SREDGE_UNKNOWN: Reason = Reason(4);
pub const SSD_STATE_UNKNOWN: Reason = Reason(5);

pub const GAP_CUT: Cause = Cause(0);
pub const NEEDS_TAP: Cause = Cause(1);
pub const LEVEL_ENDS: Cause = Cause(2);
pub const INTERRUPTED: Cause = Cause(3);
pub const FORMAT_UNFRAMED: Cause = Cause(4);
pub const READY_DELAY: Cause = Cause(5);
pub const TRANSPORT: Cause = Cause(6);
pub const SSD_UNKNOWN: Cause = Cause(7);

/// `MEANING`'s names.
pub const MEANINGS: [&str; 4] = ["reset", "selection", "deselection", "custom"];
/// `RESPONSE`'s names.
pub const RESPONSES: [&str; 4] = [
    "taken",
    "not qualified, ignored",
    "offline, ignored",
    "qualification unknown",
];
/// `RESULT`'s names.
pub const RESULTS: [&str; 2] = ["reset", "continue"];
/// `BRANCH`'s names: what the Online Activation Code selects (table 11-1).
pub const BRANCHES: [&str; 9] = [
    "every TAP.7 topology",
    "TAP.7 Series",
    "TAP.7 Star-4",
    "TAP.7 Star-2",
    "reserved TAP.7",
    "SlimBus",
    "USB",
    "Arm",
    "reserved technology",
];
/// `PARKING`'s names: the TAPC state the Extension Code requires (table 11-2).
pub const PARKING_STATES: [&str; 4] = [
    "Run-Test/Idle or Test-Logic-Reset",
    "Select-DR-Scan",
    "Pause-IR",
    "Pause-DR",
];
/// `OUTCOME`'s names.
pub const OUTCOMES: [&str; 3] = [
    "candidate",
    "another technology selected",
    "parked in another state",
];
/// `OFFLINE_BY`'s names.
pub const OFFLINE_BYS: [&str; 5] = [
    "a deselection escape",
    "a selection of another technology",
    "a selection needing another parking state",
    "a Deselection Alert",
    "a reset, offline at start-up",
];
/// `ALERT_IS`'s names.
pub const ALERTS: [&str; 2] = ["Selection", "Deselection"];
/// `DIRECTIVE`'s names (table 20-14).
pub const DIRECTIVES: [&str; 4] = [
    "Deselect All",
    "Select All",
    "Select One by CID",
    "Select One by TCA",
];
/// `SSD_OUTCOME`'s names.
pub const SSD_OUTCOMES: [&str; 2] = ["executed", "aborted"];
/// `SSD_STATE`'s names (table 20-13).
pub const SSD_STATES: [&str; 7] = [
    "SSD_SA",
    "SSD_DA_IR",
    "SSD_DA_DR",
    "SSD_NS_IR",
    "SSD_NS_DR",
    "SSD_IS_IR",
    "SSD_IS_DR",
];
/// `REGISTER`'s names: what a CR scan reaches (table 9-4), and the EPU bypass bit where CGM is 0
/// (table 9-10).
pub const REGISTERS: [&str; 20] = [
    "SGC",
    "CGM",
    "CNFG0[0]",
    "CNFG0[1]",
    "CNFG0[2]",
    "CNFG0[3]",
    "FRESETR",
    "Series detect",
    "reserved bit",
    "private bit",
    "RDBACK0",
    "RDBACK1",
    "CNFG0",
    "CNFG1",
    "CNFG2",
    "CNFG3",
    "reserved string",
    "private string",
    "AT reference",
    "EPU bypass",
];
const BYPASS: u16 = 19;

/// `BRANCH`'s name for an Online Activation Code: OAC[1:0] the technology, OAC[3:2] the branch.
fn branch(oac: u8) -> u16 {
    let (tech, topology) = (u16::from(oac & 3), u16::from(oac >> 2 & 3));
    match (tech, topology) {
        (0, _) => topology,
        (1, _) => 4,
        (2, 0..=2) => 5 + topology,
        _ => 8,
    }
}
/// What a control level is for (5.3.3.3.2): control level 0 and 7's, then 1, 2, 3, 4 and 5, 6.
pub const LEVEL_USES: [&str; 6] = [
    "the debug and test system's",
    "the system test logic's",
    "commands",
    "reserved",
    "custom scan paths",
    "forcing offline",
];

/// What a CR scan reaches (table 9-4): its `REGISTERS` name; whether its value is what the target
/// shifts out (TDO) or what the probe shifts in (TDI), where table 9-4 says; and its width.
fn reached(opcode: u8, operand: u8) -> (u16, Option<bool>, usize) {
    match (opcode, operand) {
        (8, 0 | 1 | 7) => (u16::from(operand), Some(false), 1),
        (8, 2..=6) => (u16::from(operand), Some(true), 1),
        (8, 8..=15) => (8, Some(true), 1),
        (8, _) => (9, None, 1),
        (9, 0..=5) => (10 + u16::from(operand), Some(true), 32),
        (9, 6 | 7) => (16, Some(true), 32),
        (9, _) => (17, None, 32),
        // CIDA's AT reference: TDI in a directed allocation, TDO's wired AND otherwise (9.9.3).
        (_, s) => (18, Some(s & 16 != 0), 36),
    }
}

/// CNFG0's class and revision (table 9-15), from its first eight bits.
fn cnfg0(bits: &[bool]) -> Option<String> {
    let n = |from: usize| {
        (from..from + 4)
            .rev()
            .fold(0u8, |v, k| v << 1 | u8::from(bits[k]))
    };
    if bits.len() < 8 {
        return None;
    }
    let class = match n(0) {
        0 | 15 => "TAP.1 or T0".to_string(),
        c @ 1..=5 => format!("T{c}"),
        _ => "reserved".to_string(),
    };
    Some(format!("class {class}, revision {}", n(4)))
}

fn level_use(level: u8) -> u16 {
    match level {
        1 => 1,
        2 => 2,
        3 => 3,
        4 | 5 => 4,
        6 => 5,
        _ => 0,
    }
}

/// `OPCODE`'s names: TAP.7's command mnemonics, then "reserved" for opcodes past them.
pub fn opcodes() -> Vec<&'static str> {
    MNEMONICS.iter().copied().chain(["reserved"]).collect()
}

pub fn schema() -> Schema {
    let bit = || ElementType::Bit;
    let text = |max| FieldType::Text { max };
    let names = |list: &[&'static str]| FieldType::Name(list.iter().map(|&n| name(n)).collect());
    // Rows, top to bottom: the scan format in force, the control level, what lies inside a level,
    // and the bits on the wire (escapes, Standard Protocol TMS bits and Scan Packets), which every
    // row above spans.
    Schema::new(vec![
        KindDecl::new("escape")
            .with_row(3)
            .with_fields(vec![
                field("toggles", FieldType::Word { bits: 8 }),
                field("meaning", names(&MEANINGS)),
                field("response", names(&RESPONSES)),
            ])
            .with_sequences(vec![seq("toggles", bit()), seq("tms", bit())])
            .with_labels(vec![
                Label::default().field(MEANING).text(" escape"),
                Label::default().field(MEANING),
            ]),
        KindDecl::new("jscan")
            .with_row(3)
            .with_fields(vec![field("protocol", names(&["Standard (JScan)"]))])
            .with_sequences(vec![seq("tms", bit())])
            .with_labels(vec![
                Label::default().field(PROTOCOL),
                Label::default().text("JScan"),
            ]),
        KindDecl::new("level")
            .with_row(1)
            .with_fields(vec![
                field("level", FieldType::Word { bits: 3 }),
                field("use", names(&LEVEL_USES)),
            ])
            .with_labels(vec![
                Label::default().text("control level ").field(CONTROL_LEVEL),
                Label::default().text("CL ").field(CONTROL_LEVEL),
            ]),
        KindDecl::new("check")
            .with_row(2)
            .with_parents(["level"])
            .with_fields(vec![field("result", names(&RESULTS))])
            .with_sequences(vec![seq("bits", bit())])
            .with_labels(vec![
                Label::default().text("check ").field(RESULT),
                Label::default().field(RESULT),
            ]),
        KindDecl::new("command")
            .with_row(2)
            .with_parents(["level"])
            .with_fields(vec![
                field("opcode", names(&opcodes())),
                field("operand", FieldType::Word { bits: 5 }),
                field("effect", text(64)),
                field("shifts", text(32)),
            ])
            .with_labels(vec![
                Label::default().field(OPCODE).text(" ").field(OPERAND),
                Label::default().field(OPCODE),
            ]),
        KindDecl::new("format")
            .with_fields(vec![
                field("format", text(16)),
                field("sampling", names(&["TCKC rise", "TCKC fall"])),
            ])
            .with_labels(vec![
                (Label::default().field(SCAN_FORMAT))
                    .text(", sampled at ")
                    .field(SAMPLING),
                Label::default().field(SCAN_FORMAT),
            ]),
        KindDecl::new("packet")
            .with_row(3)
            .with_fields(vec![
                field("format", text(16)),
                field("ntdi", FieldType::Bool).with_short("n"),
                field("tms", FieldType::Bool),
                field("pad", FieldType::Bool),
                field("tdo", FieldType::Bool),
                field("tdi", FieldType::Bool),
                field("advance", FieldType::Bool),
                field("driver", names(&["probe", "target"])),
            ])
            .with_sequences(vec![
                seq("ntdi", bit()),
                seq("tms", bit()),
                seq("pad", bit()),
                seq("tdo", bit()),
            ])
            .with_labels(vec![Label::default().field(PACKET_FORMAT)]),
        KindDecl::new("selection")
            .with_row(3)
            .with_fields(vec![
                field("branch", names(&BRANCHES)),
                field("parking state", names(&PARKING_STATES)),
                field("protect", FieldType::Bool),
                field("form", names(&["long", "short"])),
                field("format", text(16)),
                field("RDYC", FieldType::Word { bits: 2 }),
                field("DLYC", FieldType::Word { bits: 2 }),
                field("TPST", FieldType::Word { bits: 5 }),
                field("outcome", names(&OUTCOMES)),
            ])
            .with_sequences(vec![seq("bits", bit())])
            .with_labels(vec![
                Label::default().text("selection: ").field(OUTCOME),
                Label::default().text("selection"),
            ]),
        KindDecl::new("offline")
            .with_fields(vec![field("by", names(&OFFLINE_BYS))])
            .with_labels(vec![Label::default().text("offline")]),
        KindDecl::new("cr-scan")
            .with_row(2)
            .with_parents(["level"])
            .with_fields(vec![
                field("register", names(&REGISTERS)),
                field("length", FieldType::Word { bits: 32 }),
                field(
                    "tdi",
                    FieldType::Bits {
                        max: tap7::CR_KEPT as u32,
                    },
                ),
                field(
                    "tdo",
                    FieldType::Bits {
                        max: tap7::CR_KEPT as u32,
                    },
                ),
                field("value", FieldType::Bits { max: 36 }),
                field("meaning", text(64)),
            ])
            .with_labels(vec![
                Label::default().field(REGISTER).text(" ").field(CR_VALUE),
                Label::default().field(REGISTER),
            ]),
        // On the top row with the Offline stretches alerts begin and end.
        KindDecl::new("alert")
            .with_fields(vec![field("alert", names(&ALERTS))])
            .with_labels(vec![
                Label::default().field(ALERT_IS).text(" Alert"),
                Label::default().text("alert"),
            ]),
        KindDecl::new("ssd")
            .with_row(2)
            .with_fields(vec![
                field("directive", names(&DIRECTIVES)),
                field("CID", FieldType::Word { bits: 4 }),
                field("TCA", FieldType::Word { bits: 35 }),
                field("outcome", names(&SSD_OUTCOMES)),
                field("SSD state", names(&SSD_STATES)),
            ])
            .with_labels(vec![
                Label::default()
                    .field(DIRECTIVE)
                    .text(" ")
                    .field(SSD_OUTCOME),
                Label::default().field(DIRECTIVE),
                Label::default().text("SSD"),
            ]),
    ])
    .with_roles(["probe", "target"])
    .with_reasons([
        "the target's coupling is not established, so its drive of TDO is not assumed",
        "drive conflict protection keeps the target from driving until it alone is a group \
         member, which is not followed",
        "in the Standard Protocol, TDI and TDO are not on TMSC",
        "CGM unknown, so the scan may reach the EPU bypass bit instead",
        "SREDGE unknown: no reset decoded, or stored conditionally while CGM was unknown",
        "the SSD state is unknown: no reset decoded, a selection by TCA, or SSDs off TMSC",
    ])
    .with_causes([
        "a recording gap interrupts it",
        "the TAP state is needed to frame scan packets",
        "the control level ends first",
        "an escape interrupts it",
        "a scan format this decoder does not frame",
        "ready or delay elements change the framing",
        "transport packets change the framing",
        "an escape whose qualification the unknown SSD state decides",
    ])
    .with_open_at_most(3)
}

/// Whether the target is assumed to drive the TDO elements, before the wire shows it is coupled.
pub const ASSUME_QUALIFIED: SettingId = SettingId(0);

/// The qualification setting, as a binding gives it.
pub fn assume_qualified(on: bool) -> (&'static str, Setting) {
    ("assume_qualified", Setting::Flag(on))
}

/// Whether a reset leaves the controller Online or Offline (11.4.1.5): the names of the setting
/// `start_up`.
pub const START_UP: SettingId = SettingId(1);
pub const START_UPS: [&str; 2] = ["online", "offline"];

/// The start-up setting, as a binding gives it.
pub fn start_up(offline: bool) -> (&'static str, Setting) {
    (
        "start_up",
        Setting::Name(name(START_UPS[usize::from(offline)])),
    )
}

pub fn definition() -> Arc<Definition> {
    Arc::new(
        Definition::new("cjtag", 1, schema())
            .with_ports(["TCKC", "TMSC"])
            .with_settings(vec![
                SettingDecl::flag("assume_qualified", false),
                SettingDecl::choice("start_up", START_UPS, 0),
            ]),
    )
}

/// JTAG as it stacks on cJTAG: JTAG's name, version and schema, reading no wire, and its chain
/// setting (`jtag::chain`), since a cJTAG adapter's chain is cut as a four-wire one is.
pub fn jtag_definition() -> Arc<Definition> {
    Arc::new(
        Definition::new("jtag", 1, jtag::schema()).with_settings(vec![SettingDecl::list(
            "chain",
            1..=jtag::IR_AT_MOST,
            jtag::CHAIN_AT_MOST,
            vec![],
        )]),
    )
}

/// JTAG on cJTAG's chain setting: each TAP's IR length, the TAP nearest TDO first.
pub const JTAG_CHAIN: SettingId = SettingId(0);

/// What JTAG reads of cJTAG: the Standard Protocol's TMS bits, an escape's among them, and the Scan
/// Packets, as pieces. A
/// packet's TMS or nTDI element says where its clock was sampled.
pub fn jtag_subscription(cjtag: AnalysisId) -> Subscription {
    Subscription::new(cjtag, Mode::Pieces)
        .with_kinds(["jscan", "escape", "packet"])
        .with_sequences(["tms", "ntdi"])
}

// ---- The lines -------------------------------------------------------------------------------

/// A change of either line, or a stretch where either is not known, in time order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Clock(Tick, bool),
    Line(Tick, bool),
    /// Not known from `from` to `to`; then TCKC and TMSC at these levels.
    Gap {
        from: Tick,
        to: Tick,
        clock: bool,
        line: bool,
    },
}

/// What one line did at one tick: a boundary (known from here at a level, or not known from here)
/// and an edge's new level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LineAt {
    boundary: Option<Option<bool>>,
    edge: Option<bool>,
}

/// TCKC and TMSC observations, gathered tick by tick into changes. Observations at one tick are
/// taken in together once a later tick or `flush` comes: boundaries first, then TCKC's edge, then
/// TMSC's, so a TMSC change at a TCKC edge's tick belongs to the phase after that edge, whatever
/// order a window gives them in. While either line is not known, that is one gap, reported once
/// both are known again.
#[derive(Clone, Debug)]
pub struct Lines {
    gap_from: Option<Tick>,
    unknown: [bool; 2],
    level: [bool; 2],
    /// The tick whose observations are being gathered, and what each line did at it.
    tick: Option<Tick>,
    at: [LineAt; 2],
}

impl Lines {
    /// The lines from `at`, each at its level or not known.
    pub fn new(at: Tick, levels: [Option<bool>; 2]) -> Self {
        let unknown = levels.map(|l| l.is_none());
        Self {
            gap_from: unknown.contains(&true).then_some(at),
            unknown,
            level: levels.map(|l| l.unwrap_or(false)),
            tick: None,
            at: [LineAt::default(); 2],
        }
    }

    /// Where the lines became not known, while either is.
    pub fn gap(&self) -> Option<Tick> {
        self.gap_from
    }

    /// The lines' levels as last known.
    pub fn levels(&self) -> [bool; 2] {
        self.level
    }

    /// One observation, in window order. Other ports and start levels are ignored.
    pub fn observe(&mut self, e: &Observed, out: &mut Vec<Change>) {
        let (at, port, seen) = match *e {
            Observed::Edge { at, port, level } => (at, port, (None, Some(level))),
            Observed::Known { at, port, level } => (at, port, (Some(Some(level)), None)),
            Observed::Unknown { at, port } => (at, port, (Some(None), None)),
            _ => return,
        };
        if port > TMSC {
            return;
        }
        if self.tick != Some(at) {
            self.flush(out);
            self.tick = Some(at);
        }
        if let Some(l) = self.at.get_mut(usize::from(port)) {
            if let Some(b) = seen.0 {
                l.boundary = Some(b);
            }
            if let Some(v) = seen.1 {
                l.edge = Some(v);
            }
        }
    }

    /// Takes in the observations gathered at the last tick.
    pub fn flush(&mut self, out: &mut Vec<Change>) {
        if let Some(t) = self.tick.take() {
            let lines = std::mem::take(&mut self.at);
            self.take(t, &lines, out);
        }
    }

    fn take(&mut self, t: Tick, lines: &[LineAt; 2], out: &mut Vec<Change>) {
        for (k, l) in lines.iter().enumerate() {
            match l.boundary {
                Some(None) => {
                    self.unknown[k] = true;
                    self.gap_from.get_or_insert(t);
                }
                Some(Some(level)) => {
                    self.unknown[k] = false;
                    self.level[k] = level;
                }
                None => {}
            }
        }
        if let Some(from) = self.gap_from
            && !self.unknown.contains(&true)
        {
            out.push(Change::Gap {
                from,
                to: t,
                clock: self.level[0],
                line: self.level[1],
            });
            self.gap_from = None;
        }
        for (k, l) in lines.iter().enumerate() {
            let Some(level) = l.edge else {
                continue;
            };
            self.level[k] = level;
            if self.gap_from.is_some() {
                continue;
            }
            out.push(if k == 0 {
                Change::Clock(t, level)
            } else {
                Change::Line(t, level)
            });
        }
    }
}

// ---- The decoder ------------------------------------------------------------------------------

/// What JTAG reads of cJTAG, in order, for a pipeline that feeds it directly rather than through
/// the host: a clock per Standard Protocol bit and per advancing Scan Packet, and the gaps.
// Clocks are nearly every feed, so boxing them would allocate per clock to save room on gaps.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug)]
pub enum Feed {
    Clock(Clocked),
    Gap { from: Tick, to: Tick },
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    format: u8,
    value: bool,
    driver: Driver,
    at: Tick,
    cell: Span,
}

/// The most TMSC toggles one TCKC high phase keeps: eight already mean reset.
const TOGGLES_AT_MOST: usize = 16;

/// TAP.7 over the lines' changes, as items.
#[derive(Clone, Debug)]
pub struct Oscan {
    tap: Tap7,
    /// Whether the target's drive of TDO elements is assumed: nothing on the wire shows it until
    /// decoded TDO data does.
    qualified: bool,
    clock: bool,
    line: bool,
    high_since: Option<Tick>,
    /// Whether `high_since` is a rise seen on the wire, not where the lines became known.
    rise_seen: bool,
    last_fall: Option<Tick>,
    /// Where the lines became known: the start or a gap's end. No cell starts before it.
    floor: Tick,
    toggles: Vec<Tick>,
    slots: Vec<Slot>,
    jscan: Option<(Id, u32)>,
    check: Option<(Id, u32)>,
    selection: Option<(Id, u32)>,
    offline: Option<Id>,
    level: Option<Id>,
    format: Option<(Id, u8)>,
    /// Where a Selection Alert being matched started: its item opens there once it completes.
    alert_from: Option<Tick>,
    /// Where the last Scan Packet started, where the one before it ended, and where an SSD under
    /// way started.
    packet_from: Tick,
    before_to: Tick,
    packet_to: Tick,
    ssd_from: Option<Tick>,
    /// The starts of the last two DR scans: a command's two parts.
    dr_begins: [Tick; 2],
    /// One phase's events, kept to reuse.
    events: Vec<Ev>,
    /// What JTAG reads of it since this was last cleared.
    pub clocks: Vec<Feed>,
}

impl Oscan {
    /// A decoder from `start`, where TCKC and TMSC are at `clock` and `line`: with TCKC high there,
    /// the first high phase is taken to start there.
    pub fn new(start: Tick, clock: bool, line: bool, qualified: bool) -> Self {
        Self {
            tap: Tap7::default(),
            qualified,
            clock,
            line,
            high_since: clock.then_some(start),
            rise_seen: false,
            last_fall: None,
            floor: start,
            toggles: Vec::with_capacity(TOGGLES_AT_MOST),
            slots: Vec::with_capacity(3),
            jscan: None,
            check: None,
            selection: None,
            offline: None,
            level: None,
            format: None,
            alert_from: None,
            packet_from: start,
            before_to: start,
            packet_to: start,
            ssd_from: None,
            dr_begins: [start; 2],
            events: vec![],
            clocks: vec![],
        }
    }

    /// A controller that comes up Offline after a reset escape when `offline`.
    pub fn with_start_up(mut self, offline: bool) -> Self {
        self.tap = Tap7::new(offline);
        self
    }

    /// The earliest a new item, part or element can start: the next bit's cell, from the last fall
    /// or where the lines became known; a Scan Packet's elements already framed; a control level or
    /// command whose DR scans have begun; and a Selection Alert being matched. An escape starts at
    /// a rise, after the last fall.
    pub fn start_frontier(&self) -> Tick {
        let mut f = self.last_fall.unwrap_or(self.floor);
        if let Some(a) = self.alert_from {
            f = f.min(a);
        }
        if let Some(s) = self.ssd_from {
            f = f.min(s);
        }
        if let Some(s) = self.slots.first() {
            f = f.min(s.cell.start);
        }
        if let Some(back) = self.tap.pending_scan() {
            f = f.min(self.dr_begins[1 - back.min(1)]);
        }
        f
    }

    fn close_jscan(&mut self, at: Tick, out: &mut Out) {
        if let Some((id, _)) = self.jscan.take() {
            out.close(id, at, Status::Complete);
        }
    }

    fn close_all(&mut self, at: Tick, status: Status, out: &mut Out) {
        self.close_jscan(at, out);
        if let Some((id, _)) = self.check.take() {
            out.close(id, at, status);
        }
        if let Some((id, _)) = self.selection.take() {
            out.close(id, at, status);
        }
        if let Some(id) = self.offline.take() {
            out.close(id, at, status);
        }
        if let Some(id) = self.level.take() {
            out.close(id, at, status);
        }
        if let Some((id, _)) = self.format.take() {
            out.close(id, at, status);
        }
        self.slots.clear();
    }

    pub fn change(&mut self, c: &Change, out: &mut Out) {
        match *c {
            Change::Gap {
                from,
                to,
                clock,
                line,
            } => {
                self.tap.gap();
                self.alert_from = None;
                self.ssd_from = None;
                self.close_all(from, Status::Incomplete(GAP_CUT), out);
                self.toggles.clear();
                self.high_since = clock.then_some(to);
                self.rise_seen = false;
                self.last_fall = None;
                self.floor = to;
                self.clock = clock;
                self.line = line;
                self.clocks.push(Feed::Gap { from, to });
            }
            Change::Clock(t, level) => {
                if level == self.clock {
                    return;
                }
                if level {
                    self.high_since = Some(t);
                    self.rise_seen = true;
                    self.toggles.clear();
                } else {
                    let rise = self.high_since.unwrap_or(t);
                    let high = t.saturating_sub(rise).max(1);
                    // An element runs from the fall before its rise, the stretch TMSC holds it,
                    // unless that fall is far back across a pause.
                    let lo = (self.last_fall)
                        .filter(|&f| rise.saturating_sub(f) <= high.saturating_mul(4))
                        .unwrap_or(rise.saturating_sub(high))
                        .max(self.floor);
                    let cell = Span { start: lo, end: t };
                    let at = if self.tap.sampled_at_rise() && self.rise_seen {
                        rise
                    } else {
                        t
                    };
                    let mut events = std::mem::take(&mut self.events);
                    self.tap.phase(self.line, self.toggles.len(), &mut events);
                    for ev in events.drain(..) {
                        self.handle(ev, t, rise, at, cell, out);
                    }
                    self.events = events;
                    self.alert_from = match self.tap.alert_matched() {
                        0 => None,
                        1 => Some(cell.start),
                        _ => self.alert_from,
                    };
                    self.last_fall = Some(t);
                }
                self.clock = level;
            }
            Change::Line(t, level) => {
                if level == self.line {
                    return;
                }
                if self.clock && self.toggles.len() < TOGGLES_AT_MOST {
                    self.toggles.push(t);
                }
                self.line = level;
            }
        }
    }

    fn element(value: bool, at: Tick, cell: Span) -> Element {
        Element::new(u8::from(value), Observation::Wire(at), cell)
    }

    fn handle(&mut self, ev: Ev, t: Tick, rise: Tick, at: Tick, cell: Span, out: &mut Out) {
        match ev {
            Ev::Escape {
                kind,
                response,
                tms,
            } => {
                // An escape carrying a TMS bit takes that bit's whole cell.
                let start = if tms.is_some() {
                    cell.start
                } else if self.rise_seen {
                    rise
                } else {
                    self.toggles.first().copied().unwrap_or(rise)
                };
                match response {
                    Some(Response::Unqualified | Response::Offline | Response::Unknown) => {
                        self.close_jscan(start, out)
                    }
                    _ => {
                        if let Some((id, _)) = self.selection.take() {
                            out.close(id, start, Status::Incomplete(INTERRUPTED));
                        }
                        self.close_all(start, Status::Complete, out);
                    }
                }
                let id = Id::new(ESCAPE, start);
                out.open(id, None, start);
                let n = self.toggles.len() as u64;
                out.set(id, TOGGLES, Value::Word { value: n, bits: 8 });
                let meaning = match kind {
                    Escape::Reset => 0,
                    Escape::Selection => 1,
                    Escape::Deselection => 2,
                    Escape::Custom => 3,
                };
                out.set(id, MEANING, Value::Name(meaning));
                if let Some(r) = response {
                    let n = match r {
                        Response::Taken => 0,
                        Response::Unqualified => 1,
                        Response::Offline => 2,
                        Response::Unknown => 3,
                    };
                    out.set(id, RESPONSE, Value::Name(n));
                }
                let toggles: Vec<Element> = (self.toggles.iter().enumerate())
                    .map(|(k, &at)| Self::element(k % 2 == 1, at, Span { start: at, end: at }))
                    .collect();
                out.extend(id, TOGGLE_BITS, 0, &toggles);
                if let Some(tms) = tms {
                    out.extend(id, ESCAPE_TMS, 0, &[Self::element(tms, at, cell)]);
                    self.clocks.push(Feed::Clock(Clocked {
                        rise: at,
                        cell,
                        tms,
                        tdi: None,
                        tdo: None,
                        tdo_fall: None,
                        from: Some(Inputs {
                            tms: Observation::Upstream {
                                id,
                                seq: ESCAPE_TMS,
                                index: 0,
                            },
                            tdi: None,
                            tdo: None,
                        }),
                    }));
                }
                out.close(id, t, Status::Complete);
            }
            Ev::SelectionBit(v) => {
                self.close_jscan(cell.start, out);
                let (id, n) = match self.selection {
                    Some(s) => s,
                    None => {
                        let id = Id::new(SELECTION, cell.start);
                        out.open(id, None, cell.start);
                        (id, 0)
                    }
                };
                out.extend(id, SELECTION_BITS, n, &[Self::element(v, at, cell)]);
                self.selection = Some((id, n.saturating_add(1)));
            }
            Ev::Selection(sel) => self.selection(&sel, t, out),
            Ev::Offline(by) => {
                let by = match by {
                    Offline::Deselection => 0,
                    Offline::Alert => 3,
                    Offline::StartUp => 4,
                };
                self.offline(t, by, out);
            }
            Ev::Alert { selection } => {
                // A Selection Alert spans its bits; a Deselection Alert, the bit that parks the
                // TAPC, whose TMS is already in a JScan stretch that ends with it.
                let start = match self.alert_from {
                    Some(a) if selection => a,
                    _ => cell.start,
                };
                self.close_jscan(t, out);
                self.close_all(start, Status::Complete, out);
                let id = Id::new(ALERT, start);
                out.open(id, None, start);
                out.set(id, ALERT_IS, Value::Name(u16::from(!selection)));
                out.close(id, t, Status::Complete);
            }
            Ev::SsdStart => self.ssd_from = Some(self.packet_from),
            Ev::Ssd {
                kind,
                payload,
                executed,
                state,
            } => {
                // Its own bits: the packet that executes or aborts it may start the next.
                let start = self.ssd_from.take().unwrap_or(self.packet_from);
                let end = self.before_to.max(start);
                let id = Id::new(SSD, start);
                out.open(id, None, start);
                if let Some(k) = kind {
                    let (n, field, bits) = match k {
                        SsdKind::DeselectAll => (0, None, 0),
                        SsdKind::SelectAll => (1, None, 0),
                        SsdKind::SelectCid => (2, Some(SSD_CID), 4),
                        SsdKind::SelectTca => (3, Some(SSD_TCA), 35),
                    };
                    out.set(id, DIRECTIVE, Value::Name(n));
                    if let (Some(f), Some(value)) = (field, payload) {
                        out.set(id, f, Value::Word { value, bits });
                    }
                }
                out.set(id, SSD_OUTCOME, Value::Name(u16::from(!executed)));
                let state = match state {
                    Some(s) => Value::Name(match s {
                        Ssd::SelectAll => 0,
                        Ssd::DeselectedIr => 1,
                        Ssd::DeselectedDr => 2,
                        Ssd::NotSelectedIr => 3,
                        Ssd::NotSelectedDr => 4,
                        Ssd::SelectedIr => 5,
                        Ssd::SelectedDr => 6,
                    }),
                    None => Value::Unknown {
                        reason: SSD_STATE_UNKNOWN,
                        needs: None,
                    },
                };
                out.set(id, SSD_STATE, state);
                out.close(id, end, Status::Complete);
            }
            Ev::DefaultFormat => {
                if let Some((id, _)) = self.format.take() {
                    out.close(id, t, Status::Complete);
                }
            }
            Ev::CrScan(cr) => self.cr_scan(&cr, t, out),
            Ev::Standard { tms } => {
                let (id, n) = match self.jscan {
                    Some(j) => j,
                    None => {
                        let id = Id::new(JSCAN, cell.start);
                        out.open(id, None, cell.start);
                        out.set(id, PROTOCOL, Value::Name(0));
                        (id, 0)
                    }
                };
                out.extend(id, JSCAN_TMS, n, &[Self::element(tms, at, cell)]);
                self.jscan = Some((id, n.saturating_add(1)));
                let tms_at = Observation::Upstream {
                    id,
                    seq: JSCAN_TMS,
                    index: n,
                };
                self.clocks.push(Feed::Clock(Clocked {
                    rise: at,
                    cell,
                    tms,
                    tdi: None,
                    tdo: None,
                    tdo_fall: None,
                    from: Some(Inputs {
                        tms: tms_at,
                        tdi: None,
                        tdo: None,
                    }),
                }));
            }
            Ev::CheckBit(v) => {
                self.close_jscan(cell.start, out);
                let (id, n) = match self.check {
                    Some(c) => c,
                    None => {
                        let id = Id::new(CHECK, cell.start);
                        out.open(id, self.level, cell.start);
                        (id, 0)
                    }
                };
                out.extend(id, CHECK_BITS, n, &[Self::element(v, at, cell)]);
                self.check = Some((id, n.saturating_add(1)));
            }
            Ev::CheckEnd { reset } => {
                if let Some((id, _)) = self.check.take() {
                    out.set(id, RESULT, Value::Name(if reset { 0 } else { 1 }));
                    out.close(id, t, Status::Complete);
                }
            }
            Ev::Format(f) => {
                if self.format.is_some_and(|(_, g)| g == f) {
                    return;
                }
                if let Some((id, _)) = self.format.take() {
                    out.close(id, t, Status::Complete);
                }
                let id = Id::new(FORMAT, t);
                out.open(id, None, t);
                out.set(id, SCAN_FORMAT, Value::Text(format_name(f)));
                let sampling = match self.tap.sampling() {
                    Some(rise) => Value::Name(if rise { 0 } else { 1 }),
                    None => Value::Unknown {
                        reason: SREDGE_UNKNOWN,
                        needs: None,
                    },
                };
                out.set(id, SAMPLING, sampling);
                self.format = Some((id, f));
            }
            Ev::DrBegin => self.dr_begins = [self.dr_begins[1], cell.start],
            Ev::Level(n) => {
                let start = self.dr_begins[1];
                let id = Id::new(LEVEL, start);
                out.open(id, None, start);
                let v = Value::Word {
                    value: n.into(),
                    bits: 3,
                };
                out.set(id, CONTROL_LEVEL, v);
                out.set(id, LEVEL_USE, Value::Name(level_use(n)));
                self.level = Some(id);
            }
            Ev::LevelExit => {
                if let Some((id, _)) = self.check.take() {
                    out.close(id, t, Status::Incomplete(LEVEL_ENDS));
                }
                if let Some(id) = self.level.take() {
                    out.close(id, t, Status::Complete);
                }
            }
            Ev::Command(c) => {
                let start = self.dr_begins[0];
                let id = Id::new(COMMAND, start);
                out.open(id, self.level, start);
                let opcode = u16::from(c.opcode).min(MNEMONICS.len() as u16);
                out.set(id, OPCODE, Value::Name(opcode));
                let operand = Value::Word {
                    value: c.operand.into(),
                    bits: 5,
                };
                out.set(id, OPERAND, operand);
                out.set(id, EFFECT, Value::Text(c.effect));
                let shifts = format!("{} and {}", c.shifts.0, c.shifts.1);
                out.set(id, SHIFTS, Value::Text(shifts));
                out.close(id, t, Status::Complete);
            }
            Ev::Element {
                format,
                driver,
                value,
            } => {
                self.close_jscan(cell.start, out);
                if self.slots.len() < 3 {
                    self.slots.push(Slot {
                        format,
                        value,
                        driver,
                        at,
                        cell,
                    });
                }
            }
            Ev::Packet {
                tdi,
                tms,
                tdo,
                advance,
            } => self.packet(tdi, tms, tdo, advance, out),
            Ev::NeedContext => self.close_all(t, Status::Incomplete(NEEDS_TAP), out),
            Ev::Unsupported(why) => self.close_all(t, Status::Incomplete(why), out),
        }
    }

    fn offline(&mut self, at: Tick, by: u16, out: &mut Out) {
        if self.offline.is_none() {
            let id = Id::new(OFFLINE, at);
            out.open(id, None, at);
            out.set(id, OFFLINE_BY, Value::Name(by));
            self.offline = Some(id);
        }
    }

    fn selection(&mut self, sel: &Selection, t: Tick, out: &mut Out) {
        let Some((id, _)) = self.selection.take() else {
            return;
        };
        let word = |value: u32, bits| Value::Word {
            value: value.into(),
            bits,
        };
        out.set(id, BRANCH, Value::Name(branch(sel.oac)));
        if let Some(ec) = sel.ec {
            out.set(id, PARKING, Value::Name(u16::from(ec & 3)));
            out.set(id, PROTECT, Value::Bool(ec & 4 != 0));
            out.set(id, FORM, Value::Name(u16::from(ec >> 3 & 1)));
        }
        if let Some(r) = sel.registers {
            out.set(id, READY, word(r >> 15 & 3, 2));
            out.set(id, DELAY, word(r >> 17 & 3, 2));
            out.set(id, TRANSPORT_STATES, word(r >> 10 & 31, 5));
        }
        let outcome = match sel.outcome {
            Outcome::Candidate { format } => {
                out.set(id, SELECTED_FORMAT, Value::Text(format_name(format)));
                0
            }
            Outcome::OtherTechnology => 1,
            Outcome::StateDiffers => 2,
        };
        out.set(id, OUTCOME, Value::Name(outcome));
        out.close(id, t, Status::Complete);
        if outcome != 0 {
            self.offline(t, outcome, out);
        }
    }

    fn cr_scan(&mut self, cr: &Cr, t: Tick, out: &mut Out) {
        let start = self.dr_begins[1];
        let id = Id::new(CR_SCAN, start);
        out.open(id, self.level, start);
        let (register, read, width) = match cr.path {
            Path::Register => reached(cr.opcode, cr.operand),
            // The bypass bit holds no register value.
            Path::Bypass | Path::Unknown => (BYPASS, None, 1),
        };
        let cgm_unknown = || Value::Unknown {
            reason: CGM_UNKNOWN,
            needs: None,
        };
        if cr.path == Path::Unknown {
            out.set(id, REGISTER, cgm_unknown());
            out.set(id, CR_VALUE, cgm_unknown());
        } else {
            out.set(id, REGISTER, Value::Name(register));
        }
        let length = Value::Word {
            value: cr.shifts.into(),
            bits: 32,
        };
        out.set(id, CR_LENGTH, length);
        if cr.on_wire {
            out.set(id, CR_TDI, jtag::pack(&cr.tdi));
            out.set(id, CR_TDO, jtag::pack(&cr.tdo));
            if let Some(read) = read {
                let bits = if read { &cr.tdo } else { &cr.tdi };
                let value = &bits[..bits.len().min(width)];
                out.set(id, CR_VALUE, jtag::pack(value));
                if let Some(m) = (register == 12).then(|| cnfg0(value)).flatten() {
                    out.set(id, CR_MEANING, Value::Text(m));
                }
            }
        } else {
            let why = || Value::Unknown {
                reason: NOT_ON_TMSC,
                needs: None,
            };
            out.set(id, CR_TDI, why());
            out.set(id, CR_TDO, why());
            if read.is_some() {
                out.set(id, CR_VALUE, why());
            }
        }
        out.close(id, t, Status::Complete);
    }

    fn packet(
        &mut self,
        tdi: Option<bool>,
        tms: bool,
        tdo: Option<bool>,
        advance: bool,
        out: &mut Out,
    ) {
        let (Some(first), Some(last)) = (self.slots.first().copied(), self.slots.last().copied())
        else {
            return;
        };
        let ntdi = (NTDI, NTDI_BITS);
        let tms_part = (PACKET_TMS, TMS_BITS);
        let pad = (PAD, PAD_BITS);
        let tdo_part = (PACKET_TDO, TDO_BITS);
        let parts: &[(FieldId, SeqId)] = match self.slots.get(1) {
            None => &[tms_part],
            Some(_) if self.slots.len() == 2 => &[ntdi, tdo_part],
            Some(s) if s.driver == Driver::Padding => &[ntdi, pad, tdo_part],
            Some(_) => &[ntdi, tms_part, tdo_part],
        };
        let id = Id::new(PACKET, first.at);
        out.open(id, None, first.cell.start);
        self.packet_from = first.cell.start;
        self.before_to = self.packet_to;
        self.packet_to = last.cell.end;
        out.set(id, PACKET_FORMAT, Value::Text(format_name(first.format)));
        let mut tms_slot = None;
        let protected = self.tap.protected();
        for (k, (slot, &(f, s))) in self.slots.iter().zip(parts).enumerate() {
            let driver = match slot.driver {
                Driver::Host => Some(PROBE),
                Driver::Target if self.qualified && !protected => Some(TARGET),
                Driver::Target | Driver::Padding => None,
            };
            out.part(id, f, Value::Bool(slot.value), slot.cell, driver);
            out.extend(id, s, 0, &[Self::element(slot.value, slot.at, slot.cell)]);
            if slot.driver == Driver::Target && (protected || !self.qualified) {
                let why = Value::Unknown {
                    reason: if protected { PROTECTED } else { UNQUALIFIED },
                    needs: None,
                };
                out.set(id, DRIVER, why);
            }
            if f == PACKET_TMS {
                tms_slot = Some(k);
            }
        }
        if let Some(v) = tdi {
            out.set(id, PACKET_TDI, Value::Bool(v));
        }
        if tms_slot.is_none() {
            out.set(id, PACKET_TMS, Value::Bool(tms));
        }
        if !advance {
            out.set(id, ADVANCE, Value::Bool(false));
        }
        out.close(id, last.cell.end, Status::Complete);
        let rise = self
            .slots
            .get(tms_slot.unwrap_or(0))
            .map_or(first.at, |s| s.at);
        self.slots.clear();
        if !advance {
            return;
        }
        let up = |seq| Observation::Upstream { id, seq, index: 0 };
        let tms_seq = if tms_slot.is_some() {
            TMS_BITS
        } else {
            NTDI_BITS
        };
        self.clocks.push(Feed::Clock(Clocked {
            rise,
            cell: Span {
                start: first.cell.start,
                end: last.cell.end,
            },
            tms,
            tdi,
            tdo,
            tdo_fall: None,
            from: Some(Inputs {
                tms: up(tms_seq),
                tdi: tdi.map(|_| up(NTDI_BITS)),
                tdo: tdo.map(|_| up(TDO_BITS)),
            }),
        }));
    }

    pub fn finish(&mut self, end: Tick, out: &mut Out) {
        self.close_all(end, Status::Complete, out);
    }
}

/// cJTAG under the host: TCKC and TMSC in windows of `window` ticks. A stretch where either line
/// is unknown is lost input, declared after the ops that close what it cuts short. `qualified`
/// assumes the target drives the TDO elements (the `assume_qualified` setting); without it their
/// driver is unknown, for a reason.
#[derive(Clone, Debug)]
pub struct Cjtag {
    /// Made from the first window's start levels.
    lines: Option<Lines>,
    /// Made once both lines are known.
    oscan: Option<Oscan>,
    qualified: bool,
    /// Offline at start-up, as bound.
    offline_start: bool,
    next: Tick,
    window: Tick,
    first: [Option<bool>; 2],
    changes: Vec<Change>,
    /// A window an interrupted turn left part read, and the next observation in it.
    held: Option<(Window, usize)>,
}

impl Cjtag {
    pub fn new(local: Option<Tick>, qualified: bool, window: Tick) -> Self {
        Self {
            lines: None,
            oscan: None,
            qualified,
            offline_start: false,
            next: local.unwrap_or(0),
            window: window.max(1),
            first: [None; 2],
            changes: vec![],
            held: None,
        }
    }

    /// Makes runs of cJTAG in windows of `window` ticks, qualified and started up as bound.
    pub fn factory(window: Tick) -> Factory {
        Box::new(move |b, local| {
            let qualified = b.flag(ASSUME_QUALIFIED).unwrap_or(false);
            let mut c = Self::new(local, qualified, window);
            c.offline_start = b.choice(START_UP) == Some(1);
            Box::new(c)
        })
    }

    fn lines(&mut self, from: Tick) -> &mut Lines {
        let (first, qualified, offline) = (self.first, self.qualified, self.offline_start);
        self.lines.get_or_insert_with(|| {
            if let [Some(clock), Some(line)] = first {
                let o = Oscan::new(from, clock, line, qualified).with_start_up(offline);
                self.oscan = Some(o);
            }
            Lines::new(from, first)
        })
    }

    /// Applies the lines' changes so far.
    fn apply(&mut self, ex: &mut Exchange<'_>) {
        let mut changes = std::mem::take(&mut self.changes);
        for c in changes.drain(..) {
            match (c, &mut self.oscan) {
                (Change::Gap { from, to, .. }, Some(o)) => {
                    if from < to {
                        o.change(&c, &mut ex.out);
                        ex.lost(from, to);
                    }
                }
                (
                    Change::Gap {
                        from,
                        to,
                        clock,
                        line,
                    },
                    None,
                ) => {
                    let o = Oscan::new(to, clock, line, self.qualified);
                    self.oscan = Some(o.with_start_up(self.offline_start));
                    if from < to {
                        ex.lost(from, to);
                    }
                }
                (_, Some(o)) => o.change(&c, &mut ex.out),
                (_, None) => {}
            }
            if let Some(o) = &mut self.oscan {
                o.clocks.clear();
            }
        }
        self.changes = changes;
    }

    fn frontier(&self, ex: &mut Exchange<'_>) {
        if let Some(o) = &self.oscan {
            ex.frontier(o.start_frontier());
        }
    }

    fn ask(&self, ex: &mut Exchange<'_>) {
        ex.request(Request::Window {
            from: self.next,
            upto: self.next.saturating_add(self.window),
        });
    }

    fn finish(&mut self, end: Tick, ex: &mut Exchange<'_>) {
        let gap = self.lines.as_ref().and_then(Lines::gap);
        if let Some(from) = gap
            && from < end
        {
            if let (Some(o), Some(l)) = (&mut self.oscan, &self.lines) {
                let [clock, line] = l.levels();
                let c = Change::Gap {
                    from,
                    to: end,
                    clock,
                    line,
                };
                o.change(&c, &mut ex.out);
                o.clocks.clear();
            }
            ex.lost(from, end);
        }
        if let Some(o) = &mut self.oscan {
            o.finish(end, &mut ex.out);
        }
        ex.finish(end);
    }
}

impl Decoder for Cjtag {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
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
                self.frontier(ex);
                self.held = Some((w, k));
                return Pause::Work;
            }
            match e {
                Observed::Start { port, level } => {
                    if self.lines.is_none()
                        && let Some(l) = self.first.get_mut(usize::from(port))
                    {
                        *l = level;
                    }
                }
                Observed::Unknown { at, .. } if w.end == Some(at) => {}
                e => {
                    let mut changes = std::mem::take(&mut self.changes);
                    self.lines(w.from).observe(&e, &mut changes);
                    self.changes = changes;
                    self.apply(ex);
                }
            }
        }
        let mut changes = std::mem::take(&mut self.changes);
        self.lines(w.from).flush(&mut changes);
        self.changes = changes;
        self.apply(ex);
        if let Some(end) = w.end {
            self.finish(end, ex);
            return Pause::Finished;
        }
        self.frontier(ex);
        self.next = w.upto;
        self.ask(ex);
        Pause::Input
    }

    fn save(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(self.clone()))
    }

    fn heap_bytes(&self) -> usize {
        let oscan = self.oscan.as_ref().map_or(0, |o| {
            crate::held::<Tick>(o.toggles.capacity())
                + crate::held::<Slot>(o.slots.capacity())
                + crate::held::<Ev>(o.events.capacity())
                + crate::held::<Feed>(o.clocks.capacity())
        });
        oscan + crate::held::<Change>(self.changes.capacity()) + crate::window_bytes(&self.held)
    }
}

// ---- JTAG on cJTAG -----------------------------------------------------------------------------

/// What a Scan Packet's pieces have said so far.
#[derive(Clone, Copy, Debug)]
struct PacketSoFar {
    id: Id,
    start: Tick,
    tms: Option<bool>,
    tdi: Option<bool>,
    tdo: Option<bool>,
    advance: bool,
    /// Where its TMS element, and its nTDI element, were sampled.
    tms_at: Option<Tick>,
    ntdi_at: Option<Tick>,
}

fn wire_tick(e: &Element) -> Option<Tick> {
    match e.at {
        Observation::Wire(t) => Some(t),
        _ => None,
    }
}

/// JTAG reading cJTAG's output (`jtag_subscription`): a clock per Standard Protocol TMS bit and per
/// Scan Packet that advances the TAP, each value naming the cJTAG element it came from. Lost input
/// is a gap, declared again for JTAG's own subscribers.
#[derive(Clone, Debug)]
pub struct JtagOnCjtag {
    tap: Tap,
    started: bool,
    packet: Option<PacketSoFar>,
    up: Option<Tick>,
}

impl JtagOnCjtag {
    pub fn new(local: Option<Tick>) -> Self {
        Self {
            tap: Tap::new(local.unwrap_or(0), true),
            started: false,
            packet: None,
            up: None,
        }
    }

    pub fn factory() -> Factory {
        Box::new(|b, local| {
            let mut j = Self::new(local);
            // Bound, each length is from 1 to `jtag::IR_AT_MOST`.
            let chain = (b.list(JTAG_CHAIN).unwrap_or(&[]).iter())
                .map(|&l| l as u8)
                .collect();
            j.tap = j.tap.chain(chain);
            Box::new(j)
        })
    }

    fn piece(&mut self, op: &Op, out: &mut Out) {
        match *op {
            Op::Open { id, start, .. } if id.kind == PACKET => {
                self.packet = Some(PacketSoFar {
                    id,
                    start,
                    tms: None,
                    tdi: None,
                    tdo: None,
                    advance: true,
                    tms_at: None,
                    ntdi_at: None,
                });
            }
            Op::Set {
                id,
                field,
                value: Value::Bool(v),
                ..
            } => {
                let Some(p) = self.packet.as_mut().filter(|p| p.id == id) else {
                    return;
                };
                match field {
                    PACKET_TMS => p.tms = Some(v),
                    PACKET_TDI => p.tdi = Some(v),
                    PACKET_TDO => p.tdo = Some(v),
                    ADVANCE => p.advance = v,
                    _ => {}
                }
            }
            Op::Extend {
                id,
                seq,
                offset,
                ref elements,
            } if (id.kind == JSCAN && seq == JSCAN_TMS)
                || (id.kind == ESCAPE && seq == ESCAPE_TMS) =>
            {
                for (k, e) in elements.iter().enumerate() {
                    let Some(rise) = wire_tick(e) else {
                        continue;
                    };
                    let tms = Observation::Upstream {
                        id,
                        seq,
                        index: offset.saturating_add(k as u32),
                    };
                    let c = Clocked {
                        rise,
                        cell: e.cell,
                        tms: e.value == 1,
                        tdi: None,
                        tdo: None,
                        tdo_fall: None,
                        from: Some(Inputs {
                            tms,
                            tdi: None,
                            tdo: None,
                        }),
                    };
                    self.tap.clock(&c, out);
                }
            }
            Op::Extend {
                id,
                seq,
                ref elements,
                ..
            } => {
                let Some(p) = self.packet.as_mut().filter(|p| p.id == id) else {
                    return;
                };
                let at = elements.first().and_then(wire_tick);
                match seq {
                    TMS_BITS => p.tms_at = at,
                    NTDI_BITS => p.ntdi_at = at,
                    _ => {}
                }
            }
            Op::Close { id, end, .. } if id.kind == PACKET => {
                let Some(p) = self.packet.take().filter(|p| p.id == id) else {
                    return;
                };
                let (true, Some(rise), Some(tms)) = (p.advance, p.tms_at.or(p.ntdi_at), p.tms)
                else {
                    return;
                };
                let up = |seq| Observation::Upstream {
                    id: p.id,
                    seq,
                    index: 0,
                };
                let tms_seq = if p.tms_at.is_some() {
                    TMS_BITS
                } else {
                    NTDI_BITS
                };
                let c = Clocked {
                    rise,
                    cell: Span {
                        start: p.start,
                        end,
                    },
                    tms,
                    tdi: p.tdi,
                    tdo: p.tdo,
                    tdo_fall: None,
                    from: Some(Inputs {
                        tms: up(tms_seq),
                        tdi: p.tdi.map(|_| up(NTDI_BITS)),
                        tdo: p.tdo.map(|_| up(TDO_BITS)),
                    }),
                };
                self.tap.clock(&c, out);
            }
            _ => {}
        }
    }
}

impl Decoder for JtagOnCjtag {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        if !std::mem::replace(&mut self.started, true) {
            self.tap.begin(&mut ex.out);
        }
        while let Some(d) = ex.receive() {
            match d {
                Delivery::Piece { piece, .. } => self.piece(piece.op(), &mut ex.out),
                Delivery::Lost { span, .. } => {
                    self.packet = None;
                    self.tap.gap(span.start, span.end, &mut ex.out);
                    ex.lost(span.start, span.end);
                }
                Delivery::Frontier { start, .. } => self.up = Some(start),
                Delivery::End { end, .. } => {
                    self.tap.finish(end, &mut ex.out);
                    ex.finish(end);
                    return Pause::Finished;
                }
                _ => {}
            }
            if ex.interrupted() {
                break;
            }
        }
        // A packet still being read started after the upstream frontier received: a batch's
        // pieces come before its frontier.
        if let Some(up) = self.up {
            ex.frontier(self.tap.start_frontier(up));
        }
        if ex.interrupted() && ex.more() {
            Pause::Work
        } else {
            Pause::Input
        }
    }

    fn save(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(self.clone()))
    }

    fn heap_bytes(&self) -> usize {
        self.tap.heap_bytes()
    }
}
