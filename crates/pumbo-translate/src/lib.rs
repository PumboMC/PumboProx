//! Translation providers: `passthrough` (same versions), `multiversion`
//! (translation inside the proxy, `pumbo-translate-mv`) and `viaproxy`
//! (redirecting the client to an external ViaProxy, §8).

pub mod multiversion;

use std::net::SocketAddr;
use std::sync::Arc;

use pumbo_core::registry::{ModuleConfig, ModuleError, ModuleRegistry, opt_str};
use pumbo_core::translate::{TranslationPlan, TranslatorProvider};
use pumbo_protocol::ProtocolVersion;

pub const PASSTHROUGH: &str = "passthrough";
pub const VIAPROXY: &str = "viaproxy";

/// Equal versions (or the backend not known yet): no translation.
#[derive(Debug)]
pub struct Passthrough;

impl TranslatorProvider for Passthrough {
    fn name(&self) -> &str {
        PASSTHROUGH
    }

    fn plan(
        &self,
        client: ProtocolVersion,
        backend: Option<ProtocolVersion>,
    ) -> Option<TranslationPlan> {
        match backend {
            None => Some(TranslationPlan::Passthrough),
            Some(b) if b == client => Some(TranslationPlan::Passthrough),
            Some(_) => None,
        }
    }
}

/// When to send the client to ViaProxy (§8.1, §8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Only clients below the oldest native version.
    BelowNative,
    /// Also clients whose version no backend has.
    BackendMismatch,
}

#[derive(Debug)]
pub struct ViaProxyRedirect {
    pub address: SocketAddr,
    pub route: Route,
    pub min_native: ProtocolVersion,
}

impl TranslatorProvider for ViaProxyRedirect {
    fn name(&self) -> &str {
        VIAPROXY
    }

    fn plan(
        &self,
        client: ProtocolVersion,
        backend: Option<ProtocolVersion>,
    ) -> Option<TranslationPlan> {
        let external = Some(TranslationPlan::External {
            address: self.address,
        });
        if client < self.min_native {
            return external;
        }
        match (self.route, backend) {
            (Route::BackendMismatch, Some(b)) if b != client => external,
            _ => None,
        }
    }
}

fn passthrough_factory(_: &ModuleConfig) -> Result<Arc<dyn TranslatorProvider>, ModuleError> {
    Ok(Arc::new(Passthrough))
}

fn viaproxy_factory(cfg: &ModuleConfig) -> Result<Arc<dyn TranslatorProvider>, ModuleError> {
    let err = |message: String| ModuleError::Config {
        name: VIAPROXY.to_string(),
        message,
    };
    let address = opt_str(cfg, VIAPROXY, "address")?
        .unwrap_or("127.0.0.1:25568")
        .parse()
        .map_err(|e| err(format!("address: {e}")))?;
    let route = match opt_str(cfg, VIAPROXY, "route")?.unwrap_or("backend-mismatch") {
        "below-native" => Route::BelowNative,
        "backend-mismatch" => Route::BackendMismatch,
        other => return Err(err(format!("unknown route \"{other}\""))),
    };
    let min_native = match cfg.get("min-native-protocol") {
        None => ProtocolVersion::V767,
        Some(v) => match v.as_i64() {
            Some(v) => ProtocolVersion(
                i32::try_from(v).map_err(|e| err(format!("min-native-protocol: {e}")))?,
            ),
            None => return Err(err("min-native-protocol must be a number".into())),
        },
    };
    Ok(Arc::new(ViaProxyRedirect {
        address,
        route,
        min_native,
    }))
}

pub fn register(reg: &mut ModuleRegistry) -> Result<(), ModuleError> {
    reg.translators.register(PASSTHROUGH, passthrough_factory)?;
    reg.translators
        .register(multiversion::MULTIVERSION, multiversion::factory)?;
    reg.translators.register(VIAPROXY, viaproxy_factory)?;
    Ok(())
}

/// The first decision from the provider chain (config order).
pub fn decide(
    chain: &[Arc<dyn TranslatorProvider>],
    client: ProtocolVersion,
    backend: Option<ProtocolVersion>,
) -> Option<TranslationPlan> {
    chain.iter().find_map(|p| p.plan(client, backend))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(route: Route) -> Vec<Arc<dyn TranslatorProvider>> {
        vec![
            Arc::new(ViaProxyRedirect {
                address: "127.0.0.1:25568".parse().unwrap(),
                route,
                min_native: ProtocolVersion::V767,
            }),
            Arc::new(Passthrough),
        ]
    }

    fn kind(plan: Option<TranslationPlan>) -> &'static str {
        match plan {
            None => "none",
            Some(TranslationPlan::Passthrough) => "passthrough",
            Some(TranslationPlan::External { .. }) => "viaproxy",
            Some(TranslationPlan::Translate(_)) => "translate",
        }
    }

    #[test]
    fn decision_chain() {
        let c = chain(Route::BackendMismatch);
        let v = ProtocolVersion;
        assert_eq!(kind(decide(&c, v(47), None)), "viaproxy");
        assert_eq!(kind(decide(&c, v(777), Some(v(777)))), "passthrough");
        assert_eq!(kind(decide(&c, v(769), Some(v(777)))), "viaproxy");
        assert_eq!(kind(decide(&c, v(769), None)), "passthrough");
        let c = chain(Route::BelowNative);
        assert_eq!(kind(decide(&c, v(769), Some(v(777)))), "none");
        assert_eq!(kind(decide(&c, v(5), Some(v(777)))), "viaproxy");
    }
}
