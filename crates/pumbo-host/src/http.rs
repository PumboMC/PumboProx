//! Outgoing HTTP of plugins (plan §4.1): https only, hosts from the manifest
//! only (after normalisation), addresses after DNS outside private and
//! loopback networks, redirects only to listed hosts, 10 s, 1 MB, 16 requests
//! at a time per plugin, a neutral User-Agent. Header values are never logged.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use crate::actor::PluginSlot;
use crate::wit::http::{HttpError, Request, Response};

pub const USER_AGENT: &str = concat!("PumboProx/", env!("CARGO_PKG_VERSION"));
pub const TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_BODY: usize = 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

pub(crate) struct Http {
    allow_private: bool,
    /// Tests only: allow plain `http` (local test servers have no certificates).
    pub(crate) allow_plain: bool,
}

/// Loopback, private, link-local, CGNAT, unique-local, multicast and other
/// non-public ranges.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let [o0, o1, ..] = a.octets();
            a.is_private()
                || a.is_loopback()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_broadcast()
                || a.is_documentation()
                || a.is_multicast()
                || o0 == 0
                || (o0 == 100 && (64..128).contains(&o1))
                || (o0 == 198 && (o1 == 18 || o1 == 19))
                || o0 >= 240
        }
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_private(IpAddr::V4(v4));
            }
            let s0 = a.segments().first().copied().unwrap_or(0);
            a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (s0 & 0xfe00) == 0xfc00
                || (s0 & 0xffc0) == 0xfe80
                || (s0 == 0x2001 && a.segments().get(1) == Some(&0x0db8))
        }
    }
}

/// Lowercase, without a trailing dot.
pub fn normalize_host(h: &str) -> String {
    h.trim_end_matches('.').to_ascii_lowercase()
}

/// DNS that drops private addresses, so a listed host cannot point at local
/// services (DNS rebinding included: the filtered result is what we connect to).
struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((name.as_str(), 0))
                .await?
                .filter(|a| !is_private(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err("host resolves only to private addresses".into());
            }
            let it: Addrs = Box::new(addrs.into_iter());
            Ok(it)
        })
    }
}

impl Http {
    pub fn new(allow_private: bool) -> Http {
        Http {
            allow_private,
            allow_plain: false,
        }
    }

    /// Whether a URL may be fetched by a plugin with these hosts.
    fn check_url(&self, url: &reqwest::Url, allowed: &[String]) -> Result<(), HttpError> {
        let scheme_ok = url.scheme() == "https" || (self.allow_plain && url.scheme() == "http");
        if !scheme_ok || !url.username().is_empty() || url.password().is_some() {
            return Err(HttpError::NotAllowed);
        }
        let Some(host) = url.host_str() else {
            return Err(HttpError::NotAllowed);
        };
        let host = normalize_host(host.trim_start_matches('[').trim_end_matches(']'));
        if !allowed.iter().any(|a| normalize_host(a) == host) {
            return Err(HttpError::NotAllowed);
        }
        if let Ok(ip) = host.parse::<IpAddr>()
            && is_private(ip)
            && !self.allow_private
        {
            return Err(HttpError::NotAllowed);
        }
        Ok(())
    }

    fn client(&self, allowed: Arc<Vec<String>>) -> Result<reqwest::Client, HttpError> {
        let allow_private = self.allow_private;
        let allow_plain = self.allow_plain;
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            let checker = Http {
                allow_private,
                allow_plain,
            };
            match checker.check_url(attempt.url(), &allowed) {
                Ok(()) => attempt.follow(),
                Err(_) => attempt.error("redirect to a host that is not allowed"),
            }
        });
        let mut b = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(policy)
            .user_agent(USER_AGENT)
            .no_proxy();
        if !self.allow_private {
            b = b.dns_resolver(Arc::new(PublicOnly));
        }
        b.build().map_err(|e| HttpError::Failed(e.to_string()))
    }

    pub async fn fetch(&self, slot: &PluginSlot, req: Request) -> Result<Response, HttpError> {
        let allowed: Arc<Vec<String>> = Arc::new(slot.manifest.http.clone());
        let url = reqwest::Url::parse(&req.url).map_err(|_| HttpError::NotAllowed)?;
        self.check_url(&url, &allowed)?;
        // A clear refusal for a listed host that points at private addresses;
        // the resolver of the client filters again (DNS rebinding).
        if !self.allow_private
            && let Some(host) = url.host_str()
            && host.parse::<IpAddr>().is_err()
        {
            let public = tokio::net::lookup_host((host, 0))
                .await
                .map(|mut a| a.any(|a| !is_private(a.ip())))
                .map_err(|e| HttpError::Failed(format!("dns: {e}")))?;
            if !public {
                return Err(HttpError::NotAllowed);
            }
        }
        let method = match req.method.to_ascii_uppercase().as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "PATCH" => reqwest::Method::PATCH,
            "DELETE" => reqwest::Method::DELETE,
            "HEAD" => reqwest::Method::HEAD,
            _ => return Err(HttpError::Failed("unsupported method".into())),
        };
        if req.body.len() > MAX_BODY {
            return Err(HttpError::TooLarge);
        }
        let _permit = tokio::time::timeout(TIMEOUT, slot.http.acquire())
            .await
            .map_err(|_| HttpError::Timeout)?
            .map_err(|_| HttpError::Failed("closed".into()))?;
        let client = self.client(allowed)?;
        let mut rb = client.request(method, url.clone());
        for (k, v) in &req.headers {
            let lower = k.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "host" | "user-agent" | "content-length" | "transfer-encoding" | "connection"
            ) {
                continue;
            }
            rb = rb.header(k.as_str(), v.as_str());
        }
        if !req.body.is_empty() {
            rb = rb.body(req.body);
        }
        tracing::debug!(plugin = %slot.id, host = url.host_str().unwrap_or_default(), "http {}", req.method);
        let mut resp = rb.send().await.map_err(|e| {
            if e.is_timeout() {
                HttpError::Timeout
            } else if e.is_redirect() {
                HttpError::NotAllowed
            } else {
                // Without the URL: it may carry keys in the query.
                HttpError::Failed(e.without_url().to_string())
            }
        })?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect();
        if resp.content_length().is_some_and(|l| l > MAX_BODY as u64) {
            return Err(HttpError::TooLarge);
        }
        let mut body = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > MAX_BODY {
                        return Err(HttpError::TooLarge);
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) if e.is_timeout() => return Err(HttpError::Timeout),
                Err(e) => return Err(HttpError::Failed(e.without_url().to_string())),
            }
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ranges() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "0.0.0.0",
            "100.64.0.1",
            "100.127.255.255",
            "255.255.255.255",
            "224.0.0.1",
            "198.18.0.1",
            "::1",
            "::",
            "fc00::1",
            "fd12::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "ff02::1",
        ] {
            assert!(is_private(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "100.128.0.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_private(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn url_rules() {
        let h = Http::new(false);
        let allowed = vec!["api.mojang.com".to_string(), "127.0.0.1".to_string()];
        let ok = |u: &str| h.check_url(&reqwest::Url::parse(u).unwrap(), &allowed);
        assert_eq!(ok("https://api.mojang.com/users"), Ok(()));
        assert_eq!(ok("https://API.Mojang.com./x"), Ok(()));
        assert_eq!(ok("http://api.mojang.com/"), Err(HttpError::NotAllowed));
        assert_eq!(ok("https://evil.com/"), Err(HttpError::NotAllowed));
        assert_eq!(
            ok("https://user:pw@api.mojang.com/"),
            Err(HttpError::NotAllowed)
        );
        // A listed IP literal in a private range is still refused.
        assert_eq!(ok("https://127.0.0.1/"), Err(HttpError::NotAllowed));
        let h = Http::new(true);
        assert_eq!(
            h.check_url(
                &reqwest::Url::parse("https://127.0.0.1/").unwrap(),
                &allowed
            ),
            Ok(())
        );
    }
}
