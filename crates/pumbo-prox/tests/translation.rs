//! The translation chain the config builds: which pairs of client and
//! server versions it passes through, translates or refuses.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use pumbo_core::translate::TranslationPlan;
use pumbo_protocol::ProtocolVersion;
use pumbo_prox::config::Config;
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
            } else if backend == Some(777) && client < 777 {
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
