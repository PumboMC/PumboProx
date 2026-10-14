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
//! Network steps: N1 N2 N3 N4 N5 N5b N6 N7 N8 N14 by default; N10 checks the
//! data after a restart (the script runs it after `down` and `up`), N11 the
//! same and the time PumboPerms took to open its database after the SIGKILL.
//! Solo steps: J1-J6; J8a and J8b are the two halves of J8 (data made by the
//! released 0.1.2-beta, then read by this build), the script swaps the file
//! between them. Logs are `<dir>/<proxy|lobby|survival>/console.log` and
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
        for _ in 0..secs * 4 {
            if self.log(who).contains(needle) {
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

async fn n1(l: &Live) -> Step {
    l.wait_log("proxy", "PumboPerms", 60).await?;
    l.wait_log("proxy", "loaded", 60).await?;
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
    let lines = ask(&mut admin, "prox plugins reload pumbo-perms").await?;
    if !lines.iter().any(|m| m.contains("pumbo-perms reloaded")) {
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
    // The host reads the file again; PumboPerms gets it at its next start.
    ask(&mut admin, "prox reload").await?;
    ask(&mut admin, "prox plugins reload pumbo-perms").await?;
    l.wait_log("proxy", "changed since it was imported", 30)
        .await?;
    let lines = ask(&mut admin, "pp import run file").await?;
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
    l.wait_log("proxy", "loaded:", 60).await?;
    let log = l.log("proxy");
    let ms: u64 = log
        .lines()
        .rev()
        .find_map(|line| {
            let (_, after) = line.split_once("database opened in ")?;
            after.split_whitespace().next()?.parse().ok()
        })
        .ok_or("the proxy log does not say how long opening the database took")?;
    println!("N11: PumboPerms opened its database in {ms} ms after the SIGKILL");
    if ms >= 1000 {
        return Err(format!("opening the database took {ms} ms"));
    }
    n10(l).await
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
    ask(&mut admin, "plugin load PumboPerms-26.3.wasm").await?;
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

// ------------------------------------------------------------------ main

fn solo_uuid(nick: &str) -> Option<Uuid> {
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, nick.as_bytes());
    Uuid::from_slice(digest.as_ref().get(..16)?).ok()
}

async fn run(l: &Live, step: &str) -> Step {
    match step {
        "N1" => n1(l).await,
        "N2" => n2(l).await,
        "N3" => n3(l).await,
        "N4" => n4(l).await,
        "N5" => n5(l).await,
        "N5b" => n5b(l).await,
        "N6" => n6(l).await,
        "N7" => n7(l).await,
        "N8" => n8(l).await,
        "N10" => n10(l).await,
        "N11" => n11(l).await,
        "N14" => n14(l).await,
        "J1" => j1(l).await,
        "J2" => j2(l).await,
        "J3" => j3(l).await,
        "J4" => j4(l).await,
        "J5" => j5(l).await,
        "J6" => j6(l).await,
        "J8a" => j8a(l).await,
        "J8b" => j8b(l).await,
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
        true => &["J1", "J2", "J3", "J4", "J5", "J6"],
        false => &["N1", "N2", "N3", "N4", "N5", "N5b", "N6", "N7", "N8", "N14"],
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
