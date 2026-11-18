//! ADIv5 register accesses stacked on SWD transfers, and MEM-AP memory accesses stacked on those:
//! enough of each to show stacking by pieces and by completed items, context carried across
//! transfers, and values unknown for a stated reason.
//!
//! ADIv5 reads SWD's transfers as pieces, typed fields only. A SELECT write sets the DP bank and
//! the AP and its register bank; an AP read's data arrives in the next AP read or RDBUFF read,
//! so an AP read stays open until then. MEM-AP reads ADIv5's completed AP accesses: TAR and CSW
//! writes, then DRW accesses at the address TAR holds, auto-incremented as CSW says.

use crate::swd;
use escapement_tickwave_decode::{
    AnalysisId, Cause, Completed, Decoder, Definition, Delivery, Exchange, Factory, FieldId,
    FieldType, Id, KindDecl, KindId, LinkDecl, LinkId, LinkTo, Mode, Need, Op, Out, Pause, Reason,
    Ref, Schema, Span, Status, Subscription, Tick, Value, field, name,
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
pub const AP_NAMES: [&str; 11] = [
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
];

pub const BANK_UNKNOWN: Reason = Reason(0);
pub const NO_SELECT: Reason = Reason(1);
pub const PARITY_FAILED: Reason = Reason(2);
pub const LOST_SELECT: Reason = Reason(3);
pub const LOST_BANK: Reason = Reason(4);
pub const NEEDS_SELECT: Need = Need(0);

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
    };
    let ap = |kind: &'static str, links| {
        KindDecl::new(kind)
            .with_fields(vec![
                field("ap", w(8)),
                field("reg", w(8)),
                field("name", FieldType::Name(AP_NAMES.map(name).to_vec())),
                field("a", w(4)),
                field("value", w(32)),
            ])
            .with_links(links)
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
        ap(
            "ap-read",
            vec![
                LinkDecl::new("request", vec![transfer()]),
                LinkDecl::new("data", vec![transfer()]),
                LinkDecl::new("context", vec![dp_write()]),
            ],
        ),
    ])
    .with_reasons([
        "DP bank unknown: no SELECT write decoded before this access",
        "no SELECT write decoded before this access",
        "data parity failed",
        "the SELECT value is lost in a recording gap",
        "DP bank unknown: the SELECT value is lost in a recording gap",
    ])
    .with_needs(["SELECT"])
    .with_causes([
        "result never read back",
        "the recording ends before the result is read back",
        GAP_CUT_TEXT,
    ])
    .with_open_at_most(3)
}

pub fn adi_definition() -> Arc<Definition> {
    Arc::new(Definition::new("adiv5", 1, adi_schema()))
}

/// What ADIv5 subscribes to of SWD.
pub fn adi_subscription(swd: AnalysisId) -> Subscription {
    Subscription::new(swd, Mode::Pieces).with_kinds(["transfer"])
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

/// An AP register's name, by its index in `AP_NAMES`.
pub fn ap_name(reg: u32) -> Option<u16> {
    Some(match reg {
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
        0xFC => 10,
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
        }
    }

    pub fn factory() -> Factory {
        Box::new(|local| Box::new(Self::new(local)))
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
            out.close(p.id, from, Status::Incomplete(GAP_CUT));
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
        let fields = |out: &mut Out, id: Id| {
            out.set(id, AP, ap.map_or(unknown(why), |a| word(a.into(), 8)));
            out.set(id, AP_REG, reg.map_or(unknown(why), |r| word(r.into(), 8)));
            if let Some(n) = reg.and_then(ap_name) {
                out.set(id, AP_NAME, Value::Name(n));
            }
            out.set(id, AP_A, word(c.addr.into(), 4));
        };
        if !c.read {
            if let Some(p) = self.pending.take() {
                out.close(p.id, c.start, Status::Incomplete(NEVER_READ));
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
        self.pending = Some(Pending { id });
    }

    fn finish(&mut self, end: Tick, out: &mut Out) {
        if let Some(p) = self.pending.take() {
            out.close(p.id, end, Status::Incomplete(ENDS_UNREAD));
        }
        if let Some(ctx) = self.context.take() {
            out.close(ctx, end, Status::Complete);
        }
    }

    fn complete(&mut self, p: Pending, c: &Closed, out: &mut Out) {
        Self::value(out, p.id, AP_VALUE, c);
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
                    (swd::PARITY, &Value::Bool(ok)) => p.parity = Some(ok),
                    _ => {}
                }
            }
            Op::Close { id, end, .. } => {
                let Some(p) = self.building.remove(&id) else {
                    return;
                };
                let data = p.data.map(|(w, span)| {
                    let v = if p.parity == Some(true) {
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
pub const PACKED: Reason = Reason(3);
pub const NO_CSW: Reason = Reason(4);
pub const LOST_TAR: Reason = Reason(5);
pub const MEM_NEEDS_SELECT: Need = Need(0);
pub const NEEDS_TAR: Need = Need(1);
pub const MEM_GAP_CUT: Cause = Cause(0);

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
                field("value", w(32)),
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
        "packed auto-increment is not decoded",
        "CSW unknown, so the auto-increment is unknown",
        "the TAR value is lost in a recording gap",
    ])
    .with_needs(["SELECT", "TAR"])
    .with_causes([GAP_CUT_TEXT])
}

pub fn mem_definition() -> Arc<Definition> {
    Arc::new(Definition::new("mem-ap", 1, mem_schema()))
}

/// What MEM-AP subscribes to of ADIv5.
pub fn mem_subscription(adi: AnalysisId) -> Subscription {
    Subscription::new(adi, Mode::Completed).with_kinds(["ap-read", "ap-write"])
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
        }
    }
}

/// MEM-AP over completed AP accesses. An access closed incomplete carries no data and is
/// skipped. An access completed later starts no earlier than ADIv5's settlement frontier, so that
/// is its start frontier.
#[derive(Clone, Debug)]
pub struct Mem {
    csw: HashMap<u8, (u32, Id)>,
    tar: HashMap<u8, (Result<u32, Reason>, Ref)>,
    /// Why an AP's TAR is not in `tar`: nothing decoded yet, or a recording gap.
    no_tar: Reason,
    context: Option<Id>,
    up: Option<Tick>,
}

impl Default for Mem {
    fn default() -> Self {
        Self {
            csw: HashMap::default(),
            tar: HashMap::default(),
            no_tar: NO_TAR,
            context: None,
            up: None,
        }
    }
}

impl Mem {
    pub fn factory() -> Factory {
        Box::new(|_| Box::new(Self::default()))
    }

    fn finish(&mut self, end: Tick, out: &mut Out) {
        if let Some(ctx) = self.context.take() {
            out.close(ctx, end, Status::Complete);
        }
    }

    /// Input lost from `from`: every AP's TAR and CSW are unknown after it.
    fn gap(&mut self, from: Tick, out: &mut Out) {
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
        match reg {
            0x00 => {
                if let Some((v, _)) = a.value {
                    self.csw.insert(ap, (v, a.id));
                }
            }
            0x04 => {
                if let Some((v, _)) = a.value {
                    self.tar.insert(ap, (Ok(v), Ref::Upstream(a.id)));
                }
            }
            0x0C => self.drw(ap, a, out),
            _ => {}
        }
    }

    fn drw(&mut self, ap: u8, a: &Access, out: &mut Out) {
        let Some((v, span)) = a.value else { return };
        let kind = if a.write { MEM_WRITE } else { MEM_READ };
        let id = Id::new(kind, a.id.anchor);
        out.open(id, None, a.span.start);
        let tar = self.tar.get(&ap).copied();
        let address = match &tar {
            Some((Ok(t), _)) => word((*t).into(), 32),
            Some((Err(why), _)) => Value::Unknown {
                reason: *why,
                needs: Some(NEEDS_TAR),
            },
            None => Value::Unknown {
                reason: self.no_tar,
                needs: Some(NEEDS_TAR),
            },
        };
        out.set(id, ADDRESS, address);
        if let Some((Ok(t), _)) = &tar
            && let Some(n) = scs_name(*t)
        {
            out.set(id, MEM_NAME, Value::Name(n));
        }
        out.part(id, MEM_VALUE, word(v.into(), 32), span, None);
        out.link(id, ACCESS_LINK, Ref::Upstream(a.id));
        if let Some((_, src)) = &tar {
            out.link(id, ADDRESS_FROM, *src);
        }
        if let Some((_, src)) = self.csw.get(&ap) {
            out.link(id, INCREMENT_FROM, Ref::Upstream(*src));
        }
        out.close(id, a.span.end, Status::Complete);
        let next = match (tar.map(|t| t.0), self.csw.get(&ap)) {
            (Some(Ok(t)), Some((csw, _))) => match (csw >> 4) & 3 {
                0 => Ok(t),
                1 => {
                    let n = t.wrapping_add(1 << (csw & 7));
                    if (n ^ t) & !0x3FF != 0 {
                        Err(PAST_1K)
                    } else {
                        Ok(n)
                    }
                }
                _ => Err(PACKED),
            },
            (Some(Ok(_)), None) => Err(NO_CSW),
            (Some(Err(e)), _) => Err(e),
            (None, _) => return,
        };
        self.tar.insert(ap, (next, Ref::Own(id)));
    }
}

impl Decoder for Mem {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        while let Some(d) = ex.receive() {
            match d {
                Delivery::Item { item, .. } => {
                    if item.status == Status::Complete {
                        self.consume(&Access::of(&item), &mut ex.out);
                    }
                }
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
            ex.frontier(up);
        }
        Pause::Input
    }

    fn save(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(self.clone()))
    }

    fn heap_bytes(&self) -> usize {
        crate::held::<(u8, (u32, Id))>(self.csw.capacity())
            + crate::held::<(u8, (Result<u32, Reason>, Ref))>(self.tar.capacity())
    }
}
