#![no_main]
//! Packet decoders of the play phase(s), every version and direction.

use libfuzzer_sys::fuzz_target;
use pumbo_protocol::Phase;

fuzz_target!(|data: &[u8]| {
    pumbo_fuzz::packet(&[Phase::Play], data);
});
