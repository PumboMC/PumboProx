//! The `pumboprox` command line: help, version and unknown commands.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::process::{Command, Output};

fn pumboprox(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pumboprox"))
        .args(args)
        .current_dir(env!("CARGO_TARGET_TMPDIR"))
        .output()
        .unwrap()
}

#[test]
fn help_and_version_exit_with_success() {
    for flag in ["--help", "-h", "help"] {
        let out = pumboprox(&[flag]);
        assert!(out.status.success(), "{flag}: {:?}", out.status);
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.starts_with("usage: pumboprox"), "{flag}: {text}");
        assert!(text.contains("check-config"), "{text}");
    }
    let version = pumboprox(&["version"]);
    assert!(version.status.success());
    for flag in ["--version", "-V"] {
        let out = pumboprox(&[flag]);
        assert!(out.status.success(), "{flag}: {:?}", out.status);
        assert_eq!(out.stdout, version.stdout, "{flag}");
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(
            text.lines()
                .next()
                .unwrap()
                .starts_with(&format!("PumboProx {}", env!("CARGO_PKG_VERSION"))),
            "{text}"
        );
    }
}

#[test]
fn an_unknown_command_is_a_usage_error() {
    let out = pumboprox(&["--frobnicate"]);
    assert_eq!(out.status.code(), Some(2));
    let text = String::from_utf8(out.stderr).unwrap();
    assert_eq!(
        text.trim(),
        "pumboprox: unknown command \"--frobnicate\"; see pumboprox --help"
    );
}

/// A flag after a command is never taken for the file (`run` takes the
/// same path; it is not run here, since before the check it started a proxy).
#[test]
fn flags_after_a_command_are_no_file() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pumbo-cli-flags-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_pumboprox"))
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap()
    };
    for args in [["check-config", "-h"], ["migrate-config", "--help"]] {
        let out = run(&args);
        assert!(out.status.success(), "{args:?}: {:?}", out.status);
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.starts_with("usage: pumboprox"), "{args:?}: {text}");
    }
    let out = run(&["check-config", "--strict"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap().trim(),
        "pumboprox: unknown option \"--strict\"; see pumboprox --help"
    );
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        0,
        "nothing is written"
    );
}

/// Runs the proxy in a fresh directory with `config` and `input` on its
/// console (ending in `stop`); returns what it wrote to stderr.
fn console(tag: &str, config: &str, input: &str) -> String {
    use std::io::Write as _;
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pumbo-cli-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pumboprox.yml"), config).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_pumboprox"))
        .current_dir(&dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{:?}", out.status);
    String::from_utf8(out.stderr).unwrap()
}

/// A proxy on a free port, no backends, no bridge.
fn small_config(extra: &str) -> String {
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    format!(
        "listener:\n  - bind: \"{addr}\"\nforwarding:\n  mode: none\nbridge:\n  enabled: false\n\
         managed-servers:\n  enabled: false\n{extra}"
    )
}

#[test]
fn the_console_has_help_and_a_short_hint() {
    let err = console(
        "help",
        &small_config("login:\n  online-mode: true\n"),
        "help\nnope\nstop\n",
    );
    assert!(err.contains("reload: read pumboprox.yml again"), "{err}");
    assert!(err.contains("unknown command \"nope\"; type help"), "{err}");
    // The test-only `gate` is not offered.
    assert!(!err.contains("gate <"), "{err}");
}

#[test]
fn check_config_fails_on_an_unknown_key() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pumbo-cli-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("pumboprox.yml");
    std::fs::write(&file, small_config("limits:\n  max-player: 10\n")).unwrap();
    let out = pumboprox(&["check-config", file.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains(
            "config error: pumboprox.yml line 10: unknown key limits.max-player (did you mean max-players?); it is ignored"
        ),
        "{err}"
    );
    std::fs::write(&file, small_config("")).unwrap();
    let out = pumboprox(&["check-config", file.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn console_replies_show_at_every_log_level() {
    let err = console(
        "off",
        &small_config("login:\n  online-mode: true\nlogging:\n  level: off\n"),
        "help\nversion\nreload\nnope\nstop\n",
    );
    assert!(err.contains("reload: read pumboprox.yml again"), "{err}");
    assert!(
        err.contains(&format!(
            "PumboProx {}, protocols",
            env!("CARGO_PKG_VERSION")
        )),
        "{err}"
    );
    assert!(err.contains("config reloaded"), "{err}");
    assert!(err.contains("unknown command \"nope\"; type help"), "{err}");
    assert!(!err.contains("INFO"), "the log is off: {err}");
}

/// A fresh, empty directory for one test.
fn empty_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("pumbo-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn check_config_creates_no_secret() {
    let dir = empty_dir("no-secret");
    let file = dir.join("pumboprox.yml");
    let config = small_config("").replace("mode: none", "mode: modern");
    std::fs::write(&file, config).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_pumboprox"))
        .args(["check-config", "pumboprox.yml"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("forwarding.secret does not exist yet; the first start creates it"),
        "{err}"
    );
    assert!(!dir.join("forwarding.secret").exists());
}

#[test]
fn check_config_reads_the_metrics_token() {
    let dir = empty_dir("metrics-token");
    let config = small_config(
        "logging:\n  metrics-bind: \"127.0.0.1:0\"\n  metrics-token-file: metrics.token\n",
    );
    std::fs::write(dir.join("pumboprox.yml"), config).unwrap();
    let check = || {
        Command::new(env!("CARGO_BIN_EXE_pumboprox"))
            .args(["check-config", "pumboprox.yml"])
            .current_dir(&dir)
            .output()
            .unwrap()
    };
    let out = check();
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("config error: logging.metrics-token-file metrics.token: "),
        "{err}"
    );
    std::fs::write(dir.join("metrics.token"), "t\n").unwrap();
    let out = check();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_bridge_key_can_come_from_the_environment() {
    use std::io::Write as _;
    let dir = empty_dir("key-env");
    let bridge = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let config = small_config("login:\n  online-mode: true\n").replace(
        "bridge:\n  enabled: false\n",
        &format!("bridge:\n  listen: \"{bridge}\"\n  key-env: PUMBO_TEST_BRIDGE_KEY\n"),
    );
    std::fs::write(dir.join("pumboprox.yml"), config).unwrap();
    let key = "ab".repeat(32);
    let mut child = Command::new(env!("CARGO_BIN_EXE_pumboprox"))
        .current_dir(&dir)
        .env("PUMBO_TEST_BRIDGE_KEY", &key)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"prox bridge key\nstop\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(String::from_utf8_lossy(&out.stdout).contains(&key), "{err}");
    assert!(!dir.join("bridge.key").exists(), "no key file is written");
}

#[cfg(unix)]
#[test]
fn migrate_config_renames_old_keys_and_keeps_a_copy() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = empty_dir("migrate");
    let file = dir.join("pumboprox.yml");
    let old = small_config("# for old plugins\nbungeecord-channel:\n  enabled: false\n");
    let migrate = || {
        std::fs::write(&file, &old).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        let out = pumboprox(&["migrate-config", file.to_str().unwrap()]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let said = migrate();
    assert!(
        said.contains("bungeecord-channel is now messaging-channel"),
        "{said}"
    );
    assert!(
        said.contains("to go back to an older PumboProx, restore"),
        "{said}"
    );
    let new = std::fs::read_to_string(&file).unwrap();
    assert!(new.starts_with("config-version: 2\n"), "{new}");
    assert!(
        new.contains("# for old plugins\nmessaging-channel:\n  enabled: false\n"),
        "{new}"
    );
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o640);
    assert_eq!(
        std::fs::read_to_string(dir.join("pumboprox.yml.bak")).unwrap(),
        old
    );
    // A second migration keeps the first copy.
    migrate();
    assert!(dir.join("pumboprox.yml.bak.1").is_file());
    let out = pumboprox(&["migrate-config", file.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("needs no changes")
    );
    assert!(!dir.join("pumboprox.yml.bak.2").exists());
}

#[cfg(unix)]
#[test]
fn relative_paths_are_read_next_to_the_config() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = empty_dir("paths");
    let (conf, elsewhere) = (dir.join("conf"), dir.join("elsewhere"));
    std::fs::create_dir_all(&conf).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let file = conf.join("pumboprox.yml");
    std::fs::write(
        &file,
        small_config("").replace("mode: none", "mode: modern"),
    )
    .unwrap();
    let secret = |at: &std::path::Path| {
        let s = at.join("forwarding.secret");
        std::fs::write(&s, "a".repeat(64)).unwrap();
        std::fs::set_permissions(&s, std::fs::Permissions::from_mode(0o600)).unwrap();
    };
    let check = || {
        let out = Command::new(env!("CARGO_BIN_EXE_pumboprox"))
            .args(["check-config", file.to_str().unwrap()])
            .current_dir(&elsewhere)
            .output()
            .unwrap();
        let err = String::from_utf8(out.stderr).unwrap();
        assert!(out.status.success(), "{err}");
        err
    };
    // Only next to the config: found there, from any working directory.
    secret(&conf);
    let err = check();
    assert!(!err.contains("does not exist yet"), "{err}");
    assert!(!err.contains("working directory"), "{err}");
    // Only in the working directory (an older install): it stays, with a warning.
    std::fs::remove_file(conf.join("forwarding.secret")).unwrap();
    secret(&elsewhere);
    let err = check();
    assert!(
        err.contains(&format!(
            "relative paths are read from the working directory {} because forwarding.secret is there and missing or empty next to {}; move them next to the config",
            elsewhere.canonicalize().unwrap().display(),
            file.display()
        )),
        "{err}"
    );
    assert!(!err.contains("does not exist yet"), "{err}");
    // Plugins in the working directory and an empty plugins/ next to the
    // config (as 0.2.5-beta left it): they stay where they are.
    std::fs::rename(
        elsewhere.join("forwarding.secret"),
        conf.join("forwarding.secret"),
    )
    .unwrap();
    std::fs::create_dir_all(conf.join("plugins")).unwrap();
    std::fs::create_dir_all(elsewhere.join("plugins")).unwrap();
    std::fs::write(elsewhere.join("plugins/notes.txt"), "").unwrap();
    let err = check();
    assert!(err.contains("because plugins/ is there"), "{err}");
    std::fs::remove_dir_all(elsewhere.join("plugins")).unwrap();
    std::fs::rename(
        conf.join("forwarding.secret"),
        elsewhere.join("forwarding.secret"),
    )
    .unwrap();
}
