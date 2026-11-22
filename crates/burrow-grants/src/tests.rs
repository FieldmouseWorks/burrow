use super::*;
use Capability as C;

/// A describe's capabilities, as they come over the wire.
fn offered(network: bool, mcp: bool, devices: &[&str]) -> plugin::Capabilities {
    serde_json::from_value(serde_json::json!({
        "sandbox": "needs", "network": network, "devices": devices, "mcp": mcp,
    }))
    .unwrap()
}

fn approved(names: &[&str]) -> Vec<String> {
    names.iter().map(|&n| n.to_owned()).collect()
}

fn why(grants: &Grants, name: &str) -> Option<Why> {
    grants
        .refusals()
        .iter()
        .find(|r| r.capability == name)
        .map(|r| r.why)
}

#[test]
fn declared_and_approved_is_granted() {
    let grants = Grants::new(
        &[Kind::Job],
        &offered(true, true, &["kvm"]),
        &approved(&["workspace", "heavy", "kvm", "mcp", "network"]),
    );
    assert_eq!(
        grants.items(),
        [C::Workspace, C::Heavy, C::Kvm, C::Mcp, C::Network]
    );
}

#[test]
fn declared_and_not_approved_is_absent() {
    let grants = Grants::new(
        &[Kind::Job],
        &offered(true, true, &["kvm"]),
        &approved(&["workspace"]),
    );
    for capability in [C::Heavy, C::Nix, C::Kvm, C::Mcp, C::Network] {
        assert!(!grants.holds(capability), "{capability:?}");
        assert_eq!(why(&grants, capability.name()), Some(Why::NotApproved));
    }
}

#[test]
fn approved_and_not_declared_is_absent() {
    let grants = Grants::new(
        &[Kind::Review],
        &offered(false, false, &[]),
        &approved(&["workspace", "heavy", "kvm", "mcp", "network"]),
    );
    assert_eq!(grants.items(), []);
    for name in ["workspace", "heavy", "kvm", "mcp", "network"] {
        assert_eq!(why(&grants, name), Some(Why::NotDeclared), "{name}");
    }
}

#[test]
fn nothing_is_granted_without_an_approved_list() {
    let grants = Grants::new(&[Kind::Job], &offered(true, true, &["kvm"]), &[]);
    assert_eq!(grants.items(), []);
}

/// The most a plugin and an owner can ask for: every capability and every
/// withheld authority, both declared (as devices, describe's one free list
/// of names, besides its own fields) and approved.
#[test]
fn never_granted_whatever_is_declared_or_approved() {
    let refused = [
        ("github", NO_TOKEN),
        ("github-token", NO_TOKEN),
        ("github-installation", NO_TOKEN),
        ("settings-write", NO_SETTINGS),
        ("plugin-install", NO_SETTINGS),
    ];
    let mut names: Vec<&str> = C::ALL.iter().map(|c| c.name()).collect();
    names.extend(refused.map(|(name, _)| name));
    let devices: std::collections::BTreeSet<&str> = names.iter().copied().collect();
    let devices: Vec<&str> = devices.into_iter().collect();
    for (devices, approved) in [
        (&devices[..], approved(&names)),
        (&devices[..], vec![]),
        (&[][..], approved(&names)),
    ] {
        let grants = Grants::new(&[Kind::Job], &offered(true, true, devices), &approved);
        assert!(!grants.holds(C::Github));
        assert!(!grants.holds(C::Environment));
        assert!(matches!(why(&grants, "environment"), Some(Why::Never(_))));
        for (name, reason) in refused {
            assert!(!grants.items().iter().any(|c| c.name() == name), "{name}");
            assert_eq!(why(&grants, name), Some(Why::Never(reason)), "{name}");
        }
    }
    // Everything else the two sides agree on is still granted.
    let all = Grants::new(
        &[Kind::Job],
        &offered(true, true, &devices),
        &approved(&names),
    );
    let grantable: Vec<C> = C::ALL
        .iter()
        .copied()
        .filter(|c| !matches!(c, C::Github | C::Environment))
        .collect();
    assert_eq!(all.items(), grantable);
}

#[test]
fn a_device_name_declares_no_other_capability() {
    let grants = Grants::new(
        &[Kind::Review],
        &offered(false, false, &["network", "gpu", "heavy"]),
        &approved(&["network", "heavy"]),
    );
    assert_eq!(grants.items(), []);
    assert_eq!(why(&grants, "network"), Some(Why::NotDeclared));
    assert_eq!(why(&grants, "gpu"), Some(Why::UnknownDevice));
}

#[test]
fn a_refusal_names_the_capability_and_why() {
    let grants = Grants::new(
        &[Kind::Job],
        &offered(false, false, &["settings-write"]),
        &approved(&["kvm", "heavvy"]),
    );
    let said: Vec<String> = grants.refusals().iter().map(|r| r.to_string()).collect();
    for expected in [
        format!("\"settings-write\" refused: {NO_SETTINGS}"),
        format!("\"kvm\" refused: {}", Why::NotDeclared),
        format!("\"heavvy\" refused: {}", Why::UnknownName),
        format!("\"nix\" refused: {}", Why::NotApproved),
    ] {
        assert!(said.contains(&expected), "{expected} not in {said:?}");
    }
}
