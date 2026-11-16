//! tickwave's recording format: the model of a recording's content, the file's records, footer
//! and paged index, the sans-io writer and readers, and the encoder, verifier, repairer and
//! re-cutter over them.

#[cfg(feature = "annotations")]
pub mod annotations;
#[cfg(feature = "encode")]
pub mod bundle;
pub mod codec;
pub mod compress;
pub mod decode;
#[cfg(feature = "edit")]
pub mod edit;
#[cfg(feature = "encode")]
pub mod encode;
pub mod hash;
pub mod header;
pub mod index;
pub mod mapping;
pub mod model;
pub mod overview;
pub mod parts;
pub mod reader;
pub mod record;
#[cfg(feature = "encode")]
pub mod recut;
#[cfg(feature = "encode")]
pub mod repair;
pub mod samples;
pub mod shared;
#[cfg(feature = "encode")]
pub mod snapshot;
pub mod times;
pub mod verify;
#[cfg(feature = "encode")]
pub mod writer;
