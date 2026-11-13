//! The live view, rendered from the fixture of 2026-11-11's stall (three
//! parked slots): the schema's required fields are there, the sections come
//! in the order the page promises, and what the snapshot doesn't establish
//! isn't claimed.

use std::fs;
use std::path::Path;

use burrow_dashboard::assets::{Assets, FONTS};
use burrow_dashboard::pages::{Frame, live::live};
use burrow_proj::snapshot::Snapshot;

const FIXTURE: &str = include_str!("stall-2026-11-11.json");

#[test]
fn the_fixture_reads_as_the_schema_says() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../burrow-proj/state.schema.json")).unwrap();
    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    for key in schema["required"].as_array().unwrap() {
        assert!(fixture.get(key.as_str().unwrap()).is_some(), "{key}");
    }
    serde_json::from_str::<Snapshot>(FIXTURE).unwrap();
}

#[test]
fn sections_come_in_order_and_what_is_unknown_isnt_claimed() {
    let snap: Snapshot = serde_json::from_str(FIXTURE).unwrap();
    let fonts = tempfile::tempdir().unwrap();
    for name in FONTS {
        fs::write(fonts.path().join(name), name.as_bytes()).unwrap();
    }
    let shell = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let assets = Assets::load(&shell, fonts.path()).unwrap();
    let f = Frame {
        title: "box",
        host: "box.ts.net",
        assets: &assets,
    };
    let html = live(&f, &snap, snap.at as u64 + 60).into_string();
    let at = |id: &str| {
        html.find(&format!("id=\"{id}\""))
            .unwrap_or_else(|| panic!("{id}"))
    };
    let order = ["front", "slots", "queue", "deploy", "meters", "holds"].map(at);
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    assert_eq!(html.matches("slot stalled").count(), 3);
    assert!(html.contains("slot idle"));
    assert!(
        html.contains("3 parked") && html.contains("1 busy"),
        "{html}"
    );
    assert!(html.contains("Taken; no holder is known."));
    assert!(!html.contains("can take it"));
    assert!(html.contains("41.5"));
    assert!(html.contains("too old to move a turn"));
    assert!(html.contains("GitHub · fixture-worker / FieldmouseWorks"));
    assert!(html.contains("0 requests remaining · resets in 20 minutes"));
    assert!(html.contains("retry in 5 minutes"));
    assert!(html.contains("Remaining budget unknown"));
}
