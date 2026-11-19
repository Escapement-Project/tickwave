//! ADIv5 register accesses stacked on SWD's or JTAG-DP's transfers, and MEM-AP memory accesses
//! stacked on those: enough of each to show stacking by pieces and by completed items, context
//! carried across transfers, and values unknown for a stated reason.
//!
//! ADIv5 reads its transfers as pieces, typed fields only. A SELECT write sets the DP bank and
//! the AP and its register bank. Over SWD an AP read's data arrives in the next AP read or RDBUFF
//! read, so an AP read stays open until then; over JTAG-DP each read carries its own, which the
//! setting `reads` says ("posted" or "own").
//!
//! Each AP's class comes from its IDR reads (ADIv5.2 C1.3.2), and an AP access carries the class
//! known when it was made; an IDR read carries the class its own value gives. A register's name
//! follows the class: IDR for any AP, the only register every AP has (C1.3.1); a MEM-AP's and a
//! JTAG-AP's registers once an IDR read shows the AP is one. The setting `idr` gives APs' IDRs
//! from the start, for a recording that begins after the debugger read them; a later IDR read
//! takes over. MEM-AP skips an AP known to be of another class and takes one not yet identified
//! for a MEM-AP.

//!
//! MEM-AP reads ADIv5's AP accesses: TAR and CSW, then DRW and banked data accesses. A DRW access
//! is at the address TAR holds, auto-incremented as CSW says, singly or packed (C2.2.2, C2.2.7).
//! A banked data access is in TAR's 16-byte block and leaves TAR as it is (C2.6.2). A memory
//! access's value is the bytes it moved, lowest address first, each from the byte lane its address
//! gives (table C2-3). A wide access, of 64 to 256 bits, is one memory access over its several DRW
//! or banked data accesses, least significant word first (table C2-20). After the last, TAR has
//! moved by its size. A read whose result never came back still made its access and moved TAR. A
//! CSW write is taken as the MEM-AP took it, until a CSW read shows otherwise.

use crate::swd;
use escapement_tickwave_decode::{
    Cause, Completed, Decoder, Definition, Delivery, Exchange, Factory, FieldId, FieldType, Id,
    KindDecl, KindId, Label, LinkDecl, LinkId, LinkTo, Mode, Need, Op, Out, Pause, Reason, Ref,
    Schema, Setting, SettingDecl, SettingId, Span, Status, Tick, UpstreamDecl, Value, field, name,
};
use foldhash::HashMap;
use std::sync::Arc;

// ---- ADIv5 -------------------------------------------------------------------------------------

pub const CONTEXT: KindId = KindId(0);
pub const DP_READ: KindId = KindId(1);
pub const DP_WRITE: KindId = KindId(2);
pub const AP_WRITE: KindId = KindId(3);
pub const AP_READ: KindId = KindId(4);

/// `CONTEXT`'s field and link.
pub const SELECT: FieldId = FieldId(0);
pub const RESOLVED_BY: LinkId = LinkId(0);
/// A DP access's fields and links.
pub const DP_REG: FieldId = FieldId(0);
pub const DP_VALUE: FieldId = FieldId(1);
pub const DP_TRANSFER: LinkId = LinkId(0);
pub const DP_CONTEXT: LinkId = LinkId(1);
/// An AP access's fields.
pub const AP: FieldId = FieldId(0);
pub const AP_REG: FieldId = FieldId(1);
pub const AP_NAME: FieldId = FieldId(2);
pub const AP_A: FieldId = FieldId(3);
pub const AP_VALUE: FieldId = FieldId(4);
pub const AP_CLASS: FieldId = FieldId(5);
/// An AP write's links.
pub const W_TRANSFER: LinkId = LinkId(0);
pub const W_CONTEXT: LinkId = LinkId(1);
/// An AP read's links: the transfer that requested it, the one that returned its data.
pub const R_REQUEST: LinkId = LinkId(0);
pub const R_DATA: LinkId = LinkId(1);
pub const R_CONTEXT: LinkId = LinkId(2);

pub const DP_NAMES: [&str; 12] = [
    "DPIDR",
    "ABORT",
    "RESEND",
    "SELECT",
    "RDBUFF",
    "TARGETSEL",
    "CTRL/STAT",
    "DLCR",
    "TARGETID",
    "DLPIDR",
    "EVENTSTAT",
    "reserved",
];
pub const AP_NAMES: [&str; 17] = [
    "CSW",
    "TAR",
    "TAR[63:32]",
    "DRW",
    "BD0",
    "BD1",
    "BD2",
    "BD3",
    "CFG",
    "BASE",
    "IDR",
    "PSEL",
    "PSTA",
    "BRFIFO1",
    "BRFIFO2",
    "BRFIFO3",
    "BRFIFO4",
];

/// An AP's class, as its IDR gives it: by its index in `CLASS_NAMES`. `ABSENT` is an IDR of zero,
/// no AP present; `OTHER` a class this decoder names no registers of.
pub const CLASS_NAMES: [&str; 5] = ["MEM-AP", "JTAG-AP", "COM-AP", "other", "absent"];
pub const MEM_AP: u16 = 0;
pub const JTAG_AP: u16 = 1;
pub const COM_AP: u16 = 2;
pub const OTHER: u16 = 3;
pub const ABSENT: u16 = 4;
/// Arm's JEP106 code as IDR.DESIGNER holds it: continuation 4, identity 0x3B.
const ARM: u32 = 0x23B;

pub const BANK_UNKNOWN: Reason = Reason(0);
pub const NO_SELECT: Reason = Reason(1);
pub const PARITY_FAILED: Reason = Reason(2);
pub const LOST_SELECT: Reason = Reason(3);
pub const LOST_BANK: Reason = Reason(4);
pub const NO_IDR: Reason = Reason(5);
pub const UNIDENTIFIABLE: Reason = Reason(6);
pub const NEEDS_SELECT: Need = Need(0);
pub const NEEDS_IDR: Need = Need(1);

pub const NEVER_READ: Cause = Cause(0);
pub const ENDS_UNREAD: Cause = Cause(1);
pub const GAP_CUT: Cause = Cause(2);

const GAP_CUT_TEXT: &str = "a recording gap interrupts it";

pub fn adi_schema() -> Schema {
    let w = |bits| FieldType::Word { bits };
    let transfer = || LinkTo::Upstream(name("transfer"));
    let dp_write = || LinkTo::Own(name("dp-write"));
    let dp = |kind: &'static str| {
        KindDecl::new(kind)
            .with_fields(vec![
                field("reg", FieldType::Name(DP_NAMES.map(name).to_vec())),
                field("value", w(32)),
            ])
            .with_links(vec![
                LinkDecl::new("transfer", vec![transfer()]),
                LinkDecl::new("context", vec![dp_write()]),
            ])
            .with_labels(vec![
                Label::default().field(DP_REG).text(" ").field(DP_VALUE),
                Label::default().field(DP_REG),
            ])
    };
    let ap = |kind: &'static str, links| {
        KindDecl::new(kind)
            .with_fields(vec![
                field("ap", w(8)),
                field("reg", w(8)),
                field("name", FieldType::Name(AP_NAMES.map(name).to_vec())),
                field("a", w(4)),
                field("value", w(32)),
                field("class", FieldType::Name(CLASS_NAMES.map(name).to_vec())),
            ])
            .with_links(links)
            .with_labels(vec![
                (Label::default().text("AP ").field(AP_NAME))
                    .text(" ")
                    .field(AP_VALUE),
                Label::default().field(AP_NAME),
            ])
    };
    Schema::new(vec![
        KindDecl::new("context")
            .with_fields(vec![field("select", w(32))])
            .with_links(vec![LinkDecl::new("resolved by", vec![dp_write()])]),
        dp("dp-read"),
        dp("dp-write"),
        ap(
            "ap-write",
            vec![
                LinkDecl::new("transfer", vec![transfer()]),
                LinkDecl::new("context", vec![dp_write()]),
            ],
        ),
        // An AP read stays open until its data comes back, in the next read: two may overlap.
        ap(
            "ap-read",
            vec![
                LinkDecl::new("request", vec![transfer()]),
                LinkDecl::new("data", vec![transfer()]),
                LinkDecl::new("context", vec![dp_write()]),
            ],
        )
        .with_lanes(2),
    ])
    .with_reasons([
        "DP bank unknown: no SELECT write decoded before this access",
        "no SELECT write decoded before this access",
        "data parity failed",
        "the SELECT value is lost in a recording gap",
        "DP bank unknown: the SELECT value is lost in a recording gap",
        "no IDR read of this AP decoded before this access",
        "the AP's IDR sets reserved bits, so the AP is unidentifiable",
    ])
    .with_needs(["SELECT", "IDR"])
    .with_causes([
        "result never read back",
        "the recording ends before the result is read back",
        GAP_CUT_TEXT,
    ])
    .with_open_at_most(3)
}

/// Whether a read's data arrives in a later transfer, as SWD posts AP reads, or in its own, as
/// JTAG-DP's transfers carry it: the names of the setting `reads`.
pub const READS: SettingId = SettingId(0);
pub const READS_NAMES: [&str; 2] = ["posted", "own"];
/// Each AP's IDR from the start, AP 0 first, `IDR_NOT_GIVEN` for an AP left out. Empty, the
/// default, gives none.
pub const IDR: SettingId = SettingId(1);
pub const IDR_NOT_GIVEN: i64 = -1;

pub fn adi_definition() -> Arc<Definition> {
    Arc::new(
        Definition::new("adiv5", 1, adi_schema())
            .with_inputs([UpstreamDecl::new("transfers", Mode::Pieces)
                .from(["swd", "jtag-dp"])
                .with_kinds(["transfer"])])
            .with_settings(vec![
                SettingDecl::choice("reads", READS_NAMES, 0),
                SettingDecl::list("idr", IDR_NOT_GIVEN..=0xFFFF_FFFF, 256, vec![]),
            ]),
    )
}

/// ADIv5's reads setting, as a binding gives it: each read carrying its own data, or posted.
pub fn reads(own: bool) -> (&'static str, Setting) {
    ("reads", Setting::Name(name(READS_NAMES[usize::from(own)])))
}

/// ADIv5's IDR setting, as a binding gives it: each AP's IDR from the start, AP 0 first.
pub fn idr(by_ap: &[i64]) -> (&'static str, Setting) {
    ("idr", Setting::List(by_ap.to_vec()))
}

fn unknown(reason: Reason) -> Value {
    Value::Unknown {
        reason,
        needs: Some(NEEDS_SELECT),
    }
}

fn word(value: u64, bits: u8) -> Value {
    Value::Word { value, bits }
}

/// A DP register's name, or why it is unknown.
pub fn dp_name(read: bool, addr: u8, bank: Option<u32>) -> Value {
    let n = match (read, addr) {
        (true, 0x0) => 0,
        (false, 0x0) => 1,
        (true, 0x8) => 2,
        (false, 0x8) => 3,
        (true, 0xC) => 4,
        (false, 0xC) => 5,
        (_, 0x4) => match bank {
            None => return unknown(BANK_UNKNOWN),
            Some(0) => 6,
            Some(1) => 7,
            Some(2) if read => 8,
            Some(3) if read => 9,
            Some(4) if read => 10,
            Some(_) => 11,
        },
        _ => 11,
    };
    Value::Name(n)
}

/// An AP's class from its IDR (C1.3.2, table C1-3), by its index in `CLASS_NAMES`, or why it has
/// none.
pub fn ap_class(idr: u32) -> Result<u16, Reason> {
    let designer = idr >> 17 & 0x7FF;
    let (variant, ty) = (idr >> 4 & 0xF, idr & 0xF);
    // A designer of zero is an early Arm DAP, whose CLASS reads as zero whatever the AP is.
    let arm = designer == ARM || designer == 0;
    Ok(match idr >> 13 & 0xF {
        _ if idr == 0 => ABSENT,
        _ if idr >> 8 & 0x1F != 0 => return Err(UNIDENTIFIABLE),
        0b1000 => MEM_AP,
        0b0001 => COM_AP,
        0b0000 if arm && ty == 0 && variant != 0 => JTAG_AP,
        0b0000 if designer == 0 && matches!(ty, 0x1 | 0x2 | 0x4..=0x8) => MEM_AP,
        _ => OTHER,
    })
}

/// An AP register's name, by its index in `AP_NAMES`, for an AP of `class` (None where it is not
/// known): IDR for any AP, the others by the class's register map (C2.6, table C3-10).
pub fn ap_name(class: Option<u16>, reg: u32) -> Option<u16> {
    Some(match (class, reg) {
        (_, 0xFC) => 10,
        (Some(MEM_AP), _) => match reg {
            0x00 => 0,
            0x04 => 1,
            0x08 => 2,
            0x0C => 3,
            0x10 => 4,
            0x14 => 5,
            0x18 => 6,
            0x1C => 7,
            0xF4 => 8,
            0xF8 => 9,
            _ => return None,
        },
        (Some(JTAG_AP), _) => match reg {
            0x00 => 0,
            0x04 => 11,
            0x08 => 12,
            0x10 => 13,
            0x14 => 14,
            0x18 => 15,
            0x1C => 16,
            _ => return None,
        },
        _ => return None,
    })
}

/// A closed SWD transfer as ADIv5 reads it.
#[derive(Clone, Debug)]
pub struct Closed {
    pub id: Id,
    pub start: Tick,
    pub end: Tick,
    pub ap: bool,
    pub read: bool,
    pub addr: u8,
    pub ack: u8,
    pub data: Option<(Value, Span)>,
}

/// An AP read waiting for its data.
#[derive(Clone, Copy, Debug)]
struct Pending {
    id: Id,
    /// The AP an IDR read identifies, whose class the read takes from its own value.
    idr: Option<u8>,
}

/// What ADIv5 needs of an SWD transfer, gathered from its pieces.
#[derive(Clone, Debug, Default)]
struct Partial {
    start: Tick,
    ap: bool,
    read: bool,
    addr: u8,
    ack: u8,
    data: Option<(u64, Span)>,
    parity: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct Adi {
    select: Option<(u32, Id)>,
    /// Why `select` is None: nothing decoded yet, or a recording gap.
    unknown: Reason,
    pending: Option<Pending>,
    context: Option<Id>,
    local: Option<Tick>,
    started: bool,
    building: HashMap<Id, Partial>,
    up: Option<Tick>,
    /// Each read carries its own data, as JTAG-DP's do.
    own_reads: bool,
    /// Each AP's class from its last IDR read with a known value, else from the `idr` setting, or
    /// why it has none. An AP's IDR is read-only and fixed, so a recording gap forgets SELECT but
    /// not these.
    classes: HashMap<u8, Result<u16, Reason>>,
}

impl Adi {
    pub fn new(local: Option<Tick>) -> Self {
        Self {
            select: None,
            unknown: NO_SELECT,
            pending: None,
            context: None,
            local,
            started: false,
            building: HashMap::default(),
            up: None,
            own_reads: false,
            classes: HashMap::default(),
        }
    }

    pub fn factory() -> Factory {
        Box::new(|b, local| {
            let mut adi = Self::new(local);
            adi.own_reads = b.choice(READS) == Some(1);
            // Bound, each entry is a 32-bit IDR or `IDR_NOT_GIVEN`, and there are at most 256.
            for (ap, &v) in b.list(IDR).unwrap_or(&[]).iter().enumerate() {
                if v != IDR_NOT_GIVEN {
                    adi.classes.insert(ap as u8, ap_class(v as u32));
                }
            }
            Box::new(adi)
        })
    }

    /// A decode that starts without earlier context opens a context item where it starts.
    fn start(&mut self, out: &mut Out) {
        if std::mem::replace(&mut self.started, true) {
            return;
        }
        if let Some(at) = self.local {
            let id = Id::new(CONTEXT, at);
            out.open(id, None, at);
            out.set(id, SELECT, unknown(NO_SELECT));
            self.context = Some(id);
        }
    }

    /// Input lost from `from` to `to`: a read awaiting its result is cut short, and SELECT is
    /// unknown from `to` until a SELECT write, in a context item there.
    fn gap(&mut self, from: Tick, to: Tick, out: &mut Out) {
        if let Some(p) = self.pending.take() {
            self.unread(p, from, GAP_CUT, out);
        }
        if let Some(ctx) = self.context.take() {
            out.close(ctx, from, Status::Incomplete(GAP_CUT));
        }
        self.select = None;
        self.unknown = LOST_SELECT;
        let id = Id::new(CONTEXT, to);
        out.open(id, None, to);
        out.set(id, SELECT, unknown(LOST_SELECT));
        self.context = Some(id);
    }

    /// AP `ap`'s class as an access to it carries it.
    fn class(&self, ap: u8) -> Value {
        match self.classes.get(&ap) {
            Some(&Ok(n)) => Value::Name(n),
            Some(&Err(reason)) => Value::Unknown {
                reason,
                needs: None,
            },
            None => Value::Unknown {
                reason: NO_IDR,
                needs: Some(NEEDS_IDR),
            },
        }
    }

    /// A read closed before its data came back: an IDR read takes the class known before it.
    fn unread(&self, p: Pending, at: Tick, cause: Cause, out: &mut Out) {
        if let Some(ap) = p.idr {
            out.set(p.id, AP_CLASS, self.class(ap));
        }
        out.close(p.id, at, Status::Incomplete(cause));
    }

    fn context_link(&self, id: Id, link: LinkId, out: &mut Out) {
        if let Some((_, s)) = &self.select {
            out.link(id, link, Ref::Own(*s));
        }
    }

    fn value(out: &mut Out, id: Id, field: FieldId, c: &Closed) -> Option<(u32, Span)> {
        let (v, span) = c.data.clone()?;
        let known = match v {
            Value::Word { value, .. } => Some((value as u32, span)),
            _ => None,
        };
        out.part(id, field, v, span, None);
        known
    }

    fn consume(&mut self, c: &Closed, out: &mut Out) {
        if c.ack != 1 {
            return;
        }
        let bank = self.select.as_ref().map(|(s, _)| s & 0xF);
        if !c.ap
            && c.read
            && c.addr == 0xC
            && let Some(p) = self.pending.take()
        {
            self.complete(p, c, out);
            return;
        }
        if !c.ap {
            let kind = if c.read { DP_READ } else { DP_WRITE };
            let id = Id::new(kind, c.id.anchor);
            out.open(id, None, c.start);
            let reg = match dp_name(c.read, c.addr, bank) {
                Value::Unknown { .. } if self.unknown == LOST_SELECT => unknown(LOST_BANK),
                known => known,
            };
            out.set(id, DP_REG, reg);
            let v = Self::value(out, id, DP_VALUE, c);
            out.link(id, DP_TRANSFER, Ref::Upstream(c.id));
            if c.addr == 0x4 {
                self.context_link(id, DP_CONTEXT, out);
            }
            out.close(id, c.end, Status::Complete);
            if !c.read
                && c.addr == 0x8
                && let Some((s, _)) = v
            {
                self.select = Some((s, id));
                self.unknown = NO_SELECT;
                if let Some(ctx) = self.context.take() {
                    out.link(ctx, RESOLVED_BY, Ref::Own(id));
                    out.close(ctx, c.start, Status::Complete);
                }
            }
            return;
        }
        let ap = self.select.as_ref().map(|(s, _)| (s >> 24) as u8);
        let reg = self
            .select
            .as_ref()
            .map(|(s, _)| ((s >> 4) & 0xF) * 16 + u32::from(c.addr));
        let why = self.unknown;
        let class = ap.map_or(unknown(why), |a| self.class(a));
        let named = match class {
            Value::Name(n) => Some(n),
            _ => None,
        };
        // An IDR read's class comes with its value.
        let idr = ap.filter(|_| c.read && reg == Some(0xFC));
        let fields = |out: &mut Out, id: Id| {
            out.set(id, AP, ap.map_or(unknown(why), |a| word(a.into(), 8)));
            out.set(id, AP_REG, reg.map_or(unknown(why), |r| word(r.into(), 8)));
            if let Some(n) = reg.and_then(|r| ap_name(named, r)) {
                out.set(id, AP_NAME, Value::Name(n));
            }
            out.set(id, AP_A, word(c.addr.into(), 4));
            if idr.is_none() {
                out.set(id, AP_CLASS, class);
            }
        };
        if !c.read {
            if let Some(p) = self.pending.take() {
                self.unread(p, c.start, NEVER_READ, out);
            }
            let id = Id::new(AP_WRITE, c.id.anchor);
            out.open(id, None, c.start);
            fields(out, id);
            Self::value(out, id, AP_VALUE, c);
            out.link(id, W_TRANSFER, Ref::Upstream(c.id));
            self.context_link(id, W_CONTEXT, out);
            out.close(id, c.end, Status::Complete);
            return;
        }
        if let Some(p) = self.pending.take() {
            self.complete(p, c, out);
        }
        let id = Id::new(AP_READ, c.id.anchor);
        out.open(id, None, c.start);
        fields(out, id);
        out.link(id, R_REQUEST, Ref::Upstream(c.id));
        self.context_link(id, R_CONTEXT, out);
        let p = Pending { id, idr };
        if self.own_reads {
            self.complete(p, c, out);
        } else {
            self.pending = Some(p);
        }
    }

    fn finish(&mut self, end: Tick, out: &mut Out) {
        if let Some(p) = self.pending.take() {
            self.unread(p, end, ENDS_UNREAD, out);
        }
        if let Some(ctx) = self.context.take() {
            out.close(ctx, end, Status::Complete);
        }
    }

    fn complete(&mut self, p: Pending, c: &Closed, out: &mut Out) {
        let v = Self::value(out, p.id, AP_VALUE, c);
        if let Some(ap) = p.idr {
            if let Some((idr, _)) = v {
                self.classes.insert(ap, ap_class(idr));
            }
            out.set(p.id, AP_CLASS, self.class(ap));
        }
        out.link(p.id, R_DATA, Ref::Upstream(c.id));
        out.close(p.id, c.end, Status::Complete);
    }

    fn piece(&mut self, op: &Op, out: &mut Out) {
        match *op {
            Op::Open { id, start, .. } => {
                self.building.insert(
                    id,
                    Partial {
                        start,
                        ..Partial::default()
                    },
                );
            }
            Op::Set {
                id,
                field,
                ref value,
                span,
                ..
            } => {
                let Some(p) = self.building.get_mut(&id) else {
                    return;
                };
                match (field, value) {
                    (swd::PORT, &Value::Name(n)) => p.ap = n == swd::AP,
                    (swd::DIR, &Value::Name(n)) => p.read = n == swd::READ,
                    (swd::A, &Value::Word { value, .. }) => p.addr = value as u8,
                    (swd::ACK, &Value::Name(n)) => {
                        p.ack = [1, 2, 4, 7, 0].get(usize::from(n)).copied().unwrap_or(0);
                    }
                    (swd::DATA, &Value::Word { value, .. }) => {
                        if let Some(span) = span {
                            p.data = Some((value, span));
                        }
                    }
                    (swd::PARITY, &Value::Name(n)) => p.parity = Some(n == swd::PARITY_OK),
                    _ => {}
                }
            }
            Op::Close { id, end, .. } => {
                let Some(p) = self.building.remove(&id) else {
                    return;
                };
                let data = p.data.map(|(w, span)| {
                    // A transfer with no parity, as JTAG-DP's, has none to fail.
                    let v = if p.parity != Some(false) {
                        word(w, 32)
                    } else {
                        Value::Unknown {
                            reason: PARITY_FAILED,
                            needs: None,
                        }
                    };
                    (v, span)
                });
                let c = Closed {
                    id,
                    start: p.start,
                    end,
                    ap: p.ap,
                    read: p.read,
                    addr: p.addr,
                    ack: p.ack,
                    data,
                };
                self.consume(&c, out);
            }
            _ => {}
        }
    }
}

impl Decoder for Adi {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        self.start(&mut ex.out);
        while let Some(d) = ex.receive() {
            match d {
                Delivery::Piece { piece, .. } => self.piece(piece.op(), &mut ex.out),
                Delivery::Frontier { start, .. } => self.up = Some(start),
                Delivery::End { end, .. } => {
                    self.finish(end, &mut ex.out);
                    ex.finish(end);
                    return Pause::Finished;
                }
                Delivery::Lost { span, .. } => {
                    self.building.clear();
                    self.gap(span.start, span.end, &mut ex.out);
                    ex.lost(span.start, span.end);
                }
                _ => {}
            }
        }
        // Its items start where SWD transfers start, so SWD's start frontier is its own.
        if let Some(up) = self.up {
            ex.frontier(up);
        }
        Pause::Input
    }

    fn save(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(self.clone()))
    }

    fn heap_bytes(&self) -> usize {
        crate::held::<(Id, Partial)>(self.building.capacity())
            + crate::held::<(u8, Result<u16, Reason>)>(self.classes.capacity())
    }
}

// ---- MEM-AP ------------------------------------------------------------------------------------

pub const MEM_CONTEXT: KindId = KindId(0);
pub const MEM_READ: KindId = KindId(1);
pub const MEM_WRITE: KindId = KindId(2);

pub const ACCESS: FieldId = FieldId(0);
pub const ADDRESS: FieldId = FieldId(0);
pub const MEM_NAME: FieldId = FieldId(1);
pub const MEM_VALUE: FieldId = FieldId(2);

pub const ACCESS_LINK: LinkId = LinkId(0);
pub const ADDRESS_FROM: LinkId = LinkId(1);
pub const INCREMENT_FROM: LinkId = LinkId(2);

pub const SCS_NAMES: [&str; 9] = [
    "CPUID", "DFSR", "DHCSR", "DCRSR", "DCRDR", "DEMCR", "DWT_CTRL", "FP_CTRL", "FP_COMP",
];

pub const AP_UNKNOWN: Reason = Reason(0);
pub const NO_TAR: Reason = Reason(1);
pub const PAST_1K: Reason = Reason(2);
pub const PACKED_WORD: Reason = Reason(3);
pub const NO_CSW: Reason = Reason(4);
pub const LOST_TAR: Reason = Reason(5);
pub const PACKED_WIDE: Reason = Reason(6);
pub const WIDE_CUT: Reason = Reason(7);
pub const CSW_RESERVED: Reason = Reason(8);
pub const UNALIGNED: Reason = Reason(9);
pub const UNPREDICTABLE: Reason = Reason(10);
pub const BD_SIZE: Reason = Reason(11);
pub const BD_ORDER: Reason = Reason(12);
pub const UNREAD: Reason = Reason(13);
pub const MEM_PARITY: Reason = Reason(14);
pub const MEM_NEEDS_SELECT: Need = Need(0);
pub const NEEDS_TAR: Need = Need(1);
pub const NEEDS_CSW: Need = Need(2);
pub const MEM_GAP_CUT: Cause = Cause(0);
pub const CSW_ENDS: Cause = Cause(1);
pub const INTERRUPTED: Cause = Cause(2);
pub const MEM_ENDS: Cause = Cause(3);

pub fn mem_schema() -> Schema {
    let w = |bits| FieldType::Word { bits };
    let access = || {
        vec![
            LinkTo::Upstream(name("ap-read")),
            LinkTo::Upstream(name("ap-write")),
        ]
    };
    let mem = |kind: &'static str| {
        KindDecl::new(kind)
            .with_fields(vec![
                field("address", w(32)),
                field("name", FieldType::Name(SCS_NAMES.map(name).to_vec())),
                field("value", FieldType::Bits { max: 256 }),
            ])
            .with_links(vec![
                LinkDecl::new("access", access()),
                LinkDecl::new(
                    "address from",
                    vec![
                        LinkTo::Upstream(name("ap-read")),
                        LinkTo::Upstream(name("ap-write")),
                        LinkTo::Own(name("mem-read")),
                        LinkTo::Own(name("mem-write")),
                    ],
                ),
                LinkDecl::new("increment from", access()),
            ])
            .with_labels(vec![
                (Label::default().field(ADDRESS).text(" ").field(MEM_NAME))
                    .text(" ")
                    .field(MEM_VALUE),
                Label::default().field(MEM_NAME).text(" ").field(MEM_VALUE),
                Label::default().field(MEM_NAME),
            ])
    };
    Schema::new(vec![
        KindDecl::new("context")
            .with_fields(vec![field("access", FieldType::Name(vec![name("DRW")]))]),
        mem("mem-read"),
        mem("mem-write"),
    ])
    .with_reasons([
        "AP register unknown, so memory accesses cannot be told apart",
        "no TAR write decoded before this access",
        "auto-increment past a 1 KiB boundary is implementation defined",
        "packed word accesses increment only on a MEM-AP that takes packed transfers, which a CSW \
         write does not show",
        "CSW unknown, so the access size and auto-increment are unknown",
        "the TAR value is lost in a recording gap",
        "packed transfers wider than a word are unpredictable",
        "a wide access cut short leaves TAR unknown",
        "CSW holds a reserved AddrInc or Size, so the access is unknown",
        "an access not aligned to its size is implementation defined",
        "another register access in the middle of a wide access is unpredictable",
        "banked data accesses other than word or doubleword are unpredictable",
        "a doubleword's banked data access that starts with its pair's second register is \
         unpredictable",
        "the AP read closed before its result came back",
        "data parity failed",
    ])
    .with_needs(["SELECT", "TAR", "CSW"])
    .with_causes([
        GAP_CUT_TEXT,
        "a CSW access ends it before its last word, and a write ended so makes no memory access",
        "another register access interrupts it, which is unpredictable",
        "the recording ends before its last word",
    ])
    // A wide access stays open over its words, at most one on each AP, and the context item.
    .with_open_at_most(257)
}

pub fn mem_definition() -> Arc<Definition> {
    Arc::new(
        Definition::new("mem-ap", 1, mem_schema()).with_inputs([UpstreamDecl::new(
            "accesses",
            Mode::Completed,
        )
        .from(["adiv5"])
        .with_kinds(["ap-read", "ap-write"])]),
    )
}

/// A System Control Space register's name, by its index in `SCS_NAMES`.
pub fn scs_name(addr: u32) -> Option<u16> {
    Some(match addr {
        0xE000_ED00 => 0,
        0xE000_ED30 => 1,
        0xE000_EDF0 => 2,
        0xE000_EDF4 => 3,
        0xE000_EDF8 => 4,
        0xE000_EDFC => 5,
        0xE000_1000 => 6,
        0xE000_2000 => 7,
        0xE000_2008..=0xE000_2024 => 8,
        _ => return None,
    })
}

/// A completed ADIv5 AP access as MEM-AP reads it.
#[derive(Clone, Debug)]
pub struct Access {
    pub id: Id,
    pub span: Span,
    pub write: bool,
    pub ap: Option<u8>,
    pub reg: Option<u32>,
    pub value: Option<(u32, Span)>,
    /// The AP's class, where ADIv5 knows it.
    pub class: Option<u16>,
    /// The value's parity failed: for a write, the target ignores it; for a read, the access was
    /// made but its value is unknown.
    pub parity_failed: bool,
}

impl Access {
    pub fn of(item: &Completed) -> Self {
        let word = |f| match item.field(f).map(|f| &f.value) {
            Some(Value::Word { value, .. }) => Some(*value),
            _ => None,
        };
        let value = item.field(AP_VALUE).and_then(|f| match (&f.value, f.span) {
            (Value::Word { value, .. }, Some(s)) => Some((*value as u32, s)),
            _ => None,
        });
        Self {
            id: item.id,
            span: Span {
                start: item.start,
                end: item.end,
            },
            write: item.id.kind == AP_WRITE,
            ap: word(AP).map(|v| v as u8),
            reg: word(AP_REG).map(|v| v as u32),
            value,
            class: match item.field(AP_CLASS).map(|f| &f.value) {
                Some(&Value::Name(n)) => Some(n),
                _ => None,
            },
            parity_failed: matches!(
                item.field(AP_VALUE).map(|f| &f.value),
                Some(Value::Unknown {
                    reason: PARITY_FAILED,
                    ..
                })
            ),
        }
    }

    /// The access's data, or why it is unknown.
    fn data(&self) -> Result<(u32, Span), Reason> {
        match self.value {
            Some(v) => Ok(v),
            None if self.parity_failed => Err(MEM_PARITY),
            // A read closed incomplete: made, but its result never came back.
            None => Err(UNREAD),
        }
    }
}

/// How far a DRW access moves TAR under CSW `csw`, `read` back from the MEM-AP or as written. A
/// wide access moves it once, after its last DRW access.
fn increment(csw: u32, read: bool) -> Result<u32, Reason> {
    match (csw >> 4 & 3, csw & 7) {
        (0b00, _) => Ok(0),
        (0b11, _) | (_, 0b110 | 0b111) => Err(CSW_RESERVED),
        (0b01, size) => Ok(1 << size),
        // Packed: a word's bytes in accesses of the size, so four.
        (0b10, 0b000 | 0b001) => Ok(4),
        // A MEM-AP that takes only words takes packed for off, and a read of CSW says so.
        (0b10, 0b010) if read => Ok(4),
        (0b10, 0b010) => Err(PACKED_WORD),
        _ => Err(PACKED_WIDE),
    }
}

/// The bytes one DRW or banked data word moved, lowest address first: `n` bytes from address
/// `from`, each from the byte lane its address gives (table C2-3, C2.2.7).
fn lanes(word: u32, from: u32, n: u32) -> Value {
    let bytes = std::array::from_fn::<u8, 4, _>(|k| {
        (word >> (8 * (from.wrapping_add(k as u32) & 3))) as u8
    });
    Value::Bits {
        bytes: bytes[..n.min(4) as usize].into(),
        bits: 8 * n,
    }
}

fn unknown_value(reason: Reason, needs: Option<Need>) -> Value {
    Value::Unknown { reason, needs }
}

/// An AP's CSW as MEM-AP last saw it.
#[derive(Clone, Copy, Debug)]
struct Csw {
    value: u32,
    from: Id,
    /// Read back from the MEM-AP, so what it took rather than what a write asked for.
    read: bool,
}

/// An AP's TAR as MEM-AP knows it, and the item it came from.
type Tar = (Result<u32, Reason>, Ref);

/// A memory access wider than a word, open over the DRW or banked data accesses it takes, the
/// least significant word first (table C2-20, C2.6.2.2).
#[derive(Clone, Debug)]
struct Wide {
    id: Id,
    write: bool,
    /// The register the next word goes through: DRW, or the pair's second banked data register.
    next: u32,
    /// Each word and its span, or why it is unknown.
    words: Vec<Result<(u32, Span), Reason>>,
    need: usize,
    /// TAR at the first word, for a DRW access; a banked data access leaves TAR as it is.
    tar: Option<Tar>,
    csw: Csw,
}

/// MEM-AP over completed AP accesses. An access to an AP ADIv5 knows is of another class is
/// skipped. An access completed later starts no earlier than ADIv5's settlement frontier, so that
/// is its start frontier.
#[derive(Clone, Debug)]
pub struct Mem {
    csw: HashMap<u8, Csw>,
    tar: HashMap<u8, Tar>,
    /// Why an AP's TAR is not in `tar`: nothing decoded yet, or a recording gap.
    no_tar: Reason,
    wide: HashMap<u8, Wide>,
    context: Option<Id>,
    up: Option<Tick>,
}

impl Default for Mem {
    fn default() -> Self {
        Self {
            csw: HashMap::default(),
            tar: HashMap::default(),
            no_tar: NO_TAR,
            wide: HashMap::default(),
            context: None,
            up: None,
        }
    }
}

impl Mem {
    pub fn factory() -> Factory {
        Box::new(|_, _| Box::new(Self::default()))
    }

    fn finish(&mut self, end: Tick, out: &mut Out) {
        for (_, w) in self.wide.drain() {
            out.close(w.id, end, Status::Incomplete(MEM_ENDS));
        }
        if let Some(ctx) = self.context.take() {
            out.close(ctx, end, Status::Complete);
        }
    }

    /// Input lost from `from`: every AP's TAR and CSW are unknown after it.
    fn gap(&mut self, from: Tick, out: &mut Out) {
        for (_, w) in self.wide.drain() {
            out.close(w.id, from, Status::Incomplete(MEM_GAP_CUT));
        }
        if let Some(ctx) = self.context.take() {
            out.close(ctx, from, Status::Incomplete(MEM_GAP_CUT));
        }
        self.csw.clear();
        self.tar.clear();
        self.no_tar = LOST_TAR;
    }

    fn consume(&mut self, a: &Access, out: &mut Out) {
        let (Some(ap), Some(reg)) = (a.ap, a.reg) else {
            if self.context.is_none() {
                let id = Id::new(MEM_CONTEXT, a.id.anchor);
                out.open(id, None, a.span.start);
                let why = Value::Unknown {
                    reason: AP_UNKNOWN,
                    needs: Some(MEM_NEEDS_SELECT),
                };
                out.set(id, ACCESS, why);
                self.context = Some(id);
            }
            return;
        };
        if let Some(ctx) = self.context.take() {
            out.close(ctx, a.span.start, Status::Complete);
        }
        // A write whose data fails parity is dropped, and WDATAERR set (B4.2.8.3).
        if a.class.is_some_and(|c| c != MEM_AP) || a.write && a.value.is_none() {
            return;
        }
        if let Some(w) = self.wide.remove(&ap) {
            if reg == w.next && a.write == w.write {
                return self.word(ap, w, a, out);
            }
            let cause = if reg == 0x00 { CSW_ENDS } else { INTERRUPTED };
            out.close(w.id, a.span.start, Status::Incomplete(cause));
            if cause == INTERRUPTED {
                // The access that interrupts it, and TAR after it, are unpredictable.
                let at = (Err(UNPREDICTABLE), Ref::Upstream(a.id));
                self.tar.insert(ap, at);
                return;
            }
            if w.next == 0x0C
                && matches!(w.tar, Some((Ok(_), _)))
                && increment(w.csw.value, w.csw.read) != Ok(0)
            {
                self.tar.insert(ap, (Err(WIDE_CUT), Ref::Own(w.id)));
            }
        }
        match reg {
            0x00 => {
                if let Some((v, _)) = a.value {
                    let csw = Csw {
                        value: v,
                        from: a.id,
                        read: !a.write,
                    };
                    self.csw.insert(ap, csw);
                }
            }
            0x04 => {
                if let Some((v, _)) = a.value {
                    self.tar.insert(ap, (Ok(v), Ref::Upstream(a.id)));
                }
            }
            0x0C | 0x10..=0x1C => self.memory(ap, reg, a, out),
            _ => {}
        }
    }

    /// The address a DRW or banked data access to `reg` reaches with TAR at `tar`. A doubleword
    /// starts at its pair's first register's (table C2-11).
    fn address(&self, reg: u32, tar: Option<&Tar>) -> Result<u32, Reason> {
        let t = match tar {
            Some((t, _)) => *t,
            None => Err(self.no_tar),
        }?;
        Ok(match reg {
            0x0C => t,
            _ => (t & !0xF) + (reg - 0x10),
        })
    }

    /// A DRW or banked data access: a memory access, or the first word of a wide one.
    fn memory(&mut self, ap: u8, reg: u32, a: &Access, out: &mut Out) {
        let drw = reg == 0x0C;
        let csw = self.csw.get(&ap).copied();
        let tar = self.tar.get(&ap).copied();
        let size = csw.map(|c| c.value & 7);
        let packed = csw.is_some_and(|c| c.value >> 4 & 3 == 0b10);
        let wide = match size {
            Some(s @ 3..=5) if drw && !packed => Some(1 << (s - 2)),
            Some(3) if !drw && reg & 4 == 0 => Some(2),
            _ => None,
        };
        let address = self.address(reg, tar.as_ref());
        let id = Id::new(if a.write { MEM_WRITE } else { MEM_READ }, a.id.anchor);
        out.open(id, None, a.span.start);
        out.set(
            id,
            ADDRESS,
            address.map_or_else(
                |why| unknown_value(why, Some(NEEDS_TAR)),
                |t| word(t.into(), 32),
            ),
        );
        if let Some(n) = address.ok().and_then(scs_name) {
            out.set(id, MEM_NAME, Value::Name(n));
        }
        out.link(id, ACCESS_LINK, Ref::Upstream(a.id));
        if let Some((_, src)) = &tar {
            out.link(id, ADDRESS_FROM, *src);
        }
        if drw && let Some(c) = &csw {
            out.link(id, INCREMENT_FROM, Ref::Upstream(c.from));
        }
        if let (Some(need), Some(c)) = (wide, csw) {
            let w = Wide {
                id,
                write: a.write,
                next: reg,
                words: Vec::with_capacity(need),
                need,
                tar: drw.then_some(tar).flatten(),
                csw: c,
            };
            return self.word(ap, w, a, out);
        }
        let value = match (size, a.data()) {
            (_, Err(why)) => unknown_value(why, None),
            (None, _) => unknown_value(NO_CSW, Some(NEEDS_CSW)),
            (Some(6 | 7), _) => unknown_value(CSW_RESERVED, None),
            (Some(3..=5), _) if drw => unknown_value(PACKED_WIDE, None),
            (Some(3), _) => unknown_value(BD_ORDER, None),
            (Some(s), _) if !drw && s != 2 => unknown_value(BD_SIZE, None),
            (Some(s), Ok((v, _))) => {
                let (each, n) = (1 << s, if packed { 4 } else { 1 << s });
                match address {
                    Ok(t) if t % each != 0 => unknown_value(UNALIGNED, None),
                    // A packed access whose later bytes are past a 1 KiB boundary.
                    Ok(t) if (t ^ t.wrapping_add(n - 1)) & !0x3FF != 0 => {
                        unknown_value(PAST_1K, None)
                    }
                    Ok(t) => lanes(v, t, n),
                    // TAR unknown: a word's bytes in lane order, as an aligned access holds them.
                    Err(_) if s == 2 => lanes(v, 0, 4),
                    Err(why) => unknown_value(why, Some(NEEDS_TAR)),
                }
            }
        };
        match a.value {
            Some((_, span)) => out.part(id, MEM_VALUE, value, span, None),
            None => out.set(id, MEM_VALUE, value),
        }
        out.close(id, a.span.end, Status::Complete);
        if drw {
            self.moved(ap, tar, csw, id);
        }
    }

    /// TAR after a DRW access, or after a wide one's last word.
    fn moved(&mut self, ap: u8, tar: Option<Tar>, csw: Option<Csw>, id: Id) {
        let next = match (tar.map(|t| t.0), csw) {
            (Some(Ok(t)), Some(csw)) => increment(csw.value, csw.read).and_then(|step| {
                let n = t.wrapping_add(step);
                if (n ^ t) & !0x3FF != 0 {
                    Err(PAST_1K)
                } else {
                    Ok(n)
                }
            }),
            (Some(Ok(_)), None) => Err(NO_CSW),
            (Some(Err(e)), _) => Err(e),
            (None, _) => return,
        };
        self.tar.insert(ap, (next, Ref::Own(id)));
    }

    /// One word of the wide access `w`; its last closes it.
    fn word(&mut self, ap: u8, mut w: Wide, a: &Access, out: &mut Out) {
        if !w.words.is_empty() {
            out.link(w.id, ACCESS_LINK, Ref::Upstream(a.id));
        }
        w.words.push(a.data());
        if w.words.len() < w.need {
            if w.next != 0x0C {
                w.next += 4;
            }
            self.wide.insert(ap, w);
            return;
        }
        let words: Result<Vec<_>, _> = w.words.iter().copied().collect();
        match words {
            Ok(words) => {
                let span = Span {
                    start: words[0].1.start,
                    end: words[words.len() - 1].1.end,
                };
                let bytes = words.iter().flat_map(|(x, _)| x.to_le_bytes()).collect();
                let bits = 32 * w.need as u32;
                out.part(w.id, MEM_VALUE, Value::Bits { bytes, bits }, span, None);
            }
            Err(why) => out.set(w.id, MEM_VALUE, unknown_value(why, None)),
        }
        out.close(w.id, a.span.end, Status::Complete);
        if let Some(tar) = w.tar {
            self.moved(ap, Some(tar), Some(w.csw), w.id);
        }
    }
}

impl Decoder for Mem {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        while let Some(d) = ex.receive() {
            match d {
                Delivery::Item { item, .. } => self.consume(&Access::of(&item), &mut ex.out),
                Delivery::Frontier { settled, .. } => self.up = Some(settled),
                Delivery::End { end, .. } => {
                    self.finish(end, &mut ex.out);
                    ex.finish(end);
                    return Pause::Finished;
                }
                Delivery::Lost { span, .. } => {
                    self.gap(span.start, &mut ex.out);
                    ex.lost(span.start, span.end);
                }
                _ => {}
            }
        }
        if let Some(up) = self.up {
            // A wide access's value is set at its last word and spans back to its first.
            let held = (self.wide.values())
                .filter_map(|w| Some(w.words.first()?.ok()?.1.start))
                .min();
            ex.frontier(held.map_or(up, |h| h.min(up)));
        }
        Pause::Input
    }

    fn save(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(self.clone()))
    }

    fn heap_bytes(&self) -> usize {
        crate::held::<(u8, Csw)>(self.csw.capacity())
            + crate::held::<(u8, Tar)>(self.tar.capacity())
            + crate::held::<(u8, Wide)>(self.wide.capacity())
            + (self.wide.values())
                .map(|w| w.words.capacity() * size_of::<Result<(u32, Span), Reason>>())
                .sum::<usize>()
    }
}
