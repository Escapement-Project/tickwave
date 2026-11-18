//! TAP.7 (IEEE 1149.7) at the level of TCKC high phases: escapes; the Standard Protocol's TMS bits
//! (JScan); zero-bit DR scans, control levels and TAP.7 commands (5.3.3, 9.2, 9.4), with the CR
//! scans of the three-part ones (9.9); Check Packets; the OScan1, 2, 5 and 6 Scan Packets of the
//! Advanced Protocol (25), activated by command or by a Selection Sequence; and Offline operation
//! (11).
//!
//! A selection or deselection escape counts where 11.6.2 qualifies it: on the system path, and in
//! the Advanced Protocol on a Scan Packet's first bit. Elsewhere the controller ignores it and its
//! bit is data. A qualified deselection parks the TAPC and places the controller Offline; a
//! selection starts a Selection Sequence (11.7) that places it Online in the parking state its
//! Extension Code names, in the scan format the sequence gives, after a Check Packet. In the
//! Standard Protocol the escape's bit is a TMS bit, which moves the TAPC into its parking state
//! (table 11-5); in the Advanced Protocol the Scan Packet it starts is dropped (figure 11-15).
//! While the controller is not known to be Online, a selection escape starts a sequence and a
//! deselection places it Offline, as a debug and test system uses them only where qualified.
//!
//! Not decoded: ready and delay elements, transport packets, SScan and MScan, and the SUSPEND and
//! scan selection directive state that also qualify escapes. Each is reported when met. A bit is
//! TMSC's level before TCKC falls whichever edge the target samples at; the two agree while TMSC
//! moves only with TCKC low.

use super::{FORMAT_UNFRAMED, READY_DELAY, TRANSPORT};
use crate::jtag;
use escapement_tickwave_decode::Cause;

const TLR: u8 = 0;
const RTI: u8 = 1;
const SELECT_DR: u8 = 2;
const CAPTURE_DR: u8 = 3;
const SHIFT_DR: u8 = 4;
const PAUSE_DR: u8 = 6;
const UPDATE_DR: u8 = 8;
const SELECT_IR: u8 = 9;
const SHIFT_IR: u8 = 11;
const PAUSE_IR: u8 = 13;
const ALL: u16 = 0xFFFF;
/// The most of a CR scan's TDI and TDO bits kept.
pub const CR_KEPT: usize = 256;

pub const MNEMONICS: [&str; 16] = [
    "STMC", "STC1", "STC2", "STFMT", "STTPST", "STTESTM", "MCM", "MSC", "SCNB", "SCNS", "CIDA",
    "CIDD", "EXC0", "EXC1", "EXC2", "EXC3",
];

/// What a TCKC high phase's TMSC edges make of it (10.4; n and n + 1 edges are one escape).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escape {
    /// Two or three edges: in a compact Shift packet, the end-of-transfer mark.
    Custom,
    Deselection,
    Selection,
    Reset,
}

impl Escape {
    pub fn of(edges: usize) -> Option<Self> {
        match edges {
            0 | 1 => None,
            2 | 3 => Some(Self::Custom),
            4 | 5 => Some(Self::Deselection),
            6 | 7 => Some(Self::Selection),
            _ => Some(Self::Reset),
        }
    }
}

/// How the controller takes a selection or deselection escape (11.3.3, 11.6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Response {
    Taken,
    /// Online, where the escape is not qualified: ignored, its bit data.
    Unqualified,
    /// A deselection escape while Offline: ignored.
    Offline,
}

/// Why a Selection Sequence left the controller Offline, or the scan format the Check Packet after
/// it places the controller Online in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Candidate {
        format: u8,
    },
    /// The Online Activation Code names another technology (table 11-1).
    OtherTechnology,
    /// The TAPC is not parked in the state the Extension Code names (rule 11.9.5.2 b).
    StateDiffers,
}

/// A Selection Sequence's fields as its bits give them (tables 11-1, 11-2 and 11-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub oac: u8,
    pub ec: Option<u8>,
    /// The Global Register State, from a long-form sequence.
    pub registers: Option<u32>,
    pub outcome: Outcome,
}

/// A three-part command's CR scan (9.9): its command, length, and the TDI and TDO bits the Scan
/// Packets carried, the first `CR_KEPT` of them; none in the Standard Protocol, where they are not
/// on TMSC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cr {
    pub opcode: u8,
    pub operand: u8,
    pub shifts: u32,
    pub on_wire: bool,
    pub tdi: Vec<bool>,
    pub tdo: Vec<bool>,
}

/// Who drives a Scan Packet element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Driver {
    Host,
    Target,
    Padding,
}

/// What a command does, for a reader; `writes` whether it stores a TAP.7 register, which in the
/// Advanced Protocol brings a Scan Packet with no TAPC advance and a Check Packet after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub opcode: u8,
    pub operand: u8,
    pub effect: String,
    pub writes: bool,
    /// The two DR scans' Shift-DR counts, before reduction to five bits.
    pub shifts: (u32, u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ev {
    /// An escape; for a selection or deselection one, how it is taken, and in the Standard
    /// Protocol the TMS bit it carries.
    Escape {
        kind: Escape,
        response: Option<Response>,
        tms: Option<bool>,
    },
    /// One bit of a Selection Sequence, then the sequence once its fields are known.
    SelectionBit(bool),
    Selection(Selection),
    /// A deselection escape placed the controller Offline.
    Offline,
    CrScan(Cr),
    /// A Standard Protocol bit: TMS.
    Standard {
        tms: bool,
    },
    /// One element of a Scan Packet.
    Element {
        format: u8,
        driver: Driver,
        value: bool,
    },
    /// A whole Scan Packet; `advance` false for the one before a Check Packet.
    Packet {
        tdi: Option<bool>,
        tms: bool,
        tdo: Option<bool>,
        advance: bool,
    },
    CheckBit(bool),
    /// The Check Packet ends; `reset` for the directive that resets (two ones).
    CheckEnd {
        reset: bool,
    },
    /// A DR scan begins: Capture-DR is entered.
    DrBegin,
    /// The ZBS count is locked as a control level, and the level is exited.
    Level(u8),
    LevelExit,
    Command(Command),
    /// A scan format takes effect.
    Format(u8),
    /// Framing needs a TAPC state that is not known.
    NeedContext,
    Unsupported(Cause),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
    /// Not framed: from the start, a gap or something unsupported, until a reset or selection
    /// escape.
    Unknown,
    /// Deselected: the bits are another controller's, until a selection or reset escape.
    Offline,
    /// A Selection Sequence's bits so far, the first in bit 0.
    Selection {
        bits: u32,
        count: u8,
    },
    Standard,
    Advanced(u8),
    /// A Check Packet, then `then`.
    Check {
        then: u8,
        preamble: bool,
        previous: Option<bool>,
        post: bool,
        reset: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Tap7 {
    protocol: Protocol,
    tap: u16,
    shifts: Option<u32>,
    zbs: u8,
    level: Option<u8>,
    /// Inside the command control level: which DR scan comes next (0: CP1, 1: CP2, 2: a CR scan),
    /// and CP1's count.
    part: u8,
    cp1: u32,
    /// After a register write in the Advanced Protocol (21.7.1): the next Scan Packet does not
    /// advance the TAPC and a Check Packet follows it.
    spcp: bool,
    /// A stored format, taking effect after the Check Packet.
    stored: Option<u8>,
    inhibited: bool,
    /// SREDGE: in the Control and Advanced Protocols the target samples TMSC at TCKC's rise; zero,
    /// as after a reset, at its fall (23.4.1.4.4).
    sredge: bool,
    /// CGM, which conditional stores depend on.
    cgm: bool,
    slot: usize,
    values: [bool; 3],
    eot: bool,
    previous_shift: bool,
    /// Drive conflict protection, from a Selection Sequence's PROTECT bit (table 11-3): the target
    /// drives only once it alone is a group member, which is not followed.
    protected: bool,
    /// The CR scan a three-part command brings, from the command's end to its Update-DR.
    cr: Option<Cr>,
}

impl Default for Tap7 {
    fn default() -> Self {
        Self {
            protocol: Protocol::Unknown,
            tap: ALL,
            shifts: None,
            zbs: 0,
            level: None,
            part: 0,
            cp1: 0,
            spcp: false,
            stored: None,
            inhibited: false,
            sredge: false,
            cgm: false,
            slot: 0,
            values: [false; 3],
            eot: false,
            previous_shift: false,
            protected: false,
            cr: None,
        }
    }
}

impl Tap7 {
    fn tap(&self) -> Option<u8> {
        jtag::single(self.tap)
    }

    /// Whether a Selection Sequence turned drive conflict protection on.
    pub fn protected(&self) -> bool {
        self.protected
    }

    /// Whether the target samples the next bit at TCKC's rise; otherwise at its fall.
    pub fn sampled_at_rise(&self) -> bool {
        matches!(self.protocol, Protocol::Standard) || self.sredge
    }

    /// Which DR scan a control level or command still to come would start at, if one has begun:
    /// 0 for the latest, 1 for the one before it.
    pub fn pending_scan(&self) -> Option<usize> {
        if self.inhibited {
            return None;
        }
        let scanning = self.shifts.is_some();
        match (self.level, self.part) {
            // A nonzero DR scan after zero-bit ones locks a control level, from its own start.
            (None, _) if self.zbs > 0 && scanning => Some(0),
            // A command runs from its first DR scan's start to its second's end.
            (Some(2), 0) if scanning => Some(0),
            (Some(2), 1) => Some(usize::from(scanning)),
            // A CR scan from its own start.
            (Some(2), 2) if scanning => Some(0),
            _ => None,
        }
    }

    /// A recording gap: nothing about the controller is known after it.
    pub fn gap(&mut self) {
        *self = Self::default();
    }

    fn reset(&mut self) {
        *self = Self {
            protocol: Protocol::Standard,
            tap: 1 << TLR,
            ..Self::default()
        };
    }

    fn exit_level(&mut self, out: &mut Vec<Ev>) {
        if self.level.take().is_some() {
            out.push(Ev::LevelExit);
        }
        self.zbs = 0;
        self.part = 0;
        self.cr = None;
    }

    /// Framing is lost: nothing about the protocol or the TAPC is known until an escape.
    fn lose(&mut self) {
        self.protocol = Protocol::Unknown;
        self.tap = ALL;
    }

    /// One TCKC high phase: TMSC's level before TCKC fell, and how many TMSC edges it held.
    pub fn phase(&mut self, value: bool, edges: usize, out: &mut Vec<Ev>) {
        match Escape::of(edges) {
            Some(Escape::Reset) => {
                out.push(Ev::Escape {
                    kind: Escape::Reset,
                    response: None,
                    tms: None,
                });
                self.reset();
                return;
            }
            Some(kind @ (Escape::Selection | Escape::Deselection)) => {
                if self.escape(kind, value, out) {
                    return;
                }
            }
            Some(Escape::Custom) | None => {}
        }
        let custom = edges >= 2;
        match self.protocol {
            Protocol::Unknown | Protocol::Offline => {}
            Protocol::Selection { bits, count } => self.selection_bit(bits, count, value, out),
            Protocol::Standard => {
                out.push(Ev::Standard { tms: value });
                self.standard(value, out);
            }
            Protocol::Check {
                then,
                preamble,
                previous,
                post,
                reset,
            } => {
                out.push(Ev::CheckBit(value));
                if post {
                    out.push(Ev::CheckEnd { reset });
                    if reset {
                        self.reset();
                    } else {
                        // A JScan format brings the Standard Protocol (table 11-8).
                        self.protocol = if then < 4 {
                            Protocol::Standard
                        } else {
                            Protocol::Advanced(then)
                        };
                        self.slot = 0;
                        self.previous_shift = false;
                        out.push(Ev::Format(then));
                    }
                } else if !preamble {
                    self.protocol = Protocol::Check {
                        then,
                        preamble: true,
                        previous: None,
                        post: false,
                        reset: false,
                    };
                } else {
                    let terminal = previous == Some(value);
                    self.protocol = Protocol::Check {
                        then,
                        preamble: true,
                        previous: Some(value),
                        post: terminal,
                        reset: terminal && value,
                    };
                }
            }
            Protocol::Advanced(format) => self.packet_bit(format, value, custom, out),
        }
    }

    /// A selection or deselection escape. Returns whether it is taken, or shown as ignored; if
    /// not, its bit is data, as the controller takes it.
    fn escape(&mut self, kind: Escape, value: bool, out: &mut Vec<Ev>) -> bool {
        let system_path = self.level.is_none() && self.zbs == 0;
        let response = match self.protocol {
            Protocol::Standard | Protocol::Advanced(_) if !system_path => Response::Unqualified,
            Protocol::Advanced(_) if self.slot != 0 => Response::Unqualified,
            Protocol::Offline if kind == Escape::Deselection => Response::Offline,
            // A Check Packet or a Selection Sequence answers only a selection escape.
            Protocol::Check { .. } | Protocol::Selection { .. } if kind == Escape::Deselection => {
                Response::Unqualified
            }
            _ => Response::Taken,
        };
        let standard = self.protocol == Protocol::Standard;
        // Only the Standard Protocol shows an escape it ignores: elsewhere the bit is a Scan
        // Packet's, Check Packet's or Selection Sequence's like any other.
        if response == Response::Unqualified && !standard {
            return false;
        }
        out.push(Ev::Escape {
            kind,
            response: Some(response),
            tms: standard.then_some(value),
        });
        if standard {
            self.standard(value, out);
        }
        if response != Response::Taken {
            return true;
        }
        self.slot = 0;
        self.eot = false;
        self.protocol = match kind {
            Escape::Deselection => {
                out.push(Ev::Offline);
                Protocol::Offline
            }
            _ => Protocol::Selection { bits: 0, count: 0 },
        };
        true
    }

    /// A Standard Protocol TMS bit.
    fn standard(&mut self, value: bool, out: &mut Vec<Ev>) {
        let leaving_update = self.tap() == Some(UPDATE_DR);
        self.advance(value, None, out);
        // A non-JScan format stored in the Standard Protocol: a Check Packet follows the TMS bit
        // of the command's Update-DR (21.7.1; 4.3.2.4). A JScan one changes nothing this decoder
        // frames.
        if leaving_update
            && let Some(f) = self.stored.take()
            && f >= 4
        {
            self.protocol = Protocol::Check {
                then: f,
                preamble: false,
                previous: None,
                post: false,
                reset: false,
            };
        }
    }

    /// One bit of a Selection Sequence (11.7): the Online Activation Code, the Extension Code,
    /// and in the long form the Global Register State, each first bit first.
    fn selection_bit(&mut self, bits: u32, count: u8, value: bool, out: &mut Vec<Ev>) {
        out.push(Ev::SelectionBit(value));
        let bits = bits | u32::from(value) << count;
        let count = count + 1;
        let oac = (bits & 15) as u8;
        let ec = (bits >> 4 & 15) as u8;
        let done = |out: &mut Vec<Ev>, ec, registers, outcome| {
            out.push(Ev::Selection(Selection {
                oac,
                ec,
                registers,
                outcome,
            }));
        };
        match count {
            // OAC[1:0] other than 00 selects another technology (table 11-1).
            4 if oac & 3 != 0 => {
                done(out, None, None, Outcome::OtherTechnology);
                self.protocol = Protocol::Offline;
            }
            8 => {
                let need = match ec & 3 {
                    0 => 1 << TLR | 1 << RTI,
                    1 => 1 << SELECT_DR,
                    2 => 1 << PAUSE_IR,
                    _ => 1 << PAUSE_DR,
                };
                let parked = self.tap & need;
                if parked == 0 {
                    done(out, Some(ec), None, Outcome::StateDiffers);
                    self.protocol = Protocol::Offline;
                } else if ec & 8 != 0 {
                    // The short form takes its scan format from the topology (table 11-11).
                    let format = [2, 2, 3, 9][usize::from(oac >> 2)];
                    self.select(parked, ec, format);
                    done(out, Some(ec), None, Outcome::Candidate { format });
                } else {
                    self.tap = parked;
                    self.protocol = Protocol::Selection { bits, count };
                }
            }
            32 => {
                let registers = bits >> 8;
                let format = (registers >> 19 & 31) as u8;
                self.select(self.tap, ec, format);
                done(
                    out,
                    Some(ec),
                    Some(registers),
                    Outcome::Candidate { format },
                );
                // RDYC and DLYC, then TPST: elements this decoder does not frame.
                if format >= 4 && registers >> 15 & 15 != 0 {
                    out.push(Ev::Unsupported(READY_DELAY));
                    self.lose();
                } else if format >= 4 && registers >> 10 & 31 != 0 {
                    out.push(Ev::Unsupported(TRANSPORT));
                    self.lose();
                }
            }
            _ => self.protocol = Protocol::Selection { bits, count },
        }
    }

    /// The selection test passed: the TAPC is parked in `parked`, and the Check Packet next places
    /// the controller Online in `format`. The test state clears ZBSINH, SUSPEND and ECL (11.7.8.1).
    fn select(&mut self, parked: u16, ec: u8, format: u8) {
        *self = Self {
            protocol: Protocol::Check {
                then: format,
                preamble: false,
                previous: None,
                post: false,
                reset: false,
            },
            tap: parked,
            sredge: self.sredge,
            cgm: self.cgm,
            protected: ec & 4 != 0,
            ..Self::default()
        };
    }

    fn packet_bit(&mut self, format: u8, value: bool, custom: bool, out: &mut Vec<Ev>) {
        let Some(state) = self.tap() else {
            if [10, 13, 14].contains(&format) {
                out.push(Ev::NeedContext);
                self.lose();
                return;
            }
            // OScan1 frames alike in every state.
            return self.frame(format, false, value, custom, out);
        };
        let shifting = state == SHIFT_DR || state == SHIFT_IR;
        self.frame(format, shifting, value, custom, out);
    }

    fn frame(&mut self, format: u8, shifting: bool, value: bool, custom: bool, out: &mut Vec<Ev>) {
        if ![9, 10, 13, 14].contains(&format) {
            out.push(Ev::Unsupported(FORMAT_UNFRAMED));
            self.lose();
            return;
        }
        let advance = !self.spcp;
        let short_nonshift = [10, 14].contains(&format) && !shifting;
        let compact = [13, 14].contains(&format) && shifting;
        let pad = format == 14 && shifting && !self.previous_shift;
        let length = if short_nonshift {
            1
        } else if compact && !pad {
            2
        } else {
            3
        };
        let driver = if short_nonshift {
            Driver::Host
        } else if pad && self.slot == 1 {
            Driver::Padding
        } else if self.slot == length - 1 {
            Driver::Target
        } else {
            Driver::Host
        };
        if let Some(v) = self.values.get_mut(self.slot) {
            *v = value;
        }
        if custom && self.slot == 0 && compact {
            self.eot = true;
        }
        out.push(Ev::Element {
            format,
            driver,
            value,
        });
        self.slot += 1;
        if self.slot < length {
            return;
        }
        let tms = if short_nonshift {
            self.values[0]
        } else if compact {
            self.eot
        } else {
            self.values[1]
        };
        let tdi = (!short_nonshift).then_some(!self.values[0]);
        let tdo = (!short_nonshift).then(|| self.values.get(length - 1).copied().unwrap_or(false));
        out.push(Ev::Packet {
            tdi,
            tms,
            tdo,
            advance,
        });
        self.slot = 0;
        self.eot = false;
        if !advance {
            self.spcp = false;
            let then = self.stored.take().unwrap_or(format);
            self.protocol = Protocol::Check {
                then,
                preamble: false,
                previous: None,
                post: false,
                reset: false,
            };
            return;
        }
        self.previous_shift = shifting;
        let leaving_update = self.tap() == Some(UPDATE_DR);
        let writes = self.advance(tms, Some((tdi, tdo)), out);
        if leaving_update && writes {
            self.spcp = true;
        }
    }

    /// One TAPC step with `tms`, and in the Advanced Protocol the TDI and TDO its Scan Packet
    /// carried: DR scans, zero-bit scans, control levels, commands and CR scans. Returns whether a
    /// command that writes a register completed with it.
    fn advance(
        &mut self,
        tms: bool,
        data: Option<(Option<bool>, Option<bool>)>,
        out: &mut Vec<Ev>,
    ) -> bool {
        let before = self.tap();
        self.tap = jtag::step(self.tap, tms);
        let after = self.tap();
        let mut writes = false;
        match (before, after) {
            (Some(SELECT_DR), Some(CAPTURE_DR)) => {
                self.shifts = Some(0);
                out.push(Ev::DrBegin);
            }
            (Some(SHIFT_DR), _) => {
                if let Some(n) = self.shifts.as_mut() {
                    *n = n.saturating_add(1);
                }
                if let Some(cr) = self.cr.as_mut() {
                    cr.shifts = cr.shifts.saturating_add(1);
                    match data {
                        Some((Some(i), Some(o))) if cr.tdi.len() < CR_KEPT => {
                            cr.tdi.push(i);
                            cr.tdo.push(o);
                        }
                        Some((Some(_), Some(_))) => {}
                        _ => cr.on_wire = false,
                    }
                }
            }
            (Some(UPDATE_DR), _) => {
                if let Some(n) = self.shifts.take() {
                    writes = self.dr_done(n, out);
                }
            }
            _ => {}
        }
        if after == Some(SELECT_IR) || after == Some(TLR) {
            self.exit_level(out);
            if after == Some(TLR) {
                self.inhibited = false;
            }
        }
        writes
    }

    fn dr_done(&mut self, shifts: u32, out: &mut Vec<Ev>) -> bool {
        if self.inhibited {
            return false;
        }
        match self.level {
            None if shifts == 0 => self.zbs = (self.zbs + 1).min(7),
            None if self.zbs > 0 => {
                self.level = Some(self.zbs);
                self.part = 0;
                out.push(Ev::Level(self.zbs));
            }
            None => {}
            Some(2) => match self.part {
                0 => {
                    self.cp1 = shifts;
                    self.part = 1;
                }
                1 => {
                    let c = self.command(self.cp1, shifts, out);
                    // SCNB, SCNS and CIDA bring a CR scan (table 9-3); CIDD does not.
                    let three = (8..=10).contains(&c.opcode);
                    self.part = if three { 2 } else { 0 };
                    self.cr = three.then(|| Cr {
                        opcode: c.opcode,
                        operand: c.operand,
                        shifts: 0,
                        on_wire: true,
                        tdi: vec![],
                        tdo: vec![],
                    });
                    let (writes, ecl) = (c.writes, c.opcode == 0 && c.operand == 1);
                    out.push(Ev::Command(c));
                    // The command is inside its control level; ECL ends the level after it.
                    if ecl {
                        self.exit_level(out);
                    }
                    return writes;
                }
                _ => {
                    self.part = 0;
                    if let Some(cr) = self.cr.take() {
                        out.push(Ev::CrScan(cr));
                    }
                }
            },
            Some(_) => {}
        }
        false
    }

    fn command(&mut self, cp1: u32, cp2: u32, out: &mut Vec<Ev>) -> Command {
        let opcode = (cp1 & 31) as u8;
        let operand = (cp2 & 31) as u8;
        let (b, x, y) = (operand >> 2, (operand >> 1) & 1, operand & 1);
        // That a scan command brings a Scan Packet without advance and a Check Packet too is seen
        // in the project's OScan2, 5 and 6 captures after SCNS, not read in the standard;
        // enumerate commands are assumed alike, unchecked.
        let mut writes = opcode <= 11;
        let effect = match opcode {
            0 => match (b, x, y) {
                (0, 0, 0) => {
                    writes = false;
                    "no operation".into()
                }
                (0, 0, 1) => "exit the control level (ECL)".into(),
                (0, 1, 0) => "suspend the control levels (SUSPEND)".into(),
                (0, 1, 1) => {
                    self.inhibited = true;
                    "inhibit zero-bit scans (ZBSINH)".into()
                }
                (1, 0, v) => format!("Scan Group Candidate SGC = {v}"),
                (1, 1, v) => {
                    self.cgm = v == 1;
                    format!("Conditional Group Member CGM = {v}")
                }
                (2, _, _) | (3, _, _) => {
                    let what = if b == 2 { "RDYC" } else { "DLYC" };
                    let v = operand & 3;
                    if v != 0 {
                        out.push(Ev::Unsupported(READY_DELAY));
                    }
                    format!("{what} = {v}")
                }
                _ => format!("STMC subcommand {operand:#07b}"),
            },
            1 => {
                let (c, bit, v) = (operand >> 4, (operand >> 1) & 7, operand & 1);
                let name = ["SREDGE", "FRESET", "TRESET"].get(bit as usize).copied();
                if bit == 0 && (c == 0 || self.cgm) {
                    self.sredge = v == 1;
                }
                format!(
                    "{} = {v}{}",
                    name.unwrap_or("reserved bit"),
                    if c == 1 { ", if CGM" } else { "" }
                )
            }
            2 => format!("two-bit store {operand:#07b}"),
            3 => {
                self.stored = Some(operand);
                format!("SCNFMT = {}", format_name(operand))
            }
            4 => {
                if operand != 0 {
                    out.push(Ev::Unsupported(TRANSPORT));
                }
                format!("transport states {operand:#07b}")
            }
            5 => format!("test mode {operand:#07b}"),
            6 => {
                // A reset leaves controller ID zero valid (9.4.1.7); the probes here address it.
                if operand & 15 == 0 {
                    self.cgm = true;
                }
                format!("Conditional Group Member for controller {}", operand & 15)
            }
            7 => format!("Scan Group Candidate: controller {}", operand & 15),
            8..=10 => {
                let m = MNEMONICS.get(usize::from(opcode)).copied().unwrap_or("?");
                format!("{m} with a CR scan")
            }
            11 if operand & 16 != 0 => "de-allocate every controller ID".into(),
            11 => format!("de-allocate controller ID {}", operand & 15),
            12..=15 => "private command".into(),
            _ => "reserved".into(),
        };
        Command {
            opcode,
            operand,
            effect,
            writes,
            shifts: (cp1, cp2),
        }
    }
}

pub fn format_name(f: u8) -> String {
    match f {
        0..=3 => format!("JScan{f}"),
        8..=15 => format!("OScan{}", f - 8),
        _ => format!("format {f}"),
    }
}
