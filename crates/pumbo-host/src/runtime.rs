//! wasmtime engine, linker and the per-instance store state.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use wasmtime::component::{Component, HasSelf, Linker, ResourceTable, TypedFunc};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder, UpdateDeadline};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxView, WasiView};

use crate::HostInner;
use crate::actor::PluginSlot;
use crate::wit_bindings::Plugin;

/// How often (ms) the engine epoch advances (D-E0-2).
pub const EPOCH_TICK_MS: u64 = 10;

/// Continuous work of the guest without waiting for the host (synchronous
/// imports do not count as waiting) is stopped after this many budgets...
const RUN_BUDGETS: u32 = 5;
/// ...but not before this long (the default event deadline), so a long
/// command that only calls synchronous imports, such as an import of
/// thousands of players, still finishes. The guest gives the worker back
/// every epoch tick meanwhile.
const MIN_RUN_MS: u32 = 5_000;

/// Epoch ticks for a budget in milliseconds.
fn budget_ticks(ms: u32) -> u32 {
    ms.div_ceil(u32::try_from(EPOCH_TICK_MS).unwrap_or(10))
}

/// Epoch ticks of continuous work allowed with a budget in milliseconds.
fn run_ticks(ms: u32) -> u32 {
    budget_ticks(ms)
        .saturating_mul(RUN_BUDGETS)
        .max(budget_ticks(MIN_RUN_MS))
}

/// The optional `suggest` export: (player, command name, arguments).
pub(crate) type SuggestFn = TypedFunc<(u64, String, String), (Option<Vec<String>>,)>;

/// Store data of one plugin instance.
pub struct PluginState {
    wasi: WasiCtx,
    pub(crate) table: ResourceTable,
    limits: StoreLimits,
    /// Ticks since the guest last returned to the host.
    busy_ticks: u32,
    budget_ticks: u32,
    /// Ticks since the guest last waited for the host or a call ended.
    run_ticks: u32,
    run_limit: u32,
    /// `init` must end by then.
    pub(crate) init_by: Option<std::time::Instant>,
    pub(crate) host: Arc<HostInner>,
    pub(crate) slot: Arc<PluginSlot>,
    /// Set when the instance has the `suggest` export.
    pub(crate) suggest: Option<SuggestFn>,
}

impl PluginState {
    /// The guest returned to the host: the budget limits continuous guest
    /// work, not the length of an event (D-E0-2). Called by every import of
    /// `pumbo:prox`. (A store call hook cannot do it: it also fires for the
    /// epoch libcall itself.) A loop of synchronous imports is still stopped
    /// by the run limit, which only [`PluginState::waited`] resets.
    pub(crate) fn touch(&mut self) {
        self.busy_ticks = 0;
    }

    /// The guest waits for the host (an async import) or a call ended.
    pub(crate) fn waited(&mut self) {
        self.busy_ticks = 0;
        self.run_ticks = 0;
    }

    /// Sets the budget of continuous guest work: `load-budget-ms` while the
    /// plugin loads, `budget-ms` once it handles events.
    pub(crate) fn set_budget(&mut self, ms: u32) {
        self.waited();
        self.budget_ticks = budget_ticks(ms);
        self.run_limit = run_ticks(ms);
    }
}

impl WasiView for PluginState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Engine with epoch interruption and the thread that ticks it.
pub(crate) struct Runtime {
    pub engine: Engine,
    pub linker: Linker<PluginState>,
    stop_ticker: Arc<AtomicBool>,
}

impl Runtime {
    pub fn new() -> wasmtime::Result<Runtime> {
        let mut config = Config::new();
        config.wasm_component_model_async(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::<PluginState>::new(&engine);
        // WASI 0.2 for std (no sockets or environment are granted in the
        // context), and from WASI 0.3 only the clocks (D-E0-1).
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        wasmtime_wasi::p3::clocks::add_to_linker(&mut linker)?;
        Plugin::add_to_linker::<_, HasSelf<PluginState>>(&mut linker, |s| s)?;

        let stop_ticker = Arc::new(AtomicBool::new(false));
        let ticker_engine = engine.clone();
        let stop = Arc::clone(&stop_ticker);
        std::thread::Builder::new()
            .name("pumbo-epoch".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(EPOCH_TICK_MS));
                    ticker_engine.increment_epoch();
                }
            })
            .map_err(|e| wasmtime::Error::msg(format!("epoch thread: {e}")))?;
        Ok(Runtime {
            engine,
            linker,
            stop_ticker,
        })
    }

    pub fn compile(&self, path: &Path) -> wasmtime::Result<Component> {
        Component::from_file(&self.engine, path)
    }

    /// A store for a new instance: WASI with `/data` (read-write) and
    /// `/config` (read-only, with the default `config.yml` and `lang/` written
    /// at the first start), memory limit and time budget.
    pub fn store(
        &self,
        host: &Arc<HostInner>,
        slot: &Arc<PluginSlot>,
    ) -> wasmtime::Result<Store<PluginState>> {
        let cfg = &host.cfg.plugins;
        let data = cfg.data_dir(&slot.id);
        let conf = cfg.config_dir(&slot.id);
        std::fs::create_dir_all(&data)
            .map_err(|e| wasmtime::Error::msg(format!("{}: {e}", data.display())))?;
        std::fs::create_dir_all(&conf)
            .map_err(|e| wasmtime::Error::msg(format!("{}: {e}", conf.display())))?;
        crate::manifest::default_files(&slot.id, &slot.wasm, &conf, &data);
        let mut wasi = WasiCtx::builder();
        wasi.preopened_dir(&data, "/data", FsPerms::ReadWrite)?
            .preopened_dir(&conf, "/config", FsPerms::ReadOnly)?
            .allow_tcp(false)
            .allow_udp(false)
            .allow_ip_name_lookup(false);
        let memory = usize::try_from(cfg.memory_mb)
            .unwrap_or(usize::MAX)
            .saturating_mul(1024 * 1024);
        let state = PluginState {
            wasi: wasi.build(),
            table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size(memory)
                .trap_on_grow_failure(true)
                .build(),
            busy_ticks: 0,
            // A new instance starts by loading.
            budget_ticks: budget_ticks(cfg.load_budget_ms),
            run_ticks: 0,
            run_limit: run_ticks(cfg.load_budget_ms),
            init_by: None,
            host: Arc::clone(host),
            slot: Arc::clone(slot),
            suggest: None,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        store.set_epoch_deadline(1);
        // Fires once per crossed deadline, so the counters count the ticks in
        // which the guest ran. Yielding gives the worker back to tokio (other
        // tasks, `serve`'s control branch, timeouts); every guest call runs
        // through `run_concurrent` or `call_concurrent`, where that is allowed.
        store.epoch_deadline_callback(|mut ctx| {
            let state = ctx.data_mut();
            state.busy_ticks = state.busy_ticks.saturating_add(1);
            state.run_ticks = state.run_ticks.saturating_add(1);
            if state.busy_ticks > state.budget_ticks {
                Err(wasmtime::Error::msg("plugin time budget exceeded"))
            } else if state.run_ticks > state.run_limit {
                Err(wasmtime::Error::msg(
                    "plugin ran too long without waiting for the host",
                ))
            } else if state.slot.must_stop() {
                Err(wasmtime::Error::msg("plugin stopped"))
            } else if state
                .init_by
                .is_some_and(|t| std::time::Instant::now() >= t)
            {
                Err(wasmtime::Error::msg("init timed out"))
            } else {
                Ok(UpdateDeadline::Yield(1))
            }
        });
        Ok(store)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop_ticker.store(true, Ordering::Relaxed);
    }
}
