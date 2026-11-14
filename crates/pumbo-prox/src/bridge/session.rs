//! Bridge connections: limits, the handshake and the message loop.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use pumbo_bridge_proto::api::PermsMode;
use pumbo_bridge_proto::wire::{self, Codec, Msg, Side};
use pumbo_bridge_proto::{PROTO, err};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::{
    Bridge, HANDSHAKE, Link, MAX_TRIES_PER_IP, MAX_UNAUTHENTICATED, OUT_QUEUE, write_moving,
};

impl Bridge {
    pub(super) async fn accept_loop(self: Arc<Self>, listener: TcpListener) {
        let mut stop = match self.proxy() {
            Some(p) => p.shutdown_signal(),
            None => return,
        };
        loop {
            tokio::select! {
                () = crate::session::stopped(&mut stop) => return,
                r = listener.accept() => match r {
                    Ok((sock, peer)) => {
                        if !self.admit(peer.ip()) {
                            continue;
                        }
                        tokio::spawn(self.clone().session(sock, peer));
                    }
                    Err(e) => {
                        warn!("bridge accept: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                },
            }
        }
    }

    /// Limits before the handshake (spec §7).
    fn admit(&self, ip: IpAddr) -> bool {
        let mut st = self.lock();
        if st.unauthenticated >= MAX_UNAUTHENTICATED {
            return false;
        }
        let e = st.tries.entry(ip).or_insert((Instant::now(), 0));
        if e.0.elapsed() > Duration::from_secs(10) {
            *e = (Instant::now(), 0);
        }
        e.1 += 1;
        if e.1 > MAX_TRIES_PER_IP {
            return false;
        }
        if st.tries.len() > 4096 {
            st.tries
                .retain(|_, (t, _)| t.elapsed() < Duration::from_secs(10));
        }
        st.unauthenticated += 1;
        true
    }

    async fn session(self: Arc<Self>, mut sock: TcpStream, peer: SocketAddr) {
        let mut said_hello = false;
        let r = tokio::time::timeout(HANDSHAKE, self.handshake(&mut sock, &mut said_hello)).await;
        self.lock().unauthenticated -= 1;
        let why = match r {
            Ok(Ok(x)) => Ok(x),
            Ok(Err(why)) => Err(why),
            Err(_) => Err("handshake timed out".to_string()),
        };
        let (hello, proto, codec, buf) = match why {
            Ok(x) => x,
            Err(why) if said_hello => {
                warn!("bridge from {peer} refused: {why}");
                self.reject(peer.ip(), why);
                return;
            }
            Err(why) => {
                debug!("bridge port: connection from {peer} dropped: {why}");
                return;
            }
        };
        let (tx, rx) = mpsc::channel(OUT_QUEUE);
        let link = Arc::new(Link {
            id: self.next.fetch_add(1, Ordering::Relaxed),
            peer,
            out: tx,
            pending: Mutex::new(HashMap::new()),
            version: hello,
            proto,
            ping_at: Mutex::new(None),
            rtt_ms: AtomicU32::new(u32::MAX),
            caps: Mutex::new(Vec::new()),
            catalog: Mutex::new(Vec::new()),
            server: Mutex::new(None),
            perms_mode: Mutex::new(PermsMode::Bridge),
            perms_sent: Mutex::new(HashMap::new()),
            closed: OnceLock::new(),
            close: Notify::new(),
        });
        debug!("bridge from {peer} authenticated, pairing");
        self.lock().unpaired.insert(link.id, (link.clone(), 0));
        self.wake.notify_one();
        let why = self.run(&link, sock, codec, buf, rx).await;
        self.dropped(&link, &why);
    }

    pub(super) fn reject(&self, ip: IpAddr, why: String) {
        let mut st = self.lock();
        if st.rejected.len() > 1024 {
            st.rejected.clear();
        }
        st.rejected.insert(ip, (Instant::now(), why));
    }

    #[allow(clippy::type_complexity)]
    async fn handshake(
        &self,
        sock: &mut TcpStream,
        said_hello: &mut bool,
    ) -> Result<((String, String, String), [u16; 2], Codec, Vec<u8>), String> {
        let mut codec = Codec::new(Side::Proxy);
        let mut buf = Vec::new();
        let Msg::Hello {
            proto,
            bridge,
            pumpkin,
            mc,
            instance,
            nb,
        } = read_msg(sock, &mut codec, &mut buf).await?
        else {
            return Err("expected hello".into());
        };
        *said_hello = true;
        if proto[0] != PROTO[0] {
            return Err(format!(
                "bridge protocol {}.{}, this proxy speaks {}.x",
                proto[0], proto[1], PROTO[0]
            ));
        }
        let mut np = [0u8; 32];
        aws_lc_rs::rand::fill(&mut np).map_err(|_| "random generator failed".to_string())?;
        let proof = wire::proxy_proof(&self.key, &nb, &np, &instance);
        let frame = codec
            .encode(&Msg::Challenge { np, proof })
            .map_err(|e| e.to_string())?;
        sock.write_all(&frame).await.map_err(|e| e.to_string())?;
        // A bridge with another key sees a wrong proof here and hangs up.
        let auth = read_msg(sock, &mut codec, &mut buf)
            .await
            .map_err(|_| "closed after the challenge: wrong bridge key?".to_string())?;
        let Msg::Auth { proof } = auth else {
            return Err("expected auth".into());
        };
        if !wire::check_bridge_proof(&self.key, &np, &nb, &instance, &proof) {
            return Err("wrong bridge key".into());
        }
        codec.set_key(wire::session_key(&self.key, &nb, &np));
        Ok(((bridge, pumpkin, mc), proto, codec, buf))
    }

    /// The session until it ends; returns why it ended.
    async fn run(
        &self,
        link: &Arc<Link>,
        sock: TcpStream,
        mut codec: Codec,
        mut buf: Vec<u8>,
        mut out: mpsc::Receiver<Msg>,
    ) -> String {
        let (mut rd, mut wr) = sock.into_split();
        let mut tx = codec.clone();
        let mut last_rx = Instant::now();
        // A zero interval would panic, a tiny one ping in a busy loop.
        let silence = Duration::from_millis(self.cfg.test_silence_ms.max(300));
        let mut ping = tokio::time::interval(silence / 3);
        'session: loop {
            loop {
                match codec.decode(&buf) {
                    Ok(Some((m, n))) => {
                        buf.drain(..n);
                        last_rx = Instant::now();
                        self.on_msg(link, m);
                    }
                    Ok(None) => break,
                    Err(e) => break 'session e.to_string(),
                }
            }
            if let Some(why) = link.closed.get() {
                break why.clone();
            }
            tokio::select! {
                r = rd.read_buf(&mut buf) => match r {
                    Ok(0) => break "the bridge closed the connection".into(),
                    Ok(_) => {}
                    Err(e) => break format!("read: {e}"),
                },
                Some(m) = out.recv() => {
                    let mut bytes = match tx.encode(&m) {
                        Ok(b) => b,
                        Err(e) => break format!("cannot send: {e}"),
                    };
                    while let Ok(m) = out.try_recv() {
                        match tx.encode(&m) {
                            Ok(b) => bytes.extend(b),
                            Err(e) => break 'session format!("cannot send: {e}"),
                        }
                    }
                    let began = Instant::now();
                    if let Err(e) = write_moving(&mut wr, &bytes).await {
                        break e;
                    }
                    // Nothing is read while a write runs, so its time does
                    // not count as silence of the bridge.
                    last_rx += began.elapsed();
                }
                _ = ping.tick() => {
                    if last_rx.elapsed() > silence {
                        break format!("silent for {} s (no answer to pings)", silence.as_secs_f64());
                    }
                    if let Ok(mut p) = link.ping_at.lock() {
                        p.get_or_insert_with(Instant::now);
                    }
                    link.send(Msg::Ping);
                }
                () = link.close.notified() => break link.closed.get().cloned().unwrap_or_default(),
            }
        }
    }

    /// A session ended: one line says why, its server loses the bridge,
    /// calls in flight end with `disconnected`.
    fn dropped(&self, link: &Arc<Link>, why: &str) {
        // Sets still queued are lost: none counts as sent.
        if let Ok(mut s) = link.perms_sent.lock() {
            s.clear();
        }
        let mut lost = None;
        {
            let mut st = self.lock();
            st.unpaired.remove(&link.id);
            if let Some(server) = link.server()
                && let Some(slot) = st.servers.get_mut(&server)
                && slot.link.as_ref().is_some_and(|l| l.id == link.id)
            {
                slot.link = None;
                slot.since = Some(SystemTime::now());
                lost = Some(server);
            }
        }
        if let Ok(mut p) = link.pending.lock() {
            for (_, tx) in p.drain() {
                let _ = tx.send(Err((err::DISCONNECTED.into(), None)));
            }
        }
        match link.server() {
            Some(server) => info!("bridge on {server} closed: {why}"),
            None => warn!("bridge from {} closed before pairing: {why}", link.peer),
        }
        if let Some(server) = lost {
            if let Some(p) = self.proxy().and_then(|p| p.plugins.get().cloned()) {
                p.host.set_server_value(&server, "tps", None);
                p.host.set_server_value(&server, "mspt", None);
            }
            self.publish_status(&server);
        }
    }

    fn on_msg(&self, link: &Arc<Link>, m: Msg) {
        match m {
            Msg::Info { caps, catalog } => {
                if let Ok(mut c) = link.caps.lock() {
                    *c = caps;
                }
                if let Ok(mut c) = link.catalog.lock() {
                    *c = catalog;
                }
            }
            Msg::Seen { nonce } => self.pair(link, &nonce),
            Msg::Sync { .. } => self.synced(link),
            Msg::Res {
                id,
                ok,
                err: e,
                detail,
            } => {
                let tx = link.pending.lock().ok().and_then(|mut p| p.remove(&id));
                if let Some(tx) = tx {
                    let r = match e {
                        None => Ok(ok.unwrap_or(ciborium::Value::Null)),
                        Some(code) => Err((code, detail)),
                    };
                    let _ = tx.send(r);
                }
            }
            Msg::Ev { ev } => self.event(link, ev),
            Msg::StatsServer { tps, mspt, .. } => {
                if let (Some(server), Some(p)) = (
                    link.server(),
                    self.proxy().and_then(|p| p.plugins.get().cloned()),
                ) {
                    p.host
                        .set_server_value(&server, "tps", Some(format!("{tps:.1}")));
                    p.host
                        .set_server_value(&server, "mspt", Some(format!("{mspt:.1}")));
                }
            }
            Msg::StatsPlayer { players } => {
                let Some(p) = self.proxy().and_then(|p| p.plugins.get().cloned()) else {
                    return;
                };
                for s in players {
                    let gm = s.gamemode.map(|g| {
                        wire::value(&g)
                            .ok()
                            .and_then(|v| v.as_text().map(str::to_string))
                            .unwrap_or_default()
                    });
                    let vals = [
                        ("health", s.health.map(|h| format!("{h:.0}"))),
                        ("food", s.food.map(|f| f.to_string())),
                        ("level", s.level.map(|l| l.to_string())),
                        ("gamemode", gm),
                        ("world", s.world),
                    ];
                    for (k, v) in vals {
                        if let Some(v) = v {
                            p.set_player_value(s.player, k, v);
                        }
                    }
                }
            }
            Msg::Ping => {
                link.send(Msg::Pong);
            }
            Msg::Pong => {
                if let Some(t) = link.ping_at.lock().ok().and_then(|mut p| p.take()) {
                    let ms = u32::try_from(t.elapsed().as_millis()).unwrap_or(u32::MAX - 1);
                    link.rtt_ms.store(ms, Ordering::Relaxed);
                }
            }
            _ => {}
        }
    }
}

async fn read_msg(
    sock: &mut TcpStream,
    codec: &mut Codec,
    buf: &mut Vec<u8>,
) -> Result<Msg, String> {
    loop {
        if let Some((m, n)) = codec.decode(buf).map_err(|e| e.to_string())? {
            buf.drain(..n);
            return Ok(m);
        }
        if sock.read_buf(buf).await.map_err(|e| e.to_string())? == 0 {
            return Err("closed during the handshake".into());
        }
    }
}
