//! What a decoder emits. Kinds, fields, sequences, links, roles, reasons and causes are named by
//! their index in the decoder's schema, so an operation carries no strings of the schema's and an
//! id stays small and `Copy`.

/// A position on the analysis's timeline: its source's ticks, moved by the channel's offset.
pub type Tick = i64;

macro_rules! index {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u16);
    };
}
index!(
    /// A kind, by its index in the schema's kinds.
    KindId
);
index!(
    /// A field, by its index in its kind's fields.
    FieldId
);
index!(
    /// A sequence, by its index in its kind's sequences.
    SeqId
);
index!(
    /// A link role, by its index in its kind's links.
    LinkId
);
index!(
    /// A driver role, in phase 7d's sense, by its index in the schema's roles.
    Role
);
index!(
    /// Why a value is unknown, by its index in the schema's reasons.
    Reason
);
index!(
    /// What an unknown value needs to be known, by its index in the schema's needs.
    Need
);
index!(
    /// Why an item closed incomplete, by its index in the schema's causes.
    Cause
);

/// An item's identity within its analysis: an input anchor, its kind, and a discriminator for
/// items of one kind that share an anchor. It holds no execution order or batch number, so a rerun
/// gives the same item the same id. The anchor lies at or after the item's start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id {
    pub anchor: Tick,
    pub kind: KindId,
    pub disc: u32,
}

impl Id {
    pub const fn new(kind: KindId, anchor: Tick) -> Self {
        Self {
            anchor,
            kind,
            disc: 0,
        }
    }
}

/// An item of this analysis, or of the analysis it is stacked on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Ref {
    Own(Id),
    Upstream(Id),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: Tick,
    pub end: Tick,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Value {
    /// A spanned part that carries no value, such as a turnaround.
    Phase,
    Bool(bool),
    Word {
        value: u64,
        bits: u8,
    },
    /// An enumerated value, by its index in the field's names.
    Name(u16),
    /// Bounded text: a command's effect, a format's name.
    Text(String),
    /// A bounded bit string, the first shifted bit in the lowest bit of `bytes[0]`.
    Bits {
        bytes: Vec<u8>,
        bits: u32,
    },
    Unknown {
        reason: Reason,
        needs: Option<Need>,
    },
}

/// The input an element was decoded from: the wire observation that sampled it, or for a decoder
/// stacked on another, the upstream element, so a view can follow it down to the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Observation {
    Wire(Tick),
    Upstream { id: Id, seq: SeqId, index: u32 },
}

/// One member of a sequence: its value, the observation that defines it, and the cell a view
/// draws for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Element {
    pub value: u8,
    pub at: Observation,
    pub cell: Span,
}

impl Element {
    pub const fn new(value: u8, at: Observation, cell: Span) -> Self {
        Self { value, at, cell }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Status {
    Complete,
    Incomplete(Cause),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Op {
    Open {
        id: Id,
        parent: Option<Id>,
        start: Tick,
    },
    Set {
        id: Id,
        field: FieldId,
        value: Value,
        span: Option<Span>,
        driver: Option<Role>,
    },
    Extend {
        id: Id,
        seq: SeqId,
        offset: u32,
        elements: Vec<Element>,
    },
    Link {
        id: Id,
        link: LinkId,
        other: Ref,
    },
    Close {
        id: Id,
        end: Tick,
        status: Status,
    },
}

impl Op {
    /// The item the operation belongs to.
    pub fn id(&self) -> &Id {
        match self {
            Op::Open { id, .. }
            | Op::Set { id, .. }
            | Op::Extend { id, .. }
            | Op::Link { id, .. }
            | Op::Close { id, .. } => id,
        }
    }

    pub(crate) fn letter(&self) -> char {
        match self {
            Op::Open { .. } => 'O',
            Op::Set { .. } => 'S',
            Op::Extend { .. } => 'X',
            Op::Link { .. } => 'L',
            Op::Close { .. } => 'C',
        }
    }
}

/// The most fields an item holds.
pub const MAX_FIELDS: usize = 32;
/// The most links an item holds.
pub const MAX_LINKS: usize = 16;
/// The most elements one extend carries.
pub const MAX_CHUNK: usize = 64;
/// The longest bit string a field holds.
pub const MAX_BITS: u32 = 256;
/// The longest text a field holds, in bytes.
pub const MAX_TEXT: usize = 256;

/// A turn's output as the decoder writes it.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Out {
    pub ops: Vec<Op>,
}

impl Out {
    pub fn open(&mut self, id: Id, parent: Option<Id>, start: Tick) {
        self.ops.push(Op::Open { id, parent, start });
    }

    pub fn set(&mut self, id: Id, field: FieldId, value: Value) {
        self.ops.push(Op::Set {
            id,
            field,
            value,
            span: None,
            driver: None,
        });
    }

    /// A field with its own span, driven by `driver` where the decoder knows who drove it.
    pub fn part(&mut self, id: Id, field: FieldId, value: Value, span: Span, driver: Option<Role>) {
        self.ops.push(Op::Set {
            id,
            field,
            value,
            span: Some(span),
            driver,
        });
    }

    /// Elements of a sequence that is still growing, from element `offset` on, in chunks the host
    /// admits.
    pub fn extend(&mut self, id: Id, seq: SeqId, offset: u32, elements: &[Element]) {
        let mut offset = offset;
        for chunk in elements.chunks(MAX_CHUNK) {
            self.ops.push(Op::Extend {
                id,
                seq,
                offset,
                elements: chunk.to_vec(),
            });
            offset = offset.saturating_add(chunk.len() as u32);
        }
    }

    pub fn link(&mut self, id: Id, link: LinkId, other: Ref) {
        self.ops.push(Op::Link { id, link, other });
    }

    pub fn close(&mut self, id: Id, end: Tick, status: Status) {
        self.ops.push(Op::Close { id, end, status });
    }
}
