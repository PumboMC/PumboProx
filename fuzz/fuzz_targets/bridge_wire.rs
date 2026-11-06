#![no_main]
//! Bridge frames as either side reads them, before and after the handshake:
//! any input must decode without a panic or a hang. A keyed codec rejects
//! frames with a wrong tag, so the fuzzer mostly reaches the frames before
//! the handshake, which anyone who can connect may send.

use libfuzzer_sys::fuzz_target;
use pumbo_bridge_proto::wire::{Codec, Side};

fuzz_target!(|data: &[u8]| {
    let [mode, rest @ ..] = data else {
        return;
    };
    let mut buf = rest;
    let mut codec = Codec::new(if mode & 1 == 0 {
        Side::Proxy
    } else {
        Side::Bridge
    });
    if mode & 2 != 0 {
        codec.set_key([0xab; 32]);
    }
    while let Ok(Some((_, n))) = codec.decode(buf) {
        match buf.get(n..) {
            Some(rest) if n > 0 => buf = rest,
            _ => break,
        }
    }
});
