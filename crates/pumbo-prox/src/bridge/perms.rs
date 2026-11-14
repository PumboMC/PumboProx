//! Bridge events and the permission sets the proxy sends to bridges.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumbo_bridge_proto::api::{self, PermsMode};
use pumbo_bridge_proto::method;
use pumbo_bridge_proto::wire::{self, Event, Msg};
use tracing::info;
use uuid::Uuid;

use super::{Bridge, Link, cbor};

impl Bridge {
    pub(super) fn event(&self, link: &Arc<Link>, ev: Event) {
        let Some(server) = link.server() else {
            return;
        };
        match &ev {
            // Always: a set sent before the login may have expired in the bridge.
            Event::Join { player, .. } => self.send_perms(link, &server, *player, true),
            Event::Leave { player, .. } => {
                if let Ok(mut s) = link.perms_sent.lock() {
                    s.remove(player);
                }
            }
            Event::PermsMode { mode } => {
                let old = link
                    .perms_mode
                    .lock()
                    .map(|mut m| std::mem::replace(&mut *m, *mode))
                    .unwrap_or(*mode);
                if old != *mode {
                    match mode {
                        PermsMode::Local => {
                            info!("{server}: ranks from the local PumboPerms");
                            self.send_export(link, &server);
                        }
                        PermsMode::Bridge => {
                            info!("{server}: ranks from the proxy again");
                            if let Ok(mut s) = link.perms_sent.lock() {
                                s.clear();
                            }
                            self.synced(link);
                        }
                    }
                }
            }
            _ => {}
        }
        if let Some(p) = self.proxy().and_then(|p| p.plugins.get().cloned()) {
            let e = wire::BusEvent { server, ev };
            if let Ok(b) = cbor(&e) {
                p.host.publish(&pumbo_contracts::BRIDGE_EVENT, b);
            }
        }
    }

    pub(super) fn publish_status(&self, server: &str) {
        let Some(p) = self.proxy().and_then(|p| p.plugins.get().cloned()) else {
            return;
        };
        if let Some(s) = self.status(Some(server)).into_iter().next()
            && let Ok(b) = cbor(&s)
        {
            p.host.publish(&pumbo_contracts::BRIDGE_STATUS, b);
        }
    }

    /// A command whose answer nobody waits for (the bridge still answers);
    /// `false` when the queue to the bridge is full.
    fn notify(&self, link: &Link, m: &str, args: ciborium::Value) -> bool {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        link.send(Msg::Cmd {
            id,
            method: m.into(),
            args,
        })
    }

    /// Sends the player's decisions for `server` unless they did not change
    /// (spec §5.2; `force`: also then), then asks the permission provider
    /// for the player's info there and sends the set again when it changed.
    /// A local PumboPerms rules there: nothing to send.
    pub(super) fn send_perms(&self, link: &Arc<Link>, server: &str, player: Uuid, force: bool) {
        if link.mode() == PermsMode::Local {
            return;
        }
        let Some(plugins) = self.proxy().and_then(|p| p.plugins.get().cloned()) else {
            return;
        };
        let Some(asked) = self.put_perms(link, &plugins, server, player, None, force) else {
            return;
        };
        let (me, link, server) = (self.me.clone(), Arc::clone(link), server.to_string());
        tokio::spawn(async move {
            let Ok(info) = plugins.permissions_info(player, &server).await else {
                return;
            };
            if let Some(me) = me.upgrade() {
                me.put_perms(&link, &plugins, &server, player, Some((asked, info)), false);
            }
        });
    }

    /// Sends the current set with `info` (`None`: the last one sent) when it
    /// is new, differs from the last one or `force`. Without `info` returns
    /// the number of the info request to make; an answer to an older
    /// request is dropped.
    fn put_perms(
        &self,
        link: &Link,
        plugins: &crate::plugins::Plugins,
        server: &str,
        player: Uuid,
        info: Option<(u64, Option<String>)>,
        force: bool,
    ) -> Option<u64> {
        let catalog = link.catalog.lock().map(|c| c.clone()).unwrap_or_default();
        let nodes = plugins.backend_permissions(player, server, &catalog)?;
        let (set, asked) = {
            let mut sent = link.perms_sent.lock().ok()?;
            let new = !sent.contains_key(&player);
            let answer = info.is_some();
            // An answer after the player left must not bring the entry back.
            let last = if answer {
                sent.get_mut(&player)?
            } else {
                sent.entry(player).or_default()
            };
            let info = match info {
                Some((asked, _)) if asked != last.asked => return None,
                Some((_, info)) => info,
                None => last.info.clone(),
            };
            let send = force || new || last.nodes != nodes || last.info != info;
            last.nodes = nodes.clone();
            last.info = info.clone();
            if !answer {
                last.asked += 1;
            }
            let set = send.then_some(api::PermSet {
                player,
                nodes,
                extra: BTreeMap::new(),
                info,
            });
            (set, (!answer).then_some(last.asked))
        };
        if let Some(v) = set.and_then(|s| wire::value(&s).ok())
            && !self.notify(link, method::PERM_SET, v)
        {
            // Not sent: the next chance sends the full set (and an answer to
            // the info request in flight finds no entry and is dropped).
            if let Ok(mut sent) = link.perms_sent.lock() {
                sent.remove(&player);
            }
            return None;
        }
        asked
    }

    /// The table for the server (from the permission provider when it runs,
    /// so asked in a task).
    pub(super) fn send_export(&self, link: &Arc<Link>, server: &str) {
        let Some(plugins) = self.proxy().and_then(|p| p.plugins.get().cloned()) else {
            return;
        };
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (link, server) = (Arc::clone(link), server.to_string());
        tokio::spawn(async move {
            let data = plugins.host.permissions_export(&server).await;
            let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, data.as_bytes());
            let fingerprint = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
            if let Ok(args) = wire::value(&api::PermsExport { fingerprint, data }) {
                link.send(Msg::Cmd {
                    id,
                    method: method::PERMS_EXPORT.into(),
                    args,
                });
            }
        });
    }

    /// The session asks: the player is about to log in to `server` (before
    /// forwarding), so its permission set goes first, also when the bridge
    /// had it before (a login that failed, or a set that expired there).
    pub fn connecting(&self, player: Uuid, server: &str) {
        if let Some(link) = self.link(server) {
            self.send_perms(&link, server, player, true);
        }
    }

    /// The permission export changed (`permissions.yml` read again, the
    /// permission provider started or stopped): every paired bridge gets it.
    pub fn export_changed(&self) {
        let links: Vec<(Arc<Link>, String)> = self
            .lock()
            .servers
            .iter()
            .filter_map(|(name, slot)| Some((slot.link.clone()?, name.clone())))
            .collect();
        for (link, server) in links {
            self.send_export(&link, &server);
        }
    }

    /// The player's permissions or context changed.
    pub fn perms_changed(&self, player: Uuid, server: Option<&str>) {
        if let Some(server) = server
            && let Some(link) = self.link(server)
        {
            self.send_perms(&link, server, player, false);
        }
    }
}
