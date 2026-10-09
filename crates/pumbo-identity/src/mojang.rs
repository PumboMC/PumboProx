//! Online authentication with Mojang's sessionserver (`hasJoined`) and the
//! premium-name lookup for `online-mode: per-player` (plan §2.7).
//!
//! The flow follows the protocol description ("Protocol encryption" on
//! minecraft.wiki) and Velocity's login handler: HTTP 200 with a profile means
//! authenticated, 204 means the client did not join, anything else is an
//! outage (the proxy then refuses the login: fail-closed).

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use pumbo_core::BoxFuture;
use pumbo_core::identity::{AuthError, AuthOutcome, AuthRequest, Authenticator};
use pumbo_core::profile::{GameProfile, Property};
use pumbo_core::registry::{ModuleConfig, ModuleError};
use reqwest::{StatusCode, Url};
use serde_json::Value;
use uuid::Uuid;

pub const NAME: &str = "mojang";

const SESSION_SERVER: &str = "https://sessionserver.mojang.com";
const PROFILE_LOOKUP: &str = "https://api.minecraftservices.com/minecraft/profile/lookup/name";
/// Neutral User-Agent (plan §0.2): no data about the owner.
const USER_AGENT: &str = concat!("PumboProx/", env!("CARGO_PKG_VERSION"));
/// Profile properties accepted from the sessionserver.
const MAX_PROPERTIES: usize = 16;

#[derive(Debug)]
pub struct Mojang {
    http: reqwest::Client,
    session_server: String,
    profile_lookup: String,
}

impl Mojang {
    pub fn new(
        timeout: Duration,
        session_server: &str,
        profile_lookup: &str,
    ) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(timeout)
            .connect_timeout(timeout)
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;
        Ok(Self {
            http,
            session_server: session_server.trim_end_matches('/').to_string(),
            profile_lookup: profile_lookup.trim_end_matches('/').to_string(),
        })
    }

    async fn get(&self, url: Url) -> Result<(StatusCode, Vec<u8>), AuthError> {
        let resp = self.http.get(url).send().await.map_err(map_err)?;
        let status = resp.status();
        let body = resp.bytes().await.map_err(map_err)?;
        Ok((status, body.to_vec()))
    }
}

fn map_err(e: reqwest::Error) -> AuthError {
    if e.is_timeout() {
        AuthError::Timeout
    } else {
        // Without the URL: it carries the player's name and the server hash.
        AuthError::Unavailable(e.without_url().to_string())
    }
}

/// `hasJoined` URL; `ip` only with `prevent-proxy-connections`.
pub fn has_joined_url(
    base: &str,
    username: &str,
    server_hash: &str,
    ip: Option<IpAddr>,
) -> Result<Url, String> {
    let mut params = vec![
        ("username", username.to_string()),
        ("serverId", server_hash.to_string()),
    ];
    if let Some(ip) = ip {
        params.push(("ip", ip.to_string()));
    }
    Url::parse_with_params(&format!("{base}/session/minecraft/hasJoined"), &params)
        .map_err(|e| e.to_string())
}

/// Profile from a `hasJoined` answer.
pub fn parse_profile(body: &[u8]) -> Result<GameProfile, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::try_parse(s).ok())
        .ok_or("profile without a valid id")?;
    let name = v
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty() && n.len() <= 16)
        .ok_or("profile without a valid name")?
        .to_string();
    let mut properties = Vec::new();
    if let Some(list) = v.get("properties").and_then(Value::as_array) {
        if list.len() > MAX_PROPERTIES {
            return Err("too many profile properties".into());
        }
        for p in list {
            let field = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
            properties.push(Property {
                name: field("name").ok_or("property without a name")?,
                value: field("value").ok_or("property without a value")?,
                signature: field("signature"),
            });
        }
    }
    Ok(GameProfile {
        id,
        name,
        properties,
    })
}

impl Authenticator for Mojang {
    fn name(&self) -> &str {
        NAME
    }

    fn authenticate(&self, req: AuthRequest) -> BoxFuture<'_, Result<AuthOutcome, AuthError>> {
        Box::pin(async move {
            let url = has_joined_url(
                &self.session_server,
                &req.username,
                &req.server_hash,
                req.client_ip,
            )
            .map_err(AuthError::Unavailable)?;
            let (status, body) = self.get(url).await?;
            match status {
                StatusCode::OK => parse_profile(&body)
                    .map(AuthOutcome::Authenticated)
                    .map_err(|e| AuthError::Unavailable(format!("bad hasJoined answer: {e}"))),
                StatusCode::NO_CONTENT => Ok(AuthOutcome::Rejected),
                other => Err(AuthError::Unavailable(format!(
                    "hasJoined answered {other}"
                ))),
            }
        })
    }

    fn is_premium<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<bool, AuthError>> {
        Box::pin(async move {
            let mut url = Url::parse(&format!("{}/", self.profile_lookup))
                .map_err(|e| AuthError::Unavailable(e.to_string()))?;
            url.path_segments_mut()
                .map_err(|()| AuthError::Unavailable("bad lookup URL".into()))?
                .pop_if_empty()
                .push(name);
            let (status, _) = self.get(url).await?;
            match status {
                StatusCode::OK => Ok(true),
                StatusCode::NOT_FOUND | StatusCode::NO_CONTENT => Ok(false),
                other => Err(AuthError::Unavailable(format!(
                    "profile lookup answered {other}"
                ))),
            }
        })
    }
}

/// Factory. `timeout-ms` (default 5000). The service addresses can be changed
/// only in test builds (feature `test-endpoints`, plan E3): a release build
/// cannot be pointed at a fake sessionserver.
pub fn factory(cfg: &ModuleConfig) -> Result<Arc<dyn Authenticator>, ModuleError> {
    let err = |message: String| ModuleError::Config {
        name: NAME.to_string(),
        message,
    };
    let timeout = match cfg.get("timeout-ms") {
        None => 5000,
        Some(v) => match v.as_u64() {
            Some(ms) if ms > 0 => ms,
            _ => return Err(err("timeout-ms must be a positive number".into())),
        },
    };
    let endpoint = |key: &str, default: &'static str| -> Result<String, ModuleError> {
        match pumbo_core::registry::opt_str(cfg, NAME, key)? {
            None => Ok(default.to_string()),
            Some(url) if cfg!(feature = "test-endpoints") => Ok(url.to_string()),
            Some(_) => Err(err(format!("{key} can only be changed in test builds"))),
        }
    };
    let session = endpoint("session-server", SESSION_SERVER)?;
    let lookup = endpoint("profile-lookup", PROFILE_LOOKUP)?;
    let module = Mojang::new(Duration::from_millis(timeout), &session, &lookup).map_err(err)?;
    Ok(Arc::new(module))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_joined_url_encodes_parameters() {
        let url = has_joined_url(
            SESSION_SERVER,
            "Notch",
            "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1",
            Some("2001:db8::1".parse().unwrap()),
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://sessionserver.mojang.com/session/minecraft/hasJoined?username=Notch&serverId=-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1&ip=2001%3Adb8%3A%3A1"
        );
    }

    #[test]
    fn profile_from_has_joined() {
        let body = br#"{"id":"069a79f444e94726a5befca90e38aaf5","name":"Notch","properties":[{"name":"textures","value":"e30=","signature":"c2ln"}],"profileActions":[]}"#;
        let p = parse_profile(body).unwrap();
        assert_eq!(p.id.to_string(), "069a79f4-44e9-4726-a5be-fca90e38aaf5");
        assert_eq!(p.name, "Notch");
        assert_eq!(p.properties.len(), 1);
        assert_eq!(
            p.properties.first().unwrap().signature.as_deref(),
            Some("c2ln")
        );
        assert!(parse_profile(br#"{"name":"Notch"}"#).is_err());
        assert!(parse_profile(br#"{"id":"069a79f444e94726a5befca90e38aaf5","name":""}"#).is_err());
        assert!(parse_profile(b"not json").is_err());
    }

    #[test]
    fn endpoints_only_in_test_builds() {
        let mut cfg = ModuleConfig::new();
        cfg.insert("session-server".into(), "http://127.0.0.1:1".into());
        assert_eq!(factory(&cfg).is_ok(), cfg!(feature = "test-endpoints"));
        assert!(factory(&ModuleConfig::new()).is_ok());
    }
}
