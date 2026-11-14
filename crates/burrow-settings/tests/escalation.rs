//! The escalation policy and its route are settings: every one-way door the
//! owner may be asked about, and nothing else, with the relay's options.

use burrow_settings::schema::{Change, find};
use serde_json::json;

const DOORS: [&str; 12] = [
    "secret-create",
    "secret-rotate",
    "secret-delete",
    "zfs-destroy-or-rollback",
    "disk-wipe-or-partition",
    "remote-data-delete",
    "default-branch-force-push",
    "firmware-flash",
    "hosting-cancel-or-reinstall",
    "sign-in",
    "two-factor",
    "confirm-access",
];

#[test]
fn the_owner_reasons_are_the_one_way_doors_and_nothing_else() {
    let reasons = find("escalation.ownerReasons").expect("escalation.ownerReasons is a setting");
    assert_eq!(reasons.change, Change::Deploy);
    assert_eq!(reasons.default_value(), Some(json!(DOORS)));
    assert!(reasons.kind.check(&json!(DOORS)).is_ok());
    assert!(reasons.kind.check(&json!([])).is_ok());
    for routine in [
        "hold",
        "pause",
        "restate-authorization",
        "owner",
        "judgment",
    ] {
        assert!(reasons.kind.check(&json!([routine])).is_err(), "{routine}");
    }
}

#[test]
fn the_route_to_the_owners_project_is_a_setting() {
    let repo = find("escalation.relay.repo").expect("escalation.relay.repo is a setting");
    assert_eq!(repo.default_value(), Some(json!(null)));
    assert!(repo.kind.check(&json!("sam/homelab")).is_ok());
    let label = find("escalation.relay.label").expect("escalation.relay.label is a setting");
    assert!(label.kind.check(&json!("escalation")).is_ok());
    let acks = find("escalation.relay.acknowledgers")
        .expect("escalation.relay.acknowledgers is a setting");
    assert_eq!(acks.default_value(), Some(json!([])));
    assert!(acks.kind.check(&json!([{"login": "sam", "id": 1}])).is_ok());
    // An acknowledger is matched by account id: a login alone is refused.
    assert!(acks.kind.check(&json!([{"login": "sam"}])).is_err());
}
