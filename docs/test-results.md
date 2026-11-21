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

## Version tables on first use

The proxy used to parse the tables of every embedded version at start; now a version's tables and packet lookups are built the first time a player or a server uses it. Development builds of 2026-11-10, release, aarch64-apple-darwin, Mac16,8, macOS 27, on mains power, while another build and test run used the machine (load average about 5 to 6). Before and after ran in turns and each number is a median.

### Start and memory without players

`pumboprox run` with no servers, no plugins, bridge and managed servers off, on 127.0.0.1. Start: from the process start to the first accepted TCP connection, polled by a script, so only differences of more than about 30 ms would show; the first round of each build was cold (634 and 572 ms) and the other four took 78 to 134 ms and 78 to 106 ms, so this shows no difference between the builds. Memory: `phys_footprint` 5 s after the start, then after a status request of one version and after status requests of all eleven.

| build | start (median of 5) | without players | after one version | after eleven |
|---|---|---|---|---|
| tables at start | 106 ms | 20 MB | 20 MB | 20 MB |
| tables on first use | 104 ms | 4.0 MB | 6.1 MB | 20 MB |

### First use of a version

`first_use` in `crates/pumbo-prox/tests/first_use.rs` (ignored), each in a fresh process, median of three. Registering the eleven versions took 11.6 ms with the tables parsed at start and 0.06 ms without; the tables and packet lookups of one version take 1.0 to 1.2 ms on its first use. The translation tables of one older client for a 26.3 server take 22 to 36 ms in both builds; the proxy builds them on the blocking pool the first time a client of that version logs in after a start or `reload`.

### Forwarding one frame

`forward_one_frame` as in "Session hooks", three runs per kind and build, the setup median taken over both builds' ten setup runs (1.08 G instructions).

| frame | tables at start | tables on first use | change |
|---|---|---|---|
| entity move from the server | 810.9 | 817.6 | +0.8% |
| `set_time` from the server | 826.6 | 839.2 | +1.5% |
| `move_player_pos` from the client | 1342.6 | 1347.2 | +0.3% |

Every packet lookup now first checks that the version's lookups exist. Three runs per kind and build leave a thin margin: the runs of one build differ by up to 1.6% (entity move with the tables at start: 55.27 to 56.14 G instructions), and the ten setup runs took 0.81 to 1.77 G, about 7 instructions per frame either way. For `set_time` every run with the tables on first use is above every run with the tables at start, by 0.85 to 1.6%, so that cost is real and under the 2% bound. For the other two kinds the runs of the two builds overlap, so their difference is within the noise of this method.

## Engine adapters

What the proxy does differently for a server engine now comes from that engine's adapter: the defaults of a server's chat keys and the repair of Pumpkin's malformed trail particle, which runs as the adapter's packet filter on server connections without a translator. Release builds of 2026-11-10, aarch64-apple-darwin, Mac16,8, macOS 27, on mains power, while another build and test run used the machine (load average 3.5 to 9.3).

### Forwarding one frame

`forward_one_frame` as in "Session hooks", but 256 Mi frames per run, three runs per kind and build in turns, the setup median taken over both builds' ten setup runs (1.01 G instructions; the runs took 0.64 to 1.19 G, about 2 instructions per frame either way). Before is the commit that adds the `particles` kind (a flame particle, which the filter reads and leaves), after the one that takes the translator crate out of the build without default features.

| frame | before | after | change |
|---|---|---|---|
| entity move from the server, a kind the proxy does not read | 818.1 | 809.0 | -1.1% |
| `set_time` from the server, no filter or hook wants it | 837.4 | 842.1 | +0.6% |
| `player_position` from the server, the teleport gate | 2353.5 | 2357.7 | +0.2% |
| `level_particles` with a flame from the server, the filter reads it | 1014.3 | 1005.6 | -0.9% |
| `move_player_pos` from the client | 1352.9 | 1352.7 | 0.0% |

A first version that matched the step and the packet kind together cost `set_time` 2.3% (19 instructions per frame in every run); returning early for frames the proxy does not forward or does not know brought it to the numbers above. In `player_position` one run in three of each build came out about 100 instructions lower; the median leaves it out.

A server connection without a translator holds its filter and the kinds the filter reads: 32 bytes in the session and, for Pumpkin's filter, 8 on the heap, while the session no longer keeps the particle's id (8 bytes).

### Binary size, start and memory

Size of `target/release/pumboprox` from `cargo build --release --locked -p pumbo-prox`, the development build before the engine adapters and the one with them.

| build | before | after | change |
|---|---|---|---|
| default features | 33,767,008 | 33,789,984 | +0.07% |
| `--no-default-features` | 33,173,456 | 33,195,952 | +0.07% |

The build without default features no longer has the translator crate in its dependency tree (`tools/ci-local.sh` checks it). It linked only the particle repair from that crate, which is now in `pumbo-protocol`, so leaving the crate out saves nothing and both builds grow by the same new code.

Start and memory without players as in "Version tables on first use", five rounds of each build in turns: no difference. Start: median 124 ms before (runs 72 to 137 ms) and 102 ms after (runs 71 to 180 ms). The runs of both builds fall near 72 or near 125 ms, the steps of the polling, so the ranges overlap and the medians differ only by where the middle run fell. `phys_footprint` is 4.0 MB without players, 6.1 MB after a status request of one version and 20 MB after eleven in both builds.

## Translation tables per pair

The translator's tables are built once per pair of client and server protocol for the life of the process, every pair shares one copy of a version's data, and a `reload` keeps them; before, each provider kept its own tables with a copy of the 26.3 data per pair, and a `reload` started a new provider without them. Release builds of 2026-11-10, aarch64-apple-darwin, Mac16,8, macOS 27, on mains power, on a loaded machine (the load average is given with each measurement). Before is the commit that adds the `pair_tables` benchmark, after the one that keeps the tables per pair (first run) and the one that adds `translation.experimental-pairs` (second run, the numbers below unless said otherwise). The benchmarks were built with `PUMBO_DATA_FULL` pointing at the vanilla recordings of 2026-10-08 and ran from copies outside `target/`, before and after in turns, 11 runs each.

### First plan and memory of one provider

`pair_tables` in `crates/pumbo-translate/tests/pair_tables.rs` (ignored): one `multiversion` provider kept alive, the first plan of each older client on a 26.3 server in turn, with a counting allocator. The bytes are what the allocations ask for, without the allocator's overhead and without the peak while tables are built, so they are not the memory of the process. Load average 5.0 to 5.4.

| client | first plan before (median, ms) | after | bytes kept by the pair before | after |
|---|---|---|---|---|
| 767 (1.21) | 33.17 | 33.20 | 1,868,400 | 1,868,572 |
| 768 | 30.48 | 29.92 | 1,897,325 | 954,863 |
| 769 | 28.64 | 28.27 | 1,905,215 | 962,753 |
| 770 | 27.04 | 26.74 | 1,916,208 | 973,678 |
| 771 | 25.71 | 25.55 | 1,923,285 | 980,823 |
| 772 | 25.68 | 25.17 | 1,923,888 | 981,426 |
| 773 | 24.23 | 24.11 | 1,963,395 | 1,020,933 |
| 774 | 24.17 | 24.10 | 1,972,039 | 1,029,441 |
| 775 | 22.38 | 22.01 | 1,994,107 | 1,051,645 |
| 776 (26.2) | 20.23 | 20.01 | 2,032,758 | 1,090,296 |
| all ten | 261.73 | 259.08 | 19,396,620 | 10,914,430 |

The byte counts are the same in every run. These builds have the full registry data of the recordings built in (`PUMBO_DATA_FULL`), which release builds do not: the same benchmark on the after commit without it keeps 9,790,720 bytes for all ten pairs and 783,623 for the 26.3 version data. With the full data, the 26.3 version data takes 942,198 bytes; before, every pair held its own copy, now the first pair builds it and the others share it. All ten pairs, about 9.8 MB in a release build, is the most the tables ask for, however many players and reloads: they stay until the process ends, and a handshake before the login can already build a pair's tables.

### Translation cost per frame and per chunk

`translation_cost` in `crates/pumbo-translate/tests/bench.rs` (ignored), the recorded vanilla 26.3 session for clients 767, 770, 773 and 776. Load average 5.4. It prints `us/frame` with two decimals and `us/chunk` with one, so a rounding step is 4% of a 0.26 µs frame and of a 2.5 µs chunk. Frames are compared by the MB/s it also prints, which has three significant digits for the same bytes in every run.

| client | fastest frame run before (MB/s) | after | change in time | chunk before: min, median, mean (µs) | after | change of the mean |
|---|---|---|---|---|---|---|
| 767 | 803 | 810 | -0.9% | 2.6, 2.6, 2.636 | 2.6, 2.6, 2.618 | -0.7% |
| 770 | 950 | 967 | -1.8% | 2.6, 2.6, 2.609 | 2.6, 2.6, 2.609 | 0.0% |
| 773 | 958 | 954 | +0.4% | 2.5, 2.6, 2.573 | 2.6, 2.6, 2.600 | +1.1% |
| 776 | 930 | 947 | -1.8% | 1.8, 1.8, 1.827 | 1.8, 1.8, 1.845 | +1.0% |

Every chunk run printed one of two neighbouring values, so the minimum only says whether the lower one came up at all: for 773 it did in 3 of 11 runs before and in none after. The mean of the 11 rounded values shows smaller differences than one step, so the chunk cost is judged on the means and medians. In the first run (load average 4.6 to 4.8, after = the commit that keeps the tables per pair) the minima of 770 and 773 were 2.5 µs before and 2.6 µs after, while the medians were the same and the means differed by +1.4%, +2.1%, -0.3% and +1.5% for 767, 770, 773 and 776. Both runs stay within 3%. Frames run no slower; reading a version's data now goes through one more pointer, which these numbers do not show.

### Start, memory and size

Start and memory without players as in "Version tables on first use", but polled every 2 ms by the measuring script and with status requests a second apart. Load average 2.1 to 3.3.

- Start, 60 runs of each build in random order: median 58.6 ms before and 59.6 ms after (fastest 35.2 and 37.3 ms). In runs with a fixed order the second start of each pair of runs was slower whichever build it was (median 57.5 ms first, 65.7 ms second), so the order is random.
- `phys_footprint`, six runs of each build in turns: 4,040 and 4,064 KB without players (runs 4,000 to 4,208 and 4,032 to 4,128 KB), 6.1 MB after a status request of one version and 20 MB after eleven in both. A status request builds no translation tables; the handshake of a login does.
- Size of `target/release/pumboprox` with default features: 33,789,856 bytes before and 33,813,024 after (+23,168 bytes, +0.07%).
