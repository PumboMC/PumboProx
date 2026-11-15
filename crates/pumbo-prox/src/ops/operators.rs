//! `commands.operators`: adding and removing operators, and the one-time
//! code that makes the first one.

use tracing::{info, warn};
use uuid::Uuid;

use crate::server::{Proxy, Runtime};

/// Wrong codes for `/prox claim` before the code stops working.
const CLAIM_TRIES: u8 = 5;

/// A new one-time code for `/prox claim` on a fresh network: no operator
/// and no server yet. Kept in memory only, so every start has its own; the
/// first server ends it (`Proxy::replace`).
pub fn claim_code(proxy: &Proxy) -> Option<String> {
    let rt = proxy.runtime();
    if !rt.operators.is_empty() || !rt.backends.is_empty() {
        return None;
    }
    let n = u32::from_be_bytes(crate::session::random_bytes::<4>().ok()?) % 1_000_000;
    let code = format!("{n:06}");
    *proxy.claim.lock().ok()? = Some((code.clone(), 0));
    Some(code)
}

/// The login gate that proves who a player without a premium account is:
/// PumboAuth's. Other gates (the bot filter) do not count.
pub const LOGIN_GATE: &str = "auth";

/// Whether `plugins.required-gates` names [`LOGIN_GATE`].
pub(crate) fn login_gate_required(proxy: &Proxy) -> bool {
    proxy
        .plugins
        .get()
        .is_some_and(|p| p.host.required_gates().iter().any(|g| g == LOGIN_GATE))
}

/// [`LOGIN_GATE`] is required and its plugin runs: an offline UUID then
/// belongs to whoever passed the login.
fn login_gate_up(proxy: &Proxy) -> bool {
    login_gate_required(proxy)
        && proxy.plugins.get().is_some_and(|p| {
            p.host.plugins().iter().any(|s| {
                s.manifest
                    .gate
                    .as_ref()
                    .is_some_and(|g| g.name == LOGIN_GATE)
                    && s.status() == pumbo_host::Status::Running
            })
        })
}

/// Whether a player may become an operator: an offline UUID (version 3)
/// names anyone who joins under that name, unless the login gate checks it.
fn offline_without_login(proxy: &Proxy, id: Uuid) -> bool {
    id.get_version_num() == 3 && !login_gate_up(proxy)
}

/// Whether `/prox claim` takes a code now.
pub fn claim_open(proxy: &Proxy, rt: &Runtime) -> bool {
    rt.operators.is_empty()
        && proxy
            .claim
            .lock()
            .is_ok_and(|c| c.as_ref().is_some_and(|(_, wrong)| *wrong < CLAIM_TRIES))
}

/// Why a claim made nobody an operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    /// An offline account while the login gate does not check it.
    NotPremium,
    /// No code: the network has an operator, or no code was made.
    Nothing,
    /// Too many wrong codes.
    Exhausted,
    /// The claim came without a code.
    NoCode,
    Wrong,
    /// The config file was not written.
    Save(String),
}

/// `code` puts player `id` in `commands.operators` (in the file, comments
/// stay) and is used up.
pub fn claim(proxy: &Proxy, rt: &Runtime, id: Uuid, code: Option<&str>) -> Result<(), ClaimError> {
    if offline_without_login(proxy, id) && claim_open(proxy, rt) {
        return Err(ClaimError::NotPremium);
    }
    let mut claim = proxy
        .claim
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some((right, wrong)) = claim.as_mut().filter(|_| rt.operators.is_empty()) else {
        return Err(ClaimError::Nothing);
    };
    if *wrong >= CLAIM_TRIES {
        return Err(ClaimError::Exhausted);
    }
    let Some(code) = code else {
        return Err(ClaimError::NoCode);
    };
    if code != right.as_str() {
        *wrong += 1;
        if *wrong >= CLAIM_TRIES {
            warn!(
                "the operator code no longer works after {CLAIM_TRIES} wrong tries; restart the proxy for a new one"
            );
        }
        return Err(ClaimError::Wrong);
    }
    // Out of the lock: the save reloads, and a reload may end the code.
    let taken = claim.take();
    drop(claim);
    if let Err(e) = save_operator(proxy, id, true) {
        if proxy.runtime().backends.is_empty()
            && let Ok(mut c) = proxy.claim.lock()
        {
            *c = taken;
        }
        return Err(ClaimError::Save(e));
    }
    let (name, ip) = proxy
        .players()
        .into_iter()
        .find(|p| p.id == id)
        .map(|p| (p.name, proxy.ip(p.address)))
        .unwrap_or_default();
    info!("{name} ({id}, {ip}) is now an operator");
    Ok(())
}

/// Adds `id` to or removes it from `commands.operators` (in the file,
/// comments stay; a failed reload puts the old file back).
fn save_operator(proxy: &Proxy, id: Uuid, add: bool) -> Result<(), String> {
    crate::config_edit::save(proxy, |text| {
        // Keeps an operator someone wrote in the file without a reload.
        let mut ops = crate::config::Config::parse(text)
            .map(|c| c.commands.operators)
            .unwrap_or_default();
        ops.retain(|o| Uuid::parse_str(o).ok() != Some(id));
        if add {
            ops.push(id.to_string());
        }
        crate::config_edit::set_key(
            text,
            "commands",
            "operators",
            Some(&crate::config_edit::flow_list(&ops)),
        )
    })
}

/// Why [`operators_set`] changed nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorError {
    /// An offline account a player wants to make an operator while the
    /// login gate does not check it.
    NotPremium,
    /// Already an operator (adding) or not one (removing).
    Unchanged,
    /// The config file was not written.
    Save(String),
}

/// Who asks [`operators_set`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// The proxy's console: whoever runs the proxy.
    Console,
    /// A player in the game.
    Player(Uuid),
}

/// Makes `id` (`name` in the log) an operator or takes it away for `by`;
/// only the console may add an offline account the login gate does not
/// check.
pub fn operators_set(
    proxy: &Proxy,
    rt: &Runtime,
    id: Uuid,
    name: &str,
    add: bool,
    by: Caller,
) -> Result<(), OperatorError> {
    if add && by != Caller::Console && offline_without_login(proxy, id) {
        return Err(OperatorError::NotPremium);
    }
    if rt.operators.contains(&id) == add {
        return Err(OperatorError::Unchanged);
    }
    save_operator(proxy, id, add).map_err(OperatorError::Save)?;
    let by = match by {
        Caller::Console => "the console".to_string(),
        Caller::Player(id) => proxy
            .players()
            .into_iter()
            .find(|p| p.id == id)
            .map_or_else(|| id.to_string(), |p| format!("{} ({id})", p.name)),
    };
    if add {
        info!("{name} ({id}) is now an operator, by {by}");
    } else {
        info!("{name} ({id}) is no longer an operator, by {by}");
    }
    Ok(())
}
