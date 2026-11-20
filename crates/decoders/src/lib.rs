//! tickwave's reference protocol decoders, written against `escapement-tickwave-decode`'s public
//! contract alone, as a decoder loaded at run time would be: SWD, with ADIv5 and MEM-AP stacked on
//! it, JTAG, with JTAG-DP stacked on it and ADIv5 on that, and cJTAG. With the `recipe` feature,
//! `recipe` binds them over a recording as `[[decode]]` tables name them.

pub mod adiv5;
pub mod cjtag;
pub mod jtag;
pub mod jtagdp;
#[cfg(feature = "recipe")]
pub mod recipe;
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

#[cfg(test)]
mod tests {
    use super::*;
    use escapement_tickwave_decode::InputDecl;

    /// A form that binds a decoder says what each of its ports, settings and upstream inputs is.
    #[test]
    fn every_port_setting_and_input_says_what_it_is() {
        let defs = [
            swd::definition(),
            jtag::definition(),
            cjtag::definition(),
            jtagdp::definition(),
            adiv5::adi_definition(),
            adiv5::mem_definition(),
        ];
        for d in defs {
            for p in &d.ports {
                assert!(p.about.is_some(), "{} port {}", d.name, p.name);
            }
            for s in &d.settings {
                assert!(s.about.is_some(), "{} setting {}", d.name, s.name);
            }
            for i in &d.inputs {
                if let InputDecl::Upstream(u) = i {
                    assert!(u.about.is_some(), "{} input {}", d.name, u.port);
                }
            }
        }
    }
}
