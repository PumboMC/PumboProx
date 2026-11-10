//! Load for a short benchmark (`tools/bench.sh`): offline bots join at a
//! fixed rate, stay for the hold time (idle, moving or chatting), then leave.
//! Each bot's join result and time go to a CSV; the summary is the `SUMMARY`
//! line, then up to five distinct `ERROR` lines.
//!
//! ```text
//! loadgen <addr> <protocol> <count> <hold secs> <idle|move|chat> <joins per sec> <out.csv> [name prefix]
//! ```
//!
//! Every bot comes from 127.0.0.1: the proxy needs per-address limits above
//! `count` (`limits.connections-per-ip-per-second`, `concurrent-per-ip`).

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use pumbo_data::DataVersion;
use pumbo_protocol::packets::world::{MOVE_ON_GROUND, MovePlayer, MovePlayerPos};
use pumbo_protocol::{ProtocolVersion, VersionModule};
use pumbo_testclient::{JoinOptions, Player};

const JOIN_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Idle,
    Move,
    Chat,
}

struct Args {
    addr: SocketAddr,
    protocol: i32,
    count: usize,
    hold: Duration,
    mode: Mode,
    rate: f64,
    out: String,
    prefix: String,
}

struct Row {
    name: String,
    ok: bool,
    join_ms: f64,
    err: String,
    stayed: bool,
    keep_alives: u32,
}

fn args() -> Result<Args, String> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: loadgen <addr> <protocol> <count> <hold secs> <idle|move|chat> <joins per sec> <out.csv> [name prefix]";
    let arg = |i: usize| a.get(i).ok_or_else(|| usage.to_string());
    let num = |i: usize| -> Result<f64, String> {
        arg(i)?.parse().map_err(|_| {
            format!(
                "{usage}\nnot a number: {}",
                a.get(i).map_or("", String::as_str)
            )
        })
    };
    Ok(Args {
        addr: arg(0)?.parse().map_err(|e| format!("address: {e}"))?,
        protocol: arg(1)?.parse().map_err(|e| format!("protocol: {e}"))?,
        count: arg(2)?.parse().map_err(|e| format!("count: {e}"))?,
        hold: Duration::from_secs_f64(num(3)?),
        mode: match arg(4)?.as_str() {
            "idle" => Mode::Idle,
            "move" => Mode::Move,
            "chat" => Mode::Chat,
            other => return Err(format!("{usage}\nunknown mode {other}")),
        },
        rate: num(5)?.max(0.001),
        out: arg(6)?.clone(),
        prefix: a.get(7).cloned().unwrap_or_else(|| "Bench".into()),
    })
}

/// The value at quantile `q` of sorted `v`.
fn quantile(v: &[f64], q: f64) -> f64 {
    let at = ((v.len().saturating_sub(1)) as f64 * q).round() as usize;
    v.get(at).copied().unwrap_or(f64::NAN)
}

async fn bot(
    addr: SocketAddr,
    module: Arc<dyn VersionModule>,
    name: String,
    mode: Mode,
    stop: Arc<AtomicBool>,
    joined: impl FnOnce(),
) -> Row {
    let start = Instant::now();
    let r = tokio::time::timeout(
        JOIN_TIMEOUT,
        Player::join(addr, module, JoinOptions::new(&name)),
    )
    .await;
    joined();
    let mut row = Row {
        name,
        ok: false,
        join_ms: start.elapsed().as_secs_f64() * 1000.0,
        err: String::new(),
        stayed: false,
        keep_alives: 0,
    };
    let mut p = match r {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            row.err = e.to_string();
            return row;
        }
        Err(_) => {
            row.err = format!("no join in {} s", JOIN_TIMEOUT.as_secs());
            return row;
        }
    };
    row.ok = true;
    let mut tick = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let r = p.pump(Duration::from_millis(500), |_| false).await;
        if let Err(e) = r {
            row.err = format!("connection: {e}");
            row.keep_alives = p.c.keep_alives;
            return row;
        }
        if let Some(reason) = &p.disconnect {
            row.err = reason.clone();
            row.keep_alives = p.c.keep_alives;
            return row;
        }
        // The client records everything it gets; a bot only needs the connection.
        p.c.log.clear();
        p.kinds.clear();
        p.chunks.clear();
        p.messages.clear();
        p.chats.clear();
        tick += 1;
        if mode == Mode::Move
            && tick.is_multiple_of(2)
            && let Some((x, y, z)) = p.position
        {
            let nx = if tick.is_multiple_of(4) {
                x + 0.2
            } else {
                x - 0.2
            };
            let pos = MovePlayerPos(MovePlayer {
                position: Some((nx, y, z)),
                rotation: None,
                flags: MOVE_ON_GROUND,
            });
            let _ = p.c.send(&pos).await;
            let _ = p.tick_end().await;
            p.position = Some((nx, y, z));
        }
        if mode == Mode::Chat && tick.is_multiple_of(10) {
            let _ = p.chat("bench hello", None).await;
        }
    }
    row.stayed = true;
    row.keep_alives = p.c.keep_alives;
    p.close().await;
    row
}

#[tokio::main]
async fn main() -> ExitCode {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let tables = match pumbo_data::tables(ProtocolVersion(a.protocol)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("protocol {}: {e}", a.protocol);
            return ExitCode::from(2);
        }
    };
    let module: Arc<dyn VersionModule> = Arc::new(DataVersion::new(tables));
    let stop = Arc::new(AtomicBool::new(false));
    let joined = Arc::new(AtomicUsize::new(0));
    let all_joined = Arc::new(tokio::sync::Notify::new());
    let t0 = Instant::now();
    let mut tasks = Vec::new();
    for i in 0..a.count {
        let (module, stop) = (module.clone(), stop.clone());
        let (joined, all_joined, count) = (joined.clone(), all_joined.clone(), a.count);
        let name = format!("{}{:04}", a.prefix, i + 1);
        let at = t0 + Duration::from_secs_f64(i as f64 / a.rate);
        let (addr, mode) = (a.addr, a.mode);
        tasks.push(tokio::spawn(async move {
            tokio::time::sleep_until(at.into()).await;
            // Joined or not, the hold starts once every bot has tried.
            bot(addr, module, name, mode, stop, move || {
                if joined.fetch_add(1, Ordering::Relaxed) + 1 == count {
                    all_joined.notify_one();
                }
            })
            .await
        }));
    }
    if a.count > 0 {
        all_joined.notified().await;
    }
    let ramp = t0.elapsed();
    println!(
        "HOLD {:.1} s after a ramp of {:.1} s",
        a.hold.as_secs_f64(),
        ramp.as_secs_f64()
    );
    tokio::time::sleep(a.hold).await;
    stop.store(true, Ordering::Relaxed);
    let mut rows = Vec::new();
    for t in tasks {
        match t.await {
            Ok(r) => rows.push(r),
            Err(e) => eprintln!("bot task: {e}"),
        }
    }
    let mut csv = String::from("name,ok,join_ms,stayed,keep_alives,err\n");
    for r in &rows {
        let err = r.err.replace('"', "'").replace('\n', " ");
        csv.push_str(&format!(
            "{},{},{:.1},{},{},\"{err}\"\n",
            r.name, r.ok, r.join_ms, r.stayed, r.keep_alives
        ));
    }
    if let Err(e) = std::fs::write(&a.out, csv) {
        eprintln!("{}: {e}", a.out);
        return ExitCode::FAILURE;
    }
    let mut times: Vec<f64> = rows.iter().filter(|r| r.ok).map(|r| r.join_ms).collect();
    times.sort_by(f64::total_cmp);
    let ok = times.len();
    let stayed = rows.iter().filter(|r| r.stayed).count();
    let (p50, p95, p99) = (
        quantile(&times, 0.5),
        quantile(&times, 0.95),
        quantile(&times, 0.99),
    );
    let max = times.last().copied().unwrap_or(f64::NAN);
    let ramp_s = ramp.as_secs_f64();
    println!(
        "SUMMARY count={} ok={ok} failed={} stayed={stayed} dropped={} ramp_s={ramp_s:.1} p50={p50:.1} p95={p95:.1} p99={p99:.1} max={max:.1}",
        a.count,
        a.count - ok,
        ok - stayed
    );
    let mut errors: Vec<&str> = rows
        .iter()
        .filter(|r| !r.err.is_empty())
        .map(|r| r.err.as_str())
        .collect();
    errors.sort_unstable();
    errors.dedup();
    for e in errors.iter().take(5) {
        println!("ERROR {e}");
    }
    ExitCode::SUCCESS
}
