//! Pairing a bridge with its `servers` entry by a status ping (spec §3.4).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use pumbo_bridge_proto::wire::Msg;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::{Bridge, Link, MISSING_AFTER, PAIR_EVERY, PAIR_TRIES};

impl Bridge {
    pub(super) fn pair(&self, link: &Arc<Link>, nonce: &str) {
        let (server, old) = {
            let mut st = self.lock();
            let Some((server, _)) = st.nonces.get(nonce).cloned() else {
                debug!("bridge {} saw an unknown pairing nonce", link.peer);
                return;
            };
            if st.unpaired.remove(&link.id).is_none() {
                if link.server().as_deref() != Some(server.as_str()) {
                    warn!(
                        "the bridge paired with {} also answers for {server}: one Pumpkin under two names is not supported, {server} stays without a bridge",
                        link.server().unwrap_or_default()
                    );
                }
                return;
            }
            let slot = st.servers.entry(server.clone()).or_default();
            let old = slot.link.replace(link.clone());
            slot.since = Some(SystemTime::now());
            slot.ever = true;
            (server, old)
        };
        if let Some(old) = old {
            old.close(format!("replaced by a newer session from {}", link.peer));
        }
        if let Ok(mut s) = link.server.lock() {
            *s = Some(server.clone());
        }
        let groups = self
            .proxy()
            .and_then(|p| p.plugins.get().map(|pl| pl.host.groups_of(&server)))
            .unwrap_or_default();
        link.send(Msg::Welcome {
            server: server.clone(),
            groups,
            stats_ms: self.cfg.stats_ms,
            player_stats_ms: self.cfg.player_stats_ms,
        });
        let (b, p, mc) = &link.version;
        info!(
            "bridge on {server} paired ({}, PumboBridge {b}, Pumpkin {p}, Minecraft {mc})",
            link.peer
        );
        self.publish_status(&server);
    }

    /// After `sync`: permission sets of the players there and the export.
    pub(super) fn synced(&self, link: &Arc<Link>) {
        let (Some(server), Some(proxy)) = (link.server(), self.proxy()) else {
            return;
        };
        for p in proxy.players() {
            if p.server.as_deref() == Some(server.as_str()) {
                self.send_perms(link, &server, p.id, false);
            }
        }
        self.send_export(link, &server);
    }

    /// The pairing task: while authenticated sessions wait, every server gets
    /// a status ping with a fresh nonce every 10 s (at once for a new
    /// session); a session that matches nothing after 3 rounds is closed.
    pub(super) async fn pairing(self: Arc<Self>) {
        loop {
            tokio::select! {
                () = self.wake.notified() => {}
                () = tokio::time::sleep(PAIR_EVERY) => {}
            }
            let Some(proxy) = self.proxy() else { return };
            let rt = proxy.runtime();
            self.missing(&rt);
            let (targets, give_up) = {
                let mut st = self.lock();
                st.nonces
                    .retain(|_, (_, t)| t.elapsed() < PAIR_EVERY * (PAIR_TRIES + 1));
                if st.unpaired.is_empty() {
                    continue;
                }
                let mut give_up = Vec::new();
                for (link, tries) in st.unpaired.values_mut() {
                    *tries += 1;
                    if *tries > PAIR_TRIES {
                        give_up.push(link.clone());
                    }
                }
                for l in &give_up {
                    st.unpaired.remove(&l.id);
                }
                let mut targets = Vec::new();
                for (name, s) in &rt.config.servers {
                    let mut n = [0u8; 16];
                    if aws_lc_rs::rand::fill(&mut n).is_err() {
                        continue;
                    }
                    let nonce: String = n.iter().map(|b| format!("{b:02x}")).collect();
                    st.nonces
                        .insert(nonce.clone(), (name.clone(), Instant::now()));
                    targets.push((s.address.clone(), nonce));
                }
                (targets, give_up)
            };
            for l in give_up {
                self.reject(l.peer.ip(), "matches no server in servers:".into());
                l.close("it matches no server in servers:".into());
            }
            for (address, nonce) in targets {
                tokio::spawn(async move {
                    if let Err(e) = status_ping(&address, &nonce).await {
                        debug!("pairing ping to {address}: {e}");
                    }
                });
            }
        }
    }

    /// One line per server that never had a bridge, a minute after start.
    fn missing(&self, rt: &crate::server::Runtime) {
        if self.started.elapsed() < MISSING_AFTER {
            return;
        }
        let mut st = self.lock();
        for name in rt.config.servers.keys() {
            let slot = st.servers.entry(name.clone()).or_default();
            if !slot.ever && !slot.warned {
                slot.warned = true;
                info!(
                    "no bridge on {name}: server commands follow op levels there, bridge calls answer no-bridge"
                );
            }
        }
    }
}

fn varint(out: &mut Vec<u8>, v: i32) {
    let mut v = v as u32;
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// A status ping whose host name is `pumbo-bridge.<nonce>` (spec §3.4).
async fn status_ping(address: &str, nonce: &str) -> Result<(), String> {
    let (host, port) =
        crate::config::split_host_port(address).ok_or_else(|| "bad address".to_string())?;
    let mut sock = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect((host, port)))
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let name = format!("{}{nonce}", pumbo_bridge_proto::PING_PREFIX);
    let mut hs = Vec::new();
    varint(&mut hs, 0);
    varint(&mut hs, 767);
    varint(&mut hs, name.len() as i32);
    hs.extend_from_slice(name.as_bytes());
    hs.extend_from_slice(&port.to_be_bytes());
    varint(&mut hs, 1);
    let mut out = Vec::new();
    varint(&mut out, hs.len() as i32);
    out.extend(hs);
    out.extend([1, 0]);
    sock.write_all(&out).await.map_err(|e| e.to_string())?;
    // The answer is not needed; read a little so the server is not cut off.
    let mut buf = [0u8; 512];
    let _ = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut buf)).await;
    Ok(())
}
