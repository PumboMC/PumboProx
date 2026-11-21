use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pumbo_core::listener::Listener;
use tracing::{error, info, warn};

use super::{Proxy, SESSION_DRAIN};
use crate::config::ProxyProtocol;
use crate::modules;
use crate::net::Cidr;

/// One listener's PROXY protocol settings.
#[derive(Debug)]
pub struct ListenerSettings {
    pub bind: String,
    pub proxy_protocol: ProxyProtocol,
    pub trusted: Vec<Cidr>,
    /// The first refused direct connection was logged at warn.
    pub warned: AtomicBool,
}

impl ListenerSettings {
    pub fn is_trusted(&self, ip: IpAddr) -> bool {
        self.trusted.iter().any(|c| c.contains(ip))
    }
}

impl Proxy {
    /// Opens every `listener`.
    pub async fn bind(&self) -> Result<Vec<(Box<dyn Listener>, Arc<ListenerSettings>)>, String> {
        let rt = self.runtime();
        let registry = modules::builtin().map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for l in &rt.config.listener {
            let factory = registry
                .listeners
                .get(&l.transport)
                .map_err(|e| e.to_string())?;
            let listener = factory(&modules::listener_config(l))
                .await
                .map_err(|e| e.to_string())?;
            let trusted = rt.config.trusted(l).map_err(|e| e.to_string())?;
            info!("listening on {} ({})", listener.local_addr(), l.transport);
            if l.proxy_protocol == ProxyProtocol::Required {
                info!(
                    "listener {}: PROXY header required, only {} may connect; set proxy-protocol: optional to also accept direct connections",
                    l.bind,
                    l.trusted_proxies.join(", ")
                );
            }
            out.push((
                listener,
                Arc::new(ListenerSettings {
                    bind: l.bind.clone(),
                    proxy_protocol: l.proxy_protocol,
                    trusted,
                    warned: AtomicBool::new(false),
                }),
            ));
        }
        if out.is_empty() {
            return Err("no [[listener]] in the config".into());
        }
        Ok(out)
    }

    /// `pumboprox_backend_online{server="…"}`: 1 unless the server is
    /// offline after failed logins or a server run by the proxy is not ready.
    pub fn render_health(&self) -> String {
        use std::fmt::Write as _;
        let rt = self.runtime();
        let mut out = String::from("# TYPE pumboprox_backend_online gauge\n");
        for b in &rt.backends {
            let ready = !rt.managed.contains(&b.name)
                || self
                    .servers
                    .get()
                    .is_some_and(|m| m.state(&b.name) == pumbo_servers::State::Ready);
            let up = ready && self.health.online(&b.name);
            let name = b.name.replace('\\', "\\\\").replace('"', "\\\"");
            let _ = writeln!(
                out,
                "pumboprox_backend_online{{server=\"{name}\"}} {}",
                u8::from(up)
            );
        }
        out
    }

    /// `/ready`: the listeners take players, every required gate and plugin
    /// is loaded, and the proxy is not stopping. Backends do not count: the
    /// Pumbox holds players while they restart.
    pub fn ready(&self) -> bool {
        self.serving.load(Ordering::Relaxed)
            && !*self.shutdown.borrow()
            && self
                .plugins
                .get()
                .is_none_or(|p| p.host.login_open().is_ok())
    }

    /// The metrics, `/health` and `/ready` endpoint at `logging.metrics-bind`
    /// (once; [`Proxy::serve`] starts it when the start did not).
    pub async fn start_metrics(self: &Arc<Self>) {
        let logging = self.runtime().config.logging.clone();
        let Ok(addr) = logging.metrics_bind.parse::<SocketAddr>() else {
            return;
        };
        if self.metrics_started.swap(true, Ordering::Relaxed) {
            return;
        }
        // Never served without the token it was given; `/health` and `/ready` still are.
        let token = crate::metrics::read_token(&logging.metrics_token_file);
        if let Err(e) = &token {
            error!("logging.metrics-token-file {e}: /metrics answers 503 until a restart");
        }
        let listener = match crate::metrics::bind(addr).await {
            Ok(l) => l,
            Err(e) => return error!("metrics on {addr}: {e}"),
        };
        info!("metrics on http://{addr}/metrics");
        if matches!(token, Ok(None)) && !addr.ip().is_loopback() {
            warn!("metrics on {addr} without a token");
        }
        let (extra, ready) = (Arc::downgrade(self), Arc::downgrade(self));
        let endpoint = crate::metrics::Endpoint {
            metrics: self.metrics.clone(),
            extra: Arc::new(move || {
                let Some(p) = extra.upgrade() else {
                    return String::new();
                };
                let mut out = p.render_health();
                out.push_str(&p.translate_drops.render());
                if let Some(plugins) = p.plugins.get() {
                    out.push_str(&plugins.host.render_metrics());
                }
                out
            }),
            ready: Arc::new(move || ready.upgrade().is_some_and(|p| p.ready())),
            token,
        };
        tokio::spawn(crate::metrics::serve(listener, endpoint));
    }

    /// Accepts connections on all listeners until [`Proxy::stop`], then waits
    /// up to five seconds for sessions to end.
    pub async fn serve(
        self: Arc<Self>,
        listeners: Vec<(Box<dyn Listener>, Arc<ListenerSettings>)>,
    ) {
        self.start_metrics().await;
        let sweeper = {
            let ips = self.ips.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    ips.sweep();
                }
            })
        };
        let mut tasks = Vec::new();
        for (listener, settings) in listeners {
            let proxy = self.clone();
            tasks.push(tokio::spawn(async move {
                proxy.accept_loop(listener, settings).await
            }));
        }
        self.serving.store(true, Ordering::Relaxed);
        for t in tasks {
            let _ = t.await;
        }
        sweeper.abort();
        let drained = tokio::time::Instant::now() + SESSION_DRAIN;
        while self.active_sessions() > 0 && tokio::time::Instant::now() < drained {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn accept_loop(
        self: Arc<Self>,
        listener: Box<dyn Listener>,
        settings: Arc<ListenerSettings>,
    ) {
        let mut stop = self.shutdown_signal();
        loop {
            tokio::select! {
                () = crate::session::stopped(&mut stop) => return,
                r = listener.accept() => match r {
                    Ok(incoming) => {
                        let proxy = self.clone();
                        let settings = settings.clone();
                        proxy.active.fetch_add(1, Ordering::Relaxed);
                        tokio::spawn(async move {
                            crate::session::handle(proxy.clone(), settings, incoming).await;
                            proxy.active.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                    Err(e) => {
                        // E.g. out of file descriptors: back off instead of spinning.
                        warn!("accept on {}: {e}", settings.bind);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                },
            }
        }
    }
}
