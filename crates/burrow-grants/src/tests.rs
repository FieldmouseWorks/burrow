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

/// Grants on a machine with no MCP servers (theirs are mcp/tests.rs).
fn granted(kinds: &[Kind], offered: &plugin::Capabilities, capabilities: &[String]) -> Grants {
    let approved = Approved {
        capabilities,
        mcp_servers: &[],
        registry: &mcp::Registry::default(),
    };
    Grants::new(kinds, offered, &approved)
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
    let grants = granted(
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
    let grants = granted(
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
    let grants = granted(
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
    let grants = granted(&[Kind::Job], &offered(true, true, &["kvm"]), &[]);
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
        let grants = granted(&[Kind::Job], &offered(true, true, devices), &approved);
        assert!(!grants.holds(C::Github));
        assert!(!grants.holds(C::Environment));
        assert!(matches!(why(&grants, "environment"), Some(Why::Never(_))));
        for (name, reason) in refused {
            assert!(!grants.items().iter().any(|c| c.name() == name), "{name}");
            // Named as what asked for it: a capability, the owner's list or a device.
            let named = if name == "github" || approved.iter().any(|a| a == name) {
                name.to_owned()
            } else {
                format!("device:{name}")
            };
            assert_eq!(why(&grants, &named), Some(Why::Never(reason)), "{name}");
        }
    }
    // Everything else the two sides agree on is still granted.
    let all = granted(
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
    let grants = granted(
        &[Kind::Review],
        &offered(false, false, &["network", "gpu", "heavy"]),
        &approved(&["network", "heavy"]),
    );
    assert_eq!(grants.items(), []);
    assert_eq!(why(&grants, "network"), Some(Why::NotDeclared));
    assert_eq!(why(&grants, "device:gpu"), Some(Why::UnknownDevice));
}

#[test]
fn a_refused_device_reads_as_no_granted_capability() {
    let names = ["workspace", "heavy", "mcp", "network"];
    let grants = granted(
        &[Kind::Job],
        &offered(true, true, &names),
        &approved(&names),
    );
    assert_eq!(grants.items(), [C::Workspace, C::Heavy, C::Mcp, C::Network]);
    for name in names {
        assert_eq!(why(&grants, name), None, "{name} is granted");
        let device = format!("device:{name}");
        assert_eq!(why(&grants, &device), Some(Why::UnknownDevice), "{name}");
    }
}

#[test]
fn a_refused_capability_hides_no_refused_device() {
    let grants = granted(&[Kind::Job], &offered(false, false, &["heavy"]), &[]);
    assert_eq!(why(&grants, "heavy"), Some(Why::NotApproved));
    assert_eq!(why(&grants, "device:heavy"), Some(Why::UnknownDevice));
}

#[test]
fn a_refusal_names_the_capability_and_why() {
    let grants = granted(
        &[Kind::Job],
        &offered(false, false, &["settings-write"]),
        &approved(&["kvm", "heavvy"]),
    );
    let said: Vec<String> = grants.refusals().iter().map(|r| r.to_string()).collect();
    for expected in [
        format!("\"device:settings-write\" refused: {NO_SETTINGS}"),
        format!("\"kvm\" refused: {}", Why::NotDeclared),
        format!("\"heavvy\" refused: {}", Why::UnknownName),
        format!("\"nix\" refused: {}", Why::NotApproved),
    ] {
        assert!(said.contains(&expected), "{expected} not in {said:?}");
    }
}

#[test]
fn declared_is_what_the_describe_says_whatever_was_approved() {
    let names = |kinds: &[Kind], offered: plugin::Capabilities| -> Vec<_> {
        declared(kinds, &offered).into_iter().map(C::name).collect()
    };
    assert_eq!(
        names(&[Kind::Job], offered(true, false, &[])),
        [
            "workspace",
            "job-state",
            "git-metadata",
            "toolchains",
            "system",
            "probe",
            "units-clear",
            "heavy",
            "nix",
            "network",
        ]
    );
    assert_eq!(
        names(&[Kind::Review], offered(false, true, &["kvm", "github"])),
        ["kvm", "mcp"]
    );
    // What declared names, Grants grants once the owner approves it all.
    let all: Vec<String> = C::ALL.iter().map(|c| c.name().to_owned()).collect();
    let offer = offered(true, true, &["kvm"]);
    let grants = granted(&[Kind::Job], &offer, &all);
    assert_eq!(grants.items(), declared(&[Kind::Job], &offer));
}
