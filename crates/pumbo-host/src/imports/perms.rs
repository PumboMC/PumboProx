//! `gates` and `permissions`.

use std::sync::Arc;

use pumbo_core::permissions::Subject;
use wasmtime::component::{Accessor, HasSelf};

use super::{perm_context, query_levels};
use crate::actor::PluginSlot;
use crate::bridge::PlayerCommand;
use crate::permissions;
use crate::runtime::PluginState;
use crate::wit::types::{Context, PlayerId, QueryContext};
use crate::wit::{gates, permissions as wperm, services, types};

impl gates::Host for PluginState {
    fn release(&mut self, id: PlayerId) -> Result<(), String> {
        self.touch();
        let tx = self
            .host
            .held
            .lock()
            .ok()
            .and_then(|mut h| h.remove(&(id, self.slot.id.clone())));
        match tx {
            Some(tx) => {
                let _ = tx.send(());
                Ok(())
            }
            None => Err("player is not held by this gate".into()),
        }
    }
}

/// `permissions.set` asked for in the manifest (`permissions-write`) and
/// granted in the proxy config, only for nodes under the plugin's own
/// prefixes and never for the proxy's, the backends', groups or `*`
/// (GAP1-01). Nodes are matched without case, like the resolver does.
fn may_set(slot: &PluginSlot, node: &str) -> Result<(), String> {
    if !slot.manifest.permissions_write {
        return Err("the manifest has no permissions-write".into());
    }
    if !slot
        .scope
        .grants
        .contains(&crate::config::Grant::PermissionsWrite)
    {
        return Err(format!(
            "permissions-write is not granted (plugins.{}.grants in the proxy config)",
            slot.id
        ));
    }
    let node = node.to_ascii_lowercase();
    // Before the own prefixes: `pumbo-proxy` or `minecraft` would own these.
    let reserved = ["pumbo.proxy.", "minecraft:", "group."];
    if node == "*" || reserved.iter().any(|r| node.starts_with(r)) {
        return Err(format!("{node} is not for plugins to set"));
    }
    let own = slot.manifest.permission_prefixes();
    if !own.iter().any(|o| node.starts_with(o.as_str())) {
        return Err(format!("{node} must start with {}", own.join(" or ")));
    }
    Ok(())
}

impl wperm::Host for PluginState {
    fn has(&mut self, id: PlayerId, node: String, at: QueryContext) -> bool {
        self.touch();
        let lv = query_levels(&self.host, Some(id), &at);
        self.host.has(id, &node, &lv)
    }

    fn set(
        &mut self,
        id: PlayerId,
        node: String,
        value: Option<bool>,
        ctx: Context,
    ) -> Result<(), String> {
        self.touch();
        let player = self.host.player(id).map(|p| p.profile.name);
        if let Err(e) = may_set(&self.slot, &node) {
            tracing::warn!(plugin = %self.slot.id, ?player, %node, "permission change refused: {e}");
            return Err(e);
        }
        if !self
            .host
            .perms
            .set_node(id, &node, value, perm_context(&ctx))
        {
            return Err("unknown player".into());
        }
        tracing::info!(plugin = %self.slot.id, ?player, %node, ?value, ?ctx, "permission changed");
        self.send(id, PlayerCommand::CommandsChanged);
        Ok(())
    }

    fn replace(&mut self, id: PlayerId, set: wperm::PermissionSet) -> Result<(), String> {
        self.touch();
        if !self.host.is_permission_provider(&self.slot) {
            return Err(
                "only the configured permission provider may replace permission sets".into(),
            );
        }
        let entries = crate::dispatch::provider_entries(set);
        if !self.host.perms.replace_provider_layer(id, entries) {
            return Err("unknown player".into());
        }
        if let Some(info) = self.host.player(id) {
            self.host
                .forget_offline(Some(crate::uuid_of(&info.profile.id)));
        }
        self.send(id, PlayerCommand::CommandsChanged);
        Ok(())
    }
}

impl wperm::HostWithStore<PluginState> for HasSelf<PluginState> {
    async fn has_offline(
        accessor: &Accessor<PluginState, Self>,
        id: types::Uuid,
        node: String,
        at: QueryContext,
    ) -> Result<bool, services::ServiceError> {
        let (host, slot) = accessor.with(|mut a| {
            let s = a.get();
            s.waited();
            (Arc::clone(&s.host), Arc::clone(&s.slot))
        });
        let at = match at {
            QueryContext::Current => QueryContext::Global,
            other => other,
        };
        // The provider decides (it took the file over); while it is down the
        // file only with `on-load-failure: file-only`.
        if host.perm_provider.is_some() {
            if host.provider_slot().is_some() {
                match host.offline_from_provider(&slot, id, &node, &at).await {
                    Ok(v) => return Ok(v.unwrap_or_else(|| host.declared_default(&node))),
                    Err(e) => host.warn_provider_down(&format!("does not answer ({e:?})")),
                }
            }
            if host.cfg.permissions.on_load_failure == crate::config::OnLoadFailure::Deny {
                return Ok(host.declared_default(&node));
            }
        }
        let lv = query_levels(&host, None, &at);
        let who = Subject {
            uuid: crate::uuid_of(&id),
            name: String::new(),
        };
        let file = host
            .perms
            .file
            .read()
            .map(|f| f.entries_for(&who))
            .unwrap_or_default();
        let layers = permissions::Layers {
            file: &file,
            ..permissions::Layers::default()
        };
        match permissions::resolve(layers, &node, &lv) {
            Some(d) => Ok(d.value),
            None => Ok(host.declared_default(&node)),
        }
    }
}
