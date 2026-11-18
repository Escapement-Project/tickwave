//! TAP.7 (IEEE 1149.7) at the level of TCKC high phases: escapes; the Standard Protocol's TMS bits
//! (JScan); zero-bit DR scans, control levels and TAP.7 commands (5.3.3, 9.2, 9.4); Check Packets;
//! and the OScan1, 2, 5 and 6 Scan Packets of the Advanced Protocol (25), activated by command.
//!
//! Not decoded: selection and deselection escapes (11.6), three-part commands' CR scans beyond
//! skipping them, ready and delay elements, transport packets, SScan and MScan. Each is reported
//! when met. A bit is TMSC's level before TCKC falls whichever edge the target samples at; the two
//! agree while TMSC moves only with TCKC low.

use super::{FORMAT_UNFRAMED, READY_DELAY, SELECTION, TRANSPORT};
use crate::jtag;
use escapement_tickwave_decode::Cause;

const TLR: u8 = 0;
const SELECT_DR: u8 = 2;
const CAPTURE_DR: u8 = 3;
const SHIFT_DR: u8 = 4;
const UPDATE_DR: u8 = 8;
const SELECT_IR: u8 = 9;
const SHIFT_IR: u8 = 11;
const ALL: u16 = 0xFFFF;

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
    Escape(Escape),
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
    /// Not framed: after a selection escape, or after something unsupported, until a reset.
    Unknown,
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
        }
    }
}

impl Tap7 {
    fn tap(&self) -> Option<u8> {
        jtag::single(self.tap)
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
    }

    /// One TCKC high phase: TMSC's level before TCKC fell, and how many TMSC edges it held.
    pub fn phase(&mut self, value: bool, edges: usize, out: &mut Vec<Ev>) {
        match Escape::of(edges) {
            Some(Escape::Reset) => {
                out.push(Ev::Escape(Escape::Reset));
                self.reset();
                return;
            }
            Some(e @ (Escape::Selection | Escape::Deselection)) => {
                out.push(Ev::Escape(e));
                out.push(Ev::Unsupported(SELECTION));
                self.protocol = Protocol::Unknown;
                return;
            }
            Some(Escape::Custom) | None => {}
        }
        let custom = edges >= 2;
        match self.protocol {
            Protocol::Unknown => {}
            Protocol::Standard => {
                out.push(Ev::Standard { tms: value });
                let leaving_update = self.tap() == Some(UPDATE_DR);
                self.advance(value, out);
                // A non-JScan format stored in the Standard Protocol: a Check Packet follows the
                // TMS bit of the command's Update-DR (21.7.1; 4.3.2.4). A JScan one changes nothing
                // this decoder frames.
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
                        self.protocol = Protocol::Advanced(then);
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

    fn packet_bit(&mut self, format: u8, value: bool, custom: bool, out: &mut Vec<Ev>) {
        let Some(state) = self.tap() else {
            if [10, 13, 14].contains(&format) {
                out.push(Ev::NeedContext);
                self.protocol = Protocol::Unknown;
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
            self.protocol = Protocol::Unknown;
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
        let writes = self.advance(tms, out);
        if leaving_update && writes {
            self.spcp = true;
        }
    }

    /// One TAPC step with `tms`: DR scans, zero-bit scans, control levels and commands. Returns
    /// whether a command that writes a register completed with it.
    fn advance(&mut self, tms: bool, out: &mut Vec<Ev>) -> bool {
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
                    self.part = if (8..=11).contains(&c.opcode) { 2 } else { 0 };
                    let (writes, ecl) = (c.writes, c.opcode == 0 && c.operand == 1);
                    out.push(Ev::Command(c));
                    // The command is inside its control level; ECL ends the level after it.
                    if ecl {
                        self.exit_level(out);
                    }
                    return writes;
                }
                _ => self.part = 0,
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
            8..=11 => {
                let m = MNEMONICS.get(usize::from(opcode)).copied().unwrap_or("?");
                format!("{m} with a CR scan")
            }
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
