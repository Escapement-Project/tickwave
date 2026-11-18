//! ARM's JTAG-DP (ADIv5, "The JTAG Debug Port") stacked on JTAG's TAP parts: the DP's instruction
//! from its TAP's `instruction` parts, and each access its `data` parts make, as transfers ADIv5
//! reads as it reads SWD's. JTAG needs the chain set (`jtag::chain`) to cut its scans into parts;
//! the setting `tap` names the DP's place in it, nearest TDO first.
//!
//! The DP's instruction is 4 bits: ABORT, DPACC, APACC, IDCODE or BYPASS among them. Under DPACC
//! or APACC a DR scan shifts in 35 bits, RnW then A[3:2] then the data written, and captures 3
//! bits of ACK then the 32-bit result of the transaction before. OK (0b010, which JTAG-DP also
//! answers for a fault, kept in CTRL/STAT's sticky flags) says that transaction is done and the
//! DP takes this scan's request; WAIT (0b001) says it is not, and the DP ignores this request.
//! So a transfer opens at its own scan, with the ack its scan captured; a write closes there, and
//! a read when the next scan answering OK returns its result. Each read carries its own data,
//! which ADIv5 reads with its `reads` setting at "own". An ABORT scan is a DP write to ABORT, and
//! a 32-bit DR scan under IDCODE an `idcode` item.

use crate::{jtag, swd};
use escapement_tickwave_decode::{
    AnalysisId, Cause, Completed, Decoder, Definition, Delivery, Exchange, Factory, FieldId,
    FieldType, Id, KindDecl, KindId, Label, Mode, Out, Pause, Reason, Schema, SettingDecl,
    SettingId, Span, Status, Subscription, Tick, Value, field,
};
use std::sync::Arc;

pub const TRANSFER: KindId = KindId(0);
pub const IDCODE: KindId = KindId(1);
/// `IDCODE`'s field.
pub const IDCODE_VALUE: FieldId = FieldId(0);

pub const NO_TDO: Reason = Reason(0);

pub const GAP_CUT: Cause = Cause(0);
pub const ENDS_UNREAD: Cause = Cause(1);

/// The DP's place in JTAG's chain, nearest TDO first.
pub const TAP: SettingId = SettingId(0);

/// JTAG-DP's instructions this decoder reads.
const ABORT: u8 = 0x8;
const DPACC: u8 = 0xA;
const APACC: u8 = 0xB;
const IDCODE_IR: u8 = 0xE;
/// The ACKs a DPACC or APACC scan captures.
const OK: u64 = 0b010;
const WAIT: u64 = 0b001;

pub fn schema() -> Schema {
    Schema::new(vec![
        KindDecl::new("transfer")
            .with_fields(swd::transfer_fields())
            .with_labels(swd::transfer_labels()),
        KindDecl::new("idcode")
            .with_fields(vec![field("idcode", FieldType::Word { bits: 32 })])
            .with_labels(vec![
                Label::default().text("IDCODE ").field(IDCODE_VALUE),
                Label::default().text("IDCODE"),
            ]),
    ])
    .with_reasons(["JTAG's TDO is not known, so no ACK or result is"])
    .with_causes([
        "a recording gap interrupts the read before its result",
        "the recording ends before the read's result",
    ])
    .with_open_at_most(3)
}

pub fn definition() -> Arc<Definition> {
    Arc::new(
        Definition::new("jtag-dp", 1, schema()).with_settings(vec![SettingDecl::int(
            "tap",
            0..=7,
            0,
        )]),
    )
}

/// What JTAG-DP subscribes to of JTAG.
pub fn subscription(jtag: AnalysisId) -> Subscription {
    Subscription::new(jtag, Mode::Completed).with_kinds(["instruction", "data"])
}

/// A TAP part's bits, first shifted first, where they are known.
fn bits(item: &Completed, f: FieldId) -> Option<Vec<bool>> {
    match item.field(f).map(|f| &f.value)? {
        Value::Bits { bytes, bits } => Some(
            (0..*bits as usize)
                .map(|k| bytes.get(k / 8).is_some_and(|b| b >> (k % 8) & 1 == 1))
                .collect(),
        ),
        _ => None,
    }
}

/// Bits `from..to` of `b` as a number, the first the lowest.
fn number(b: &[bool], from: usize, to: usize) -> u64 {
    (b[from..to].iter().rev()).fold(0, |v, &x| v << 1 | u64::from(x))
}

fn word(value: u64, bits: u8) -> Value {
    Value::Word { value, bits }
}

#[derive(Clone, Debug)]
pub struct JtagDp {
    tap: u64,
    /// The DP's instruction, as its TAP's last instruction part shifted it in.
    ir: Option<u8>,
    /// The read the DP last took, open until a scan returns its result.
    pending: Option<(Id, Tick)>,
    up: Option<Tick>,
}

impl JtagDp {
    pub fn new(tap: u64) -> Self {
        Self {
            tap,
            ir: None,
            pending: None,
            up: None,
        }
    }

    pub fn factory() -> Factory {
        Box::new(|b, _| Box::new(Self::new(b.int(TAP).unwrap_or(0) as u64)))
    }

    fn part(&mut self, item: &Completed, out: &mut Out) {
        let tap = match item.field(jtag::TAP_INDEX).map(|f| &f.value) {
            Some(Value::Word { value, .. }) => *value,
            _ => return,
        };
        if tap != self.tap {
            return;
        }
        let span = Span {
            start: item.start,
            end: item.end,
        };
        let tdi = bits(item, jtag::PART_TDI);
        if item.id.kind == jtag::INSTRUCTION {
            self.ir = tdi.filter(|b| b.len() == 4).map(|b| number(&b, 0, 4) as u8);
            return;
        }
        let tdo = bits(item, jtag::PART_TDO);
        match (self.ir, tdi) {
            (Some(DPACC | APACC), Some(tdi)) if tdi.len() == 35 => {
                self.access(item.id, span, &tdi, tdo.as_deref(), out);
            }
            (Some(ABORT), Some(tdi)) if tdi.len() == 35 => {
                let id = Id::new(TRANSFER, item.id.anchor);
                out.open(id, None, span.start);
                out.set(id, swd::PORT, Value::Name(swd::DP));
                out.set(id, swd::DIR, Value::Name(swd::WRITE));
                out.set(id, swd::A, word(0, 4));
                out.set(id, swd::ACK, Value::Name(0));
                out.part(id, swd::DATA, word(number(&tdi, 3, 35), 32), span, None);
                out.close(id, span.end, Status::Complete);
            }
            (Some(IDCODE_IR), _) => {
                if let Some(tdo) = tdo.filter(|b| b.len() == 32) {
                    let id = Id::new(IDCODE, item.id.anchor);
                    out.open(id, None, span.start);
                    out.part(id, IDCODE_VALUE, word(number(&tdo, 0, 32), 32), span, None);
                    out.close(id, span.end, Status::Complete);
                }
            }
            _ => {}
        }
    }

    /// A DPACC or APACC scan: it returns the read the DP last took where it answers OK, and
    /// makes this scan's request a transfer.
    fn access(&mut self, at: Id, span: Span, tdi: &[bool], tdo: Option<&[bool]>, out: &mut Out) {
        let ack = tdo.map(|b| number(b, 0, 3));
        if ack == Some(OK)
            && let (Some((p, _)), Some(tdo)) = (self.pending.take(), tdo)
        {
            out.part(p, swd::DATA, word(number(tdo, 3, 35), 32), span, None);
            out.close(p, span.end, Status::Complete);
        }
        let id = Id::new(TRANSFER, at.anchor);
        let port = if self.ir == Some(APACC) {
            swd::AP
        } else {
            swd::DP
        };
        let read = tdi[0];
        out.open(id, None, span.start);
        out.set(id, swd::PORT, Value::Name(port));
        let dir = if read { swd::READ } else { swd::WRITE };
        out.set(id, swd::DIR, Value::Name(dir));
        out.set(id, swd::A, word(number(tdi, 1, 3) << 2, 4));
        let ack = match ack {
            Some(OK) => Value::Name(0),
            Some(WAIT) => Value::Name(1),
            Some(_) => Value::Name(4),
            None => Value::Unknown {
                reason: NO_TDO,
                needs: None,
            },
        };
        let taken = ack == Value::Name(0);
        out.set(id, swd::ACK, ack);
        if !read {
            out.part(id, swd::DATA, word(number(tdi, 3, 35), 32), span, None);
        }
        if read && taken {
            self.pending = Some((id, span.start));
        } else {
            out.close(id, span.end, Status::Complete);
        }
    }

    fn cut(&mut self, at: Tick, cause: Cause, out: &mut Out) {
        if let Some((p, _)) = self.pending.take() {
            out.close(p, at, Status::Incomplete(cause));
        }
    }
}

impl Decoder for JtagDp {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        while let Some(d) = ex.receive() {
            match d {
                Delivery::Item { item, .. } => {
                    if item.status == Status::Complete {
                        self.part(&item, &mut ex.out);
                    }
                }
                Delivery::Frontier { settled, .. } => self.up = Some(settled),
                Delivery::End { end, .. } => {
                    self.cut(end, ENDS_UNREAD, &mut ex.out);
                    ex.finish(end);
                    return Pause::Finished;
                }
                Delivery::Lost { span, .. } => {
                    self.cut(span.start, GAP_CUT, &mut ex.out);
                    self.ir = None;
                    ex.lost(span.start, span.end);
                }
                _ => {}
            }
        }
        // A stacked decoder reads a transfer once it closes and starts its own item where the
        // transfer starts, so no frontier passes the start of a read still open.
        if let Some(up) = self.up {
            ex.frontier(self.pending.map_or(up, |(_, start)| up.min(start)));
        }
        Pause::Input
    }

    fn save(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(self.clone()))
    }
}
