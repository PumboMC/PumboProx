//! Arguments and results of the bridge methods (spec §4.2–4.3, §6.2).
//!
//! Every argument struct has an optional `server`: the server whose bridge
//! runs the call; without it the proxy takes the server the player (or
//! viewer) is on. The bridge ignores the field. Unknown fields are skipped
//! and new fields are optional, so a minor version only adds.
//!
//! Worlds are dimension names (`minecraft:overworld`, `minecraft:the_nether`):
//! Pumpkin gives one level name to all three dimensions of a level. Effects,
//! items and enchantments are `minecraft:...` names; text is a JSON text
//! component.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A position in a world; yaw and pitch are kept when missing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pos {
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GameMode {
    Survival,
    Creative,
    Adventure,
    Spectator,
}

/// Where `teleport` sends a player.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    Pos(Pos),
    /// To another player on the same server.
    Player(Uuid),
    /// To the spawn of a world (`None`: the overworld).
    Spawn(Option<String>),
}

/// The routing fields the proxy reads from any payload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub player: Option<Uuid>,
    #[serde(default)]
    pub viewer: Option<Uuid>,
}

/// `teleport`: held by the proxy while the player has a teleport the client
/// has not confirmed (at most 10 s, then `expired`; a newer one makes it
/// `superseded`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Teleport {
    pub player: Uuid,
    pub to: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetGamemode {
    pub player: Uuid,
    pub mode: GameMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

/// `heal`: health (default: maximum), food, saturation, fire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heal {
    pub player: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub food: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saturation: Option<f32>,
    #[serde(default)]
    pub extinguish: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectOp {
    Add,
    Remove,
    Clear,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub player: Uuid,
    pub op: EffectOp,
    /// `minecraft:speed`; needed for `add` and `remove`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amplifier: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub particles: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fly {
    pub player: Uuid,
    pub allow: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flying: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

/// Part of an inventory. Slots: `main` 0–35 (0–8 hotbar), `armor` 0–3
/// (head, chest, legs, feet), `offhand` 0, `ender` 0–26.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Part {
    Main,
    Armor,
    Offhand,
    Ender,
}

impl Part {
    pub fn size(self) -> u8 {
        match self {
            Part::Main => 36,
            Part::Armor => 4,
            Part::Offhand => 1,
            Part::Ender => 27,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ench {
    pub id: String,
    pub lvl: u8,
}

/// Components in the format of one Pumpkin release (`fmt`, e.g.
/// `pumpkin-0.2.0`): a bridge with the same `fmt` restores the item exactly,
/// another one takes the descriptive fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Raw {
    pub fmt: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub count: u8,
    /// JSON text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// JSON texts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lore: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ench: Vec<Ench>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dmg: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<Raw>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotItem {
    pub slot: u8,
    pub item: Item,
}

/// `inv-get` → every occupied slot of the part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvGet {
    pub player: Uuid,
    pub part: Part,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvSet {
    pub player: Uuid,
    pub part: Part,
    pub slot: u8,
    /// `None` empties the slot.
    #[serde(default)]
    pub item: Option<Item>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

/// `inv-give`: first free (or matching) main slots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvGive {
    pub player: Uuid,
    pub item: Item,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Given {
    /// What did not fit.
    pub leftover: u8,
}

/// `inv-clear`: one part, or main, armor, offhand and ender when `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvClear {
    pub player: Uuid,
    #[serde(default)]
    pub part: Option<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

/// `show-items`: a read-only chest view of copies (base of `/invsee`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShowItems {
    pub viewer: Uuid,
    /// JSON text.
    pub title: String,
    /// 1–6.
    pub rows: u8,
    pub items: Vec<SlotItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

/// `perm-set` (proxy only): decisions for the player on this server.
/// `nodes` become attachments; `extra` are proxy nodes the bridge keeps for
/// itself (e.g. a PumboGuard bypass node).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermSet {
    pub player: Uuid,
    #[serde(default)]
    pub nodes: BTreeMap<String, bool>,
    #[serde(default)]
    pub extra: BTreeMap<String, bool>,
}

/// `perms-export` (proxy only): the proxy's permission table in this
/// server's context as a PumboPerms export (JSON text, format `pumboperms`
/// version 1) and the SHA-256 (hex) of that text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermsExport {
    pub fingerprint: String,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QPlayer {
    pub player: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PlayerInfo {
    pub pos: Pos,
    pub gamemode: GameMode,
    pub health: f32,
    pub max_health: f32,
    pub food: u8,
    pub saturation: f32,
    pub level: i32,
    pub xp: f32,
    pub flying: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QServer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldInfo {
    /// Level name (the same for the dimensions of one level).
    pub name: String,
    pub dimension: String,
    pub players: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Counted by the bridge (task runs per second); Pumpkin's own value is
    /// not usable.
    pub tps: f32,
    pub mspt: f32,
    pub players: u32,
    pub worlds: Vec<WorldInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QSpawn {
    /// Dimension; `None` = the overworld.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

/// `q-entities` → entities per dimension; at most once a second (`busy`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QEntities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

pub type Entities = BTreeMap<String, u32>;

/// `status` (answered by the proxy).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Connected,
    /// There was a session; `since` says when it ended.
    Lost,
    /// Never had a session since the proxy started.
    None,
    /// A bridge from this server's address failed the handshake (a wrong
    /// key, another protocol major); `detail` says which.
    Rejected,
}

/// Who writes the permission attachments on a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermsMode {
    /// The bridge (from the proxy table).
    #[default]
    Bridge,
    /// A local PumboPerms (it has precedence, spec §5.3).
    Local,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ServerStatus {
    pub server: String,
    pub state: State,
    /// Unix milliseconds of the last change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    #[serde(default)]
    pub bridge: String,
    #[serde(default)]
    pub pumpkin: String,
    #[serde(default)]
    pub mc: String,
    #[serde(default)]
    pub caps: Vec<String>,
    #[serde(default)]
    pub perms_mode: PermsMode,
    /// Contract version of the bridge (`1.0`).
    #[serde(default)]
    pub proto: String,
    /// Round trip of the last ping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One command of `send-to`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub method: String,
    pub args: ciborium::Value,
}

impl Action {
    pub fn new<T: Serialize>(method: &str, args: &T) -> Result<Self, String> {
        Ok(Action {
            method: method.to_string(),
            args: ciborium::Value::serialized(args).map_err(|e| e.to_string())?,
        })
    }
}

/// `send-to`: `no-bridge` before the player moves when the target has no
/// bridge; the actions run on the target once the player has joined and the
/// client confirmed its spawn teleport (at most `ttl-ms`, default 30 s).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SendTo {
    pub player: Uuid,
    pub server: String,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round<T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) {
        let mut b = Vec::new();
        ciborium::into_writer(v, &mut b).unwrap();
        let back: T = ciborium::from_reader(b.as_slice()).unwrap();
        assert_eq!(&back, v);
    }

    #[test]
    fn types_round_trip_and_route_reads_any_payload() {
        let p = Uuid::from_u128(7);
        let tp = Teleport {
            player: p,
            to: Target::Pos(Pos {
                world: "minecraft:overworld".into(),
                x: 1.0,
                y: 2.0,
                z: 3.0,
                yaw: None,
                pitch: None,
            }),
            server: Some("lobby".into()),
        };
        round(&tp);
        round(&Teleport {
            player: p,
            to: Target::Spawn(None),
            server: None,
        });
        round(&InvSet {
            player: p,
            part: Part::Ender,
            slot: 3,
            item: None,
            server: None,
        });
        let mut b = Vec::new();
        ciborium::into_writer(&tp, &mut b).unwrap();
        let r: Route = ciborium::from_reader(b.as_slice()).unwrap();
        assert_eq!((r.server.as_deref(), r.player), (Some("lobby"), Some(p)));
        // Unknown fields are skipped (a newer minor version).
        let v = ciborium::Value::Map(vec![
            (
                ciborium::Value::Text("player".into()),
                ciborium::Value::serialized(&p).unwrap(),
            ),
            (
                ciborium::Value::Text("future".into()),
                ciborium::Value::Integer(1.into()),
            ),
        ]);
        let q: QPlayer = v.deserialized().unwrap();
        assert_eq!(q.player, p);
    }
}
