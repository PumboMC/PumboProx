//! `/prox op`, `deop` and `claim`, and what the start says about operators.

use std::sync::Arc;

use pumbo_text::template::escape_mini as esc;
use uuid::Uuid;

use super::{OP, Reply, Source, exact_denied, permitted};
use crate::ops::LOGIN_GATE;
use crate::ops::operators::login_gate_required;
use crate::server::{Proxy, Runtime};

const OFFLINE_OP: &str = "<s>Anyone who joins under that name would get it. Install PumboAuth and add auth to plugins.required-gates first, or type prox op \\<uuid> in the console.";

/// Warnings for the start: logins that let anyone in under any name.
pub fn login_warnings(proxy: &Proxy) -> Vec<String> {
    let rt = proxy.runtime();
    let login = &rt.config.login;
    let mut out = Vec::new();
    if login.online_mode.is_none() {
        out.push(
            "login.online-mode is not set in pumboprox.yml, so it is per-player; new installs get online-mode: true. Set it to keep this choice".into(),
        );
    }
    if login.online_mode() == crate::config::OnlineMode::Always || login_gate_required(proxy) {
        return out;
    }
    out.push(format!(
        "online-mode is not true and plugins.required-gates has no {LOGIN_GATE} gate: players without a premium account join under any name with no password, an operator's name too. Install PumboAuth and add {LOGIN_GATE} to plugins.required-gates, or set login.online-mode: true"
    ));
    let offline: Vec<String> = rt
        .operators
        .iter()
        .filter(|id| id.get_version_num() == 3)
        .map(Uuid::to_string)
        .collect();
    if !offline.is_empty() {
        out.push(format!(
            "commands.operators has offline UUIDs ({}): anyone who joins under those names gets every proxy permission",
            offline.join(", ")
        ));
    }
    out
}

/// What the console says about the claim code at the start.
pub fn claim_hint(proxy: &Proxy, code: &str) -> String {
    let mode = proxy.runtime().config.login.online_mode();
    if mode == crate::config::OnlineMode::Always || login_gate_required(proxy) {
        return format!("No operator yet: join and type /prox claim {code} to become one");
    }
    if mode == crate::config::OnlineMode::Never {
        return "No operator yet: type prox op <uuid> here; /prox claim needs online mode or PumboAuth".into();
    }
    format!(
        "No operator yet: join with a premium account and type /prox claim {code}, or type prox op <uuid> here"
    )
}

/// `/prox claim <code>`: the right code puts the player in
/// `commands.operators` (in the file, comments stay) and is used up.
pub(super) fn claim(
    proxy: &Arc<Proxy>,
    rt: &Runtime,
    source: &Source<'_>,
    code: Option<&str>,
) -> Reply {
    use crate::ops::ClaimError;
    use crate::servers::{reply, usage};
    let Source::Player { id, .. } = source else {
        return reply("<err>Only players can claim. <s>Put a UUID in commands.operators instead.");
    };
    match crate::ops::claim(proxy, rt, *id, code) {
        Ok(()) => reply("<ok>You are now an operator of this proxy."),
        Err(ClaimError::NotPremium) => reply(&format!(
            "<err>Your account is not premium, so you cannot claim the proxy. {OFFLINE_OP}"
        )),
        Err(ClaimError::Nothing) => reply("<s>There is nothing to claim."),
        Err(ClaimError::Exhausted) => reply(
            "<err>The code no longer works after too many wrong tries. <s>A restart of the proxy makes a new one.",
        ),
        Err(ClaimError::NoCode) => usage("/prox claim <code>"),
        Err(ClaimError::Wrong) => reply("<err>Wrong code."),
        Err(ClaimError::Save(e)) => reply(&format!("<err>Not saved: <s>{}", esc(&e))),
    }
}

/// `/prox op|deop <player|uuid>`: an online player's name or a UUID.
pub(super) fn op(
    proxy: &Proxy,
    rt: &Runtime,
    source: &Source<'_>,
    add: bool,
    who: Option<&str>,
) -> Reply {
    use crate::ops::{Caller, OperatorError};
    use crate::servers::{reply, usage};
    if !permitted(proxy, rt, source, OP) {
        return reply(&exact_denied(OP));
    }
    let Some(who) = who else {
        return usage(if add {
            "/prox op <player|uuid>"
        } else {
            "/prox deop <player|uuid>"
        });
    };
    let (id, name) = match Uuid::parse_str(who) {
        Ok(id) => match proxy.players().into_iter().find(|p| p.id == id) {
            Some(p) => (id, p.name),
            None => (id, id.to_string()),
        },
        Err(_) => match proxy.find_player(who) {
            Some(p) => (p.id, p.name),
            None => {
                return reply(&format!(
                    "<err>There is no player <v>{}</v> online. <s>Use a UUID for someone offline.",
                    esc(who)
                ));
            }
        },
    };
    let by = match source {
        Source::Console => Caller::Console,
        Source::Player { id, .. } => Caller::Player(*id),
    };
    match crate::ops::operators_set(proxy, rt, id, &name, add, by) {
        Ok(()) if add => reply(&format!("<ok><v>{}</v> is now an operator.", esc(&name))),
        Ok(()) => reply(&format!(
            "<ok><v>{}</v> is no longer an operator.",
            esc(&name)
        )),
        Err(OperatorError::NotPremium) => reply(&format!(
            "<err><v>{}</v> has no premium account. {OFFLINE_OP}",
            esc(&name)
        )),
        Err(OperatorError::Unchanged) => {
            let state = if add { "already" } else { "not" };
            reply(&format!("<s><v>{}</v> is {state} an operator.", esc(&name)))
        }
        Err(OperatorError::Save(e)) => reply(&format!("<err>Not saved: <s>{}", esc(&e))),
    }
}
