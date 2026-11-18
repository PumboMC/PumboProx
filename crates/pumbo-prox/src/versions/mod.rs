//! Which client versions reach which servers: the one place that asks the
//! translation chain, for the handshake, the session and backend logins.

use pumbo_core::routing::BackendInfo;
use pumbo_core::translate::TranslationPlan;
use pumbo_protocol::{ProtocolVersion, VersionModule};
use pumbo_text::Component;
use tracing::debug;

use crate::server::{Proxy, Runtime};
use crate::session::Fail;
use crate::status::parse_text;

#[cfg(test)]
mod tests;

/// The first decision of the translation chain for `client` on a server of
/// `backend` (`None`: not known, or at the handshake).
pub(crate) fn plan(
    rt: &Runtime,
    client: ProtocolVersion,
    backend: Option<ProtocolVersion>,
) -> Option<TranslationPlan> {
    rt.modules
        .translators
        .iter()
        .find_map(|t| t.plan(client, backend))
}

/// The translation chain lets this client on `b`: the same version, or
/// translated in the proxy.
pub(crate) fn reaches(rt: &Runtime, client: ProtocolVersion, b: &BackendInfo) -> bool {
    matches!(
        plan(rt, client, b.protocol),
        Some(TranslationPlan::Passthrough | TranslationPlan::Translate(_))
    )
}

/// A network without servers, or one server that [`reaches`]: whatever its
/// health and whether players are routed to it.
pub(crate) fn has_way(rt: &Runtime, client: ProtocolVersion) -> bool {
    rt.backends.is_empty() || rt.backends.iter().any(|b| reaches(rt, client, b))
}

/// The handshake's decision on a client's version, before its login: the
/// module to log in with, or the refusal. A version passes when the
/// translation chain lets it in and, on a network with servers, one of them
/// takes it.
pub(crate) fn check<'m>(
    proxy: &Proxy,
    rt: &Runtime,
    protocol: ProtocolVersion,
    module: Option<&'m dyn VersionModule>,
) -> Result<&'m dyn VersionModule, Fail> {
    let (Some(TranslationPlan::Passthrough), Some(module)) = (plan(rt, protocol, None), module)
    else {
        debug!("a login with protocol {protocol} is refused: an unsupported version");
        return Err(unsupported(protocol));
    };
    if !has_way(rt, protocol) {
        debug!("a login with protocol {protocol} is refused: no server takes it");
        return Err(Fail::Kick(Box::new(no_way_text(proxy, rt, module))));
    }
    Ok(module)
}

/// The refusal of a version the proxy does not let in, naming the versions
/// it does.
fn unsupported(protocol: ProtocolVersion) -> Fail {
    let clients = ClientVersions::default();
    let newest = clients.newest();
    let text = match pumbo_data::releases(newest).last() {
        Some(last) if protocol > newest => {
            format!("This server supports Minecraft up to {last}.")
        }
        _ => format!(
            "Unsupported Minecraft version. Use {}.",
            clients.names(" to ")
        ),
    };
    Fail::Kick(Box::new(Component::text(text)))
}

/// `messages.no-server-for-version` for a client no server takes: its
/// release and the releases of the servers.
pub(crate) fn no_way_text(proxy: &Proxy, rt: &Runtime, client: &dyn VersionModule) -> Component {
    let mut protocols: Vec<_> = rt.backends.iter().filter_map(|b| b.protocol).collect();
    protocols.sort();
    protocols.dedup();
    let supported: Vec<String> = protocols
        .iter()
        .filter_map(|p| proxy.versions.get(*p))
        .map(|m| releases(m.release_names()))
        .collect();
    parse_text(
        &rt.config
            .messages
            .no_server_for_version
            .replace("{version}", &releases(client.release_names()))
            .replace("{supported}", &supported.join(", ")),
    )
}

/// Releases of a protocol for messages: "26.3" or "26.1–26.1.2".
pub(crate) fn releases(names: &[String]) -> String {
    match (names.first(), names.last()) {
        (Some(a), Some(b)) if a != b => format!("{a}–{b}"),
        (Some(a), _) => a.clone(),
        _ => "?".into(),
    }
}

/// The client versions of `versions.clients`: `all`, Minecraft releases and
/// ranges of them, as the protocols they let in. A release lets in every
/// release with its protocol: the handshake carries only the protocol.
#[derive(Debug, Clone)]
pub struct ClientVersions {
    /// Sorted, no repeats, never empty.
    protocols: Vec<ProtocolVersion>,
    /// Elements this build does not know, left out or cut, for the owner;
    /// `Config::parse_checked` reports them with their line.
    notes: Vec<String>,
}

impl Default for ClientVersions {
    /// `all`.
    fn default() -> Self {
        Self {
            protocols: pumbo_data::protocols().collect(),
            notes: Vec::new(),
        }
    }
}

impl ClientVersions {
    /// `items` against the releases this build knows. An element it does
    /// not know is left out with a note, a range end it does not know cuts
    /// the range with a note, so a config written for a newer build still
    /// loads; an empty list, or one that names nothing this build knows, is
    /// an error.
    pub fn parse(items: &[String]) -> Result<Self, String> {
        if items.is_empty() {
            return Err(
                "versions.clients is empty: no player could join; write all or name releases"
                    .into(),
            );
        }
        let mut protocols = std::collections::BTreeSet::new();
        let mut notes = Vec::new();
        for item in items {
            let item = item.trim();
            if item.eq_ignore_ascii_case("all") {
                protocols.extend(pumbo_data::protocols());
            } else if let Some((from, to)) = item.split_once(['-', '–']) {
                // Either dash: the proxy's own messages write the long one.
                match range(item, from.trim(), to.trim()) {
                    Ok((within, note)) => {
                        protocols.extend(within);
                        notes.extend(note);
                    }
                    Err(note) => notes.push(note),
                }
            } else if let Some(p) = protocol_of(item) {
                protocols.insert(p);
            } else {
                notes.push(unknown(item));
            }
        }
        if protocols.is_empty() {
            return Err(format!(
                "versions.clients names no release this build knows ({}): no player could join",
                known()
            ));
        }
        Ok(Self {
            protocols: protocols.into_iter().collect(),
            notes,
        })
    }

    pub fn admits(&self, protocol: ProtocolVersion) -> bool {
        self.protocols.binary_search(&protocol).is_ok()
    }

    /// The newest protocol it lets in.
    pub fn newest(&self) -> ProtocolVersion {
        self.protocols.last().copied().unwrap_or(ProtocolVersion(0))
    }

    pub fn protocols(&self) -> &[ProtocolVersion] {
        &self.protocols
    }

    /// Its releases as runs (see [`names`]).
    pub fn names(&self, to: &str) -> String {
        names(&self.protocols, to)
    }

    /// What was left out or cut; empty afterwards.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }
}

impl<'de> serde::Deserialize<'de> for ClientVersions {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Items;
        impl<'de> serde::de::Visitor<'de> for Items {
            type Value = Vec<String>;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(
                    "all, a release in quotes such as \"26.3\", a range such as 1.21.4-26.3, or a list of them",
                )
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Self::Value, E> {
                Ok(vec![s.to_string()])
            }

            // Each element as text, as written: an unquoted 26.10 stays
            // "26.10" instead of becoming the number 26.1.
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element::<String>()? {
                    items.push(item);
                }
                Ok(items)
            }
        }
        let items = d.deserialize_any(Items)?;
        Self::parse(&items).map_err(serde::de::Error::custom)
    }
}

/// The protocol of a release this build knows.
fn protocol_of(release: &str) -> Option<ProtocolVersion> {
    pumbo_data::protocols().find(|p| pumbo_data::releases(*p).contains(&release))
}

/// Every release this build knows, as one run: "1.21 to 26.3".
fn known() -> String {
    names(&pumbo_data::protocols().collect::<Vec<_>>(), " to ")
}

/// The note for an element that is no release of this build.
fn unknown(item: &str) -> String {
    let protocol = item.parse().ok().map(ProtocolVersion);
    match protocol.and_then(|p| pumbo_data::releases(p).last()) {
        Some(release) => format!(
            "versions.clients: {item} is a protocol number, left out; write the release name ({release})"
        ),
        None => format!(
            "versions.clients: unknown Minecraft release {item}, left out; this build knows {} (pumboprox version lists every release)",
            known()
        ),
    }
}

/// A release name as numbers: "1.21.10" is [1, 21, 10].
fn version(release: &str) -> Option<Vec<u32>> {
    release.split('.').map(|n| n.parse().ok()).collect()
}

/// `a` against `b`, a missing part counting as 0: 1.21 = 1.21.0 < 1.21.10.
fn compare(a: &[u32], b: &[u32]) -> std::cmp::Ordering {
    (0..a.len().max(b.len()))
        .map(|i| {
            let part = |v: &[u32]| v.get(i).copied().unwrap_or(0);
            part(a).cmp(&part(b))
        })
        .find(|o| o.is_ne())
        .unwrap_or(std::cmp::Ordering::Equal)
}

/// The protocols with a release from `from` to `to`, and a note when an end
/// is no release of this build; `Err` with the note when it lets in none.
fn range(
    item: &str,
    from: &str,
    to: &str,
) -> Result<(Vec<ProtocolVersion>, Option<String>), String> {
    let (Some(a), Some(b)) = (version(from), version(to)) else {
        return Err(unknown(item));
    };
    if compare(&a, &b).is_gt() {
        return Err(format!(
            "versions.clients: {item} runs from a newer release to an older one, left out; write {to}-{from}"
        ));
    }
    let within: Vec<ProtocolVersion> = pumbo_data::protocols()
        .filter(|p| {
            pumbo_data::releases(*p).iter().any(|r| {
                version(r).is_some_and(|r| compare(&a, &r).is_le() && compare(&r, &b).is_le())
            })
        })
        .collect();
    if within.is_empty() {
        return Err(format!(
            "versions.clients: {item} lets in no release this build knows, left out"
        ));
    }
    let strange: Vec<&str> = [from, to]
        .into_iter()
        .filter(|end| protocol_of(end).is_none())
        .collect();
    let note = (!strange.is_empty()).then(|| {
        format!(
            "versions.clients: this build does not know {}, so {item} lets in {}",
            strange.join(" and "),
            names(&within, " to ")
        )
    });
    Ok((within, note))
}

/// Releases of `protocols` (sorted) for messages, as runs of neighbouring
/// protocols of this build: "1.21.4 to 1.21.6, 26.3" with `to` " to ",
/// "1.21-26.3" with "-".
pub fn names(protocols: &[ProtocolVersion], to: &str) -> String {
    let all: Vec<ProtocolVersion> = pumbo_data::protocols().collect();
    let at = |p: ProtocolVersion| all.iter().position(|x| *x == p);
    let mut runs: Vec<(ProtocolVersion, ProtocolVersion)> = Vec::new();
    for &p in protocols {
        match runs.last_mut() {
            Some((_, last)) if at(p).is_some() && at(*last).map(|i| i + 1) == at(p) => *last = p,
            _ => runs.push((p, p)),
        }
    }
    runs.iter()
        .map(|&(a, b)| {
            match (
                pumbo_data::releases(a).first(),
                pumbo_data::releases(b).last(),
            ) {
                (Some(first), Some(last)) if first != last => format!("{first}{to}{last}"),
                (Some(first), _) => (*first).to_string(),
                _ => format!("protocol {a}"),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}
