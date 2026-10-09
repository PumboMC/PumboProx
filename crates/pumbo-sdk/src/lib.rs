//! SDK for PumboProx plugins (`wasm32-wasip2`).
//!
//! ```ignore
//! #[derive(Default)]
//! struct Hello;
//!
//! impl pumbo_sdk::Plugin for Hello {
//!     async fn init(&self) -> Result<(), String> {
//!         pumbo_sdk::Command::new("hello").register()
//!     }
//!     async fn on_command(&self, e: pumbo_sdk::CommandEvent) {
//!         if let Some(p) = e.player {
//!             pumbo_sdk::players::send_message(p, pumbo_sdk::text::mini("<p>Hello!"));
//!         }
//!     }
//! }
//!
//! pumbo_sdk::plugin!(Hello);
//! ```
//!
//! - [`Plugin`]: every event with a default answer; implement what you need.
//! - host functions by interface: [`players`], [`servers`], [`permissions`],
//!   [`services`], [`placeholders`], [`bus`], [`scheduler`], [`http`], ...
//! - [`service!`]: typed client and dispatcher for a service (CBOR payloads).
//! - [`config::Config`]: `config.yml` with per-server and per-group overlays,
//!   [`lang::Lang`]: translations with the language of each player,
//!   [`describe::Description`]: machine-readable description for the host.
//! - [`testing`]: a fake host for `cargo test` without WASM.
//!
//! Plugins run single-threaded: never block (no `std::thread::sleep`, no long
//! synchronous I/O); use [`scheduler::sleep`] and other async functions.
//!
//! License: MIT OR Apache-2.0.

#[allow(unsafe_code, clippy::all, missing_docs)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "../../wit",
        world: "plugin",
        pub_export_macro: true,
        export_macro_name: "__export",
        default_bindings_module: "pumbo_sdk::bindings",
        additional_derives: [PartialEq],
    });
}

pub mod config;
pub mod describe;
mod embed;
mod host;
pub mod lang;
pub mod service;
#[cfg(not(target_arch = "wasm32"))]
pub mod testing;
pub mod text;

pub use host::*;

/// The virtual world (WIT `virtual`, plan §5): worlds, map images, entering,
/// teleports, maps in hand, release. In `cargo test` the fake host of
/// [`testing`] records the calls ([`testing::virtual_calls`]).
pub mod virtual_world {
    #[cfg(target_arch = "wasm32")]
    pub use crate::bindings::pumbo::prox::virtual_::*;
    #[cfg(not(target_arch = "wasm32"))]
    pub use crate::bindings::pumbo::prox::virtual_::{
        BlockPos, GameMode, Hand, Input, Position, WorldOptions,
    };
    #[cfg(not(target_arch = "wasm32"))]
    pub use crate::testing::backend::virtual_::*;
}

pub use bindings::exports::pumbo::prox::events::{
    BackendCommandEvent, BackendCommandReply, ChatReply, CommandEvent, ConnectEvent, ConnectReply,
    GateReply, KickedEvent, KickedReply, PreLoginEvent, PreLoginReply, ProfilePatch,
    PropertyChange, ServiceCall, StatusEvent, StatusReply,
};
pub use bindings::pumbo::prox::admin::{Actor, PluginDescription};
pub use bindings::pumbo::prox::permissions::{PermissionEntry, PermissionSet};
pub use bindings::pumbo::prox::placeholders::PlaceholderRequest;
pub use bindings::pumbo::prox::services::{CallReject, ServiceError, ServiceRef};
pub use bindings::pumbo::prox::types::{
    Connection, Context, PlayerContext, PlayerId, PlayerInfo, Profile, Property, QueryContext,
    Text, TextTemplate, Uuid, Verdict,
};
pub use describe::{Action, Description};
pub use host::commands::Command;

pub use ciborium;
/// Contracts of the `pumbo:` services and topics.
pub use pumbo_contracts as contracts;
pub use serde;
#[doc(hidden)]
pub use wit_bindgen;

/// A plugin: every event has a default answer (allow, pass, keep, nothing).
/// The instance lives for the whole life of the WASM instance; use `Cell` and
/// `RefCell` for state and never hold a `RefCell` borrow across `.await`.
#[allow(async_fn_in_trait, unused_variables)]
pub trait Plugin: Default + 'static {
    async fn init(&self) -> Result<(), String> {
        Ok(())
    }

    /// Machine-readable description (plan §6.6.5).
    fn describe(&self) -> Description {
        Description::new()
    }

    /// `/pumbo <short-name> reload`: the host has validated the config files;
    /// reload them here.
    async fn on_reload(&self) -> Result<(), String> {
        Ok(())
    }

    /// Admin actions from the description; `reload` and `debug` are handled
    /// by the SDK before this is called.
    async fn on_admin_action(
        &self,
        action: String,
        args: Vec<(String, String)>,
        by: Actor,
    ) -> Result<Text, Text> {
        Err(text::mini("<err>Unknown action"))
    }

    async fn shutdown(&self) {}

    async fn on_handshake(&self, c: Connection) -> Verdict {
        Verdict::Allow
    }

    async fn on_status(&self, e: StatusEvent) -> StatusReply {
        StatusReply::Keep
    }

    async fn on_pre_login(&self, e: PreLoginEvent) -> PreLoginReply {
        PreLoginReply::Allow
    }

    async fn on_profile(&self, p: PlayerInfo) -> ProfilePatch {
        ProfilePatch {
            id: None,
            name: None,
            properties: Vec::new(),
        }
    }

    async fn on_login(&self, p: PlayerInfo) -> Verdict {
        Verdict::Allow
    }

    async fn on_gate(&self, p: PlayerInfo) -> GateReply {
        GateReply::Pass
    }

    async fn on_server_connect(&self, e: ConnectEvent) -> ConnectReply {
        ConnectReply::Allow
    }

    async fn on_server_connected(&self, p: PlayerId, server: String, previous: Option<String>) {}

    async fn on_server_kicked(&self, e: KickedEvent) -> KickedReply {
        KickedReply::Keep
    }

    async fn on_disconnect(&self, p: PlayerId) {}

    async fn on_chat(&self, p: PlayerId, message: String) -> ChatReply {
        ChatReply::Pass
    }

    async fn on_backend_command(&self, e: BackendCommandEvent) -> BackendCommandReply {
        BackendCommandReply::Pass
    }

    async fn on_command(&self, e: CommandEvent) {}

    async fn on_plugin_message(&self, p: PlayerId, channel: String, data: Vec<u8>) -> bool {
        true
    }

    async fn on_timer(&self, timer: u64) {}

    async fn on_context_changed(
        &self,
        p: PlayerId,
        now: PlayerContext,
        previous: Option<PlayerContext>,
    ) {
    }

    /// Calls of services from `provides`; see [`service!`] for a dispatcher.
    async fn on_service_call(&self, c: ServiceCall) -> Result<Vec<u8>, CallReject> {
        Err(CallReject::UnknownMethod)
    }

    async fn on_service_changed(&self, service: String, available: Option<ServiceRef>) {}

    /// Pull placeholders: one answer per request, `None` = fallback.
    async fn on_placeholder(&self, reqs: Vec<PlaceholderRequest>) -> Vec<Option<Text>> {
        vec![None; reqs.len()]
    }

    /// Permission provider only: every entry of the player in every context.
    async fn on_permission_load(&self, p: PlayerInfo) -> Result<PermissionSet, String> {
        Err("not a permission provider".into())
    }

    async fn on_bus_event(
        &self,
        topic: String,
        major: u16,
        minor: u16,
        publisher: String,
        payload: Vec<u8>,
    ) {
    }

    /// Inputs of players this plugin put in a virtual world, one batch per
    /// tick (manifest event `virtual-input`).
    async fn on_virtual_input(&self, batch: Vec<virtual_world::Input>) {}
}

static DEBUG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Debug mode, switched by `/pumbo <short-name> debug`.
pub fn debug_enabled() -> bool {
    DEBUG.load(std::sync::atomic::Ordering::Relaxed)
}

#[doc(hidden)]
pub mod __private {
    use super::*;

    /// Standard admin actions the host routes to every plugin.
    pub async fn admin_action<P: Plugin>(
        p: &P,
        action: String,
        args: Vec<(String, String)>,
        by: Actor,
    ) -> Result<Text, Text> {
        match action.as_str() {
            "reload" => p
                .on_reload()
                .await
                .map(|()| text::mini("<ok>Reloaded."))
                .map_err(|e| text::template("<err>Reload failed: {error}", &[("error", &e)])),
            "debug" => {
                let on = !DEBUG.fetch_xor(true, std::sync::atomic::Ordering::Relaxed);
                Ok(text::mini(if on {
                    "<ok>Debug on."
                } else {
                    "<ok>Debug off."
                }))
            }
            _ => p.on_admin_action(action, args, by).await,
        }
    }
}

/// Exports a [`Plugin`] type to the host. The type needs `Default`; one
/// instance lives as long as the WASM instance.
#[macro_export]
macro_rules! plugin {
    ($t:ty) => {
        #[doc(hidden)]
        #[allow(dead_code)]
        fn __pumbo_instance() -> &'static $t {
            ::std::thread_local! {
                static INSTANCE: &'static $t = ::std::boxed::Box::leak(::std::boxed::Box::new(<$t as ::core::default::Default>::default()));
            }
            INSTANCE.with(|i| *i)
        }

        #[cfg(target_arch = "wasm32")]
        #[doc(hidden)]
        struct __PumboExport;

        #[cfg(target_arch = "wasm32")]
        const _: () = {
            use $crate::bindings::exports::pumbo::prox::events as ev;
            use $crate::bindings::pumbo::prox as wit;
            use $crate::Plugin as _;

            impl ev::Guest for __PumboExport {
                async fn init() -> ::core::result::Result<(), ::std::string::String> {
                    __pumbo_instance().init().await
                }
                fn describe() -> wit::admin::PluginDescription {
                    __pumbo_instance().describe().into_wit()
                }
                async fn on_admin_action(
                    action: ::std::string::String,
                    args: ::std::vec::Vec<(::std::string::String, ::std::string::String)>,
                    by: wit::admin::Actor,
                ) -> ::core::result::Result<wit::types::Text, wit::types::Text> {
                    $crate::__private::admin_action(__pumbo_instance(), action, args, by).await
                }
                async fn shutdown() {
                    __pumbo_instance().shutdown().await
                }
                async fn on_handshake(c: wit::types::Connection) -> wit::types::Verdict {
                    __pumbo_instance().on_handshake(c).await
                }
                async fn on_status(e: ev::StatusEvent) -> ev::StatusReply {
                    __pumbo_instance().on_status(e).await
                }
                async fn on_pre_login(e: ev::PreLoginEvent) -> ev::PreLoginReply {
                    __pumbo_instance().on_pre_login(e).await
                }
                async fn on_profile(p: wit::types::PlayerInfo) -> ev::ProfilePatch {
                    __pumbo_instance().on_profile(p).await
                }
                async fn on_login(p: wit::types::PlayerInfo) -> wit::types::Verdict {
                    __pumbo_instance().on_login(p).await
                }
                async fn on_gate(p: wit::types::PlayerInfo) -> ev::GateReply {
                    __pumbo_instance().on_gate(p).await
                }
                async fn on_server_connect(e: ev::ConnectEvent) -> ev::ConnectReply {
                    __pumbo_instance().on_server_connect(e).await
                }
                async fn on_server_connected(
                    p: wit::types::PlayerId,
                    server: ::std::string::String,
                    previous: ::core::option::Option<::std::string::String>,
                ) {
                    __pumbo_instance().on_server_connected(p, server, previous).await
                }
                async fn on_server_kicked(e: ev::KickedEvent) -> ev::KickedReply {
                    __pumbo_instance().on_server_kicked(e).await
                }
                async fn on_disconnect(p: wit::types::PlayerId) {
                    __pumbo_instance().on_disconnect(p).await
                }
                async fn on_chat(p: wit::types::PlayerId, message: ::std::string::String) -> ev::ChatReply {
                    __pumbo_instance().on_chat(p, message).await
                }
                async fn on_backend_command(e: ev::BackendCommandEvent) -> ev::BackendCommandReply {
                    __pumbo_instance().on_backend_command(e).await
                }
                async fn on_command(e: ev::CommandEvent) {
                    __pumbo_instance().on_command(e).await
                }
                async fn on_plugin_message(
                    p: wit::types::PlayerId,
                    channel: ::std::string::String,
                    data: ::std::vec::Vec<u8>,
                ) -> bool {
                    __pumbo_instance().on_plugin_message(p, channel, data).await
                }
                async fn on_timer(timer: u64) {
                    __pumbo_instance().on_timer(timer).await
                }
                async fn on_context_changed(
                    p: wit::types::PlayerId,
                    now: wit::types::PlayerContext,
                    previous: ::core::option::Option<wit::types::PlayerContext>,
                ) {
                    __pumbo_instance().on_context_changed(p, now, previous).await
                }
                async fn on_service_call(
                    c: ev::ServiceCall,
                ) -> ::core::result::Result<::std::vec::Vec<u8>, wit::services::CallReject> {
                    __pumbo_instance().on_service_call(c).await
                }
                async fn on_service_changed(
                    service: ::std::string::String,
                    available: ::core::option::Option<wit::services::ServiceRef>,
                ) {
                    __pumbo_instance().on_service_changed(service, available).await
                }
                async fn on_placeholder(
                    reqs: ::std::vec::Vec<wit::placeholders::PlaceholderRequest>,
                ) -> ::std::vec::Vec<::core::option::Option<wit::types::Text>> {
                    __pumbo_instance().on_placeholder(reqs).await
                }
                async fn on_permission_load(
                    p: wit::types::PlayerInfo,
                ) -> ::core::result::Result<wit::permissions::PermissionSet, ::std::string::String> {
                    __pumbo_instance().on_permission_load(p).await
                }
                async fn on_bus_event(
                    topic: ::std::string::String,
                    major: u16,
                    minor: u16,
                    publisher: ::std::string::String,
                    payload: ::std::vec::Vec<u8>,
                ) {
                    __pumbo_instance().on_bus_event(topic, major, minor, publisher, payload).await
                }
                async fn on_virtual_input(
                    batch: ::std::vec::Vec<$crate::virtual_world::Input>,
                ) {
                    __pumbo_instance().on_virtual_input(batch).await
                }
            }

            $crate::bindings::__export!(__PumboExport with_types_in $crate::bindings);
        };
    };
}
