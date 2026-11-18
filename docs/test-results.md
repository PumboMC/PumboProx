# Test results

## Load benchmark

`tools/bench.sh`: bots through the proxy to Pumpkin 0.2.0, without a pass mark.

| date | commit | machine | bots | joined | stayed | ramp s | join p50 ms | p95 ms | max ms |
|---|---|---|---|---|---|---|---|---|---|
| 2026-10-28T05:18Z | b00238b | Darwin arm64 | 200 idle at 20/s, 30 s | 200 | 200 | 10.0 | 3.5 | 10.8 | 18.7 |

## Memory and CPU under load

Offline bots from pumbo-testclient's loadgen example join through the proxy to one Pumpkin server and stay idle. RSS is the proxy process in the steady state (10 to 35 s after the last join), CPU the average share of one core in that window, join times from the bots. Every bot joined and stayed.

| date | build | machine | backend | idle | 500 bots | 1000 bots | CPU 500 | CPU 1000 | join p50 / p95 ms (1000) |
|---|---|---|---|---|---|---|---|---|---|
| 2026-11-02 | development build of 2026-11-01 | Linux x86_64, 8 vCPU | Pumpkin 0.2.1 | 36 MB | 131 MB | 246 MB | 20% | 28% | 8.8 / 14.1 |
| 2026-11-02 | 0.2.5-beta | Linux x86_64, 8 vCPU | Pumpkin 0.2.1 | 35 MB | 352 MB | 681 MB | 17% | 41% | 6.6 / 14.1 |

Setup: proxy in offline mode with modern forwarding, no plugins, per-address limits raised to 5000, open-file limit 65536; Pumpkin with view and simulation distance 4; bots on protocol 777 joining 20 a second and staying 40 s, 500 and 1000 in separate runs on the same proxy process. One run per build on a shared machine, so differences of 10 to 20% can be noise. The development build has the buffer fixes that came after 0.2.5-beta: buffers shrink after a burst and give back memory after big frames.

## Session hooks

The bridge's teleport gate and the answers on the old backend plugin channel moved out of the session loop into session hooks; for every frame the loop now checks a bit mask of the packet kinds its hooks want.

### Forwarding one frame

`forward_one_frame` in `crates/pumbo-prox/src/play/forward_bench.rs` (ignored, release build): a session in play over in-memory pipes forwards 64 Mi frames of one kind, uncompressed on both sides. Instructions retired by the process (`/usr/bin/time -l`) minus the median of five runs that only set the session up, per frame, median of three runs. Mac16,8, macOS 27, on mains power, 2026-11-08. Before the hooks is the commit that adds the benchmark, with the hooks the one that moves the messaging channel onto a hook.

| frame | before the hooks | with the hooks | change |
|---|---|---|---|
| entity move from the server, a kind the proxy does not read | 820.6 | 803.4 | -2.1% |
| `set_time` from the server, no hook wants it | 830.5 | 824.5 | -0.7% |
| `player_position` from the server, the teleport gate | 2358.7 | 2335.6 | -1.0% |
| `move_player_pos` from the client, no hook wants it | 1338.2 | 1344.4 | +0.5% |
| `accept_teleportation` from the client, the teleport gate | 1760.0 | 1746.6 | -0.8% |

The setup costs 0.8 to 2.0 G instructions (a new RSA key in every run), so a single run can be off by about 9 instructions per frame either way: the 2% bound sits at the noise floor of this method, and changes under about 2% are within the noise. The two columns' setup medians differ by 0.06 G, about 1 instruction per frame.

A session's hook table takes 112 bytes in the session and 72 bytes per hook on the heap, and the two built-in hooks 40 and 8 bytes (48 and 16 as allocated): about 320 bytes per player.

## Version translation as a cargo feature

The translator is behind the `translate` feature, on by default; `--no-default-features` leaves it out. Size of `target/release/pumboprox` from `cargo build --release --locked -p pumbo-prox`, aarch64-apple-darwin, Mac16,8, macOS 27, 2026-11-10.

| build | bytes | change |
|---|---|---|
| development build of 2026-11-08, before the feature | 33,667,920 | |
| development build of 2026-11-10, default features | 33,690,336 | +0.07% |
| development build of 2026-11-10, `--no-default-features` | 33,096,704 | -1.76% against the default build |

Without the feature the binary still holds the protocol tables of every version, which the proxy reads itself, and the one function of the translator crate that repairs a malformed particle from Pumpkin.
