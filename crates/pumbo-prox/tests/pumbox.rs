//! The Pumbox and `/prox claim`: without a server a player waits in the
//! proxy's empty world instead of a kick and joins the server a reload
//! brings; the first operator claims with the code from the console, also
//! without any permission, while every other `/prox` command stays closed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::{message, module};
use pumbo_prox::commands::{self, Source};
use pumbo_prox::config::Config;
use pumbo_prox::server::Proxy;
use pumbo_testclient::{JoinOptions, Player};

const WAIT: Duration = Duration::from_secs(15);

const BASE: &str = "# Test network\nlistener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n\
                    limits:\n  connections-per-ip-per-second: 1000\n  concurrent-per-ip: 1000\n";

/// The config file in a fresh directory of this test.
fn file(tag: &str, text: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("prox-pumbox-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pumboprox.yml");
    std::fs::write(&path, text).unwrap();
    path
}

fn run(proxy: &Arc<Proxy>, who: &Source<'_>, line: &str) -> String {
    let rt = proxy.runtime();
    commands::run(proxy, &rt, who, line)
        .unwrap()
        .lines
        .iter()
        .map(|l| l.plain_text())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_server_players_wait_in_the_pumbox_until_a_reload_brings_one() {
    let path = file("reload", BASE);
    let (proxy, addr) = common::start_proxy_at(BASE, Some(path.clone())).await;
    let code = commands::claim_code(&proxy).unwrap();
    let mut p = Player::join(addr, module(777), JoinOptions::new("Early"))
        .await
        .unwrap();
    assert_eq!(p.logins[0].dimension, "pumbo:virtual");
    assert!(
        p.pump(WAIT, |p| p
            .titles
            .iter()
            .any(|t| t == "Waiting for a server…"))
            .await
            .unwrap(),
        "{:?} {:?}",
        p.titles,
        p.disconnect
    );
    assert!(
        message(&mut p, "No server is available right now.").await,
        "{:?}",
        p.messages
    );
    // Proxy commands run as on a server, with the same permissions; the
    // code from the console is open to everyone.
    assert_eq!(roots(&p), ["server", "prox"]);
    p.command("server").await.unwrap();
    assert!(message(&mut p, "You are on -.").await, "{:?}", p.messages);
    p.command("spawn").await.unwrap();
    assert!(message(&mut p, "Only proxy commands work here.").await);
    p.command("prox reload").await.unwrap();
    assert!(message(&mut p, "You do not have permission to do that.").await);
    // An offline account cannot claim without the login gate (anyone could
    // join under its name); the console can still op it.
    p.command(&format!("prox claim {code}")).await.unwrap();
    assert!(message(&mut p, "Your account is not premium").await);
    assert!(commands::claim_open(&proxy, &proxy.runtime()));
    assert!(run(&proxy, &Source::Console, "prox op Early").contains("Early is now an operator"));
    p.command("prox reload").await.unwrap();
    assert!(
        message(&mut p, "Config reloaded.").await,
        "{:?}",
        p.messages
    );
    // A new server list without a server: still there.
    p.pump(Duration::from_millis(500), |_| false).await.unwrap();
    assert!(p.disconnect.is_none(), "{:?}", p.disconnect);
    assert_eq!(p.logins.len(), 1);
    assert_eq!(proxy.find_player("Early").unwrap().server, None);

    let (lobby, log) = common::backend().await;
    let text = std::fs::read_to_string(&path).unwrap();
    let text =
        format!("{text}servers:\n  lobby: {{ address: \"{lobby}\" }}\nrouting:\n  try: [lobby]\n");
    std::fs::write(&path, text).unwrap();
    proxy.reload().unwrap();
    assert!(
        p.pump(WAIT, |p| p.logins.len() == 2).await.unwrap(),
        "{:?}",
        p.disconnect
    );
    assert_eq!(p.logins[1].dimension, "minecraft:overworld");
    assert_eq!(p.reconfigurations, 1);
    assert_eq!(log.joins.load(std::sync::atomic::Ordering::SeqCst), 1);
    let id = pumbo_identity::offline_uuid("Early");
    for _ in 0..50 {
        if proxy.server_of(id).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(proxy.server_of(id).as_deref(), Some("lobby"));
    p.close().await;

    // Off: the old kick.
    let off = format!("{BASE}pumbox:\n  enabled: false\n");
    let (_proxy, addr) = common::start_proxy(&off).await;
    let err = match Player::join(addr, module(777), JoinOptions::new("Late")).await {
        Err(e) => e.to_string(),
        Ok(_) => panic!("joined"),
    };
    assert!(err.contains("No server is available right now."), "{err}");
    proxy.stop();
}

#[test]
fn a_code_makes_one_operator_and_wrong_codes_close_it() {
    let path = file("claim", BASE);
    let proxy = Proxy::new(Config::parse(BASE).unwrap(), Some(path.clone())).unwrap();
    let (a, b) = (uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2));
    let player = |id| Source::Player { id, server: None };
    let code = commands::claim_code(&proxy).unwrap();
    assert_eq!(code.len(), 6);
    assert!(commands::claim_open(&proxy, &proxy.runtime()));
    assert!(run(&proxy, &Source::Console, "prox claim 1").contains("Only players"));
    assert!(run(&proxy, &player(a), "prox claim").contains("Usage: /prox claim <code>"));
    assert!(run(&proxy, &player(a), "prox claim 1").contains("Wrong code."));
    let done = run(&proxy, &player(a), &format!("prox claim {code}"));
    assert!(done.contains("You are now an operator"), "{done}");
    // In the file, the comment stays; the reload made it count.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# Test network\n"), "{text}");
    assert!(
        text.ends_with(&format!("commands:\n  operators: [{a}]\n")),
        "{text}"
    );
    assert!(proxy.runtime().operators.contains(&a));
    // Used up: nobody claims again, not even with the same code.
    assert!(!commands::claim_open(&proxy, &proxy.runtime()));
    let again = run(&proxy, &player(b), &format!("prox claim {code}"));
    assert!(again.contains("nothing to claim"), "{again}");
    assert!(commands::claim_code(&proxy).is_none(), "an operator exists");

    // Five wrong codes close it until a restart.
    let path = file("claim-wrong", BASE);
    let proxy = Proxy::new(Config::parse(BASE).unwrap(), Some(path.clone())).unwrap();
    let code = commands::claim_code(&proxy).unwrap();
    let wrong = if code == "000000" { "000001" } else { "000000" };
    for _ in 0..5 {
        assert!(run(&proxy, &player(b), &format!("prox claim {wrong}")).contains("Wrong code."));
    }
    assert!(!commands::claim_open(&proxy, &proxy.runtime()));
    let late = run(&proxy, &player(a), &format!("prox claim {code}"));
    assert!(late.contains("no longer works"), "{late}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), BASE);
}

#[test]
fn offline_uuids_become_operators_only_from_the_console() {
    let path = file("offline-op", BASE);
    let proxy = Proxy::new(Config::parse(BASE).unwrap(), Some(path.clone())).unwrap();
    let cracked = pumbo_identity::offline_uuid("Cracked");
    let premium = uuid::Uuid::from_u128(0x853c80ef3c3749fdaa49938b674adae6);
    let player = |id| Source::Player { id, server: None };
    let code = commands::claim_code(&proxy).unwrap();
    let no = run(&proxy, &player(cracked), &format!("prox claim {code}"));
    assert!(no.contains("not premium"), "{no}");
    // The refusal costs no try and leaves the code open.
    assert!(commands::claim_open(&proxy, &proxy.runtime()));
    // The console may op an offline UUID typed in.
    let ok = run(&proxy, &Source::Console, &format!("prox op {cracked}"));
    assert!(ok.contains("now an operator"), "{ok}");
    assert!(proxy.runtime().operators.contains(&cracked));
    // An operator in the game may not add another one, but may remove it.
    let added = run(&proxy, &Source::Console, &format!("prox op {premium}"));
    assert!(added.contains("now an operator"), "{added}");
    let other = pumbo_identity::offline_uuid("Other");
    let no = run(&proxy, &player(premium), &format!("prox op {other}"));
    assert!(no.contains("has no premium account"), "{no}");
    assert!(!proxy.runtime().operators.contains(&other));
    let gone = run(&proxy, &player(premium), &format!("prox deop {cracked}"));
    assert!(gone.contains("no longer an operator"), "{gone}");
    // The start warns about logins without the gate.
    let warnings = commands::login_warnings(&proxy).join("\n");
    assert!(
        warnings.contains("no auth gate") && !warnings.contains("is not set"),
        "{warnings}"
    );
}

/// Completions for `line` as a player would get them.
fn suggest_as(proxy: &Proxy, id: uuid::Uuid, line: &str) -> Vec<String> {
    let rt = proxy.runtime();
    let who = Source::Player { id, server: None };
    commands::suggest(proxy, &rt, &who, line)
        .map(|(_, m)| m)
        .unwrap_or_default()
}

/// Root command names of the client's tree.
fn roots(p: &Player) -> Vec<String> {
    let t = p.commands.as_ref().unwrap();
    t.nodes[t.root as usize]
        .children
        .iter()
        .filter_map(|c| t.nodes[*c as usize].name.clone())
        .collect()
}

/// Tab completion of `text` as the client asks for it.
async fn suggest(p: &mut Player, id: i32, text: &str) -> Vec<String> {
    let before = p.suggestions.len();
    p.c.send(&pumbo_protocol::packets::play::CommandSuggestion {
        id,
        text: text.into(),
    })
    .await
    .unwrap();
    assert!(
        p.pump(WAIT, |p| p.suggestions.len() > before)
            .await
            .unwrap(),
        "no answer to {text}"
    );
    let last = p.suggestions.last().unwrap();
    assert_eq!(last.id, id);
    last.matches.iter().map(|m| m.text.clone()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_player_without_permissions_claims_from_the_pumbox() {
    let path = file("game", BASE);
    let (proxy, addr) = common::start_proxy_at(BASE, Some(path.clone())).await;
    let code = commands::claim_code(&proxy).unwrap();
    let mut p = Player::join(addr, module(777), JoinOptions::new("Owner"))
        .await
        .unwrap();
    let id = pumbo_identity::offline_uuid("Owner");
    // Offered while the code works: the tree, the completion, the help.
    assert!(
        p.pump(WAIT, |p| p
            .commands
            .as_ref()
            .is_some_and(|t| t.nodes.len() > 1))
            .await
            .unwrap()
    );
    assert_eq!(roots(&p), ["server", "prox"]);
    // Arguments the client asks the proxy to complete.
    let t = p.commands.as_ref().unwrap();
    let prox = t
        .nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("prox"))
        .unwrap();
    let arg = &t.nodes[prox.children[0] as usize];
    assert_eq!(arg.suggestions.as_deref(), Some("minecraft:ask_server"));
    assert_eq!(suggest(&mut p, 1, "/prox cl").await, ["claim"]);
    p.command("prox help").await.unwrap();
    assert!(
        message(&mut p, "Become an operator").await,
        "{:?}",
        p.messages
    );
    assert!(!p.messages.iter().any(|m| m.contains("Reloads")));
    // Nothing else of /prox without permissions.
    for denied in ["prox reload", "prox route", "prox op Owner"] {
        p.messages.clear();
        p.command(denied).await.unwrap();
        assert!(
            message(&mut p, "You do not have permission").await,
            "{denied}: {:?}",
            p.messages
        );
    }
    // An offline account without the login gate: refused, the console ops it.
    p.command(&format!("prox claim {code}")).await.unwrap();
    assert!(
        message(&mut p, "Your account is not premium").await,
        "{:?}",
        p.messages
    );
    assert!(run(&proxy, &Source::Console, "prox op Owner").contains("now an operator"));
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains(&format!("operators: [{id}]"))
    );
    p.command("prox reload").await.unwrap();
    assert!(
        message(&mut p, "Config reloaded.").await,
        "{:?}",
        p.messages
    );
    // An operator's tree and completions, as on a server; the code is gone.
    assert!(
        p.pump(WAIT, |p| roots(p).contains(&"glist".to_string()))
            .await
            .unwrap(),
        "{:?}",
        roots(&p)
    );
    let subs = suggest(&mut p, 2, "/prox ").await;
    assert!(subs.contains(&"servers".to_string()), "{subs:?}");
    assert!(!subs.contains(&"claim".to_string()), "{subs:?}");
    // Answered (nothing to offer: this proxy runs no servers).
    assert!(suggest(&mut p, 3, "/prox servers ").await.is_empty());
    assert_eq!(suggest(&mut p, 4, "/prox help ").await, ["1", "2"]);
    assert_eq!(suggest(&mut p, 5, "/prox op Ow").await, ["Owner"]);
    p.close().await;
    proxy.stop();
}

/// Changes of a player's commands reach the session also when its queue is
/// full, and a series of them is one rebuild of the tree.
#[tokio::test]
async fn commands_changed_survives_full_queue() {
    let (proxy, addr) = common::start_proxy_at(BASE, Some(file("queue", BASE))).await;
    let mut p = Player::join(addr, module(777), JoinOptions::new("Alex"))
        .await
        .unwrap();
    assert!(p.pump(WAIT, |p| p.commands.is_some()).await.unwrap());
    p.pump(Duration::from_millis(500), |_| false).await.unwrap();
    let trees = |p: &Player| {
        p.kinds
            .iter()
            .filter(|(_, k)| *k == pumbo_protocol::PacketKind::Commands)
            .count()
    };
    let before = trees(&p);
    let id = pumbo_identity::offline_uuid("Alex");
    // One runtime thread: the session cannot drain the queue meanwhile.
    while proxy.send_to(id, pumbo_prox::server::SessionCmd::ClearTitle) {}
    assert!(proxy.commands_changed(id));
    assert!(proxy.commands_changed(id));
    assert!(
        p.pump(WAIT, |p| trees(p) > before).await.unwrap(),
        "no rebuild"
    );
    p.pump(Duration::from_secs(1), |_| false).await.unwrap();
    assert_eq!(trees(&p), before + 1, "two changes, one rebuild");
    p.close().await;
    proxy.stop();
}

#[test]
fn no_code_when_the_network_has_a_server() {
    let text = format!("{BASE}servers:\n  lobby: {{ address: \"127.0.0.1:1\" }}\n");
    let proxy = Proxy::new(Config::parse(&text).unwrap(), Some(file("served", &text))).unwrap();
    assert!(commands::claim_code(&proxy).is_none());
    assert!(!commands::claim_open(&proxy, &proxy.runtime()));
    let who = Source::Player {
        id: uuid::Uuid::from_u128(1),
        server: None,
    };
    assert!(run(&proxy, &who, "prox claim 123456").contains("nothing to claim"));
}

#[test]
fn the_code_expires_for_good_when_a_server_is_added() {
    let path = file("expires", BASE);
    let proxy = Proxy::new(Config::parse(BASE).unwrap(), Some(path.clone())).unwrap();
    let a = uuid::Uuid::from_u128(1);
    let code = commands::claim_code(&proxy).unwrap();
    assert_eq!(suggest_as(&proxy, a, "prox cl"), ["claim"]);
    let served = format!("{BASE}servers:\n  lobby: {{ address: \"127.0.0.1:1\" }}\n");
    std::fs::write(&path, &served).unwrap();
    proxy.reload().unwrap();
    // Unclaimed and gone: the reply, the completion; also once the server
    // is removed again.
    assert!(!commands::claim_open(&proxy, &proxy.runtime()));
    let who = Source::Player {
        id: a,
        server: None,
    };
    let late = run(&proxy, &who, &format!("prox claim {code}"));
    assert!(late.contains("nothing to claim"), "{late}");
    assert!(suggest_as(&proxy, a, "prox cl").is_empty());
    std::fs::write(&path, BASE).unwrap();
    proxy.reload().unwrap();
    assert!(!commands::claim_open(&proxy, &proxy.runtime()));
    assert!(proxy.runtime().operators.is_empty());
}

#[test]
fn op_and_deop_edit_the_operators() {
    let path = file("op", BASE);
    let proxy = Proxy::new(Config::parse(BASE).unwrap(), Some(path.clone())).unwrap();
    let steve = pumbo_core::profile::GameProfile {
        id: uuid::Uuid::from_u128(7),
        name: "Steve".into(),
        properties: Vec::new(),
    };
    let (_guard, _rx) = proxy
        .register_player(&steve, "127.0.0.1:1".parse().unwrap())
        .unwrap();
    let console = Source::Console;
    // A player without the permission is refused.
    let them = Source::Player {
        id: steve.id,
        server: None,
    };
    let no = run(&proxy, &them, "prox op Steve");
    assert!(no.contains("You do not have permission"), "{no}");
    assert!(run(&proxy, &console, "prox op Nobody").contains("no player Nobody online"));
    assert!(run(&proxy, &console, "prox op").contains("Usage: /prox op <player|uuid>"));
    // By name: in the file (the comment stays) and after a reload.
    let done = run(&proxy, &console, "prox op steve");
    assert!(done.contains("Steve is now an operator."), "{done}");
    proxy.reload().unwrap();
    assert!(proxy.runtime().operators.contains(&steve.id));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# Test network\n"), "{text}");
    assert!(
        text.contains(&format!("operators: [{}]", steve.id)),
        "{text}"
    );
    assert!(run(&proxy, &console, "prox op Steve").contains("Steve is already an operator."));
    // By UUID, someone offline; then the operator Steve takes it away.
    let alex = uuid::Uuid::from_u128(8);
    assert!(run(&proxy, &them, &format!("prox op {alex}")).contains("is now an operator."));
    assert!(proxy.runtime().operators.contains(&alex));
    assert!(run(&proxy, &them, &format!("prox deop {alex}")).contains("is no longer an operator."));
    assert!(run(&proxy, &them, &format!("prox deop {alex}")).contains("is not an operator."));
    let gone = run(&proxy, &console, "prox deop Steve");
    assert!(gone.contains("Steve is no longer an operator."), "{gone}");
    assert!(proxy.runtime().operators.is_empty());
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("operators: []"), "{text}");
    assert_eq!(
        commands::suggest(&proxy, &proxy.runtime(), &console, "prox deop st")
            .unwrap()
            .1,
        ["Steve"]
    );
}
