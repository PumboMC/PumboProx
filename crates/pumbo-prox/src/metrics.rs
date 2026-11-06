//! Counters and the Prometheus text endpoint, on loopback only (§2.11).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pumbo_core::translate::Dropped;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

macro_rules! metrics {
    ($($(#[$doc:meta])* $field:ident: $kind:ident $name:literal $labels:literal;)*) => {
        /// Proxy-wide counters (`counter`) and gauges (`gauge`).
        #[derive(Debug, Default)]
        pub struct Metrics {
            $($(#[$doc])* pub $field: metrics!(@ty $kind),)*
        }

        impl Metrics {
            /// Prometheus text format (version 0.0.4).
            pub fn render(&self) -> String {
                let mut out = String::new();
                let mut last = "";
                $(
                    if last != $name {
                        let _ = writeln!(out, "# TYPE {} {}", $name, stringify!($kind));
                        last = $name;
                    }
                    let _ = writeln!(out, "{}{} {}", $name, $labels, self.$field.load(Ordering::Relaxed));
                )*
                let _ = last;
                out
            }
        }
    };
    (@ty counter) => { AtomicU64 };
    (@ty gauge) => { AtomicI64 };
}

metrics! {
    connections: counter "pumboprox_connections_total" "";
    rejected_pending: counter "pumboprox_connections_rejected_total" "{reason=\"pending_logins\"}";
    rejected_ip_rate: counter "pumboprox_connections_rejected_total" "{reason=\"ip_rate\"}";
    rejected_ip_concurrent: counter "pumboprox_connections_rejected_total" "{reason=\"ip_concurrent\"}";
    rejected_status_rate: counter "pumboprox_connections_rejected_total" "{reason=\"status_rate\"}";
    rejected_proxy_header: counter "pumboprox_connections_rejected_total" "{reason=\"proxy_header\"}";
    rejected_proxy_untrusted: counter "pumboprox_connections_rejected_total" "{reason=\"proxy_untrusted\"}";
    timeouts_handshake: counter "pumboprox_timeouts_total" "{phase=\"handshake\"}";
    timeouts_login: counter "pumboprox_timeouts_total" "{phase=\"login\"}";
    timeouts_idle: counter "pumboprox_timeouts_total" "{phase=\"idle\"}";
    status_pings: counter "pumboprox_status_total" "{kind=\"modern\"}";
    legacy_pings: counter "pumboprox_status_total" "{kind=\"legacy\"}";
    logins_online: counter "pumboprox_logins_total" "{result=\"online\"}";
    logins_offline: counter "pumboprox_logins_total" "{result=\"offline\"}";
    logins_denied: counter "pumboprox_logins_total" "{result=\"denied\"}";
    auth_failures: counter "pumboprox_logins_total" "{result=\"auth_failed\"}";
    auth_unavailable: counter "pumboprox_logins_total" "{result=\"auth_unavailable\"}";
    protocol_errors: counter "pumboprox_protocol_errors_total" "";
    kicked_packet_rate: counter "pumboprox_limit_kicks_total" "{limit=\"packets\"}";
    dropped_suggestions: counter "pumboprox_dropped_packets_total" "{kind=\"command_suggestion\"}";
    dropped_keep_alives: counter "pumboprox_dropped_packets_total" "{kind=\"keep_alive\"}";
    dropped_payloads: counter "pumboprox_dropped_packets_total" "{kind=\"custom_payload\"}";
    /// Commands for a session whose queue was full (messages, titles, switches).
    dropped_commands: counter "pumboprox_dropped_packets_total" "{kind=\"session_command\"}";
    dropped_inputs: counter "pumboprox_dropped_packets_total" "{kind=\"virtual_input\"}";
    bytes_from_clients: counter "pumboprox_bytes_total" "{from=\"client\"}";
    bytes_from_backends: counter "pumboprox_bytes_total" "{from=\"backend\"}";
    backend_connects: counter "pumboprox_backend_connects_total" "{result=\"ok\"}";
    backend_failures: counter "pumboprox_backend_connects_total" "{result=\"failed\"}";
    switches_ok: counter "pumboprox_switches_total" "{result=\"ok\"}";
    switches_failed: counter "pumboprox_switches_total" "{result=\"failed\"}";
    fallbacks: counter "pumboprox_fallbacks_total" "";
    reconnects: counter "pumboprox_reconnects_total" "";
    proxy_commands: counter "pumboprox_proxy_commands_total" "";
    chat_cancelled: counter "pumboprox_chat_cancelled_total" "";
    virtual_entries: counter "pumboprox_virtual_entries_total" "";
    virtual_releases: counter "pumboprox_virtual_releases_total" "";
    /// Pumpkin's `trail` particle without options, repaired (D-COMPAT-1).
    fixed_trails: counter "pumboprox_fixed_packets_total" "{kind=\"trail_particle\"}";
    auth_requests: counter "pumboprox_auth_requests_total" "";
    auth_micros: counter "pumboprox_auth_request_microseconds_total" "";
    players_online: gauge "pumboprox_players_online" "";
    pending_logins: gauge "pumboprox_pending_logins" "";
    players_in_gates: gauge "pumboprox_players_in_gates" "";
    /// `transfer` packets from servers without `allow-transfers` (GAP2-01).
    transfers_blocked: counter "pumboprox_transfers_total" "{result=\"blocked\"}";
    transfers_switched: counter "pumboprox_transfers_total" "{result=\"switched\"}";
    /// Premium-name checks of `online-mode: per-player` (SEC-04).
    premium_cached: counter "pumboprox_premium_checks_total" "{result=\"cached\"}";
    premium_lookups: counter "pumboprox_premium_checks_total" "{result=\"lookup\"}";
    premium_throttled: counter "pumboprox_premium_checks_total" "{result=\"throttled\"}";
    premium_rate_limited: counter "pumboprox_premium_checks_total" "{result=\"rate_limited\"}";
}

impl Metrics {
    pub fn inc(c: &AtomicU64) {
        c.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }
}

/// `pumboprox_translate_dropped_total{to, packet}`: packets the version
/// translator dropped, by name from the version tables (bounded labels).
#[derive(Debug, Default)]
pub struct TranslateDrops(Mutex<Drops>);

#[derive(Debug, Default)]
struct Drops {
    counts: BTreeMap<(bool, &'static str), u64>,
    /// Per server, the packets already logged.
    logged: HashMap<String, HashSet<(bool, &'static str)>>,
}

impl TranslateDrops {
    /// Counts one; the first of each packet per server is logged at `debug`
    /// (a client can cause one per packet it sends).
    pub fn count(&self, server: &str, d: Dropped) {
        let Ok(mut drops) = self.0.lock() else {
            return;
        };
        let key = (d.to_client, d.packet);
        *drops.counts.entry(key).or_default() += 1;
        let first = match drops.logged.get_mut(server) {
            Some(seen) => seen.insert(key),
            None => {
                drops
                    .logged
                    .insert(server.to_string(), HashSet::from([key]));
                true
            }
        };
        if first {
            let from = if d.to_client { "server" } else { "client" };
            tracing::debug!(
                "server {server}: the version translator dropped {} from the {from} (pumboprox_translate_dropped_total)",
                d.packet
            );
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::from("# TYPE pumboprox_translate_dropped_total counter\n");
        if let Ok(drops) = self.0.lock() {
            for ((to_client, packet), n) in &drops.counts {
                let to = if *to_client { "client" } else { "server" };
                let _ = writeln!(
                    out,
                    "pumboprox_translate_dropped_total{{to=\"{to}\",packet=\"{packet}\"}} {n}"
                );
            }
        }
        out
    }
}

/// Gauge +1 now, -1 on drop.
#[derive(Debug)]
pub struct GaugeGuard(Arc<Metrics>, fn(&Metrics) -> &AtomicI64);

impl GaugeGuard {
    pub fn new(m: Arc<Metrics>, gauge: fn(&Metrics) -> &AtomicI64) -> Self {
        gauge(&m).fetch_add(1, Ordering::Relaxed);
        Self(m, gauge)
    }

    /// Counts one more only while the gauge is below `max` (`0`: no limit).
    /// Each caller sees its own count, so two at once never both lose the
    /// last place.
    pub fn below(m: Arc<Metrics>, gauge: fn(&Metrics) -> &AtomicI64, max: i64) -> Option<Self> {
        let before = gauge(&m).fetch_add(1, Ordering::Relaxed);
        let slot = Self(m, gauge);
        (max <= 0 || before < max).then_some(slot)
    }
}

impl Drop for GaugeGuard {
    fn drop(&mut self) {
        (self.1)(&self.0).fetch_sub(1, Ordering::Relaxed);
    }
}

/// What the HTTP endpoint answers with.
#[derive(Clone)]
pub struct Endpoint {
    pub metrics: Arc<Metrics>,
    /// More series appended to the proxy's (backends, plugin metrics).
    pub extra: Arc<dyn Fn() -> String + Send + Sync>,
    /// `/ready`: the proxy takes players.
    pub ready: Arc<dyn Fn() -> bool + Send + Sync>,
    /// `Authorization: Bearer <token>` that `/metrics` wants; `/health` and
    /// `/ready` carry no data and answer without it. An error (a token file
    /// that cannot be read): `/metrics` answers 503, the others still work.
    pub token: Result<Option<Arc<[u8]>>, String>,
}

/// The token in `path` (empty: none). A file other users can read is a
/// warning: a mounted container secret often is, and is left as it is.
pub fn read_token(path: &str) -> Result<Option<Arc<[u8]>>, String> {
    if path.is_empty() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            tracing::warn!(
                "{path}: the metrics token is readable by other users (mode {mode:04o}); chmod 600 it"
            );
        }
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    match text.trim() {
        "" => Err(format!("{path}: the file is empty")),
        t => Ok(Some(Arc::from(t.as_bytes()))),
    }
}

/// A request head larger than this is not answered.
const MAX_HEAD: usize = 8192;

/// Connections answered at once; more are closed at once, so the endpoint
/// cannot use up the file descriptors the players need.
const MAX_CONNECTIONS: usize = 64;

/// Time for a whole request: reading it and writing the reply.
const REQUEST_TIME: Duration = Duration::from_secs(5);

/// Serves `GET /metrics` (also `GET /`), `GET /health` and `GET /ready` over
/// plain HTTP/1.1, one request per connection.
pub async fn serve(listener: TcpListener, endpoint: Endpoint) {
    serve_from(|| listener.accept(), endpoint).await;
}

async fn serve_from<F, A>(mut accept: F, endpoint: Endpoint)
where
    F: FnMut() -> A,
    A: Future<Output = std::io::Result<(TcpStream, SocketAddr)>>,
{
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let mut stream = match accept().await {
            Ok((s, _)) => s,
            Err(e) => {
                // E.g. out of file descriptors: back off instead of spinning.
                tracing::warn!("metrics accept: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let endpoint = endpoint.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let _ = tokio::time::timeout(REQUEST_TIME, async {
                let Some(head) = read_head(&mut stream).await else {
                    return;
                };
                let (status, body) = answer(&head, &endpoint);
                let challenge = match status {
                    UNAUTHORIZED => "WWW-Authenticate: Bearer\r\n",
                    _ => "",
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\n{challenge}Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            })
            .await;
        });
    }
}

/// The request up to its empty line; `None` when the peer closes first or
/// sends more than [`MAX_HEAD`].
async fn read_head(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = stream.read(&mut buf).await.ok().filter(|n| *n > 0)?;
        head.extend_from_slice(buf.get(..n)?);
        if head.windows(4).any(|w| w == b"\r\n\r\n") {
            return Some(head);
        }
        if head.len() > MAX_HEAD {
            return None;
        }
    }
}

fn answer(head: &[u8], e: &Endpoint) -> (&'static str, String) {
    let line = head.split(|b| *b == b'\r').next().unwrap_or_default();
    let mut parts = line.split(|b| *b == b' ');
    let (method, target) = (parts.next(), parts.next().unwrap_or_default());
    let path = target.split(|b| *b == b'?').next().unwrap_or_default();
    match (method, path) {
        (Some(b"GET"), b"/health") => ("200 OK", "ok\n".into()),
        (Some(b"GET"), b"/ready") if (e.ready)() => ("200 OK", "ready\n".into()),
        (Some(b"GET"), b"/ready") => ("503 Service Unavailable", "not ready\n".into()),
        (Some(b"GET"), b"/metrics" | b"/") if e.token.is_err() => (
            "503 Service Unavailable",
            "the metrics token file cannot be read, see the log\n".into(),
        ),
        (Some(b"GET"), b"/metrics" | b"/")
            if !authorized(head, e.token.as_ref().ok().and_then(Option::as_deref)) =>
        {
            (UNAUTHORIZED, String::new())
        }
        (Some(b"GET"), b"/metrics" | b"/") => ("200 OK", e.metrics.render() + &(e.extra)()),
        _ => ("404 Not Found", String::new()),
    }
}

const UNAUTHORIZED: &str = "401 Unauthorized";

/// `Authorization: Bearer <token>` among the headers, compared in constant
/// time; anything goes without a token.
fn authorized(head: &[u8], token: Option<&[u8]>) -> bool {
    let Some(token) = token else {
        return true;
    };
    head.split(|b| *b == b'\n').skip(1).any(|line| {
        let Some(colon) = line.iter().position(|b| *b == b':') else {
            return false;
        };
        let (name, value) = line.split_at(colon);
        let value = value.get(1..).unwrap_or_default().trim_ascii();
        let (scheme, given) = value.split_at(value.len().min(7));
        name.trim_ascii().eq_ignore_ascii_case(b"authorization")
            && scheme.eq_ignore_ascii_case(b"bearer ")
            && aws_lc_rs::constant_time::verify_slices_are_equal(given.trim_ascii(), token).is_ok()
    })
}

/// Binds the endpoint.
pub async fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(addr).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Many at once for one place: exactly one gets it, every time.
    #[test]
    fn one_place_goes_to_exactly_one() {
        use std::sync::atomic::AtomicBool;
        for _ in 0..500 {
            let m = Arc::new(Metrics::default());
            let (go, done) = (AtomicBool::new(false), std::sync::Barrier::new(8));
            let got = std::thread::scope(|s| {
                let tries: Vec<_> = (0..8)
                    .map(|_| {
                        let (m, go, done) = (m.clone(), &go, &done);
                        s.spawn(move || {
                            while !go.load(Ordering::Acquire) {
                                std::hint::spin_loop();
                            }
                            let slot = GaugeGuard::below(m, |m| &m.players_in_gates, 1);
                            done.wait();
                            slot.is_some()
                        })
                    })
                    .collect();
                go.store(true, Ordering::Release);
                tries
                    .into_iter()
                    .map(|t| t.join().unwrap())
                    .filter(|won| *won)
                    .count()
            });
            assert_eq!(got, 1);
            assert_eq!(m.players_in_gates.load(Ordering::Relaxed), 0);
        }
        let m = Arc::new(Metrics::default());
        let all: Vec<_> = (0..3)
            .map(|_| GaugeGuard::below(m.clone(), |m| &m.players_in_gates, 0))
            .collect();
        assert!(all.iter().all(Option::is_some), "0: no limit");
    }

    /// A family's samples follow its one `# TYPE` line: Prometheus rejects a
    /// scrape that repeats it.
    #[test]
    fn renders_types_once_per_family() {
        let m = Metrics::default();
        Metrics::inc(&m.rejected_ip_rate);
        let text = m.render();
        assert!(text.contains("pumboprox_connections_rejected_total{reason=\"ip_rate\"} 1\n"));
        assert!(text.contains("# TYPE pumboprox_players_online gauge"));
        let mut seen = std::collections::HashSet::new();
        let mut current = "";
        for line in text.lines() {
            if let Some(t) = line.strip_prefix("# TYPE ") {
                current = t.split(' ').next().unwrap_or_default();
                assert!(seen.insert(current), "{current} typed twice:\n{text}");
            } else {
                let name = line.split(['{', ' ']).next().unwrap_or_default();
                assert_eq!(name, current, "{line} outside its family");
            }
        }
    }

    fn endpoint(ready: impl Fn() -> bool + Send + Sync + 'static) -> Endpoint {
        Endpoint {
            metrics: Arc::default(),
            extra: Arc::new(|| "extra 1\n".into()),
            ready: Arc::new(ready),
            token: Ok(None),
        }
    }

    #[tokio::test]
    async fn a_token_guards_the_metrics_only() {
        let e = Endpoint {
            token: Ok(Some(Arc::from(&b"s3cret"[..]))),
            ..endpoint(|| true)
        };
        let ask = |auth: &'static str| {
            let e = e.clone();
            async move {
                let reply = get(e, &["GET /metrics HTTP/1.1\r\n", auth, "\r\n"]).await;
                reply.lines().next().unwrap_or_default().to_string()
            }
        };
        assert_eq!(ask("").await, "HTTP/1.1 401 Unauthorized");
        assert_eq!(
            ask("Authorization: Bearer wrong\r\n").await,
            "HTTP/1.1 401 Unauthorized"
        );
        assert_eq!(
            ask("Authorization: s3cret\r\n").await,
            "HTTP/1.1 401 Unauthorized"
        );
        assert_eq!(
            ask("Authorization: Bearer s3cret\r\n").await,
            "HTTP/1.1 200 OK"
        );
        assert_eq!(
            ask("authorization:bearer  s3cret \r\n").await,
            "HTTP/1.1 200 OK"
        );
        // The header in pieces, after the request line.
        let split = get(
            e.clone(),
            &[
                "GET /metrics HTTP/1.1\r\nAuthor",
                "ization: Bearer s3",
                "cret\r\n\r\n",
            ],
        )
        .await;
        assert!(split.starts_with("HTTP/1.1 200 OK"), "{split}");
        let denied = get(e.clone(), &["GET / HTTP/1.1\r\n\r\n"]).await;
        assert!(denied.contains("WWW-Authenticate: Bearer\r\n"), "{denied}");
        for path in ["/health", "/ready"] {
            let reply = get(e.clone(), &[&format!("GET {path} HTTP/1.1\r\n\r\n")]).await;
            assert!(reply.starts_with("HTTP/1.1 200 OK"), "{path}: {reply}");
        }
    }

    /// A token file that cannot be read keeps `/metrics` closed, not the
    /// health checks.
    #[tokio::test]
    async fn without_its_token_only_the_metrics_are_closed() {
        let e = Endpoint {
            token: Err("metrics.token: No such file".into()),
            ..endpoint(|| true)
        };
        let metrics = get(e.clone(), &["GET /metrics HTTP/1.1\r\n\r\n"]).await;
        assert!(metrics.starts_with("HTTP/1.1 503"), "{metrics}");
        assert!(!metrics.contains("pumboprox_"), "{metrics}");
        for path in ["/health", "/ready"] {
            let reply = get(e.clone(), &[&format!("GET {path} HTTP/1.1\r\n\r\n")]).await;
            assert!(reply.starts_with("HTTP/1.1 200 OK"), "{path}: {reply}");
        }
    }

    /// Idle connections hold at most [`MAX_CONNECTIONS`] descriptors; one
    /// more is closed unanswered, and a place frees up when one goes.
    #[tokio::test]
    async fn connections_at_once_are_limited() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, endpoint(|| true)));
        let mut idle = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            idle.push(TcpStream::connect(addr).await.unwrap());
        }
        let ask = || async {
            let mut s = TcpStream::connect(addr).await.unwrap();
            let _ = s.write_all(b"GET /health HTTP/1.1\r\n\r\n").await;
            let mut reply = String::new();
            let _ = s.read_to_string(&mut reply).await;
            reply
        };
        assert_eq!(ask().await, "", "one too many is closed");
        drop(idle.pop());
        let mut reply = String::new();
        for _ in 0..50 {
            reply = ask().await;
            if !reply.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
        server.abort();
    }

    #[test]
    fn the_token_file_is_private_and_trimmed() {
        let path = std::env::temp_dir().join(format!("pumbo-metrics-token-{}", std::process::id()));
        std::fs::write(&path, "s3cret\n").unwrap();
        let p = path.to_str().unwrap();
        assert!(read_token("").unwrap().is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // A mounted secret (0644, 0440) works, with a warning.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(read_token(p).unwrap().as_deref(), Some(&b"s3cret"[..]));
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(read_token(p).unwrap().as_deref(), Some(&b"s3cret"[..]));
        std::fs::write(&path, " \n").unwrap();
        assert!(read_token(p).unwrap_err().contains("empty"));
        std::fs::remove_file(&path).unwrap();
        assert!(read_token(p).is_err());
    }

    /// The reply to `request`, written in pieces.
    async fn get(endpoint: Endpoint, request: &[&str]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(serve(listener, endpoint));
        let mut s = TcpStream::connect(addr).await.unwrap();
        for part in request {
            s.write_all(part.as_bytes()).await.unwrap();
            s.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut reply = String::new();
        s.read_to_string(&mut reply).await.unwrap();
        server.abort();
        reply
    }

    #[tokio::test]
    async fn health_ready_and_metrics() {
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = ready.clone();
        let e = endpoint(move || flag.load(Ordering::Relaxed));
        let health = get(e.clone(), &["GET /health HTTP/1.1\r\n\r\n"]).await;
        assert!(
            health.starts_with("HTTP/1.1 200 OK") && health.ends_with("ok\n"),
            "{health}"
        );
        let not_ready = get(e.clone(), &["GET /ready HTTP/1.1\r\n\r\n"]).await;
        assert!(not_ready.starts_with("HTTP/1.1 503"), "{not_ready}");
        ready.store(true, Ordering::Relaxed);
        let is_ready = get(e.clone(), &["GET /ready HTTP/1.1\r\n\r\n"]).await;
        assert!(is_ready.starts_with("HTTP/1.1 200 OK"), "{is_ready}");
        // A head that arrives in pieces is read to its end.
        let metrics = get(
            e.clone(),
            &["GET /metrics?x=1 HTTP/1.1\r\n", "Host: x\r\n", "\r\n"],
        )
        .await;
        assert!(metrics.starts_with("HTTP/1.1 200 OK"), "{metrics}");
        assert!(
            metrics.contains("\npumboprox_players_online 0\n") && metrics.ends_with("\nextra 1\n"),
            "{metrics}"
        );
        let other = get(e.clone(), &["GET /metricsx HTTP/1.1\r\n\r\n"]).await;
        assert!(other.starts_with("HTTP/1.1 404"), "{other}");
        let post = get(e, &["POST /metrics HTTP/1.1\r\n\r\n"]).await;
        assert!(post.starts_with("HTTP/1.1 404"), "{post}");
    }

    #[test]
    fn translate_drops_by_packet() {
        let drops = TranslateDrops::default();
        let d = |to_client, packet| Dropped { to_client, packet };
        drops.count("lobby", d(true, "add_entity"));
        drops.count("lobby", d(true, "add_entity"));
        drops.count("game", d(true, "add_entity"));
        drops.count("lobby", d(false, "unknown"));
        assert_eq!(
            drops.render(),
            "# TYPE pumboprox_translate_dropped_total counter\n\
             pumboprox_translate_dropped_total{to=\"server\",packet=\"unknown\"} 1\n\
             pumboprox_translate_dropped_total{to=\"client\",packet=\"add_entity\"} 3\n"
        );
        let logged = drops
            .0
            .lock()
            .unwrap()
            .logged
            .values()
            .map(HashSet::len)
            .sum::<usize>();
        assert_eq!(logged, 3, "once per packet and server");
    }

    #[tokio::test]
    async fn failing_accept_backs_off() {
        let tries = Arc::new(AtomicU64::new(0));
        let counted = tries.clone();
        let accept = move || {
            let counted = counted.clone();
            async move {
                Metrics::inc(&counted);
                tokio::task::yield_now().await;
                Err(std::io::Error::other("out of file descriptors"))
            }
        };
        let serve = serve_from(accept, endpoint(|| true));
        let _ = tokio::time::timeout(Duration::from_millis(300), serve).await;
        let tries = tries.load(Ordering::Relaxed);
        assert!(tries <= 10, "{tries} accepts in 300 ms");
    }
}
