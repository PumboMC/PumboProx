//! A status ping to a server reads no version tables. In its own process,
//! so nothing else parses tables meanwhile.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn a_status_ping_loads_no_tables() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 512];
        let _ = s.read(&mut buf).await;
        // status_response: packet 0 with the JSON as a string, both shorter
        // than 128 bytes, so every length is one byte.
        let json = br#"{"version":{"name":"26.3","protocol":777}}"#;
        let mut packet = vec![0, u8::try_from(json.len()).unwrap()];
        packet.extend_from_slice(json);
        let mut frame = vec![u8::try_from(packet.len()).unwrap()];
        frame.extend(packet);
        s.write_all(&frame).await.unwrap();
        let _ = s.read(&mut buf).await;
    });
    let guard = Duration::from_secs(15);
    let ask = pumbo_prox::ping::ASK_ANY;
    let answer = pumbo_prox::ping::status(&address, ask, guard).await;
    assert_eq!(answer.map(|a| a.protocol), Ok(777));
    let parsed: Vec<_> = pumbo_data::protocols()
        .filter(|p| pumbo_data::is_parsed(*p))
        .collect();
    assert_eq!(parsed, []);
}
