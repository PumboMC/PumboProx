//! The live scenario of PumboPerms: bots join the test network through the
//! proxy, or a lone Pumpkin, and run the steps a client has to see.
//! `tools/perms-live.sh` in pumbo-plugins writes the data, starts, restarts
//! and kills the servers; this driver plays the players and reads the logs.
//!
//! ```text
//! perms_live --uuid <nick>        offline UUID behind the proxy (MD5, v3)
//! perms_live --solo-uuid <nick>   offline UUID on a lone Pumpkin (SHA-256)
//! perms_live network <proxy ip:port> <network dir> [step...]
//! perms_live solo <server ip:port> <server dir> [step...]
//! ```
//!
//! Network steps: N1 N2 N3 N4 N5 N5b N6 N7 N8 N9 N15 N14 by default; N10 checks the
//! data after a restart (the script runs it after `down` and `up`), N11 the
//! same and the time PumboPerms took to open its database after the SIGKILL.
//! N1p, N10p and N11p are their proxy parts, for servers without PumboBridge.
//! N12 (load, `PERMS_LIVE_N12_SECS`, 300) and N13 (long run with
//! `<network dir>/footprint.log`, `PERMS_LIVE_N13_SECS`, 3600) run only when
//! named. Their bots (`PpOp7x`, `PpPlain7x`, `PpPend7x`, `PpSlow7x`) must be
//! ops on survival: Pumpkin kicks anyone else who sends more than about one
//! command a second. N16 runs only when named, after the script gave the
//! proxy a new database with 400 000 names and `memory-mb: 64`.
//! `soak` is the long run of `tools/perms-soak.sh` (its variables are on
//! the step).
//! Solo steps: J1-J7; J8a and J8b are the two halves of J8 (data made by the
//! released 0.1.2-beta, then read by this build), the script swaps the file
//! between them. J10 runs only when named, on a lone Pumpkin the script gave
//! the same database and `max_memory_mb = 64` for PumboPerms.
//! Logs are `<dir>/<proxy|lobby|survival>/console.log` and
//! `<server dir>/console.log`. Each step prints `PASS <step>` or
//! `FAIL <step>: <why>`; any failure exits with 1.
//! `PUMBO_PROTOCOL` (777) is the protocol of the bots. Bans go through `/pb`:
//! on Pumpkin 0.2.0 the `/ban` shortcut of PumboBans keeps the vanilla
//! permission (`minecraft:command.ban`) in the command tree.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pumbo_data::DataVersion;
use pumbo_protocol::{PacketKind, Phase, ProtocolVersion, VersionModule};
use pumbo_testclient::player::saw;
use pumbo_testclient::{JoinOptions, Player};
use uuid::Uuid;

type Step = Result<(), String>;

const ADMIN: &str = "PpAdm7x";
const MODERATOR: &str = "PpMod7x";
const VICTIM: &str = "PpVic7x";
const OP: &str = "PpOp7x";
const VIP: &str = "PpVip7x";
const PENDING: &str = "PpPend7x";
const PLAIN: &str = "PpPlain7x";
const SLOW: &str = "PpSlow7x";
const STRANGER: &str = "PpNew7x";
/// Bots of N12 and N13: four, because the proxy lets five connections in
/// per address by default and the admin takes one.
const LOAD: [&str; 4] = [OP, PLAIN, PENDING, SLOW];
const LOGS: [&str; 3] = ["proxy", "lobby", "survival"];
/// Names in the database of N16 and J10, as `names_fixture` of
/// pumbo-perms-core writes them: `Player<i>` with the UUID
/// `00000000-0000-4000-8000-<i in 12 digits>`.
const NAMES: u32 = 400_000;

fn env_secs(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

struct Live {
    addr: SocketAddr,
    dir: PathBuf,
    module: Arc<dyn VersionModule>,
    /// A lone Pumpkin: its log is `<dir>/console.log`.
    solo: bool,
}

impl Live {
    async fn bot(&self, name: &str) -> Result<Player, String> {
        let join = || Player::join(self.addr, self.module.clone(), JoinOptions::new(name));
        match join().await {
            // The proxy asks Mojang about every name; a slow answer refuses the login.
            Err(e) if e.to_string().contains("Could not check your account") => {
                println!("RETRY {name}: the proxy could not check the account");
                join().await
            }
            other => other,
        }
        .map_err(|e| format!("{name} cannot join: {e}"))
    }

    /// A bot on `survival` (behind the proxy) or on the lone server.
    async fn on_survival(&self, name: &str) -> Result<Player, String> {
        let mut p = self.bot(name).await?;
        if !self.solo {
            to_server(&mut p, "survival").await?;
        }
        Ok(p)
    }

    fn log(&self, who: &str) -> String {
        let path = if self.solo {
            self.dir.join("console.log")
        } else {
            self.dir.join(who).join("console.log")
        };
        std::fs::read_to_string(path).unwrap_or_default()
    }

    async fn wait_log(&self, who: &str, needle: &str, secs: u64) -> Step {
        self.wait_log_after(who, 0, needle, secs).await
    }

    /// Like `wait_log`, in what the log got after its first `from` bytes.
    async fn wait_log_after(&self, who: &str, from: usize, needle: &str, secs: u64) -> Step {
        for _ in 0..secs * 4 {
            if self
                .log(who)
                .get(from..)
                .is_some_and(|new| new.contains(needle))
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err(format!("the {who} log has no \"{needle}\" after {secs} s"))
    }

    fn log_lacks(&self, who: &str, needle: &str) -> Step {
        match self.log(who).contains(needle) {
            true => Err(format!("the {who} log has \"{needle}\"")),
            false => Ok(()),
        }
    }

    /// Lengths of the network logs now, to read only what comes after.
    fn log_ends(&self) -> Vec<usize> {
        LOGS.iter().map(|w| self.log(w).len()).collect()
    }

    /// No trapped plugin call on the proxy and no failed Wasm call on the
    /// servers since `ends`. A lone Pumpkin has one log: give one end.
    fn no_plugin_errors(&self, ends: &[usize]) -> Step {
        for (who, from) in LOGS.iter().zip(ends) {
            let log = self.log(who);
            let new = log.get(*from..).unwrap_or(&log);
            let bad = |l: &&str| {
                l.contains("time budget exceeded") || (l.contains("Wasm") && l.contains("failed"))
            };
            if let Some(line) = new.lines().find(bad) {
                let who = if self.solo { "server" } else { *who };
                return Err(format!("the {who} log has: {line}"));
            }
        }
        Ok(())
    }

    fn pid(&self, who: &str) -> Result<String, String> {
        let path = self.dir.join(who).join("pid");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| s.split_whitespace().next().map(str::to_string))
            .ok_or_else(|| format!("no pid in {}", path.display()))
    }
}

async fn pump(p: &mut Player, ms: u64, until: impl FnMut(&Player) -> bool) -> Result<bool, String> {
    p.pump(Duration::from_millis(ms), until)
        .await
        .map_err(|e| format!("{}: {e}", p.opts.name))
}

/// Keeps every bot answering while time passes.
async fn idle(bots: &mut [&mut Player], secs: u64) -> Step {
    let until = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < until {
        for p in bots.iter_mut() {
            pump(p, 100, |_| false).await?;
        }
    }
    Ok(())
}

/// Sends a command and returns the system messages that answer it.
async fn ask(p: &mut Player, line: &str) -> Result<Vec<String>, String> {
    let before = p.messages.len();
    p.command(line.trim_start_matches('/'))
        .await
        .map_err(|e| format!("{}: /{line}: {e}", p.opts.name))?;
    pump(p, 5000, |p| p.messages.len() > before).await?;
    pump(p, 400, |_| false).await?;
    Ok(p.messages.get(before..).unwrap_or_default().to_vec())
}

/// Sends a command and waits up to `secs` for a system message with
/// `needle`; returns every message that came in meanwhile.
async fn ask_for(
    p: &mut Player,
    line: &str,
    needle: &str,
    secs: u64,
) -> Result<Vec<String>, String> {
    let before = p.messages.len();
    p.command(line.trim_start_matches('/'))
        .await
        .map_err(|e| format!("{}: /{line}: {e}", p.opts.name))?;
    let has = |p: &Player| {
        p.messages
            .get(before..)
            .is_some_and(|m| m.iter().any(|m| m.contains(needle)))
    };
    pump(p, secs * 1000, has).await?;
    Ok(p.messages.get(before..).unwrap_or_default().to_vec())
}

/// The server refused the command (unknown to this player, or no right).
fn denied(lines: &[String]) -> bool {
    lines.iter().any(|l| {
        let l = l.to_ascii_lowercase();
        ["unknown", "incomplete", "permission", "not allowed"]
            .iter()
            .any(|w| l.contains(w))
    })
}

async fn must_run(p: &mut Player, line: &str) -> Step {
    let lines = ask(p, line).await?;
    match denied(&lines) {
        true => Err(format!("{}: /{line} was refused: {lines:?}", p.opts.name)),
        false => Ok(()),
    }
}

async fn must_fail(p: &mut Player, line: &str) -> Step {
    let lines = ask(p, line).await?;
    match denied(&lines) {
        true => Ok(()),
        false => Err(format!(
            "{}: /{line} was not refused: {lines:?}",
            p.opts.name
        )),
    }
}

/// `/pp ... permission check <node>` says true.
async fn check_true(admin: &mut Player, user: &str, node: &str) -> Result<bool, String> {
    let lines = ask(admin, &format!("pp user {user} permission check {node}")).await?;
    // One message holds the answer and the lines that explain it.
    Ok(lines
        .iter()
        .flat_map(|m| m.lines())
        .any(|l| l.contains(node) && l.trim_end().ends_with("true")))
}

/// The root of the player's command tree has `name`.
fn tree_has(p: &Player, name: &str) -> bool {
    let Some(c) = &p.commands else {
        return false;
    };
    let node = |i: i32| usize::try_from(i).ok().and_then(|i| c.nodes.get(i));
    node(c.root).is_some_and(|root| {
        root.children
            .iter()
            .filter_map(|i| node(*i))
            .any(|n| n.name.as_deref() == Some(name))
    })
}

async fn to_server(p: &mut Player, server: &str) -> Step {
    let logins = p.logins.len();
    p.command(&format!("server {server}"))
        .await
        .map_err(|e| format!("{}: /server {server}: {e}", p.opts.name))?;
    match pump(p, 30_000, |p| p.logins.len() > logins).await? {
        true => pump(p, 1000, |_| false).await.map(|_| ()),
        false => Err(format!(
            "{} did not reach {server}: {:?}",
            p.opts.name, p.disconnect
        )),
    }
}

/// The victim of a ban left the server (kicked, or sent to another one).
async fn banned(victim: &mut Player) -> Result<bool, String> {
    let logins = victim.logins.len();
    pump(victim, 5000, |p| {
        p.disconnect.is_some() || p.logins.len() > logins
    })
    .await
}

// ------------------------------------------------------------------ network

/// The proxy part of N1, for a network without PumboBridge.
async fn n1p(l: &Live) -> Step {
    l.wait_log("proxy", "PumboPerms", 60).await?;
    l.wait_log("proxy", "loaded", 60).await
}

async fn n1(l: &Live) -> Step {
    n1p(l).await?;
    for s in ["lobby", "survival"] {
        l.wait_log(s, "stepped back", 60).await?;
        l.wait_log(s, "the bridge writes the permissions", 60)
            .await?;
    }
    Ok(())
}

async fn n2(l: &Live) -> Step {
    tokio::time::sleep(Duration::from_secs(40)).await;
    for s in ["lobby", "survival"] {
        l.log_lacks(s, "takes over the permissions here again")?;
    }
    l.log_lacks("proxy", "TooLarge")
}

async fn n3(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    // `/pp user <nick>` finds only players who joined once.
    let mut victim = l.on_survival(VICTIM).await?;
    must_run(&mut admin, "pp group mod permission set pumbo.bans.* true").await?;
    must_run(
        &mut admin,
        "pp group mod permission set pumbo.bans.exempt.* false",
    )
    .await?;
    must_run(&mut admin, &format!("pp user {VICTIM} parent add mod")).await?;
    let mut moderator = l.on_survival(MODERATOR).await?;
    pump(&mut victim, 500, |_| false).await?;
    let lines = ask(&mut moderator, &format!("pb ban {VICTIM} test")).await?;
    if denied(&lines) || !banned(&mut victim).await? {
        return Err(format!("the ban did not reach {VICTIM}: {lines:?}"));
    }
    must_run(&mut moderator, &format!("pb unban {VICTIM}")).await?;
    victim.close().await;
    moderator.close().await;
    admin.close().await;
    Ok(())
}

async fn n4(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    let mut moderator = l.on_survival(MODERATOR).await?;
    if !tree_has(&moderator, "tempban") {
        return Err(format!("{MODERATOR} has no /ban before the change"));
    }
    let trees = saw(&moderator, Phase::Play, PacketKind::Commands);
    must_run(
        &mut admin,
        &format!("pp user {MODERATOR} parent remove mod"),
    )
    .await?;
    let fresh = pump(&mut moderator, 2000, |p| {
        saw(p, Phase::Play, PacketKind::Commands) > trees && !tree_has(p, "tempban")
    })
    .await?;
    if !fresh {
        return Err(format!("{MODERATOR} still has /ban 2 s after the change"));
    }
    must_fail(&mut moderator, &format!("pb ban {VICTIM} test")).await?;
    moderator.close().await;
    admin.close().await;
    Ok(())
}

async fn n5(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    let mut moderator = l.on_survival(MODERATOR).await?;
    let done = "pumbo-perms reloaded";
    let lines = ask_for(&mut admin, "prox plugins reload pumbo-perms", done, 30).await?;
    if !lines.iter().any(|m| m.contains(done)) {
        return Err(format!("the reload was not confirmed: {lines:?}"));
    }
    // Online through the restart: the tree never offers /ban.
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < until {
        pump(&mut admin, 100, |_| false).await?;
        if pump(&mut moderator, 100, |p| tree_has(p, "tempban")).await? {
            return Err(format!("{MODERATOR} got /ban back during the reload"));
        }
    }
    must_fail(&mut moderator, &format!("pb ban {VICTIM} test")).await?;
    l.wait_log("proxy", "players online keep their permissions", 5)
        .await?;
    l.wait_log("proxy", "(/prox plugins reload pumbo-perms", 5)
        .await?;
    moderator.close().await;
    admin.close().await;
    Ok(())
}

async fn n5b(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    let mut moderator = l.on_survival(MODERATOR).await?;
    ask(&mut admin, "prox plugins unload pumbo-perms").await?;
    idle(&mut [&mut admin, &mut moderator], 40).await?;
    let result = async {
        for s in ["lobby", "survival"] {
            l.log_lacks(s, "takes over the permissions here again")?;
        }
        if tree_has(&moderator, "tempban") {
            return Err(format!("{MODERATOR} got /ban back with the provider down"));
        }
        must_fail(&mut moderator, &format!("pb ban {VICTIM} test")).await?;
        match l.bot(STRANGER).await {
            Ok(p) => {
                p.close().await;
                Err(format!("{STRANGER} joined with the provider down"))
            }
            Err(e) if e.contains("temporarily unavailable") => Ok(()),
            Err(e) => Err(format!("{STRANGER} refused for another reason: {e}")),
        }
    }
    .await;
    ask(&mut admin, "prox plugins load pumbo-perms").await?;
    moderator.close().await;
    admin.close().await;
    result
}

async fn n6(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    let mut vip = l.on_survival(VIP).await?;
    must_run(&mut admin, &format!("pp user {VIP} parent addtemp vip 15s")).await?;
    pump(&mut vip, 1000, |_| false).await?;
    must_run(&mut vip, "gamemode creative").await?;
    idle(&mut [&mut admin, &mut vip], 17).await?;
    must_fail(&mut vip, "gamemode creative").await?;
    if check_true(&mut admin, VIP, "group.vip").await? {
        return Err(format!("{VIP} is still in vip after 17 s"));
    }
    vip.close().await;
    admin.close().await;
    Ok(())
}

async fn n8(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    let pending = l.bot(PENDING).await?;
    if check_true(&mut admin, PENDING, "group.mod").await? {
        return Err(format!("{PENDING} (offline account) got mod"));
    }
    l.wait_log(
        "proxy",
        &format!("{PENDING} joined with an offline-mode account"),
        5,
    )
    .await?;
    pending.close().await;
    admin.close().await;
    Ok(())
}

/// Tab completion of `text`, as the client asks the proxy for it.
async fn suggest(p: &mut Player, text: &str) -> Result<Vec<String>, String> {
    let before = p.suggestions.len();
    let id = i32::try_from(before).unwrap_or(0) + 1;
    p.c.send(&pumbo_protocol::packets::play::CommandSuggestion {
        id,
        text: text.into(),
    })
    .await
    .map_err(|e| format!("{}: {text}: {e}", p.opts.name))?;
    if !pump(p, 5000, |p| p.suggestions.iter().any(|s| s.id == id)).await? {
        return Err(format!("no answer to {text}"));
    }
    let answer = p.suggestions.iter().rev().find(|s| s.id == id);
    Ok(answer
        .map(|s| s.matches.iter().map(|m| m.text.clone()).collect())
        .unwrap_or_default())
}

/// Tab completion of `/pp` on the proxy: subcommands for the first word,
/// only those the player may use, then names of online players, groups and
/// permission nodes.
async fn n9(l: &Live) -> Step {
    // 3 connections a second from one address, and N8 just joined two.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut admin = l.bot(ADMIN).await?;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let mut online = l.bot(VICTIM).await?;
    pump(&mut online, 500, |_| false).await?;
    let first = suggest(&mut admin, "/pp ").await?;
    if !["user", "group", "track"]
        .iter()
        .all(|w| first.iter().any(|s| s == w))
        || first.iter().any(|s| s == ADMIN || s == VICTIM)
    {
        return Err(format!("/pp <TAB> gave {first:?}"));
    }
    let users = suggest(&mut admin, "/pp user ").await?;
    if !users.iter().any(|s| s == VICTIM) {
        return Err(format!("/pp user <TAB> gave {users:?}"));
    }
    let groups = suggest(&mut admin, &format!("/pp user {VICTIM} parent add ")).await?;
    if !["mod", "vip"].iter().all(|g| groups.iter().any(|s| s == g)) {
        return Err(format!(
            "/pp user {VICTIM} parent add <TAB> gave {groups:?}"
        ));
    }
    let nodes = suggest(
        &mut admin,
        &format!("/pp user {VICTIM} permission set minecraft:command.game"),
    )
    .await?;
    if !nodes.iter().any(|s| s == "minecraft:command.gamemode") {
        return Err(format!(
            "/pp user {VICTIM} permission set <TAB> gave {nodes:?}"
        ));
    }
    let node = "pumbo.perms.user.info";
    must_run(
        &mut admin,
        &format!("pp user {VICTIM} permission set {node} true"),
    )
    .await?;
    let mut limited = Vec::new();
    for _ in 0..10 {
        limited = suggest(&mut online, "/pp ").await?;
        limited.sort();
        if limited == ["help", "user"] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    must_run(
        &mut admin,
        &format!("pp user {VICTIM} permission unset {node}"),
    )
    .await?;
    if limited != ["help", "user"] {
        return Err(format!("/pp <TAB> with only {node} gave {limited:?}"));
    }
    online.close().await;
    admin.close().await;
    Ok(())
}

/// `/tick freeze` on survival stops Pumpkin's plugin tasks, PumboBridge's
/// too; its handler of each server list ping answers the proxy instead. The
/// proxy keeps the bridge through 20 s of pings, and after `/tick unfreeze`
/// the bridge is still connected.
async fn n15(l: &Live) -> Step {
    let (proxy_from, survival_from) = (l.log("proxy").len(), l.log("survival").len());
    let mut admin = l.bot(ADMIN).await?;
    let mut op = l.on_survival(OP).await?;
    must_run(&mut op, "tick freeze").await?;
    // The proxy answers a ping to its own port itself; survival listens on the proxy's port + 2 (dev-network.sh).
    let survival = SocketAddr::new(l.addr.ip(), l.addr.port() + 2);
    let pinged = async {
        for _ in 0..20 {
            pumbo_testclient::status(survival, l.module.clone())
                .await
                .map_err(|e| format!("server list ping of survival: {e}"))?;
            idle(&mut [&mut admin, &mut op], 1).await?;
        }
        Ok::<(), String>(())
    }
    .await;
    ask(&mut op, "tick unfreeze").await?;
    pinged?;
    let new = |who: &str, from: usize| l.log(who).get(from..).unwrap_or_default().to_string();
    if !new("survival", survival_from).contains("ticks are frozen") {
        return Err("PumboBridge on survival did not see the frozen ticks".into());
    }
    if let Some(line) = new("proxy", proxy_from)
        .lines()
        .find(|l| l.contains("bridge on survival closed"))
    {
        return Err(format!("the proxy dropped the frozen bridge: {line}"));
    }
    l.wait_log_after("survival", survival_from, "the bridge works again", 10)
        .await?;
    let rows = ask(&mut admin, "prox bridge").await?;
    if !rows
        .iter()
        .flat_map(|m| m.lines())
        .any(|r| r.contains("survival") && r.contains("connected"))
    {
        return Err(format!("/prox bridge after /tick unfreeze: {rows:?}"));
    }
    op.close().await;
    admin.close().await;
    Ok(())
}

/// Re-import of a changed `permissions.yml` after an admin built a track,
/// an inheritance and a weight on its groups: all three stay.
async fn n7(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    let mut slowest = Duration::ZERO;
    for line in [
        "pp createtrack ranks",
        "pp track ranks append helper",
        "pp track ranks append builder",
        "pp track ranks append mod",
        "pp creategroup trial",
        "pp group trial parent add builder",
        "pp group builder setweight 20",
    ] {
        let started = tokio::time::Instant::now();
        must_run(&mut admin, line).await?;
        slowest = slowest.max(started.elapsed());
    }
    // `ask` waits 400 ms after the answer; the rest is the change and the round trip.
    let answer = slowest.saturating_sub(Duration::from_millis(400));
    println!(
        "N7: the slowest /pp change was answered in {} ms",
        answer.as_millis()
    );
    let file = l.dir.join("proxy/permissions.yml");
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let changed = text.replacen(
        "  builder: {}",
        "  builder: { permissions: [\"minecraft:command.list\"] }",
        1,
    );
    if changed == text {
        return Err("permissions.yml has no \"  builder: {}\" line".into());
    }
    std::fs::write(&file, changed).map_err(|e| e.to_string())?;
    // The host reads the file again and hands it to the running PumboPerms,
    // and to the restarted one at its start: each says the file changed.
    let from = l.log("proxy").len();
    ask(&mut admin, "prox reload").await?;
    l.wait_log_after("proxy", from, "changed since it was imported", 30)
        .await?;
    let from2 = l.log("proxy").len();
    ask_for(
        &mut admin,
        "prox plugins reload pumbo-perms",
        "pumbo-perms reloaded",
        30,
    )
    .await?;
    l.wait_log_after("proxy", from2, "changed since it was imported", 30)
        .await?;
    // The answer to a large import comes after the import finishes.
    let lines = ask_for(&mut admin, "pp import run file", "Imported file", 60).await?;
    if !lines.iter().any(|m| m.contains("Imported file")) {
        return Err(format!("the re-import did not run: {lines:?}"));
    }
    ranks_kept(&mut admin).await?;
    admin.close().await;
    Ok(())
}

/// The track, inheritance and weight of N7.
async fn ranks_kept(admin: &mut Player) -> Step {
    let text = |lines: Vec<String>| lines.join("\n");
    let track = text(ask(admin, "pp track ranks info").await?);
    if !["1. helper", "2. builder", "3. mod"]
        .iter()
        .all(|g| track.contains(g))
    {
        return Err(format!("the track ranks changed: {track}"));
    }
    let parents = text(ask(admin, "pp group trial parent info").await?);
    if !parents.contains("builder") {
        return Err(format!("trial no longer inherits builder: {parents}"));
    }
    let builder = text(ask(admin, "pp group builder info").await?);
    if !builder
        .lines()
        .any(|l| l.contains("Weight") && l.contains("20"))
    {
        return Err(format!("builder lost its weight: {builder}"));
    }
    Ok(())
}

/// After a SIGKILL: PumboPerms on the proxy opened its database (3000
/// players) in under a second, and the data is there (N10).
async fn n11(l: &Live) -> Step {
    opened_fast(l).await?;
    n10(l).await
}

/// N11 without PumboBridge: the time to open and the data of N10p.
async fn n11p(l: &Live) -> Step {
    opened_fast(l).await?;
    n10p(l).await
}

/// The last "database opened in <n> ms" of PumboPerms in a log.
fn opened_ms(log: &str) -> Option<u64> {
    log.lines().rev().find_map(|line| {
        let (_, after) = line.split_once("database opened in ")?;
        after.split_whitespace().next()?.parse().ok()
    })
}

async fn opened_fast(l: &Live) -> Step {
    l.wait_log("proxy", "loaded:", 60).await?;
    let ms = opened_ms(&l.log("proxy"))
        .ok_or("the proxy log does not say how long opening the database took")?;
    println!("N11: PumboPerms opened its database in {ms} ms after the SIGKILL");
    if ms >= 1000 {
        return Err(format!("opening the database took {ms} ms"));
    }
    Ok(())
}

/// The proxy part of N10, for a network without PumboBridge: the track,
/// inheritance and weight of N7.
async fn n10p(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    ranks_kept(&mut admin).await?;
    admin.close().await;
    Ok(())
}

/// After a restart or a SIGKILL: the changes of N3, N4 and N7 are still there.
async fn n10(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    let moderator = l.on_survival(MODERATOR).await?;
    if tree_has(&moderator, "tempban") || check_true(&mut admin, MODERATOR, "group.mod").await? {
        return Err(format!("{MODERATOR} is in mod again"));
    }
    if !check_true(&mut admin, VICTIM, "group.mod").await? {
        return Err(format!("{VICTIM} lost mod"));
    }
    ranks_kept(&mut admin).await?;
    moderator.close().await;
    admin.close().await;
    Ok(())
}

/// Load: the four bots send 100 commands a second each on survival (the 400
/// of eight bots at 50 in the plan) and the admin 20 `/pp` changes a second
/// in `default`. Nobody is disconnected, no plugin call traps or fails, and
/// the last change holds afterwards.
async fn n12(l: &Live) -> Step {
    let secs = env_secs("PERMS_LIVE_N12_SECS", 300);
    let ends = l.log_ends();
    // The proxy lets 3 connections a second in from one address, and the step before just joined some.
    let pace = || tokio::time::sleep(Duration::from_millis(400));
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut admin = l.bot(ADMIN).await?;
    let mut load = Vec::new();
    for name in LOAD {
        pace().await;
        load.push(l.on_survival(name).await?);
    }
    let node = "pumbo.load.node";
    let tick = Duration::from_millis(10);
    let started = tokio::time::Instant::now();
    let until = started + Duration::from_secs(secs);
    let mut next = started;
    let (mut ticks, mut commands, mut changes, mut answers) = (0u64, 0u64, 0u64, 0usize);
    while tokio::time::Instant::now() < until {
        for p in &mut load {
            p.command("gamemode survival")
                .await
                .map_err(|e| format!("{}: {e}", p.opts.name))?;
            commands += 1;
        }
        if ticks % 5 == 0 {
            let line = match changes % 2 {
                0 => format!("pp group default permission set {node} true"),
                _ => format!("pp group default permission unset {node}"),
            };
            admin
                .command(&line)
                .await
                .map_err(|e| format!("{ADMIN}: {e}"))?;
            changes += 1;
        }
        ticks += 1;
        // Behind schedule: drop the missed ticks instead of a burst.
        next = (next + tick).max(tokio::time::Instant::now());
        loop {
            for p in load.iter_mut().chain(std::iter::once(&mut admin)) {
                pump(p, 1, |_| false).await?;
            }
            if tokio::time::Instant::now() >= next {
                break;
            }
        }
        if let Some(p) = load.iter().chain([&admin]).find(|p| p.disconnect.is_some()) {
            let hint = match p.disconnect.as_deref() {
                Some(d) if d.contains("spam") => " (make it an op on survival)",
                _ => "",
            };
            return Err(format!(
                "{} was disconnected under load: {:?}{hint}",
                p.opts.name, p.disconnect
            ));
        }
        answers += admin.messages.len();
        for p in load.iter_mut().chain(std::iter::once(&mut admin)) {
            p.messages.clear();
            p.kinds.clear();
        }
    }
    let took = started.elapsed().as_secs_f64();
    println!(
        "N12: {commands} commands on survival ({:.0}/s), {changes} /pp changes ({:.1}/s), {answers} answers to the admin",
        commands as f64 / took,
        changes as f64 / took,
    );
    {
        let mut all: Vec<&mut Player> = load.iter_mut().collect();
        all.push(&mut admin);
        idle(&mut all, 3).await?;
    }
    if answers.saturating_mul(2) < usize::try_from(changes).unwrap_or(usize::MAX) {
        return Err(format!("the proxy answered {answers} of {changes} changes"));
    }
    ask(
        &mut admin,
        &format!("pp group default permission unset {node}"),
    )
    .await?;
    if check_true(&mut admin, PLAIN, node).await? {
        return Err(format!("{node} is still set after the last change"));
    }
    l.no_plugin_errors(&ends)?;
    for p in load {
        p.close().await;
    }
    admin.close().await;
    Ok(())
}

/// RSS of a process from `ps` and its footprint from `footprint` (macOS),
/// in KB.
fn memory(pid: &str) -> (Option<u64>, Option<u64>) {
    let run = |cmd: &str, args: &[&str]| {
        std::process::Command::new(cmd)
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let rss = run("ps", &["-o", "rss=", "-p", pid]).trim().parse().ok();
    // "Footprint: 58 MB (16384 bytes per page)"
    let footprint = run("footprint", &[pid]).lines().find_map(|l| {
        let (_, r) = l.split_once("Footprint: ")?;
        let mut w = r.split_whitespace();
        let n: f64 = w.next()?.parse().ok()?;
        let kb = match w.next()? {
            "B" => n / 1024.0,
            "KB" => n,
            "MB" => n * 1024.0,
            "GB" => n * 1024.0 * 1024.0,
            _ => return None,
        };
        Some(kb as u64)
    });
    (rss, footprint)
}

/// Long run: a bot joins survival and leaves every 10 s, the admin makes one
/// `/pp` change every 10 s (a new node in `default`, then its removal), and
/// the memory of the proxy and survival goes to `footprint.log` 60 times.
/// After the first sixth (the 10 min warm-up of an hour) the footprint of
/// the proxy grows by at most 5%: macOS compresses idle pages, so RSS
/// falls while the memory a process holds still grows.
async fn n13(l: &Live) -> Step {
    let secs = env_secs("PERMS_LIVE_N13_SECS", 3600);
    let ends = l.log_ends();
    let path = l.dir.join("footprint.log");
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let (proxy, survival) = (l.pid("proxy")?, l.pid("survival")?);
    let started = tokio::time::Instant::now();
    let length = Duration::from_secs(secs);
    let every = length / 60;
    let mut next_sample = started;
    // Proxy footprint after the warm-up and the last one; the same for survival.
    let mut held: [(Option<u64>, u64); 2] = [(None, 0); 2];
    let mut admin = l.bot(ADMIN).await?;
    let mut bot: Option<Player> = None;
    let mut cycle: u32 = 0;
    while started.elapsed() < length {
        if let Some(p) = bot.take() {
            p.close().await;
        }
        let name = LOAD.get(cycle as usize % LOAD.len()).copied().unwrap_or(OP);
        bot = Some(l.on_survival(name).await?);
        let line = match cycle % 2 {
            0 => format!(
                "pp group default permission set pumbo.soak.n{} true",
                cycle / 2
            ),
            _ => format!(
                "pp group default permission unset pumbo.soak.n{}",
                cycle / 2
            ),
        };
        must_run(&mut admin, &line).await?;
        cycle += 1;
        let cycle_end = started + Duration::from_secs(10) * cycle;
        while tokio::time::Instant::now() < cycle_end {
            pump(&mut admin, 100, |_| false).await?;
            if let Some(p) = bot.as_mut() {
                pump(p, 100, |_| false).await?;
            }
            if tokio::time::Instant::now() < next_sample {
                continue;
            }
            next_sample += every;
            let at = started.elapsed();
            let mut row = format!("{}", at.as_secs());
            for (i, (who, pid)) in [("proxy", &proxy), ("survival", &survival)]
                .into_iter()
                .enumerate()
            {
                let (rss, footprint) = memory(pid);
                let kb = |v: Option<u64>| v.map_or("?".into(), |k| k.to_string());
                row.push_str(&format!(
                    " {who} pid={pid} rss_kb={} footprint_kb={}",
                    kb(rss),
                    kb(footprint)
                ));
                // RSS where `footprint` is missing (Linux).
                if let (Some(kb), Some(slot)) = (footprint.or(rss), held.get_mut(i)) {
                    if at >= length / 6 {
                        slot.0.get_or_insert(kb);
                    }
                    slot.1 = kb;
                }
            }
            use std::io::Write as _;
            writeln!(out, "{row}").map_err(|e| format!("{}: {e}", path.display()))?;
        }
        for p in [Some(&admin), bot.as_ref()].into_iter().flatten() {
            if p.disconnect.is_some() {
                return Err(format!(
                    "{} was disconnected: {:?}",
                    p.opts.name, p.disconnect
                ));
            }
        }
        admin.messages.clear();
        admin.kinds.clear();
    }
    if cycle % 2 == 1 {
        must_run(
            &mut admin,
            &format!(
                "pp group default permission unset pumbo.soak.n{}",
                cycle / 2
            ),
        )
        .await?;
    }
    if let Some(p) = bot {
        p.close().await;
    }
    admin.close().await;
    l.no_plugin_errors(&ends)?;
    let growth = |(base, last): (Option<u64>, u64)| {
        base.filter(|b| *b > 0)
            .map(|b| (b, last, (last as f64 / b as f64 - 1.0) * 100.0))
    };
    let [p, s] = held;
    if let Some((b, last, pct)) = growth(s) {
        println!(
            "N13: survival footprint {b} KB after the warm-up, {last} KB at the end ({pct:+.1}%)"
        );
    }
    let Some((b, last, pct)) = growth(p) else {
        return Err(format!(
            "no proxy footprint after the warm-up in {}",
            path.display()
        ));
    };
    println!("N13: proxy footprint {b} KB after the warm-up, {last} KB at the end ({pct:+.1}%)");
    match pct <= 5.0 {
        true => Ok(()),
        false => Err(format!(
            "the proxy footprint grew by {pct:.1}% after the warm-up"
        )),
    }
}

/// The run of `tools/perms-soak.sh` in pumbo-plugins: a bot joins and leaves
/// every `PERMS_SOAK_JOIN_SECS` (5), the admin makes a `/pp` change every
/// `PERMS_SOAK_CHANGE_SECS` (10) and every `PERMS_SOAK_TEMP_SECS` (60) gives
/// itself a node that ends 30 s later, for `PERMS_SOAK_SECS` (14400). The
/// bots stay on lobby, so the script can restart survival meanwhile; a bot
/// that cannot join tries again at its next turn. The script measures memory.
async fn soak(l: &Live) -> Step {
    let length = Duration::from_secs(env_secs("PERMS_SOAK_SECS", 14_400));
    let join_every = Duration::from_secs(env_secs("PERMS_SOAK_JOIN_SECS", 5).max(1));
    let change_every = Duration::from_secs(env_secs("PERMS_SOAK_CHANGE_SECS", 10).max(1));
    let temp_every = Duration::from_secs(env_secs("PERMS_SOAK_TEMP_SECS", 60).max(31));
    let ends = l.log_ends();
    let mut admin = l.bot(ADMIN).await?;
    let started = tokio::time::Instant::now();
    let (mut next_join, mut next_change, mut next_temp) = (started, started, started);
    let mut bot: Option<Player> = None;
    let (mut joins, mut failed, mut changes, mut temps) = (0u32, 0u32, 0u32, 0u32);
    while started.elapsed() < length {
        let now = tokio::time::Instant::now();
        if now >= next_join {
            next_join += join_every;
            if let Some(p) = bot.take() {
                p.close().await;
            }
            let name = LOAD.get(joins as usize % LOAD.len()).copied().unwrap_or(OP);
            match l.bot(name).await {
                Ok(p) => bot = Some(p),
                Err(e) => {
                    failed += 1;
                    println!("RETRY {name}: {e}");
                }
            }
            joins += 1;
        }
        if now >= next_change {
            next_change += change_every;
            let line = match changes % 2 {
                0 => "pp group default permission set pumbo.soak.node true",
                _ => "pp group default permission unset pumbo.soak.node",
            };
            must_run(&mut admin, line).await?;
            changes += 1;
        }
        if now >= next_temp {
            next_temp += temp_every;
            must_run(
                &mut admin,
                &format!("pp user {ADMIN} permission settemp pumbo.soak.temp true 30s"),
            )
            .await?;
            temps += 1;
        }
        pump(&mut admin, 100, |_| false).await?;
        if let Some(p) = bot.as_mut() {
            pump(p, 100, |_| false).await?;
        }
        if let Some(d) = &admin.disconnect {
            return Err(format!("{ADMIN} was disconnected: {d}"));
        }
        admin.messages.clear();
        admin.kinds.clear();
    }
    if let Some(p) = bot {
        p.close().await;
    }
    admin.close().await;
    println!(
        "soak: {joins} joins ({failed} refused), {changes} /pp changes, {temps} temporary nodes in {} s",
        length.as_secs()
    );
    if failed.saturating_mul(10) > joins {
        return Err(format!("{failed} of {joins} joins were refused"));
    }
    l.no_plugin_errors(&ends)
}

async fn n14(l: &Live) -> Step {
    let mut slow = l.bot(SLOW).await?;
    let mut victim = l.on_survival(VICTIM).await?;
    // The set for survival waits 30 s in the bridge; the login takes 35 s.
    slow.hold_configuration(Duration::from_secs(35));
    let logins = slow.logins.len();
    slow.command("server survival")
        .await
        .map_err(|e| e.to_string())?;
    let mut joined = false;
    for _ in 0..60 {
        joined = pump(&mut slow, 1000, |p| p.logins.len() > logins).await?;
        pump(&mut victim, 50, |_| false).await?;
        if joined {
            break;
        }
    }
    if !joined {
        return Err(format!(
            "{SLOW} did not reach survival: {:?}",
            slow.disconnect
        ));
    }
    slow.hold_configuration(Duration::ZERO);
    pump(&mut slow, 1000, |_| false).await?;
    let lines = ask(&mut slow, &format!("pb ban {VICTIM} test")).await?;
    if denied(&lines) || !banned(&mut victim).await? {
        return Err(format!(
            "{SLOW} has no mod rank after the long login: {lines:?}"
        ));
    }
    must_run(&mut slow, &format!("pb unban {VICTIM}")).await?;
    victim.close().await;
    slow.close().await;
    Ok(())
}

/// Waits until PumboPerms opened its database (in the log of `who`) and
/// prints how long that took with the `NAMES` of N16 and J10.
async fn print_opened(l: &Live, step: &str, who: &str) -> Step {
    l.wait_log(who, "database opened in ", 60).await?;
    let ms = opened_ms(&l.log(who))
        .ok_or_else(|| format!("the {who} log does not say how long opening the database took"))?;
    println!("{step}: database with {NAMES} names opened in {ms} ms");
    Ok(())
}

/// `/pp user` finds the last of the `NAMES`, with its UUID.
async fn last_name_found(admin: &mut Player) -> Step {
    let last = NAMES - 1;
    let uuid = format!("00000000-0000-4000-8000-{last:012}");
    let line = format!("pp user Player{last} info");
    let lines = ask(admin, &line).await?;
    match lines.iter().any(|m| m.contains(&uuid)) {
        true => Ok(()),
        false => Err(format!("/{line} does not show {uuid}: {lines:?}")),
    }
}

/// After the script gave the proxy a new database with `NAMES` names and
/// `memory-mb: 64`, and started the network again with fresh logs. The new
/// database has no ledger of permissions.yml, so PumboPerms imports the file
/// again; the admin's `*` comes from it.
async fn n16(l: &Live) -> Step {
    print_opened(l, "N16", "proxy").await?;
    l.wait_log("proxy", "Imported file", 60).await?;
    let mut admin = l.bot(ADMIN).await?;
    last_name_found(&mut admin).await?;
    let (rss, footprint) = memory(&l.pid("proxy")?);
    let kb = |v: Option<u64>| v.map_or("?".into(), |k| k.to_string());
    println!(
        "N16: proxy memory {} KB RSS, {} KB footprint",
        kb(rss),
        kb(footprint)
    );
    admin.close().await;
    l.no_plugin_errors(&[0; LOGS.len()])
}

// ------------------------------------------------------------------ solo

async fn j1(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    let lines = ask(&mut admin, "pp info").await?;
    admin.close().await;
    match lines.iter().any(|x| x.contains("permission attachments")) {
        true => Ok(()),
        false => Err(format!("/pp info shows no attachment mode: {lines:?}")),
    }
}

async fn j2(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    // `/pp user <nick>` finds only players who joined once.
    let mut moderator = l.bot(MODERATOR).await?;
    for line in [
        "pp creategroup mod".to_string(),
        "pp group mod permission set pumbo.bans.* true".into(),
        format!("pp user {MODERATOR} parent add mod"),
        "pp creategroup vip".into(),
        "pp group vip permission set pumbo.bans.exempt.ban true".into(),
        "pp creategroup helper".into(),
        "pp group helper permission set pumbo.bans.limit.helper true".into(),
    ] {
        must_run(&mut admin, &line).await?;
    }
    pump(&mut moderator, 500, |_| false).await?;
    let mut op = l.bot(OP).await?;
    let lines = ask(&mut moderator, &format!("pb ban {OP} test")).await?;
    if denied(&lines) || !banned(&mut op).await? {
        return Err(format!(
            "the ban did not reach {OP} (op 3 outside vip): {lines:?}"
        ));
    }
    must_run(&mut moderator, &format!("pb unban {OP}")).await?;
    if check_true(&mut admin, OP, "pumbo.bans.limit.helper").await? {
        return Err(format!("{OP} has the limits of helper"));
    }
    moderator.close().await;
    admin.close().await;
    Ok(())
}

async fn j3(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    let mut moderator = l.bot(MODERATOR).await?;
    let node = "minecraft:command.gamemode";
    must_run(
        &mut admin,
        &format!("pp user {MODERATOR} permission set {node} true"),
    )
    .await?;
    pump(&mut moderator, 1000, |_| false).await?;
    must_run(&mut moderator, "gamemode creative").await?;
    let unset = format!("pp user {MODERATOR} permission unset {node}");
    let (_, sent) = tokio::join!(moderator.close(), admin.command(&unset));
    sent.map_err(|e| e.to_string())?;
    pump(&mut admin, 1000, |_| false).await?;
    let mut moderator = l.bot(MODERATOR).await?;
    must_fail(&mut moderator, "gamemode creative").await?;
    moderator.close().await;
    admin.close().await;
    Ok(())
}

async fn j4(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    let mut moderator = l.bot(MODERATOR).await?;
    let node = "minecraft:command.gamemode";
    must_run(
        &mut admin,
        &format!("pp user {MODERATOR} permission set {node} true"),
    )
    .await?;
    pump(&mut moderator, 1000, |_| false).await?;
    must_run(&mut moderator, "gamemode creative").await?;
    moderator.close().await;
    must_run(
        &mut admin,
        &format!("pp user {MODERATOR} permission unset {node}"),
    )
    .await?;
    ask(&mut admin, "plugin unload pumboperms").await?;
    // Pumpkin 0.2.0 loads a file named by one word from its working
    // directory; the script puts a copy there.
    let variant = match std::env::var("PUMBO_PROTOCOL").as_deref() {
        Ok("776") => "26.2",
        _ => "26.3",
    };
    ask(
        &mut admin,
        &format!("plugin load PumboPerms-{variant}.wasm"),
    )
    .await?;
    idle(&mut [&mut admin], 2).await?;
    let lines = ask(&mut admin, "pp info").await?;
    if !lines.iter().any(|l| l.contains("Platform")) {
        return Err(format!("PumboPerms did not load again: {lines:?}"));
    }
    let mut moderator = l.bot(MODERATOR).await?;
    must_fail(&mut moderator, "gamemode creative").await?;
    moderator.close().await;
    admin.close().await;
    Ok(())
}

async fn j5(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    let mut vip = l.bot(VIP).await?;
    ask(&mut admin, "pp creategroup vip").await?;
    must_run(
        &mut admin,
        "pp group vip permission set minecraft:command.gamemode true",
    )
    .await?;
    must_run(&mut admin, "tick freeze").await?;
    let result = async {
        must_run(&mut admin, &format!("pp user {VIP} parent addtemp vip 10s")).await?;
        pump(&mut vip, 1000, |_| false).await?;
        must_run(&mut vip, "gamemode creative").await?;
        idle(&mut [&mut admin, &mut vip], 12).await?;
        ask(&mut vip, "pp help").await?;
        must_fail(&mut vip, "gamemode creative").await
    }
    .await;
    ask(&mut admin, "tick unfreeze").await?;
    vip.close().await;
    admin.close().await;
    result
}

async fn j6(l: &Live) -> Step {
    let mut plain = l.bot(PLAIN).await?;
    pump(&mut plain, 2000, |_| false).await?;
    let (at, teleports) = (plain.position, plain.teleports);
    must_fail(&mut plain, &format!("execute run tp {PLAIN} 0 200 0")).await?;
    must_fail(&mut plain, "execute run banip 1.2.3.4").await?;
    pump(&mut plain, 1000, |_| false).await?;
    if plain.teleports != teleports || plain.position != at {
        return Err(format!("{PLAIN} moved: {at:?} -> {:?}", plain.position));
    }
    plain.close().await;
    Ok(())
}

/// Tab completion on Pumpkin: `/pp` is a command of its own (Pumpkin 0.2.0
/// suggests nothing right after an alias), its arguments have names, and a
/// complete command offers nothing more. A short form (`/pp u`) runs and
/// completes, and `/pp editor` gets PumboPerms' answer, not Pumpkin's error.
/// Before J4: a command registered again keeps Pumpkin's old argument nodes,
/// which ask the unloaded plugin.
async fn j7(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    let has = |got: &[String], want: &[&str]| want.iter().all(|w| got.iter().any(|s| s == w));
    let first = suggest(&mut admin, "/pp ").await?;
    if !has(&first, &["user", "group", "track"]) {
        return Err(format!("/pp <TAB> gave {first:?}"));
    }
    let user = suggest(&mut admin, &format!("/pp user {ADMIN} ")).await?;
    if !has(&user, &["info", "permission", "parent", "meta", "promote"]) {
        return Err(format!("/pp user {ADMIN} <TAB> gave {user:?}"));
    }
    let names = suggest(&mut admin, "/pp user PpAd").await?;
    if names != [ADMIN] {
        return Err(format!("/pp user PpAd<TAB> gave {names:?}"));
    }
    let done = suggest(&mut admin, "/pp track x create ").await?;
    if !done.is_empty() {
        return Err(format!("/pp track x create <TAB> gave {done:?}"));
    }
    // Other names of words go to PumboPerms with the rest of the line.
    let short = ask(&mut admin, &format!("pp u {ADMIN} info")).await?;
    if !short.iter().any(|m| m.contains("UUID")) {
        return Err(format!("/pp u {ADMIN} info gave {short:?}"));
    }
    let names = suggest(&mut admin, "/pp u PpAd").await?;
    if names != [ADMIN] {
        return Err(format!("/pp u PpAd<TAB> gave {names:?}"));
    }
    let editor = ask(&mut admin, "pp editor").await?;
    if !editor.iter().any(|m| m.contains("editor")) || denied(&editor) {
        return Err(format!("/pp editor gave {editor:?}"));
    }
    admin.close().await;
    Ok(())
}

/// J8, first half on the released 0.1.2-beta: groups with weight, prefix,
/// inheritance and a node, a track, and a player with a rank, a temporary
/// rank and meta.
async fn j8a(l: &Live) -> Step {
    let mut admin = l.bot(ADMIN).await?;
    // `/pp user <nick>` finds only players who joined once.
    let moderator = l.bot(MODERATOR).await?;
    for line in [
        "pp creategroup helper".to_string(),
        "pp creategroup mod".into(),
        "pp group mod setweight 20".into(),
        "pp group mod permission set pumbo.bans.* true".into(),
        "pp group mod parent add helper".into(),
        "pp group mod meta setprefix 50 &c[Mod]".into(),
        "pp createtrack staff".into(),
        "pp track staff append helper".into(),
        "pp track staff append mod".into(),
        format!("pp user {MODERATOR} parent add mod"),
        format!("pp user {MODERATOR} parent addtemp helper 30d"),
        format!("pp user {MODERATOR} meta set color red"),
    ] {
        must_run(&mut admin, &line).await?;
    }
    j8_same(&mut admin).await?;
    moderator.close().await;
    admin.close().await;
    Ok(())
}

/// J8, second half on this build with the same database: everything is there.
async fn j8b(l: &Live) -> Step {
    l.wait_log("", "schema 1 -> 2", 30).await?;
    let mut admin = l.bot(ADMIN).await?;
    j8_same(&mut admin).await?;
    admin.close().await;
    Ok(())
}

async fn j8_same(admin: &mut Player) -> Step {
    for node in ["group.mod", "group.helper", "pumbo.bans.ban"] {
        if !check_true(admin, MODERATOR, node).await? {
            return Err(format!("{MODERATOR} has no {node}"));
        }
    }
    let text = |lines: Vec<String>| lines.join("\n");
    let track = text(ask(admin, "pp track staff info").await?);
    if !(track.contains("1. helper") && track.contains("2. mod")) {
        return Err(format!("the track staff is not helper, mod: {track}"));
    }
    let group = text(ask(admin, "pp group mod info").await?);
    if !(group
        .lines()
        .any(|l| l.contains("Weight") && l.contains("20"))
        && group.contains("[Mod]"))
    {
        return Err(format!("mod lost its weight or prefix: {group}"));
    }
    let user = text(ask(admin, &format!("pp user {MODERATOR} meta info")).await?);
    if !(user.contains("color") && user.contains("red")) {
        return Err(format!("{MODERATOR} lost the meta color: {user}"));
    }
    Ok(())
}

/// A lone Pumpkin with a database of `NAMES` names and `max_memory_mb = 64`
/// for PumboPerms (the script writes both): the database opens, the last
/// name resolves and no Wasm call fails.
async fn j10(l: &Live) -> Step {
    print_opened(l, "J10", "server").await?;
    let mut admin = l.bot(ADMIN).await?;
    last_name_found(&mut admin).await?;
    admin.close().await;
    l.no_plugin_errors(&[0])
}

// ------------------------------------------------------------------ main

fn solo_uuid(nick: &str) -> Option<Uuid> {
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, nick.as_bytes());
    Uuid::from_slice(digest.as_ref().get(..16)?).ok()
}

async fn run(l: &Live, step: &str) -> Step {
    match step {
        "N1" => n1(l).await,
        "N1p" => n1p(l).await,
        "N2" => n2(l).await,
        "N3" => n3(l).await,
        "N4" => n4(l).await,
        "N5" => n5(l).await,
        "N5b" => n5b(l).await,
        "N6" => n6(l).await,
        "N7" => n7(l).await,
        "N8" => n8(l).await,
        "N9" => n9(l).await,
        "N10" => n10(l).await,
        "N10p" => n10p(l).await,
        "N11" => n11(l).await,
        "N11p" => n11p(l).await,
        "N12" => n12(l).await,
        "N13" => n13(l).await,
        "N14" => n14(l).await,
        "N15" => n15(l).await,
        "N16" => n16(l).await,
        "soak" => soak(l).await,
        "J1" => j1(l).await,
        "J2" => j2(l).await,
        "J3" => j3(l).await,
        "J4" => j4(l).await,
        "J5" => j5(l).await,
        "J6" => j6(l).await,
        "J7" => j7(l).await,
        "J8a" => j8a(l).await,
        "J8b" => j8b(l).await,
        "J10" => j10(l).await,
        other => Err(format!("unknown step {other}")),
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    match (arg(0), arg(1)) {
        (Some("--uuid"), Some(nick)) => {
            println!("{}", pumbo_identity::offline_uuid(nick).hyphenated());
            return std::process::ExitCode::SUCCESS;
        }
        (Some("--solo-uuid"), Some(nick)) => {
            if let Some(u) = solo_uuid(nick) {
                println!("{}", u.hyphenated());
            }
            return std::process::ExitCode::SUCCESS;
        }
        _ => {}
    }
    let (Some(kind @ ("network" | "solo")), Some(addr), Some(dir)) = (arg(0), arg(1), arg(2))
    else {
        eprintln!(
            "usage: perms_live --uuid <nick> | --solo-uuid <nick> | network <proxy ip:port> <network dir> [step...] | solo <server ip:port> <server dir> [step...]"
        );
        return std::process::ExitCode::from(2);
    };
    let Ok(addr) = addr.parse::<SocketAddr>() else {
        eprintln!("{addr} is not ip:port");
        return std::process::ExitCode::from(2);
    };
    let protocol = std::env::var("PUMBO_PROTOCOL")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(777);
    let Ok(tables) = pumbo_data::tables(ProtocolVersion(protocol)) else {
        eprintln!("no protocol data for {protocol}");
        return std::process::ExitCode::from(2);
    };
    let live = Live {
        addr,
        dir: PathBuf::from(dir),
        module: Arc::new(DataVersion::new(tables)),
        solo: kind == "solo",
    };
    let defaults: &[&str] = match live.solo {
        true => &["J1", "J2", "J3", "J7", "J4", "J5", "J6"],
        false => &[
            "N1", "N2", "N3", "N4", "N5", "N5b", "N6", "N7", "N8", "N9", "N15", "N14",
        ],
    };
    let steps: Vec<&str> = match args.get(3..) {
        Some(s) if !s.is_empty() => s.iter().map(String::as_str).collect(),
        _ => defaults.to_vec(),
    };
    let mut failed = false;
    for step in steps {
        match run(&live, step).await {
            Ok(()) => println!("PASS {step}"),
            Err(why) => {
                failed = true;
                println!("FAIL {step}: {why}");
            }
        }
    }
    match failed {
        true => std::process::ExitCode::FAILURE,
        false => std::process::ExitCode::SUCCESS,
    }
}
