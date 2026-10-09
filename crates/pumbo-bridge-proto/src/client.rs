//! Typed client of `pumbo:bridge@1.0` for PumboProx plugins (feature `sdk`).
//!
//! ```ignore
//! // manifest: uses = [{ service = "pumbo:bridge", version = "1.0" }]
//! use pumbo_bridge_proto::{api, client::Bridge};
//! let r = Bridge::client().set_gamemode(&api::SetGamemode { player, mode: api::GameMode::Creative, server: None }).await;
//! // errors: ClientError::Service(ServiceError::Rejected("no-bridge")), see crate::err
//! ```
//!
//! The provider is the proxy itself; [`BridgeApi`] only exists because the
//! macro generates it.

use crate::api::*;

pumbo_sdk::service! {
    /// `pumbo:bridge@1.0`.
    pub service Bridge("pumbo:bridge", 1, 0) provider BridgeApi {
        fn teleport(Teleport) -> ();
        fn set_gamemode(SetGamemode) -> ();
        fn heal(Heal) -> ();
        fn effect(Effect) -> ();
        fn fly(Fly) -> ();
        fn inv_get(InvGet) -> Vec<SlotItem>;
        fn inv_set(InvSet) -> ();
        fn inv_give(InvGive) -> Given;
        fn inv_clear(InvClear) -> ();
        fn show_items(ShowItems) -> ();
        fn q_player(QPlayer) -> PlayerInfo;
        fn q_server(QServer) -> ServerInfo;
        fn q_spawn(QSpawn) -> Pos;
        fn q_entities(QEntities) -> Entities;
        fn status(Status) -> Vec<ServerStatus>;
        fn send_to(SendTo) -> ();
    }
}

/// The error code of a rejected call (`no-bridge`, `no-player`, ...).
pub fn code(e: &pumbo_sdk::service::ClientError) -> Option<&str> {
    match e {
        pumbo_sdk::service::ClientError::Service(pumbo_sdk::services::ServiceError::Rejected(
            c,
        )) => Some(c),
        _ => None,
    }
}
