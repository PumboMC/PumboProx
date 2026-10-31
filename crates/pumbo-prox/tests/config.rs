use pumbo_prox::config::Config;
use pumbo_prox::modules;

const SAMPLE: &str = r#"
listener:
  - bind: "127.0.0.1:0"

forwarding:
  mode: none

servers:
  lobby: { address: "127.0.0.1:25570", protocol: 777 }

routing:
  selector: try
  try: [lobby]

translation:
  chain: [viaproxy, passthrough]

viaproxy:
  address: "127.0.0.1:25568"
  route: backend-mismatch
"#;

#[test]
fn modules_from_config() {
    let cfg = Config::parse(SAMPLE).unwrap();
    let reg = modules::builtin().unwrap();
    let m = modules::build(&reg, &cfg).unwrap();
    assert_eq!(m.forwarding.name(), "none");
    assert_eq!(m.selector.name(), "try");
    let names: Vec<_> = m.translators.iter().map(|t| t.name().to_string()).collect();
    assert_eq!(names, ["viaproxy", "passthrough"]);
}

#[test]
fn unknown_module_is_an_error() {
    let cfg = Config::parse(&SAMPLE.replace("mode: none", "mode: shared-token")).unwrap();
    let reg = modules::builtin().unwrap();
    let err = modules::build(&reg, &cfg).unwrap_err().to_string();
    assert!(err.contains("shared-token"), "{err}");
    assert!(err.contains("modern"), "{err}");
}

#[tokio::test]
async fn listener_from_config() {
    let cfg = Config::parse(SAMPLE).unwrap();
    let reg = modules::builtin().unwrap();
    let l = cfg.listener.first().unwrap();
    let factory = reg.listeners.get(&l.transport).unwrap();
    let listener = factory(&modules::listener_config(l)).await.unwrap();
    assert!(listener.local_addr().port() != 0);
}

#[test]
fn e4_sections_are_checked() {
    let ok = format!(
        "{SAMPLE}\nforced-hosts:\n  mc.example.org: [lobby]\ncommands:\n  operators: [069a79f4-44e9-4726-a5be-fca90e38aaf5]\n\
         bungeecord-channel:\n  subchannels: [Connect]\nswitching:\n  resource-packs-on-switch: keep\n"
    );
    Config::parse(&ok).unwrap();
    for (bad, why) in [
        (
            // Without servers from the proxy, every name must be in `servers`.
            "managed-servers:\n  enabled: false\nforced-hosts:\n  mc.example.org: [nowhere]\n",
            "unknown server nowhere",
        ),
        ("commands:\n  operators: [Notch]\n", "not a UUID"),
        (
            "bungeecord-channel:\n  subchannels: [KickPlayer]\n",
            "unknown subchannel KickPlayer",
        ),
    ] {
        let err = Config::parse(&format!("{SAMPLE}\n{bad}"))
            .unwrap_err()
            .to_string();
        assert!(err.contains(why), "{err}");
    }
    let per_server = SAMPLE.replace(
        "protocol: 777 }",
        "protocol: 777, chat-session-forwarding: false, signed-chat-cancel: kick }",
    );
    let cfg = Config::parse(&per_server).unwrap();
    let lobby = &cfg.servers["lobby"];
    assert!(!lobby.chat_session_forwarding);
    assert_eq!(
        lobby.signed_chat_cancel,
        pumbo_prox::config::SignedChatCancel::Kick
    );
}

#[test]
fn server_names_differing_by_case_are_rejected() {
    let two = SAMPLE.replace(
        "  lobby: { address: \"127.0.0.1:25570\", protocol: 777 }",
        "  lobby: { address: \"127.0.0.1:25570\", protocol: 777 }\n  Lobby: { address: \"127.0.0.1:25571\", protocol: 777 }",
    );
    assert_ne!(two, SAMPLE);
    let err = Config::parse(&two).unwrap_err().to_string();
    assert!(err.contains("Lobby") && err.contains("lobby"), "{err}");
}

#[test]
fn yaml_errors_name_the_line() {
    // `try` indented less than `selector`: not valid YAML.
    let bad = SAMPLE.replace("  try: [lobby]", " try: [lobby]");
    let err = Config::parse(&bad).unwrap_err().to_string();
    assert!(err.contains("line 13"), "{err}");
    // A value of the wrong type names its line too.
    let err = Config::parse(&SAMPLE.replace("protocol: 777", "protocol: many"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("line 9"), "{err}");
    // YAML 1.2: `yes` is not a boolean.
    let err = Config::parse(&format!(
        "{SAMPLE}
login:
  encrypt-offline: yes
"
    ))
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("encrypt-offline") || err.contains("bool"),
        "{err}"
    );
}

#[test]
fn the_default_file_is_valid() {
    let cfg = Config::parse(pumbo_prox::config::DEFAULT_FILE).unwrap();
    assert_eq!(cfg.listener[0].bind, "0.0.0.0:25565");
    assert!(cfg.servers.is_empty() && cfg.try_order().is_empty());
    assert!(cfg.managed_servers.enabled && cfg.managed_servers.download_from_game);
    assert_eq!(cfg.forwarding.mode, "modern");
    assert!(cfg.pumbox.enabled);
    assert!(cfg.commands.operators.is_empty());
    assert_eq!(
        cfg.login.online_mode,
        Some(pumbo_prox::config::OnlineMode::Always)
    );
    let host = pumbo_host::HostConfig::parse(pumbo_prox::config::DEFAULT_FILE).unwrap();
    assert_eq!(host.plugins.dir, std::path::Path::new("plugins"));
    // A server named in routing.try before the proxy made it: accepted.
    let ahead = format!(
        "{}routing:\n  try: [lobby]\n",
        pumbo_prox::config::DEFAULT_FILE
    );
    assert_eq!(Config::parse(&ahead).unwrap().try_order(), ["lobby"]);
}

/// The config of a fresh install turns PumboBridge on, on loopback, so the
/// servers the proxy creates get ranks from it without any change.
#[test]
fn fresh_network_has_bridge_enabled() {
    let cfg = Config::parse(pumbo_prox::config::DEFAULT_FILE).unwrap();
    assert!(cfg.bridge.enabled);
    assert_eq!(cfg.bridge.listen, "127.0.0.1:25578");
    assert_eq!(cfg.bridge.key_file, "bridge.key");
}

/// Logins in flight keep their permits through a reload, and a smaller
/// limit stays a hard one: the pools are resized, not replaced.
#[tokio::test]
async fn reload_resizes_the_login_limits() {
    use std::time::Duration;
    let path = std::env::temp_dir().join(format!("pumbo-limits-{}.yml", std::process::id()));
    let write = |pending: usize, auth: usize| {
        let text = format!(
            "{SAMPLE}\nlimits:\n  max-pending-logins: {pending}\n  max-concurrent-has-joined: {auth}\n"
        );
        std::fs::write(&path, &text).unwrap();
        text
    };
    let text = write(10, 4);
    let proxy =
        pumbo_prox::server::Proxy::new(Config::parse(&text).unwrap(), Some(path.clone())).unwrap();
    let held = proxy.pending.clone().try_acquire_many_owned(8).unwrap();
    write(5, 2);
    proxy.reload().unwrap();
    assert_eq!(proxy.auth_slots.available_permits(), 2);
    assert_eq!(proxy.pending.available_permits(), 0);
    drop(held);
    // Three of the eight returned permits go to the smaller limit.
    for _ in 0..50 {
        if proxy.pending.available_permits() == 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(proxy.pending.available_permits(), 5);
    write(20, 8);
    proxy.reload().unwrap();
    assert_eq!(proxy.pending.available_permits(), 20);
    assert_eq!(proxy.auth_slots.available_permits(), 8);
    let _ = std::fs::remove_file(&path);
}

/// Forwarding answers and bridge frames are not encrypted: a public server
/// or bridge address needs `allow-public`.
#[test]
fn public_servers_and_bridge_need_allow_public() {
    let with = |extra: &str| Config::parse(&format!("{SAMPLE}\n{extra}"));
    let server = |lobby: &str| {
        Config::parse(&SAMPLE.replace(r#"{ address: "127.0.0.1:25570", protocol: 777 }"#, lobby))
    };
    for private in [
        "10.0.0.5:25566",
        "100.64.1.2:25566",
        "[fd00::5]:25566",
        "localhost:25566",
    ] {
        let cfg = server(&format!("{{ address: \"{private}\" }}")).unwrap();
        assert!(cfg.check_backend_names().unwrap().is_empty(), "{private}");
    }
    let err = server(r#"{ address: "203.0.113.5:25566" }"#)
        .unwrap_err()
        .to_string();
    assert!(err.contains("servers.lobby.allow-public: true"), "{err}");
    server(r#"{ address: "203.0.113.5:25566", allow-public: true }"#).unwrap();

    let err = with("bridge:\n  listen: \"0.0.0.0:25578\"\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("every interface"), "{err}");
    assert!(err.contains("bridge.allow-public: true"), "{err}");
    let err = with("bridge:\n  listen: \"[::]:25578\"\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("every interface"), "{err}");
    with("bridge:\n  listen: \"0.0.0.0:25578\"\n  allow-public: true\n").unwrap();
    with("bridge:\n  listen: \"192.168.1.2:25578\"\n").unwrap();
    with("bridge:\n  enabled: false\n  listen: \"0.0.0.0:25578\"\n").unwrap();
}

/// A server name that does not resolve now is a warning, not a refusal (a
/// container of the same network may start after the proxy).
#[test]
fn unresolved_server_names_only_warn() {
    let cfg =
        Config::parse(&SAMPLE.replace("127.0.0.1:25570", "pumbo-nowhere.invalid:25566")).unwrap();
    let warnings = cfg.check_backend_names().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("does not resolve"), "{warnings:?}");
}
