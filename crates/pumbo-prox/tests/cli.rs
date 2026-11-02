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
