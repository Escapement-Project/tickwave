//! Serial Wire Debug (ADIv5.2, B4): each transfer framed from SWDIO sampled at SWCLK's rise, with
//! its request, acknowledge, data and parity as spanned parts and their bits as elements.
//!
//! A transfer is recognised by its request's start, stop and park bits and its parity, outside
//! the line resets and JTAG-to-SWD selection sequences, which are masked. Recognition looks a
//! bounded number of rises ahead, so a transfer is emitted once the rises after it are held, or at
//! a gap or the end of input, where what the lookahead holds is framed before the rest is dropped.
//!
//! The turnaround is a setting, `turnaround`, one to four cycles as DLCR sets it, fixed for the
//! run: a DLCR write that changes it is not followed, and the transfers after it are framed with
//! the turnaround set. Not decoded: the data phase that follows a WAIT or FAULT acknowledge when
//! ORUNDETECT is set, which is framed as a transfer without data, its data cycles left to the
//! search for the next request.

use escapement_tickwave_decode::{
    Cause, Decoder, Definition, Element, ElementType, Exchange, Factory, FieldDecl, FieldId,
    FieldType, Id, KindDecl, KindId, Label, LinkDecl, LinkId, LinkTo, Observation, Observed, Out,
    Pause, Ref, Request, Role, Schema, SeqId, Setting, SettingDecl, SettingId, Span, Status, Tick,
    Value, field, name, seq,
};
use std::{collections::VecDeque, sync::Arc};

pub const SWCLK: u8 = 0;
pub const SWDIO: u8 = 1;

pub const TRANSFER: KindId = KindId(0);

pub const PORT: FieldId = FieldId(0);
pub const DIR: FieldId = FieldId(1);
pub const A: FieldId = FieldId(2);
pub const REQUEST: FieldId = FieldId(3);
pub const TRN: FieldId = FieldId(4);
pub const ACK: FieldId = FieldId(5);
pub const TRN2: FieldId = FieldId(6);
pub const DATA: FieldId = FieldId(7);
pub const PARITY: FieldId = FieldId(8);

pub const REQUEST_BITS: SeqId = SeqId(0);
pub const ACK_BITS: SeqId = SeqId(1);
pub const DATA_BITS: SeqId = SeqId(2);
pub const PARITY_BITS: SeqId = SeqId(3);

pub const RETRY_OF: LinkId = LinkId(0);

pub const PROBE: Role = Role(0);
pub const TARGET: Role = Role(1);
pub const NEITHER: Role = Role(2);

/// `PORT`'s names.
pub const AP: u16 = 0;
pub const DP: u16 = 1;
/// `DIR`'s names.
pub const READ: u16 = 0;
pub const WRITE: u16 = 1;
/// `PARITY`'s names: whether the data's parity bit matched it.
pub const PARITY_OK: u16 = 0;
pub const PARITY_BAD: u16 = 1;
/// `ACK`'s names, in this order.
pub const ACKS: [&str; 5] = ["OK", "WAIT", "FAULT", "no response", "invalid"];
/// A transfer framed whose rises the decoder no longer held: a fault in the decoder, never in
/// the capture, closed incomplete so it shows rather than going missing.
pub const DROPPED: Cause = Cause(0);

/// A transfer's fields, in the order `PORT` to `PARITY` index them: a decoder whose transfers ADIv5
/// reads as it reads SWD's declares these.
pub fn transfer_fields() -> Vec<FieldDecl> {
    vec![
        field("port", FieldType::Name(vec![name("AP"), name("DP")])),
        field("dir", FieldType::Name(vec![name("read"), name("write")])),
        field("a", FieldType::Word { bits: 4 }),
        field("request", FieldType::Word { bits: 8 }),
        field("trn", FieldType::Phase),
        field("ack", FieldType::Name(ACKS.map(name).to_vec())),
        field("trn2", FieldType::Phase),
        field("data", FieldType::Word { bits: 32 }),
        field("parity", FieldType::Name(vec![name("ok"), name("bad")])).with_short("P"),
    ]
}

/// A transfer's labels, fullest first.
pub fn transfer_labels() -> Vec<Label> {
    let head = || {
        Label::default()
            .field(DIR)
            .text(" ")
            .field(PORT)
            .text(" ")
            .field(A)
            .text(" ")
            .field(ACK)
    };
    vec![
        head().text(" ").field(DATA),
        head(),
        Label::default().field(PORT),
    ]
}

pub fn schema() -> Schema {
    let bit = || ElementType::Bit;
    Schema::new(vec![
        KindDecl::new("transfer")
            .with_fields(transfer_fields())
            .with_sequences(vec![
                seq("request", bit()),
                seq("ack", bit()),
                seq("data", bit()),
                seq("parity", bit()),
            ])
            .with_links(vec![LinkDecl::new(
                "retry of",
                vec![LinkTo::Own(name("transfer"))],
            )])
            .with_labels(transfer_labels()),
    ])
    .with_roles(["probe", "target", "neither"])
    .with_causes(["the decoder no longer held the clock rises the transfer spans"])
}

/// The turnaround, in cycles.
pub const TURNAROUND: SettingId = SettingId(0);

/// The turnaround setting, as a binding gives it.
pub fn turnaround(cycles: u8) -> (&'static str, Setting) {
    ("turnaround", Setting::Int(cycles.into()))
}

pub fn definition() -> Arc<Definition> {
    Arc::new(
        Definition::new("swd", 1, schema())
            .with_ports(["SWCLK", "SWDIO"])
            .with_settings(vec![SettingDecl::int("turnaround", 1..=4, 1)]),
    )
}

/// Rises the recogniser holds past a candidate before framing it, and the most it holds.
const LOOKAHEAD: usize = 160;
const HELD: usize = LOOKAHEAD + 52;

/// A transfer as the recogniser frames it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub anchor: Tick,
    pub ap: bool,
    pub read: bool,
    pub address: u8,
    pub ack: u8,
    pub word: Option<u32>,
    pub parity: Option<bool>,
    /// Its turnaround, in cycles.
    pub turnaround: usize,
}

impl Transfer {
    /// The rises it spans: an OK transfer carries a data phase.
    pub fn rises(&self) -> usize {
        rises(self.ack, self.turnaround)
    }
}

fn rises(ack: u8, turnaround: usize) -> usize {
    if ack == 1 {
        44 + 2 * turnaround
    } else {
        11 + 2 * turnaround
    }
}

/// A setting a decoder cannot run with.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BadSetting {
    #[error("a turnaround of {0} cycles: SWD's is 1 to 4")]
    Turnaround(u8),
}

#[derive(Clone, Copy, Debug)]
struct Bit {
    at: Tick,
    value: bool,
    run: u8,
    prior_run: u8,
    masked: bool,
}

/// Transfers framed from bits sampled at the clock's rises.
#[derive(Clone, Debug)]
pub struct Recogniser {
    turnaround: usize,
    bits: VecDeque<Bit>,
    /// The last 128 bits pushed, the newest in the top bit.
    recent: u128,
    high: u8,
    after: usize,
    out: VecDeque<Transfer>,
    /// How often each masked sequence was seen: line reset, JTAG-to-SWD, and two selection
    /// sequences.
    pub masks: [u64; 4],
}

/// Masked sequences: the bits, their length, how many bits before and after they mask, and
/// whether they follow a line reset.
const MASKED: [(u128, usize, usize, usize, bool); 4] = [
    (0x19BC0EA2E3DDAFE986852D956209F392, 128, 8, 12, false),
    (0x33BBBBBA, 31, 5, 0, false),
    (0xE3BC, 16, 0, 0, true),
    (0xE79E, 16, 0, 0, true),
];

impl Recogniser {
    /// A recogniser for a turnaround of `cycles`, 1 to 4.
    pub fn new(cycles: u8) -> Result<Self, BadSetting> {
        if !(1..=4).contains(&cycles) {
            return Err(BadSetting::Turnaround(cycles));
        }
        Ok(Self::of(cycles))
    }

    fn of(cycles: u8) -> Self {
        Self {
            turnaround: usize::from(cycles),
            bits: VecDeque::new(),
            recent: 0,
            high: 0,
            after: 0,
            out: VecDeque::new(),
            masks: [0; 4],
        }
    }

    /// A gap in the input: what the lookahead holds is framed as at the end of input, then the
    /// rest is dropped. A transfer the gap cuts is lost with it.
    pub fn gap(&mut self) {
        self.pump(true);
        self.bits.clear();
        self.high = 0;
        self.after = 0;
    }

    pub fn push(&mut self, at: Tick, value: bool) {
        let prior_run = self.high;
        self.high = if value {
            self.high.saturating_add(1)
        } else {
            0
        };
        let masked = self.after > 0;
        self.after = self.after.saturating_sub(1);
        self.bits.push_back(Bit {
            at,
            value,
            run: self.high,
            prior_run,
            masked,
        });
        self.recent = (self.recent >> 1) | (u128::from(value) << 127);
        let n = self.bits.len();
        for (kind, (pattern, len, before, after, reset)) in MASKED.into_iter().enumerate() {
            // The deque holds the latest pushes, so where it holds `len` they are `recent`'s top.
            if n < len {
                continue;
            }
            let start = n - len;
            if reset && self.bits[start].prior_run < 50 {
                continue;
            }
            let last = self.recent >> (128 - len);
            let mask = u128::MAX >> (128 - len);
            if last & mask == pattern {
                self.masks[kind] += 1;
                for b in self.bits.range_mut(start.saturating_sub(before)..n) {
                    b.masked = true;
                }
                self.after = self.after.max(after);
            }
        }
        self.pump(false);
    }

    /// Frames what it holds, at the end of input.
    pub fn finish(&mut self) {
        self.pump(true);
    }

    pub fn poll(&mut self) -> Option<Transfer> {
        self.out.pop_front()
    }

    /// The rises it holds.
    pub fn retained(&self) -> usize {
        self.bits.len()
    }

    fn pump(&mut self, flush: bool) {
        loop {
            if !flush && self.bits.len() < HELD {
                return;
            }
            let trn = self.turnaround;
            let ack_at = 8 + trn;
            let header = ack_at + 3;
            if self.bits.len() < header {
                if flush {
                    self.bits.clear();
                }
                return;
            }
            let b = &self.bits;
            let valid = b[0].value
                && !b[6].value
                && b[7].value
                && (b[1].value ^ b[2].value ^ b[3].value ^ b[4].value) == b[5].value
                && (0..8).all(|i| b[i].run < 50)
                && (0..header).all(|i| !b[i].masked);
            if !valid {
                self.bits.pop_front();
                continue;
            }
            let ack = (0..3).fold(0, |v, k| v | (u8::from(b[ack_at + k].value) << k));
            let needed = rises(ack, trn);
            if b.len() < needed {
                if flush {
                    self.bits.pop_front();
                    continue;
                }
                return;
            }
            let read = b[2].value;
            let data_at = header + if read { 0 } else { trn };
            let word = (ack == 1)
                .then(|| (0..32).fold(0u32, |v, k| v | (u32::from(b[data_at + k].value) << k)));
            let parity = word.map(|word| (word.count_ones() % 2 == 1) == b[data_at + 32].value);
            self.out.push_back(Transfer {
                anchor: b[0].at,
                ap: b[1].value,
                read,
                address: (u8::from(b[3].value) | u8::from(b[4].value) << 1) * 4,
                ack,
                word,
                parity,
                turnaround: trn,
            });
            self.bits.drain(..needed);
            if !flush {
                return;
            }
        }
    }
}

/// The earliest a transfer whose first rise is `first` can start: the midpoint with the rise
/// before it, or `floor` (the start of input or a gap's end) where none was seen since.
pub fn start_bound(prev: Option<Tick>, floor: Tick, first: Tick) -> Tick {
    prev.map_or(floor, |q| q + (first - q) / 2)
}

/// Emits transfers as items: their parts spanned by the cells between rises, and their bits.
#[derive(Clone, Debug, Default)]
pub struct Track {
    last_wait: Option<(Id, u8)>,
    /// Transfers whose first cell `start_bound` moved later than the mirrored half-period.
    pub clamped: u64,
    /// A transfer's cell bounds and a part's elements, kept to reuse.
    cells: Vec<Tick>,
    elements: Vec<Element>,
}

impl Track {
    /// Emits transfer `t`. `rises` and `levels` start at its anchor and hold its clocks. `prev` is
    /// the rise before the anchor, if one was seen since `floor`. The first cell mirrors the next
    /// half-period but never starts before `start_bound`: it must not depend on later rises
    /// alone, or no start frontier could bound where the next transfer starts.
    pub fn emit(
        &mut self,
        t: &Transfer,
        prev: Option<Tick>,
        floor: Tick,
        rises: &[Tick],
        levels: &[bool],
        out: &mut Out,
    ) {
        let n = t.rises();
        if rises.len() < n || levels.len() < n {
            self.dropped(t.anchor, out);
            return;
        }
        let p = &rises[..n];
        let mut c = std::mem::take(&mut self.cells);
        c.clear();
        c.resize(n + 1, 0);
        for j in 1..n {
            c[j] = p[j - 1] + (p[j] - p[j - 1]) / 2;
        }
        let mirrored = p[0] - (c[1] - p[0]);
        let bound = start_bound(prev, floor, p[0]);
        if bound > mirrored {
            self.clamped += 1;
        }
        c[0] = mirrored.max(bound);
        c[n] = p[n - 1] + (p[n - 1] - c[n - 1]);
        let span = |a: usize, b: usize| Span {
            start: c[a],
            end: c[b],
        };
        let mut buf = std::mem::take(&mut self.elements);
        let mut bits = |out: &mut Out, id: Id, seq: SeqId, a: usize, b: usize| {
            buf.clear();
            buf.extend((a..b).map(|j| {
                Element::new(u8::from(levels[j]), Observation::Wire(p[j]), span(j, j + 1))
            }));
            out.extend(id, seq, 0, &buf);
        };
        let request = (0..8).fold(0u64, |v, j| v | (u64::from(levels[j]) << j));
        let id = Id::new(TRANSFER, t.anchor);
        out.open(id, None, c[0]);
        out.set(id, PORT, Value::Name(if t.ap { AP } else { DP }));
        out.set(id, DIR, Value::Name(if t.read { READ } else { WRITE }));
        let a = u64::from(t.address);
        out.set(id, A, Value::Word { value: a, bits: 4 });
        let word = |value, bits| Value::Word { value, bits };
        out.part(id, REQUEST, word(request, 8), span(0, 8), Some(PROBE));
        bits(out, id, REQUEST_BITS, 0, 8);
        let tr = t.turnaround;
        let ack_at = 8 + tr;
        // The second turnaround, before a write's data or after a read's parity.
        let trn2 = if t.ack == 1 && t.read {
            44 + tr
        } else {
            ack_at + 3
        };
        out.part(id, TRN, Value::Phase, span(8, ack_at), Some(NEITHER));
        let ack = match t.ack {
            1 => 0,
            2 => 1,
            4 => 2,
            7 => 3,
            _ => 4,
        };
        out.part(
            id,
            ACK,
            Value::Name(ack),
            span(ack_at, ack_at + 3),
            Some(TARGET),
        );
        bits(out, id, ACK_BITS, ack_at, ack_at + 3);
        let turn2 = |out: &mut Out| {
            out.part(id, TRN2, Value::Phase, span(trn2, trn2 + tr), Some(NEITHER));
        };
        if let (1, Some(w)) = (t.ack, t.word) {
            let at = if t.read { ack_at + 3 } else { ack_at + 3 + tr };
            let driver = Some(if t.read { TARGET } else { PROBE });
            if !t.read {
                turn2(out);
            }
            let data = word(u64::from(w), 32);
            out.part(id, DATA, data, span(at, at + 32), driver);
            bits(out, id, DATA_BITS, at, at + 32);
            let ok = if t.parity == Some(true) {
                PARITY_OK
            } else {
                PARITY_BAD
            };
            out.part(id, PARITY, Value::Name(ok), span(at + 32, at + 33), driver);
            bits(out, id, PARITY_BITS, at + 32, at + 33);
            if t.read {
                turn2(out);
            }
        } else {
            turn2(out);
        }
        if let Some((wait, req)) = self.last_wait.take()
            && u64::from(req) == request
        {
            out.link(id, RETRY_OF, Ref::Own(wait));
        }
        if t.ack == 2 {
            self.last_wait = Some((id, request as u8));
        }
        out.close(id, c[n], Status::Complete);
        self.elements = buf;
        self.cells = c;
    }

    /// Closes the transfer framed at `anchor` incomplete, its rises no longer held: it breaks
    /// any retry chain it was in.
    pub fn dropped(&mut self, anchor: Tick, out: &mut Out) {
        self.last_wait = None;
        let id = Id::new(TRANSFER, anchor);
        out.open(id, None, anchor);
        out.close(id, anchor + 1, Status::Incomplete(DROPPED));
    }
}

/// SWD under the host: SWCLK and SWDIO in windows of `window` ticks, a window a turn. SWDIO is
/// sampled at each SWCLK rise: an SWDIO change at the rise's own tick counts after it. A stretch
/// where either line is unknown is lost input.
#[derive(Clone, Debug)]
pub struct Swd {
    recogniser: Recogniser,
    track: Track,
    /// The rises the recogniser holds, and up to a transfer's worth before them.
    ring: VecDeque<(Tick, bool)>,
    next: Tick,
    window: Tick,
    eof: Option<Tick>,
    /// Where the rises in `ring` start being continuous: the run's start or a gap's end.
    floor: Tick,
    /// Each line's level, None where it is not known.
    clock: Option<bool>,
    data: Option<bool>,
    /// SWDIO's level before its last change, and where that change was.
    data_before: Option<(Tick, Option<bool>)>,
    /// Where input became unknown, while it is.
    gap: Option<Tick>,
    /// A framed transfer's rises and levels, kept to reuse.
    held: (Vec<Tick>, Vec<bool>),
}

impl Swd {
    /// SWD from `local` (None: the recording's start), in windows of `window` ticks, with a
    /// turnaround of `turnaround` cycles.
    pub fn new(local: Option<Tick>, window: Tick, turnaround: u8) -> Result<Self, BadSetting> {
        Ok(Self::with(local, window, Recogniser::new(turnaround)?))
    }

    fn with(local: Option<Tick>, window: Tick, recogniser: Recogniser) -> Self {
        let at = local.unwrap_or(0);
        Self {
            recogniser,
            track: Track::default(),
            ring: VecDeque::new(),
            next: at,
            window: window.max(1),
            eof: None,
            floor: at,
            clock: None,
            data: None,
            data_before: None,
            gap: None,
            held: (Vec::new(), Vec::new()),
        }
    }

    /// Makes runs of SWD in windows of `window` ticks, with the turnaround bound.
    pub fn factory(window: Tick) -> Factory {
        Box::new(move |b, local| {
            // The host checked the turnaround against its declaration, 1 to 4.
            let cycles = b.int(TURNAROUND).map_or(1, |t| t.clamp(1, 4) as u8);
            Box::new(Self::with(local, window, Recogniser::of(cycles)))
        })
    }

    pub fn masks(&self) -> [u64; 4] {
        self.recogniser.masks
    }

    /// Where the next transfer can start at the earliest: at the first cell of the oldest rise the
    /// lookahead still holds, or of a rise still to come, bounded as `Track::emit` bounds it.
    fn frontier(&self) -> Tick {
        let held = self.recogniser.retained();
        let k = self.ring.len() - held.min(self.ring.len());
        match (held, k) {
            (0, _) => self.ring.back().map_or(self.floor, |r| r.0),
            (_, 0) => self.floor,
            _ => start_bound(Some(self.ring[k - 1].0), self.floor, self.ring[k].0),
        }
    }

    fn drain(&mut self, out: &mut Out) {
        while let Some(t) = self.recogniser.poll() {
            let k = self.ring.partition_point(|&(at, _)| at < t.anchor);
            if self.ring.get(k).map(|r| r.0) != Some(t.anchor) {
                self.track.dropped(t.anchor, out);
                continue;
            }
            let n = t.rises();
            let (mut rises, mut levels) = std::mem::take(&mut self.held);
            rises.clear();
            levels.clear();
            for &(at, level) in self.ring.range(k..(k + n).min(self.ring.len())) {
                rises.push(at);
                levels.push(level);
            }
            let prev = k.checked_sub(1).map(|j| self.ring[j].0);
            self.track.emit(&t, prev, self.floor, &rises, &levels, out);
            self.held = (rises, levels);
        }
    }

    fn known(&self) -> bool {
        self.clock.is_some() && self.data.is_some()
    }

    /// Input became unknown at `at`.
    fn lose(&mut self, at: Tick, out: &mut Out) {
        if self.gap.is_none() {
            self.gap = Some(at);
            self.recogniser.gap();
            self.drain(out);
            self.ring.clear();
        }
    }

    /// Input became known again at `at`, after a gap.
    fn regain(&mut self, at: Tick, ex: &mut Exchange<'_>) {
        if let Some(from) = self.gap.take() {
            if from < at {
                ex.lost(from, at);
            }
            self.floor = at;
        }
    }

    fn rise(&mut self, at: Tick, out: &mut Out) {
        let level = match self.data_before {
            Some((t, before)) if t == at => before,
            _ => self.data,
        };
        let Some(level) = level else {
            return;
        };
        self.ring.push_back((at, level));
        self.recogniser.push(at, level);
        self.drain(out);
    }
}

impl Decoder for Swd {
    fn turn(&mut self, ex: &mut Exchange<'_>) -> Pause {
        if let Some(end) = self.eof {
            self.recogniser.finish();
            self.drain(&mut ex.out);
            if let Some(from) = self.gap.take()
                && from < end
            {
                ex.lost(from, end);
            }
            ex.finish(end);
            return Pause::Finished;
        }
        let Some(w) = ex.reply() else {
            ex.request(Request::Window {
                from: self.next,
                upto: self.next.saturating_add(self.window),
            });
            return Pause::Input;
        };
        for e in &w.events {
            match *e {
                Observed::Start { port, level } => {
                    let line = match port {
                        SWCLK => &mut self.clock,
                        SWDIO => &mut self.data,
                        _ => continue,
                    };
                    *line = level;
                    if level.is_none() {
                        self.lose(w.from, &mut ex.out);
                    }
                }
                Observed::Edge { at, port, level } => match port {
                    SWCLK => {
                        self.clock = Some(level);
                        if level && self.known() {
                            self.rise(at, &mut ex.out);
                        }
                    }
                    SWDIO => {
                        self.data_before = Some((at, self.data));
                        self.data = Some(level);
                    }
                    _ => {}
                },
                Observed::Known { at, port, level } => {
                    match port {
                        SWCLK => self.clock = Some(level),
                        SWDIO => self.data = Some(level),
                        _ => continue,
                    }
                    if self.known() {
                        self.regain(at, ex);
                    }
                }
                Observed::Unknown { at, port } => {
                    match port {
                        SWCLK => self.clock = None,
                        SWDIO => self.data = None,
                        _ => continue,
                    }
                    if w.end != Some(at) {
                        self.lose(at, &mut ex.out);
                    }
                }
                _ => {}
            }
        }
        // Whatever the lookahead still holds, plus the longest transfer it has already framed.
        let keep = self.recogniser.retained() + 64;
        while self.ring.len() > keep {
            self.ring.pop_front();
        }
        // In a gap, nothing new starts before the gap did.
        let frontier = self.gap.unwrap_or_else(|| self.frontier());
        ex.frontier(frontier.min(w.upto));
        if let Some(end) = w.end {
            self.eof = Some(end);
            return Pause::Work;
        }
        self.next = w.upto;
        ex.request(Request::Window {
            from: self.next,
            upto: self.next.saturating_add(self.window),
        });
        Pause::Input
    }

    fn save(&self) -> Option<Box<dyn Decoder>> {
        Some(Box::new(self.clone()))
    }

    fn heap_bytes(&self) -> usize {
        let r = &self.recogniser;
        crate::held::<Bit>(r.bits.capacity())
            + crate::held::<Transfer>(r.out.capacity())
            + crate::held::<Tick>(self.track.cells.capacity())
            + crate::held::<Element>(self.track.elements.capacity())
            + crate::held::<(Tick, bool)>(self.ring.capacity())
            + crate::held::<Tick>(self.held.0.capacity())
            + crate::held::<bool>(self.held.1.capacity())
    }
}
