//! Moving, messaging and kicking players.

use pumbo_text::Component;
use uuid::Uuid;

use crate::server::{Proxy, Runtime, SessionCmd};
use crate::status::parse_text;

/// Players on their way to a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub players: usize,
    /// The server's name as configured.
    pub server: String,
}

/// Why [`send_players`] sent nobody.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// No server has this name.
    NoServer(String),
    /// `current` without a server to take the players from.
    NoCurrent,
    /// No player of this name is online.
    NotOnline(String),
}

/// Sends players to server `to`. `who` is `all`, `current` (everyone on
/// `current`), a server (its players) or a player; names ignore case.
pub fn send_players(
    proxy: &Proxy,
    rt: &Runtime,
    who: &str,
    current: Option<&str>,
    to: &str,
) -> Result<Sent, SendError> {
    let Some(target) = rt.backends.iter().find(|b| b.name.eq_ignore_ascii_case(to)) else {
        return Err(SendError::NoServer(to.to_string()));
    };
    let players = proxy.players();
    let chosen: Vec<_> = if who.eq_ignore_ascii_case("all") {
        players
    } else if who.eq_ignore_ascii_case("current") {
        let Some(here) = current else {
            return Err(SendError::NoCurrent);
        };
        players
            .into_iter()
            .filter(|p| p.server.as_deref() == Some(here))
            .collect()
    } else if let Some(from) = rt
        .backends
        .iter()
        .find(|b| b.name.eq_ignore_ascii_case(who))
    {
        players
            .into_iter()
            .filter(|p| p.server.as_deref() == Some(from.name.as_str()))
            .collect()
    } else {
        match proxy.find_player(who) {
            Some(p) => vec![p],
            None => return Err(SendError::NotOnline(who.to_string())),
        }
    };
    let n = chosen
        .iter()
        .filter(|p| {
            proxy.send_to(
                p.id,
                SessionCmd::Connect {
                    server: target.name.clone(),
                    quiet: true,
                },
            )
        })
        .count();
    Ok(Sent {
        players: n,
        server: target.name.clone(),
    })
}

/// `[Alert] text` (`&` codes and text tags) to every player online; returns
/// how many are online.
pub fn alert(proxy: &Proxy, text: &str) -> usize {
    let message = parse_text("&8[&cAlert&8] &r").append(parse_text(text));
    let players = proxy.players();
    for p in &players {
        proxy.send_to(p.id, SessionCmd::Message(Box::new(message.clone())));
    }
    players.len()
}

/// Disconnects a player with `reason`; `false` if the player is not online.
pub fn kick(proxy: &Proxy, id: Uuid, reason: Component) -> bool {
    proxy.send_to(id, SessionCmd::Kick(Box::new(reason)))
}
