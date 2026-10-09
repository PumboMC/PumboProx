#![no_main]
//! Text parsers: JSON, NBT, MiniMessage and legacy codes.

use libfuzzer_sys::fuzz_target;
use pumbo_nbt::{Limits, read_network};
use pumbo_text::{Component, TextFormat, parse_legacy, parse_mini};

fuzz_target!(|data: &[u8]| {
    let [mode, rest @ ..] = data else {
        return;
    };
    match mode % 4 {
        0 => {
            if let Ok(s) = std::str::from_utf8(rest)
                && let Ok(c) = Component::from_json(s)
            {
                let _ = c.to_json(TextFormat::V770);
                let _ = c.to_nbt(TextFormat::V767);
            }
        }
        1 => {
            let mut input = rest;
            if let Ok(Some(tag)) = read_network(&mut input, Limits::CLIENT)
                && let Ok(c) = Component::from_nbt(&tag)
            {
                let _ = c.to_nbt(TextFormat::V770);
                let _ = c.plain_text();
            }
        }
        2 => {
            if let Ok(s) = std::str::from_utf8(rest) {
                let _ = parse_mini(s).to_nbt(TextFormat::V770);
            }
        }
        _ => {
            if let Ok(s) = std::str::from_utf8(rest) {
                let _ = parse_legacy(s).to_json(TextFormat::V767);
            }
        }
    }
});
