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
    let cfg = Config::parse(&SAMPLE.replace("mode: none", "mode: bungeeguard")).unwrap();
    let reg = modules::builtin().unwrap();
    let err = modules::build(&reg, &cfg).unwrap_err().to_string();
    assert!(err.contains("bungeeguard"), "{err}");
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
            "forced-hosts:\n  mc.example.org: [nowhere]\n",
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
