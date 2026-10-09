//! E0 spike: plugin host on wasmtime with component-model async.
//!
//! Model as in the plan (§4.1, §4.4): one instance per plugin, in its own
//! tokio task (an actor with a mailbox). Events arrive through a channel, and
//! every export call is a separate task in the instance (`call_concurrent`),
//! so tasks interleave at `await` points.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use tokio::sync::{mpsc, oneshot};
use wasmtime::component::{Accessor, Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Result, Store, UpdateDeadline};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../wit",
    world: "plugin",
    // Synchronous exports are also called through `call_concurrent` (with `Accessor`).
    exports: { default: async | store },
});

/// How often (ms) the engine epoch advances.
pub const EPOCH_TICK_MS: u64 = 10;

/// A call of a plugin export.
#[derive(Debug, Clone, Copy)]
pub enum Call {
    Command(u32),
    Event(u32),
    Sleep(u32),
    WasiSleep(u32),
    Block(u32),
    Spin,
    Panic,
    File(u32),
    Sync(u32),
}

struct Request {
    call: Call,
    reply: oneshot::Sender<Result<u32, String>>,
}

/// Host state in the `Store` of one plugin.
pub struct HostState {
    wasi: WasiCtx,
    table: ResourceTable,
    mailbox: mpsc::UnboundedSender<Request>,
    counter: u64,
    /// Number of consecutive epoch deadlines without a return to the host.
    busy_ticks: u32,
    budget_ticks: u32,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl pumbo::spike::host::Host for HostState {
    fn counter(&mut self) -> u64 {
        self.busy_ticks = 0;
        self.counter = self.counter.wrapping_add(1);
        self.counter
    }
}

impl<T> pumbo::spike::host::HostWithStore<T> for HasSelf<HostState> {
    async fn sleep_ms(accessor: &Accessor<T, Self>, ms: u32) {
        accessor.with(|mut a| a.get().busy_ticks = 0);
        if ms == 0 {
            tokio::task::yield_now().await;
        } else {
            tokio::time::sleep(Duration::from_millis(u64::from(ms))).await;
        }
    }

    async fn emit(accessor: &Accessor<T, Self>, n: u32) -> u32 {
        // Scenario #2056: the import dispatches an event to the same plugin
        // and waits for the result. The event goes through the actor's mailbox,
        // which runs it as a new task in the same instance.
        let mailbox = accessor.with(|mut a| {
            let state = a.get();
            state.busy_ticks = 0;
            state.mailbox.clone()
        });
        let (reply, answer) = oneshot::channel();
        if mailbox
            .send(Request {
                call: Call::Event(n),
                reply,
            })
            .is_err()
        {
            return u32::MAX;
        }
        match answer.await {
            Ok(Ok(v)) => v,
            _ => u32::MAX,
        }
    }
}

/// Engine with epoch interruption and a thread that ticks the epoch.
pub struct Runtime {
    pub engine: Engine,
    pub linker: Linker<HostState>,
    stop_ticker: Arc<AtomicBool>,
}

impl Runtime {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model_async(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::<HostState>::new(&engine);
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi::p3::add_to_linker(&mut linker)?;
        Plugin::add_to_linker::<_, HasSelf<HostState>>(&mut linker, |s| s)?;

        let stop_ticker = Arc::new(AtomicBool::new(false));
        let ticker_engine = engine.clone();
        let stop = Arc::clone(&stop_ticker);
        std::thread::Builder::new()
            .name("epoch-ticker".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(EPOCH_TICK_MS));
                    ticker_engine.increment_epoch();
                }
            })
            .map_err(|e| wasmtime::Error::msg(format!("epoch thread: {e}")))?;
        Ok(Self {
            engine,
            linker,
            stop_ticker,
        })
    }

    pub fn load(&self, path: &Path) -> Result<Component> {
        Component::from_file(&self.engine, path)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop_ticker.store(true, Ordering::Relaxed);
    }
}

/// Handle to a plugin actor.
pub struct PluginHandle {
    mailbox: mpsc::UnboundedSender<Request>,
    pub task: tokio::task::JoinHandle<Result<()>>,
}

impl PluginHandle {
    /// Starts the instance in its own tokio task. `budget_ticks` limits
    /// continuous guest work without returning to the host, in epoch ticks.
    pub async fn start(
        rt: &Runtime,
        component: &Component,
        budget_ticks: u32,
        data_dir: Option<&Path>,
    ) -> Result<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Request>();
        let mut wasi = WasiCtx::builder();
        if let Some(dir) = data_dir {
            wasi.preopened_dir(dir, "/data", FsPerms::ReadWrite)?;
        }
        let state = HostState {
            wasi: wasi.build(),
            table: ResourceTable::new(),
            mailbox: tx.clone(),
            counter: 0,
            busy_ticks: 0,
            budget_ticks,
        };
        let mut store = Store::new(&rt.engine, state);
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(|mut ctx| {
            let state = ctx.data_mut();
            state.busy_ticks = state.busy_ticks.saturating_add(1);
            if state.busy_ticks > state.budget_ticks {
                Err(wasmtime::Error::msg("plugin time budget exceeded"))
            } else {
                Ok(UpdateDeadline::Continue(1))
            }
        });
        let instance = Plugin::instantiate_async(&mut store, component, &rt.linker).await?;
        let task = tokio::spawn(async move {
            store
                .run_concurrent(async |acc| -> Result<()> {
                    let events = instance.pumbo_spike_events();
                    let mut inflight = FuturesUnordered::new();
                    loop {
                        tokio::select! {
                            req = rx.recv() => {
                                let Some(req) = req else { break };
                                inflight.push(async move {
                                    let result = match req.call {
                                        Call::Command(n) => events.call_on_command(acc, n).await,
                                        Call::Event(n) => events.call_on_event(acc, n).await,
                                        Call::Sleep(ms) => events.call_on_sleep(acc, ms).await,
                                        Call::WasiSleep(ms) => events.call_on_wasi_sleep(acc, ms).await,
                                        Call::Block(ms) => events.call_on_block(acc, ms).await,
                                        Call::Spin => events.call_on_spin(acc).await,
                                        Call::Panic => events.call_on_panic(acc).await,
                                        Call::File(n) => events.call_on_file(acc, n).await,
                                        Call::Sync(n) => events.call_on_sync(acc, n).await,
                                    };
                                    acc.with(|mut a| a.get().busy_ticks = 0);
                                    let _ = req.reply.send(result.map_err(|e| format!("{e:#}")));
                                });
                            }
                            Some(()) = inflight.next(), if !inflight.is_empty() => {}
                        }
                    }
                    while inflight.next().await.is_some() {}
                    Ok(())
                })
                .await?
        });
        Ok(Self { mailbox: tx, task })
    }

    /// Sends an event and waits for the result.
    pub async fn call(&self, call: Call) -> Result<u32, String> {
        let (reply, answer) = oneshot::channel();
        self.mailbox
            .send(Request { call, reply })
            .map_err(|_| "plugin actor is dead".to_string())?;
        answer
            .await
            .map_err(|_| "plugin actor ended without an answer".to_string())?
    }
}
