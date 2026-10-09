//! The committed tables equal what the generator produces from the cached
//! reports of every release. Skipped when the cache is not filled
//! (`cargo run -p pumbo-datagen --release -- tables` fills it).

use pumbo_datagen::{default_cache, diff, fetch, group, reports};

#[test]
fn committed_tables_match_the_reports() {
    let cache = default_cache();
    let Ok(releases) = fetch::releases_since(&cache, "1.21", true) else {
        eprintln!("no cached manifest in {}, skipping", cache.display());
        return;
    };
    let mut per_release = Vec::new();
    for r in &releases {
        let dir = cache.join(&r.id);
        let jar = dir.join("server.jar");
        if !dir.join("out").join(".pumbo-datagen-ok").is_file() || !jar.is_file() {
            eprintln!("{} not generated in the cache, skipping", r.id);
            return;
        }
        let version = reports::jar_version(&jar).unwrap();
        let sha1 = fetch::sha1_file(&jar).unwrap();
        per_release
            .push(reports::tables(&dir.join("out").join("reports"), &version, &sha1).unwrap());
    }
    let protocols = group(per_release).unwrap();
    assert_eq!(
        protocols.iter().map(|t| t.protocol).collect::<Vec<_>>(),
        pumbo_data::protocols().collect::<Vec<_>>(),
        "protocols differ from the committed tables (run the generator)"
    );
    for t in &protocols {
        let committed = pumbo_data::tables(t.protocol).unwrap();
        assert!(
            diff::same_content(t, committed) && t.releases == committed.releases,
            "protocol {} differs from the committed tables: {:?}",
            t.protocol,
            diff::describe(committed, t)
        );
    }
}
