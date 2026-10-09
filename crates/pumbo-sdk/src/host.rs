//! Host functions by interface. In WASM they call the host; natively (in
//! `cargo test`) they call the fake host from [`crate::testing`], which has
//! the same signatures.

#[cfg(target_arch = "wasm32")]
use crate::bindings::pumbo::prox as backend;
#[cfg(not(target_arch = "wasm32"))]
use crate::testing::backend;

use crate::bindings::pumbo::prox::types::{PlayerId, Text, TitleTimes};

pub mod players {
    use super::*;
    pub use crate::bindings::pumbo::prox::players::ConnectError;
    pub use backend::players::{all, clear_title, connect, find, get, reconnect};

    pub fn send_message(id: PlayerId, msg: impl Into<Text>) {
        backend::players::send_message(id, &msg.into());
    }

    pub fn send_action_bar(id: PlayerId, msg: impl Into<Text>) {
        backend::players::send_action_bar(id, &msg.into());
    }

    pub fn send_title(
        id: PlayerId,
        title: impl Into<Text>,
        subtitle: impl Into<Text>,
        times: TitleTimes,
    ) {
        backend::players::send_title(id, &title.into(), &subtitle.into(), times);
    }

    pub fn play_sound(id: PlayerId, sound: &str, volume: f32, pitch: f32) {
        backend::players::play_sound(id, sound, volume, pitch);
    }

    pub fn tab_header_footer(id: PlayerId, header: impl Into<Text>, footer: impl Into<Text>) {
        backend::players::tab_header_footer(id, &header.into(), &footer.into());
    }

    pub fn kick(id: PlayerId, reason: impl Into<Text>) {
        backend::players::kick(id, &reason.into());
    }

    pub fn set_property(id: PlayerId, prop: &crate::Property) -> Result<(), String> {
        backend::players::set_property(id, prop)
    }

    pub fn remove_property(id: PlayerId, name: &str) -> Result<(), String> {
        backend::players::remove_property(id, name)
    }
}

pub mod bossbar {
    pub use super::backend::bossbar::Bar;
    pub use crate::bindings::pumbo::prox::types::{BossbarColor, BossbarOverlay};
}

pub mod servers {
    pub use super::backend::servers::all;
    pub use crate::bindings::pumbo::prox::servers::ServerInfo;
}

pub mod commands {
    use super::backend;
    pub use crate::bindings::pumbo::prox::commands::{CommandSpec, PermissionState};

    /// Builder of a command spec.
    #[derive(Debug, Clone)]
    pub struct Command(pub CommandSpec);

    impl Command {
        pub fn new(name: &str) -> Command {
            Command(CommandSpec {
                name: name.to_string(),
                aliases: Vec::new(),
                permission: None,
                state: PermissionState::Always,
                sensitive: false,
                virtual_only: false,
                usage: String::new(),
                umbrella: false,
            })
        }

        pub fn alias(mut self, a: &str) -> Command {
            self.0.aliases.push(a.to_string());
            self
        }

        /// Needs the node (`pumbo.<plugin>.<action>`).
        pub fn permission(mut self, node: &str) -> Command {
            self.0.permission = Some(node.to_string());
            self.0.state = PermissionState::Permission;
            self
        }

        pub fn sensitive(mut self) -> Command {
            self.0.sensitive = true;
            self
        }

        pub fn virtual_only(mut self) -> Command {
            self.0.virtual_only = true;
            self
        }

        pub fn usage(mut self, u: &str) -> Command {
            self.0.usage = u.to_string();
            self
        }

        /// Under `/pumbo <short-name> <name>`.
        pub fn umbrella(mut self) -> Command {
            self.0.umbrella = true;
            self
        }

        pub fn register(self) -> Result<(), String> {
            backend::commands::register(&self.0)
        }
    }
}

pub mod gates {
    pub use super::backend::gates::release;
}

pub mod permissions {
    pub use super::backend::permissions::{has, has_offline, replace, set};
    pub use crate::bindings::pumbo::prox::permissions::{PermissionEntry, PermissionSet};
}

pub mod services {
    pub use super::backend::services::{call, lookup, set_available};
    pub use crate::bindings::pumbo::prox::services::{CallOptions, ServiceError, ServiceRef};
}

pub mod bus {
    use super::backend;

    pub fn publish(topic: &str, payload: &[u8]) -> Result<(), String> {
        backend::bus::publish(topic, payload)
    }

    /// Publishes a value as CBOR.
    pub fn publish_value<T: serde::Serialize>(topic: &str, value: &T) -> Result<(), String> {
        let bytes = crate::service::to_cbor(value).map_err(|e| e.to_string())?;
        publish(topic, &bytes)
    }
}

/// Plugin metrics; names must be declared in the description.
pub mod metrics {
    pub use super::backend::admin::{counter_add, gauge_set, histogram_record};
}

pub mod placeholders {
    use super::*;
    pub use crate::bindings::pumbo::prox::placeholders::{PlaceholderRequest, ResolveError};
    pub use backend::placeholders::{clear, invalidate, resolve};

    /// Push value of a key of the own namespace.
    pub fn set(
        key: &str,
        player: Option<PlayerId>,
        value: impl Into<Text>,
        ctx: &crate::Context,
    ) -> Result<(), String> {
        backend::placeholders::set(key, player, &value.into(), ctx)
    }
}

pub mod scheduler {
    pub use super::backend::scheduler::{after, cancel, every, sleep};
}

pub mod messaging {
    pub use super::backend::messaging::{send, subscribe};
    pub use crate::bindings::pumbo::prox::messaging::Side;
}

pub mod crypto {
    pub use super::backend::crypto::{argon2id_hash, argon2id_verify, bcrypt_verify};
    pub use crate::bindings::pumbo::prox::crypto::Argon2Params;
}

pub mod http {
    pub use super::backend::http::fetch;
    pub use crate::bindings::pumbo::prox::http::{HttpError, Request, Response};
}

pub mod log {
    use super::backend;
    pub use crate::bindings::pumbo::prox::log::Level;

    pub fn write(level: Level, msg: &str) {
        backend::log::write(level, msg);
    }

    pub fn info(msg: &str) {
        write(Level::Info, msg);
    }

    pub fn warn(msg: &str) {
        write(Level::Warn, msg);
    }

    pub fn error(msg: &str) {
        write(Level::Error, msg);
    }

    /// Only in debug mode (`/pumbo <short-name> debug`).
    pub fn debug(msg: &str) {
        if crate::debug_enabled() {
            write(Level::Info, msg);
        } else {
            write(Level::Debug, msg);
        }
    }
}

/// Files of the plugin: `/config` (read-only: config, overlays, language
/// overrides) and `/data` (read-write).
pub(crate) fn read_config_file(path: &str) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        std::fs::read_to_string(format!("/config/{path}")).ok()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        crate::testing::with(|h| h.config_files.get(path).cloned())
    }
}

pub(crate) fn list_config_dir(dir: &str) -> Vec<String> {
    #[cfg(target_arch = "wasm32")]
    {
        std::fs::read_dir(format!("/config/{dir}"))
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().to_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let prefix = format!("{dir}/");
        crate::testing::with(|h| {
            h.config_files
                .keys()
                .filter_map(|k| k.strip_prefix(&prefix).map(str::to_string))
                .collect()
        })
    }
}
