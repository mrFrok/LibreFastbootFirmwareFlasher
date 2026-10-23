//! Asks the real servers: `cargo test -p lfff-ota -- --ignored`.

use lfff_ota::protocol::query;
use lfff_ota::{Answer, Mode, Query, Region};

fn md5(code: &str, region: Region) -> String {
    let q = Query {
        ota_version: format!("{code}_11.A"),
        model: None,
        region,
        mode: Mode::Manual,
    };
    match query(&q).unwrap() {
        Answer::Build(b) => {
            let packet = &b["components"][0]["componentPackets"];
            let url = packet["url"].as_str().unwrap();
            assert!(url.contains("downloadCheck"), "{url}");
            packet["md5"].as_str().unwrap().to_string()
        }
        other => panic!("{code} {region:?}: {other:?}"),
    }
}

/// OnePlus 15: each region has a build of its own.
#[test]
#[ignore = "network"]
fn oneplus_15_in_every_region() {
    let gl = md5("CPH2747", Region::GL);
    let eu = md5("CPH2747", Region::EU);
    let india = md5("CPH2745", Region::IN);
    assert_ne!(gl, eu);
    assert_ne!(gl, india);
}

/// The whole search finds the three regional builds and their changelog.
#[test]
#[ignore = "network"]
fn search_finds_regional_builds() {
    use std::sync::atomic::AtomicBool;
    let found = lfff_ota::search(
        &lfff_ota::Search::new("CPH2747"),
        &AtomicBool::new(false),
        &|_, _| {},
    )
    .unwrap();
    for f in &found {
        eprintln!("{} {:?} {:?}", f.build.version, f.regions, f.build.md5);
    }
    assert!(found.len() >= 2);
    let url = found[0].build.changelog_url.as_deref().unwrap();
    let text = lfff_ota::changelog::fetch(url).unwrap();
    assert!(!text.is_empty());
}

/// realme answers its newest builds only to `RMX3709TR` / `RMX3709EX`.
#[test]
#[ignore = "network"]
fn realme_android_16_by_regional_model() {
    use std::sync::atomic::AtomicBool;
    let found = lfff_ota::search(
        &lfff_ota::Search::new("RMX3709"),
        &AtomicBool::new(false),
        &|_, _| {},
    )
    .unwrap();
    assert!(
        found
            .iter()
            .any(|f| f.build.version.starts_with("RMX3709_16.")),
        "{:?}",
        found.iter().map(|f| &f.build.version).collect::<Vec<_>>()
    );
}
