//! tickwave's reference protocol decoders, written against `escapement-tickwave-decode`'s public
//! contract alone, as a decoder loaded at run time would be: SWD, with ADIv5 and MEM-AP stacked on
//! it, JTAG, and cJTAG.

pub mod adiv5;
pub mod swd;
