//! The contract between gate plugins (PumboFilter, PumboAuth) that hold a player
//! until they are done with them.
//!
//! On Pumpkin the messages go through the host's `ipc` (synchronous, an error
//! means the other plugin is not there); on PumboProx through the service
//! registry. Both sides answer both requests:
//!
//! | request | answer |
//! | --- | --- |
//! | `{"op":"hello"}` | `{"ok":true,"plugin":"PumboFilter","version":"0.1.0","protocol":1}` |
//! | `{"op":"holding","uuid":"..."}` | `{"ok":true,"state":"holding"\|"free"\|"unknown"}` |
//!
//! A plugin that is done with a player marks them free first and asks the other
//! one afterwards; it releases the player unless the other one still holds
//! them. Whatever the interleaving, at least one of the two releases.

use serde::{Deserialize, Serialize};
use serde_json::json;

/// Version of the message format.
pub const PROTOCOL: u32 = 1;

/// What a gate plugin knows about a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Hold {
    /// The plugin holds the player (they are in its gate).
    Holding,
    /// The plugin saw the player join and does not hold them.
    Free,
    /// The plugin has not seen the player join (yet), or was busy: ask again.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    Hello,
    Holding { uuid: String },
}

/// `{"op":"holding","uuid":..}`.
pub fn holding_request(uuid: &str) -> Vec<u8> {
    json!({"op": "holding", "uuid": uuid}).to_string().into_bytes()
}

pub fn parse_request(message: &[u8]) -> Result<Request, String> {
    serde_json::from_slice(message).map_err(|e| format!("bad request: {e}"))
}

pub fn holding_answer(state: Hold) -> Vec<u8> {
    json!({"ok": true, "state": state}).to_string().into_bytes()
}

pub fn hello_answer(plugin: &str, version: &str) -> Vec<u8> {
    json!({"ok": true, "plugin": plugin, "version": version, "protocol": PROTOCOL}).to_string().into_bytes()
}

pub fn error_answer(error: &str) -> Vec<u8> {
    json!({"ok": false, "error": error}).to_string().into_bytes()
}

/// The state in an answer to `holding`. Anything unreadable (an error answer,
/// a newer format) counts as [`Hold::Unknown`].
pub fn parse_holding(answer: &[u8]) -> Hold {
    #[derive(Deserialize)]
    struct Answer {
        #[serde(default)]
        ok: bool,
        state: Option<Hold>,
    }
    match serde_json::from_slice::<Answer>(answer) {
        Ok(Answer { ok: true, state: Some(s) }) => s,
        _ => Hold::Unknown,
    }
}

/// Answers a request with what the plugin knows (`state` for `holding`).
pub fn answer(message: &[u8], plugin: &str, version: &str, state: impl FnOnce(&str) -> Hold) -> Vec<u8> {
    match parse_request(message) {
        Ok(Request::Hello) => hello_answer(plugin, version),
        Ok(Request::Holding { uuid }) => holding_answer(state(&uuid)),
        Err(e) => error_answer(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let req = holding_request("069a79f4-44e9-4726-a5be-fca90e38aaf5");
        assert_eq!(
            parse_request(&req).unwrap(),
            Request::Holding { uuid: "069a79f4-44e9-4726-a5be-fca90e38aaf5".into() }
        );
        for s in [Hold::Holding, Hold::Free, Hold::Unknown] {
            assert_eq!(parse_holding(&holding_answer(s)), s);
        }
        assert_eq!(String::from_utf8(holding_answer(Hold::Free)).unwrap(), r#"{"ok":true,"state":"free"}"#);
    }

    #[test]
    fn answers_every_request() {
        let hello = answer(br#"{"op":"hello"}"#, "PumboFilter", "0.1.0", |_| Hold::Free);
        let v: serde_json::Value = serde_json::from_slice(&hello).unwrap();
        assert_eq!(v["plugin"], "PumboFilter");
        assert_eq!(v["protocol"], 1);
        let st =
            answer(br#"{"op":"holding","uuid":"x"}"#, "P", "1", |u| if u == "x" { Hold::Holding } else { Hold::Free });
        assert_eq!(parse_holding(&st), Hold::Holding);
        let bad = answer(b"nope", "P", "1", |_| Hold::Free);
        assert_eq!(parse_holding(&bad), Hold::Unknown);
        assert!(String::from_utf8(bad).unwrap().contains("\"ok\":false"));
        assert_eq!(parse_holding(br#"{"ok":true,"state":"later"}"#), Hold::Unknown);
    }
}
