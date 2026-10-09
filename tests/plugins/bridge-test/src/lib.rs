//! PumboBridge end-to-end test plugin: `/bt …` (also from the proxy console)
//! calls `pumbo:bridge@1.0` and logs `bt <word>: ok …` or `bt <word>: err …`,
//! which the test driver reads from the proxy log.
//!
//! `bt tp <player> <x> <y> <z> [dimension]`, `bt tpp <player> <to>`,
//! `bt spawn <player> [dimension]`, `bt gm <player> <mode>`, `bt heal <player>`,
//! `bt effect <player> <id>|clear`, `bt fly <player> on|off`,
//! `bt give <player> <item> <count>`, `bt inv <player> [main|armor|offhand|ender]`,
//! `bt set <player> <slot> <item> <count>`, `bt clear <player>`,
//! `bt show <viewer> <of>`, `bt qp <player>`, `bt qs <server>`,
//! `bt qe <server>`, `bt status`, `bt send <player> <server> [x y z]`,
//! `bt load <player> <n>` (n queries at once), `bt tpload <player> <n>`
//! (n teleports one after another), `bt raw <method> <server>` (no player).

use std::time::Instant;

use pumbo_bridge_proto::api::{self, GameMode, Part, Pos, Target};
use pumbo_bridge_proto::client::{Bridge, code};
use pumbo_bridge_proto::uuid::Uuid;
use pumbo_bridge_proto::{api::Action, method};
use pumbo_sdk::{Command, CommandEvent};

#[derive(Default)]
struct BridgeTest;

fn log(s: &str) {
    pumbo_sdk::log::info(s);
}

fn player(name: &str) -> Option<Uuid> {
    let p = pumbo_sdk::players::find(name)?;
    Some(Uuid::from_u64_pair(p.profile.id.high, p.profile.id.low))
}

fn f(args: &[String], i: usize) -> f64 {
    args.get(i).and_then(|a| a.parse().ok()).unwrap_or(0.0)
}

fn report<T: std::fmt::Debug>(word: &str, r: Result<T, pumbo_sdk::service::ClientError>) {
    match r {
        Ok(v) => log(&format!("bt {word}: ok {v:?}")),
        Err(e) => log(&format!(
            "bt {word}: err {}",
            code(&e).map_or_else(|| format!("{e}"), str::to_string)
        )),
    }
}

fn mode(s: &str) -> GameMode {
    match s {
        "creative" => GameMode::Creative,
        "adventure" => GameMode::Adventure,
        "spectator" => GameMode::Spectator,
        _ => GameMode::Survival,
    }
}

fn item(id: &str, count: &str) -> api::Item {
    api::Item {
        id: id.into(),
        count: count.parse().unwrap_or(1),
        name: None,
        lore: Vec::new(),
        ench: Vec::new(),
        dmg: None,
        raw: None,
    }
}

async fn run(a: &[String]) {
    let b = Bridge::client();
    let word = a.first().map(String::as_str).unwrap_or_default();
    let who = a.get(1).and_then(|n| player(n));
    let need = |p: Option<Uuid>| {
        if p.is_none() {
            log(&format!("bt {word}: err offline"));
        }
        p
    };
    match word {
        "status" => report(word, b.status(&api::Status::default()).await),
        "qs" | "qe" | "raw" => {
            let server = a.get(if word == "raw" { 2 } else { 1 }).cloned();
            match word {
                "qs" => report(word, b.q_server(&api::QServer { server }).await),
                "qe" => report(
                    word,
                    b.q_entities(&api::QEntities {
                        world: None,
                        server,
                    })
                    .await,
                ),
                _ => {
                    let m = a.get(1).cloned().unwrap_or_default();
                    let r = match m.as_str() {
                        method::Q_SPAWN => b
                            .q_spawn(&api::QSpawn {
                                world: None,
                                server,
                            })
                            .await
                            .map(|p| format!("{p:?}")),
                        _ => Ok("unknown".into()),
                    };
                    report(word, r);
                }
            }
        }
        _ => {
            let Some(p) = need(who) else { return };
            match word {
                "tp" => {
                    let world = a
                        .get(5)
                        .cloned()
                        .unwrap_or_else(|| "minecraft:overworld".into());
                    let to = Target::Pos(Pos {
                        world,
                        x: f(a, 2),
                        y: f(a, 3),
                        z: f(a, 4),
                        yaw: None,
                        pitch: None,
                    });
                    report(
                        word,
                        b.teleport(&api::Teleport {
                            player: p,
                            to,
                            server: None,
                        })
                        .await,
                    );
                }
                "tpp" => {
                    let Some(to) = a.get(2).and_then(|n| player(n)) else {
                        return log("bt tpp: err offline");
                    };
                    report(
                        word,
                        b.teleport(&api::Teleport {
                            player: p,
                            to: Target::Player(to),
                            server: None,
                        })
                        .await,
                    );
                }
                "spawn" => {
                    let to = Target::Spawn(a.get(2).cloned());
                    report(
                        word,
                        b.teleport(&api::Teleport {
                            player: p,
                            to,
                            server: None,
                        })
                        .await,
                    );
                }
                "gm" => {
                    let m = mode(a.get(2).map(String::as_str).unwrap_or_default());
                    report(
                        word,
                        b.set_gamemode(&api::SetGamemode {
                            player: p,
                            mode: m,
                            server: None,
                        })
                        .await,
                    );
                }
                "heal" => {
                    let h = api::Heal {
                        player: p,
                        health: None,
                        food: Some(20),
                        saturation: None,
                        extinguish: true,
                        server: None,
                    };
                    report(word, b.heal(&h).await);
                }
                "effect" => {
                    let id = a.get(2).cloned().unwrap_or_default();
                    let e = api::Effect {
                        player: p,
                        op: if id == "clear" {
                            api::EffectOp::Clear
                        } else {
                            api::EffectOp::Add
                        },
                        id: Some(id),
                        amplifier: Some(1),
                        seconds: Some(60),
                        particles: None,
                        server: None,
                    };
                    report(word, b.effect(&e).await);
                }
                "fly" => {
                    let on = a.get(2).is_some_and(|s| s == "on");
                    let fl = api::Fly {
                        player: p,
                        allow: on,
                        flying: Some(on),
                        speed: None,
                        server: None,
                    };
                    report(word, b.fly(&fl).await);
                }
                "give" => {
                    let it = item(
                        a.get(2).map_or("minecraft:stone", String::as_str),
                        a.get(3).map_or("1", String::as_str),
                    );
                    report(
                        word,
                        b.inv_give(&api::InvGive {
                            player: p,
                            item: it,
                            server: None,
                        })
                        .await,
                    );
                }
                "inv" => {
                    let part = match a.get(2).map(String::as_str) {
                        Some("armor") => Part::Armor,
                        Some("offhand") => Part::Offhand,
                        Some("ender") => Part::Ender,
                        _ => Part::Main,
                    };
                    report(
                        word,
                        b.inv_get(&api::InvGet {
                            player: p,
                            part,
                            server: None,
                        })
                        .await,
                    );
                }
                "set" => {
                    let slot = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                    let it = item(
                        a.get(3).map_or("minecraft:stone", String::as_str),
                        a.get(4).map_or("1", String::as_str),
                    );
                    let s = api::InvSet {
                        player: p,
                        part: Part::Main,
                        slot,
                        item: Some(it),
                        server: None,
                    };
                    report(word, b.inv_set(&s).await);
                }
                "clear" => report(
                    word,
                    b.inv_clear(&api::InvClear {
                        player: p,
                        part: None,
                        server: None,
                    })
                    .await,
                ),
                "show" => {
                    let Some(of) = a.get(2).and_then(|n| player(n)) else {
                        return log("bt show: err offline");
                    };
                    let items = match b
                        .inv_get(&api::InvGet {
                            player: of,
                            part: Part::Main,
                            server: None,
                        })
                        .await
                    {
                        Ok(i) => i,
                        Err(e) => return report::<()>(word, Err(e)),
                    };
                    let s = api::ShowItems {
                        viewer: p,
                        title: r#"{"text":"Inventory"}"#.into(),
                        rows: 4,
                        items,
                        server: None,
                    };
                    report(word, b.show_items(&s).await);
                }
                "qp" => report(
                    word,
                    b.q_player(&api::QPlayer {
                        player: p,
                        server: None,
                    })
                    .await,
                ),
                "send" => {
                    let server = a.get(2).cloned().unwrap_or_default();
                    let mut actions = Vec::new();
                    if a.len() >= 6 {
                        let to = Target::Pos(Pos {
                            world: "minecraft:overworld".into(),
                            x: f(a, 3),
                            y: f(a, 4),
                            z: f(a, 5),
                            yaw: None,
                            pitch: None,
                        });
                        let tp = api::Teleport {
                            player: p,
                            to,
                            server: None,
                        };
                        actions.extend(Action::new(method::TELEPORT, &tp).ok());
                    }
                    let gm = api::SetGamemode {
                        player: p,
                        mode: GameMode::Creative,
                        server: None,
                    };
                    actions.extend(Action::new(method::SET_GAMEMODE, &gm).ok());
                    report(
                        word,
                        b.send_to(&api::SendTo {
                            player: p,
                            server,
                            actions,
                            ttl_ms: None,
                        })
                        .await,
                    );
                }
                "load" => {
                    let n: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(1000);
                    let t0 = Instant::now();
                    let (mut ok, mut errs) = (0, std::collections::BTreeMap::<String, u32>::new());
                    // In waves of 50: the host allows 64 calls in flight per pair.
                    for wave in (0..n).collect::<Vec<_>>().chunks(50) {
                        let calls = wave.iter().map(|i| {
                            let b = b.clone();
                            async move {
                                if i % 2 == 0 {
                                    b.q_player(&api::QPlayer {
                                        player: p,
                                        server: None,
                                    })
                                    .await
                                    .map(|_| ())
                                } else {
                                    let h = api::Heal {
                                        player: p,
                                        health: None,
                                        food: None,
                                        saturation: None,
                                        extinguish: false,
                                        server: None,
                                    };
                                    b.heal(&h).await
                                }
                            }
                        });
                        for r in futures::future::join_all(calls).await {
                            match r {
                                Ok(()) => ok += 1,
                                Err(e) => {
                                    *errs
                                        .entry(code(&e).unwrap_or("other").to_string())
                                        .or_default() += 1
                                }
                            }
                        }
                    }
                    log(&format!(
                        "bt load: ok {ok} of {n} in {} ms, errors {errs:?}",
                        t0.elapsed().as_millis()
                    ));
                }
                "tpload" => {
                    let n: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(100);
                    let t0 = Instant::now();
                    let (mut ok, mut errs) = (0, std::collections::BTreeMap::<String, u32>::new());
                    for i in 0..n {
                        let x = if i % 2 == 0 { 8.5 } else { -8.5 };
                        let to = Target::Pos(Pos {
                            world: "minecraft:overworld".into(),
                            x,
                            y: 120.0,
                            z: 0.5,
                            yaw: None,
                            pitch: None,
                        });
                        match b
                            .teleport(&api::Teleport {
                                player: p,
                                to,
                                server: None,
                            })
                            .await
                        {
                            Ok(()) => ok += 1,
                            Err(e) => {
                                *errs
                                    .entry(code(&e).unwrap_or("other").to_string())
                                    .or_default() += 1
                            }
                        }
                    }
                    log(&format!(
                        "bt tpload: ok {ok} of {n} in {} ms, errors {errs:?}",
                        t0.elapsed().as_millis()
                    ));
                }
                _ => log(&format!("bt {word}: err unknown")),
            }
        }
    }
}

impl pumbo_sdk::Plugin for BridgeTest {
    async fn init(&self) -> Result<(), String> {
        Command::new("bt").usage("/bt <test> ...").register()
    }

    async fn on_command(&self, e: CommandEvent) {
        if e.name == "bt" {
            run(&e.args).await;
        }
    }
}

pumbo_sdk::plugin!(BridgeTest);
