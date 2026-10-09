//! Plugin for pumbo-host tests. Behaviour comes from the command, the player
//! name and `config.yml` of the plugin, so one module covers many cases:
//!
//! - commands (with `prefix` from the config): `/spin` (endless loop), `/panic`, `/reg <name>` (registers a
//!   command), `/http <url>`, `/call <service> <method> <n>`, `/flood <n>`,
//!   `/sleep <ms>`, `/echo <text>`, `/alloc <mb>`,
//! - gate by name prefix: `deny_`, `hold_` (released after 50 ms), `slow_`,
//!   `never_` (hold without release), `locale_` (deny with the locale the
//!   gate sees), `early_` (title, action bar and boss bar, then a virtual
//!   world, all before the client is in play), otherwise pass,
//! - `on-pre-login` answers `pre_login` from the config (`force-online`,
//!   `force-offline`, `deny` for `deny_` players) and reports to `report`,
//! - `on-server-connect` to `loop` reconnects the player to `lobby` (import
//!   leading to the same event of the same plugin); to `games` denies
//!   `nogames*` players; a join to `lobby` of `togo*` players goes to `games`,
//! - `on-server-kicked` with "redirect me" in the reason moves to `lobby`,
//! - `on-chat` with `echo:<n>` sends `echo:<n-1>` back to the player,
//! - `on-profile` appends `tag` to the property `trail`,
//! - services: `ping <n>` calls `peer` with `n-1` (cycles A→B→A), `slow`,
//!   `spin`, `hang`, `size <n>` (answer of n bytes),
//! - shared services (part B): `/push <key> <value> [context]`,
//!   `/resolve <template>`, `/pub <topic> <count>`, `/metric <name> <value>
//!   [label]`, `/avail <service> <on|off>`, `/lookup <service>`, `/hasoff
//!   <uuid> <node>`; pull placeholders answer `pull:<key>:<arg>:<server>`
//!   (`slow` after 300 ms); bus events and service changes are reported to
//!   player `report`; a permission provider with `perms` from the config.

#![allow(clippy::panic)]

use std::cell::Cell;

use pumbo_sdk::bindings::pumbo::prox::admin::ParamKind;
use pumbo_sdk::config::Config;
use pumbo_sdk::services::{CallOptions, ServiceError};
use pumbo_sdk::text::{mini, plain};
use pumbo_sdk::{
    Action, Actor, CallReject, ChatReply, Command, CommandEvent, ConnectEvent, ConnectReply,
    Context, Description, GateReply, PermissionEntry, PermissionSet, PlaceholderRequest,
    PlayerInfo, PreLoginEvent, PreLoginReply, ProfilePatch, Property, PropertyChange, QueryContext,
    ServiceCall, ServiceRef, Text, TextTemplate,
};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default)]
struct PermCfg {
    node: String,
    value: bool,
    /// `global`, `server=<name>` or `group=<name>`.
    ctx: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Cfg {
    /// Player that gets reports (bus events, service changes).
    report: u64,
    /// Entries of the permission provider; `fail_load` makes it fail.
    perms: Vec<PermCfg>,
    fail_load: bool,
    level: u32,
    #[schemars(extend("x-pumbo-secret" = true))]
    api_key: String,
    /// Each bus event takes this long (to fill the subscriber queue).
    bus_sleep_ms: u64,
    tag: String,
    peer: String,
    /// `describe` waits on an async import (sync export: wasmtime traps).
    bad_describe: bool,
    /// `init` fails.
    fail_init: bool,
    /// Prefix of the command names, so several instances can be loaded.
    prefix: String,
    /// Answer to `on-pre-login`: `force-online`, `force-offline`, `deny`.
    pre_login: String,
}

#[derive(Default)]
struct TestPlugin {
    config: Config<Cfg>,
    calls: Cell<u64>,
}

fn arg(e: &CommandEvent, i: usize) -> String {
    e.args.get(i).cloned().unwrap_or_default()
}

fn reply(e: &CommandEvent, msg: String) {
    if let Some(p) = e.player {
        pumbo_sdk::players::send_message(p, plain(msg));
    } else {
        pumbo_sdk::log::info(&msg);
    }
}

fn show(t: &Text) -> String {
    match t {
        Text::Plain(s) | Text::Mini(s) | Text::Legacy(s) | Text::Json(s) => s.clone(),
        Text::Template(t) => t.mini.clone(),
    }
}

fn ctx_of(s: &str) -> Context {
    if let Some(g) = s.strip_prefix("group=") {
        Context::Group(g.into())
    } else if let Some(v) = s.strip_prefix("server=") {
        Context::Server(v.into())
    } else {
        Context::Global
    }
}

fn err_name(e: &ServiceError) -> String {
    match e {
        ServiceError::Rejected(s) => format!("rejected({s})"),
        other => format!("{other:?}").to_ascii_lowercase(),
    }
}

async fn call(
    service: &str,
    method: &str,
    n: u64,
    ctx: Option<u64>,
) -> Result<Vec<u8>, ServiceError> {
    pumbo_sdk::services::call(
        service.to_string(),
        method.to_string(),
        n.to_le_bytes().to_vec(),
        CallOptions {
            timeout_ms: None,
            player: None,
            ctx,
        },
    )
    .await
}

fn num(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    for (d, s) in a.iter_mut().zip(b) {
        *d = *s;
    }
    u64::from_le_bytes(a)
}

impl pumbo_sdk::Plugin for TestPlugin {
    async fn init(&self) -> Result<(), String> {
        if self.config.get().fail_init {
            return Err("init failed on purpose".into());
        }
        let prefix = self.config.get().prefix.clone();
        for name in [
            "spin", "panic", "reg", "http", "call", "flood", "sleep", "echo", "alloc", "push",
            "resolve", "pub", "metric", "avail", "lookup", "hasoff", "pset", "prepl", "bar",
            "timer", "every", "cancel", "hash", "file",
        ] {
            Command::new(&format!("{prefix}{name}")).register()?;
        }
        Command::new(&format!("{prefix}secret"))
            .sensitive()
            .register()?;
        Ok(())
    }

    fn describe(&self) -> Description {
        if self.config.get().bad_describe {
            pumbo_sdk::wit_bindgen::block_on(pumbo_sdk::scheduler::sleep(10));
        }
        Description::new()
            .config::<Cfg>()
            .action(
                Action::new("kick", "pumbo.test.kick")
                    .param("player", ParamKind::Player, true, "a.kick.player")
                    .param("reason", ParamKind::Text, false, "a.kick.reason"),
            )
            .action(
                Action::new("wipe", "pumbo.test.wipe")
                    .dangerous()
                    .sensitive(),
            )
            .counter("hits", "", &["kind"], "m.hits")
    }

    async fn on_reload(&self) -> Result<(), String> {
        self.config.reload()
    }

    async fn on_admin_action(
        &self,
        action: String,
        args: Vec<(String, String)>,
        by: Actor,
    ) -> Result<Text, Text> {
        let args: Vec<String> = args.into_iter().map(|(k, v)| format!("{k}={v}")).collect();
        Ok(plain(format!(
            "action {action} {} by {by:?} level {}",
            args.join(","),
            self.config.get().level
        )))
    }

    async fn on_placeholder(&self, reqs: Vec<PlaceholderRequest>) -> Vec<Option<Text>> {
        let mut out = Vec::new();
        for r in reqs {
            if r.key == "slow" {
                pumbo_sdk::scheduler::sleep(300).await;
            }
            if r.key == "inject" {
                out.push(Some(mini(
                    "%test_secret%<click:run_command:/op me>x</click>",
                )));
                continue;
            }
            let server = r.context.server.clone().unwrap_or_else(|| "-".into());
            out.push(Some(plain(format!(
                "pull:{}:{}:{server}",
                r.key,
                r.arg.unwrap_or_default()
            ))));
        }
        out
    }

    async fn on_permission_load(&self, p: PlayerInfo) -> Result<PermissionSet, String> {
        let c = self.config.get();
        pumbo_sdk::players::send_message(c.report, plain(format!("load {}", p.profile.name)));
        if c.fail_load {
            return Err("provider database down".into());
        }
        Ok(PermissionSet {
            entries: c
                .perms
                .iter()
                .map(|p| PermissionEntry {
                    node: p.node.clone(),
                    value: p.value,
                    context: ctx_of(&p.ctx),
                })
                .collect(),
        })
    }

    async fn on_bus_event(
        &self,
        topic: String,
        _major: u16,
        _minor: u16,
        publisher: String,
        payload: Vec<u8>,
    ) {
        let to = self.config.get().report;
        pumbo_sdk::players::send_message(
            to,
            plain(format!("bus {topic} {} from {publisher}", num(&payload))),
        );
    }

    async fn on_timer(&self, timer: u64) {
        let to = self.config.get().report;
        pumbo_sdk::players::send_message(to, plain(format!("fired {timer}")));
    }

    async fn on_context_changed(
        &self,
        p: u64,
        now: pumbo_sdk::PlayerContext,
        previous: Option<pumbo_sdk::PlayerContext>,
    ) {
        let to = self.config.get().report;
        let prev = previous
            .and_then(|c| c.server)
            .unwrap_or_else(|| "-".into());
        pumbo_sdk::players::send_message(
            to,
            plain(format!(
                "ctx {p} {} from {prev}",
                now.server.unwrap_or_else(|| "-".into())
            )),
        );
    }

    async fn on_service_changed(&self, service: String, available: Option<ServiceRef>) {
        let to = self.config.get().report;
        let state = available.map_or("none".to_string(), |r| format!("{}.{}", r.major, r.minor));
        pumbo_sdk::players::send_message(to, plain(format!("changed {service} {state}")));
    }

    async fn on_command(&self, e: CommandEvent) {
        let prefix = self.config.get().prefix.clone();
        match e.name.strip_prefix(prefix.as_str()).unwrap_or_default() {
            "spin" => {
                let mut x: u64 = 0;
                loop {
                    x = std::hint::black_box(x.wrapping_add(1));
                }
            }
            "panic" => panic!("deliberate panic"),
            "reg" => {
                let r = Command::new(&arg(&e, 0)).register();
                reply(&e, format!("reg {r:?}"));
            }
            "http" => {
                let r = pumbo_sdk::http::fetch(pumbo_sdk::http::Request {
                    method: "GET".into(),
                    url: arg(&e, 0),
                    headers: vec![("Authorization".into(), "Bearer secret".into())],
                    body: Vec::new(),
                })
                .await;
                let msg = match r {
                    Ok(resp) => format!(
                        "http ok {} {}",
                        resp.status,
                        String::from_utf8_lossy(&resp.body)
                    ),
                    Err(err) => format!("http err {err:?}"),
                };
                reply(&e, msg);
            }
            "call" => {
                let n = arg(&e, 2).parse().unwrap_or(0);
                let msg = match call(&arg(&e, 0), &arg(&e, 1), n, None).await {
                    Ok(b) => format!("call ok {}", num(&b)),
                    Err(err) => format!("call err {}", err_name(&err)),
                };
                reply(&e, msg);
            }
            "flood" => {
                let n: usize = arg(&e, 0).parse().unwrap_or(0);
                let service = arg(&e, 1);
                let calls = (0..n).map(|_| call(&service, "slow", 0, None));
                let results = futures::future::join_all(calls).await;
                let overloaded = results
                    .iter()
                    .filter(|r| matches!(r, Err(ServiceError::Overloaded)))
                    .count();
                let other: Vec<String> = results
                    .iter()
                    .filter_map(|r| match r {
                        Err(ServiceError::Overloaded) | Ok(_) => None,
                        Err(e) => Some(err_name(e)),
                    })
                    .take(1)
                    .collect();
                reply(&e, format!("flood overloaded={overloaded} other={other:?}"));
            }
            "sleep" => {
                let ms = arg(&e, 0).parse().unwrap_or(0);
                pumbo_sdk::scheduler::sleep(ms).await;
                reply(&e, format!("slept {ms}"));
            }
            "echo" => reply(&e, format!("echo {}", e.args.join(" "))),
            "push" => {
                let key = arg(&e, 0);
                let player = if key.starts_with("g_") {
                    None
                } else {
                    e.player
                };
                let r = pumbo_sdk::placeholders::set(
                    &key,
                    player,
                    mini(arg(&e, 1)),
                    &ctx_of(&arg(&e, 2)),
                );
                reply(&e, format!("push {r:?}"));
            }
            "resolve" => {
                let t = TextTemplate {
                    mini: e.args.join(" "),
                    args: vec![(
                        "arg".into(),
                        "%test_secret% <click:run_command:/op me>y".into(),
                    )],
                };
                let r = pumbo_sdk::placeholders::resolve(t, e.player, QueryContext::Current).await;
                let msg = match r {
                    Ok(t) => format!("resolved {}", show(&t)),
                    Err(err) => format!("resolve err {err:?}"),
                };
                reply(&e, msg);
            }
            "pub" => {
                let n: u64 = arg(&e, 1).parse().unwrap_or(1);
                let mut errors = Vec::new();
                for i in 0..n {
                    if let Err(err) = pumbo_sdk::bus::publish(&arg(&e, 0), &i.to_le_bytes()) {
                        errors.push(err);
                    }
                }
                reply(&e, format!("pub {} {:?}", n, errors.first()));
            }
            "metric" => {
                let v: u64 = arg(&e, 1).parse().unwrap_or(1);
                let labels: Vec<(String, String)> = match e.args.get(2) {
                    Some(l) => vec![("kind".into(), l.clone())],
                    None => Vec::new(),
                };
                let r = pumbo_sdk::metrics::counter_add(&arg(&e, 0), v, &labels);
                reply(&e, format!("metric {r:?}"));
            }
            "avail" => {
                let r = pumbo_sdk::services::set_available(&arg(&e, 0), arg(&e, 1) == "on");
                reply(&e, format!("avail {r:?}"));
            }
            "lookup" => {
                let r = pumbo_sdk::services::lookup(&arg(&e, 0))
                    .map(|r| format!("{}.{}", r.major, r.minor));
                reply(&e, format!("lookup {r:?}"));
            }
            "hasoff" => {
                let high: u64 = arg(&e, 0).parse().unwrap_or(0);
                let id = pumbo_sdk::Uuid { high, low: high };
                let r =
                    pumbo_sdk::permissions::has_offline(id, arg(&e, 1), QueryContext::Global).await;
                reply(&e, format!("hasoff {r:?}"));
            }
            "bar" => {
                if let Some(p) = e.player {
                    let bar = pumbo_sdk::bossbar::Bar::new(
                        &plain("boss"),
                        0.1,
                        pumbo_sdk::bossbar::BossbarColor::Red,
                        pumbo_sdk::bossbar::BossbarOverlay::Progress,
                    );
                    bar.show(p);
                    bar.set_progress(0.5);
                    if arg(&e, 0) != "keep" {
                        bar.hide(p);
                    } else {
                        // Left shown: the host hides it when the instance ends.
                        // The native fake bar has no destructor; the WASM one does.
                        #[allow(clippy::forget_non_drop)]
                        std::mem::forget(bar);
                    }
                }
                reply(&e, "bar ok".into());
            }
            "timer" => {
                let id = pumbo_sdk::scheduler::after(arg(&e, 0).parse().unwrap_or(10));
                reply(&e, format!("timer {id}"));
            }
            "every" => {
                let id = pumbo_sdk::scheduler::every(arg(&e, 0).parse().unwrap_or(50));
                reply(&e, format!("timer {id}"));
            }
            "cancel" => {
                pumbo_sdk::scheduler::cancel(arg(&e, 0).parse().unwrap_or(0));
                reply(&e, "cancelled".into());
            }
            "hash" => {
                let params = pumbo_sdk::crypto::Argon2Params {
                    memory_kib: 4096,
                    iterations: 1,
                    parallelism: 1,
                };
                let msg = match pumbo_sdk::crypto::argon2id_hash(arg(&e, 0), params).await {
                    Ok(phc) => {
                        let good =
                            pumbo_sdk::crypto::argon2id_verify(arg(&e, 0), phc.clone()).await;
                        let bad = pumbo_sdk::crypto::argon2id_verify("wrong".into(), phc).await;
                        format!("hash {good} {bad}")
                    }
                    Err(err) => format!("hash err {err}"),
                };
                reply(&e, msg);
            }
            "file" => {
                let path = format!("/data/{}", arg(&e, 0));
                let written = std::fs::write(&path, arg(&e, 1)).is_ok();
                let back = std::fs::read_to_string(&path).unwrap_or_default();
                let ro = std::fs::write("/config/x.txt", "x").is_err();
                let outside = std::fs::read_to_string("/etc/hosts").is_err();
                reply(
                    &e,
                    format!("file {written} {back} ro={ro} outside={outside}"),
                );
            }
            "pset" => {
                let r = e.player.map(|p| {
                    pumbo_sdk::permissions::set(
                        p,
                        &arg(&e, 0),
                        Some(arg(&e, 1) == "true"),
                        &ctx_of(&arg(&e, 2)),
                    )
                });
                reply(&e, format!("pset {r:?}"));
            }
            "prepl" => {
                let r = e.player.map(|p| {
                    pumbo_sdk::permissions::replace(
                        p,
                        &PermissionSet {
                            entries: Vec::new(),
                        },
                    )
                });
                reply(&e, format!("prepl {r:?}"));
            }
            "alloc" => {
                let mb: usize = arg(&e, 0).parse().unwrap_or(0);
                let v = vec![1u8; mb * 1024 * 1024];
                reply(&e, format!("alloc {}", std::hint::black_box(v).len()));
            }
            _ => {}
        }
    }

    async fn on_virtual_input(&self, batch: Vec<pumbo_sdk::virtual_world::Input>) {
        use pumbo_sdk::virtual_world as v;
        for input in batch {
            match input {
                v::Input::Loaded(id) => {
                    let img = v::MapImage::new(&[34u8; 128 * 128]);
                    v::show_map(id, &img, v::Hand::Main);
                    v::set_xp(id, 0.5, 3);
                    pumbo_sdk::players::send_title(
                        id,
                        "Plugin world",
                        "held by a gate",
                        pumbo_sdk::bindings::pumbo::prox::types::TitleTimes {
                            fade_in: 10,
                            stay: 60,
                            fade_out: 10,
                        },
                    );
                }
                v::Input::Command((id, line)) if line == "release" => {
                    let _ = v::release(id);
                }
                _ => {}
            }
        }
    }

    async fn on_gate(&self, p: PlayerInfo) -> GateReply {
        let name = p.profile.name.as_str();
        if name.starts_with("deny_") {
            GateReply::Deny(mini("<err>denied by test gate"))
        } else if name.starts_with("hold_") {
            let id = p.id;
            pumbo_sdk::scheduler::sleep(50).await;
            let _ = pumbo_sdk::gates::release(id);
            GateReply::Hold
        } else if name.starts_with("locale_") {
            let locale = p.settings.map_or_else(|| "none".into(), |s| s.locale);
            GateReply::Deny(plain(format!("locale {locale}")))
        } else if name.starts_with("virtual_") || name.starts_with("early_") {
            // E6: hold the player in a virtual world; `/release` lets it go.
            use pumbo_sdk::virtual_world as v;
            if name.starts_with("early_") {
                // Before the client is in play: the session keeps these.
                let times = pumbo_sdk::bindings::pumbo::prox::types::TitleTimes {
                    fade_in: 0,
                    stay: 100,
                    fade_out: 0,
                };
                pumbo_sdk::players::send_title(p.id, "Early title", "", times);
                pumbo_sdk::players::send_action_bar(p.id, plain("early bar text"));
                let bar = pumbo_sdk::bossbar::Bar::new(
                    &plain("early bar"),
                    0.2,
                    pumbo_sdk::bossbar::BossbarColor::Red,
                    pumbo_sdk::bossbar::BossbarOverlay::Progress,
                );
                bar.show(p.id);
                bar.set_progress(0.7);
                // Dropping the bar would hide it.
                #[allow(clippy::forget_non_drop)]
                std::mem::forget(bar);
            }
            let w = v::World::new(v::WorldOptions {
                time: 6000,
                light: 15,
                game_mode: v::GameMode::Adventure,
                view_distance: 2,
            });
            let _ = w.set_block(v::BlockPos { x: 0, y: 64, z: 0 }, "minecraft:stone");
            let at = v::Position {
                x: 0.5,
                y: 65.0,
                z: 0.5,
                yaw: 0.0,
                pitch: 0.0,
            };
            match v::enter(p.id, &w, at, &["release".to_string()]) {
                Ok(()) => GateReply::Hold,
                Err(e) => GateReply::Deny(plain(&e)),
            }
        } else if name.starts_with("never_") {
            GateReply::Hold
        } else if name.starts_with("slow_") {
            pumbo_sdk::scheduler::sleep(1000).await;
            GateReply::Pass
        } else {
            GateReply::Pass
        }
    }

    async fn on_server_connect(&self, e: ConnectEvent) -> ConnectReply {
        let name = pumbo_sdk::players::get(e.player)
            .map(|p| p.profile.name)
            .unwrap_or_default();
        if e.target == "games" && name.starts_with("nogames") {
            return ConnectReply::Deny(plain("games closed"));
        }
        if e.target == "lobby" && e.reason == "join" && name.starts_with("togo") {
            return ConnectReply::Redirect("games".into());
        }
        if e.target == "loop" {
            // The import leads to another on-server-connect of this plugin.
            let _ = pumbo_sdk::players::connect(e.player, "lobby".into()).await;
            return ConnectReply::Deny(plain("moved to lobby"));
        }
        ConnectReply::Allow
    }

    async fn on_server_kicked(&self, e: pumbo_sdk::KickedEvent) -> pumbo_sdk::KickedReply {
        if show(&e.reason).contains("redirect me") {
            return pumbo_sdk::KickedReply::Redirect(("lobby".into(), plain("moved")));
        }
        pumbo_sdk::KickedReply::Keep
    }

    async fn on_chat(&self, p: u64, message: String) -> ChatReply {
        if let Some(n) = message
            .strip_prefix("echo:")
            .and_then(|n| n.parse::<u32>().ok())
        {
            if n > 0 {
                pumbo_sdk::players::send_message(p, plain(format!("echo:{}", n - 1)));
            }
            return ChatReply::Cancel;
        }
        ChatReply::Pass
    }

    async fn on_pre_login(&self, e: PreLoginEvent) -> PreLoginReply {
        let c = self.config.get();
        pumbo_sdk::players::send_message(c.report, plain(format!("pre {} {}", c.prefix, e.name)));
        match c.pre_login.as_str() {
            "force-online" => PreLoginReply::ForceOnline,
            "force-offline" => PreLoginReply::ForceOffline,
            "deny" if e.name.starts_with("deny_") => PreLoginReply::Deny(plain("denied")),
            _ => PreLoginReply::Allow,
        }
    }

    async fn on_profile(&self, p: PlayerInfo) -> ProfilePatch {
        let tag = self.config.get().tag.clone();
        let before = p
            .profile
            .properties
            .iter()
            .find(|q| q.name == "trail")
            .map(|q| q.value.clone())
            .unwrap_or_default();
        ProfilePatch {
            id: None,
            name: None,
            properties: vec![PropertyChange::Set(Property {
                name: "trail".into(),
                value: format!("{before}{tag}"),
                signature: None,
            })],
        }
    }

    async fn on_service_call(&self, c: ServiceCall) -> Result<Vec<u8>, CallReject> {
        self.calls.set(self.calls.get() + 1);
        if c.service == pumbo_sdk::contracts::PERMISSIONS.name
            && c.method == pumbo_sdk::contracts::METHOD_FILE
        {
            let f: pumbo_sdk::contracts::PermissionsFile =
                pumbo_sdk::service::from_cbor(&c.payload).map_err(CallReject::Rejected)?;
            let report = self.config.get().report;
            pumbo_sdk::players::send_message(report, plain(format!("file {}", f.data.len())));
            return pumbo_sdk::service::to_cbor(&true).map_err(CallReject::Rejected);
        }
        if c.service == pumbo_sdk::contracts::PERMISSIONS.name {
            let req: pumbo_sdk::contracts::CheckOffline =
                pumbo_sdk::service::from_cbor(&c.payload).map_err(CallReject::Rejected)?;
            let value = self
                .config
                .get()
                .perms
                .iter()
                .find(|p| p.node == req.node && p.ctx == req.context)
                .map(|p| p.value);
            return pumbo_sdk::service::to_cbor(&pumbo_sdk::contracts::CheckOfflineAnswer {
                value,
            })
            .map_err(CallReject::Rejected);
        }
        let n = num(&c.payload);
        match c.method.as_str() {
            "ping" => {
                if n == 0 {
                    return Ok(0u64.to_le_bytes().to_vec());
                }
                let peer = self.config.get().peer.clone();
                match call(&peer, "ping", n - 1, Some(c.ctx)).await {
                    Ok(b) => Ok((num(&b) + 1).to_le_bytes().to_vec()),
                    Err(e) => Err(CallReject::Rejected(err_name(&e))),
                }
            }
            "slow" => {
                pumbo_sdk::scheduler::sleep(10_000).await;
                Ok(Vec::new())
            }
            "hang" => {
                pumbo_sdk::scheduler::sleep(3_600_000).await;
                Ok(Vec::new())
            }
            "spin" => {
                let mut x: u64 = 0;
                loop {
                    x = std::hint::black_box(x.wrapping_add(1));
                }
            }
            "size" => Ok(vec![7u8; usize::try_from(n).unwrap_or(0)]),
            _ => Err(CallReject::UnknownMethod),
        }
    }
}

pumbo_sdk::plugin!(TestPlugin);
