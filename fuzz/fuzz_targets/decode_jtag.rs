//! JTAG through the host on wires a structured input builds: no panic, and no batch the
//! host refuses.
#![no_main]

use escapement_tickwave_fuzz::wires::{self, Case};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|case: Case| wires::jtag(&case));
