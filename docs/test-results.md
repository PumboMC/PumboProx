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
