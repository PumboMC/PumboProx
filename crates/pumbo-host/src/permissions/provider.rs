//! The permission provider plugin: its source, start and loads.

use std::sync::atomic::Ordering;

use pumbo_core::permissions::{PermissionEntry, PermissionSource, Subject};

use super::fnv_hex;

/// The `plugin` source: `on-permission-load` of the provider (plan §5.8.4).
pub(crate) struct PluginSource<'h> {
    pub host: &'h crate::HostInner,
    pub slot: std::sync::Arc<crate::actor::PluginSlot>,
}

impl std::fmt::Debug for PluginSource<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginSource")
            .field("plugin", &self.slot.id)
            .finish()
    }
}

impl PermissionSource for PluginSource<'_> {
    fn name(&self) -> &str {
        &self.slot.id
    }

    fn load<'a>(
        &'a self,
        who: &'a Subject,
    ) -> pumbo_core::BoxFuture<'a, Result<Vec<PermissionEntry>, String>> {
        Box::pin(async move {
            let host = self.host;
            let info = host
                .players
                .read()
                .ok()
                .and_then(|p| {
                    p.values()
                        .find(|i| crate::uuid_of(&i.profile.id) == who.uuid)
                        .cloned()
                })
                .ok_or("player not online")?;
            let cfg = &host.cfg;
            let wait = std::time::Duration::from_millis(cfg.services.restart_wait_ms);
            let deadline = std::time::Duration::from_millis(cfg.permissions.load_timeout_ms);
            let set = self
                .slot
                .call(wait, deadline, move |acc, g| {
                    Box::pin(async move { g.call_on_permission_load(acc, info).await })
                })
                .await
                .map_err(|e| e.to_string())??;
            Ok(crate::dispatch::provider_entries(set))
        })
    }
}

impl crate::HostInner {
    pub(crate) fn is_permission_provider(&self, slot: &crate::actor::PluginSlot) -> bool {
        self.perm_provider.as_deref() == Some(slot.id.as_str())
    }

    /// The provider while it runs or is about to run again; `None`: the
    /// file decides.
    pub(crate) fn provider_slot(&self) -> Option<std::sync::Arc<crate::actor::PluginSlot>> {
        let slot = self.plugins.get(self.perm_provider.as_deref()?)?;
        slot.status()
            .is_coming()
            .then(|| std::sync::Arc::clone(slot))
    }

    /// Warns (at most once a minute) that the provider is down: players
    /// online keep its last set, `on-load-failure` decides new logins.
    pub(crate) fn warn_provider_down(&self, why: &str) {
        let Some(provider) = self.perm_provider.as_deref() else {
            return;
        };
        if let Ok(mut last) = self.provider_down_logged.lock()
            && last.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(60))
        {
            *last = Some(std::time::Instant::now());
            let logins = match self.cfg.permissions.on_load_failure {
                crate::config::OnLoadFailure::Deny => "new logins are refused".to_string(),
                crate::config::OnLoadFailure::FileOnly => {
                    format!("new logins get {}", self.cfg.permissions.file.display())
                }
            };
            tracing::warn!(
                "permission provider {provider} {why}: players online keep their permissions, {logins}; fix it from the proxy console (/prox plugins reload {provider}, or permissions.provider: file)"
            );
        }
    }

    /// `on-load-failure` for a player the provider cannot load.
    fn load_failed(&self) -> Result<(), pumbo_text::Component> {
        match self.cfg.permissions.on_load_failure {
            crate::config::OnLoadFailure::Deny => {
                Err(self.message(&self.cfg.plugins.messages.permissions_unavailable))
            }
            crate::config::OnLoadFailure::FileOnly => Ok(()),
        }
    }

    /// Calls a method of the provider's `pumbo:permissions` service.
    pub(crate) async fn call_provider<A: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        req: &impl serde::Serialize,
        timeout_ms: u32,
    ) -> Result<A, String> {
        let mut payload = Vec::new();
        ciborium::into_writer(req, &mut payload).map_err(|e| e.to_string())?;
        let c = pumbo_contracts::PERMISSIONS;
        let opts = crate::wit::services::CallOptions {
            timeout_ms: Some(timeout_ms),
            player: None,
            ctx: None,
        };
        let out = self
            .call_service(
                crate::services::HOST_CALLER,
                c.name,
                c.major,
                c.minor,
                method.into(),
                payload,
                opts,
            )
            .await
            .map_err(|e| format!("{e:?}"))?;
        ciborium::from_reader(out.as_slice()).map_err(|e| format!("answer: {e}"))
    }

    /// The provider's `instance` started: it gets `permissions.yml` to take
    /// over, then the `players` that were past the point of loading are
    /// loaded from it again. A player who logged in since is loaded by the
    /// login; a load still running, or one the new instance already answered
    /// (a login can reach it before the file), is not repeated.
    pub(crate) async fn provider_started(
        &self,
        players: Vec<crate::wit::types::PlayerId>,
        instance: u64,
    ) {
        self.hand_file(true).await;
        let due = self.perms.not_loaded_by(instance);
        for id in players.into_iter().filter(|id| due.contains(id)) {
            if let Err(t) = self.load_provider_permissions(id).await {
                tracing::warn!(player = id, "permissions not reloaded: {}", t.plain_text());
            }
        }
    }

    /// Hands `permissions.yml` to the provider (method `file` with the
    /// fingerprint) until it has accepted the file as it is now, at least
    /// once with `always` (the provider started). One handoff at a time, so
    /// the newest file is the last one the provider gets.
    pub(crate) async fn hand_file(&self, mut always: bool) {
        let _one = self.perms.handing.lock().await;
        loop {
            let data = self
                .perms
                .file
                .read()
                .map(|f| f.snapshot().to_string())
                .unwrap_or_default();
            let fingerprint = fnv_hex(&data);
            let taken = self
                .perms
                .accepted
                .lock()
                .is_ok_and(|a| a.as_ref() == Some(&fingerprint));
            if taken && !always {
                return;
            }
            always = false;
            let file = pumbo_contracts::PermissionsFile { fingerprint, data };
            let sent = self
                .call_provider::<bool>(pumbo_contracts::METHOD_FILE, &file, 10_000)
                .await;
            if let Ok(mut a) = self.perms.accepted.lock() {
                // A file that timed out may still land, so a failure leaves none accepted.
                *a = sent.is_ok().then_some(file.fingerprint);
            }
            if let Err(e) = sent {
                tracing::warn!("permissions.yml not handed to the permission provider: {e}");
                return;
            }
        }
    }

    /// Loads the provider layer of a player (plan §5.8.4, `load-at`). When
    /// the provider is down or fails, `on-load-failure` decides: `deny`
    /// keeps the player out, `file-only` lets them in on the file.
    pub(crate) async fn load_provider_permissions(
        &self,
        id: crate::wit::types::PlayerId,
    ) -> Result<(), pumbo_text::Component> {
        let Some(provider) = self.perm_provider.as_deref() else {
            return Ok(());
        };
        self.perms.mark_eligible(id);
        let _loading = self.perms.start_load(id);
        // A load that outlives a restart counts for the old instance, so the
        // provider's start loads the player again.
        let instance = self.perms.instance.load(Ordering::SeqCst);
        let Some(slot) = self.provider_slot() else {
            self.warn_provider_down("is not running");
            return self.load_failed();
        };
        let (Some(info), Some(epoch)) = (self.player(id), self.perms.epoch(id)) else {
            return Err(self.message(&self.cfg.plugins.messages.permissions_unavailable));
        };
        let who = crate::subject(&info);
        let source = PluginSource { host: self, slot };
        match source.load(&who).await {
            Ok(entries) => {
                // A set pushed while this load ran is newer.
                if self.perms.load_provider_layer(id, epoch, instance, entries) {
                    self.forget_offline(Some(who.uuid));
                    self.bridge
                        .send(id, crate::bridge::PlayerCommand::CommandsChanged);
                }
                Ok(())
            }
            Err(e) => {
                tracing::warn!(player = id, provider = %provider, "permissions not loaded: {e}");
                self.load_failed()
            }
        }
    }

    /// Loads again the players past the point of loading that have no set
    /// from the provider (it was down or failed for them), while it runs.
    pub(crate) async fn retry_unloaded(&self) {
        if self
            .provider_slot()
            .is_none_or(|s| s.status() != crate::actor::Status::Running)
        {
            return;
        }
        for id in self.perms.unloaded() {
            if let Err(t) = self.load_provider_permissions(id).await {
                tracing::warn!(
                    player = id,
                    "permissions not loaded again: {}",
                    t.plain_text()
                );
            }
        }
    }
}
