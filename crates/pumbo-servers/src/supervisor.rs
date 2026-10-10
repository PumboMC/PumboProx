//! Watching the server processes: start, ready, stop with a kill after the
//! timeout, restarts after crashes, taking over servers a crashed proxy left
//! running, stopping everything with the proxy.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::Manager;
use crate::runtime::{Exit, Log};
use crate::servers::Entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Stopped,
    Starting,
    /// Its port answers.
    Ready,
    Stopping,
    /// Crashed more often than `restart-on-crash` allows, or did not start.
    Crashed,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Stopped => "stopped",
            State::Starting => "starting",
            State::Ready => "ready",
            State::Stopping => "stopping",
            State::Crashed => "crashed",
        }
    }
}

/// A server for `list`.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub name: String,
    pub entry: Entry,
    pub state: State,
    pub pid: Option<u32>,
    /// Since the process started (or was taken over).
    pub uptime: Option<Duration>,
    pub rss_kib: Option<u64>,
}

pub(crate) struct Slot {
    state: State,
    pid: Option<u32>,
    since: Option<Instant>,
    log: Arc<Log>,
    stop: Option<watch::Sender<bool>>,
    task: Option<JoinHandle<()>>,
}

/// Waits between restarts after the 1st, 2nd and later crashes.
const BACKOFF: [u64; 3] = [1, 5, 15];
const CRASH_WINDOW: Duration = Duration::from_secs(600);
/// Program names of servers a crashed proxy may have left running (D-SRV-3).
const ORPHAN_NAMES: &[&str] = &["pumpkin", "java"];

impl Manager {
    fn slot<T>(&self, name: &str, f: impl FnOnce(&mut Slot) -> T) -> T {
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = slots.entry(name.to_string()).or_insert_with(|| Slot {
            state: State::Stopped,
            pid: None,
            since: None,
            log: Arc::new(Log::new(self.config.dir.join(name).join("console.log"))),
            stop: None,
            task: None,
        });
        f(slot)
    }

    fn set(&self, name: &str, state: State, pid: Option<u32>) {
        self.slot(name, |s| {
            if s.pid != pid {
                s.since = pid.map(|_| Instant::now());
            }
            s.state = state;
            s.pid = pid;
        });
    }

    fn save_pid(&self, name: &str, pid: Option<u32>) {
        let r = self.edit(|s| {
            if let Some(e) = s.entries.get_mut(name) {
                e.pid = pid;
            }
        });
        if let Err(e) = r {
            warn!("{e}");
        }
    }

    pub fn state(&self, name: &str) -> State {
        self.slot(name, |s| s.state)
    }

    pub fn list(&self) -> Vec<ServerInfo> {
        self.entries()
            .into_iter()
            .map(|(name, entry)| {
                let (state, pid, since) = self.slot(&name, |s| (s.state, s.pid, s.since));
                ServerInfo {
                    rss_kib: pid
                        .and_then(|p| self.runner.status(p))
                        .and_then(|s| s.rss_kib),
                    uptime: since.map(|s| s.elapsed()),
                    name,
                    entry,
                    state,
                    pid,
                }
            })
            .collect()
    }

    /// The last `n` console lines of a server.
    pub fn logs(&self, name: &str, n: usize) -> Result<Vec<String>, String> {
        if self.entry(name).is_none() {
            return Err(format!("no server named {name}"));
        }
        Ok(self.slot(name, |s| s.log.tail(n)))
    }

    /// A line on the server's console.
    pub fn console(&self, name: &str, line: &str) -> Result<(), String> {
        let pid = self
            .slot(name, |s| s.pid)
            .ok_or_else(|| format!("{name} is not running"))?;
        self.runner.write_stdin(pid, line)
    }

    /// Starts a server and watches it; returns at once.
    pub fn start(self: &Arc<Self>, name: &str) -> Result<(), String> {
        self.watch(name, None)
    }

    fn watch(self: &Arc<Self>, name: &str, orphan: Option<u32>) -> Result<(), String> {
        if self.entry(name).is_none() {
            return Err(format!("no server named {name}"));
        }
        if self.closing.load(Ordering::Relaxed) {
            return Err("the proxy is stopping".into());
        }
        let (tx, rx) = watch::channel(false);
        let free = self.slot(name, |s| {
            let running = s.stop.is_some() && s.task.as_ref().is_none_or(|t| !t.is_finished());
            if !running {
                s.state = State::Starting;
                s.stop = Some(tx);
                s.task = None;
            }
            !running
        });
        if !free {
            return Err(format!("{name} is running already"));
        }
        let task = tokio::spawn(self.clone().supervise(name.to_string(), rx, orphan));
        self.slot(name, |s| s.task = Some(task));
        Ok(())
    }

    /// Stops a server (console `stop`, a kill after `stop-timeout-secs`) and
    /// waits for it.
    pub async fn stop(&self, name: &str) -> Result<(), String> {
        if self.entry(name).is_none() {
            return Err(format!("no server named {name}"));
        }
        let (stop, task) = self.slot(name, |s| (s.stop.take(), s.task.take()));
        if let Some(stop) = stop {
            let _ = stop.send(true);
        }
        if let Some(task) = task {
            let _ = task.await;
        }
        Ok(())
    }

    pub async fn restart(self: &Arc<Self>, name: &str) -> Result<(), String> {
        self.stop(name).await?;
        self.start(name)
    }

    /// At proxy start: takes over servers a crashed proxy left running
    /// (their PID in `servers.yml` belongs to a `pumpkin` or `java`
    /// process), then starts the `autostart` ones.
    pub fn boot(self: &Arc<Self>) {
        for (name, entry) in self.entries() {
            let alive = entry.pid.and_then(|pid| {
                let st = self.runner.status(pid)?;
                ORPHAN_NAMES.contains(&st.name.as_str()).then_some(pid)
            });
            let r = match alive {
                Some(pid) => {
                    info!(
                        "taking over server {name} (pid {pid}), left running by the previous proxy"
                    );
                    self.watch(&name, Some(pid))
                }
                None => {
                    if entry.pid.is_some() {
                        self.save_pid(&name, None);
                    }
                    if entry.autostart {
                        self.start(&name)
                    } else {
                        Ok(())
                    }
                }
            };
            if let Err(e) = r {
                warn!("server {name}: {e}");
            }
        }
    }

    /// Stops every server in parallel; no restarts from now on.
    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::Relaxed);
        let names: Vec<String> = self.entries().into_keys().collect();
        let stops = names.iter().map(|n| self.stop(n));
        let limit = Duration::from_secs(self.config.stop_timeout_secs + 10);
        if tokio::time::timeout(limit, futures::future::join_all(stops))
            .await
            .is_err()
        {
            warn!("servers did not stop in {} s", limit.as_secs());
        }
    }

    async fn supervise(
        self: Arc<Self>,
        name: String,
        mut stop: watch::Receiver<bool>,
        mut orphan: Option<u32>,
    ) {
        let mut crashes: Vec<Instant> = Vec::new();
        loop {
            let (pid, mut exited) = match orphan.take() {
                Some(pid) => {
                    self.set(&name, State::Ready, Some(pid));
                    (pid, self.watch_pid(pid))
                }
                None => match self.spawn(&name) {
                    Ok(s) => {
                        self.set(&name, State::Starting, Some(s.pid));
                        self.save_pid(&name, Some(s.pid));
                        tokio::spawn(self.clone().wait_ready(name.clone(), s.pid));
                        (s.pid, s.exited)
                    }
                    Err(e) => {
                        error!("server {name} did not start: {e}");
                        self.slot(&name, |s| {
                            s.log.push(&format!("[PumboProx] not started: {e}"))
                        });
                        self.set(&name, State::Crashed, None);
                        return;
                    }
                },
            };
            let exit: Option<Exit> = tokio::select! {
                e = &mut exited => Some(e.unwrap_or(None)),
                _ = stop.wait_for(|s| *s) => None,
            };
            let code = match exit {
                None => {
                    self.set(&name, State::Stopping, Some(pid));
                    if let Err(e) = self.runner.stop(pid) {
                        warn!("server {name}: {e}");
                    }
                    let limit = Duration::from_secs(self.config.stop_timeout_secs);
                    if tokio::time::timeout(limit, &mut exited).await.is_err() {
                        warn!(
                            "server {name} did not stop in {} s, killing pid {pid}",
                            limit.as_secs()
                        );
                        let _ = self.runner.kill(pid);
                        let _ = tokio::time::timeout(Duration::from_secs(10), exited).await;
                    }
                    info!("server {name} stopped");
                    self.set(&name, State::Stopped, None);
                    self.save_pid(&name, None);
                    return;
                }
                Some(code) => code,
            };
            self.save_pid(&name, None);
            if code == Some(0) {
                info!("server {name} stopped by itself");
                self.set(&name, State::Stopped, None);
                return;
            }
            let limit = self.template_of(&name).restart_on_crash as usize;
            crashes.retain(|t| t.elapsed() < CRASH_WINDOW);
            if crashes.len() >= limit || self.closing.load(Ordering::Relaxed) {
                error!(
                    "server {name} crashed (exit {code:?}); {} crash(es) in 10 minutes, not restarting it",
                    crashes.len() + 1
                );
                self.set(&name, State::Crashed, None);
                return;
            }
            let wait = BACKOFF
                .get(crashes.len())
                .or(BACKOFF.last())
                .copied()
                .unwrap_or(15);
            crashes.push(Instant::now());
            warn!("server {name} crashed (exit {code:?}), restarting it in {wait} s");
            self.set(&name, State::Starting, None);
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(wait)) => {}
                _ = stop.wait_for(|s| *s) => {
                    self.set(&name, State::Stopped, None);
                    return;
                }
            }
        }
    }

    fn template_of(&self, name: &str) -> crate::Template {
        self.entry(name)
            .and_then(|e| self.config.templates().remove(&e.template))
            .unwrap_or_default()
    }

    fn spawn(&self, name: &str) -> Result<crate::runtime::Started, String> {
        let entry = self.entry(name).ok_or("no such server")?;
        let source = self.versions.source(&entry.source)?;
        let file = self
            .versions
            .file(&entry.source, &entry.version)
            .ok_or_else(|| {
                format!(
                    "{} {} is not downloaded (/prox download {} {})",
                    entry.source, entry.version, entry.source, entry.version
                )
            })?;
        // Absolute: the process runs in the server's folder.
        let file = std::path::absolute(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let launch = source.launch(&file, &self.template_of(name))?;
        let dir = self.config.dir.join(name);
        let log = self.slot(name, |s| s.log.clone());
        self.runner.start(&dir, &launch, log)
    }

    /// `Ready` once the server's port answers.
    async fn wait_ready(self: Arc<Self>, name: String, pid: u32) {
        let Some(port) = self.entry(&name).map(|e| e.port) else {
            return;
        };
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let (state, now) = self.slot(&name, |s| (s.state, s.pid));
            if state != State::Starting || now != Some(pid) {
                return;
            }
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                self.slot(&name, |s| {
                    if s.state == State::Starting && s.pid == Some(pid) {
                        s.state = State::Ready;
                    }
                });
                info!("server {name} is ready on 127.0.0.1:{port}");
                return;
            }
        }
    }

    /// A taken-over process has no exit status: its PID is polled.
    fn watch_pid(&self, pid: u32) -> oneshot::Receiver<Exit> {
        let (tx, rx) = oneshot::channel();
        let runner = self.runner.clone();
        tokio::spawn(async move {
            while runner.status(pid).is_some() {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            let _ = tx.send(None);
        });
        rx
    }
}
