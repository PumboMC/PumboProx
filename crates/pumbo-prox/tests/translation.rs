//! The translation chain the config builds: which pairs of client and
//! server versions it passes through, translates or refuses.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use pumbo_core::translate::TranslationPlan;
use pumbo_protocol::ProtocolVersion;
use pumbo_prox::config::Config;
use pumbo_prox::migrate::OLD_PROVIDER;
use pumbo_prox::modules::{self, Modules};

/// A config without a `translation` section: the default chain.
const PLAIN: &str = "listener:\n  - bind: \"127.0.0.1:0\"\nforwarding:\n  mode: none\n";

fn build(text: &str) -> Modules {
    let cfg = Config::parse(text).unwrap();
    modules::build(&modules::builtin().unwrap(), &cfg).unwrap()
}

/// The first decision of the chain: `same`, `translate` or `none`.
fn route(m: &Modules, client: i32, backend: Option<i32>) -> &'static str {
    let plan = m
        .translators
        .iter()
        .find_map(|t| t.plan(ProtocolVersion(client), backend.map(ProtocolVersion)));
    match plan {
        Some(TranslationPlan::Passthrough) => "same",
        Some(TranslationPlan::Translate(_)) => "translate",
        _ => "none",
    }
}

#[test]
fn the_default_chain_routes_like_before() {
    let m = build(PLAIN);
    let protocols: Vec<i32> = pumbo_data::protocols().map(|p| p.0).collect();
    assert!(protocols.len() > 1, "{protocols:?}");
    for &client in &protocols {
        for backend in std::iter::once(None).chain(protocols.iter().copied().map(Some)) {
            let want = if backend.is_none() || backend == Some(client) {
                "same"
            } else if backend == Some(777) && client < 777 && cfg!(feature = "translate") {
                "translate"
            } else {
                "none"
            };
            assert_eq!(
                route(&m, client, backend),
                want,
                "client {client}, server {backend:?}"
            );
        }
    }
}

#[test]
fn a_typo_in_the_translation_section_is_named() {
    let text = format!("{PLAIN}translation:\n  chains: [passthrough]\n");
    let (_, remarks) = Config::parse_checked(&text).unwrap();
    assert_eq!(
        remarks.unknown,
        [(
            Some(text.lines().count()),
            "unknown key translation.chains (did you mean chain?); it is ignored".to_string()
        )]
    );
}

fn names(m: &Modules) -> Vec<&str> {
    m.translators.iter().map(|t| t.name()).collect()
}

#[test]
fn the_default_chain_is_multiversion_then_passthrough() {
    // A client older than every table passes the chain, as no provider
    // refuses it any more; the login turns it away for want of a version.
    let m = build(PLAIN);
    assert_eq!(route(&m, 766, None), "same");
    assert_eq!(route(&m, 766, Some(766)), "same");
    let want: &[&str] = if cfg!(feature = "translate") {
        &["multiversion", "passthrough"]
    } else {
        &["passthrough"]
    };
    assert_eq!(names(&build(PLAIN)), want);
}

/// The keys of the first-start file of 25e6a3f, with the old default chain
/// written out and the old provider's section.
#[test]
fn an_old_config_with_the_external_provider_loads_with_warnings() {
    let p = OLD_PROVIDER;
    let old = "listener:\n  - bind: \"127.0.0.1:0\"\nlogin:\n  online-mode: true\nservers: {}\n\
               managed-servers:\n  enabled: true\n  download-from-game: true\nforwarding:\n  mode: modern\n  \
               secret-file: forwarding.secret\nplugins:\n  dir: plugins\npumbox:\n  enabled: true\n\
               commands:\n  operators: []\n";
    let at = old.lines().count();
    for chain in [
        format!("[multiversion, {p}, passthrough]"),
        format!("[{p}, multiversion, passthrough]"),
    ] {
        let text = format!(
            "{old}translation:\n  chain: {chain}\n{p}:\n  address: \"127.0.0.1:25568\"\n  route: backend-mismatch\n  min-native-protocol: 767\n"
        );
        let (mut cfg, remarks) = Config::parse_checked(&text).unwrap();
        assert_eq!(remarks.unknown, [], "{chain}");
        assert_eq!(
            remarks.warnings,
            [
                (
                    Some(at + 2),
                    format!(
                        "translation.chain: {p} is no longer a provider; it is skipped, and `pumboprox migrate-config` removes it"
                    )
                ),
                (
                    Some(at + 3),
                    format!("{p} is no longer read; `pumboprox migrate-config` removes it")
                ),
            ],
            "{chain}"
        );
        assert_eq!(cfg.translation.chain, ["multiversion", "passthrough"]);
        // As check-config builds them: no forwarding secret is created.
        cfg.check_only = true;
        let built = modules::build(&modules::builtin().unwrap(), &cfg);
        if cfg!(feature = "translate") {
            assert_eq!(names(&built.unwrap()), ["multiversion", "passthrough"]);
        } else {
            let e = built.unwrap_err().to_string();
            assert!(e.contains("this build has no version translation"), "{e}");
        }
    }
}

#[test]
fn a_chain_of_only_the_external_provider_lets_nobody_in() {
    let text = format!("{PLAIN}translation:\n  chain: [{OLD_PROVIDER}]\n");
    let (cfg, remarks) = Config::parse_checked(&text).unwrap();
    assert!(cfg.translation.chain.is_empty());
    let empty = (
        Some(text.lines().count()),
        "translation.chain is empty, so no player can join: add passthrough".to_string(),
    );
    assert_eq!(remarks.warnings.len(), 2, "{:?}", remarks.warnings);
    assert!(
        remarks.warnings[0].1.starts_with(&format!(
            "translation.chain: {OLD_PROVIDER} is no longer a provider"
        )),
        "{:?}",
        remarks.warnings
    );
    assert_eq!(remarks.warnings[1], empty);
    // An empty chain written as such gets the same warning.
    let (_, remarks) =
        Config::parse_checked(&format!("{PLAIN}translation:\n  chain: []\n")).unwrap();
    assert_eq!(remarks.warnings, [empty]);
    let m = modules::build(&modules::builtin().unwrap(), &cfg).unwrap();
    assert_eq!(route(&m, 777, None), "none");
    assert_eq!(route(&m, 777, Some(777)), "none");
}

/// Providers have no settings: a section named after one is a typo.
#[test]
fn a_section_named_after_a_provider_is_unknown() {
    let text = format!("{PLAIN}multiversion:\n  x: 1\n");
    let (_, remarks) = Config::parse_checked(&text).unwrap();
    assert_eq!(remarks.unknown.len(), 1, "{:?}", remarks.unknown);
    assert!(
        remarks.unknown[0].1.starts_with("unknown key multiversion"),
        "{:?}",
        remarks.unknown
    );
}

/// Without the translate feature the proxy says why `multiversion` is
/// missing, and does not offer it for a misspelled name.
#[cfg(not(feature = "translate"))]
#[test]
fn a_build_without_translation_refuses_multiversion() {
    let reg = modules::builtin().unwrap();
    let chain = |c: &str| {
        let text = format!("{PLAIN}translation:\n  chain: {c}\n");
        modules::build(&reg, &Config::parse(&text).unwrap())
            .unwrap_err()
            .to_string()
    };
    assert_eq!(
        chain("[multiversion, passthrough]"),
        "configuration of module \"multiversion\": this build has no version translation (built without the translate feature); remove multiversion from translation.chain"
    );
    assert_eq!(
        chain("[multiversoin]"),
        "unknown translator module \"multiversoin\" (available: passthrough)"
    );
}
