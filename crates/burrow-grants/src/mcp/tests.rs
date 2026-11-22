use super::*;
use crate::{Approved, Capability, Grants};
use conary_core::plugin::{Capabilities, Kind, Sandbox};

/// `ledger` and `notes` are ordinary servers, `signed-in` acts as the owner.
fn registry() -> Registry {
    serde_json::from_value(serde_json::json!({
        "ledger": {
            "command": "/bin/ledger", "args": ["--stdio"],
            "env": {"SETTING": "value"}, "actsAsOwner": false,
        },
        "notes": {"command": "/bin/notes", "args": [], "env": {}, "actsAsOwner": false},
        "signed-in": {"command": "/bin/signed-in", "args": [], "env": {}, "actsAsOwner": true},
    }))
    .unwrap()
}

fn plugin(declares_mcp: bool, capabilities: &[&str], servers: &[&str]) -> Grants {
    let names = |list: &[&str]| list.iter().map(|&n| n.to_owned()).collect::<Vec<_>>();
    let offered = Capabilities {
        sandbox: Sandbox::Needs,
        network: false,
        devices: vec![],
        mcp: declares_mcp,
    };
    let approved = Approved {
        capabilities: &names(capabilities),
        mcp_servers: &names(servers),
        registry: &registry(),
    };
    Grants::new(&[Kind::Job], &offered, &approved)
}

fn reached(grants: &Grants) -> Vec<&str> {
    grants.servers().keys().map(String::as_str).collect()
}

fn why(grants: &Grants, server: &str) -> Option<Why> {
    let name = format!("mcp:{server}");
    grants
        .refusals()
        .iter()
        .find(|r| r.capability == name)
        .map(|r| r.why)
}

#[test]
fn mcp_alone_brings_no_server() {
    let grants = plugin(true, &["mcp"], &[]);
    assert!(grants.holds(Capability::Mcp));
    assert!(reached(&grants).is_empty());
    assert_eq!(why(&grants, "ledger"), Some(Why::ServerNotApproved));
    assert_eq!(why(&grants, "signed-in"), Some(Why::Never(ACTS_AS_OWNER)));
}

#[test]
fn a_plugin_gets_only_the_servers_approved_for_it() {
    let grants = plugin(true, &["mcp"], &["ledger"]);
    assert_eq!(reached(&grants), ["ledger"]);
    assert_eq!(why(&grants, "notes"), Some(Why::ServerNotApproved));
    assert_eq!(
        grants.servers()["ledger"],
        Server {
            command: "/bin/ledger".into(),
            args: vec!["--stdio".into()],
            env: BTreeMap::from([("SETTING".into(), "value".into())]),
        }
    );
}

#[test]
fn a_server_that_acts_as_the_owner_is_never_granted() {
    let grants = plugin(true, &["mcp"], &["ledger", "notes", "signed-in"]);
    assert_eq!(reached(&grants), ["ledger", "notes"]);
    assert_eq!(why(&grants, "signed-in"), Some(Why::Never(ACTS_AS_OWNER)));
}

#[test]
fn an_approved_server_needs_the_mcp_grant() {
    for (declared, capabilities) in [(false, &["mcp"][..]), (true, &[][..])] {
        let grants = plugin(declared, capabilities, &["ledger"]);
        assert!(reached(&grants).is_empty());
        assert_eq!(why(&grants, "ledger"), Some(Why::McpNotGranted));
    }
}

#[test]
fn a_name_outside_the_registry_is_refused() {
    let grants = plugin(true, &["mcp"], &["ledgr"]);
    assert!(reached(&grants).is_empty());
    assert_eq!(why(&grants, "ledgr"), Some(Why::UnknownServer));
}

#[test]
fn a_registry_takes_nothing_of_the_host_jobs() {
    let entry = serde_json::json!({
        "command": "/bin/server", "args": [], "env": {}, "actsAsOwner": false,
    });
    let reads = |entry: &serde_json::Value| {
        serde_json::from_value::<Registry>(serde_json::json!({ "server": entry })).is_ok()
    };
    assert!(reads(&entry));
    for field in ["envVars", "readPaths", "writePaths"] {
        let mut with = entry.clone();
        with[field] = serde_json::json!(["GH_TOKEN"]);
        assert!(!reads(&with), "{field}");
    }
    // Whether it acts as the owner is always said, never assumed.
    let mut unsaid = entry.clone();
    unsaid.as_object_mut().unwrap().remove("actsAsOwner");
    assert!(!reads(&unsaid));
}
