//! E4 against real servers (plan E4, §7.2). Ignored by default: they start
//! Java servers and Pumpkin binaries and take minutes. Ports 25640–25649.
//!
//! - `vanilla_switching`: two vanilla servers of every protocol 767–777
//!   behind the proxy (`forwarding = none`): the merged command tree,
//!   `/server` there and back `PUMBO_ROUND_TRIPS` times (default 50),
//!   `PUMBO_RECONNECTS` reconnects to the same server (default 20), the same
//!   resource pack on both (not sent again), a shutdown of B2 (fallback to
//!   B1), B2 again with another pack (pushed, the old one popped), a killed
//!   B1 (fallback to B2). Jars from `PUMBO_JARS` (default
//!   `~/.cache/pumbo-datagen`), servers in `PUMBO_E4_WORK/vanilla`.
//! - `chat_ack_backends`: a signed chat message and a signed `/msg`
//!   cancelled by the proxy (replacement `chat_ack`), then 10 messages, on
//!   Pumpkin (`PUMBO_PUMPKIN_BIN`, `PUMBO_PUMPKIN_TEMPLATE`) and on Paper
//!   (`PUMBO_PAPER_JAR`) with `enforce-secure-profile` false and true, each
//!   with and without `chat-session-forwarding`. Paper trusts a services key
//!   made here (`-Dminecraft.api.services.host`, `openssl` signs the chat
//!   key), so signatures are really checked. Results are printed.
//!
//! `cargo test -p pumbo-prox --test backends_switching -- --ignored --nocapture --test-threads=1`
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{Pumpkin, module, pumpkin_config, secret_file, start_proxy, toml_path, wait_port};
use pumbo_datagen::record::{PLAYER, Server, prepare_dir};
use pumbo_protocol::packets::commands::NODE_LITERAL;
use pumbo_protocol::{Direction, Phase, ProtocolVersion};
use pumbo_prox::server::ChatInput;
use pumbo_testclient::session::FINAL_KICK;
use pumbo_testclient::{ChatKey, Ending, JoinOptions, Player, SessionOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const B1_PORT: u16 = 25640;
const B2_PORT: u16 = 25641;
const PROXY_PORT: u16 = 25642;
const BACKEND_PORT: u16 = 25643;
const CHAT_PROXY_PORT: u16 = 25644;
const SERVICES_PORT: u16 = 25649;
const WAIT: Duration = Duration::from_secs(40);

fn env_path(key: &str, default: &str) -> PathBuf {
    std::env::var_os(key).map_or_else(|| PathBuf::from(default), PathBuf::from)
}

fn env_num(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn work() -> PathBuf {
    env_path("PUMBO_E4_WORK", "target/pumbo-e4")
}

fn kill(pid: u32) {
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
}

async fn next_login(p: &mut Player, what: &str) -> Result<(), String> {
    let n = p.logins.len();
    if p.pump(WAIT, |p| p.logins.len() > n)
        .await
        .map_err(|e| e.to_string())?
    {
        Ok(())
    } else {
        Err(format!("{what}: no new login ({:?})", p.disconnect))
    }
}

// ---------------------------------------------------------------- vanilla

#[derive(Debug)]
struct SwitchRow {
    protocol: i32,
    release: String,
    switches: usize,
    reconnects: usize,
    merged: Vec<String>,
    tree_nodes: usize,
    pushes: usize,
    pops: usize,
    seconds: u64,
}

fn set_pack(dir: &Path, url: &str, sha1: &str) {
    let path = dir.join("server.properties");
    let props = std::fs::read_to_string(&path).unwrap();
    let props: String = props
        .lines()
        .map(|l| {
            if l.starts_with("resource-pack=") {
                format!("resource-pack={url}\n")
            } else if l.starts_with("resource-pack-sha1=") {
                format!("resource-pack-sha1={sha1}\n")
            } else {
                format!("{l}\n")
            }
        })
        .collect();
    std::fs::write(path, props).unwrap();
}

fn start_vanilla(dir: &Path, jar: &Path, port: u16) -> Result<Server, String> {
    let mut s = Server::start(dir, jar, "java").map_err(|e| e.to_string())?;
    eprintln!("vanilla pid {} on {port}", s.pid);
    s.wait_ready(
        SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(240),
    )
    .map_err(|e| e.to_string())?;
    Ok(s)
}

fn vanilla_switch_one(protocol: i32) -> Result<SwitchRow, String> {
    let tables = pumbo_data::tables(ProtocolVersion(protocol)).map_err(|e| e.to_string())?;
    let release = tables.releases.last().ok_or("no release")?.name.clone();
    let home = std::env::var("HOME").unwrap_or_default();
    let jar = env_path("PUMBO_JARS", &format!("{home}/.cache/pumbo-datagen"))
        .join(&release)
        .join("server.jar");
    let d1 = work().join("vanilla").join(format!("{release}-b1"));
    let d2 = work().join("vanilla").join(format!("{release}-b2"));
    prepare_dir(&d1, B1_PORT, protocol).map_err(|e| e.to_string())?;
    prepare_dir(&d2, B2_PORT, protocol).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let (r1, r2) = std::thread::scope(|s| {
        let a = s.spawn(|| start_vanilla(&d1, &jar, B1_PORT));
        let b = s.spawn(|| start_vanilla(&d2, &jar, B2_PORT));
        (a.join().unwrap(), b.join().unwrap())
    });
    let (b1, mut b2) = match (r1, r2) {
        (Ok(a), Ok(b)) => (Some(a), Some(b)),
        (a, b) => {
            for s in [a, b].into_iter().flatten() {
                s.stop();
            }
            return Err("a vanilla server did not start".into());
        }
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let round_trips = env_num("PUMBO_ROUND_TRIPS", 50);
    let reconnects = env_num("PUMBO_RECONNECTS", 20);
    // Boxed: the future is large in debug builds and the test thread's stack small.
    let result = rt.block_on(Box::pin(async {
        let op = pumbo_identity::offline_uuid(PLAYER);
        let cfg = format!(
            "listener:\n  - bind: \"127.0.0.1:{PROXY_PORT}\"\nlogin:\n  online-mode: false\nforwarding:\n  mode: none\n\
             servers:\n  b1: {{ address: \"127.0.0.1:{B1_PORT}\", protocol: {protocol} }}\n\
             \x20 b2: {{ address: \"127.0.0.1:{B2_PORT}\", protocol: {protocol} }}\n\
             routing:\n  try: [b1, b2]\nswitching:\n  reconnect-cooldown-ms: 0\n\
             commands:\n  operators: [{op}]\n"
        );
        let (proxy, addr) = start_proxy(&cfg).await;
        let r = async {
            let mut p = Player::join(addr, module(protocol), JoinOptions::new(PLAYER))
                .await
                .map_err(|e| format!("join: {e}"))?;
            // The vanilla tree with the proxy's commands, read back.
            p.pump(WAIT, |p| p.commands.is_some()).await.map_err(|e| e.to_string())?;
            let t = p.commands.clone().ok_or("no command tree")?;
            let roots: Vec<String> = t.nodes[t.root as usize]
                .children
                .iter()
                .map(|c| &t.nodes[*c as usize])
                .filter(|n| n.kind() == NODE_LITERAL)
                .filter_map(|n| n.name.clone())
                .collect();
            let merged: Vec<String> = ["server", "glist", "send", "find", "alert"]
                .iter()
                .filter(|n| roots.iter().any(|r| r == *n))
                .map(|n| n.to_string())
                .collect();
            if !roots.iter().any(|r| r == "gamemode") || merged.len() != 5 {
                return Err(format!("tree not merged: {roots:?}"));
            }
            let mut switches = 0;
            for i in 0..round_trips {
                for to in ["b2", "b1"] {
                    p.command(&format!("server {to}")).await.map_err(|e| e.to_string())?;
                    next_login(&mut p, &format!("round trip {i} to {to}")).await?;
                    switches += 1;
                }
            }
            let id = pumbo_identity::offline_uuid(PLAYER);
            for i in 0..reconnects {
                if !proxy.reconnect(id) {
                    return Err("reconnect not queued".into());
                }
                next_login(&mut p, &format!("reconnect {i}")).await?;
            }
            if p.packs_pushed.len() != 1 {
                return Err(format!("same pack sent {} times", p.packs_pushed.len()));
            }
            // B2 shuts down: the player lands on B1.
            p.command("server b2").await.map_err(|e| e.to_string())?;
            next_login(&mut p, "to b2 before stop").await?;
            let first_pack = p.packs_pushed[0];
            let s2 = b2.take().ok_or("b2")?;
            let stopper = std::thread::spawn(move || s2.stop());
            next_login(&mut p, "fallback after b2 stop").await?;
            stopper.join().map_err(|_| "stop thread")?;
            if !p.pump(WAIT, |p| p.messages.iter().any(|m| m.contains("You were moved to b1"))).await.map_err(|e| e.to_string())? {
                return Err(format!("no fallback notice: {:?}", p.messages));
            }
            // B2 again, with another pack.
            set_pack(&d2, "https\\://example.invalid/other.zip", "fedcba9876543210fedcba9876543210fedcba98");
            let d2c = d2.clone();
            let jarc = jar.clone();
            b2 = Some(tokio::task::spawn_blocking(move || start_vanilla(&d2c, &jarc, B2_PORT)).await.map_err(|e| e.to_string())??);
            p.command("server b2").await.map_err(|e| e.to_string())?;
            next_login(&mut p, "to the restarted b2").await?;
            if p.packs_pushed.len() != 2 || !p.packs_popped.contains(&Some(first_pack)) {
                return Err(format!("packs: pushed {:?}, popped {:?}", p.packs_pushed, p.packs_popped));
            }
            // B1 is killed while the player is on it: fallback to B2.
            p.command("server b1").await.map_err(|e| e.to_string())?;
            next_login(&mut p, "back to b1").await?;
            kill(b1.as_ref().ok_or("b1")?.pid);
            next_login(&mut p, "fallback after b1 kill").await?;
            let on = proxy.find_player(PLAYER).and_then(|e| e.server);
            if on.as_deref() != Some("b2") || p.disconnect.is_some() {
                return Err(format!("ended on {on:?}, {:?}", p.disconnect));
            }
            let (pushes, pops) = (p.packs_pushed.len(), p.packs_popped.len());
            p.close().await;
            // E3's play script on the remaining server (B1 is dead, so the
            // join falls through the try list): titles, boss bars, teams,
            // dialogs, chat and the final kick through the E4 relay.
            let out = Box::pin(pumbo_testclient::run(
                addr,
                module(protocol),
                &SessionOptions { name: PLAYER.into(), ..SessionOptions::default() },
            ))
            .await
            .map_err(|e| e.to_string())?;
            if out.ending != Ending::Disconnected(FINAL_KICK.into()) {
                return Err(format!("play script: {:?} {:?}", out.ending, out.notes));
            }
            let row = SwitchRow {
                protocol,
                release: release.clone(),
                switches,
                reconnects,
                merged,
                tree_nodes: t.nodes.len(),
                pushes,
                pops,
                seconds: started.elapsed().as_secs(),
            };
            Ok(row)
        }
        .await;
        proxy.stop();
        r
    }));
    for s in [b1, b2].into_iter().flatten() {
        s.stop();
    }
    result
}

#[test]
#[ignore = "starts two vanilla servers of every protocol"]
fn vanilla_switching() {
    let only: Vec<i32> = std::env::var("PUMBO_ONLY")
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let mut failed = Vec::new();
    for p in pumbo_data::protocols().map(|p| p.0) {
        if !only.is_empty() && !only.contains(&p) {
            continue;
        }
        // Debug builds of the client futures need more than the 2 MiB of a test thread.
        let run = std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(move || vanilla_switch_one(p))
            .unwrap()
            .join()
            .unwrap();
        match run {
            Ok(r) => eprintln!(
                "OK   {} ({}): {} switches, {} reconnects, tree {} nodes with {:?}, packs pushed {} popped {}, {} s",
                r.protocol,
                r.release,
                r.switches,
                r.reconnects,
                r.tree_nodes,
                r.merged,
                r.pushes,
                r.pops,
                r.seconds
            ),
            Err(e) => {
                eprintln!("FAIL {p}: {e}");
                failed.push(p);
            }
        }
    }
    assert!(failed.is_empty(), "failed: {failed:?}");
}

// ---------------------------------------------------------------- chat_ack

/// A services key made with `openssl` (SHA1withRSA, as the game expects).
struct Services {
    dir: PathBuf,
    public_b64: String,
}

impl Services {
    fn new(dir: &Path) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let key = dir.join("services.pem");
        let der = dir.join("services.der");
        let run = |args: &[&str]| {
            let out = Command::new("openssl").args(args).output().unwrap();
            assert!(out.status.success(), "openssl {args:?}");
            out.stdout
        };
        run(&["genrsa", "-out", key.to_str().unwrap(), "2048"]);
        run(&[
            "rsa",
            "-in",
            key.to_str().unwrap(),
            "-pubout",
            "-outform",
            "DER",
            "-out",
            der.to_str().unwrap(),
        ]);
        let b64 = run(&["base64", "-A", "-in", der.to_str().unwrap()]);
        Self {
            dir: dir.to_path_buf(),
            public_b64: String::from_utf8(b64).unwrap().trim().to_string(),
        }
    }

    fn sign(&self, payload: &[u8]) -> Vec<u8> {
        let input = self.dir.join("payload.bin");
        let sig = self.dir.join("payload.sig");
        std::fs::write(&input, payload).unwrap();
        let ok = Command::new("openssl")
            .args(["dgst", "-sha1", "-sign"])
            .arg(self.dir.join("services.pem"))
            .arg("-out")
            .arg(&sig)
            .arg(&input)
            .status()
            .unwrap();
        assert!(ok.success());
        std::fs::read(sig).unwrap()
    }

    /// On 127.0.0.1:SERVICES_PORT: `GET /publickeys`, 404 for the other
    /// service paths, and the discovery document (authlib 10, 26.x) pointing
    /// every service here for anything else.
    async fn serve(&self) -> tokio::task::JoinHandle<()> {
        let listener = TcpListener::bind(("127.0.0.1", SERVICES_PORT))
            .await
            .unwrap();
        let k = &self.public_b64;
        let keys = format!(
            r#"{{"profilePropertyKeys":[{{"publicKey":"{k}"}}],"playerCertificateKeys":[{{"publicKey":"{k}"}}],"authenticationKeys":[{{"publicKey":"{k}"}}]}}"#
        );
        let base = format!("http://127.0.0.1:{SERVICES_PORT}");
        let ep = |name: &str, path: &str| format!(r#""{name}":{{"uri":"{base}{path}"}}"#);
        let discovery = format!(
            r#"{{"environment":"pumbo-test","product":"minecraft","discovery":{{"authentication":{{"endpoints":{{{},{}}}}},"session":{{"endpoints":{{{},{},{}}}}},"player":{{"endpoints":{{{},{},{},{},{},{},{},{}}}}},"profiles":{{"endpoints":{{{},{},"getTexture":{{"validUris":["http://textures.minecraft.net/texture/{{textureId}}"]}}}}}},"telemetry":{{"endpoints":{{{}}}}}}}}}"#,
            ep("getPublicKeys", "/publickeys"),
            ep("loginXbox", "/x/login"),
            ep("getProfileById", "/x/profile/{profileId}"),
            ep("verify", "/x/hasJoined"),
            ep("join", "/x/join"),
            ep("updatePresence", "/x/presence"),
            ep("sendReport", "/x/report"),
            ep("getAttributes", "/x/attributes"),
            ep("getFriends", "/x/friends"),
            ep("getCertificates", "/x/certificates"),
            ep("updateAttributes", "/x/attributes"),
            ep("updateFriends", "/x/friends"),
            ep("getBlocklist", "/x/blocklist"),
            ep("getManyByName", "/x/profiles"),
            ep("getByName", "/x/profiles/{name}"),
            ep("sendEvents", "/x/events"),
        );
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else {
                    return;
                };
                let (keys, discovery) = (keys.clone(), discovery.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).to_string();
                    eprintln!("services: {}", head.lines().next().unwrap_or(""));
                    let (status, body) = if head.starts_with("GET /publickeys") {
                        ("200 OK", keys)
                    } else if head.contains(" /x/") {
                        ("404 Not Found", String::new())
                    } else {
                        ("200 OK", discovery)
                    };
                    let reply = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.write_all(reply.as_bytes()).await;
                });
            }
        })
    }
}

/// A Paper server, stopped by its own PID only.
struct Paper {
    child: Child,
    pid: u32,
}

/// A panicking test must not leave the server running (own PID only).
impl Drop for Paper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Paper {
    fn start(dir: &Path, jar: &Path) -> Self {
        let log = std::fs::File::create(dir.join("server-output.log")).unwrap();
        let services = format!("http://127.0.0.1:{SERVICES_PORT}");
        let child = Command::new("java")
            .current_dir(dir)
            .args(["-Xms256M", "-Xmx1G"])
            .args(
                [
                    "auth",
                    "account",
                    "session",
                    "services",
                    "profiles",
                    "discovery",
                ]
                .map(|h| format!("-Dminecraft.api.{h}.host={services}")),
            )
            .arg("-jar")
            .arg(jar)
            .arg("nogui")
            .stdin(Stdio::piped())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let pid = child.id();
        eprintln!("paper pid {pid}");
        Self { child, pid }
    }

    fn log(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("server-output.log")).unwrap_or_default()
    }

    fn stop(mut self) {
        use std::io::Write as _;
        let _ = self.child.stdin.as_mut().map(|s| writeln!(s, "stop"));
        let _ = self.child.stdin.as_mut().map(std::io::Write::flush);
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(60) {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        eprintln!("paper pid {} did not stop, killing it", self.pid);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A Paper directory with Velocity forwarding; the first run writes the
/// default configs, which are then patched.
fn prepare_paper(dir: &Path, jar: &Path, secret: &str, enforce: bool) {
    std::fs::create_dir_all(dir.join("plugins/bStats")).unwrap();
    std::fs::write(dir.join("eula.txt"), "eula=true\n").unwrap();
    // No statistics to bstats.org.
    std::fs::write(dir.join("plugins/bStats/config.yml"), "enabled: false\n").unwrap();
    std::fs::write(
        dir.join("server.properties"),
        format!(
            "server-ip=127.0.0.1\nserver-port={BACKEND_PORT}\nonline-mode=false\nenforce-secure-profile={enforce}\n\
             level-type=minecraft\\:flat\ngenerate-structures=false\nview-distance=3\nsimulation-distance=3\n\
             spawn-protection=0\nmax-players=10\nwhite-list=false\nenforce-whitelist=false\nsync-chunk-writes=false\nenable-query=false\nenable-rcon=false\n\
             pause-when-empty-seconds=-1\n"
        ),
    )
    .unwrap();
    let global = dir.join("config/paper-global.yml");
    if !global.exists() {
        let first = Paper::start(dir, jar);
        let addr = SocketAddr::from(([127, 0, 0, 1], BACKEND_PORT));
        let t = Instant::now();
        while std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err() {
            assert!(
                t.elapsed() < Duration::from_secs(400),
                "paper first start: {}",
                Paper::log(dir)
            );
            std::thread::sleep(Duration::from_millis(500));
        }
        first.stop();
    }
    let yml = std::fs::read_to_string(&global).unwrap();
    let start = yml.find("  velocity:\n").expect("velocity section");
    let end = start + yml[start..].find("    secret:").unwrap();
    let line_end = end + yml[end..].find('\n').unwrap();
    let patched = format!(
        "{}  velocity:\n    enabled: true\n    online-mode: true\n    secret: '{secret}'{}",
        &yml[..start],
        &yml[line_end..]
    );
    std::fs::write(&global, patched).unwrap();
}

/// Printed with `Debug`.
#[allow(dead_code)]
#[derive(Debug)]
struct ChatRow {
    backend: String,
    sessions: bool,
    enforces: bool,
    signed_seen: u32,
    control: bool,
    delivered: usize,
    msg_after: bool,
    kicked: Option<String>,
    sender_messages: Vec<String>,
}

/// One scenario: two players, a session for the sender, a cancelled message
/// and `/msg`, then 10 messages and one `/msg`.
async fn chat_scenario(
    backend: &str,
    protocol: i32,
    sessions: bool,
    secret: &str,
    services: Option<&Services>,
) -> ChatRow {
    let secret_path = secret_file(&format!("e4-chat-{}", rand_tag()), secret);
    let cfg = format!(
        "listener:\n  - bind: \"127.0.0.1:{CHAT_PROXY_PORT}\"\nlogin:\n  online-mode: false\n\
         forwarding:\n  mode: modern\n  secret-file: \"{}\"\n\
         servers:\n  lobby: {{ address: \"127.0.0.1:{BACKEND_PORT}\", protocol: {protocol}, chat-session-forwarding: {sessions} }}\n\
         routing:\n  try: [lobby]\n",
        toml_path(&secret_path)
    );
    let (proxy, addr) = start_proxy(&cfg).await;
    proxy.set_chat_filter(Some(Arc::new(
        |_p: &pumbo_core::profile::GameProfile, input: ChatInput<'_>| match input {
            ChatInput::Message(m) => m.contains("secret"),
            ChatInput::Command(c) => c.contains("secret"),
        },
    )));
    let chat_id = pumbo_data::tables(ProtocolVersion(protocol))
        .unwrap()
        .packet_id(Phase::Play, Direction::Clientbound, "player_chat");
    let opts = |name: &str| {
        let mut o = JoinOptions::new(name);
        o.player_chat_id = chat_id;
        o
    };
    let mut sender = Player::join(addr, module(protocol), opts("PumboSend"))
        .await
        .unwrap();
    let mut observer = Player::join(addr, module(protocol), opts("PumboSee"))
        .await
        .unwrap();
    let enforces = sender.logins[0].enforces_secure_chat;
    let key = ChatKey::generate().unwrap();
    let expires = i64::try_from(
        (SystemTime::now() + Duration::from_secs(86_400))
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let id = pumbo_identity::offline_uuid("PumboSend");
    let signature = match services {
        Some(s) => s.sign(&key.signed_payload(id, expires)),
        None => vec![0; 256],
    };
    sender
        .start_chat_session(key, expires, signature)
        .await
        .unwrap();
    settle(&mut sender, 1500).await;
    settle(&mut observer, 100).await;
    let seen = |o: &Player, text: &str| {
        o.c.log.iter().any(|f| {
            f.direction == Direction::Clientbound
                && f.phase == Phase::Play
                && f.payload.windows(text.len()).any(|w| w == text.as_bytes())
        })
    };
    // Control: a signed message before anything is cancelled.
    sender.chat("control message", None).await.unwrap();
    settle(&mut sender, 800).await;
    settle(&mut observer, 800).await;
    let control = seen(&observer, "control message");
    sender.chat("a secret", None).await.unwrap();
    settle(&mut sender, 300).await;
    sender
        .signed_command(
            "msg PumboSee secret two",
            &[("message", "secret two")],
            None,
        )
        .await
        .unwrap();
    settle(&mut sender, 300).await;
    for i in 0..10 {
        sender.chat(&format!("after {i}"), None).await.unwrap();
        settle(&mut sender, 400).await;
        settle(&mut observer, 50).await;
    }
    sender
        .signed_command("msg PumboSee after msg", &[("message", "after msg")], None)
        .await
        .unwrap();
    settle(&mut sender, 1000).await;
    settle(&mut observer, 1500).await;
    let delivered = (0..10)
        .filter(|i| seen(&observer, &format!("after {i}")))
        .count();
    let row = ChatRow {
        backend: backend.into(),
        sessions,
        enforces,
        signed_seen: observer.signed_chats,
        control,
        delivered,
        msg_after: seen(&observer, "after msg"),
        kicked: sender.disconnect.clone(),
        sender_messages: sender.messages.clone(),
    };
    sender.close().await;
    observer.close().await;
    proxy.stop();
    tokio::time::sleep(Duration::from_millis(500)).await;
    row
}

async fn settle(p: &mut Player, ms: u64) {
    let _ = p.pump(Duration::from_millis(ms), |_| false).await;
}

fn rand_tag() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[test]
#[ignore = "starts Pumpkin and Paper"]
fn chat_ack_backends() {
    let secret = "pumbo-e4-chat-secret-0123456789abcdef";
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut rows = Vec::new();
    let backend = SocketAddr::from(([127, 0, 0, 1], BACKEND_PORT));
    // Pumpkin.
    if let Some(bin) = std::env::var_os("PUMBO_PUMPKIN_BIN").map(PathBuf::from) {
        let template =
            std::fs::read_to_string(env_path("PUMBO_PUMPKIN_TEMPLATE", "pumpkin.toml.template"))
                .unwrap();
        let protocol = env_num("PUMBO_PUMPKIN_PROTOCOL", 777) as i32;
        let dir = work().join(format!("pumpkin-chat-{}", rand_tag()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = pumpkin_config(&template, BACKEND_PORT, secret).replace(
            "[chat.anti_spam]\nenabled = true",
            "[chat.anti_spam]\nenabled = false",
        );
        std::fs::write(dir.join("pumpkin.toml"), cfg).unwrap();
        let pumpkin = Pumpkin::start(&bin, &dir);
        wait_port(backend);
        rows.push(rt.block_on(Box::pin(chat_scenario(
            "Pumpkin 0.2.0",
            protocol,
            true,
            secret,
            None,
        ))));
        let log = pumpkin.log();
        pumpkin.stop();
        for l in log
            .lines()
            .filter(|l| l.contains("chat") || l.contains("PumboSend"))
            .take(30)
        {
            eprintln!("pumpkin: {l}");
        }
    }
    // Paper with enforce-secure-profile false and true.
    if let Some(jar) = std::env::var_os("PUMBO_PAPER_JAR").map(PathBuf::from) {
        let jar = std::fs::canonicalize(jar).unwrap();
        let protocol = env_num("PUMBO_PAPER_PROTOCOL", 777) as i32;
        let services = Services::new(&work().join("services"));
        let _services_task = rt.block_on(services.serve());
        for enforce in [false, true] {
            let dir = work().join(format!("paper-{enforce}"));
            prepare_paper(&dir, &jar, secret, enforce);
            let paper = Paper::start(&dir, &jar);
            let t = Instant::now();
            while std::net::TcpStream::connect_timeout(&backend, Duration::from_millis(300))
                .is_err()
            {
                assert!(
                    t.elapsed() < Duration::from_secs(300),
                    "paper: {}",
                    Paper::log(&dir)
                );
                std::thread::sleep(Duration::from_millis(500));
            }
            for sessions in [true, false] {
                rows.push(rt.block_on(Box::pin(chat_scenario(
                    &format!("Paper 26.3 enforce-secure-profile={enforce}"),
                    protocol,
                    sessions,
                    secret,
                    Some(&services),
                ))));
            }
            paper.stop();
            for l in Paper::log(&dir)
                .lines()
                .filter(|l| l.contains("PumboSend") || l.contains("chat") || l.contains("Chat"))
                .take(40)
            {
                eprintln!("paper({enforce}): {l}");
            }
        }
    }
    for r in &rows {
        eprintln!("{r:?}");
    }
    assert!(
        !rows.is_empty(),
        "set PUMBO_PUMPKIN_BIN and/or PUMBO_PAPER_JAR"
    );
}

// ---------------------------------------------------------------- Pumpkin

/// Two Pumpkin servers (`PUMBO_PUMPKIN_BIN`, template `PUMBO_PUMPKIN_TEMPLATE`)
/// with Velocity forwarding: `/server` there and back, reconnects, and a
/// stopped server with fallback (Pumpkin's "Server stopped" kick).
#[test]
#[ignore = "starts two Pumpkin binaries"]
fn pumpkin_switching() {
    let bin = env_path("PUMBO_PUMPKIN_BIN", "pumpkin");
    let template =
        std::fs::read_to_string(env_path("PUMBO_PUMPKIN_TEMPLATE", "pumpkin.toml.template"))
            .unwrap();
    let protocol = env_num("PUMBO_PUMPKIN_PROTOCOL", 777) as i32;
    let round_trips = env_num("PUMBO_ROUND_TRIPS", 20);
    let reconnects = env_num("PUMBO_RECONNECTS", 5);
    let secret = "pumbo-e4-pumpkin-switch-secret-0123";
    let start = |port: u16| {
        let dir = work().join(format!("pumpkin-switch-{port}-{}", rand_tag()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pumpkin.toml"),
            pumpkin_config(&template, port, secret),
        )
        .unwrap();
        let p = Pumpkin::start(&bin, &dir);
        wait_port(SocketAddr::from(([127, 0, 0, 1], port)));
        p
    };
    let b1 = start(BACKEND_PORT);
    let b2 = start(B1_PORT);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut b2 = Some(b2);
    let report = rt.block_on(Box::pin(async {
        let secret_path = secret_file(&format!("e4-pumpkin-{}", rand_tag()), secret);
        let cfg = format!(
            "listener:\n  - bind: \"127.0.0.1:{CHAT_PROXY_PORT}\"\nlogin:\n  online-mode: false\n\
             forwarding:\n  mode: modern\n  secret-file: \"{}\"\n\
             servers:\n  b1: {{ address: \"127.0.0.1:{BACKEND_PORT}\", protocol: {protocol} }}\n\
             \x20 b2: {{ address: \"127.0.0.1:{B1_PORT}\", protocol: {protocol} }}\n\
             routing:\n  try: [b1, b2]\nswitching:\n  reconnect-cooldown-ms: 0\n",
            toml_path(&secret_path)
        );
        let (proxy, addr) = start_proxy(&cfg).await;
        let r = async {
            let mut p = Player::join(addr, module(protocol), JoinOptions::new("PumboHop"))
                .await
                .map_err(|e| e.to_string())?;
            let t = Instant::now();
            for i in 0..round_trips {
                for to in ["b2", "b1"] {
                    p.command(&format!("server {to}")).await.map_err(|e| e.to_string())?;
                    next_login(&mut p, &format!("round trip {i} to {to}")).await?;
                }
            }
            let switched = t.elapsed();
            let id = pumbo_identity::offline_uuid("PumboHop");
            for i in 0..reconnects {
                proxy.reconnect(id);
                next_login(&mut p, &format!("reconnect {i}")).await?;
            }
            p.command("server b2").await.map_err(|e| e.to_string())?;
            next_login(&mut p, "to b2").await?;
            let s2 = b2.take().ok_or("b2")?;
            let stopper = std::thread::spawn(move || s2.stop());
            next_login(&mut p, "fallback after b2 stop").await?;
            stopper.join().map_err(|_| "stop thread")?;
            let notice = p
                .pump(WAIT, |p| p.messages.iter().any(|m| m.contains("You were moved to b1")))
                .await
                .map_err(|e| e.to_string())?;
            let on = proxy.find_player("PumboHop").and_then(|e| e.server);
            let out = format!(
                "{} switches in {:?}, {reconnects} reconnects, fallback notice {notice}, on {on:?}, disconnect {:?}, messages {:?}",
                round_trips * 2,
                switched,
                p.disconnect,
                p.messages
            );
            if !notice || on.as_deref() != Some("b1") || p.disconnect.is_some() {
                return Err(out);
            }
            p.close().await;
            Ok(out)
        }
        .await;
        proxy.stop();
        r
    }));
    for s in b2.into_iter().chain([b1]) {
        s.stop();
    }
    eprintln!("{report:?}");
    report.unwrap();
}
