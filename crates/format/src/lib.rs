//! tickwave's recording format: the model of a recording's content, the file's records, footer
//! and index, the sans-io writer and readers, and the encoder, verifier and repairer over them.

#[cfg(feature = "annotations")]
pub mod annotations;
pub mod bundle;
pub mod codec;
pub mod decode;
#[cfg(feature = "encode")]
pub mod encode;
pub mod hash;
pub mod header;
pub mod model;
pub mod overview;
pub mod reader;
pub mod record;
#[cfg(feature = "encode")]
pub mod repair;
pub mod verify;
#[cfg(feature = "encode")]
pub mod writer;
