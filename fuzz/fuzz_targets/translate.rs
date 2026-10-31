#![no_main]
//! Version translation of server packets for every older client, in
//! configuration or play: any input must translate without a panic or a hang.

use std::sync::{Arc, OnceLock};

use libfuzzer_sys::fuzz_target;
use pumbo_protocol::ProtocolVersion;
use pumbo_translate::multiversion::version_data;
use pumbo_translate_mv as mv;

fn tables() -> &'static [(Arc<mv::Tables>, i32)] {
    static TABLES: OnceLock<Vec<(Arc<mv::Tables>, i32)>> = OnceLock::new();
    TABLES.get_or_init(|| {
        let server = version_data(ProtocolVersion(mv::SERVER_PROTOCOL)).expect("server data");
        let finish = server.packets[0][0]
            .iter()
            .position(|n| n == "finish_configuration")
            .expect("finish_configuration") as i32;
        pumbo_data::protocols()
            .filter_map(|v| {
                let t = mv::Tables::new(version_data(v)?, server.clone()).ok()?;
                Some((Arc::new(t), finish))
            })
            .collect()
    })
}

// Input layout: client selector, flags (bit 0: client packet, bit 1: play), packet id, payload.
fuzz_target!(|data: &[u8]| {
    let [v, flags, id, payload @ ..] = data else {
        return;
    };
    let all = tables();
    let Some((t, finish)) = all.get(usize::from(*v) % all.len().max(1)) else {
        return;
    };
    let mut tr = mv::Translator::new(t.clone());
    let mut out = mv::Output::default();
    if flags & 2 != 0 {
        tr.to_client(*finish, &[], &mut out);
        out.clear();
    }
    if flags & 1 == 0 {
        tr.to_client(i32::from(*id), payload, &mut out);
    } else {
        tr.to_server(i32::from(*id), payload, &mut out);
    }
});
