//! tickwave's protocol decoder contract and its host.
//!
//! A decoder is a [`Definition`] (its name, its major version, the contract version it targets and
//! its [`Schema`]) and a [`Decoder`] that the [`Host`] runs a turn at a time. A turn takes the
//! input that is ready, a window of wire observations or deliveries from the analysis it is
//! stacked on, and emits [`Op`]s: items of declared kinds opened, given fields and sequences,
//! linked and closed. The host checks every operation against the schema and the item lifecycle,
//! admits a turn's output whole or refuses it whole, keeps whole items only where a view asked for
//! them, and fans output out to the analyses stacked on it.
//!
//! The decoder-facing contract (the definition, the schema, requests and replies, the operations
//! and the errors a decoder sees) is versioned on its own, apart from how the host schedules,
//! stores and checkpoints. A host refuses a decoder that targets a contract it does not support
//! when the decoder is added, never mid-run.
//!
//! Every rule a decoder can break comes back as a [`Refusal`] or a [`BindError`], never a panic:
//! a decoder loaded at run time is untrusted input, as a file is to a reader.

mod host;
mod output;
mod schema;
mod store;
#[cfg(feature = "wire")]
pub mod wire;

pub use host::{
    AnalysisId, Batch, BindError, Checkpoint, CheckpointError, Completed, Decoder, Delivery,
    Exchange, Factory, Host, HostOptions, InputSpec, Keeping, Kept, LogEntry, Mode, Observed,
    Pause, Piece, Refusal, Reply, Request, RequestId, RunId, Seek, Source, SourceError,
    Subscription, Turn, Window,
};
pub use output::{
    Cause, Element, FieldId, Id, KindId, LinkId, MAX_BITS, MAX_CHUNK, MAX_FIELDS, MAX_LINKS,
    MAX_TEXT, Need, Observation, Op, Out, Reason, Ref, Role, SeqId, Span, Status, Tick, Value,
};
pub use schema::{
    CONTRACT, Contract, Definition, ElementType, FieldDecl, FieldType, KindDecl, LinkDecl, LinkTo,
    MAX_FIELD_NAMES, MAX_KINDS, MAX_LINK_ROLES, MAX_LINK_TARGETS, MAX_LIST, MAX_NAME, MAX_PHRASE,
    MAX_ROLES, MAX_SEQUENCES, Mismatch, Name, Schema, SchemaError, SeqDecl, field, name, seq,
};
pub use store::{Counts, Field, Item, Refused, Store};

/// Words folded into one value that differs where they do, for keys compared within one build.
pub fn fold(words: impl IntoIterator<Item = u64>) -> u64 {
    words.into_iter().fold(0xcbf2_9ce4_8422_2325, |h, w| {
        (h ^ w).wrapping_mul(0x100_0000_01b3).rotate_left(29)
    })
}
