//! Counters and the Prometheus text endpoint, on loopback only (§2.11).

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

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
    dropped_inputs: counter "pumboprox_dropped_packets_total" "{kind=\"virtual_input\"}";
    /// Pumpkin's `trail` particle without options, repaired (D-COMPAT-1).
    fixed_trails: counter "pumboprox_fixed_packets_total" "{kind=\"trail_particle\"}";
    auth_requests: counter "pumboprox_auth_requests_total" "";
    auth_micros: counter "pumboprox_auth_request_microseconds_total" "";
    players_online: gauge "pumboprox_players_online" "";
    pending_logins: gauge "pumboprox_pending_logins" "";
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
}

impl Drop for GaugeGuard {
    fn drop(&mut self) {
        (self.1)(&self.0).fetch_sub(1, Ordering::Relaxed);
    }
}

/// Serves `GET /metrics` over plain HTTP/1.1 on `addr` (loopback, checked by
/// the config). One request per connection.
/// `extra`: more series appended to the proxy's (plugin metrics, E5).
pub async fn serve(
    listener: TcpListener,
    metrics: Arc<Metrics>,
    extra: Arc<dyn Fn() -> String + Send + Sync>,
) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            continue;
        };
        let metrics = metrics.clone();
        let extra = extra.clone();
        tokio::spawn(async move {
            let mut head = [0u8; 1024];
            let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut head)).await;
            let Ok(Ok(n)) = read else { return };
            let request = head.get(..n).unwrap_or_default();
            let (status, body) =
                if request.starts_with(b"GET /metrics ") || request.starts_with(b"GET / ") {
                    ("200 OK", metrics.render() + &extra())
                } else {
                    ("404 Not Found", String::new())
                };
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
    }
}

/// Binds the endpoint.
pub async fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(addr).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_types_once_per_family() {
        let m = Metrics::default();
        Metrics::inc(&m.rejected_ip_rate);
        let text = m.render();
        assert!(text.contains("pumboprox_connections_rejected_total{reason=\"ip_rate\"} 1\n"));
        assert_eq!(
            text.matches("# TYPE pumboprox_connections_rejected_total counter")
                .count(),
            1
        );
        assert!(text.contains("# TYPE pumboprox_players_online gauge"));
    }
}
