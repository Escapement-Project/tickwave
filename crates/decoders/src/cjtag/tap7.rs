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
//! SUSPEND (17.5) stops control levels being made and puts the System Path in use, so escapes
//! qualify, until the eighth zero-bit scan in a row, Test-Logic-Reset or a selection test. CGM and
//! SGC follow the commands that store them and the controller ID they are addressed by (9.4.1.7,
//! table 9-4), so an SCNS or private SCNB scan reaches its register only where CGM is 1, else the
//! EPU bypass bit (table 9-10). A Selection Alert (10.5) while the controller is not known to be
//! Online starts a Selection Sequence; a Deselection Alert, control level six then Run-Test/Idle
//! (10.6), places it Offline unless a CNFG0 read says it has none. A controller Offline at start-up
//! (11.9.7), as the `start_up` setting or a CNFG0 read's DAS bit says, comes up Offline after a
//! reset escape and takes a selection escape only after its alternating preamble.
//!
//! Scan Selection Directives (20.10) are followed where they are on TMSC, in OScan1 and OScan5:
//! their SSD state, one TCK late, qualifies escapes (11.6.2), and in Run-Test/Idle they store SGC.
//! In JScan3 they are TDI data off TMSC, so with SSDE set the SSD state is unknown there, and an
//! escape whose qualification it decides ends the framing.
//!
//! Not decoded: ready and delay elements, transport packets, SScan and MScan. Each is reported
//! when met. A bit is
//! TMSC's level before TCKC falls whichever edge the target samples at; the two agree while TMSC
//! moves only with TCKC low.

use super::{FORMAT_UNFRAMED, READY_DELAY, SSD_UNKNOWN, TRANSPORT};
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
/// The Selection Alert, first bit (the start bit, 0) in bit 0 (10.5.1.2).
const SELECTION_ALERT: u128 = 0x19BC_0EA2_E3DD_AFE9_8685_2D95_6209_F392;
/// The alert's 128 bits, then the four padding bits before its Selection Sequence.
const ALERT_BITS: u8 = 128 + 4;
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
    /// The SSD state that decides it is not known.
    Unknown,
}

/// A Scan Selection Directive (table 20-14).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SsdKind {
    DeselectAll,
    SelectAll,
    /// Select one by its controller ID.
    SelectCid,
    /// Select one by its TAP.7 Controller Address.
    SelectTca,
}

/// The SSD state (table 20-13): who may be a Pause-xR Group Member after the last SSD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ssd {
    SelectAll,
    DeselectedIr,
    DeselectedDr,
    NotSelectedIr,
    NotSelectedDr,
    SelectedIr,
    SelectedDr,
}

/// What placed the controller Offline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offline {
    Deselection,
    /// A Deselection Alert (10.6).
    Alert,
    /// A reset with the Offline-at-Start-up option (11.9.7).
    StartUp,
}

/// What a three-part command's CR scan reaches (tables 9-9 and 9-10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    /// The register its command and operand name.
    Register,
    /// The EPU bypass bit: CGM is 0.
    Bypass,
    /// CGM is not known.
    Unknown,
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
    pub path: Path,
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
    /// The controller went Offline.
    Offline(Offline),
    /// A Selection Alert, once its padding bits are in, or a Deselection Alert where it is taken.
    Alert {
        selection: bool,
    },
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
    /// Test-Logic-Reset brings back the default scan format, whose Standard Protocol follows.
    DefaultFormat,
    /// A Scan Selection Directive's start bit.
    SsdStart,
    /// A Scan Selection Directive ends: executed, with the SSD state after it where known, or
    /// aborted; its kind once its directive bits are in, and its payload.
    Ssd {
        kind: Option<SsdKind>,
        payload: Option<u64>,
        executed: bool,
        state: Option<Ssd>,
    },
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
    /// Offline at start-up (11.9.7): a selection escape needs its preamble.
    StartUp,
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

/// The controller's ID (9.4.1.7, table 9-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cid {
    /// After a reset: zero, valid, and selection commands naming another ID do nothing, until a
    /// CIDD.
    Reset,
    Valid(u8),
    Invalid,
    Unknown,
}

/// A Scan Selection Directive under way: the TAPC state it is sent in, and its TDI bits so far,
/// the first in bit 0.
#[derive(Clone, Copy, Debug)]
struct SsdRun {
    state: u8,
    bits: u64,
    count: u8,
}

impl SsdRun {
    fn kind(&self) -> Option<SsdKind> {
        (self.count >= 3).then_some(match (self.bits >> 1 & 1, self.bits >> 2 & 1) {
            (0, 0) => SsdKind::DeselectAll,
            (0, _) => SsdKind::SelectAll,
            (_, 0) => SsdKind::SelectCid,
            _ => SsdKind::SelectTca,
        })
    }

    /// Its size in bits, directive and payload (table 20-14), once the directive is in.
    fn size(&self) -> u8 {
        match self.kind() {
            None => 3,
            Some(SsdKind::DeselectAll | SsdKind::SelectAll) => 3,
            Some(SsdKind::SelectCid) => 7,
            Some(SsdKind::SelectTca) => 38,
        }
    }
}

/// The Selection Alert detector: consecutive 1s before a start bit, then how many of the alert's
/// bits have matched.
#[derive(Clone, Copy, Debug, Default)]
struct Alert {
    ones: u8,
    matched: u8,
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
    /// SREDGE, where known: in the Control and Advanced Protocols the target samples TMSC at
    /// TCKC's rise; zero, as after a reset, at its fall (23.4.1.4.4).
    sredge: Option<bool>,
    /// CGM, where known: conditional stores and the CR scans of SCNS and private SCNB depend on it.
    cgm: Option<bool>,
    /// SGC, where known.
    sgc: Option<bool>,
    cid: Cid,
    /// SUSPEND (17.5).
    suspend: bool,
    /// Whether a reset leaves the controller Offline: the `start_up` setting until a CNFG0 read's
    /// DAS bit says.
    offline_start: bool,
    /// Whether the controller takes Deselection Alerts, where a CNFG0 read says.
    dsel_alerts: Option<bool>,
    /// Control level six reached Run-Test/Idle: the next bit's TAPC state is where a Deselection
    /// Alert parks it (10.6).
    parking: bool,
    alert: Alert,
    /// Offline at start-up: the last bits, the latest in bit 0, and how many since an escape.
    history: u32,
    since_escape: u8,
    /// SSDE, where known: whether Scan Selection Directives are processed.
    ssde: Option<bool>,
    /// The SSD state where known, and the same one TCK late (the Delayed SSD State).
    ssd: Option<Ssd>,
    ssd_late: Option<Ssd>,
    ssd_run: Option<SsdRun>,
    /// The Standard Protocol is in JScan3, where SSDs are TDI data off TMSC.
    jscan3: bool,
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
            sredge: None,
            cgm: None,
            sgc: None,
            cid: Cid::Unknown,
            suspend: false,
            offline_start: false,
            dsel_alerts: None,
            parking: false,
            alert: Alert::default(),
            history: 0,
            since_escape: 0,
            ssde: None,
            ssd: None,
            ssd_late: None,
            ssd_run: None,
            jscan3: false,
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
    /// A controller that comes up Offline after a reset when `offline_start`.
    pub fn new(offline_start: bool) -> Self {
        Self {
            offline_start,
            ..Self::default()
        }
    }

    fn tap(&self) -> Option<u8> {
        jtag::single(self.tap)
    }

    /// How many bits of a Selection Alert have matched, its padding included: 1 at its start bit.
    pub fn alert_matched(&self) -> u8 {
        self.alert.matched
    }

    /// Whether a Selection Sequence turned drive conflict protection on.
    pub fn protected(&self) -> bool {
        self.protected
    }

    /// Whether the target samples the next bit at TCKC's rise; otherwise at its fall.
    pub fn sampled_at_rise(&self) -> bool {
        self.sampling() == Some(true)
    }

    /// Whether the target samples at TCKC's rise, or None where SREDGE is not known.
    pub fn sampling(&self) -> Option<bool> {
        match self.protocol {
            Protocol::Standard => Some(true),
            _ => self.sredge,
        }
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
            (None, _) if self.zbs > 0 && scanning && !self.suspend => Some(0),
            // A command runs from its first DR scan's start to its second's end.
            (Some(2), 0) if scanning => Some(0),
            (Some(2), 1) => Some(usize::from(scanning)),
            // A CR scan from its own start.
            (Some(2), 2) if scanning => Some(0),
            _ => None,
        }
    }

    /// A recording gap: nothing about the controller's state is known after it; what CNFG0 says
    /// of it stays.
    pub fn gap(&mut self) {
        *self = Self {
            offline_start: self.offline_start,
            dsel_alerts: self.dsel_alerts,
            ..Self::default()
        };
    }

    /// A reset escape (Type-3): Online in the Standard Protocol, or Offline at start-up with the
    /// TAPC moved to Run-Test/Idle (11.9.7.2 b); the registers as table 9-2 gives them.
    fn reset(&mut self, out: &mut Vec<Ev>) {
        let start_up = self.offline_start;
        *self = Self {
            protocol: if start_up {
                Protocol::StartUp
            } else {
                Protocol::Standard
            },
            tap: 1 << if start_up { RTI } else { TLR },
            offline_start: start_up,
            dsel_alerts: self.dsel_alerts,
            ..Self::default()
        };
        self.registers_reset();
        if start_up {
            out.push(Ev::Offline(Offline::StartUp));
        }
    }

    /// The registers a Type-0 to Type-4 reset sets (table 9-2): CGM and SREDGE 0, controller ID
    /// zero, SUSPEND and ZBSINH 0; SGC its default, which only an Offline-at-Start-up option gives
    /// (table 10-7).
    fn registers_reset(&mut self) {
        self.sredge = Some(false);
        self.cgm = Some(false);
        self.sgc = self.offline_start.then_some(false);
        self.cid = Cid::Reset;
        self.suspend = false;
        self.inhibited = false;
        self.stored = None;
        // The SSD state starts at Select All (rule 20.10.2 o), SSDE at 0.
        self.ssde = Some(false);
        self.ssd = Some(Ssd::SelectAll);
        self.ssd_run = None;
        // The default scan format is JScan0 or JScan1 (table 10-7).
        self.jscan3 = false;
    }

    fn exit_level(&mut self, out: &mut Vec<Ev>) {
        if self.level.take().is_some() {
            out.push(Ev::LevelExit);
        }
        self.zbs = 0;
        self.part = 0;
        self.cr = None;
    }

    /// Framing is lost: nothing about the protocol or the TAPC is known until an escape, and the
    /// registers commands store may change unseen.
    fn lose(&mut self) {
        self.protocol = Protocol::Unknown;
        self.tap = ALL;
        self.sredge = None;
        self.cgm = None;
        self.sgc = None;
        self.cid = Cid::Unknown;
        self.ssde = None;
        self.ssd = None;
        self.ssd_late = None;
        self.ssd_run = None;
    }

    /// One TCKC high phase: TMSC's level before TCKC fell, and how many TMSC edges it held.
    pub fn phase(&mut self, value: bool, edges: usize, out: &mut Vec<Ev>) {
        let custom = edges >= 2;
        // Offline at start-up, a selection escape counts only after its preamble (11.9.7.2 c).
        let preamble = self.protocol == Protocol::StartUp && self.preamble();
        if self.protocol == Protocol::StartUp {
            self.history = self.history << 1 | u32::from(value);
            self.since_escape = if custom {
                0
            } else {
                self.since_escape.saturating_add(1)
            };
        }
        match Escape::of(edges) {
            Some(Escape::Reset) => {
                out.push(Ev::Escape {
                    kind: Escape::Reset,
                    response: None,
                    tms: None,
                });
                self.reset(out);
                return;
            }
            Some(kind @ (Escape::Selection | Escape::Deselection)) => {
                if self.escape(kind, value, preamble, out) {
                    return;
                }
            }
            Some(Escape::Custom) | None => {}
        }
        match self.protocol {
            Protocol::Unknown | Protocol::Offline | Protocol::StartUp => {
                self.alert_bit(value, custom, out);
            }
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
                        self.reset(out);
                    } else {
                        // A JScan format brings the Standard Protocol (table 11-8).
                        self.jscan3 = then == 3;
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
    fn escape(&mut self, kind: Escape, value: bool, preamble: bool, out: &mut Vec<Ev>) -> bool {
        // SUSPEND keeps the System Path in use whatever the zero-bit scan count (11.6.1.1).
        let system_path = self.level.is_none() && (self.zbs == 0 || self.suspend);
        let start_up = self.protocol == Protocol::StartUp;
        let response = match self.protocol {
            Protocol::Standard | Protocol::Advanced(_) if !system_path => Response::Unqualified,
            Protocol::Advanced(_) if self.slot != 0 => Response::Unqualified,
            // Only with the Delayed SSD State Select All and no SSD under way (11.6.2 b).
            Protocol::Standard | Protocol::Advanced(_) if self.ssd_run.is_some() => {
                Response::Unqualified
            }
            Protocol::Standard | Protocol::Advanced(_) => match self.ssd_late {
                Some(Ssd::SelectAll) => Response::Taken,
                Some(_) => Response::Unqualified,
                None => Response::Unknown,
            },
            Protocol::Offline | Protocol::StartUp if kind == Escape::Deselection => {
                Response::Offline
            }
            Protocol::StartUp if !preamble => Response::Unqualified,
            // A Check Packet or a Selection Sequence answers only a selection escape.
            Protocol::Check { .. } | Protocol::Selection { .. } if kind == Escape::Deselection => {
                Response::Unqualified
            }
            _ => Response::Taken,
        };
        let standard = self.protocol == Protocol::Standard;
        // Only the Standard Protocol and start-up show an escape they ignore: elsewhere the bit is a
        // Scan Packet's, Check Packet's or Selection Sequence's like any other. One whose response is
        // unknown is shown wherever it is.
        if response == Response::Unqualified && !standard && !start_up {
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
        if response == Response::Unknown {
            out.push(Ev::Unsupported(SSD_UNKNOWN));
            self.lose();
        }
        if response != Response::Taken {
            return true;
        }
        self.slot = 0;
        self.eot = false;
        self.alert = Alert::default();
        self.protocol = match kind {
            Escape::Deselection => {
                out.push(Ev::Offline(Offline::Deselection));
                Protocol::Offline
            }
            _ => Protocol::Selection { bits: 0, count: 0 },
        };
        true
    }

    /// Offline at start-up: whether the 28 bits before the one just past alternate, with no
    /// escape among the last 28 (11.9.7.2 c).
    fn preamble(&self) -> bool {
        let run = self.history >> 1 & ((1 << 28) - 1);
        self.since_escape >= 28 && (run ^ run >> 1) & ((1 << 27) - 1) == (1 << 27) - 1
    }

    /// One bit while the controller is not known to be Online: a Selection Alert is at least eight
    /// 1s, its 128 bits from the 0 start bit, then four padding bits; a Selection Sequence follows
    /// (10.5).
    fn alert_bit(&mut self, value: bool, escape: bool, out: &mut Vec<Ev>) {
        let a = &mut self.alert;
        if escape {
            *a = Alert::default();
        } else if a.matched > 0 {
            let k = u32::from(a.matched);
            if k >= 128 || value == (SELECTION_ALERT >> k & 1 == 1) {
                a.matched += 1;
            } else {
                // The alert holds no eight 1s in a row, so a mismatch is never a new start.
                *a = Alert {
                    ones: u8::from(value),
                    matched: 0,
                };
            }
        } else if value {
            a.ones = a.ones.saturating_add(1);
        } else if a.ones >= 8 {
            a.matched = 1;
        } else {
            a.ones = 0;
        }
        if self.alert.matched == ALERT_BITS {
            self.alert = Alert::default();
            out.push(Ev::Alert { selection: true });
            self.protocol = Protocol::Selection { bits: 0, count: 0 };
        }
    }

    /// A Standard Protocol TMS bit.
    fn standard(&mut self, value: bool, out: &mut Vec<Ev>) {
        self.ssd_late = self.ssd;
        let state = self.tap();
        // In JScan3 an SSD is TDI data, off TMSC: one may start wherever it could.
        if self.jscan3
            && self.ssde != Some(false)
            && !value
            && state.is_some_and(|s| matches!(s, RTI | PAUSE_IR | PAUSE_DR))
            && self.ssd_path()
        {
            self.ssd = None;
        }
        let leaving_update = state == Some(UPDATE_DR);
        self.advance(value, None, out);
        // A non-JScan format stored in the Standard Protocol: a Check Packet follows the TMS bit
        // of the command's Update-DR (21.7.1; 4.3.2.4). A JScan one takes effect there.
        if leaving_update && let Some(f) = self.stored.take() {
            if f < 4 {
                self.jscan3 = f == 3;
                return;
            }
            self.protocol = Protocol::Check {
                then: f,
                preamble: false,
                previous: None,
                post: false,
                reset: false,
            };
        }
    }

    /// Whether the System Path is in use for SSDs: no control level, and a zero-bit count of at
    /// most one unless ZBSINH or SUSPEND (figure 20-9).
    fn ssd_path(&self) -> bool {
        self.level.is_none() && (self.inhibited || self.suspend || self.zbs <= 1)
    }

    /// One TCK's TDI and TMS for Scan Selection Directives, the TAPC in `state`, in a format that
    /// carries them (table 23-10): start, continue, execute or abort one (20.10.2 e to l).
    fn ssd_bit(&mut self, state: u8, tdi: bool, tms: bool, out: &mut Vec<Ev>) {
        if let Some(mut run) = self.ssd_run.take() {
            // TMS 1 in any of its bits, or in the one after them, aborts it.
            if tms {
                out.push(Ev::Ssd {
                    kind: run.kind(),
                    payload: None,
                    executed: false,
                    state: self.ssd,
                });
                return;
            }
            if run.count < run.size() {
                run.bits |= u64::from(tdi) << run.count;
                run.count += 1;
                self.ssd_run = Some(run);
                return;
            }
            self.ssd_execute(run, out);
        }
        let supporting = matches!(state, RTI | PAUSE_IR | PAUSE_DR);
        if !supporting || tms || tdi || !self.ssd_path() {
            return;
        }
        // Processing is permitted by the Delayed SSD State (table 20-16).
        let permitted = match (self.ssd_late, state) {
            (Some(Ssd::SelectAll), _) => Some(true),
            (Some(Ssd::DeselectedIr | Ssd::NotSelectedIr | Ssd::SelectedIr), PAUSE_IR) => {
                Some(true)
            }
            (Some(Ssd::DeselectedDr | Ssd::NotSelectedDr | Ssd::SelectedDr), PAUSE_DR) => {
                Some(true)
            }
            (Some(_), _) => Some(false),
            (None, _) => None,
        };
        match (self.ssde, permitted) {
            (Some(true), Some(true)) => {
                self.ssd_run = Some(SsdRun {
                    state,
                    bits: 0,
                    count: 1,
                });
                out.push(Ev::SsdStart);
            }
            (Some(false), _) | (_, Some(false)) => {}
            // One may start here, unseen.
            _ => self.ssd = None,
        }
    }

    /// An SSD whose bits and the TCK after them held TMS 0: its SSD state and SGC (tables 20-15,
    /// 20-17), and an unlocked zero-bit count zeroed (20.10.1.7).
    fn ssd_execute(&mut self, run: SsdRun, out: &mut Vec<Ev>) {
        let Some(kind) = run.kind() else { return };
        let payload = run.bits >> 3;
        // A CID is known where the controller's is; a TCA, its own 35-bit address, never.
        let matched = match (kind, self.cid) {
            (SsdKind::SelectCid, Cid::Reset) => Some(payload == 0),
            (SsdKind::SelectCid, Cid::Valid(c)) => Some(payload == u64::from(c)),
            (SsdKind::SelectCid, Cid::Invalid) => Some(false),
            _ => None,
        };
        let ir = run.state == PAUSE_IR;
        let state = match kind {
            _ if run.state == RTI => Some(Ssd::SelectAll),
            SsdKind::SelectAll => Some(Ssd::SelectAll),
            SsdKind::DeselectAll if ir => Some(Ssd::DeselectedIr),
            SsdKind::DeselectAll => Some(Ssd::DeselectedDr),
            _ => matched.map(|m| match (m, ir) {
                (true, true) => Ssd::SelectedIr,
                (true, false) => Ssd::SelectedDr,
                (false, true) => Ssd::NotSelectedIr,
                (false, false) => Ssd::NotSelectedDr,
            }),
        };
        if run.state == RTI {
            self.sgc = match (kind, matched) {
                (SsdKind::DeselectAll, _) => Some(false),
                (SsdKind::SelectAll, _) | (_, Some(true)) => Some(true),
                (_, Some(false)) => self.sgc,
                // Set if addressed, else left: known only where it is already set.
                (_, None) => self.sgc.filter(|&s| s),
            };
        }
        if self.level.is_none() {
            self.zbs = 0;
        }
        self.ssd = state;
        let payload = matches!(kind, SsdKind::SelectCid | SsdKind::SelectTca).then_some(payload);
        out.push(Ev::Ssd {
            kind: Some(kind),
            payload,
            executed: true,
            state,
        });
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
            sgc: self.sgc,
            cid: self.cid,
            offline_start: self.offline_start,
            dsel_alerts: self.dsel_alerts,
            ssde: self.ssde,
            ssd: self.ssd,
            ssd_late: self.ssd_late,
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
        self.ssd_late = self.ssd;
        if let (Some(state), Some(tdi)) = (self.tap(), tdi)
            && matches!(format, 9 | 13)
        {
            self.ssd_bit(state, tdi, tms, out);
        }
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
        if std::mem::take(&mut self.parking) {
            // The Deselection Alert parks the TAPC where this bit takes it (10.6.1).
            out.push(Ev::Alert { selection: false });
            self.exit_level(out);
            self.protocol = Protocol::Offline;
            out.push(Ev::Offline(Offline::Alert));
            return false;
        }
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
        }
        if after == Some(TLR) && before != Some(TLR) {
            // A Type-4 reset: the registers as table 9-2 gives them, and the default scan format,
            // whose Standard Protocol the Advanced Protocol gives way to (table 11-8).
            self.registers_reset();
            self.spcp = false;
            if let Protocol::Advanced(_) = self.protocol {
                self.protocol = Protocol::Standard;
                out.push(Ev::DefaultFormat);
            }
        }
        if self.level == Some(6) && after == Some(RTI) && before != Some(RTI) {
            self.parking = self.dsel_alerts != Some(false);
        }
        writes
    }

    fn dr_done(&mut self, shifts: u32, out: &mut Vec<Ev>) -> bool {
        if self.inhibited {
            return false;
        }
        match self.level {
            // The eighth zero-bit scan in a row ends SUSPEND (17.5.2 d).
            None if shifts == 0 && self.suspend && self.zbs == 7 => {
                self.suspend = false;
                self.zbs = 0;
            }
            None if shifts == 0 => self.zbs = (self.zbs + 1).min(7),
            None if self.zbs > 0 && !self.suspend => {
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
                    // SCNS and a private SCNB reach their path only where CGM is 1 (table 9-10).
                    let path = match (c.opcode, self.cgm) {
                        (8, _) if c.operand < 16 => Path::Register,
                        (8 | 9, Some(true)) | (10, _) => Path::Register,
                        (8 | 9, Some(false)) => Path::Bypass,
                        _ => Path::Unknown,
                    };
                    self.cr = three.then(|| Cr {
                        opcode: c.opcode,
                        operand: c.operand,
                        path,
                        shifts: 0,
                        on_wire: true,
                        tdi: vec![],
                        tdo: vec![],
                    });
                    let writes = c.writes;
                    // ECL and SUSPEND end the level after the command inside it (17.5.1.2).
                    let ends = c.opcode == 0 && matches!(c.operand, 1 | 2);
                    out.push(Ev::Command(c));
                    if ends {
                        self.exit_level(out);
                    }
                    return writes;
                }
                _ => {
                    self.part = 0;
                    if let Some(cr) = self.cr.take() {
                        self.cr_stores(&cr);
                        out.push(Ev::CrScan(cr));
                    }
                }
            },
            Some(_) => {}
        }
        false
    }

    /// What a CR scan stores: SCNB's public SGC and CGM bits take the last TDI bit (table 9-4); a
    /// CNFG0 read says whether the controller starts Offline and takes Deselection Alerts
    /// (table 9-15).
    fn cr_stores(&mut self, cr: &Cr) {
        if cr.path != Path::Register || cr.shifts == 0 {
            return;
        }
        // The last bit shifted, where it was kept and on TMSC.
        let whole = cr.on_wire && cr.tdi.len() == cr.shifts as usize;
        let last = cr.tdi.last().copied().filter(|_| whole);
        match (cr.opcode, cr.operand) {
            (8, 0) => self.sgc = last,
            (8, 1) => self.cgm = last,
            (9, 2) if cr.on_wire && cr.tdo.len() >= 32 => {
                self.offline_start = cr.tdo[19];
                self.dsel_alerts = Some(cr.tdo[31]);
            }
            _ => {}
        }
    }

    /// CGM or SGC after MCM or MSC with `m` and controller ID `cid`, `now` before (table 9-4): an
    /// invalid ID clears it, a matching one sets it, another clears it with `m` 0 and leaves it
    /// with `m` 1. After a reset, until a CIDD, only ID zero acts (9.4.1.7).
    fn member(&self, m: bool, cid: u8, now: Option<bool>) -> Option<bool> {
        match self.cid {
            Cid::Invalid => Some(false),
            Cid::Reset if cid == 0 => Some(true),
            Cid::Reset => now,
            Cid::Valid(c) if c == cid => Some(true),
            Cid::Valid(_) if m => now,
            Cid::Valid(_) => Some(false),
            // Set if addressed, else cleared or left: known only where both agree.
            Cid::Unknown if m && now == Some(true) => now,
            Cid::Unknown => None,
        }
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
                (0, 1, 0) => {
                    self.suspend = true;
                    "suspend the control levels (SUSPEND)".into()
                }
                (0, 1, 1) => {
                    self.inhibited = true;
                    "inhibit zero-bit scans (ZBSINH)".into()
                }
                (1, 0, v) => {
                    self.sgc = Some(v == 1);
                    format!("Scan Group Candidate SGC = {v}")
                }
                (1, 1, v) => {
                    self.cgm = Some(v == 1);
                    format!("Conditional Group Member CGM = {v}")
                }
                (4, 0, v) => {
                    self.ssde = Some(v == 1);
                    format!("Scan Selection Directives enabled SSDE = {v}")
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
                if bit == 0 {
                    // A conditional store acts only where CGM is 1.
                    self.sredge = match (c, self.cgm) {
                        (0, _) | (_, Some(true)) => Some(v == 1),
                        (_, Some(false)) => self.sredge,
                        (_, None) if self.sredge == Some(v == 1) => self.sredge,
                        (_, None) => None,
                    };
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
                self.cgm = self.member(operand & 16 != 0, operand & 15, self.cgm);
                format!("Conditional Group Member for controller {}", operand & 15)
            }
            7 => {
                self.sgc = self.member(operand & 16 != 0, operand & 15, self.sgc);
                format!("Scan Group Candidate: controller {}", operand & 15)
            }
            8..=10 => {
                // CIDA gives an ID to a controller without one, but which wins is not on TMSC.
                if opcode == 10 && matches!(self.cid, Cid::Invalid | Cid::Unknown) {
                    self.cid = Cid::Unknown;
                }
                let m = MNEMONICS.get(usize::from(opcode)).copied().unwrap_or("?");
                format!("{m} with a CR scan")
            }
            11 if operand & 16 != 0 => {
                self.cid = Cid::Invalid;
                "de-allocate every controller ID".into()
            }
            11 => {
                let cid = operand & 15;
                self.cid = match self.cid {
                    Cid::Reset | Cid::Valid(0) if cid == 0 => Cid::Invalid,
                    Cid::Reset => Cid::Valid(0),
                    Cid::Valid(c) if c == cid => Cid::Invalid,
                    other => other,
                };
                format!("de-allocate controller ID {cid}")
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
