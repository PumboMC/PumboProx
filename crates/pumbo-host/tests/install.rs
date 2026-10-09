//! Installing a plugin like a `.jar` on Velocity or Paper: one `.wasm` with
//! the manifest built in, the default config and language files written at
//! the first start and kept once changed, a `<id>.yml` next to the module
//! overriding the built-in manifest, a bad plugin skipped, an update asking
//! for a restart when its manifest changed (D-STD-1 to D-STD-5).

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use common::*;
use std::sync::{Arc, Mutex, OnceLock};

use pumbo_host::{CommandOutcome, CommandSender, Host, NoProxy, Status};

const TEMPLATE: &str = include_str!("../../../plugins/example/assets/config.yml");
const LANG_PL: &str = include_str!("../../../plugins/example/lang/pl.yml");

/// Only `pumbo-example.wasm` in `plugins/`.
fn example_alone(env: &Env) {
    std::fs::copy(wasm("example"), env.plugins().join("pumbo-example.wasm")).unwrap();
}

/// The log of this test binary so far (the host logs from its own threads).
fn logs() -> String {
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    static LOG: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    let log = LOG.get_or_init(|| {
        let log = Arc::new(Mutex::new(Vec::new()));
        let w = Arc::clone(&log);
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || Buf(Arc::clone(&w)))
            .init();
        log
    });
    String::from_utf8_lossy(&log.lock().unwrap()).into_owned()
}

/// Replaces text in a module, keeping the lengths (a "newer version").
fn patch(file: &std::path::Path, pairs: &[(&str, &str)]) {
    let mut bytes = std::fs::read(file).unwrap();
    for (from, to) in pairs {
        assert_eq!(from.len(), to.len());
        let mut found = false;
        while let Some(at) = bytes.windows(from.len()).position(|w| w == from.as_bytes()) {
            bytes.splice(at..at + from.len(), to.bytes());
            found = true;
        }
        assert!(found, "{from}");
    }
    std::fs::write(file, bytes).unwrap();
}

/// D-STD-7: a file nobody changed follows an update of the plugin (a config
/// only while its values stay), a changed one stays and the log names what it
/// still has from the previous version, a missing one is created.
#[tokio::test(flavor = "multi_thread")]
async fn unchanged_files_follow_an_update() {
    logs();
    let env = Env::new("follow");
    example_alone(&env);
    let file = env.plugins().join("pumbo-example.wasm");
    let dir = env.plugins().join("pumbo-example");
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
    let path = |f: &str| dir.join(f).display().to_string();
    let (host, _) = start(env.host_config("")).await;
    let generated = env.plugins().join("data/pumbo-example/.generated");
    assert_eq!(
        std::fs::read_to_string(generated.join("lang/pl.yml")).unwrap(),
        LANG_PL
    );
    assert_eq!(
        std::fs::read_to_string(generated.join("config.yml")).unwrap(),
        TEMPLATE
    );
    // The admin changes one Polish text.
    let pl = read("lang/pl.yml").replace("Cześć z konsoli.", "Hej, z konsoli.");
    std::fs::write(dir.join("lang/pl.yml"), &pl).unwrap();

    // New texts and a new comment in the config.
    patch(
        &file,
        &[
            ("Counter reset.", "Counter RESET."),
            ("wyzerowany", "WYZEROWANY"),
            ("in the server list", "in the SERVER list"),
        ],
    );
    host.reload_plugin("pumbo-example").await.unwrap();
    assert!(read("lang/en.yml").contains("Counter RESET."));
    assert_eq!(read("lang/pl.yml"), pl);
    assert!(read("config.yml").contains("in the SERVER list"));
    let log = logs();
    for line in [
        format!("updated {} to the new built-in texts", path("lang/en.yml")),
        format!(
            "{} differs from the new built-in texts (delete the file to get them) for: admin.reset",
            path("lang/pl.yml")
        ),
        format!(
            "updated {} to the new built-in template",
            path("config.yml")
        ),
    ] {
        assert!(log.contains(&line), "{line}\n{log}");
    }

    // A new default value: the config keeps its values.
    patch(&file, &[("greeting: Hello ", "greeting: Howdy ")]);
    host.reload_plugin("pumbo-example").await.unwrap();
    assert!(read("config.yml").contains("greeting: Hello "));
    let line = format!(
        "{} keeps the previous defaults (they apply) for: greeting",
        path("config.yml")
    );
    assert!(logs().contains(&line), "{line}");

    // A deleted file comes back with the texts of this version.
    std::fs::remove_file(dir.join("lang/pl.yml")).unwrap();
    host.reload_plugin("pumbo-example").await.unwrap();
    assert!(read("lang/pl.yml").contains("Licznik WYZEROWANY."));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_file_plugin_writes_its_config_once() {
    let env = Env::new("one-file");
    example_alone(&env);
    let config = env.plugins().join("pumbo-example/config.yml");
    let lang_pl = env.plugins().join("pumbo-example/lang/pl.yml");
    let (host, _) = start(env.host_config("")).await;
    let slot = host.plugin("pumbo-example").unwrap();
    assert_eq!(slot.status(), Status::Running);
    assert_eq!(slot.manifest.short_name.as_deref(), Some("example"));
    // The template with its comments, byte for byte, the language files and
    // the data folder.
    assert_eq!(std::fs::read_to_string(&config).unwrap(), TEMPLATE);
    assert_eq!(std::fs::read_to_string(&lang_pl).unwrap(), LANG_PL);
    assert_eq!(
        std::fs::read_to_string(env.plugins().join("pumbo-example/lang/en.yml")).unwrap(),
        include_str!("../../../plugins/example/lang/en.yml")
    );
    assert!(env.plugins().join("data/pumbo-example").is_dir());
    assert_eq!(host.validate_config("pumbo-example"), Ok(()));
    host.shutdown().await;

    // An edited file (missing options included) stays as it is.
    std::fs::write(&config, "greeting: Hi\n").unwrap();
    std::fs::write(&lang_pl, "hello:\n  greeting: Siema\n").unwrap();
    let (host, _) = start(env.host_config("")).await;
    assert_eq!(
        host.plugin("pumbo-example").unwrap().status(),
        Status::Running
    );
    host.reload_plugin("pumbo-example").await.unwrap();
    assert_eq!(std::fs::read_to_string(&config).unwrap(), "greeting: Hi\n");
    assert_eq!(
        std::fs::read_to_string(&lang_pl).unwrap(),
        "hello:\n  greeting: Siema\n"
    );

    // A deleted one comes back at the next start of the plugin.
    std::fs::remove_file(&config).unwrap();
    std::fs::remove_file(&lang_pl).unwrap();
    host.reload_plugin("pumbo-example").await.unwrap();
    assert_eq!(std::fs::read_to_string(&config).unwrap(), TEMPLATE);
    assert_eq!(std::fs::read_to_string(&lang_pl).unwrap(), LANG_PL);
    host.shutdown().await;

    // Removing the plugin keeps its settings and data.
    std::fs::remove_file(env.plugins().join("pumbo-example.wasm")).unwrap();
    let (host, _) = start(env.host_config("")).await;
    assert!(host.plugin("pumbo-example").is_none());
    assert!(config.is_file() && lang_pl.is_file());
    assert!(env.plugins().join("data/pumbo-example").is_dir());
}

#[tokio::test(flavor = "multi_thread")]
async fn manifest_file_overrides_the_built_in_one() {
    let env = Env::new("override");
    example_alone(&env);
    let manifest = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/example/pumbo-example.yml"
    ))
    .unwrap();
    std::fs::write(
        env.plugins().join("pumbo-example.yml"),
        manifest.replace("version: 0.1.0", "version: 9.9.9"),
    )
    .unwrap();
    let (host, _) = start(env.host_config("")).await;
    let slot = host.plugin("pumbo-example").unwrap();
    assert_eq!(slot.manifest.version, "9.9.9");
    assert_eq!(slot.status(), Status::Running);
}

#[tokio::test(flavor = "multi_thread")]
async fn module_without_a_manifest() {
    // A build without `embed!` (like the test plugin) needs `<id>.yml`.
    let env = Env::new("no-manifest");
    std::fs::copy(wasm("test"), env.plugins().join("old.wasm")).unwrap();
    let (host, _) = start(env.host_config("")).await;
    assert!(host.plugin("old").is_none());
    assert!(pumbo_list(&host).contains("old.wasm: no manifest built in"));
    host.shutdown().await;
    env.plugin("old", "test", "");
    let (host, _) = start(env.host_config("")).await;
    assert_eq!(host.plugin("old").unwrap().status(), Status::Running);
    // Its config folder gets no file: there is no template.
    assert!(!env.plugins().join("old/config.yml").exists());
}

/// Like a `.jar`: the file name is free, the id comes from the built-in
/// manifest; two files with one id are both skipped (Velocity does the same).
#[tokio::test(flavor = "multi_thread")]
async fn any_file_name_and_duplicate_ids() {
    let env = Env::new("file-name");
    let dir = env.plugins();
    std::fs::copy(wasm("example"), dir.join("PumboExample-1.2.wasm")).unwrap();
    // `<id>.yml` overrides by the id, whatever the module is called.
    let manifest = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/example/pumbo-example.yml"
    ))
    .unwrap();
    std::fs::write(
        dir.join("pumbo-example.yml"),
        manifest.replace("version: 0.1.0", "version: 9.9.9"),
    )
    .unwrap();
    // A manifest file must carry its id as its name.
    std::fs::write(
        dir.join("other.yml"),
        "id: another\nversion: 1.0.0\napi: \"0.1\"\n",
    )
    .unwrap();
    let (host, _) = start(env.host_config("")).await;
    let slot = host.plugin("pumbo-example").unwrap();
    assert_eq!(slot.status(), Status::Running);
    assert_eq!(slot.manifest.version, "9.9.9");
    assert!(dir.join("pumbo-example/config.yml").is_file());
    assert!(dir.join("data/pumbo-example").is_dir());
    let list = pumbo_list(&host);
    assert!(
        list.contains("other.yml: manifest id another does not match the file name"),
        "{list}"
    );
    host.shutdown().await;

    std::fs::remove_file(dir.join("pumbo-example.yml")).unwrap();
    std::fs::copy(wasm("example"), dir.join("pumbo-example.wasm")).unwrap();
    let (host, _) = start(env.host_config("")).await;
    assert!(host.plugin("pumbo-example").is_none());
    let list = pumbo_list(&host);
    assert!(
        list.contains(
            "duplicate plugin id pumbo-example in PumboExample-1.2.wasm and pumbo-example.wasm"
        ),
        "{list}"
    );
    // Its folders stay.
    assert!(dir.join("pumbo-example/config.yml").is_file());
}

/// Updating by swapping the file and `plugin reload`: the code is new, the
/// manifest waits for a restart of the proxy (D-STD-3); a file without a
/// manifest or of another plugin is not started with this one.
#[tokio::test(flavor = "multi_thread")]
async fn reload_of_a_newer_file_asks_for_a_restart() {
    let env = Env::new("update");
    example_alone(&env);
    let file = env.plugins().join("pumbo-example.wasm");
    let (host, _) = start(env.host_config("")).await;
    host.reload_plugin("pumbo-example").await.unwrap();
    assert!(!pumbo_list(&host).contains("restart needed"));

    // The same module with another built-in manifest (same lengths).
    let mut newer = std::fs::read(&file).unwrap();
    for (from, to) in [
        (
            "id: pumbo-example\nversion: 0.1.0",
            "id: pumbo-example\nversion: 0.2.0",
        ),
        ("Use /hello", "Say /hello"),
    ] {
        let mut found = false;
        while let Some(at) = newer.windows(from.len()).position(|w| w == from.as_bytes()) {
            newer.splice(at..at + from.len(), to.bytes());
            found = true;
        }
        assert!(found, "{from}");
    }
    std::fs::write(&file, newer).unwrap();
    host.reload_plugin("pumbo-example").await.unwrap();
    let slot = host.plugin("pumbo-example").unwrap();
    assert_eq!(slot.status(), Status::Running);
    assert_eq!(slot.manifest.version, "0.1.0");
    let list = pumbo_list(&host);
    assert!(
        list.contains(
            "pumbo-example 0.1.0 Running, restart needed (permissions, version 0.1.0 -> 0.2.0)"
        ),
        "{list}"
    );

    std::fs::copy(wasm("test"), &file).unwrap();
    let e = host.reload_plugin("pumbo-example").await.unwrap_err();
    assert!(e.contains("no manifest built in"), "{e}");
    assert!(matches!(slot.status(), Status::Failed(_)));
    std::fs::copy(wasm("example"), &file).unwrap();
    host.reload_plugin("pumbo-example").await.unwrap();
    assert_eq!(slot.status(), Status::Running);
    assert!(!pumbo_list(&host).contains("restart needed"));
}

/// `/pumbo` as the console sees it.
fn pumbo_list(host: &Host) -> String {
    match host.dispatch_command(CommandSender::Console, "/pumbo") {
        CommandOutcome::Reply(lines) => lines
            .iter()
            .map(|l| l.plain_text())
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("{other:?}"),
    }
}

/// Like Velocity: a plugin that does not load is skipped (reason in the log
/// and in `/pumbo`), the others run. A required one stops the start.
#[tokio::test(flavor = "multi_thread")]
async fn a_bad_plugin_does_not_stop_the_proxy() {
    let env = Env::new("bad-plugin");
    example_alone(&env);
    let dir = env.plugins();
    // No manifest, a newer plugin API, broken YAML, not a component.
    std::fs::copy(wasm("test"), dir.join("old.wasm")).unwrap();
    std::fs::copy(wasm("test"), dir.join("future.wasm")).unwrap();
    std::fs::write(
        dir.join("future.yml"),
        "id: future\nversion: 2.0.0\napi: \"0.2\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("broken.yml"), "id: [\n").unwrap();
    env.plugin("corrupt", "", "");
    std::fs::write(dir.join("corrupt.wasm"), b"\0asm not a component").unwrap();

    let (host, _) = start(env.host_config("")).await;
    let example = host.plugin("pumbo-example").unwrap();
    assert_eq!(example.status(), Status::Running);
    assert!(matches!(
        host.plugin("corrupt").unwrap().status(),
        Status::Failed(_)
    ));
    assert!(host.plugin("future").is_none() && host.plugin("old").is_none());
    let list = pumbo_list(&host);
    for want in [
        "pumbo-example 0.1.0 Running",
        "old.wasm ? Failed",
        "old.wasm: no manifest built in",
        "future 2.0.0 Failed",
        "built for plugin API 0.2, this proxy provides 0.1",
        "broken.yml ? Failed",
        "corrupt 0.1.0 Failed",
    ] {
        assert!(list.contains(want), "{want:?} in\n{list}");
    }
    host.shutdown().await;

    // Required plugins and gates are fail-closed: no start.
    let start_with = |extra: &str| Host::start(env.host_config(extra), Arc::new(NoProxy));
    let e = start_with("plugins:\n  required-plugins: [future]")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("plugin future: ") && e.contains("API 0.2"),
        "{e}"
    );
    assert!(e.contains("so the proxy does not start"), "{e}");
    // Without a readable manifest it may be the missing gate.
    let e = start_with("plugins:\n  required-gates: [auth]")
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("may be the missing gate auth"), "{e}");
    // A module that does not compile shows up once the plugins started.
    let host = start_with("plugins:\n  required-plugins: [corrupt]")
        .await
        .unwrap();
    let e = host
        .wait_started(std::time::Duration::from_secs(30))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("plugin corrupt: ") && e.contains("required"),
        "{e}"
    );
}
