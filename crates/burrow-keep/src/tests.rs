//! Picking what a run built for upload to the build store: unsigned non-`.drv`
//! paths, plus flat downloads only when a built path refers to them directly
//! or transitively.

use std::collections::BTreeMap;

use super::{Info, Store, picked};

fn sp(name: &str) -> String {
    format!("/nix/store/{name}")
}

fn info(signed: bool, download: bool, refs: &[&str]) -> Info {
    Info {
        signed,
        download,
        references: refs.iter().map(|r| sp(r)).collect(),
        nar_hash: "sha256-A=".into(),
        nar_size: 1,
    }
}

#[test]
fn picks_unsigned_non_drvs_and_referenced_downloads_only() {
    let store = Store {
        dir: "/nix/store".into(),
        paths: BTreeMap::from([
            (
                sp("aaa-built"),
                info(false, false, &["bbb-public", "ccc-dl-1"]),
            ),
            (sp("bbb-public"), info(true, false, &["ddd-dl-public"])),
            (sp("ccc-dl-1"), info(false, true, &["eee-dl-2"])),
            (sp("ddd-dl-public"), info(false, true, &[])),
            (sp("eee-dl-2"), info(false, true, &[])),
            (sp("fff-dl-unused"), info(false, true, &[])),
            (sp("ggg-build.drv"), info(false, false, &[])),
            (sp("hhh-signed-built"), info(true, false, &[])),
        ]),
    };
    assert_eq!(
        picked(&store),
        [sp("aaa-built"), sp("ccc-dl-1"), sp("eee-dl-2")]
    );
}
