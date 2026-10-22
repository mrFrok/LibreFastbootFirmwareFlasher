//! Asks the real servers: `cargo test -p lfff-ota -- --ignored`.

use lfff_ota::protocol::{agent, query};
use lfff_ota::{Answer, Mode, Query, Region};

fn md5(code: &str, region: Region) -> String {
    let q = Query {
        ota_version: format!("{code}_11.A"),
        model: None,
        region,
        mode: Mode::Manual,
    };
    match query(&agent(), &q).unwrap() {
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
    let gl = md5("CPH2747", Region::Gl);
    let eu = md5("CPH2747", Region::Eu);
    let india = md5("CPH2745", Region::In);
    assert_ne!(gl, eu);
    assert_ne!(gl, india);
}
