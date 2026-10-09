//! The contract of PumboBans for other plugins: is a player banned or muted,
//! place a punishment, read the history.
//!
//! The same JSON on both platforms:
//!
//! - Pumpkin: bytes through the host's `ipc` to the plugin [`PLUGIN`]
//!   (synchronous; an error means PumboBans is not there);
//! - PumboProx: the service [`SERVICE`] `@1.0`, method [`METHOD`], payload and
//!   answer as below (`uses = [{ service = "pumbobans:punish", version = "1.0" }]`
//!   in the manifest; `unavailable` means PumboBans is not there).
//!
//! | request | answer |
//! | --- | --- |
//! | `{"op":"hello"}` | `{"ok":true,"plugin":"PumboBans","version":"0.1.0","protocol":1}` |
//! | `{"op":"check","uuid":..,"name":..,"ip":..}` (any of the three) | [`CheckAnswer`] |
//! | `{"op":"punish","kind":"ban","target":"Steve","duration":"1h","reason":"..","ip":false,"silent":false}` | `{"ok":true,"id":12}` |
//! | `{"op":"history","target":"Steve","limit":20}` | `{"ok":true,"punishments":[`[`Summary`]`..]}` |
//!
//! Errors are `{"ok":false,"error":"..."}`. PumboBans answers `check` and
//! `history` when its `ipc.allow-queries` is on, and `punish` only for plugins
//! listed in `ipc.allow-punish`. New fields may appear in answers; readers
//! ignore what they do not know.
//!
//! Events on PumboProx: PumboBans publishes `pumbo:player-punished@1.0` and
//! `pumbo:punishment-revoked@1.0` (types in `pumbo-contracts` of PumboProx).

use serde::{Deserialize, Serialize};

/// Recipient name on Pumpkin.
pub const PLUGIN: &str = "pumbobans";
/// Service name on PumboProx.
pub const SERVICE: &str = "pumbobans:punish";
pub const SERVICE_MAJOR: u16 = 1;
pub const SERVICE_MINOR: u16 = 0;
/// The one method of the service; the operation is in the JSON (`op`).
pub const METHOD: &str = "request";
/// Version of the JSON format.
pub const PROTOCOL: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    Hello,
    Check {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uuid: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ip: Option<String>,
    },
    Punish {
        /// `ban`, `mute`, `warn` or `kick`.
        kind: String,
        /// A player name or UUID, or with `ip` an address, range or player.
        target: String,
        /// `30m`, `1d`, `perm`; none: permanent (warnings: the configured length).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default)]
        ip: bool,
        #[serde(default)]
        silent: bool,
    },
    History {
        target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
}

impl Request {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn parse(message: &[u8]) -> Result<Request, String> {
        serde_json::from_slice(message).map_err(|e| format!("bad request: {e}"))
    }
}

/// A punishment in answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub id: u64,
    /// `ban`, `mute`, `warn`, `kick`.
    pub kind: String,
    /// Who it is aimed at: a name, `Name (1.2.3.4)` or an address range.
    pub target: String,
    pub reason: String,
    pub operator: String,
    /// Unix milliseconds.
    pub created: u64,
    /// Unix milliseconds; none: permanent.
    #[serde(default)]
    pub expires: Option<u64>,
    #[serde(default)]
    pub revoked: bool,
}

/// Answer to `check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckAnswer {
    #[serde(default)]
    pub ban: Option<Summary>,
    #[serde(default)]
    pub mute: Option<Summary>,
    /// Active warnings of the account.
    #[serde(default)]
    pub warnings: usize,
}

/// Reads the answer to `check`; an error answer becomes `Err` with its text.
pub fn parse_check(answer: &[u8]) -> Result<CheckAnswer, String> {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        ok: bool,
        #[serde(default)]
        error: Option<String>,
        #[serde(flatten)]
        body: CheckAnswer,
    }
    let e: Envelope = serde_json::from_slice(answer).map_err(|e| format!("bad answer: {e}"))?;
    if e.ok { Ok(e.body) } else { Err(e.error.unwrap_or_else(|| "refused".into())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_answers() {
        let r = Request::Check { uuid: None, name: Some("Steve".into()), ip: None };
        assert_eq!(String::from_utf8(r.to_bytes()).unwrap(), r#"{"op":"check","name":"Steve"}"#);
        assert_eq!(Request::parse(&r.to_bytes()).unwrap(), r);
        assert_eq!(Request::parse(br#"{"op":"hello"}"#).unwrap(), Request::Hello);
        assert!(Request::parse(b"{}").is_err());
        let a = parse_check(br#"{"ok":true,"ban":null,"mute":{"id":3,"kind":"mute","target":"Steve","reason":"","operator":"Console","created":1,"expires":5,"revoked":false},"warnings":2,"later":1}"#).unwrap();
        assert_eq!((a.ban, a.mute.map(|m| m.id), a.warnings), (None, Some(3), 2));
        assert_eq!(parse_check(br#"{"ok":false,"error":"no"}"#).unwrap_err(), "no");
    }
}
