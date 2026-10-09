//! A stand-in for Pumpkin in the tests of servers run by the proxy. Like
//! Pumpkin it runs without arguments in its folder and reads `pumpkin.toml`
//! from there: it listens on the port of `[networking.java] address` and
//! ends on `stop` from stdin.
//!
//! Files in its folder change what it does:
//! - `fake-upstream` (`host:port`): every connection is forwarded there, so
//!   a scripted backend of a test answers the players;
//! - `fake-ignore-stop`: `stop` is ignored (the kill after the stop timeout).
//!
//! The end of stdin does not end it (the proxy died, the server lives on).

use std::io::{BufRead, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

fn main() -> Result<(), String> {
    let toml = std::fs::read_to_string("pumpkin.toml").map_err(|e| format!("pumpkin.toml: {e}"))?;
    let port = toml
        .lines()
        .find_map(|l| l.trim().strip_prefix("address = \"127.0.0.1:"))
        .and_then(|p| p.trim_end_matches('"').parse::<u16>().ok())
        .ok_or("no 127.0.0.1 address in pumpkin.toml")?;
    let listener =
        TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("bind {port}: {e}"))?;
    let upstream = std::fs::read_to_string("fake-upstream")
        .ok()
        .map(|s| s.trim().to_string());
    say(&format!("fake pumpkin listening on 127.0.0.1:{port}"));
    std::thread::spawn(move || {
        for client in listener.incoming().flatten() {
            if let Some(to) = &upstream {
                let to = to.clone();
                std::thread::spawn(move || forward(client, &to));
            }
        }
    });
    let ignore_stop = Path::new("fake-ignore-stop").exists();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        say(&format!("> {line}"));
        if line.trim() == "stop" {
            if ignore_stop {
                say("ignoring stop");
            } else {
                say("stopping");
                return Ok(());
            }
        }
    }
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn forward(client: TcpStream, to: &str) {
    let Ok(server) = TcpStream::connect(to) else {
        return;
    };
    let (Ok(mut c2), Ok(mut s2)) = (client.try_clone(), server.try_clone()) else {
        return;
    };
    let (mut client, mut server) = (client, server);
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut c2, &mut s2);
        let _ = s2.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut server, &mut client);
    let _ = client.shutdown(std::net::Shutdown::Write);
}
