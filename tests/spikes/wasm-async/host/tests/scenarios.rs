//! E0 spike scenarios. Measurements are printed to stderr
//! (`cargo test -p spike-wasm-async --release -- --nocapture --test-threads=1`).

// Test code: helpers outside `#[test]` may also abort the test.
#![allow(clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use spike_wasm_async::{Call, PluginHandle, Runtime};
use wasmtime::component::Component;

/// Builds the guest for `wasm32-wasip2` once per test run.
fn guest_wasm() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        if let Ok(p) = std::env::var("PUMBO_SPIKE_GUEST") {
            return PathBuf::from(p);
        }
        let host_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let guest_dir = host_dir.join("../guest");
        let target_dir = host_dir.join("../../../../target/spike-guest");
        let status = Command::new(env!("CARGO"))
            .args(["build", "--release", "--target", "wasm32-wasip2"])
            .arg("--manifest-path")
            .arg(guest_dir.join("Cargo.toml"))
            .arg("--target-dir")
            .arg(&target_dir)
            .status()
            .expect("cargo build of the guest");
        assert!(status.success(), "building the guest failed");
        target_dir.join("wasm32-wasip2/release/spike_wasm_async_guest.wasm")
    })
    .clone()
}

fn runtime_and_component() -> (Runtime, Component) {
    let rt = Runtime::new().expect("engine");
    let path = guest_wasm();
    let started = Instant::now();
    let component = rt.load(&path).expect("component compilation");
    eprintln!(
        "[measure] guest size {} B, component compilation {:?}",
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        started.elapsed()
    );
    (rt, component)
}

const BUDGET_TICKS: u32 = 20; // 200 ms at a 10 ms tick

async fn with_timeout<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), f)
        .await
        .unwrap_or_else(|_| panic!("{what}: hang (no result within 10 s)"))
}

/// Scenario #2056: a handler calls an import that fires an event at the same
/// plugin and waits for its result.
#[tokio::test(flavor = "multi_thread")]
async fn reentrancy_2056() {
    let (rt, component) = runtime_and_component();
    let started = Instant::now();
    let plugin = PluginHandle::start(&rt, &component, BUDGET_TICKS, None)
        .await
        .expect("instance");
    eprintln!("[measure] instantiation {:?}", started.elapsed());

    // on-command(0) -> emit(0) -> on-event(0) = 1000 -> +1
    let r = with_timeout("on-command", plugin.call(Call::Command(0))).await;
    assert_eq!(r, Ok(1001));

    // Chain: on-command(10) -> on-event(10) -> ... -> on-event(0):
    // 11 tasks of one instance suspended at the same time.
    let r = with_timeout("chain", plugin.call(Call::Command(10))).await;
    assert_eq!(r, Ok(1011));

    // Deep chain: 200 suspended tasks in one instance.
    let r = with_timeout("chain 200", plugin.call(Call::Command(200))).await;
    assert_eq!(r, Ok(1201));

    // Many chains at once.
    let started = Instant::now();
    let calls = (0..200).map(|i| plugin.call(Call::Command(i % 20)));
    let results = with_timeout("200 chains", futures::future::join_all(calls)).await;
    for (i, r) in results.into_iter().enumerate() {
        let i = u32::try_from(i).unwrap();
        assert_eq!(r, Ok(1001 + i % 20));
    }
    eprintln!(
        "[measure] 200 parallel reentrancy chains (depth 0..19): {:?}",
        started.elapsed()
    );
}

/// Tasks interleave at `await` points: 1000 handlers waiting 100 ms finish
/// in about 100 ms, not 100 s.
#[tokio::test(flavor = "multi_thread")]
async fn tasks_interleave() {
    let (rt, component) = runtime_and_component();
    let plugin = PluginHandle::start(&rt, &component, BUDGET_TICKS, None)
        .await
        .expect("instance");

    for (name, call) in [
        ("host import sleep-ms", Call::Sleep(100)),
        ("wasi:clocks@0.3.0 wait-for", Call::WasiSleep(100)),
    ] {
        let started = Instant::now();
        let calls = (0..1000).map(|_| plugin.call(call));
        let results = with_timeout(name, futures::future::join_all(calls)).await;
        let elapsed = started.elapsed();
        assert!(
            results.iter().all(|r| matches!(r, Ok(v) if *v >= 99)),
            "{results:?}"
        );
        eprintln!("[measure] 1000 x 100 ms through {name}: {elapsed:?}");
        assert!(elapsed < Duration::from_secs(3), "{name}: no interleaving");
    }
}

/// A blocking WASI 0.2 call (std::thread::sleep) stops the whole instance:
/// other events wait. Consequence for the SDK: plugins use async imports only.
#[tokio::test(flavor = "multi_thread")]
async fn blocking_wasi_p2_stops_the_instance() {
    let (rt, component) = runtime_and_component();
    let plugin = PluginHandle::start(&rt, &component, 1000, None)
        .await
        .expect("instance");
    let block = plugin.call(Call::Block(300));
    let probe = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let started = Instant::now();
        let r = plugin.call(Call::Sync(1)).await;
        (r, started.elapsed())
    };
    let (b, (r, waited)) = with_timeout("blocking", async { tokio::join!(block, probe) }).await;
    assert_eq!(b, Ok(300));
    assert!(r.is_ok());
    eprintln!("[measure] on-sync during a blocking 300 ms sleep waited {waited:?}");
}

/// A trap in one task (guest panic) and its effect on the instance.
#[tokio::test(flavor = "multi_thread")]
async fn guest_panic() {
    let (rt, component) = runtime_and_component();
    let plugin = PluginHandle::start(&rt, &component, BUDGET_TICKS, None)
        .await
        .expect("instance");
    // A task suspended while another task traps.
    let sleeper = plugin.call(Call::Sleep(300));
    let panic = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        plugin.call(Call::Panic).await
    };
    let (s, p) = with_timeout("panic", async { tokio::join!(sleeper, panic) }).await;
    eprintln!("[result] panic: {p:?}");
    eprintln!("[result] task suspended during the panic: {s:?}");
    assert!(p.is_err());
    let after = with_timeout("after panic", plugin.call(Call::Sync(1))).await;
    eprintln!("[result] call after the panic: {after:?}");
    let ended = plugin.task.await;
    eprintln!("[result] actor task after the panic: {ended:?}");
    // After a trap the instance is unusable; the host creates a new one (§4.4 item 7).
    let fresh = PluginHandle::start(&rt, &component, BUDGET_TICKS, None)
        .await
        .expect("new instance");
    assert_eq!(
        with_timeout("new", fresh.call(Call::Command(0))).await,
        Ok(1001)
    );
}

/// An endless loop interrupted by the epoch budget.
#[tokio::test(flavor = "multi_thread")]
async fn loop_interrupted_by_epochs() {
    let (rt, component) = runtime_and_component();
    let plugin = PluginHandle::start(&rt, &component, BUDGET_TICKS, None)
        .await
        .expect("instance");
    let started = Instant::now();
    let r = with_timeout("loop", plugin.call(Call::Spin)).await;
    let elapsed = started.elapsed();
    eprintln!("[result] loop: {r:?} after {elapsed:?}");
    assert!(r.is_err());
    assert!(elapsed < Duration::from_secs(2));
    let ended = plugin.task.await;
    eprintln!("[result] actor task after the loop: {ended:?}");
}

/// Call cost: synchronous export and async export with reentrancy.
#[tokio::test(flavor = "multi_thread")]
async fn call_cost() {
    let (rt, component) = runtime_and_component();
    let plugin = PluginHandle::start(&rt, &component, BUDGET_TICKS, None)
        .await
        .expect("instance");
    let n = 20_000u32;

    let started = Instant::now();
    for i in 0..n {
        let r = plugin.call(Call::Sync(i)).await;
        assert!(r.is_ok(), "{r:?}");
    }
    let sync = started.elapsed();

    let started = Instant::now();
    for _ in 0..n {
        assert!(plugin.call(Call::Sleep(0)).await.is_ok());
    }
    let async_one = started.elapsed();

    let started = Instant::now();
    for _ in 0..n {
        assert_eq!(plugin.call(Call::Command(0)).await, Ok(1001));
    }
    let reentrant = started.elapsed();

    let started = Instant::now();
    let calls = (0..n).map(|i| plugin.call(Call::Sync(i)));
    let results = futures::future::join_all(calls).await;
    assert!(results.iter().all(Result::is_ok));
    let sync_batch = started.elapsed();

    let per = |d: Duration| d.as_nanos() / u128::from(n);
    eprintln!(
        "[measure] {n} calls: sync one by one {} ns/call, async with import (yield) {} ns/call, \
         reentrancy (2 events) {} ns/call, sync all at once {} ns/call",
        per(sync),
        per(async_one),
        per(reentrant),
        per(sync_batch)
    );
}

/// Memory: 200 instances of the same plugin.
#[tokio::test(flavor = "multi_thread")]
async fn instance_memory() {
    let (rt, component) = runtime_and_component();
    let rss_before = rss_kb();
    let mut plugins = Vec::new();
    let started = Instant::now();
    for _ in 0..200 {
        plugins.push(
            PluginHandle::start(&rt, &component, BUDGET_TICKS, None)
                .await
                .expect("instance"),
        );
    }
    let elapsed = started.elapsed();
    for p in &plugins {
        assert_eq!(p.call(Call::Command(0)).await, Ok(1001));
    }
    let rss_after = rss_kb();
    eprintln!(
        "[measure] 200 instances: {:?} ({} µs/instance), RSS +{} KB (~{} KB/instance)",
        elapsed,
        elapsed.as_micros() / 200,
        rss_after.saturating_sub(rss_before),
        rss_after.saturating_sub(rss_before) / 200
    );
}

fn rss_kb() -> u64 {
    let pid = std::process::id().to_string();
    Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Files through std (WASI 0.2) in async handlers, interleaved.
#[tokio::test(flavor = "multi_thread")]
async fn files_in_async_handlers() {
    let (rt, component) = runtime_and_component();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("spike-data");
    std::fs::create_dir_all(&dir).unwrap();
    let plugin = PluginHandle::start(&rt, &component, BUDGET_TICKS, Some(&dir))
        .await
        .expect("instance");
    let calls = (0..100).map(|i| plugin.call(Call::File(i)));
    let results = with_timeout("files", futures::future::join_all(calls)).await;
    for (i, r) in results.into_iter().enumerate() {
        assert_eq!(r, Ok(u32::try_from(i).unwrap()));
    }
    assert!(dir.join("file-7.txt").exists());
}
