//! Counters and the Prometheus text endpoint, on loopback only (§2.11).

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

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
}

/// A request head larger than this is not answered.
const MAX_HEAD: usize = 8192;

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
        let endpoint = endpoint.clone();
        tokio::spawn(async move {
            let read = tokio::time::timeout(Duration::from_secs(2), read_head(&mut stream)).await;
            let Ok(Some(head)) = read else { return };
            let (status, body) = answer(&head, &endpoint);
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes()).await;
            let _ = stream.shutdown().await;
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
        (Some(b"GET"), b"/metrics" | b"/") => ("200 OK", e.metrics.render() + &(e.extra)()),
        _ => ("404 Not Found", String::new()),
    }
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
        }
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
