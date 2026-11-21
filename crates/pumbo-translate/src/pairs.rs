//! `translation.experimental-pairs`: the experimental pairs the owner turns
//! on, checked against the pairs and releases this build knows.

use pumbo_protocol::ProtocolVersion;

/// `translation.experimental-pairs` against the pairs this build translates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExperimentalPairs {
    /// The experimental pairs the list turns on, as (client, server).
    pub on: Vec<(ProtocolVersion, ProtocolVersion)>,
    /// Entries this build does not know (a release, a pair), left out:
    /// (index in the list, why). Reported like keys nothing reads.
    pub unknown: Vec<(usize, String)>,
    /// Entries for pairs that are on without them: (index in the list, why).
    pub warnings: Vec<(usize, String)>,
}

impl ExperimentalPairs {
    /// `Err((index, message))` for an entry that is not written as a pair.
    pub fn parse(listed: &[String]) -> Result<Self, (usize, String)> {
        let known = Known {
            built: crate::HAS_MULTIVERSION,
            find: &find,
            protocol_of: &protocol_of,
        };
        check(listed, &known)
    }

    /// The pairs turned on whose tables this build cannot make, each with
    /// the reason. Builds those tables, so only `check-config` asks.
    pub fn missing_data(&self) -> Vec<(ProtocolVersion, ProtocolVersion, String)> {
        self.on
            .iter()
            .filter_map(|&(client, server)| Some((client, server, why_not(client, server)?)))
            .collect()
    }
}

/// Whether this build translates `client` on `server`, and if so whether
/// the pair is experimental.
#[cfg(feature = "multiversion")]
fn find(client: i32, server: i32) -> Option<bool> {
    pumbo_translate_mv::find(client, server).map(|p| p.experimental)
}

#[cfg(not(feature = "multiversion"))]
fn find(_: i32, _: i32) -> Option<bool> {
    None
}

/// The protocol of a release this build knows.
#[cfg(feature = "multiversion")]
fn protocol_of(release: &str) -> Option<i32> {
    pumbo_data::protocol_of(release).map(|p| p.0)
}

#[cfg(not(feature = "multiversion"))]
fn protocol_of(_: &str) -> Option<i32> {
    None
}

/// Why `client` on `server` has no tables; `None` when it has them.
#[cfg(feature = "multiversion")]
fn why_not(client: ProtocolVersion, server: ProtocolVersion) -> Option<String> {
    crate::multiversion::why_not(&pumbo_translate_mv::Pair {
        client: client.0,
        server: server.0,
        experimental: true,
    })
}

#[cfg(not(feature = "multiversion"))]
fn why_not(_: ProtocolVersion, _: ProtocolVersion) -> Option<String> {
    None
}

/// What a build knows for checking the list.
struct Known<'a> {
    /// The build has version translation.
    built: bool,
    /// Whether it translates a (client, server) pair, and if so whether the
    /// pair is experimental.
    find: &'a dyn Fn(i32, i32) -> Option<bool>,
    protocol_of: &'a dyn Fn(&str) -> Option<i32>,
}

const KEY: &str = "translation.experimental-pairs";

fn check(listed: &[String], known: &Known<'_>) -> Result<ExperimentalPairs, (usize, String)> {
    let mut out = ExperimentalPairs::default();
    for (i, entry) in listed.iter().enumerate() {
        let mut sides = entry.split('>').map(str::trim);
        let (Some(client), Some(server), None) = (sides.next(), sides.next(), sides.next()) else {
            return Err((i, not_a_pair(entry)));
        };
        if client.is_empty() || server.is_empty() {
            return Err((i, not_a_pair(entry)));
        }
        if !known.built {
            out.unknown.push((
                i,
                format!(
                    "{KEY}: this build has no version translation (built without the translate feature); \"{entry}\" is left out"
                ),
            ));
            continue;
        }
        let protocol = |side: &str| {
            (known.protocol_of)(side).ok_or_else(|| {
                format!(
                    "{KEY}: unknown Minecraft release {side} in \"{entry}\"; the entry is left out (write release names, such as \"26.3>26.2\")"
                )
            })
        };
        let (c, s) = match (protocol(client), protocol(server)) {
            (Ok(c), Ok(s)) => (c, s),
            (Err(why), _) | (_, Err(why)) => {
                out.unknown.push((i, why));
                continue;
            }
        };
        match (known.find)(c, s) {
            None => out.unknown.push((
                i,
                format!(
                    "{KEY}: this build does not translate {client} clients on {server} servers; \"{entry}\" is left out"
                ),
            )),
            Some(false) => out.warnings.push((
                i,
                format!(
                    "{KEY}: this build translates {client} clients on {server} servers without this entry; remove \"{entry}\""
                ),
            )),
            Some(true) => {
                let pair = (ProtocolVersion(c), ProtocolVersion(s));
                if !out.on.contains(&pair) {
                    out.on.push(pair);
                }
            }
        }
    }
    Ok(out)
}

fn not_a_pair(entry: &str) -> String {
    format!(
        "{KEY}: \"{entry}\" is not a pair; write the client's release, > and the server's release, such as \"26.3>26.2\""
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 26.3 on 26.2 is experimental here, 26.2 on 26.3 is not.
    fn pair(client: i32, server: i32) -> Option<bool> {
        match (client, server) {
            (777, 776) => Some(true),
            (776, 777) => Some(false),
            _ => None,
        }
    }

    fn release(name: &str) -> Option<i32> {
        match name {
            "26.3" => Some(777),
            "26.2" => Some(776),
            "26.1.2" => Some(775),
            _ => None,
        }
    }

    fn check_with(built: bool, listed: &[&str]) -> Result<ExperimentalPairs, (usize, String)> {
        let listed: Vec<String> = listed.iter().map(|s| (*s).to_string()).collect();
        let known = Known {
            built,
            find: &pair,
            protocol_of: &release,
        };
        check(&listed, &known)
    }

    #[test]
    fn a_listed_experimental_pair_is_turned_on() {
        let got = check_with(true, &["26.3>26.2", " 26.3 > 26.2 ", "26.3>26.2"]).unwrap();
        assert_eq!(
            got,
            ExperimentalPairs {
                on: vec![(ProtocolVersion(777), ProtocolVersion(776))],
                ..Default::default()
            }
        );
    }

    #[test]
    fn entries_this_build_does_not_know_are_unknown() {
        let got = check_with(true, &["26.1.2>26.3", "26.3>26.3", "27.9>26.3", "776>777"]).unwrap();
        assert_eq!(got.on, []);
        assert_eq!(got.warnings, []);
        assert_eq!(
            got.unknown,
            [
                (
                    0,
                    "translation.experimental-pairs: this build does not translate 26.1.2 clients on 26.3 servers; \"26.1.2>26.3\" is left out".to_string()
                ),
                (
                    1,
                    "translation.experimental-pairs: this build does not translate 26.3 clients on 26.3 servers; \"26.3>26.3\" is left out".to_string()
                ),
                (
                    2,
                    "translation.experimental-pairs: unknown Minecraft release 27.9 in \"27.9>26.3\"; the entry is left out (write release names, such as \"26.3>26.2\")".to_string()
                ),
                (
                    3,
                    "translation.experimental-pairs: unknown Minecraft release 776 in \"776>777\"; the entry is left out (write release names, such as \"26.3>26.2\")".to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_pair_that_is_on_anyway_is_a_warning() {
        let got = check_with(true, &["26.2>26.3"]).unwrap();
        assert_eq!(got.on, []);
        assert_eq!(got.unknown, []);
        assert_eq!(
            got.warnings,
            [(
                0,
                "translation.experimental-pairs: this build translates 26.2 clients on 26.3 servers without this entry; remove \"26.2>26.3\"".to_string()
            )]
        );
    }

    #[test]
    fn an_entry_that_is_no_pair_is_an_error() {
        for bad in ["26.3-26.2", "26.3>", ">26.2", "a>b>c", ""] {
            let (at, message) = check_with(true, &["26.3>26.2", bad]).unwrap_err();
            assert_eq!(at, 1, "{bad}");
            assert_eq!(
                message,
                format!(
                    "translation.experimental-pairs: \"{bad}\" is not a pair; write the client's release, > and the server's release, such as \"26.3>26.2\""
                )
            );
        }
    }

    #[test]
    fn a_build_without_translation_knows_no_pair() {
        let got = check_with(false, &["26.3>26.2", "nonsense>pair"]).unwrap();
        assert_eq!(got.on, []);
        assert_eq!(got.warnings, []);
        let note = |entry: &str| {
            format!(
                "translation.experimental-pairs: this build has no version translation (built without the translate feature); \"{entry}\" is left out"
            )
        };
        assert_eq!(
            got.unknown,
            [(0, note("26.3>26.2")), (1, note("nonsense>pair"))]
        );
        assert!(check_with(false, &["26.3"]).is_err());
    }

    #[cfg(feature = "multiversion")]
    #[test]
    fn missing_data_names_pairs_without_tables() {
        let v = ProtocolVersion;
        let pairs = ExperimentalPairs {
            on: vec![(v(769), v(777)), (v(700), v(777))],
            ..Default::default()
        };
        let missing = pairs.missing_data();
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert_eq!((missing[0].0, missing[0].1), (v(700), v(777)));
        assert!(missing[0].2.contains("protocol 700"), "{missing:?}");
    }
}
