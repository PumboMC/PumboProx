//! E0 spike guest: async exports and imports in the component model.

wit_bindgen::generate!({
    path: "../wit",
    world: "plugin",
});

use exports::pumbo::spike::events::Guest;
use pumbo::spike::host;

struct Spike;

impl Guest for Spike {
    async fn on_command(n: u32) -> u32 {
        host::emit(n).await.wrapping_add(1)
    }

    async fn on_event(n: u32) -> u32 {
        if n == 0 {
            1000
        } else {
            host::emit(n - 1).await.wrapping_add(1)
        }
    }

    async fn on_sleep(ms: u32) -> u32 {
        host::sleep_ms(ms).await;
        ms
    }

    async fn on_wasi_sleep(ms: u32) -> u32 {
        let start = wasip3::clocks::monotonic_clock::now();
        wasip3::clocks::monotonic_clock::wait_for(u64::from(ms) * 1_000_000).await;
        let elapsed = wasip3::clocks::monotonic_clock::now().saturating_sub(start);
        u32::try_from(elapsed / 1_000_000).unwrap_or(u32::MAX)
    }

    async fn on_block(ms: u32) -> u32 {
        std::thread::sleep(std::time::Duration::from_millis(u64::from(ms)));
        ms
    }

    #[allow(clippy::empty_loop)]
    async fn on_spin() -> u32 {
        let mut x: u32 = 0;
        loop {
            x = std::hint::black_box(x.wrapping_add(1));
        }
    }

    #[allow(clippy::panic)]
    async fn on_panic() -> u32 {
        panic!("deliberate panic in the spike")
    }

    async fn on_file(n: u32) -> u32 {
        let path = format!("/data/file-{n}.txt");
        if std::fs::write(&path, n.to_string()).is_err() {
            return u32::MAX;
        }
        host::sleep_ms(1).await;
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(u32::MAX)
    }

    fn on_sync(n: u32) -> u32 {
        n.wrapping_mul(2)
            .wrapping_add(u32::try_from(host::counter() % 7).unwrap_or(0))
    }
}

export!(Spike);
