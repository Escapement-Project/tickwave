//! chrono's recording format: the model of a recording's content, the file's head and blocks,
//! the encoder and the decoder.

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
