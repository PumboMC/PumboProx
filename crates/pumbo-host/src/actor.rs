//! Plugin actors (plan §4.1, §4.4): one instance per plugin in its own tokio
//! task, a mailbox of events, every event a separate task in the instance
//! (`call_concurrent`), so tasks interleave at `await` points. A supervisor
//! restarts the instance after a trap with backoff and disables the plugin
//! after too many failures. Nothing here holds a lock across an `await`.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use wasmtime::component::{Accessor, Component};

use crate::HostInner;
use crate::config::PluginScope;
use crate::manifest::Manifest;
use crate::runtime::PluginState;
use crate::wit::admin::PluginDescription;
use crate::wit_bindings::Plugin;
use crate::wit_bindings::exports::pumbo::prox::events::Guest;

/// A job for the instance: starts one export call.
pub(crate) type Job =
    Box<dyn for<'a> FnOnce(&'a Accessor<PluginState>, &'a Guest) -> BoxFuture<'a, ()> + Send>;

/// Builds a [`Job`] (helps closure inference with the higher-ranked bound).
pub(crate) fn job<F>(f: F) -> Job
where
    F: for<'a> FnOnce(&'a Accessor<PluginState>, &'a Guest) -> BoxFuture<'a, ()> + Send + 'static,
{
    Box::new(f)
}

/// Tasks in flight per instance; beyond this the mailbox fills up and
/// callers get `Overloaded`.
const MAX_IN_FLIGHT: usize = 4096;
/// Graceful end of an instance (shutdown export and events in flight).
const DRAIN: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Starting,
    Running,
    /// After a trap, waiting for the next instance.
    Restarting(String),
    Reloading,
    /// `init` failed; retrying with backoff.
    InitFailed(String),
    /// Too many failures; only a reload starts it again.
    Disabled(String),
    /// No `.wasm` file.
    Missing,
    /// The `.wasm` does not load (compile error, or a new file with a manifest
    /// that does not read or names another plugin); only a reload tries again.
    Failed(String),
    Stopped,
}

impl Status {
    /// Running or about to run again.
    pub fn is_coming(&self) -> bool {
        matches!(
            self,
            Status::Starting | Status::Running | Status::Restarting(_) | Status::Reloading
        )
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Status::Disabled(_) | Status::Missing | Status::Failed(_) | Status::Stopped
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    #[error("plugin unavailable")]
    Unavailable,
    #[error("plugin overloaded")]
    Overloaded,
    #[error("deadline exceeded")]
    Timeout,
    #[error("plugin failed: {0}")]
    Failed(String),
}

pub(crate) enum Control {
    Reload(oneshot::Sender<Result<(), String>>),
    /// Down until a reload (`/pumbo proxy plugin unload`).
    Unload(oneshot::Sender<Result<(), String>>),
    Stop,
}

/// Status reason of a plugin an administrator unloaded.
pub(crate) const UNLOADED: &str = "unloaded";

/// Log lines per second with a summary of dropped lines.
#[derive(Debug, Default)]
pub(crate) struct LogLimiter {
    pub second: u64,
    pub count: u32,
    pub dropped: u64,
}

/// Host-side state of one plugin, shared by its actor and the host.
pub struct PluginSlot {
    pub id: String,
    pub manifest: Manifest,
    /// The manifest text the proxy started with (to compare on reload).
    pub(crate) manifest_text: String,
    /// What changed in the manifest of a reloaded file: the new code runs
    /// with the old manifest until the proxy restarts (D-STD-3).
    pub(crate) restart: Mutex<Option<String>>,
    pub wasm: PathBuf,
    pub scope: PluginScope,
    status: watch::Sender<Status>,
    mailbox: mpsc::Sender<Job>,
    control: mpsc::UnboundedSender<Control>,
    pub(crate) description: RwLock<Option<Arc<PluginDescription>>>,
    pub(crate) timers: Mutex<HashMap<u64, tokio::task::AbortHandle>>,
    next_id: AtomicU64,
    /// Instances started so far (a restart adds one).
    pub generation: AtomicU64,
    pub(crate) http: Semaphore,
    pub(crate) log: Mutex<LogLimiter>,
    /// Plugin message channels from `messaging.subscribe`.
    pub(crate) channels: Mutex<BTreeSet<String>>,
    /// Boss bars of the current instance: id → state.
    pub(crate) bars: Mutex<HashMap<u64, crate::imports::BarState>>,
    /// Virtual world inputs for this plugin, batched per tick.
    pub(crate) inputs: Mutex<Option<mpsc::Sender<pumbo_virtual::PlayerInput>>>,
}

pub(crate) struct Receivers {
    jobs: mpsc::Receiver<Job>,
    control: mpsc::UnboundedReceiver<Control>,
}

impl PluginSlot {
    pub(crate) fn new(
        manifest: Manifest,
        manifest_text: String,
        wasm: PathBuf,
        scope: PluginScope,
        mailbox: usize,
    ) -> (Arc<PluginSlot>, Receivers) {
        let (tx, jobs) = mpsc::channel(mailbox.max(1));
        let (control, crx) = mpsc::unbounded_channel();
        let initial = if wasm.exists() {
            Status::Starting
        } else {
            Status::Missing
        };
        let slot = Arc::new(PluginSlot {
            id: manifest.id.clone(),
            manifest,
            manifest_text,
            restart: Mutex::new(None),
            wasm,
            scope,
            status: watch::Sender::new(initial),
            mailbox: tx,
            control,
            description: RwLock::new(None),
            timers: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            generation: AtomicU64::new(0),
            http: Semaphore::new(16),
            log: Mutex::new(LogLimiter::default()),
            channels: Mutex::new(BTreeSet::new()),
            bars: Mutex::new(HashMap::new()),
            inputs: Mutex::new(None),
        });
        (slot, Receivers { jobs, control: crx })
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    /// The status for `/pumbo`, with the manifest changes that wait for a
    /// restart of the proxy.
    pub(crate) fn status_text(&self) -> String {
        let status = format!("{:?}", self.status());
        match self.restart.lock().ok().and_then(|r| r.clone()) {
            Some(changes) => format!("{status}, restart needed ({changes})"),
            None => status,
        }
    }

    pub(crate) fn set_status(&self, s: Status) {
        self.status.send_replace(s);
    }

    pub(crate) fn subscribe_status(&self) -> watch::Receiver<Status> {
        self.status.subscribe()
    }

    pub fn description(&self) -> Option<Arc<PluginDescription>> {
        self.description.read().ok().and_then(|d| d.clone())
    }

    pub(crate) fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Waits until the instance runs, at most `wait`.
    async fn wait_running(&self, wait: Duration) -> Result<(), CallError> {
        if *self.status.borrow() == Status::Running {
            return Ok(());
        }
        let mut rx = self.status.subscribe();
        let waited = tokio::time::timeout(
            wait,
            rx.wait_for(|s| *s == Status::Running || s.is_terminal()),
        )
        .await;
        match waited {
            Ok(Ok(s)) if *s == Status::Running => Ok(()),
            _ => Err(CallError::Unavailable),
        }
    }

    /// Queues a job without waiting for the instance.
    pub(crate) fn submit(&self, job: Job) -> Result<(), CallError> {
        if !self.status.borrow().is_coming() {
            return Err(CallError::Unavailable);
        }
        self.mailbox.try_send(job).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => CallError::Overloaded,
            mpsc::error::TrySendError::Closed(_) => CallError::Unavailable,
        })
    }

    /// Calls an export: waits for a running instance at most `wait`, then
    /// for the result at most `deadline`. The guest task cannot be cancelled
    /// in wasmtime 49; after the deadline its result is dropped.
    pub(crate) async fn call<R, F>(
        &self,
        wait: Duration,
        deadline: Duration,
        f: F,
    ) -> Result<R, CallError>
    where
        R: Send + 'static,
        F: for<'a> FnOnce(
                &'a Accessor<PluginState>,
                &'a Guest,
            ) -> BoxFuture<'a, wasmtime::Result<R>>
            + Send
            + 'static,
    {
        self.wait_running(wait).await?;
        let (tx, rx) = oneshot::channel();
        self.submit(job(move |acc, g| {
            Box::pin(async move {
                let r = f(acc, g).await;
                let _ = tx.send(r);
            })
        }))?;
        match tokio::time::timeout(deadline, rx).await {
            Err(_) => Err(CallError::Timeout),
            Ok(Err(_)) => Err(CallError::Failed("instance stopped".into())),
            Ok(Ok(Err(e))) => Err(CallError::Failed(format!("{e:#}"))),
            Ok(Ok(Ok(r))) => Ok(r),
        }
    }

    /// Reloads the plugin from its file; resolves when the new instance runs
    /// or fails to start. Gate events wait in the mailbox meanwhile.
    pub fn reload(&self) -> impl Future<Output = Result<(), String>> + Send + 'static {
        self.ask(Control::Reload)
    }

    /// Stops the instance and keeps it down until [`PluginSlot::reload`].
    /// Gates of the plugin deny meanwhile, a player it holds is kicked.
    pub fn unload(&self) -> impl Future<Output = Result<(), String>> + Send + 'static {
        self.ask(Control::Unload)
    }

    /// Sends the request at once, not when the answer is awaited, so requests
    /// reach the actor in the order they were made (an unload and a load right
    /// after it, each awaited in its own task).
    pub(crate) fn ask(
        &self,
        make: fn(oneshot::Sender<Result<(), String>>) -> Control,
    ) -> impl Future<Output = Result<(), String>> + Send + 'static {
        let (tx, rx) = oneshot::channel();
        let sent = self.control.send(make(tx)).is_ok();
        async move {
            if !sent {
                return Err("plugin actor stopped".to_string());
            }
            rx.await
                .unwrap_or_else(|_| Err("plugin actor stopped".to_string()))
        }
    }

    pub(crate) fn stop(&self) {
        let _ = self.control.send(Control::Stop);
    }

    pub(crate) fn cancel_timers(&self) {
        if let Ok(mut t) = self.timers.lock() {
            for (_, h) in t.drain() {
                h.abort();
            }
        }
    }
}

/// The reason of a trap without the wasm backtrace (for status and replies).
fn short_reason(msg: &str) -> String {
    match msg.rfind("wasm trap: ") {
        Some(i) => msg.get(i..).unwrap_or(msg).to_string(),
        None => msg.lines().last().unwrap_or(msg).to_string(),
    }
}

enum Outcome {
    Stopped,
    Reload(oneshot::Sender<Result<(), String>>),
    Unload(oneshot::Sender<Result<(), String>>),
    /// `init` true: the instance never ran (init error or timeout).
    Failed {
        init: bool,
        msg: String,
    },
}

#[allow(non_snake_case)]
fn InitFailed(msg: String) -> Outcome {
    Outcome::Failed { init: true, msg }
}

#[allow(non_snake_case)]
fn Trapped(msg: String) -> Outcome {
    Outcome::Failed { init: false, msg }
}

/// Without an instance until a reload (the answer channel) or a stop
/// (`None`); an unload meanwhile only marks the plugin unloaded.
async fn wait_for_reload(
    slot: &PluginSlot,
    rx: &mut Receivers,
) -> Option<oneshot::Sender<Result<(), String>>> {
    loop {
        match rx.control.recv().await {
            Some(Control::Reload(r)) => return Some(r),
            Some(Control::Unload(r)) => {
                slot.set_status(Status::Disabled(UNLOADED.into()));
                let _ = r.send(Ok(()));
            }
            _ => {
                slot.set_status(Status::Stopped);
                return None;
            }
        }
    }
}

pub(crate) fn spawn(host: Arc<HostInner>, slot: Arc<PluginSlot>, rx: Receivers) {
    tokio::spawn(supervise(host, slot, rx));
}

async fn supervise(host: Arc<HostInner>, slot: Arc<PluginSlot>, mut rx: Receivers) {
    let cfg = &host.cfg.plugins;
    let mut failures: VecDeque<Instant> = VecDeque::new();
    let mut reply: Option<oneshot::Sender<Result<(), String>>> = None;
    let mut component: Option<Component> = None;
    loop {
        if component.is_none() {
            let compiled = crate::manifest::check_update(&slot).and_then(|()| {
                host.runtime
                    .compile(&slot.wasm)
                    .map_err(|e| format!("{e:#}"))
            });
            match compiled {
                Ok(c) => component = Some(c),
                Err(e) => {
                    let missing = !slot.wasm.exists();
                    tracing::error!(plugin = %slot.id, "cannot load plugin: {}: {e}", slot.wasm.display());
                    let file = slot.wasm.file_name().unwrap_or_default().to_string_lossy();
                    let msg = format!("{file}: {e}");
                    slot.set_status(if missing {
                        Status::Missing
                    } else {
                        Status::Failed(msg.clone())
                    });
                    // After the status, so the caller sees it with the answer.
                    if let Some(r) = reply.take() {
                        let _ = r.send(Err(msg.clone()));
                    }
                    match wait_for_reload(&slot, &mut rx).await {
                        Some(r) => {
                            reply = Some(r);
                            continue;
                        }
                        None => return,
                    }
                }
            }
        }
        let Some(c) = component.as_ref() else {
            continue;
        };
        slot.generation.fetch_add(1, Ordering::SeqCst);
        let outcome = run_instance(&host, &slot, c, &mut rx, &mut reply).await;
        host.plugin_down(&slot);
        match outcome {
            Outcome::Stopped => {
                slot.set_status(Status::Stopped);
                return;
            }
            Outcome::Reload(r) => {
                slot.set_status(Status::Reloading);
                reply = Some(r);
                component = None;
                failures.clear();
            }
            Outcome::Unload(r) => {
                tracing::info!(plugin = %slot.id, "plugin unloaded");
                slot.set_status(Status::Disabled(UNLOADED.into()));
                let _ = r.send(Ok(()));
                let Some(r) = wait_for_reload(&slot, &mut rx).await else {
                    return;
                };
                reply = Some(r);
                component = None;
                failures.clear();
            }
            Outcome::Failed { init, msg } => {
                tracing::error!(plugin = %slot.id, init, "plugin failed: {msg}");
                let msg = short_reason(&msg);
                let now = Instant::now();
                failures.push_back(now);
                let window = Duration::from_millis(cfg.failure_window_ms);
                while failures
                    .front()
                    .is_some_and(|t| now.duration_since(*t) > window)
                {
                    failures.pop_front();
                }
                if failures.len() >= cfg.max_failures {
                    tracing::error!(plugin = %slot.id, "plugin disabled after {} failures", failures.len());
                    slot.set_status(Status::Disabled(msg.clone()));
                    if let Some(r) = reply.take() {
                        let _ = r.send(Err(msg));
                    }
                    let Some(r) = wait_for_reload(&slot, &mut rx).await else {
                        return;
                    };
                    reply = Some(r);
                    component = None;
                    failures.clear();
                    continue;
                }
                slot.set_status(if init {
                    Status::InitFailed(msg.clone())
                } else {
                    Status::Restarting(msg.clone())
                });
                if let Some(r) = reply.take() {
                    let _ = r.send(Err(msg));
                }
                let step = failures.len().saturating_sub(1);
                let backoff = cfg
                    .restart_backoff_ms
                    .get(step)
                    .or(cfg.restart_backoff_ms.last())
                    .copied()
                    .unwrap_or(1000);
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(backoff)) => {}
                    c = rx.control.recv() => match c {
                        Some(Control::Reload(r)) => {
                            reply = Some(r);
                            component = None;
                            failures.clear();
                        }
                        Some(Control::Unload(r)) => {
                            slot.set_status(Status::Disabled(UNLOADED.into()));
                            let _ = r.send(Ok(()));
                            let Some(r) = wait_for_reload(&slot, &mut rx).await else {
                                return;
                            };
                            reply = Some(r);
                            component = None;
                            failures.clear();
                        }
                        _ => {
                            slot.set_status(Status::Stopped);
                            return;
                        }
                    },
                }
            }
        }
    }
}

async fn run_instance(
    host: &Arc<HostInner>,
    slot: &Arc<PluginSlot>,
    component: &Component,
    rx: &mut Receivers,
    reply: &mut Option<oneshot::Sender<Result<(), String>>>,
) -> Outcome {
    let mut store = match host.runtime.store(host, slot) {
        Ok(s) => s,
        Err(e) => return InitFailed(format!("{e:#}")),
    };
    let instance =
        match Plugin::instantiate_async(&mut store, component, &host.runtime.linker).await {
            Ok(i) => i,
            Err(e) => return InitFailed(format!("instantiate: {e:#}")),
        };
    let init_deadline = Duration::from_millis(host.cfg.plugins.init_timeout_ms);
    // `init` registers the commands of this instance again.
    host.commands.clear_plugin(&slot.id);
    let result = store
        .run_concurrent(async |acc| -> Outcome {
            let g = instance.pumbo_prox_events();
            match tokio::time::timeout(init_deadline, g.call_init(acc)).await {
                Err(_) => return InitFailed("init timed out".into()),
                Ok(Err(e)) => return Trapped(format!("init: {e:#}")),
                Ok(Ok(Err(msg))) => return InitFailed(msg),
                Ok(Ok(Ok(()))) => {}
            }
            let desc = match g.call_describe(acc).await {
                Ok(d) => d,
                Err(e) => return Trapped(format!("describe: {e:#}")),
            };
            if let Err(msg) = host.plugin_ready(slot, desc) {
                return InitFailed(msg);
            }
            slot.set_status(Status::Running);
            if let Some(r) = reply.take() {
                let _ = r.send(Ok(()));
            }
            host.plugin_running(slot);
            serve(acc, g, rx).await
        })
        .await;
    match result {
        Ok(o) => o,
        Err(e) => Trapped(format!("{e:#}")),
    }
}

async fn serve(acc: &Accessor<PluginState>, g: &Guest, rx: &mut Receivers) -> Outcome {
    let mut inflight = FuturesUnordered::new();
    let outcome = loop {
        tokio::select! {
            biased;
            c = rx.control.recv() => break match c {
                Some(Control::Reload(r)) => Outcome::Reload(r),
                Some(Control::Unload(r)) => Outcome::Unload(r),
                _ => Outcome::Stopped,
            },
            Some(()) = inflight.next(), if !inflight.is_empty() => {}
            job = rx.jobs.recv(), if inflight.len() < MAX_IN_FLIGHT => match job {
                Some(job) => {
                    acc.with(|mut a| a.get().touch());
                    inflight.push(async move {
                        job(acc, g).await;
                        acc.with(|mut a| a.get().touch());
                    });
                }
                None => break Outcome::Stopped,
            },
        }
    };
    let drain = async {
        let _ = g.call_shutdown(acc).await;
        while inflight.next().await.is_some() {}
    };
    let _ = tokio::time::timeout(DRAIN, drain).await;
    outcome
}
