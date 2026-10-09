//! Messages, frames and the handshake (spec §3.3–3.5, §4).
//!
//! Before the handshake a frame is `u32 length ‖ CBOR` (at most 4 KiB). After
//! it: `u32 length ‖ u64 seq ‖ CBOR ‖ 16 B tag`, the tag being the first 16
//! bytes of `HMAC-SHA256(Ks, direction ‖ seq ‖ CBOR)`. `seq` starts at 0 and
//! grows by one in each direction; a wrong tag or `seq` ends the session, so
//! a frame cannot be replayed within a session, and fresh nonces make every
//! session key new.
//!
//! Handshake with the bridge key `K` (32 bytes the proxy keeps in its key
//! file and prints with `pumbo proxy bridge key`):
//!
//! 1. B→P `hello {proto, bridge, pumpkin, mc, instance, nb}`
//! 2. P→B `challenge {np, proof = HMAC(K, "P"‖nb‖np‖instance)}`
//! 3. the bridge checks the proof before it sends anything else, then B→P
//!    `auth {proof = HMAC(K, "B"‖np‖nb‖instance)}`
//! 4. both switch to `Ks = HMAC(K, "S"‖nb‖np)`.

use std::collections::BTreeMap;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use crate::api::{GameMode, PermsMode, Pos, WorldInfo};

pub type Key = [u8; 32];
pub type Nonce = [u8; 32];
pub type Instance = [u8; 16];

/// Frame limits.
pub const MAX_PRE_AUTH: usize = 4 * 1024;
pub const MAX_FRAME: usize = 1024 * 1024 + 1024;
pub const TAG_LEN: usize = 16;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum Msg {
    Hello {
        proto: [u16; 2],
        bridge: String,
        pumpkin: String,
        mc: String,
        instance: Instance,
        nb: Nonce,
    },
    Challenge {
        np: Nonce,
        proof: Key,
    },
    Auth {
        proof: Key,
    },
    /// Right after `auth`: methods this bridge has on this Pumpkin, and the
    /// permission nodes of Pumpkin's built-in commands.
    Info {
        caps: Vec<String>,
        catalog: Vec<String>,
    },
    /// The bridge saw a pairing ping with this nonce (32 hex digits).
    Seen {
        nonce: String,
    },
    #[serde(rename_all = "kebab-case")]
    Welcome {
        server: String,
        #[serde(default)]
        groups: Vec<String>,
        stats_ms: u32,
        player_stats_ms: u32,
    },
    Sync {
        players: Vec<PlayerState>,
        worlds: Vec<WorldInfo>,
    },
    Cmd {
        id: u64,
        method: String,
        args: ciborium::Value,
    },
    Res {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<ciborium::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        err: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Ev {
        ev: Event,
    },
    StatsServer {
        tps: f32,
        mspt: f32,
        worlds: Vec<WorldInfo>,
    },
    StatsPlayer {
        players: Vec<PlayerStats>,
    },
    Ping,
    Pong,
    /// A message of a newer minor version: skipped.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
    pub player: Uuid,
    pub name: String,
    pub pos: Pos,
    pub gamemode: GameMode,
}

/// Changed fields only (and the player).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayerStats {
    pub player: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub food: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamemode: Option<GameMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<String>,
}

/// Events (spec §4.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Event {
    Join {
        player: Uuid,
        name: String,
        pos: Pos,
    },
    /// With the last position (e.g. `/back` after a server change).
    Leave {
        player: Uuid,
        pos: Pos,
    },
    Death {
        player: Uuid,
        pos: Pos,
        /// JSON text.
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        killer: Option<Uuid>,
    },
    Respawn {
        player: Uuid,
        pos: Pos,
    },
    /// Found by polling the player's dimension (not an event subscription).
    World {
        player: Uuid,
        from: String,
        to: String,
        pos: Pos,
    },
    PermsMode {
        mode: PermsMode,
    },
    /// Reserved for PumboGuard; not sent in 0.1.
    Denied {
        player: Uuid,
        flag: String,
        region: String,
        pos: Pos,
    },
    #[serde(other)]
    Unknown,
}

/// Payload of the bus topic `pumbo:bridge-event@1.0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusEvent {
    pub server: String,
    pub ev: Event,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    TooLarge(usize),
    Cbor(String),
    /// Wrong tag or sequence number: the session must end.
    BadTag,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLarge(n) => write!(f, "frame of {n} bytes is too large"),
            FrameError::Cbor(e) => write!(f, "bad message: {e}"),
            FrameError::BadTag => f.write_str("bad frame tag or sequence"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Which end of the connection this codec is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Proxy,
    Bridge,
}

impl Side {
    fn byte(self) -> u8 {
        match self {
            Side::Proxy => b'P',
            Side::Bridge => b'B',
        }
    }

    fn other(self) -> Side {
        match self {
            Side::Proxy => Side::Bridge,
            Side::Bridge => Side::Proxy,
        }
    }
}

/// Frames of one connection, without I/O.
#[derive(Clone)]
pub struct Codec {
    side: Side,
    key: Option<Key>,
    tx_seq: u64,
    rx_seq: u64,
}

impl std::fmt::Debug for Codec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The session key never goes to logs.
        f.debug_struct("Codec")
            .field("side", &self.side)
            .field("keyed", &self.key.is_some())
            .finish()
    }
}

fn mac(key: &Key, parts: &[&[u8]]) -> Hmac<Sha256> {
    // HMAC pads a key shorter than the block with zeros (RFC 2104), so this
    // is HMAC(key) without the fallible constructor.
    let mut block = [0u8; 64];
    for (d, s) in block.iter_mut().zip(key) {
        *d = *s;
    }
    let mut m = <Hmac<Sha256> as Mac>::new(&block.into());
    for p in parts {
        m.update(p);
    }
    m
}

fn full(key: &Key, parts: &[&[u8]]) -> [u8; 32] {
    mac(key, parts).finalize().into_bytes().into()
}

impl Codec {
    pub fn new(side: Side) -> Self {
        Codec {
            side,
            key: None,
            tx_seq: 0,
            rx_seq: 0,
        }
    }

    pub fn keyed(&self) -> bool {
        self.key.is_some()
    }

    /// Switches to authenticated frames; both counters start at 0.
    pub fn set_key(&mut self, ks: Key) {
        self.key = Some(ks);
        self.tx_seq = 0;
        self.rx_seq = 0;
    }

    pub fn encode(&mut self, m: &Msg) -> Result<Vec<u8>, FrameError> {
        let mut body = Vec::new();
        ciborium::into_writer(m, &mut body).map_err(|e| FrameError::Cbor(e.to_string()))?;
        let mut out = Vec::with_capacity(body.len() + 28);
        match self.key {
            None => {
                if body.len() > MAX_PRE_AUTH {
                    return Err(FrameError::TooLarge(body.len()));
                }
                out.extend_from_slice(&(body.len() as u32).to_be_bytes());
                out.extend_from_slice(&body);
            }
            Some(k) => {
                let len = 8 + body.len() + TAG_LEN;
                if len > MAX_FRAME {
                    return Err(FrameError::TooLarge(len));
                }
                let seq = self.tx_seq.to_be_bytes();
                let tag = full(&k, &[&[self.side.byte()], &seq, &body]);
                out.extend_from_slice(&(len as u32).to_be_bytes());
                out.extend_from_slice(&seq);
                out.extend_from_slice(&body);
                out.extend_from_slice(tag.get(..TAG_LEN).unwrap_or_default());
                self.tx_seq += 1;
            }
        }
        Ok(out)
    }

    /// Decodes one frame from the front of `buf`: `Ok(None)` until a whole
    /// frame is there, else the message and how many bytes it took.
    pub fn decode(&mut self, buf: &[u8]) -> Result<Option<(Msg, usize)>, FrameError> {
        let Some(head) = buf.first_chunk::<4>() else {
            return Ok(None);
        };
        let len = u32::from_be_bytes(*head) as usize;
        let max = if self.key.is_some() {
            MAX_FRAME
        } else {
            MAX_PRE_AUTH
        };
        if len > max {
            return Err(FrameError::TooLarge(len));
        }
        let Some(frame) = buf.get(4..4 + len) else {
            return Ok(None);
        };
        let body = match self.key {
            None => frame,
            Some(k) => {
                if len < 8 + TAG_LEN {
                    return Err(FrameError::BadTag);
                }
                let (seq, rest) = frame.split_at(8);
                let (body, tag) = rest.split_at(rest.len() - TAG_LEN);
                let expected = self.rx_seq.to_be_bytes();
                let m = mac(&k, &[&[self.side.other().byte()], &expected, body]);
                if seq != expected || m.verify_truncated_left(tag).is_err() {
                    return Err(FrameError::BadTag);
                }
                self.rx_seq += 1;
                body
            }
        };
        let msg = ciborium::from_reader(body).map_err(|e| FrameError::Cbor(e.to_string()))?;
        Ok(Some((msg, 4 + len)))
    }
}

/// `HMAC(K, "P"‖nb‖np‖instance)`: the proxy proves it has the key.
pub fn proxy_proof(k: &Key, nb: &Nonce, np: &Nonce, instance: &Instance) -> Key {
    full(k, &[b"P", nb, np, instance])
}

/// `HMAC(K, "B"‖np‖nb‖instance)`: the bridge proves it has the key.
pub fn bridge_proof(k: &Key, np: &Nonce, nb: &Nonce, instance: &Instance) -> Key {
    full(k, &[b"B", np, nb, instance])
}

/// Checks a proof in constant time.
pub fn check_proxy_proof(k: &Key, nb: &Nonce, np: &Nonce, instance: &Instance, got: &Key) -> bool {
    mac(k, &[b"P", nb, np, instance]).verify_slice(got).is_ok()
}

pub fn check_bridge_proof(k: &Key, np: &Nonce, nb: &Nonce, instance: &Instance, got: &Key) -> bool {
    mac(k, &[b"B", np, nb, instance]).verify_slice(got).is_ok()
}

/// `Ks = HMAC(K, "S"‖nb‖np)`.
pub fn session_key(k: &Key, nb: &Nonce, np: &Nonce) -> Key {
    full(k, &[b"S", nb, np])
}

/// The key as 64 lowercase hex digits.
pub fn key_hex(k: &Key) -> String {
    k.iter().map(|b| format!("{b:02x}")).collect()
}

/// A key from 64 hex digits (spaces around are ignored).
pub fn parse_key(s: &str) -> Option<Key> {
    let s = s.trim();
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(k)
}

/// The nonce part of a pairing host name, if it is one.
pub fn ping_nonce(hostname: &str) -> Option<&str> {
    let n = hostname.strip_prefix(crate::PING_PREFIX)?;
    (n.len() == 32 && n.bytes().all(|b| b.is_ascii_hexdigit())).then_some(n)
}

/// Encodes a value for `cmd.args` / `res.ok`.
pub fn value<T: Serialize>(v: &T) -> Result<ciborium::Value, String> {
    ciborium::Value::serialized(v).map_err(|e| e.to_string())
}

/// Decodes `cmd.args` / `res.ok`.
pub fn from_value<T: serde::de::DeserializeOwned>(v: &ciborium::Value) -> Result<T, String> {
    v.deserialized().map_err(|e| e.to_string())
}

/// `perm-set` difference: attachments to set and to remove so that `old`
/// becomes `new`.
pub fn perm_diff(
    old: &BTreeMap<String, bool>,
    new: &BTreeMap<String, bool>,
) -> (Vec<(String, bool)>, Vec<String>) {
    let set = new
        .iter()
        .filter(|(k, v)| old.get(*k) != Some(v))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    let unset = old
        .keys()
        .filter(|k| !new.contains_key(*k))
        .cloned()
        .collect();
    (set, unset)
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: Key = [0x11; 32];
    const NB: Nonce = [0x22; 32];
    const NP: Nonce = [0x33; 32];
    const INST: Instance = [0x44; 16];

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// Vectors computed independently (Python `hmac`, see the spec).
    #[test]
    fn handshake_vectors() {
        assert_eq!(hex(&proxy_proof(&K, &NB, &NP, &INST)), VEC_P);
        assert_eq!(hex(&bridge_proof(&K, &NP, &NB, &INST)), VEC_B);
        assert_eq!(hex(&session_key(&K, &NB, &NP)), VEC_S);
        let p = proxy_proof(&K, &NB, &NP, &INST);
        assert!(check_proxy_proof(&K, &NB, &NP, &INST, &p));
        assert!(
            !check_proxy_proof(&[0x12; 32], &NB, &NP, &INST, &p),
            "other key"
        );
        assert!(
            !check_bridge_proof(&K, &NP, &NB, &INST, &p),
            "a proxy proof is not a bridge proof"
        );
        let b = bridge_proof(&K, &NP, &NB, &INST);
        assert!(check_bridge_proof(&K, &NP, &NB, &INST, &b));
        assert!(
            !check_bridge_proof(&K, &NP, &NB, &[0x45; 16], &b),
            "other instance"
        );
    }

    const VEC_P: &str = "623e37cc00224135bc40f7f6dbee1d5fb06b57d4253af57ca52cc7883feab203";
    const VEC_B: &str = "ab3443c70252ea827d53977c33fbe2713227475539e52af5cedb5ffdfb1bb081";
    const VEC_S: &str = "00e6e624ea0c231bc693898def7cbb739a3bb358b87980504b65c6fec21e96dd";

    fn pair() -> (Codec, Codec) {
        let mut p = Codec::new(Side::Proxy);
        let mut b = Codec::new(Side::Bridge);
        let ks = session_key(&K, &NB, &NP);
        p.set_key(ks);
        b.set_key(ks);
        (p, b)
    }

    #[test]
    fn frames_round_trip_in_pieces() {
        let (mut p, mut b) = pair();
        let mut stream = Vec::new();
        for i in 0..3u64 {
            let m = Msg::Cmd {
                id: i,
                method: "heal".into(),
                args: ciborium::Value::Null,
            };
            stream.extend(p.encode(&m).unwrap());
        }
        stream.extend(p.encode(&Msg::Ping).unwrap());
        // Byte by byte: nothing until a whole frame is there.
        let mut buf = Vec::new();
        let mut got = Vec::new();
        for byte in stream {
            buf.push(byte);
            while let Some((m, n)) = b.decode(&buf).unwrap() {
                buf.drain(..n);
                got.push(m);
            }
        }
        assert_eq!(got.len(), 4);
        assert_eq!(got[3], Msg::Ping);
        assert!(matches!(got[2], Msg::Cmd { id: 2, .. }));
    }

    #[test]
    fn replayed_reordered_or_forged_frames_end_the_session() {
        let (mut p, mut b) = pair();
        let f0 = p.encode(&Msg::Ping).unwrap();
        let f1 = p.encode(&Msg::Pong).unwrap();
        assert!(b.decode(&f0).unwrap().is_some());
        assert_eq!(
            b.clone().decode(&f0),
            Err(FrameError::BadTag),
            "replay of frame 0"
        );
        let mut forged = f1.clone();
        let last = forged.len() - 1;
        forged[last] ^= 1;
        assert_eq!(
            b.clone().decode(&forged),
            Err(FrameError::BadTag),
            "changed tag"
        );
        let mut body = f1.clone();
        body[13] ^= 1;
        assert_eq!(
            b.clone().decode(&body),
            Err(FrameError::BadTag),
            "changed body"
        );
        // A frame of the other direction (reflected back) is rejected.
        let mut b2 = Codec::new(Side::Bridge);
        b2.set_key(session_key(&K, &NB, &NP));
        let own = b2.encode(&Msg::Ping).unwrap();
        assert_eq!(
            b.clone().decode(&own),
            Err(FrameError::BadTag),
            "reflected frame"
        );
        // Another session key (fresh nonces) rejects frames of the old one.
        let mut other = Codec::new(Side::Bridge);
        other.set_key(session_key(&K, &NB, &[0x34; 32]));
        assert_eq!(
            other.decode(&f0),
            Err(FrameError::BadTag),
            "frame from another session"
        );
        assert_eq!(b.decode(&f1).unwrap().map(|x| x.0), Some(Msg::Pong));
    }

    #[test]
    fn limits_and_unknown_messages() {
        let mut c = Codec::new(Side::Proxy);
        let big = (MAX_PRE_AUTH as u32 + 1).to_be_bytes();
        assert!(matches!(c.decode(&big), Err(FrameError::TooLarge(_))));
        let long = "x".repeat(5000);
        let mut enc = Codec::new(Side::Bridge);
        assert!(
            enc.encode(&Msg::Seen { nonce: long }).is_err(),
            "pre-auth frames stay small"
        );
        // A message type of a newer version decodes as Unknown.
        let mut body = Vec::new();
        let v = ciborium::Value::Map(vec![(
            ciborium::Value::Text("t".into()),
            ciborium::Value::Text("later".into()),
        )]);
        ciborium::into_writer(&v, &mut body).unwrap();
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend(body);
        assert_eq!(c.decode(&frame).unwrap().map(|x| x.0), Some(Msg::Unknown));
    }

    #[test]
    fn messages_round_trip() {
        let mut p = Codec::new(Side::Proxy);
        let mut b = Codec::new(Side::Bridge);
        let msgs = [
            Msg::Hello {
                proto: crate::PROTO,
                bridge: "0.1.0".into(),
                pumpkin: "0.2.0".into(),
                mc: "26.3".into(),
                instance: INST,
                nb: NB,
            },
            Msg::Welcome {
                server: "lobby".into(),
                groups: vec!["g".into()],
                stats_ms: 5000,
                player_stats_ms: 2000,
            },
            Msg::Ev {
                ev: Event::Join {
                    player: Uuid::from_u128(1),
                    name: "Steve".into(),
                    pos: Pos {
                        world: "minecraft:overworld".into(),
                        x: 0.5,
                        y: 64.0,
                        z: 0.5,
                        yaw: Some(90.0),
                        pitch: None,
                    },
                },
            },
            Msg::Res {
                id: 9,
                ok: None,
                err: Some(crate::err::NO_PLAYER.into()),
                detail: None,
            },
        ];
        for m in msgs {
            let f = b.encode(&m).unwrap();
            assert_eq!(p.decode(&f).unwrap().map(|x| x.0), Some(m));
        }
    }

    #[test]
    fn keys_nonces_and_diffs() {
        assert_eq!(parse_key(&key_hex(&K)), Some(K));
        assert_eq!(parse_key("zz"), None);
        assert_eq!(parse_key(&"g".repeat(64)), None);
        assert_eq!(
            ping_nonce("pumbo-bridge.0123456789abcdef0123456789abcdef"),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(ping_nonce("pumbo-bridge.short"), None);
        assert_eq!(ping_nonce("play.example.com"), None);
        let old: BTreeMap<String, bool> = [("a".into(), true), ("b".into(), true)].into();
        let new: BTreeMap<String, bool> = [("a".into(), false), ("c".into(), true)].into();
        let (set, unset) = perm_diff(&old, &new);
        assert_eq!(set, vec![("a".to_string(), false), ("c".to_string(), true)]);
        assert_eq!(unset, vec!["b".to_string()]);
    }
}
