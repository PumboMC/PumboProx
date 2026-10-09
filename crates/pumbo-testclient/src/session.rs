//! A scripted session against a server: login offline, configuration with or
//! without acknowledging the core pack, and a play script that makes a vanilla
//! server send the packets the proxy decodes and accepts the packets the
//! proxy sends.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pumbo_protocol::crypto::encrypt_with_public;
use pumbo_protocol::packets::common::{
    ClientInformation, CookieRequest, CookieResponse, CustomClickAction, Disconnect, KeepAlive,
    Ping, Pong, ResourcePackPush, ResourcePackResponse, ServerboundCustomPayload,
};
use pumbo_protocol::packets::configuration::{
    AcceptCodeOfConduct, FinishConfiguration, SelectKnownPacks,
};
use pumbo_protocol::packets::login::{
    CustomQuery, CustomQueryAnswer, EncryptionRequest, EncryptionResponse, LoginAcknowledged,
    LoginCompression, LoginDisconnect, LoginFinished, LoginStart,
};
use pumbo_protocol::packets::play::{
    Chat, ChatAck, ChatCommand, ChatCommandSigned, ChatSessionUpdate, CommandSuggestion, LastSeen,
    Login,
};
use pumbo_protocol::packets::status::{
    Intention, PingRequest, PongResponse, StatusRequest, StatusResponse,
};
use pumbo_protocol::types::WriteExt;
use pumbo_protocol::{PacketKind, Phase, RawFrame, VersionModule};
use uuid::Uuid;

use crate::client::{Client, ClientError};
use crate::recording::Recorded;

/// What the session does.
#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub name: String,
    /// Confirm `minecraft:core` in known packs (registry data without NBT).
    pub acknowledge_core: bool,
    /// Go on to play and run the script; otherwise stop at the end of
    /// configuration.
    pub play: bool,
    /// Wait for a play keep-alive (vanilla sends one every 15 s).
    pub wait_keep_alive: bool,
    /// Instead of the play script, send only `chat_session_update` with a
    /// real RSA key (the server decodes the key while decoding the packet).
    pub session_update_probe: bool,
    /// Run the operator command script in play (vanilla servers). Without it
    /// the client only idles in play for `idle` and then closes.
    pub script: bool,
    /// Time spent in play answering keep-alives (before the script's end).
    pub idle: Duration,
    /// Send a PROXY v2 header announcing this client address first.
    pub proxy_source: Option<SocketAddr>,
    /// Stop after the server's encryption request (a premium name against a
    /// real sessionserver cannot get further without a Mojang account).
    pub stop_at_encryption: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            name: "PumboTest".into(),
            acknowledge_core: true,
            play: true,
            wait_keep_alive: false,
            session_update_probe: false,
            script: true,
            idle: Duration::ZERO,
            proxy_source: None,
            stop_at_encryption: false,
        }
    }
}

/// The server's encryption request, as the client saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptionSeen {
    pub should_authenticate: bool,
    /// Server hash the client computed (what it would send to Mojang's `join`).
    pub server_hash: String,
    pub public_key_len: usize,
}

/// How the session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// The script finished and closed the connection.
    Finished,
    /// The server disconnected with this reason (plain text).
    Disconnected(String),
}

#[derive(Debug)]
pub struct SessionOutcome {
    pub frames: Vec<Recorded>,
    pub ending: Ending,
    /// What the script observed, for the recorder's report.
    pub notes: Vec<String>,
    pub encryption: Option<EncryptionSeen>,
    pub keep_alives: u32,
}

/// What a session observed on the way.
#[derive(Debug, Default)]
struct Observed {
    notes: Vec<String>,
    encryption: Option<EncryptionSeen>,
}

/// The kick message that ends a successful play script.
pub const FINAL_KICK: &str = "pumbo-testclient done";

/// The reason of a `disconnect` (configuration or play). A server that writes
/// it as a JSON string instead of NBT (the login-phase format) is reported as
/// such instead of failing the session.
fn disconnect_reason(c: &Client, f: &RawFrame) -> String {
    match c.decode::<Disconnect>(f) {
        Ok(d) => text_plain(&d.reason),
        Err(e) => {
            let mut r = pumbo_protocol::types::Reader::new(&f.payload);
            match r.string(262_144) {
                Ok(json) if r.is_empty() => {
                    format!("malformed disconnect (JSON string, not NBT): {json}")
                }
                _ => format!("undecodable disconnect ({e})"),
            }
        }
    }
}

/// A disconnect reason as text: plain for literal text, JSON otherwise (so
/// translation keys keep their arguments, e.g. a decode error).
fn text_plain(tag: &pumbo_nbt::Tag) -> String {
    match pumbo_text::Component::from_nbt(tag) {
        Ok(c) => match c.as_plain_str() {
            Some(s) => s.to_string(),
            None => c.to_json(pumbo_text::TextFormat::V770),
        },
        Err(_) => format!("{tag:?}"),
    }
}

fn brand() -> Vec<u8> {
    let mut data = Vec::new();
    let _ = data.put_string("vanilla", 32767);
    data
}

fn client_information() -> ClientInformation {
    ClientInformation {
        locale: "en_us".into(),
        view_distance: 2,
        chat_mode: 0,
        chat_colors: true,
        skin_parts: 0x7F,
        main_hand: 1,
        text_filtering: false,
        server_listing: true,
        particle_status: 0,
    }
}

/// Commands run as an operator to make the server send the packets under test.
pub fn play_commands(module: &dyn VersionModule) -> Vec<String> {
    let modern = module.features().text_nbt_snake_case;
    let rich = if modern {
        r#"tellraw @s {text:"pumbo",color:"gold",bold:true,click_event:{action:"open_url",url:"https://example.org"},hover_event:{action:"show_text",value:{text:"hover",italic:true}},extra:[" ",{translate:"chat.type.text",with:["a","b"]}]}"#
    } else {
        r#"tellraw @s {"text":"pumbo","color":"gold","bold":true,"clickEvent":{"action":"open_url","value":"https://example.org"},"hoverEvent":{"action":"show_text","contents":{"text":"hover","italic":true}},"extra":[" ",{"translate":"chat.type.text","with":["a","b"]}]}"#
    };
    let mut commands: Vec<String> = [
        "title @s times 10 70 20",
        r#"title @s title "Pumbo""#,
        r#"title @s subtitle "Prox""#,
        r#"title @s actionbar "bar""#,
        "title @s clear",
        "title @s reset",
        rich,
        r#"tellraw @s "plain""#,
        r#"bossbar add pumbo:test "Boss""#,
        "bossbar set pumbo:test players @s",
        "bossbar set pumbo:test value 50",
        "bossbar set pumbo:test color red",
        "bossbar set pumbo:test style notched_10",
        r#"bossbar set pumbo:test name "Boss 2""#,
        "bossbar set pumbo:test visible false",
        "bossbar remove pumbo:test",
        r#"scoreboard objectives add kills dummy "Kills""#,
        "scoreboard objectives setdisplay sidebar kills",
        r#"scoreboard objectives modify kills numberformat fixed "-""#,
        r#"scoreboard objectives modify kills numberformat styled {"color":"red"}"#,
        "scoreboard objectives modify kills numberformat blank",
        "scoreboard objectives modify kills rendertype hearts",
        "scoreboard objectives remove kills",
        r#"team add red "Red""#,
        r#"team modify red prefix "[R] ""#,
        "team modify red color red",
        "team modify red nametagVisibility hideForOtherTeams",
        "team modify red collisionRule never",
        "team modify red friendlyFire false",
        "team join red @s",
        "team leave @s",
        "team remove red",
        // An entity spawn arrives as a bundle.
        "summon minecraft:armor_stand ~ ~ ~ {NoGravity:1b}",
        "kill @e[type=minecraft:armor_stand]",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    if module
        .packet_id(
            Phase::Play,
            pumbo_protocol::Direction::Clientbound,
            PacketKind::ShowDialog,
        )
        .is_some()
    {
        commands.push("dialog show @s minecraft:server_links".into());
        commands.push(r#"dialog show @s {type:"minecraft:notice",title:"Pumbo"}"#.into());
        commands.push("dialog clear @s".into());
    }
    commands
}

/// Runs a session and returns everything that crossed the wire.
pub async fn run(
    addr: SocketAddr,
    module: Arc<dyn VersionModule>,
    opts: &SessionOptions,
) -> Result<SessionOutcome, ClientError> {
    let mut c = Client::connect_via(addr, module.clone(), opts.proxy_source).await?;
    let mut obs = Observed::default();
    let ending = match script(&mut c, opts, &mut obs).await {
        Ok(e) => e,
        Err(ClientError::Closed) => Ending::Disconnected("connection closed".into()),
        Err(e) => {
            obs.notes.push(format!("error: {e}"));
            Ending::Disconnected(format!("client error: {e}"))
        }
    };
    let keep_alives = c.keep_alives;
    let frames = c.close().await;
    Ok(SessionOutcome {
        frames,
        ending,
        notes: obs.notes,
        encryption: obs.encryption,
        keep_alives,
    })
}

async fn script(
    c: &mut Client,
    opts: &SessionOptions,
    obs: &mut Observed,
) -> Result<Ending, ClientError> {
    if let Some(e) = login(c, opts, obs).await? {
        return Ok(e);
    }
    if let Some(e) = configuration(c, opts, &mut obs.notes).await? {
        return Ok(e);
    }
    play(c, opts, &mut obs.notes).await
}

/// Answers an encryption request like a client: a fresh shared secret and the
/// verify token, both encrypted with the server's key, then AES on both sides.
async fn encrypt(c: &mut Client, req: &EncryptionRequest) -> Result<EncryptionSeen, ClientError> {
    let mut secret = [0u8; 16];
    aws_lc_rs::rand::fill(&mut secret)
        .map_err(|_| ClientError::Protocol("no randomness".into()))?;
    let wrap = |data: &[u8]| {
        encrypt_with_public(&req.public_key, data).map_err(|e| ClientError::Protocol(e.to_string()))
    };
    let response = EncryptionResponse {
        shared_secret: wrap(&secret)?,
        verify_token: wrap(&req.verify_token)?,
    };
    c.send(&response).await?;
    c.enable_encryption(&secret)?;
    Ok(EncryptionSeen {
        should_authenticate: req.should_authenticate,
        server_hash: pumbo_identity::server_hash(&[
            req.server_id.as_bytes(),
            &secret,
            &req.public_key,
        ]),
        public_key_len: req.public_key.len(),
    })
}

async fn login(
    c: &mut Client,
    opts: &SessionOptions,
    obs: &mut Observed,
) -> Result<Option<Ending>, ClientError> {
    let protocol = c.module().protocol().0;
    c.send(&Intention {
        protocol,
        address: "127.0.0.1".into(),
        port: 25565,
        intent: Intention::LOGIN,
    })
    .await?;
    c.phase = Phase::Login;
    c.send(&LoginStart {
        name: opts.name.clone(),
        uuid: pumbo_identity::offline_uuid(&opts.name),
    })
    .await?;
    loop {
        let (kind, f) = c.recv().await?;
        match kind {
            Some(PacketKind::LoginCompression) => {
                let p: LoginCompression = c.decode(&f)?;
                c.set_compression(p.threshold);
            }
            Some(PacketKind::LoginFinished) => {
                let _: LoginFinished = c.decode(&f)?;
                c.send(&LoginAcknowledged).await?;
                c.phase = Phase::Configuration;
                return Ok(None);
            }
            Some(PacketKind::CustomQuery) => {
                let q: CustomQuery = c.decode(&f)?;
                c.send(&CustomQueryAnswer {
                    message_id: q.message_id,
                    data: None,
                })
                .await?;
            }
            Some(PacketKind::CookieRequest) => {
                let q: CookieRequest = c.decode(&f)?;
                c.send(&CookieResponse {
                    key: q.key,
                    payload: None,
                })
                .await?;
            }
            Some(PacketKind::LoginDisconnect) => {
                let d: LoginDisconnect = c.decode(&f)?;
                return Ok(Some(Ending::Disconnected(d.reason_json)));
            }
            Some(PacketKind::Hello) => {
                let req: EncryptionRequest = c.decode(&f)?;
                if opts.stop_at_encryption {
                    obs.encryption = Some(EncryptionSeen {
                        should_authenticate: req.should_authenticate,
                        server_hash: String::new(),
                        public_key_len: req.public_key.len(),
                    });
                    return Ok(Some(Ending::Finished));
                }
                obs.encryption = Some(encrypt(c, &req).await?);
            }
            _ => {}
        }
    }
}

async fn configuration(
    c: &mut Client,
    opts: &SessionOptions,
    notes: &mut Vec<String>,
) -> Result<Option<Ending>, ClientError> {
    c.send(&client_information()).await?;
    c.send(&ServerboundCustomPayload {
        channel: "minecraft:brand".into(),
        data: brand(),
    })
    .await?;
    // Part of the vanilla script only: other servers (Pumpkin) may kick for it.
    if opts.play
        && opts.script
        && c.module()
            .packet_id(
                Phase::Configuration,
                pumbo_protocol::Direction::Serverbound,
                PacketKind::CustomClickAction,
            )
            .is_some()
    {
        c.send(&CustomClickAction {
            id: "pumbo:test".into(),
            payload: Some(pumbo_nbt::Tag::Compound(pumbo_nbt::Compound(vec![(
                "x".into(),
                pumbo_nbt::Tag::Int(1),
            )]))),
        })
        .await?;
        notes.push("sent custom_click_action in configuration".into());
    }
    loop {
        let (kind, f) = c.recv().await?;
        match kind {
            Some(PacketKind::SelectKnownPacks) => {
                let offered: SelectKnownPacks = c.decode(&f)?;
                notes.push(format!(
                    "known packs offered: {}",
                    offered
                        .packs
                        .iter()
                        .map(|p| format!("{}:{}@{}", p.namespace, p.id, p.version))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                let reply = if opts.acknowledge_core {
                    offered
                } else {
                    SelectKnownPacks { packs: Vec::new() }
                };
                c.send(&reply).await?;
            }
            Some(PacketKind::KeepAlive) => {
                let k: KeepAlive = c.decode(&f)?;
                c.send(&k).await?;
                c.keep_alives += 1;
            }
            Some(PacketKind::Ping) => {
                let p: Ping = c.decode(&f)?;
                c.send(&Pong { id: p.id }).await?;
            }
            Some(PacketKind::ResourcePackPush) => {
                let p: ResourcePackPush = c.decode(&f)?;
                for result in [
                    ResourcePackResponse::ACCEPTED,
                    ResourcePackResponse::DOWNLOADED,
                    ResourcePackResponse::SUCCESSFULLY_LOADED,
                ] {
                    c.send(&ResourcePackResponse { id: p.id, result }).await?;
                }
                notes.push("answered a resource pack in configuration".into());
            }
            Some(PacketKind::CodeOfConduct) => {
                c.send(&AcceptCodeOfConduct).await?;
                notes.push("accepted the code of conduct".into());
            }
            Some(PacketKind::CookieRequest) => {
                let q: CookieRequest = c.decode(&f)?;
                c.send(&CookieResponse {
                    key: q.key,
                    payload: None,
                })
                .await?;
            }
            Some(PacketKind::Disconnect) => {
                return Ok(Some(Ending::Disconnected(disconnect_reason(c, &f))));
            }
            Some(PacketKind::FinishConfiguration) => {
                if !opts.play {
                    return Ok(Some(Ending::Finished));
                }
                c.send(&FinishConfiguration).await?;
                c.phase = Phase::Play;
                return Ok(None);
            }
            _ => {}
        }
    }
}

/// Handles background traffic; returns a disconnect reason if the server
/// kicked the client.
async fn handle_common(
    c: &mut Client,
    kind: Option<PacketKind>,
    f: &RawFrame,
) -> Result<Option<String>, ClientError> {
    match kind {
        Some(PacketKind::KeepAlive) => {
            let k: KeepAlive = c.decode(f)?;
            c.send(&k).await?;
            c.keep_alives += 1;
        }
        Some(PacketKind::Ping) => {
            let p: Ping = c.decode(f)?;
            c.send(&Pong { id: p.id }).await?;
        }
        Some(PacketKind::Disconnect) => {
            return Ok(Some(disconnect_reason(c, f)));
        }
        _ => {}
    }
    Ok(None)
}

/// Reads for `wait`, answering keep-alives. Stops early when `until` sees a
/// matching kind (returned as `Ok(Some(kind))`).
async fn drain(
    c: &mut Client,
    wait: Duration,
    until: Option<PacketKind>,
) -> Result<Result<Option<PacketKind>, String>, ClientError> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Ok(Ok(None));
        }
        let (kind, f) = match c.recv_within(left).await {
            Ok(x) => x,
            Err(ClientError::Timeout) => return Ok(Ok(None)),
            Err(e) => return Err(e),
        };
        if let Some(reason) = handle_common(c, kind, &f).await? {
            return Ok(Err(reason));
        }
        if kind.is_some() && kind == until {
            return Ok(Ok(kind));
        }
    }
}

fn last_seen() -> LastSeen {
    LastSeen {
        offset: 0,
        acknowledged: [0; 3],
        checksum: 0,
    }
}

async fn play(
    c: &mut Client,
    opts: &SessionOptions,
    notes: &mut Vec<String>,
) -> Result<Ending, ClientError> {
    // The play login comes first.
    loop {
        let (kind, f) = c.recv().await?;
        if let Some(reason) = handle_common(c, kind, &f).await? {
            return Ok(Ending::Disconnected(reason));
        }
        if kind == Some(PacketKind::Login) {
            let login: Login = c.decode(&f)?;
            notes.push(format!(
                "play login: entity {}, enforces_secure_chat {}",
                login.entity_id, login.enforces_secure_chat
            ));
            break;
        }
    }
    macro_rules! step {
        ($wait:expr, $until:expr) => {
            match drain(c, $wait, $until).await? {
                Ok(found) => found,
                Err(reason) => return Ok(Ending::Disconnected(reason)),
            }
        };
    }
    step!(Duration::from_secs(2), None);
    if !opts.script {
        step!(opts.idle, None);
        notes.push(format!("idled {:?} in play", opts.idle));
        return Ok(Ending::Finished);
    }
    if opts.session_update_probe {
        let key = pumbo_protocol::crypto::ServerKey::generate()
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        c.send(&ChatSessionUpdate {
            session_id: Uuid::from_u128(1),
            expires_at: 4_102_444_800_000,
            public_key: key.public_der().to_vec(),
            key_signature: vec![0; 512],
        })
        .await?;
        step!(Duration::from_secs(2), None);
        c.send(&ChatCommand {
            command: format!("kick @s {FINAL_KICK}"),
        })
        .await?;
        return match drain(c, Duration::from_secs(5), None).await? {
            Err(reason) => Ok(Ending::Disconnected(reason)),
            Ok(_) => Ok(Ending::Finished),
        };
    }
    for cmd in play_commands(c.module().as_ref()) {
        c.send(&ChatCommand { command: cmd }).await?;
        step!(Duration::from_millis(400), None);
    }

    // Packets the proxy sends to servers: the server must accept them.
    c.send(&Chat {
        message: "hello from pumbo".into(),
        timestamp: 0,
        salt: 0,
        signature: None,
        last_seen: last_seen(),
    })
    .await?;
    step!(Duration::from_millis(400), None);
    c.send(&ChatAck { offset: 0 }).await?;
    c.send(&client_information()).await?;
    c.send(&ServerboundCustomPayload {
        channel: "minecraft:brand".into(),
        data: brand(),
    })
    .await?;
    c.send(&ResourcePackResponse {
        id: Uuid::from_u128(0x5ca1ab1e),
        result: ResourcePackResponse::DECLINED,
    })
    .await?;
    c.send(&Pong { id: 0 }).await?;
    step!(Duration::from_millis(400), None);
    c.send(&CommandSuggestion {
        id: 7,
        text: "/tim".into(),
    })
    .await?;
    if step!(Duration::from_secs(3), Some(PacketKind::CommandSuggestions)).is_some() {
        notes.push("got command_suggestions".into());
    }
    c.send(&ChatCommandSigned {
        command: "help".into(),
        timestamp: 0,
        salt: 0,
        arguments: Vec::new(),
        last_seen: last_seen(),
    })
    .await?;
    step!(Duration::from_millis(400), None);
    if opts.wait_keep_alive && step!(Duration::from_secs(25), Some(PacketKind::KeepAlive)).is_some()
    {
        notes.push("answered a play keep-alive".into());
    }
    if !opts.idle.is_zero() {
        let before = c.keep_alives;
        step!(opts.idle, None);
        notes.push(format!(
            "idled {:?} in play, answered {} keep-alives",
            opts.idle,
            c.keep_alives - before
        ));
    }
    c.send(&ChatCommand {
        command: "transfer 127.0.0.1 25565 @s".into(),
    })
    .await?;
    step!(Duration::from_millis(400), Some(PacketKind::Transfer));
    c.send(&ChatCommand {
        command: format!("kick @s {FINAL_KICK}"),
    })
    .await?;
    match drain(c, Duration::from_secs(5), None).await? {
        Err(reason) => Ok(Ending::Disconnected(reason)),
        Ok(_) => Ok(Ending::Finished),
    }
}

/// A status ping: `status_request`, `status_response`, `ping_request`,
/// `pong_response`. Returns the frames and the status JSON.
pub async fn status(
    addr: SocketAddr,
    module: Arc<dyn VersionModule>,
) -> Result<(Vec<Recorded>, String), ClientError> {
    let mut c = Client::connect(addr, module.clone()).await?;
    c.send(&Intention {
        protocol: module.protocol().0,
        address: "127.0.0.1".into(),
        port: addr.port(),
        intent: Intention::STATUS,
    })
    .await?;
    c.phase = Phase::Status;
    c.send(&StatusRequest).await?;
    let (_, f) = c.recv().await?;
    let response: StatusResponse = c.decode(&f)?;
    c.send(&PingRequest {
        time: 0x0123_4567_89AB,
    })
    .await?;
    let (_, f) = c.recv().await?;
    let pong: PongResponse = c.decode(&f)?;
    if pong.time != 0x0123_4567_89AB {
        return Err(ClientError::Protocol("pong does not echo the ping".into()));
    }
    Ok((c.close().await, response.json))
}
