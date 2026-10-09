//! A player for proxy tests (plan E4): logs in, follows reconfigurations
//! (server switches), answers keep-alives, pings, known packs and resource
//! packs, and records what it saw. With a chat key it signs chat like the
//! vanilla client (chain index, last-seen window and checksum, §2.8).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_lc_rs::encoding::{AsDer, PublicKeyX509Der};
use aws_lc_rs::rsa::{KeyPair, KeySize};
use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256};
use pumbo_protocol::packets::commands::Commands;
use pumbo_protocol::packets::common::{
    ClientInformation, ClientboundCustomPayload, CookieRequest, CookieResponse, Disconnect,
    KeepAlive, Ping, Pong, ResourcePackPop, ResourcePackPush, ResourcePackResponse,
    ServerboundCustomPayload,
};
use pumbo_protocol::packets::configuration::{
    AcceptCodeOfConduct, FinishConfiguration, SelectKnownPacks,
};
use pumbo_protocol::packets::login::{
    EncryptionRequest, LoginAcknowledged, LoginCompression, LoginDisconnect, LoginStart,
};
use pumbo_protocol::packets::play::{
    ArgumentSignature, BossAction, BossEvent, Chat, ChatCommand, ChatCommandSigned,
    ChatSessionUpdate, CommandSuggestions, ConfigurationAcknowledged, LastSeen, Login,
    SetTitleText, SystemChat, TabList,
};
use pumbo_protocol::packets::status::Intention;
use pumbo_protocol::packets::world::{
    AcceptTeleportation, ChunkBatchFinished, ChunkBatchReceived, ClientTickEnd, ContainerSetSlot,
    LevelChunkWithLight, MOVE_ON_GROUND, MapItemData, MovePlayer, MovePlayerPos, MovePlayerPosRot,
    PlayerLoaded, PlayerPosition, Respawn,
};
use pumbo_protocol::types::{Reader, WriteExt};
use pumbo_protocol::{Direction, PacketKind, Phase, RawFrame, VersionModule};
use uuid::Uuid;

use crate::client::{Client, ClientError};

/// How to join.
#[derive(Debug, Clone)]
pub struct JoinOptions {
    pub name: String,
    /// Host in the handshake (forced hosts).
    pub host: String,
    pub proxy_source: Option<SocketAddr>,
    pub brand: String,
    /// ID of `player_chat` in this protocol, to read other players' chat.
    pub player_chat_id: Option<i32>,
    /// Confirm the offered known packs (`false`: a client without packs,
    /// which gets the full registry data).
    pub known_packs: bool,
}

impl JoinOptions {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            host: "127.0.0.1".into(),
            proxy_source: None,
            brand: "vanilla".into(),
            player_chat_id: None,
            known_packs: true,
        }
    }
}

/// A chat key pair; its public half must be signed by the services key the
/// server trusts.
pub struct ChatKey {
    pair: KeyPair,
    pub public_der: Vec<u8>,
}

impl ChatKey {
    pub fn generate() -> Result<Self, ClientError> {
        let pair = KeyPair::generate(KeySize::Rsa2048)
            .map_err(|_| ClientError::Protocol("key generation".into()))?;
        let der: PublicKeyX509Der<'static> = pair
            .public_key()
            .as_der()
            .map_err(|_| ClientError::Protocol("key encoding".into()))?;
        Ok(Self {
            public_der: der.as_ref().to_vec(),
            pair,
        })
    }

    /// What the services key signs (SHA1withRSA): profile UUID, expiry, key.
    pub fn signed_payload(&self, profile: Uuid, expires_at: i64) -> Vec<u8> {
        let mut out = profile.as_bytes().to_vec();
        out.extend_from_slice(&expires_at.to_be_bytes());
        out.extend_from_slice(&self.public_der);
        out
    }

    fn sign(&self, data: &[u8]) -> Result<Box<[u8; 256]>, ClientError> {
        let mut sig = Box::new([0u8; 256]);
        self.pair
            .sign(
                &RSA_PKCS1_SHA256,
                &aws_lc_rs::rand::SystemRandom::new(),
                data,
                sig.as_mut_slice(),
            )
            .map_err(|_| ClientError::Protocol("signing".into()))?;
        Ok(sig)
    }
}

/// The client side of signed chat: chain index and the last-seen window.
struct Signer {
    key: ChatKey,
    sender: Uuid,
    session: Uuid,
    index: i32,
    window: [Option<[u8; 256]>; 20],
    tail: usize,
    offset: i32,
    last: Option<[u8; 256]>,
    last_timestamp: i64,
}

impl Signer {
    fn add_pending(&mut self, sig: [u8; 256]) {
        if self.last == Some(sig) {
            return;
        }
        if let Some(slot) = self.window.get_mut(self.tail) {
            *slot = Some(sig);
        }
        self.tail = (self.tail + 1) % 20;
        self.offset += 1;
        self.last = Some(sig);
    }

    /// The acknowledgement fields and the seen signatures, oldest first.
    fn update(&mut self) -> (LastSeen, Vec<[u8; 256]>) {
        let offset = std::mem::take(&mut self.offset);
        let mut bits = [0u8; 3];
        let mut seen = Vec::new();
        for i in 0..20 {
            if let Some(Some(sig)) = self.window.get((self.tail + i) % 20) {
                if let Some(b) = bits.get_mut(i / 8) {
                    *b |= 1 << (i % 8);
                }
                seen.push(*sig);
            }
        }
        // LastSeenMessages.computeChecksum: 31 * h + Arrays.hashCode(sig), 0 → 1.
        let mut h: i32 = 1;
        for sig in &seen {
            let mut a: i32 = 1;
            for b in sig {
                a = a.wrapping_mul(31).wrapping_add(i32::from(*b as i8));
            }
            h = h.wrapping_mul(31).wrapping_add(a);
        }
        let checksum = match h as u8 {
            0 => 1,
            c => c,
        };
        (
            LastSeen {
                offset,
                acknowledged: bits,
                checksum,
            },
            seen,
        )
    }

    fn timestamp(&mut self) -> i64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        self.last_timestamp = now.max(self.last_timestamp + 1);
        self.last_timestamp
    }

    /// Signature of one message body; advances the chain index.
    fn sign(
        &mut self,
        content: &str,
        timestamp: i64,
        salt: i64,
        seen: &[[u8; 256]],
    ) -> Result<Box<[u8; 256]>, ClientError> {
        let mut data = Vec::new();
        data.extend_from_slice(&1i32.to_be_bytes());
        data.extend_from_slice(self.sender.as_bytes());
        data.extend_from_slice(self.session.as_bytes());
        data.extend_from_slice(&self.index.to_be_bytes());
        data.extend_from_slice(&salt.to_be_bytes());
        data.extend_from_slice(&(timestamp / 1000).to_be_bytes());
        let bytes = content.as_bytes();
        data.extend_from_slice(&i32::try_from(bytes.len()).unwrap_or(0).to_be_bytes());
        data.extend_from_slice(bytes);
        data.extend_from_slice(&i32::try_from(seen.len()).unwrap_or(0).to_be_bytes());
        for s in seen {
            data.extend_from_slice(s);
        }
        self.index += 1;
        self.key.sign(&data)
    }
}

/// A connected player.
pub struct Player {
    pub c: Client,
    pub opts: JoinOptions,
    /// Play logins seen (one per server joined).
    pub logins: Vec<Login>,
    /// `start_configuration` packets answered.
    pub reconfigurations: u32,
    /// System messages, plain text.
    pub messages: Vec<String>,
    /// Bodies of `player_chat` messages.
    pub chats: Vec<String>,
    /// How many of them carried a signature.
    pub signed_chats: u32,
    pub packs_pushed: Vec<Uuid>,
    pub packs_popped: Vec<Option<Uuid>>,
    /// Plugin messages from the server.
    pub payloads: Vec<(String, Vec<u8>)>,
    pub commands: Option<Commands>,
    pub suggestions: Vec<CommandSuggestions>,
    /// Every recognized packet from the server, in order.
    pub kinds: Vec<(Phase, PacketKind)>,
    pub disconnect: Option<String>,
    /// Wait this long before acknowledging `start_configuration`.
    pub ack_delay: Duration,
    signer: Option<Signer>,
    /// Last absolute position from `player_position` (confirmed at once,
    /// like the vanilla client) or from our own movement.
    pub position: Option<(f64, f64, f64)>,
    pub teleports: u32,
    /// Chunks received (x, z), with the packet of each.
    pub chunks: Vec<LevelChunkWithLight>,
    pub maps: Vec<MapItemData>,
    pub slots: Vec<ContainerSetSlot>,
    pub titles: Vec<String>,
    pub bossbars: Vec<String>,
    /// Tab headers, plain text.
    pub tab_headers: Vec<String>,
    pub respawns: u32,
    /// `player_loaded` sent (769+), or the world counted as shown before.
    pub worlds_shown: u32,
    teleported: bool,
    batch_done: bool,
    showing: bool,
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

impl Player {
    pub async fn join(
        addr: SocketAddr,
        module: Arc<dyn VersionModule>,
        opts: JoinOptions,
    ) -> Result<Self, ClientError> {
        let mut c = Client::connect_via(addr, module.clone(), opts.proxy_source).await?;
        c.send(&Intention {
            protocol: module.protocol().0,
            address: opts.host.clone(),
            port: addr.port(),
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
                Some(PacketKind::LoginFinished) => break,
                Some(PacketKind::LoginDisconnect) => {
                    let d: LoginDisconnect = c.decode(&f)?;
                    return Err(ClientError::Protocol(format!("refused: {}", d.reason_json)));
                }
                Some(PacketKind::Hello) => {
                    let _: EncryptionRequest = c.decode(&f)?;
                    return Err(ClientError::Protocol(
                        "online login not supported here".into(),
                    ));
                }
                _ => {}
            }
        }
        c.send(&LoginAcknowledged).await?;
        c.phase = Phase::Configuration;
        c.send(&client_information()).await?;
        let mut brand = Vec::new();
        brand.put_string(&opts.brand, 32_767)?;
        c.send(&ServerboundCustomPayload {
            channel: "minecraft:brand".into(),
            data: brand,
        })
        .await?;
        let mut p = Self {
            c,
            opts,
            logins: Vec::new(),
            reconfigurations: 0,
            messages: Vec::new(),
            chats: Vec::new(),
            signed_chats: 0,
            packs_pushed: Vec::new(),
            packs_popped: Vec::new(),
            payloads: Vec::new(),
            commands: None,
            suggestions: Vec::new(),
            kinds: Vec::new(),
            disconnect: None,
            ack_delay: Duration::ZERO,
            signer: None,
            position: None,
            teleports: 0,
            chunks: Vec::new(),
            maps: Vec::new(),
            slots: Vec::new(),
            titles: Vec::new(),
            bossbars: Vec::new(),
            tab_headers: Vec::new(),
            respawns: 0,
            worlds_shown: 0,
            teleported: false,
            batch_done: false,
            showing: false,
        };
        let joined = p
            .pump(Duration::from_secs(30), |p| !p.logins.is_empty())
            .await?;
        if !joined {
            return Err(ClientError::Protocol(format!(
                "no play login: {:?}",
                p.disconnect
            )));
        }
        Ok(p)
    }

    /// Reads for up to `wait`, handling everything, until `until` holds or
    /// the server disconnects; returns whether `until` holds.
    pub async fn pump(
        &mut self,
        wait: Duration,
        mut until: impl FnMut(&Self) -> bool,
    ) -> Result<bool, ClientError> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if until(self) {
                return Ok(true);
            }
            if self.disconnect.is_some() {
                return Ok(false);
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return Ok(false);
            }
            let (kind, f) = match self.c.recv_within(left).await {
                Ok(x) => x,
                Err(ClientError::Timeout) => return Ok(until(self)),
                Err(ClientError::Closed) => {
                    self.disconnect = Some("connection closed".into());
                    return Ok(until(self));
                }
                Err(e) => return Err(e),
            };
            match self.handle(kind, &f).await {
                // The server closed after its last packets (a kick): the reply to one
                // of them fails, the packets already received (the disconnect) are
                // still read.
                Err(ClientError::Io(e)) if closed_by_server(&e) => {}
                r => r?,
            }
        }
    }

    async fn handle(&mut self, kind: Option<PacketKind>, f: &RawFrame) -> Result<(), ClientError> {
        let phase = self.c.phase;
        if let Some(k) = kind {
            self.kinds.push((phase, k));
        }
        match kind {
            Some(PacketKind::KeepAlive) => {
                let k: KeepAlive = self.c.decode(f)?;
                self.c.send(&k).await?;
                self.c.keep_alives += 1;
            }
            Some(PacketKind::Ping) => {
                let p: Ping = self.c.decode(f)?;
                self.c.send(&Pong { id: p.id }).await?;
            }
            Some(PacketKind::Disconnect) => {
                let d: Disconnect = self.c.decode(f)?;
                self.disconnect = Some(
                    pumbo_text::Component::from_nbt(&d.reason)
                        .map(|c| c.plain_text())
                        .unwrap_or_else(|_| format!("{:?}", d.reason)),
                );
            }
            Some(PacketKind::CustomPayload) => {
                let p: ClientboundCustomPayload = self.c.decode(f)?;
                self.payloads.push((p.channel, p.data));
            }
            Some(PacketKind::ResourcePackPush) => {
                let p: ResourcePackPush = self.c.decode(f)?;
                self.packs_pushed.push(p.id);
                for result in [
                    ResourcePackResponse::ACCEPTED,
                    ResourcePackResponse::DOWNLOADED,
                    ResourcePackResponse::SUCCESSFULLY_LOADED,
                ] {
                    self.c
                        .send(&ResourcePackResponse { id: p.id, result })
                        .await?;
                }
            }
            Some(PacketKind::ResourcePackPop) => {
                let p: ResourcePackPop = self.c.decode(f)?;
                self.packs_popped.push(p.id);
            }
            Some(PacketKind::CookieRequest) => {
                let q: CookieRequest = self.c.decode(f)?;
                self.c
                    .send(&CookieResponse {
                        key: q.key,
                        payload: None,
                    })
                    .await?;
            }
            _ => {}
        }
        match (phase, kind) {
            (Phase::Configuration, Some(PacketKind::SelectKnownPacks)) => {
                let offered: SelectKnownPacks = self.c.decode(f)?;
                let reply = if self.opts.known_packs {
                    offered
                } else {
                    SelectKnownPacks { packs: Vec::new() }
                };
                self.c.send(&reply).await?;
            }
            (Phase::Configuration, Some(PacketKind::CodeOfConduct)) => {
                self.c.send(&AcceptCodeOfConduct).await?;
            }
            (Phase::Configuration, Some(PacketKind::FinishConfiguration)) => {
                self.c.send(&FinishConfiguration).await?;
                self.c.phase = Phase::Play;
            }
            (Phase::Play, Some(PacketKind::StartConfiguration)) => {
                tokio::time::sleep(self.ack_delay).await;
                self.c.send(&ConfigurationAcknowledged).await?;
                self.c.phase = Phase::Configuration;
                self.reconfigurations += 1;
            }
            (Phase::Play, Some(PacketKind::Login)) => {
                let login: Login = self.c.decode(f)?;
                self.logins.push(login);
                self.new_level();
            }
            (Phase::Play, Some(PacketKind::Respawn)) => {
                let _: Respawn = self.c.decode(f)?;
                self.respawns += 1;
                self.new_level();
            }
            (Phase::Play, Some(PacketKind::PlayerPosition)) => {
                let p: PlayerPosition = self.c.decode(f)?;
                self.teleports += 1;
                self.c
                    .send(&AcceptTeleportation {
                        id: p.teleport_id,
                        at: Some((p.x, p.y, p.z, p.yaw, p.pitch)),
                    })
                    .await?;
                if p.flags == 0 {
                    self.position = Some((p.x, p.y, p.z));
                    self.c
                        .send(&MovePlayerPosRot(MovePlayer {
                            position: Some((p.x, p.y, p.z)),
                            rotation: Some((p.yaw, p.pitch)),
                            flags: 0,
                        }))
                        .await?;
                    // One position per client tick (26.3 vanilla kicks a second one).
                    self.tick_end().await?;
                }
                self.teleported = true;
                self.maybe_shown().await?;
            }
            (Phase::Play, Some(PacketKind::LevelChunkWithLight)) => {
                self.chunks.push(self.c.decode(f)?);
            }
            (Phase::Play, Some(PacketKind::ChunkBatchFinished)) => {
                let _: ChunkBatchFinished = self.c.decode(f)?;
                self.c
                    .send(&ChunkBatchReceived {
                        chunks_per_tick: 9.0,
                    })
                    .await?;
                self.batch_done = true;
                self.maybe_shown().await?;
            }
            (Phase::Play, Some(PacketKind::MapItemData)) => {
                self.maps.push(self.c.decode(f)?);
            }
            (Phase::Play, Some(PacketKind::ContainerSetSlot)) => {
                self.slots.push(self.c.decode(f)?);
            }
            (Phase::Play, Some(PacketKind::SetTitleText)) => {
                let t: SetTitleText = self.c.decode(f)?;
                if let Ok(c) = pumbo_text::Component::from_nbt(&t.text) {
                    self.titles.push(c.plain_text());
                }
            }
            (Phase::Play, Some(PacketKind::TabList)) => {
                let t: TabList = self.c.decode(f)?;
                if let Ok(c) = pumbo_text::Component::from_nbt(&t.header) {
                    self.tab_headers.push(c.plain_text());
                }
            }
            (Phase::Play, Some(PacketKind::BossEvent)) => {
                let b: BossEvent = self.c.decode(f)?;
                if let BossAction::Add { title, .. } = b.action
                    && let Ok(c) = pumbo_text::Component::from_nbt(&title)
                {
                    self.bossbars.push(c.plain_text());
                }
            }
            (Phase::Play, Some(PacketKind::SystemChat)) => {
                let m: SystemChat = self.c.decode(f)?;
                if let Ok(c) = pumbo_text::Component::from_nbt(&m.content) {
                    self.messages.push(c.plain_text());
                }
            }
            (Phase::Play, Some(PacketKind::Commands)) => {
                self.commands = Some(self.c.decode(f)?);
            }
            (Phase::Play, Some(PacketKind::CommandSuggestions)) => {
                self.suggestions.push(self.c.decode(f)?);
            }
            (Phase::Play, None) if Some(f.id) == self.opts.player_chat_id => {
                self.player_chat(&f.payload)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// `client_tick_end` (768+): closes a client tick.
    pub async fn tick_end(&mut self) -> Result<(), ClientError> {
        let has = self
            .c
            .module()
            .packet_id(
                Phase::Play,
                Direction::Serverbound,
                PacketKind::ClientTickEnd,
            )
            .is_some();
        if has {
            self.c.send(&ClientTickEnd).await?;
        }
        Ok(())
    }

    /// The current level shows (position and chunks there).
    pub fn shown(&self) -> bool {
        !self.showing
    }

    fn new_level(&mut self) {
        // Like the vanilla client: no movement until the new position arrives.
        self.position = None;
        self.showing = true;
        self.teleported = false;
        self.batch_done = false;
    }

    /// Like the vanilla client: once the chunks and the position are there,
    /// the loading screen closes (`player_loaded` from 769).
    async fn maybe_shown(&mut self) -> Result<(), ClientError> {
        if !(self.showing && self.teleported && self.batch_done) {
            return Ok(());
        }
        self.showing = false;
        self.worlds_shown += 1;
        let has = self
            .c
            .module()
            .packet_id(
                Phase::Play,
                Direction::Serverbound,
                PacketKind::PlayerLoaded,
            )
            .is_some();
        if has {
            self.c.send(&PlayerLoaded).await?;
        }
        Ok(())
    }

    /// Falls from the current position like the vanilla client (gravity
    /// 0.08 and drag 0.98 per tick, applied after the move), one movement
    /// packet per 50 ms tick, until `ground` or `ticks`. Handles everything
    /// that arrives meanwhile. Returns the heights reported.
    pub async fn fall(&mut self, ground: f64, ticks: u32) -> Result<Vec<f64>, ClientError> {
        let (x, mut y, z) = self
            .position
            .ok_or(ClientError::Protocol("no position".into()))?;
        let tick_end = self
            .c
            .module()
            .packet_id(
                Phase::Play,
                Direction::Serverbound,
                PacketKind::ClientTickEnd,
            )
            .is_some();
        let mut v: f64 = 0.0;
        let mut reported = Vec::new();
        for _ in 0..ticks {
            if v.abs() < 0.003 {
                v = 0.0;
            }
            y += v;
            v = (v - 0.08) * f64::from(0.98f32);
            let landed = y <= ground;
            if landed {
                y = ground;
            }
            self.c
                .send(&MovePlayerPos(MovePlayer {
                    position: Some((x, y, z)),
                    rotation: None,
                    flags: if landed { MOVE_ON_GROUND } else { 0 },
                }))
                .await?;
            if tick_end {
                self.c.send(&ClientTickEnd).await?;
            }
            reported.push(y);
            self.position = Some((x, y, z));
            self.pump(Duration::from_millis(50), |_| false).await?;
            if landed || self.disconnect.is_some() {
                break;
            }
        }
        Ok(reported)
    }

    /// `player_chat`: tracks signatures for the last-seen window.
    fn player_chat(&mut self, payload: &[u8]) -> Result<(), ClientError> {
        let mut r = Reader::new(payload);
        // The global index came with the chat checksum (770, from recordings).
        if self.c.module().features().chat_checksum {
            r.varint()?;
        }
        r.uuid()?;
        r.varint()?;
        let signature = if r.bool()? {
            let mut s = [0u8; 256];
            s.copy_from_slice(r.take(256)?);
            Some(s)
        } else {
            None
        };
        self.chats.push(r.string(256)?);
        self.signed_chats += u32::from(signature.is_some());
        if let (Some(sig), Some(signer)) = (signature, self.signer.as_mut()) {
            signer.add_pending(sig);
        }
        Ok(())
    }

    pub fn module(&self) -> &Arc<dyn VersionModule> {
        self.c.module()
    }

    /// An unsigned command (no slash).
    pub async fn command(&mut self, command: &str) -> Result<(), ClientError> {
        self.c
            .send(&ChatCommand {
                command: command.into(),
            })
            .await
    }

    /// Sends the chat session and signs chat from now on.
    pub async fn start_chat_session(
        &mut self,
        key: ChatKey,
        expires_at: i64,
        key_signature: Vec<u8>,
    ) -> Result<(), ClientError> {
        let session = Uuid::from_bytes(rand_bytes()?);
        self.c
            .send(&ChatSessionUpdate {
                session_id: session,
                expires_at,
                public_key: key.public_der.clone(),
                key_signature,
            })
            .await?;
        self.signer = Some(Signer {
            key,
            sender: pumbo_identity::offline_uuid(&self.opts.name),
            session,
            index: 0,
            window: [None; 20],
            tail: 0,
            offset: 0,
            last: None,
            last_timestamp: 0,
        });
        Ok(())
    }

    /// The UUID the signer signs as; set it before the session if the server
    /// knows the player under another UUID (e.g. forwarding).
    pub fn set_sender(&mut self, id: Uuid) {
        if let Some(s) = self.signer.as_mut() {
            s.sender = id;
        }
    }

    /// A chat message, signed when a chat session is on. `offset` overrides
    /// the acknowledgement count of an unsigned message (tests of `chat_ack`).
    pub async fn chat(&mut self, message: &str, offset: Option<i32>) -> Result<(), ClientError> {
        let salt = i64::from_be_bytes(rand_bytes()?);
        let packet = match self.signer.as_mut() {
            Some(s) => {
                let timestamp = s.timestamp();
                let (last_seen, seen) = s.update();
                let signature = Some(s.sign(message, timestamp, salt, &seen)?);
                Chat {
                    message: message.into(),
                    timestamp,
                    salt,
                    signature,
                    last_seen,
                }
            }
            None => Chat {
                message: message.into(),
                timestamp: now_ms(),
                salt,
                signature: None,
                last_seen: LastSeen {
                    offset: offset.unwrap_or(0),
                    acknowledged: [0; 3],
                    checksum: 0,
                },
            },
        };
        self.c.send(&packet).await
    }

    /// A command with signed `message` arguments, e.g. `msg Bob hi` with
    /// `[("message", "hi")]`. Without a chat session the signatures are zeros.
    pub async fn signed_command(
        &mut self,
        command: &str,
        args: &[(&str, &str)],
        offset: Option<i32>,
    ) -> Result<(), ClientError> {
        let salt = i64::from_be_bytes(rand_bytes()?);
        let packet = match self.signer.as_mut() {
            Some(s) => {
                let timestamp = s.timestamp();
                let (last_seen, seen) = s.update();
                let mut arguments = Vec::new();
                for (name, value) in args {
                    arguments.push(ArgumentSignature {
                        name: (*name).into(),
                        signature: s.sign(value, timestamp, salt, &seen)?,
                    });
                }
                ChatCommandSigned {
                    command: command.into(),
                    timestamp,
                    salt,
                    arguments,
                    last_seen,
                }
            }
            None => ChatCommandSigned {
                command: command.into(),
                timestamp: now_ms(),
                salt,
                arguments: args
                    .iter()
                    .map(|(name, _)| ArgumentSignature {
                        name: (*name).into(),
                        signature: Box::new([0; 256]),
                    })
                    .collect(),
                last_seen: LastSeen {
                    offset: offset.unwrap_or(0),
                    acknowledged: [0; 3],
                    checksum: 0,
                },
            },
        };
        self.c.send(&packet).await
    }

    pub async fn close(self) {
        let _ = self.c.close().await;
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn rand_bytes<const N: usize>() -> Result<[u8; N], ClientError> {
    let mut b = [0u8; N];
    aws_lc_rs::rand::fill(&mut b).map_err(|_| ClientError::Protocol("no randomness".into()))?;
    Ok(b)
}

/// Phase and direction helpers for tests that read `kinds`.
pub fn saw(p: &Player, phase: Phase, kind: PacketKind) -> usize {
    p.kinds
        .iter()
        .filter(|(ph, k)| *ph == phase && *k == kind)
        .count()
}

fn closed_by_server(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(e.kind(), BrokenPipe | ConnectionReset | ConnectionAborted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_seen_window() -> Result<(), ClientError> {
        let mut s = Signer {
            key: ChatKey::generate()?,
            sender: Uuid::nil(),
            session: Uuid::nil(),
            index: 0,
            window: [None; 20],
            tail: 0,
            offset: 0,
            last: None,
            last_timestamp: 0,
        };
        let (empty, seen) = s.update();
        assert_eq!(
            (empty.offset, empty.acknowledged, seen.len()),
            (0, [0; 3], 0)
        );
        s.add_pending([1; 256]);
        s.add_pending([1; 256]); // the same message twice counts once
        s.add_pending([2; 256]);
        let (u, seen) = s.update();
        assert_eq!(u.offset, 2);
        // Oldest first: the window starts at the tail.
        assert_eq!(u.acknowledged, [0, 0, 0b0000_1100]);
        assert_eq!(seen.len(), 2);
        assert_ne!(u.checksum, 0);
        Ok(())
    }
}
