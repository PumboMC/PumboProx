use super::*;

#[test]
fn experimental_pairs_line() {
    assert_eq!(pairs_line(&[]), None);
    let p = ProtocolVersion;
    let line = pairs_line(&[(p(777), p(776)), (p(775), p(777))]).unwrap();
    assert_eq!(line, "experimental pairs: 26.3>26.2, 26.1.2>26.3");
    // Each pair as the key takes it back.
    for entry in line["experimental pairs: ".len()..].split(", ") {
        let listed = [entry.to_string()];
        assert!(
            pumbo_translate::ExperimentalPairs::parse(&listed).is_ok(),
            "{entry}"
        );
    }
}

#[test]
fn release_ranges() {
    assert_eq!(releases(&["26.3".into()]), "26.3");
    assert_eq!(
        releases(&["26.1".into(), "26.1.1".into(), "26.1.2".into()]),
        "26.1–26.1.2"
    );
    assert_eq!(releases(&[]), "?");
}

fn v(protocols: &[i32]) -> Vec<ProtocolVersion> {
    protocols.iter().copied().map(ProtocolVersion).collect()
}

fn parse(items: &[&str]) -> ClientVersions {
    let items: Vec<String> = items.iter().map(|s| s.to_string()).collect();
    ClientVersions::parse(&items).unwrap()
}

#[test]
fn releases_and_ranges_let_in_their_protocols() {
    let all: Vec<ProtocolVersion> = pumbo_data::protocols().collect();
    assert_eq!(parse(&["all"]).protocols(), all);
    assert_eq!(parse(&["All"]).protocols(), all);
    assert_eq!(ClientVersions::default().protocols(), all);
    assert_eq!(parse(&["1.21.4", "26.3"]).protocols(), v(&[769, 777]));
    // 1.21.7 and 1.21.8 share a protocol.
    assert_eq!(parse(&["1.21.7"]).protocols(), v(&[772]));
    assert_eq!(parse(&["26.1-26.1.2"]).protocols(), v(&[775]));
    // As `messages.no-server-for-version` writes a range.
    assert_eq!(parse(&["26.1–26.1.2"]).protocols(), v(&[775]));
    assert_eq!(parse(&["1.21.8-1.21.9"]).protocols(), v(&[772, 773]));
    // Numbers, not text: 1.21.10 comes after 1.21.9.
    assert_eq!(parse(&["1.21.9-1.21.10"]).protocols(), v(&[773]));
    assert_eq!(
        parse(&["1.21.4 - 1.21.6", "1.21.5", "26.3"]).protocols(),
        v(&[769, 770, 771, 777])
    );
    let mut c = parse(&["1.21.4", "26.3"]);
    assert!(c.admits(ProtocolVersion(769)) && !c.admits(ProtocolVersion(770)));
    assert_eq!(c.newest(), ProtocolVersion(777));
    assert!(c.take_notes().is_empty());
}

/// What this build does not know is left out or cut with a note, so a list
/// written for a newer build still loads.
#[test]
fn unknown_releases_are_left_out_with_a_note() {
    let mut c = parse(&[
        "26.3",
        "1.21.12",
        "777",
        "1.21.4-26.4",
        "26.3-1.21.4",
        "1.8-1.12",
    ]);
    assert_eq!(
        c.protocols(),
        v(&[769, 770, 771, 772, 773, 774, 775, 776, 777])
    );
    assert_eq!(
        c.take_notes(),
        [
            "versions.clients: unknown Minecraft release 1.21.12, left out; this build knows 1.21 to 26.3 (pumboprox version lists every release)",
            "versions.clients: 777 is a protocol number, left out; write the release name (26.3)",
            "versions.clients: this build does not know 26.4, so 1.21.4-26.4 lets in 1.21.4 to 26.3",
            "versions.clients: 26.3-1.21.4 runs from a newer release to an older one, left out; write 1.21.4-26.3",
            "versions.clients: 1.8-1.12 lets in no release this build knows, left out",
        ]
    );
    assert!(c.take_notes().is_empty());
}

#[test]
fn an_empty_list_or_one_without_a_known_release_is_an_error() {
    let err = |items: &[&str]| {
        let items: Vec<String> = items.iter().map(|s| s.to_string()).collect();
        ClientVersions::parse(&items).unwrap_err()
    };
    assert_eq!(
        err(&[]),
        "versions.clients is empty: no player could join; write all or name releases"
    );
    assert_eq!(
        err(&["26.4", "777x"]),
        "versions.clients names no release this build knows (1.21 to 26.3): no player could join"
    );
}

/// Elements are read as written, also unquoted; a bare number is refused.
#[test]
fn yaml_elements_are_text() {
    let read = |text: &str| pumbo_core::yaml::from_str::<ClientVersions>(text);
    assert_eq!(read("[26.2, 26.3]").unwrap().protocols(), v(&[776, 777]));
    assert_eq!(read("1.21.4-26.3").unwrap().protocols().len(), 9);
    let mut c = read("[26.3, 26.10]").unwrap();
    assert_eq!(c.protocols(), v(&[777]));
    let notes = c.take_notes();
    assert!(
        notes[0].contains("unknown Minecraft release 26.10,"),
        "{notes:?}"
    );
    let e = read("26.3").unwrap_err();
    assert!(e.contains("expected all, a release in quotes"), "{e}");
}

#[test]
fn names_come_in_runs() {
    let all: Vec<ProtocolVersion> = pumbo_data::protocols().collect();
    assert_eq!(names(&all, " to "), "1.21 to 26.3");
    assert_eq!(names(&all, "-"), "1.21-26.3");
    assert_eq!(
        names(&v(&[769, 770, 771, 777]), " to "),
        "1.21.4 to 1.21.6, 26.3"
    );
    assert_eq!(names(&v(&[775]), " to "), "26.1 to 26.1.2");
    assert_eq!(names(&v(&[777]), " to "), "26.3");
    assert_eq!(names(&v(&[767, 777]), "-"), "1.21-1.21.1, 26.3");
}

/// Ranges split at the first `-`: no release name of this build has one.
#[test]
fn no_release_name_has_a_dash() {
    for p in pumbo_data::protocols() {
        for r in pumbo_data::releases(p) {
            assert!(!r.contains('-'), "{p}: {r}");
        }
    }
}

/// Through the whole config: releases this build does not know are notes
/// with the line of `clients`, like unknown keys; elements stay text.
#[test]
fn unknown_releases_in_the_config_are_remarks() {
    use crate::config::Config;
    let text = "status: {}\nversions:\n  clients: [26.3, 26.4, 777, 26.10, \"1.21.4-26.9\"]\n";
    let (mut cfg, remarks) = Config::parse_checked(text).unwrap();
    assert_eq!(
        cfg.versions.clients.protocols(),
        v(&[769, 770, 771, 772, 773, 774, 775, 776, 777])
    );
    assert!(cfg.versions.clients.take_notes().is_empty());
    let lines: Vec<Option<usize>> = remarks.unknown.iter().map(|(l, _)| *l).collect();
    assert_eq!(lines, [Some(3); 4], "{:?}", remarks.unknown);
    assert!(
        remarks.unknown[2].1.contains("release 26.10,"),
        "{:?}",
        remarks.unknown
    );
    let err = |text: &str| Config::parse(text).unwrap_err().to_string();
    assert!(err("versions:\n  clients: [26.4]\n").contains("names no release this build knows"));
    assert!(err("versions:\n  clients: []\n").contains("is empty"));
    let e = err("versions:\n  clients: 26.3\n");
    assert!(
        e.contains("expected all, a release in quotes") && e.contains("line 2"),
        "{e}"
    );
}

/// A server whose version no client may join with still has its version:
/// translated clients reach it, and its tables are there when it is used.
#[cfg(feature = "translate")]
#[test]
fn a_server_off_the_list_keeps_its_version() {
    use crate::config::Config;
    use crate::server::Runtime;
    let text = "forwarding:\n  mode: none\nversions:\n  clients: [\"1.21.4\"]\n\
                servers:\n  lobby: { address: \"127.0.0.1:1\", protocol: 777 }\n";
    let rt = Runtime::build(Config::parse(text).unwrap()).unwrap();
    assert!(!rt.config.versions.clients.admits(ProtocolVersion(777)));
    assert!(has_way(&rt, ProtocolVersion(769)));
    let registry = crate::modules::versions().unwrap();
    let server = registry.get(ProtocolVersion(777)).unwrap();
    assert!(
        server
            .packet_id(
                pumbo_protocol::Phase::Login,
                pumbo_protocol::Direction::Serverbound,
                pumbo_protocol::PacketKind::Hello
            )
            .is_some()
    );
}

#[test]
fn unreachable_only_when_no_listed_version_reaches_a_server() {
    use crate::config::Config;
    use crate::server::Runtime;
    let rt = |servers: &str| {
        let text = format!(
            "forwarding:\n  mode: none\nversions:\n  clients: [\"26.3\"]\nservers: {servers}\n"
        );
        Runtime::build(Config::parse(&text).unwrap()).unwrap()
    };
    assert_eq!(
        unreachable(&rt("{ old: { address: \"127.0.0.1:2\", protocol: 776 } }")).as_deref(),
        Some(
            "versions.clients: no listed version reaches any server (old 26.2); every player is refused"
        )
    );
    assert_eq!(
        unreachable(&rt("{ new: { address: \"127.0.0.1:2\", protocol: 777 } }")),
        None
    );
    assert_eq!(
        unreachable(&rt("{ any: { address: \"127.0.0.1:2\" } }")),
        None
    );
    assert_eq!(unreachable(&rt("{}")), None);
}

/// The way of a protocol is worked out once per runtime and kept.
#[tokio::test]
async fn the_way_of_a_version_is_kept_per_runtime() {
    use crate::config::Config;
    use crate::server::Runtime;
    let rt = |protocol: i32| {
        let text = format!(
            "forwarding:\n  mode: none\nservers:\n  lobby: {{ address: \"127.0.0.1:1\", protocol: {protocol} }}\n"
        );
        Arc::new(Runtime::build(Config::parse(&text).unwrap()).unwrap())
    };
    let old = rt(776);
    let newer = ProtocolVersion(777);
    assert!(!way(&old, newer).await);
    assert_eq!(old.ways.of(newer).and_then(OnceLock::get), Some(&false));
    assert!(way(&old, ProtocolVersion(776)).await);
    assert_eq!(
        old.ways.of(ProtocolVersion(776)).and_then(OnceLock::get),
        Some(&true)
    );
    assert_eq!(
        old.ways.of(ProtocolVersion(775)).and_then(OnceLock::get),
        None
    );
    // Translated: 26.2 on a 26.3 server, with the translator built in.
    if cfg!(feature = "translate") {
        let new = rt(777);
        assert!(way(&new, ProtocolVersion(776)).await);
        assert_eq!(
            new.ways.of(ProtocolVersion(776)).and_then(OnceLock::get),
            Some(&true)
        );
    }
}
