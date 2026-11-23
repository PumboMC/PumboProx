//! The facts of the servers and what they give each runtime.

use std::sync::{Arc, Mutex};

use pumbo_core::engine::{EngineRef, SignedChatCancel};

use super::*;
use crate::config::Config;
use crate::modules::EngineSource::{Assumed, Brand, Bridge, Declared};
use crate::play::quirk_tests::capture_logs;

/// A proxy whose `servers` section is `servers`.
fn proxy(servers: &str) -> Arc<Proxy> {
    let text = format!("forwarding:\n  mode: none\nservers:\n{servers}");
    Proxy::new(Config::parse(&text).unwrap(), None).unwrap()
}

const LOBBY: &str = "  lobby: { address: \"127.0.0.1:1\", protocol: 777 }\n";

fn named(name: &str) -> EngineRef {
    EngineRef {
        name: name.into(),
        version: None,
    }
}

/// The adapter's name, the engine's and the source.
fn shown(e: &ServerEngine) -> (&str, &str, EngineSource) {
    (e.adapter.name(), e.engine.name.as_str(), e.source)
}

fn facts(proxy: &Proxy) -> HashMap<String, Facts> {
    proxy.detect.lock().clone()
}

#[test]
fn the_first_recognised_brand_stays() {
    let (_guard, log) = capture_logs();
    let p = proxy(LOBBY);
    let rt = p.runtime();
    let first = p.detect.brand(&p, &rt, "lobby", "vanilla");
    assert_eq!(shown(&first), ("vanilla", "vanilla", Brand));
    for _ in 0..3 {
        // This connection's own brand counts for it; the fact stays.
        let second = p.detect.brand(&p, &rt, "lobby", "Paper");
        assert_eq!(shown(&second), ("paper", "paper", Brand));
    }
    assert_eq!(facts(&p)["lobby"].brand, Some(named("vanilla")));
    assert_eq!(
        shown(&p.detect.engine(&rt, "lobby")),
        ("vanilla", "vanilla", Brand)
    );
    let log = String::from_utf8(log.lock().unwrap().clone()).unwrap();
    let lines: Vec<&str> = log.lines().filter(|l| l.contains("brand")).collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(
        lines[0].ends_with(
            "server lobby now shows the brand of paper, it showed vanilla before; the proxy keeps vanilla until a reload"
        ),
        "{log}"
    );
}

#[test]
fn made_up_brands_leave_no_trace() {
    let p = proxy(LOBBY);
    let rt = p.runtime();
    let long = "Pumpkin".repeat(30);
    let brands = (0..50).map(|n| format!("MyNet {n}")).chain([long]);
    for brand in brands {
        let e = p.detect.brand(&p, &rt, "lobby", &brand);
        assert_eq!(shown(&e), ("pumpkin", "pumpkin", Assumed), "{brand}");
    }
    assert!(facts(&p).is_empty());
}

#[test]
fn the_bridge_beats_the_brand_and_the_entry_beats_both() {
    let p = proxy(&format!(
        "{LOBBY}  games: {{ address: \"127.0.0.1:2\", protocol: 777, engine: pumpkin }}\n"
    ));
    let rt = p.runtime();
    for server in ["lobby", "games"] {
        p.detect.brand(&p, &rt, server, "vanilla");
        p.detect.bridge_hello(&rt, server, Some(named("paper")));
    }
    assert_eq!(
        shown(&p.detect.engine(&rt, "lobby")),
        ("paper", "paper", Bridge)
    );
    assert_eq!(
        shown(&p.detect.engine(&rt, "games")),
        ("pumpkin", "pumpkin", Declared)
    );
    // A bridge that names no engine leaves the brand.
    p.detect.bridge_hello(&rt, "lobby", None);
    assert_eq!(
        shown(&p.detect.engine(&rt, "lobby")),
        ("vanilla", "vanilla", Brand)
    );
}

#[test]
fn facts_of_another_address_do_not_count() {
    let p = proxy(LOBBY);
    p.detect.brand(&p, &p.runtime(), "lobby", "vanilla");
    let moved = proxy("  lobby: { address: \"127.0.0.1:3\", protocol: 777 }\n");
    let rt = moved.runtime();
    let old = p.detect.lock().clone();
    *moved.detect.lock() = old.clone();
    assert_eq!(
        shown(&moved.detect.engine(&rt, "lobby")),
        ("pumpkin", "pumpkin", Assumed)
    );
    assert_eq!(moved.detect.backends(&rt)[0].engine, None);
    assert_eq!(facts(&moved), old);
}

#[test]
fn the_facts_reach_the_backends_and_twice_is_the_same() {
    let p = proxy(&format!(
        "{LOBBY}  games: {{ address: \"127.0.0.1:2\", protocol: 777 }}\n"
    ));
    let rt = p.runtime();
    p.detect.brand(&p, &rt, "games", "Paper");
    let first = p.detect.backends(&rt);
    let engines: Vec<_> = first
        .iter()
        .map(|b| (b.name.as_str(), b.engine.clone()))
        .collect();
    assert_eq!(engines, [("games", Some(named("paper"))), ("lobby", None)]);
    assert_eq!(p.detect.backends(&rt), first);
    // The runtime's own list does not count: its servers are worked out
    // from the config.
    let mut stale = Runtime::build(rt.config.clone()).unwrap();
    stale.backends.clear();
    assert_eq!(p.detect.backends(&stale), first);
    let kick = p.detect.engine(&rt, "games").chat(None).1;
    assert_eq!(kick, SignedChatCancel::Kick);
}

#[test]
fn the_first_runtime_and_a_reloaded_one_carry_the_engines() {
    let dir = std::env::temp_dir().join(format!("pumbo-detect-reload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pumboprox.yml");
    let text = format!(
        "forwarding:\n  mode: none\nservers:\n{LOBBY}  games: {{ address: \"127.0.0.1:2\", engine: paper }}\n"
    );
    std::fs::write(&path, &text).unwrap();
    let p = Proxy::new(Config::parse(&text).unwrap(), Some(path.clone())).unwrap();
    let engine = |rt: &Runtime, name: &str| {
        rt.backends
            .iter()
            .find(|b| b.name == name)
            .and_then(|b| b.engine.clone())
    };
    assert_eq!(engine(&p.runtime(), "games"), Some(named("paper")));
    p.detect.brand(&p, &p.runtime(), "lobby", "vanilla");
    p.reload().unwrap();
    assert_eq!(engine(&p.runtime(), "lobby"), Some(named("vanilla")));
    assert_eq!(engine(&p.runtime(), "games"), Some(named("paper")));
    // The brand stays until the next player confirms or replaces it.
    let lobby = &facts(&p)["lobby"];
    assert_eq!(
        (&lobby.brand, lobby.rearmed),
        (&Some(named("vanilla")), true)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_protocol_follows_the_entry_and_the_answer() {
    use ServerProtocol::{Any, Auto, Fixed};
    let p = |n| Some(ProtocolVersion(n));
    for (entry, answer, want) in [
        (Auto, None, None),
        (Auto, Some((-1, 776)), p(776)),
        (Auto, Some((-1, 7777)), None),
        (Any, Some((-1, 776)), None),
        (Fixed(777), None, p(777)),
        (Fixed(777), Some((777, 777)), p(777)),
        (Fixed(777), Some((777, 776)), p(776)),
        (Fixed(777), Some((777, 7777)), p(777)),
        // An answer to another question than the entry asks now.
        (Auto, Some((775, 775)), None),
        (Fixed(776), Some((-1, 777)), p(776)),
    ] {
        assert_eq!(protocol(entry, answer), want, "{entry:?} {answer:?}");
    }
}

/// `lobby` and its protocol in the runtime.
fn lobby_protocol(p: &Proxy) -> Option<i32> {
    let rt = p.runtime();
    let lobby = rt.backends.iter().find(|b| b.name == "lobby");
    lobby.and_then(|b| b.protocol).map(|v| v.0)
}

const AUTO: &str = "  lobby: { address: \"127.0.0.1:1\" }\n";

#[test]
fn only_a_changed_result_makes_a_new_runtime() {
    let p = proxy(AUTO);
    for _ in 0..2 {
        p.detect.answered(&p.runtime(), "lobby", -1, Ok(776));
    }
    assert!(p.apply_detection());
    let rt = p.runtime();
    p.detect.answered(&rt, "lobby", -1, Ok(776));
    assert!(!p.apply_detection());
    assert!(Arc::ptr_eq(&rt, &p.runtime()));
    assert_eq!(lobby_protocol(&p), Some(776));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_protocol_drops_the_remembered_ways() {
    let p = proxy(AUTO);
    let old = p.runtime();
    assert!(crate::versions::way(&old, ProtocolVersion::V777).await);
    p.detect.answered(&old, "lobby", -1, Ok(776));
    assert!(p.apply_detection());
    assert!(!crate::versions::way(&p.runtime(), ProtocolVersion::V777).await);
}

#[test]
fn a_refused_reload_keeps_the_facts() {
    let dir = std::env::temp_dir().join(format!("pumbo-detect-refused-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pumboprox.yml");
    let text = format!("forwarding:\n  mode: none\nservers:\n{AUTO}");
    std::fs::write(&path, &text).unwrap();
    let p = Proxy::new(Config::parse(&text).unwrap(), Some(path.clone())).unwrap();
    p.detect.answered(&p.runtime(), "lobby", -1, Ok(776));
    assert!(p.apply_detection());
    let before = (facts(&p), p.runtime().backends.clone());
    // What a reload works out before it is refused: the server moved.
    let moved = text.replace("127.0.0.1:1", "127.0.0.1:3");
    let prepared = Runtime::build(Config::parse(&moved).unwrap()).unwrap();
    assert_eq!(p.detect.backends(&prepared)[0].protocol, None);
    std::fs::write(&path, moved.replace("\" }", "\", engine: stone }")).unwrap();
    assert!(p.reload().is_err());
    assert!(!p.apply_detection());
    assert_eq!((facts(&p), p.runtime().backends.clone()), before);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn facts_of_removed_servers_go_after_a_reload_that_passed() {
    let dir = std::env::temp_dir().join(format!("pumbo-detect-removed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pumboprox.yml");
    let games = "  games: { address: \"127.0.0.1:2\" }\n";
    let text = format!("forwarding:\n  mode: none\nservers:\n{AUTO}{games}");
    std::fs::write(&path, &text).unwrap();
    let p = Proxy::new(Config::parse(&text).unwrap(), Some(path.clone())).unwrap();
    for server in ["lobby", "games"] {
        p.detect.answered(&p.runtime(), server, -1, Ok(776));
    }
    std::fs::write(&path, text.replace(games, "")).unwrap();
    p.reload().unwrap();
    p.apply_detection();
    let left: Vec<String> = facts(&p).into_keys().collect();
    assert_eq!(left, ["lobby"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lines of `log` that name `lobby`.
fn lines(log: &Mutex<Vec<u8>>) -> Vec<String> {
    let text = String::from_utf8(log.lock().unwrap().clone()).unwrap();
    text.lines()
        .filter(|l| l.contains("lobby"))
        .map(String::from)
        .collect()
}

#[test]
fn a_wrong_protocol_entry_is_reported_when_the_answer_changes() {
    let (_guard, log) = capture_logs();
    let p = proxy(LOBBY);
    let rt = p.runtime();
    for _ in 0..3 {
        p.detect.answered(&rt, "lobby", 777, Ok(776));
    }
    let said = lines(&log);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].ends_with(
            "server lobby: protocol is set to 777 but the server answers 776; the proxy uses 776. Fix servers.lobby.protocol, or set protocol: any when the server takes several versions itself"
        ),
        "{said:?}"
    );
    p.detect.answered(&rt, "lobby", 777, Ok(775));
    assert_eq!(lines(&log).len(), 2);
    p.detect.answered(&rt, "lobby", 777, Ok(777));
    let said = lines(&log);
    assert!(
        said[2]
            .ends_with("server lobby runs Minecraft 26.3 (protocol 777), from its status answer"),
        "{said:?}"
    );
}

#[test]
fn a_silent_server_is_reported_once_per_outage() {
    let (_guard, log) = capture_logs();
    let p = proxy(AUTO);
    let rt = p.runtime();
    let silent = || p.detect.answered(&rt, "lobby", -1, Err("timed out".into()));
    silent();
    silent();
    let said = lines(&log);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].ends_with(
            "server lobby does not answer the status ping (timed out); its version stays unknown and every client is sent to it"
        ),
        "{said:?}"
    );
    p.detect.answered(&rt, "lobby", -1, Ok(776));
    silent();
    silent();
    let said = lines(&log);
    assert_eq!(said.len(), 3, "{said:?}");
    assert!(
        said[2].ends_with(
            "server lobby does not answer the status ping (timed out); the proxy keeps protocol 776 from its last answer"
        ),
        "{said:?}"
    );
}

/// Brands and the bridge change the result, never the pings: nothing a
/// player or an unauthenticated server does makes the proxy connect.
#[tokio::test(flavor = "multi_thread")]
async fn a_brand_or_a_bridge_asks_nobody() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let accepts = Arc::new(AtomicUsize::new(0));
    let counted = accepts.clone();
    tokio::spawn(async move {
        while let Ok((s, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            drop(s);
        }
    });
    let p = proxy(&format!("  lobby: {{ address: \"{address}\" }}\n"));
    let wait = std::time::Duration::from_secs(15);
    let timing = task::Timing {
        ping_timeout: wait,
        start_wait: wait,
        wake_gap: std::time::Duration::ZERO,
        ..task::Timing::default()
    };
    task::start_with(&p, timing).await;
    let asked = accepts.load(Ordering::SeqCst);
    let changes = Arc::new(AtomicUsize::new(0));
    let mut seen = p.runtime_changes();
    seen.mark_unchanged();
    let counter = changes.clone();
    tokio::spawn(async move {
        while seen.changed().await.is_ok() {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    });
    let rt = p.runtime();
    for n in 0..50 {
        p.detect.brand(&p, &rt, "lobby", &format!("MyNet {n}"));
    }
    for n in 0..10 {
        let brand = if n % 2 == 0 { "vanilla" } else { "Paper" };
        p.detect.brand(&p, &rt, "lobby", brand);
    }
    p.detect.bridge_hello(&rt, "lobby", Some(named("paper")));
    let mut now = p.runtime_changes();
    let bridged = |rt: &Arc<Runtime>| rt.backends[0].engine == Some(named("paper"));
    tokio::time::timeout(wait, now.wait_for(bridged))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(accepts.load(Ordering::SeqCst), asked);
    assert!(changes.load(Ordering::SeqCst) <= 2);
    p.stop();
}

/// A proxy that reloads from a file with these servers; the file's path.
fn from_file(tag: &str, servers: &str) -> (Arc<Proxy>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pumbo-detect-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pumboprox.yml");
    let text = format!("forwarding:\n  mode: none\nservers:\n{servers}");
    std::fs::write(&path, &text).unwrap();
    let p = Proxy::new(Config::parse(&text).unwrap(), Some(path.clone())).unwrap();
    (p, path)
}

/// A reload keeps the engine of the last brand until the next player shows
/// a brand: no new runtime and no line about an assumed engine meanwhile.
#[test]
fn a_reload_keeps_a_brand_until_the_next_player() {
    let (_guard, log) = capture_logs();
    let (p, _) = from_file("rearm", LOBBY);
    p.detect.brand(&p, &p.runtime(), "lobby", "Paper");
    assert!(p.apply_detection());
    p.reload().unwrap();
    assert!(!p.apply_detection());
    assert_eq!(p.runtime().backends[0].engine, Some(named("paper")));
    // The next brand counts, a made-up one too.
    p.detect.brand(&p, &p.runtime(), "lobby", "vanilla");
    assert_eq!(facts(&p)["lobby"].brand, Some(named("vanilla")));
    p.reload().unwrap();
    p.detect.brand(&p, &p.runtime(), "lobby", "MyNet");
    assert_eq!(facts(&p)["lobby"].brand, None);
    let log = String::from_utf8(log.lock().unwrap().clone()).unwrap();
    assert!(
        !log.contains("assumed") && !log.contains("now shows"),
        "{log}"
    );
}

/// A session on a runtime from before a reload that moved the server
/// leaves the facts of the new address alone.
#[test]
fn a_session_on_an_old_runtime_leaves_the_new_address_alone() {
    let (p, path) = from_file("moved", LOBBY);
    let old = p.runtime();
    std::fs::write(
        &path,
        std::fs::read_to_string(&path)
            .unwrap()
            .replace(":1\"", ":3\""),
    )
    .unwrap();
    p.reload().unwrap();
    p.detect
        .bridge_hello(&p.runtime(), "lobby", Some(named("paper")));
    let e = p.detect.brand(&p, &old, "lobby", "vanilla");
    assert_eq!(shown(&e), ("vanilla", "vanilla", Brand));
    assert_eq!(facts(&p)["lobby"].bridge, Some(named("paper")));
}

#[test]
fn the_last_answer_is_of_this_address_and_while_it_answers() {
    let p = proxy(AUTO);
    let rt = p.runtime();
    let other = |text: &str| Runtime::build(Config::parse(text).unwrap()).unwrap();
    let base = format!("forwarding:\n  mode: none\nservers:\n{AUTO}");
    p.detect.answered(&rt, "lobby", -1, Ok(776));
    assert_eq!(p.detect.last_answer(&rt, "lobby"), Some(776));
    let moved = other(&base.replace(":1\"", ":3\""));
    assert_eq!(p.detect.last_answer(&moved, "lobby"), None);
    let off = other(&format!("{base}detection:\n  ping: false\n"));
    assert_eq!(p.detect.last_answer(&off, "lobby"), None);
    p.detect.answered(&rt, "lobby", -1, Err("timed out".into()));
    assert_eq!(p.detect.last_answer(&rt, "lobby"), None);
}

#[test]
fn an_engine_change_and_a_contradicted_entry_are_logged_once() {
    let (_guard, log) = capture_logs();
    let p = proxy(&format!(
        "{LOBBY}  games: {{ address: \"127.0.0.1:2\", protocol: 777, engine: pumpkin }}\n"
    ));
    let rt = p.runtime();
    for server in ["lobby", "games"] {
        p.detect.brand(&p, &rt, server, "Paper");
    }
    assert!(p.apply_detection());
    assert!(!p.apply_detection());
    let log = String::from_utf8(log.lock().unwrap().clone()).unwrap();
    let lines: Vec<&str> = log.lines().filter(|l| l.contains("engine")).collect();
    assert_eq!(lines.len(), 2, "{log}");
    let mismatch = "server games: engine is set to pumpkin but the server shows paper; the proxy keeps pumpkin";
    assert!(
        lines
            .iter()
            .any(|l| l.ends_with("server lobby: engine paper (from its brand)")),
        "{log}"
    );
    assert!(lines.iter().any(|l| l.ends_with(mismatch)), "{log}");
}

/// The same brand again asks for no recheck: a player joining a server
/// never makes the detector take the config lock.
#[tokio::test]
async fn only_a_new_fact_asks_for_a_recheck() {
    let p = proxy(LOBBY);
    let rt = p.runtime();
    let none = std::time::Duration::ZERO;
    p.detect.brand(&p, &rt, "lobby", "Paper");
    assert!(
        tokio::time::timeout(none, p.detect.recheck.notified())
            .await
            .is_ok()
    );
    p.detect.brand(&p, &rt, "lobby", "Paper");
    assert!(
        tokio::time::timeout(none, p.detect.recheck.notified())
            .await
            .is_err()
    );
}

#[test]
fn wakes_in_the_gap_wait() {
    use std::time::Duration;
    let now = tokio::time::Instant::now();
    let gap = Duration::from_secs(5);
    assert!(task::wake_round(true, None, now, gap));
    assert!(!task::wake_round(true, Some(now), now, gap));
    assert!(task::wake_round(true, Some(now), now + gap, gap));
    assert!(!task::wake_round(false, None, now, gap));
}
