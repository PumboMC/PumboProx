//! `bungeecord:main` for old backend plugins (§2.8, decision §9 point 8):
//! `Connect`, `ConnectOther`, `IP`, `UUID`, `GetServer`, `GetServers`,
//! `PlayerCount`, `PlayerList`. Formats as in Velocity's
//! `BungeeCordMessageResponder` (GPL-3.0): Java `writeUTF` strings, big-endian
//! ints, lists joined with ", ", UUIDs without dashes.

use std::net::SocketAddr;

use uuid::Uuid;

use crate::server::{Proxy, Runtime, SessionCmd};

pub const CHANNEL: &str = "bungeecord:main";
/// Name used before namespaced channels; Paper still maps it.
pub const LEGACY_CHANNEL: &str = "BungeeCord";

pub fn is_channel(channel: &str) -> bool {
    channel == CHANNEL || channel == LEGACY_CHANNEL
}

/// The player whose backend sent the message.
#[derive(Debug, Clone, Copy)]
pub struct Sender<'a> {
    pub id: Uuid,
    pub address: SocketAddr,
    pub server: Option<&'a str>,
}

/// What the session does with a message.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Send this payload back to the backend on [`CHANNEL`].
    Reply(Vec<u8>),
    /// Move the sender to a server.
    Connect(String),
    None,
}

fn read_utf(data: &mut &[u8]) -> Option<String> {
    let (len, rest) = data.split_first_chunk::<2>()?;
    let len = usize::from(u16::from_be_bytes(*len));
    let (s, rest) = rest.split_at_checked(len)?;
    *data = rest;
    pumbo_nbt::mutf8::decode(s)
}

fn put_utf(out: &mut Vec<u8>, s: &str) {
    let bytes = pumbo_nbt::mutf8::encode(s);
    // writeUTF refuses more than 65535 bytes; cut lists that long (no player
    // list gets near it in practice).
    let bytes = bytes.get(..usize::from(u16::MAX)).unwrap_or(&bytes);
    out.extend_from_slice(&u16::try_from(bytes.len()).unwrap_or(u16::MAX).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn reply(parts: &[&str], int: Option<i32>) -> Action {
    let mut out = Vec::new();
    for p in parts {
        put_utf(&mut out, p);
    }
    if let Some(i) = int {
        out.extend_from_slice(&i.to_be_bytes());
    }
    Action::Reply(out)
}

/// Handles one message from a backend.
pub fn handle(proxy: &Proxy, rt: &Runtime, sender: &Sender<'_>, mut data: &[u8]) -> Action {
    let cfg = &rt.config.bungeecord_channel;
    let Some(sub) = read_utf(&mut data) else {
        return Action::None;
    };
    if !cfg.enabled || !cfg.subchannels.contains(&sub) {
        return Action::None;
    }
    let server_named = |name: &str| rt.backends.iter().find(|b| b.name == name);
    let players = || proxy.players();
    match sub.as_str() {
        "Connect" => match read_utf(&mut data).as_deref().and_then(server_named) {
            Some(b) => Action::Connect(b.name.clone()),
            None => Action::None,
        },
        "ConnectOther" => {
            let (Some(who), Some(to)) = (read_utf(&mut data), read_utf(&mut data)) else {
                return Action::None;
            };
            if let (Some(p), Some(b)) = (proxy.find_player(&who), server_named(&to)) {
                proxy.send_to(
                    p.id,
                    SessionCmd::Connect {
                        server: b.name.clone(),
                        quiet: true,
                    },
                );
            }
            Action::None
        }
        "IP" => reply(
            &["IP", &sender.address.ip().to_string()],
            Some(i32::from(sender.address.port())),
        ),
        "UUID" => reply(&["UUID", &sender.id.simple().to_string()], None),
        "GetServer" => match sender.server {
            Some(s) => reply(&["GetServer", s], None),
            None => Action::None,
        },
        "GetServers" => {
            let names: Vec<&str> = rt.backends.iter().map(|b| b.name.as_str()).collect();
            reply(&["GetServers", &names.join(", ")], None)
        }
        "PlayerCount" | "PlayerList" => {
            let Some(target) = read_utf(&mut data) else {
                return Action::None;
            };
            let on: Vec<String> = if target == "ALL" {
                players().into_iter().map(|p| p.name).collect()
            } else if server_named(&target).is_some() {
                players()
                    .into_iter()
                    .filter(|p| p.server.as_deref() == Some(target.as_str()))
                    .map(|p| p.name)
                    .collect()
            } else {
                return Action::None;
            };
            if sub == "PlayerCount" {
                reply(
                    &["PlayerCount", &target],
                    Some(i32::try_from(on.len()).unwrap_or(i32::MAX)),
                )
            } else {
                reply(&["PlayerList", &target, &on.join(", ")], None)
            }
        }
        _ => Action::None,
    }
}

/// A request as a backend plugin writes it (for tests).
pub fn request(parts: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in parts {
        put_utf(&mut out, p);
    }
    out
}

/// Strings of a reply (for tests); a trailing int is returned separately.
pub fn parse_reply(mut data: &[u8], strings: usize) -> Option<(Vec<String>, Option<i32>)> {
    let mut out = Vec::new();
    for _ in 0..strings {
        out.push(read_utf(&mut data)?);
    }
    let int = data.first_chunk::<4>().map(|b| i32::from_be_bytes(*b));
    Some((out, int))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf_round_trip() {
        let data = request(&["PlayerList", "ALL"]);
        assert_eq!(data.get(..2), Some(&[0u8, 10][..]));
        let (parts, int) = parse_reply(&data, 2).unwrap_or_default();
        assert_eq!(parts, ["PlayerList", "ALL"]);
        assert_eq!(int, None);
        let mut short: &[u8] = &[0, 5, b'a'];
        assert_eq!(read_utf(&mut short), None);
    }
}
