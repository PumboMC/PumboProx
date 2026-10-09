#![no_main]
//! Network NBT under client limits. What decodes must encode; one more
//! decode/encode cycle gives the same bytes (vanilla accepts non-canonical
//! modified UTF-8, which re-encodes canonically).

use libfuzzer_sys::fuzz_target;
use pumbo_nbt::{Limits, read_network, write_network};

fuzz_target!(|data: &[u8]| {
    let mut input = data;
    if let Ok(tag) = read_network(&mut input, Limits::CLIENT) {
        let mut out = Vec::new();
        write_network(&mut out, tag.as_ref()).expect("decoded NBT must encode");
        let again = read_network(&mut out.as_slice(), Limits::CLIENT).expect("must decode again");
        let mut out2 = Vec::new();
        write_network(&mut out2, again.as_ref()).expect("must encode again");
        assert_eq!(out2, out);
    }
});
