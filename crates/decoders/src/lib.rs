//! tickwave's reference protocol decoders, written against `escapement-tickwave-decode`'s public
//! contract alone, as a decoder loaded at run time would be: SWD, with ADIv5 and MEM-AP stacked on
//! it, JTAG, and cJTAG.

pub mod adiv5;
pub mod cjtag;
pub mod jtag;
pub mod swd;

/// The bytes a vector or queue has allocated, for a decoder's `heap_bytes`.
pub(crate) fn held<T>(capacity: usize) -> usize {
    capacity * std::mem::size_of::<T>()
}

/// The bytes a held window's events take.
pub(crate) fn window_bytes(w: &Option<(escapement_tickwave_decode::Window, usize)>) -> usize {
    w.as_ref().map_or(0, |(w, _)| {
        held::<escapement_tickwave_decode::Observed>(w.events.capacity())
    })
}
