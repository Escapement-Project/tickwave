//! The host facing played decoders that emit anything, alone and stacked, while it checkpoints,
//! derives and retires windows: no panic, and its state within the bounds its options set.
#![no_main]

use escapement_tickwave_fuzz::host::{self, HostCase};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|case: HostCase| host::run(&case));
