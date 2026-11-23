//! Status pings to servers: a handshake for status and a status request.

use std::time::Duration;

use bytes::BytesMut;
use pumbo_protocol::frame::{self, FrameConfig};
use pumbo_protocol::packets::status::Intention;
use pumbo_protocol::types::{MAX_STRING, Reader, WriteExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A server's answer to a status ping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// `version.protocol`.
    pub protocol: i32,
}

/// The protocol a ping asks with when it wants the server's own version.
pub const ASK_ANY: i32 = -1;

/// The longest status answer read; the JSON in it may have at most 32,767
/// characters (about 96 KiB), so this frame limit is never the one that
/// refuses an answer.
const MAX_ANSWER: usize = 256 * 1024;

/// The id of `intention`, `status_request` and `status_response` in every
/// protocol since 1.7: a ping needs no version tables.
const ID: i32 = 0;

/// One status ping to `address` within `deadline`: connect, handshake with
/// protocol `ask`, `status_request`, the answer.
pub async fn status(address: &str, ask: i32, deadline: Duration) -> Result<Answer, String> {
    tokio::time::timeout(deadline, ask_server(address, ask))
        .await
        .unwrap_or_else(|_| Err("timed out".into()))
}

async fn ask_server(address: &str, ask: i32) -> Result<Answer, String> {
    let (host, port) = crate::config::split_host_port(address).ok_or("bad address")?;
    let out = frames(ask, host, port)?;
    let mut sock = TcpStream::connect((host, port))
        .await
        .map_err(|e| format!("connect: {e}"))?;
    let answer = |e: &dyn std::fmt::Display| format!("answer: {e}");
    sock.write_all(&out).await.map_err(|e| answer(&e))?;
    let cfg = FrameConfig {
        max_frame: MAX_ANSWER,
        ..FrameConfig::from_backend()
    };
    let mut buf = BytesMut::new();
    let f = loop {
        if let Some(f) = frame::decode(&mut buf, &cfg).map_err(|e| answer(&e))? {
            break f;
        }
        if sock.read_buf(&mut buf).await.map_err(|e| answer(&e))? == 0 {
            return Err(answer(&"the server closed the connection"));
        }
    };
    if f.id != ID {
        return Err(answer(&format!("packet {} is no status response", f.id)));
    }
    let json = Reader::new(&f.payload)
        .string(MAX_STRING)
        .map_err(|e| answer(&e))?;
    let protocol = version(&json).ok_or("the answer names no version")?;
    Ok(Answer { protocol })
}

/// `version.protocol` of a status JSON, when it is a protocol number.
fn version(json: &str) -> Option<i32> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let p = v.get("version")?.get("protocol")?.as_i64()?;
    i32::try_from(p).ok().filter(|p| *p > 0)
}

/// `intention` (status, to `host`, protocol `ask`) and `status_request` as
/// uncompressed frames. Their layout is the same in every version.
pub(crate) fn frames(ask: i32, host: &str, port: u16) -> Result<BytesMut, String> {
    let mut intention = Vec::new();
    intention.put_varint(ask);
    intention.put_string(host, 255).map_err(|e| e.to_string())?;
    intention.put_u16(port);
    intention.put_varint(Intention::STATUS);
    let (cfg, mut out) = (FrameConfig::from_client(), BytesMut::new());
    for payload in [&intention[..], &[]] {
        frame::encode(&mut out, ID, payload, &cfg).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use pumbo_protocol::packets::status::{StatusRequest, StatusResponse};
    use pumbo_protocol::packets::{self, Ctx};
    use pumbo_protocol::{Direction, Phase, VersionModule};
    use tokio::net::TcpListener;

    use super::*;

    /// Holds every test to an end.
    const GUARD: Duration = Duration::from_secs(15);

    fn newest() -> Arc<dyn VersionModule> {
        let versions = crate::modules::versions().unwrap();
        let v = versions.newest().unwrap();
        versions.get(v).cloned().unwrap()
    }

    /// A status response frame with `json`.
    fn response(json: &str) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.put_string(json, 32_767).unwrap();
        let mut out = BytesMut::new();
        frame::encode(&mut out, ID, &payload, &FrameConfig::from_client()).unwrap();
        out.to_vec()
    }

    /// The ping writes and reads these packets by this id, whatever the
    /// version.
    #[test]
    fn the_status_packets_have_one_id_in_every_version() {
        let versions = crate::modules::versions().unwrap();
        for v in versions.versions() {
            let m = versions.get(v).unwrap();
            let (sb, cb) = (Direction::Serverbound, Direction::Clientbound);
            let (to, from) = (Ctx::new(&**m, sb), Ctx::new(&**m, cb));
            let ids = [
                packets::id_of::<Intention>(&to, Phase::Handshake, sb),
                packets::id_of::<StatusRequest>(&to, Phase::Status, sb),
                packets::id_of::<StatusResponse>(&from, Phase::Status, cb),
            ];
            assert_eq!(ids, [Some(ID); 3], "{v:?}");
        }
    }

    /// A server on a port of its own that answers every ping with `reply`
    /// (nothing when `None`) and keeps the connection until the other end
    /// closes it; with the protocols the handshakes asked with.
    async fn server(reply: Option<Vec<u8>>) -> (String, Arc<Mutex<Vec<i32>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let (reply, seen) = (reply.clone(), seen.clone());
                tokio::spawn(async move {
                    let mut buf = BytesMut::new();
                    let cfg = FrameConfig::from_client();
                    let m = newest();
                    let ctx = Ctx::new(&*m, Direction::Serverbound);
                    while let Ok(n) = sock.read_buf(&mut buf).await {
                        if n == 0 {
                            return;
                        }
                        if let Ok(Some(f)) = frame::decode(&mut buf, &cfg)
                            && let Ok(hs) = packets::decode::<Intention>(&f.payload, &ctx)
                        {
                            seen.lock().unwrap().push(hs.protocol);
                            if let Some(r) = &reply {
                                let _ = sock.write_all(r).await;
                            }
                        }
                    }
                });
            }
        });
        (address, asked)
    }

    async fn ping(address: &str, ask: i32, deadline: Duration) -> Result<Answer, String> {
        tokio::time::timeout(GUARD, status(address, ask, deadline))
            .await
            .expect("the ping outlived its deadline")
    }

    #[tokio::test]
    async fn the_answer_gives_the_servers_protocol() {
        let json = r#"{"version":{"name":"26.2","protocol":776},"players":{"max":20,"online":0},"description":{"text":"lobby"}}"#;
        let (address, _) = server(Some(response(json))).await;
        assert_eq!(
            ping(&address, ASK_ANY, GUARD).await,
            Ok(Answer { protocol: 776 })
        );
    }

    #[tokio::test]
    async fn the_ping_asks_with_the_given_protocol() {
        let json = r#"{"version":{"name":"26.3","protocol":777}}"#;
        let (address, asked) = server(Some(response(json))).await;
        for ask in [ASK_ANY, 776] {
            ping(&address, ask, GUARD).await.unwrap();
        }
        assert_eq!(*asked.lock().unwrap(), [-1, 776]);
    }

    #[tokio::test]
    async fn a_server_that_never_answers_ends_at_the_deadline() {
        let (address, _) = server(None).await;
        let r = ping(&address, ASK_ANY, Duration::from_millis(200)).await;
        assert_eq!(r, Err("timed out".into()));
    }

    #[tokio::test]
    async fn a_closed_port_is_an_error() {
        let address = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().to_string()
        };
        let err = ping(&address, ASK_ANY, GUARD).await.unwrap_err();
        assert!(err.starts_with("connect:"), "{err}");
    }

    #[tokio::test]
    async fn an_answer_without_a_version_is_refused() {
        for json in [
            "{}",
            r#"{"version":{"name":"x","protocol":0}}"#,
            r#"{"version":{"name":"x","protocol":1099511627776}}"#,
        ] {
            let (address, _) = server(Some(response(json))).await;
            let r = ping(&address, ASK_ANY, GUARD).await;
            assert_eq!(r, Err("the answer names no version".into()), "{json}");
        }
    }

    #[tokio::test]
    async fn an_answer_longer_than_the_limit_is_refused() {
        let len = 300 * 1024;
        let mut reply = Vec::new();
        reply.put_varint(i32::try_from(len).unwrap());
        reply.resize(reply.len() + len, 0);
        let (address, _) = server(Some(reply)).await;
        let err = ping(&address, ASK_ANY, GUARD).await.unwrap_err();
        assert!(err.contains("exceeds the limit of 262144"), "{err}");
    }
}
