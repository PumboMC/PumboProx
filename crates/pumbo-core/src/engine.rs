//! Extension point: server engines. An adapter says what a server of its
//! engine needs from the proxy: defaults for its `servers` entry and repairs
//! of packets the engine gets wrong.

use serde::Deserialize;

/// Cancelling a signed message leaves a gap in the signature chain. Vanilla
/// and Paper then reject every later signed message of that player until
/// they reconnect ("chat chain broken"); Pumpkin does not check (E4 test).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignedChatCancel {
    /// Drop it with a replacement `chat_ack`; the player stays.
    #[default]
    Drop,
    /// Disconnect the player with `messages.signed-chat-blocked`.
    Kick,
}
