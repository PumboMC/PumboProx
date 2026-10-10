//! One player connection: PROXY header, handshake, status or login, the
//! backend login with forwarding, then relaying configuration and play
//! (plan §2.2, §2.7, §2.9, §3.1).
//!
//! The order of the first join follows Velocity: the client finishes login
//! with the proxy and waits in configuration while the proxy logs in to the
//! backend; the play part (relaying, switching) is in `play`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use pumbo_core::identity::{AuthOutcome, AuthRequest};
use pumbo_core::listener::Incoming;
use pumbo_core::profile::{ForwardedPlayer, GameProfile, Property};
use pumbo_core::translate::{PacketTranslator, TranslationPlan};
use pumbo_protocol::frame::{FrameConfig, FrameError};
use pumbo_protocol::packets::common::{self as pc, CookieRequest, CookieResponse, Disconnect};
use pumbo_protocol::packets::login::{
    CustomQuery, CustomQueryAnswer, EncryptionRequest, EncryptionResponse, LoginAcknowledged,
    LoginCompression, LoginDisconnect, LoginFinished, LoginStart,
};
use pumbo_protocol::packets::status::{Intention, PingRequest, PongResponse, StatusResponse};
use pumbo_protocol::packets::{self, Ctx, Packet};
use pumbo_protocol::types::{DecodeError, EncodeError};
use pumbo_protocol::{Direction, PacketKind, Phase, ProtocolVersion, RawFrame, VersionModule};
use pumbo_text::{Component, TextFormat};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{Instant, timeout, timeout_at};
use tracing::{Instrument as _, debug, info, warn};
use uuid::Uuid;

use crate::config::{OnlineMode, split_host_port};
use crate::conn::{Conn, ConnError};
use crate::limits::IpReject;
use crate::metrics::{GaugeGuard, Metrics};
use crate::net::parse_proxy_header;
use crate::server::{ListenerSettings, PlayerGuard, Proxy, Runtime, SessionCmd};
use crate::status;

/// Frame limits from the client per phase (§2.7).
const MAX_HANDSHAKE_FRAME: usize = 1024;
const MAX_LOGIN_FRAME: usize = 8192;
/// Time to read a PROXY header on a trusted listener (§3.4).
const PROXY_HEADER_TIMEOUT: Duration = Duration::from_secs(2);
/// TCP connect plus login with a backend.
pub(crate) const BACKEND_LOGIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a phase ended early.
#[derive(Debug)]
pub(crate) enum Fail {
    /// Tell the client this, then close.
    Kick(Box<Component>),
    /// Broken or hostile peer: close without a message.
    Protocol(String),
    Closed,
    Timeout,
}

impl From<ConnError> for Fail {
    fn from(e: ConnError) -> Self {
        match e {
            ConnError::Closed | ConnError::Io(_) => Fail::Closed,
            ConnError::WriteTimeout => Fail::Timeout,
            ConnError::Frame(e) => Fail::Protocol(e.to_string()),
        }
    }
}

impl From<FrameError> for Fail {
    fn from(e: FrameError) -> Self {
        Fail::Protocol(e.to_string())
    }
}

impl From<DecodeError> for Fail {
    fn from(e: DecodeError) -> Self {
        Fail::Protocol(e.to_string())
    }
}

impl From<EncodeError> for Fail {
    fn from(e: EncodeError) -> Self {
        Fail::Protocol(format!("encoding: {e}"))
    }
}

fn kick(text: impl Into<String>) -> Fail {
    Fail::Kick(Box::new(Component::text(text)))
}

/// Entry point for an accepted connection.
pub async fn handle(proxy: Arc<Proxy>, listener: Arc<ListenerSettings>, incoming: Incoming) {
    let span = tracing::info_span!("session", id = %crate::logging::session_id());
    run(proxy, listener, incoming).instrument(span).await;
}

async fn run(proxy: Arc<Proxy>, listener: Arc<ListenerSettings>, incoming: Incoming) {
    let m = proxy.metrics.clone();
    Metrics::inc(&m.connections);
    let rt = proxy.runtime();
    let Ok(pending) = rt.pending.clone().try_acquire_owned() else {
        Metrics::inc(&m.rejected_pending);
        return;
    };
    let pending_gauge = GaugeGuard::new(m.clone(), |m| &m.pending_logins);
    let limits = &rt.config.limits;
    let mut client = Conn::new(
        incoming.transport,
        FrameConfig {
            max_frame: MAX_HANDSHAKE_FRAME,
            ..FrameConfig::from_client()
        },
        FrameConfig::from_client(),
    );
    let mut peer = incoming.peer;
    if listener.proxy_protocol && listener.is_trusted(peer.ip()) {
        match timeout(PROXY_HEADER_TIMEOUT, read_proxy_header(&mut client)).await {
            Ok(Ok(Some(source))) => peer = source,
            Ok(Ok(None)) => {}
            Ok(Err(e)) => {
                Metrics::inc(&m.rejected_proxy_header);
                debug!("PROXY header from {}: {e}", proxy.ip(peer));
                return;
            }
            Err(_) => {
                Metrics::inc(&m.rejected_proxy_header);
                return;
            }
        }
    }
    let _ip_guard = match proxy.ips.connect(peer.ip(), limits) {
        Ok(g) => g,
        Err(IpReject::Rate) => return Metrics::inc(&m.rejected_ip_rate),
        Err(IpReject::Concurrent) => return Metrics::inc(&m.rejected_ip_concurrent),
    };
    let hs_deadline = Instant::now() + Duration::from_millis(limits.handshake_timeout_ms);
    let first = match timeout_at(hs_deadline, handshake(&mut client)).await {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => return note_fail(&m, &e),
        Err(_) => return Metrics::inc(&m.timeouts_handshake),
    };
    let intention = match first {
        First::Legacy(modern) => {
            Metrics::inc(&m.legacy_pings);
            let motd = rt.motd.plain_text();
            let reply = status::legacy_response(
                modern,
                &rt.version_name,
                &motd,
                proxy.online(),
                rt.config.status.max_players,
            );
            let _ = client.write_raw(&reply).await;
            client.shutdown().await;
            return;
        }
        First::Intention(i) => i,
    };
    let protocol = ProtocolVersion(intention.protocol);
    let module = proxy.versions.get(protocol).cloned();
    match intention.intent {
        Intention::STATUS => {
            if !proxy.ips.status(peer.ip(), limits) {
                return Metrics::inc(&m.rejected_status_rate);
            }
            Metrics::inc(&m.status_pings);
            let module = match &module {
                Some(md) => md.clone(),
                None => proxy.newest(),
            };
            let r = timeout_at(
                hs_deadline,
                answer_status(
                    &proxy,
                    &rt,
                    &mut client,
                    &*module,
                    protocol,
                    peer,
                    &intention,
                ),
            )
            .await;
            if let Ok(Err(e)) = r {
                note_fail(&m, &e);
            }
        }
        Intention::LOGIN | Intention::TRANSFER => {
            let login_module = module.clone().unwrap_or_else(|| proxy.newest());
            let deadline = Instant::now() + Duration::from_millis(limits.login_timeout_ms);
            let result = timeout_at(
                deadline,
                login(
                    &proxy,
                    &rt,
                    &mut client,
                    module.as_deref(),
                    &intention,
                    peer,
                ),
            )
            .await
            .unwrap_or(Err(Fail::Timeout));
            let (profile, guard, cmds) = match result {
                Ok(ok) => ok,
                Err(Fail::Kick(text)) => {
                    Metrics::inc(&m.logins_denied);
                    let _ = send_login_kick(&mut client, &*login_module, &text).await;
                    client.shutdown().await;
                    return;
                }
                Err(Fail::Timeout) => return Metrics::inc(&m.timeouts_login),
                Err(e) => return note_fail(&m, &e),
            };
            drop(pending);
            drop(pending_gauge);
            let Some(module) = module else { return };
            let host = normalize_host(&intention.address);
            crate::play::run(
                &proxy, &rt, client, module, profile, guard, cmds, peer, &host,
            )
            .await;
        }
        _ => {
            Metrics::inc(&m.protocol_errors);
        }
    }
}

fn note_fail(m: &Metrics, e: &Fail) {
    match e {
        Fail::Protocol(why) => {
            Metrics::inc(&m.protocol_errors);
            debug!("protocol error: {why}");
        }
        Fail::Timeout => Metrics::inc(&m.timeouts_handshake),
        Fail::Kick(_) | Fail::Closed => {}
    }
}

async fn read_proxy_header(client: &mut Conn) -> Result<Option<SocketAddr>, String> {
    loop {
        match parse_proxy_header(client.buffered()) {
            Ok(Some((n, source))) => {
                client.consume(n);
                return Ok(source);
            }
            Ok(None) => {
                client.fill().await.map_err(|e| e.to_string())?;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

enum First {
    /// Pre-1.7 ping; `true` when the client sent `FE 01`.
    Legacy(bool),
    Intention(Intention),
}

async fn handshake(client: &mut Conn) -> Result<First, Fail> {
    while client.buffered().is_empty() {
        client.fill().await?;
    }
    if client.buffered().first() == Some(&0xFE) {
        if client.buffered().len() < 2 {
            let _ = timeout(Duration::from_millis(200), client.fill()).await;
        }
        return Ok(First::Legacy(client.buffered().get(1) == Some(&0x01)));
    }
    let (f, _) = client.read_frame().await?;
    if f.id != 0 {
        return Err(Fail::Protocol(format!("handshake with packet {}", f.id)));
    }
    let mut r = pumbo_protocol::types::Reader::new(&f.payload);
    // The handshake layout is the same in every version, so no module is needed.
    let intention = Intention {
        protocol: r.varint()?,
        address: r.string(255)?,
        port: r.u16()?,
        intent: r.varint()?,
    };
    r.finish()?;
    Ok(First::Intention(intention))
}

/// Host from the handshake: no Forge/FML suffix after `\0`, no trailing dot,
/// lower case (§3.2).
pub fn normalize_host(address: &str) -> String {
    address
        .split('\0')
        .next()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

pub(crate) fn ctx(module: &dyn VersionModule, direction: Direction) -> Ctx<'_> {
    Ctx::new(module, direction)
}

/// Queues a packet towards the client.
pub(crate) fn queue_out<P: Packet>(
    conn: &mut Conn,
    module: &dyn VersionModule,
    phase: Phase,
    p: &P,
) -> Result<(), Fail> {
    queue(conn, module, phase, Direction::Clientbound, p)
}

pub(crate) fn queue<P: Packet>(
    conn: &mut Conn,
    module: &dyn VersionModule,
    phase: Phase,
    direction: Direction,
    p: &P,
) -> Result<(), Fail> {
    let id = module.packet_id(phase, direction, P::KIND).ok_or_else(|| {
        Fail::Protocol(format!(
            "{:?} has no {:?} in {phase:?}",
            module.protocol(),
            P::KIND
        ))
    })?;
    let payload = packets::encode(p, &ctx(module, direction))?;
    conn.queue(id, &payload)?;
    Ok(())
}

fn expect<P: Packet>(
    module: &dyn VersionModule,
    phase: Phase,
    direction: Direction,
    f: &RawFrame,
) -> Result<P, Fail> {
    if module.packet_kind(phase, direction, f.id) != Some(P::KIND) {
        return Err(Fail::Protocol(format!(
            "expected {:?}, got packet {}",
            P::KIND,
            f.id
        )));
    }
    Ok(packets::decode(&f.payload, &ctx(module, direction))?)
}

async fn answer_status(
    proxy: &Proxy,
    rt: &Runtime,
    client: &mut Conn,
    module: &dyn VersionModule,
    protocol: ProtocolVersion,
    peer: SocketAddr,
    intention: &Intention,
) -> Result<(), Fail> {
    let shown = if proxy.versions.get(protocol).is_some() {
        protocol
    } else {
        module.protocol()
    };
    let mut answered = false;
    loop {
        let (f, _) = client.read_frame().await?;
        match module.packet_kind(Phase::Status, Direction::Serverbound, f.id) {
            // One status per connection, like vanilla: no amplification by repeats.
            Some(PacketKind::StatusRequest) if !answered => {
                answered = true;
                let mut motd = rt.motd.clone();
                let mut online = u32::try_from(proxy.online()).unwrap_or(u32::MAX);
                let mut max = rt.config.status.max_players;
                let mut favicon = rt.favicon.is_some();
                if let Some(p) = proxy.plugins.get() {
                    // Placeholders in the MOTD: push and cached values only,
                    // the status is a flood target (§5.8.3).
                    if rt.config.status.motd.contains('%') {
                        motd = p.render(None, &rt.config.status.motd);
                    }
                    let conn = crate::plugins::connection(
                        peer,
                        &normalize_host(&intention.address),
                        intention.protocol,
                    );
                    match p.status(conn, &motd, online, max, favicon).await {
                        pumbo_host::StatusOutcome::Keep => {}
                        pumbo_host::StatusOutcome::Change {
                            motd: m,
                            online: o,
                            max: x,
                            favicon: f,
                        } => (motd, online, max, favicon) = (m, o, x, f),
                        pumbo_host::StatusOutcome::Cancel => return Ok(()),
                    }
                }
                // A plugin can hide the icon, not add one.
                let icon = rt.favicon.as_deref().filter(|_| favicon);
                let json = status::status_json(
                    &rt.version_name,
                    shown.0,
                    online as usize,
                    max,
                    &motd,
                    icon,
                );
                queue_out(client, module, Phase::Status, &StatusResponse { json })?;
                client.flush().await?;
            }
            Some(PacketKind::PingRequest) => {
                let ping: PingRequest =
                    packets::decode(&f.payload, &ctx(module, Direction::Serverbound))?;
                queue_out(
                    client,
                    module,
                    Phase::Status,
                    &PongResponse { time: ping.time },
                )?;
                client.shutdown().await;
                return Ok(());
            }
            _ => return Err(Fail::Protocol(format!("status packet {}", f.id))),
        }
    }
}

async fn send_login_kick(
    client: &mut Conn,
    module: &dyn VersionModule,
    text: &Component,
) -> Result<(), Fail> {
    let reason_json = text.to_json(TextFormat::for_protocol(module.protocol().0));
    queue_out(
        client,
        module,
        Phase::Login,
        &LoginDisconnect { reason_json },
    )?;
    client.flush().await?;
    Ok(())
}

/// Names the vanilla server accepts: 1–16 of `A-Z a-z 0-9 _` (§2.7).
pub fn valid_name(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N], Fail> {
    let mut b = [0u8; N];
    aws_lc_rs::rand::fill(&mut b).map_err(|_| Fail::Protocol("no randomness".into()))?;
    Ok(b)
}

fn version_kick(proxy: &Proxy, protocol: ProtocolVersion) -> Fail {
    let (oldest, newest) = proxy.release_range();
    if proxy.versions.newest().is_some_and(|n| protocol > n) {
        kick(format!("This server supports Minecraft up to {newest}."))
    } else {
        kick(format!(
            "Unsupported Minecraft version. Use {oldest} to {newest}."
        ))
    }
}

/// Login with the client: online or offline, encryption, compression,
/// `login_finished`, `login_acknowledged`. The client is in configuration
/// afterwards.
async fn login(
    proxy: &Arc<Proxy>,
    rt: &Runtime,
    client: &mut Conn,
    module: Option<&dyn VersionModule>,
    intention: &Intention,
    peer: SocketAddr,
) -> Result<(GameProfile, PlayerGuard, mpsc::Receiver<SessionCmd>), Fail> {
    let m = &proxy.metrics;
    let protocol = ProtocolVersion(intention.protocol);
    if intention.intent == Intention::TRANSFER && !rt.config.login.accept_transfers {
        return Err(Fail::Kick(Box::new(Component::translatable(
            "multiplayer.disconnect.transfers_disabled",
            Vec::new(),
        ))));
    }
    // The translation chain decides at handshake (e.g. ViaProxy for old
    // clients, E11); in E3 only native versions get further.
    let plan = rt
        .modules
        .translators
        .iter()
        .find_map(|t| t.plan(protocol, None));
    let (Some(TranslationPlan::Passthrough), Some(module)) = (plan, module) else {
        return Err(version_kick(proxy, protocol));
    };
    client.inbound.max_frame = MAX_LOGIN_FRAME;
    let (f, _) = client.read_frame().await?;
    let start: LoginStart = expect(module, Phase::Login, Direction::Serverbound, &f)?;
    if !valid_name(&start.name) {
        return Err(kick("Invalid username."));
    }
    let login_cfg = &rt.config.login;
    let auth = &rt.modules.authenticator;
    // Plugins (E5): closed logins and `on-pre-login` may decide online mode.
    let plugins = proxy.plugins.get();
    let conn = crate::plugins::connection(
        peer,
        &normalize_host(&intention.address),
        intention.protocol,
    );
    let forced = match plugins {
        Some(p) => p
            .pre_login(conn.clone(), &start.name)
            .await
            .map_err(|t| Fail::Kick(Box::new(t)))?,
        None => None,
    };
    let online = match forced.map_or(login_cfg.online_mode, |o| {
        if o {
            OnlineMode::Always
        } else {
            OnlineMode::Never
        }
    }) {
        OnlineMode::Always => true,
        OnlineMode::Never => false,
        OnlineMode::PerPlayer => {
            let _slot = rt.auth_slots.acquire().await.map_err(|_| Fail::Closed)?;
            match auth.is_premium(&start.name).await {
                Ok(premium) => premium,
                Err(e) => {
                    Metrics::inc(&m.auth_unavailable);
                    warn!("premium check for {} failed: {e}", start.name);
                    return Err(kick(
                        "Could not check your account right now. Please try again later.",
                    ));
                }
            }
        }
    };
    let mut authenticated = None;
    if online || login_cfg.encrypt_offline {
        let token = random_bytes::<4>()?;
        queue_out(
            client,
            module,
            Phase::Login,
            &EncryptionRequest {
                server_id: String::new(),
                public_key: proxy.key.public_der().to_vec(),
                verify_token: token.to_vec(),
                should_authenticate: online,
            },
        )?;
        client.flush().await?;
        let (f, _) = client.read_frame().await?;
        let resp: EncryptionResponse = expect(module, Phase::Login, Direction::Serverbound, &f)?;
        // Both values are decrypted before either is checked, and every
        // failure ends the same way, so the proxy is no padding oracle (§2.7).
        let secret = proxy.key.decrypt(&resp.shared_secret);
        let echoed = proxy.key.decrypt(&resp.verify_token);
        let secret = match (secret, echoed) {
            (Ok(s), Ok(t))
                if s.len() == 16
                    && aws_lc_rs::constant_time::verify_slices_are_equal(&t, &token).is_ok() =>
            {
                s
            }
            _ => return Err(Fail::Protocol("invalid encryption response".into())),
        };
        client
            .enable_encryption(&secret)
            .map_err(|e| Fail::Protocol(e.to_string()))?;
        if online {
            let server_hash = pumbo_identity::server_hash(&[b"", &secret, proxy.key.public_der()]);
            let _slot = rt.auth_slots.acquire().await.map_err(|_| Fail::Closed)?;
            let started = Instant::now();
            let result = auth
                .authenticate(AuthRequest {
                    username: start.name.clone(),
                    server_hash,
                    client_ip: login_cfg.prevent_proxy_connections.then_some(peer.ip()),
                })
                .await;
            Metrics::inc(&m.auth_requests);
            Metrics::add(
                &m.auth_micros,
                u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            );
            match result {
                Ok(AuthOutcome::Authenticated(p)) => authenticated = Some(p),
                Ok(AuthOutcome::Rejected) => {
                    Metrics::inc(&m.auth_failures);
                    return Err(Fail::Kick(Box::new(Component::translatable(
                        "multiplayer.disconnect.unverified_username",
                        Vec::new(),
                    ))));
                }
                Err(e) => {
                    Metrics::inc(&m.auth_unavailable);
                    warn!("authentication of {} failed: {e}", start.name);
                    return Err(kick(
                        "Authentication servers are unavailable. Please try again later.",
                    ));
                }
            }
        }
    }
    let mut profile = authenticated.unwrap_or_else(|| GameProfile {
        id: pumbo_identity::offline_uuid(&start.name),
        name: start.name.clone(),
        properties: Vec::new(),
    });
    if let Some(p) = plugins {
        // Profile patches, `on-login` and the gates (E5).
        profile = p
            .joined(profile, online, conn)
            .await
            .map_err(|t| Fail::Kick(Box::new(t)))?;
    }
    let Some((guard, cmds)) = proxy.register_player(&profile) else {
        return Err(kick("You are already connected to this proxy."));
    };
    Metrics::inc(if online {
        &m.logins_online
    } else {
        &m.logins_offline
    });
    let threshold = login_cfg.compression_threshold;
    if threshold >= 0 {
        queue_out(
            client,
            module,
            Phase::Login,
            &LoginCompression { threshold },
        )?;
        client.flush().await?;
        client.set_compression(threshold);
    }
    let session_id = Uuid::from_bytes(random_bytes::<16>()?);
    queue_out(
        client,
        module,
        Phase::Login,
        &LoginFinished {
            profile: to_wire(&profile),
            strict_error_handling: true,
            session_id: Some(session_id),
        },
    )?;
    client.flush().await?;
    let (f, _) = client.read_frame().await?;
    let _: LoginAcknowledged = expect(module, Phase::Login, Direction::Serverbound, &f)?;
    client.inbound.max_frame = pumbo_protocol::frame::MAX_FRAME;
    info!(
        "{} ({}) logged in from {}, {}",
        profile.name,
        profile.id,
        proxy.ip(peer),
        if online { "online" } else { "offline" }
    );
    Ok((profile, guard, cmds))
}

pub(crate) fn to_wire(p: &GameProfile) -> pc::GameProfile {
    pc::GameProfile {
        id: p.id,
        name: p.name.clone(),
        properties: p
            .properties
            .iter()
            .map(|p| pc::Property {
                name: p.name.clone(),
                value: p.value.clone(),
                signature: p.signature.clone(),
            })
            .collect(),
    }
}

fn from_wire(p: &pc::GameProfile) -> GameProfile {
    GameProfile {
        id: p.id,
        name: p.name.clone(),
        properties: p
            .properties
            .iter()
            .map(|p| Property {
                name: p.name.clone(),
                value: p.value.clone(),
                signature: p.signature.clone(),
            })
            .collect(),
    }
}

/// Why connecting to one backend failed.
#[derive(Debug)]
pub(crate) enum BackendFail {
    /// The backend refused the player (`login_disconnect`, JSON text).
    Refused(String),
    /// Not reachable or broken; the next candidate may work.
    Unavailable(String),
    /// A plugin sent the player to another server (`on-server-connect`, E5).
    Redirect(String),
}

impl From<ConnError> for BackendFail {
    fn from(e: ConnError) -> Self {
        BackendFail::Unavailable(e.to_string())
    }
}

impl From<DecodeError> for BackendFail {
    fn from(e: DecodeError) -> Self {
        BackendFail::Unavailable(format!("decoding: {e}"))
    }
}

impl From<Fail> for BackendFail {
    fn from(e: Fail) -> Self {
        BackendFail::Unavailable(format!("{e:?}"))
    }
}

/// A backend's version module and the translator between it and the client.
pub(crate) type BackendVersion = (Arc<dyn VersionModule>, Option<Box<dyn PacketTranslator>>);

/// The backend's version and the translator between it and the client, if the
/// translation chain lets this client on this backend (plan §1.1, E3b).
pub(crate) fn backend_version(
    proxy: &Proxy,
    rt: &Runtime,
    client: ProtocolVersion,
    backend: Option<ProtocolVersion>,
) -> Option<BackendVersion> {
    let plan = rt
        .modules
        .translators
        .iter()
        .find_map(|t| t.plan(client, backend))?;
    match plan {
        TranslationPlan::Passthrough => Some((proxy.versions.get(client)?.clone(), None)),
        TranslationPlan::Translate(t) => Some((proxy.versions.get(backend?)?.clone(), Some(t))),
        TranslationPlan::External { .. } => None,
    }
}

pub(crate) async fn backend_login(
    rt: &Runtime,
    module: &dyn VersionModule,
    address: &str,
    player: &ForwardedPlayer,
) -> Result<Conn, BackendFail> {
    let (host, port) = split_host_port(address)
        .ok_or_else(|| BackendFail::Unavailable(format!("bad address {address}")))?;
    let stream = TcpStream::connect(address)
        .await
        .map_err(|e| BackendFail::Unavailable(format!("connect: {e}")))?;
    let _ = stream.set_nodelay(true);
    let mut conn = Conn::new(
        Box::pin(stream),
        // Backends are trusted to frame correctly; lenient like the vanilla client.
        FrameConfig {
            strict: false,
            ..FrameConfig::from_backend()
        },
        FrameConfig::from_client(),
    );
    let forwarding = &rt.modules.forwarding;
    let sb = Direction::Serverbound;
    queue(
        &mut conn,
        module,
        Phase::Handshake,
        sb,
        &Intention {
            protocol: module.protocol().0,
            address: forwarding.handshake_address(host, player),
            port,
            intent: Intention::LOGIN,
        },
    )?;
    queue(
        &mut conn,
        module,
        Phase::Login,
        sb,
        &LoginStart {
            name: player.profile.name.clone(),
            uuid: player.profile.id,
        },
    )?;
    conn.flush().await?;
    let cb = ctx(module, Direction::Clientbound);
    let mut forwarded = false;
    loop {
        let (f, _) = conn.read_frame().await?;
        match module.packet_kind(Phase::Login, Direction::Clientbound, f.id) {
            Some(PacketKind::CustomQuery) => {
                let q: CustomQuery = packets::decode(&f.payload, &cb)?;
                let data = match forwarding.answer_login_query(&q.channel, &q.data, player) {
                    Some(Ok(d)) => {
                        forwarded = true;
                        Some(d)
                    }
                    Some(Err(e)) => {
                        return Err(BackendFail::Unavailable(format!("forwarding: {e}")));
                    }
                    None => None,
                };
                queue(
                    &mut conn,
                    module,
                    Phase::Login,
                    sb,
                    &CustomQueryAnswer {
                        message_id: q.message_id,
                        data,
                    },
                )?;
                conn.flush().await?;
            }
            Some(PacketKind::LoginCompression) => {
                let c: LoginCompression = packets::decode(&f.payload, &cb)?;
                conn.set_compression(c.threshold);
            }
            Some(PacketKind::CookieRequest) => {
                let c: CookieRequest = packets::decode(&f.payload, &cb)?;
                queue(
                    &mut conn,
                    module,
                    Phase::Login,
                    sb,
                    &CookieResponse {
                        key: c.key,
                        payload: None,
                    },
                )?;
                conn.flush().await?;
            }
            Some(PacketKind::Hello) => {
                return Err(BackendFail::Unavailable(
                    "backend is in online mode; set online_mode = false on it".into(),
                ));
            }
            Some(PacketKind::LoginDisconnect) => {
                let d: LoginDisconnect = packets::decode(&f.payload, &cb)?;
                return Err(BackendFail::Refused(d.reason_json));
            }
            Some(PacketKind::LoginFinished) => {
                let done: LoginFinished = packets::decode(&f.payload, &cb)?;
                let seen = from_wire(&done.profile);
                if forwarding.expects_login_query() {
                    if !forwarded {
                        return Err(BackendFail::Unavailable(
                            "backend did not ask for the player's identity; enable Velocity modern forwarding on it".into(),
                        ));
                    }
                    if seen.id != player.profile.id || seen.name != player.profile.name {
                        return Err(BackendFail::Unavailable(format!(
                            "backend changed the identity to {} ({})",
                            seen.name, seen.id
                        )));
                    }
                } else if seen.id != player.profile.id {
                    debug!("backend uses UUID {} (no forwarding)", seen.id);
                }
                queue(&mut conn, module, Phase::Login, sb, &LoginAcknowledged)?;
                conn.flush().await?;
                return Ok(conn);
            }
            _ => {
                return Err(BackendFail::Unavailable(format!(
                    "unexpected login packet {}",
                    f.id
                )));
            }
        }
    }
}

pub(crate) async fn kick_in(
    client: &mut Conn,
    module: &dyn VersionModule,
    phase: Phase,
    text: &Component,
) -> Result<(), Fail> {
    let reason = text.to_nbt(TextFormat::for_protocol(module.protocol().0));
    queue_out(client, module, phase, &Disconnect { reason })?;
    client.flush().await?;
    Ok(())
}

/// Resolves once the proxy is stopping (also if it already is). The watch
/// guard is dropped inside, so callers stay `Send`.
pub async fn stopped(rx: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = rx.wait_for(|stop| *stop).await;
}

/// Queues a frame to the other side: the received body when both sides use
/// the same compression threshold (no recompression, §2.2), otherwise
/// encoded again. A translated connection (E3b) has no body to pass on
/// (empty) and gets every frame through its translator.
pub(crate) fn forward(
    to: &mut Conn,
    f: &RawFrame,
    body: &Bytes,
    same_threshold: bool,
) -> Result<(), FrameError> {
    if same_threshold && !body.is_empty() && !to.translates() {
        to.queue_body(body)
    } else {
        to.queue(f.id, &f.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_hosts() {
        assert!(valid_name("Notch"));
        assert!(valid_name("jeb_"));
        assert!(valid_name("a"));
        assert!(!valid_name(""));
        assert!(!valid_name("seventeen_chars_x"));
        assert!(!valid_name("bad name"));
        assert!(!valid_name("zażółć"));
        assert_eq!(
            normalize_host("Play.Example.ORG.\0FML3\0"),
            "play.example.org"
        );
        assert_eq!(normalize_host("mc.example.org"), "mc.example.org");
    }
}
