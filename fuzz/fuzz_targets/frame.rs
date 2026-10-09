#![no_main]
//! Frame decoder with and without compression, from both sides' limits.

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use pumbo_protocol::frame::{FrameConfig, decode};

fuzz_target!(|data: &[u8]| {
    let [mode, rest @ ..] = data else {
        return;
    };
    let base = if mode & 1 == 0 {
        FrameConfig::from_client()
    } else {
        FrameConfig::from_backend()
    };
    let cfg = match mode >> 1 {
        0 => base,
        1 => base.with_threshold(0),
        2 => base.with_threshold(64),
        _ => base.with_threshold(256),
    };
    let mut buf = BytesMut::from(rest);
    while let Ok(Some(_)) = decode(&mut buf, &cfg) {}
});
