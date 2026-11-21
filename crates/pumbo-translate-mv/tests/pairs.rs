//! The protocol pairs the translator builds tables for.

use pumbo_translate_mv::{Tables, VersionData};

fn stub(protocol: i32) -> VersionData {
    VersionData {
        protocol,
        ..Default::default()
    }
}

/// Tables exist exactly for clients 1.21 to 26.2 on a 26.3 server.
#[test]
fn tables_exist_only_for_older_clients_on_26_3() {
    for server in 760..=780 {
        for client in 760..=780 {
            let built = Tables::new(stub(client), stub(server)).is_ok();
            let expected = server == 777 && (767..=776).contains(&client);
            assert_eq!(built, expected, "client {client} on server {server}");
        }
    }
}
