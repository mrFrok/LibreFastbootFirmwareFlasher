//! Asks the real servers: `cargo test -p lfff-ota -- --ignored`.

use lfff_ota::protocol::{agent, query};
use lfff_ota::{Answer, Mode, Query, Region};

#[test]
#[ignore = "network"]
fn oneplus_15_global_has_a_build() {
    let q = Query {
        ota_version: "CPH2747_11.A".into(),
        model: None,
        region: Region::Gl,
        mode: Mode::Manual,
    };
    match query(&agent(), &q).unwrap() {
        Answer::Build(b) => {
            assert!(
                b["otaVersion"]
                    .as_str()
                    .unwrap()
                    .starts_with("CPH2747_11.A.")
            );
            let url = b["components"][0]["componentPackets"]["url"]
                .as_str()
                .unwrap();
            assert!(url.contains("downloadCheck"), "{url}");
        }
        other => panic!("{other:?}"),
    }
}
