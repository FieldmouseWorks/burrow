//! The age: the machine's config, its globalExtends, and the repo's own.

use serde_json::{Value, json};

use crate::rig::*;

/// h 1.1.0 is 5 days old: past the 3 days, not 14. With `files` in the
/// base and the machine's `policy`, whether the refresh to it holds back.
fn held(files: &[(&str, &str)], policy: &Value) -> bool {
    let mut base = vec![("Cargo.lock", crates(&[("b", "2.0.0"), ("h", "1.0.0")]))];
    base.extend(files.iter().map(|(p, t)| (*p, (*t).to_owned())));
    let base: Vec<(&str, &str)> = base.iter().map(|(p, t)| (*p, t.as_str())).collect();
    let mut rig = Rig::repo(&base);
    rig.write("Cargo.lock", &crates(&[("b", "2.1.0"), ("h", "1.1.0")]));
    assert_eq!(
        rig.run(policy, "main"),
        0,
        "{files:?} {policy}: {:?}",
        rig.said
    );
    match rig.versions("Cargo.lock")["h"].as_str() {
        "1.0.0" => true,
        "1.1.0" => false,
        v => panic!("{v}"),
    }
}

fn two_weeks() -> String {
    json!({"packageRules": [{"matchManagers": ["cargo"], "minimumReleaseAge": "2 weeks"}],
           "vulnerabilityAlerts": {"minimumReleaseAge": null}})
    .to_string()
}

/// The repo's own config asks for longer (anywhere in it): that age holds.
/// Without it, 5 days is old enough.
#[test]
fn the_repos_own_longer_age_holds() {
    assert!(held(&[(".github/renovate.json", &two_weeks())], &policy()));
    assert!(!held(&[], &policy()));
}

/// What isn't an age doesn't count: a status check's name, a comment, a
/// description, package.json without a "renovate" key. With one, it's a
/// config.
#[test]
fn only_an_age_counts() {
    let json5 = "// minimumReleaseAge: 2 weeks, one day\n{statusCheckNames: \
                 {minimumReleaseAge: 'custom/stability-days'}, description: 'minimumReleaseAge'}";
    assert!(!held(&[("renovate.json5", json5)], &policy()));
    let package = json!({"name": "x", "minimumReleaseAge": "2 weeks"}).to_string();
    assert!(!held(&[("package.json", &package)], &policy()));
    let package = json!({"renovate": {"cargo": {"minimumReleaseAge": "14 days"}}}).to_string();
    assert!(held(&[("package.json", &package)], &policy()));
}

/// Renovate reads only the first config file it finds, .gitlab/'s among
/// them (Renovate looks there on GitHub too).
#[test]
fn only_the_first_config_file_counts() {
    assert!(!held(
        &[
            ("renovate.json", "{}"),
            (".github/renovate.json", &two_weeks())
        ],
        &policy()
    ));
    assert!(held(
        &[(".gitlab/renovate.json5", "{minimumReleaseAge: '2 weeks'}")],
        &policy()
    ));
}

/// A preset it can resolve counts, in the repo's config or the machine's,
/// at any scope.
#[test]
fn a_presets_age_counts() {
    let rule = r#"{"packageRules": [{"matchManagers": ["cargo"], "extends": ["slow"]}]}"#;
    for (policy, files) in [
        (
            policy(),
            vec![("renovate.json", r#"{"extends": ["slow"]}"#)],
        ),
        (policy(), vec![("renovate.json", rule)]),
        (policy_with(json!({"extends": ["slow"]})), vec![]),
        (policy_with(json!({"cargo": {"extends": ["slow"]}})), vec![]),
        (policy_with(json!({"globalExtends": ["slow"]})), vec![]),
    ] {
        assert!(held(&files, &policy), "{policy} {files:?}");
    }
}

/// One it can't resolve, or an age it can't read, fails the refresh before
/// anything runs.
#[test]
fn what_it_cant_read_fails_it() {
    for (policy, files) in [
        (
            policy(),
            vec![("renovate.json5", r#"{extends: ["github>someone/presets"]}"#)],
        ),
        (
            policy(),
            vec![("renovate.json5", "{minimumReleaseAge: someAge}")],
        ),
        (
            policy_with(json!({"extends": ["github>someone/presets"]})),
            vec![],
        ),
        (policy(), vec![("renovate.json", "{")]),
        (policy(), vec![("renovate.json", "[]")]),
    ] {
        let mut base = vec![("Cargo.lock", crates(&[("b", "2.0.0")]))];
        base.extend(files.iter().map(|(p, t)| (*p, (*t).to_owned())));
        let base: Vec<(&str, &str)> = base.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let mut rig = Rig::repo(&base);
        rig.write("Cargo.lock", &crates(&[("b", "2.1.0")]));
        assert_eq!(rig.run(&policy, "main"), 1, "{policy} {files:?}");
        assert!(rig.ran_nothing(), "{policy} {files:?}");
    }
}

/// JSONC, and a config file the machine adds (configFileNames), read first,
/// its name whole.
#[test]
fn jsonc_and_the_machines_own_config_files() {
    assert!(held(
        &[(
            "renovate.jsonc",
            r#"{/* slow */ "minimumReleaseAge": "2 weeks",}"#
        )],
        &policy()
    ));
    let named = policy_with(json!({"configFileNames": ["ci/renovate config.json"]}));
    assert!(held(
        &[
            ("renovate.json", "{}"),
            (
                "ci/renovate config.json",
                r#"{"minimumReleaseAge": "2 weeks"}"#
            )
        ],
        &named
    ));
}

/// The machine's longest age holds, wherever in its config it's set.
#[test]
fn the_machines_longest_age_holds() {
    assert!(held(
        &[],
        &policy_with(json!({"cargo": {"minimumReleaseAge": "1 weeks"}}))
    ));
}

/// A machine that sets no age at all holds to none, and still to the repo's.
#[test]
fn no_age_is_none() {
    for policy in [json!({"minimumReleaseAge": null}), json!({})] {
        let mut rig = Rig::repo(&[("Cargo.lock", &crates(&[("c", "0.4.0")]))]);
        rig.write("Cargo.lock", &crates(&[("c", "0.4.1")]));
        assert_eq!(rig.run(&policy, "main"), 0, "{policy}");
        assert_eq!(
            rig.versions("Cargo.lock"),
            map(&[("c", "0.4.1")]),
            "{policy}"
        );
        assert!(
            held(&[("renovate.json", &two_weeks())], &policy),
            "{policy}"
        );
    }
}

/// Config the machine inherits from another repo fails every refresh, set
/// directly or by a preset.
#[test]
fn inherited_config_fails_it() {
    for policy in [
        policy_with(json!({"inheritConfig": true})),
        policy_with(json!({"extends": ["inherits"]})),
        policy_with(json!({"globalExtends": ["inherits"]})),
    ] {
        let mut rig = Rig::repo(&[("Cargo.lock", &crates(&[("b", "2.0.0")]))]);
        rig.write("Cargo.lock", &crates(&[("b", "2.1.0")]));
        assert_eq!(rig.run(&policy, "main"), 1, "{policy}");
        assert!(rig.ran_nothing(), "{policy}");
    }
}

/// Config files the machine adds are read, from its globalExtends too, but
/// not from its ordinary extends (Renovate settles the files before it
/// resolves those); the machine's own configFileNames win over
/// globalExtends', null included.
#[test]
fn config_files_come_from_the_machine_as_written() {
    for (policy, holds) in [
        (policy_with(json!({"extends": ["files"]})), false),
        (policy_with(json!({"globalExtends": ["files"]})), true),
        (
            policy_with(json!({"globalExtends": ["files"], "configFileNames": []})),
            false,
        ),
        (
            policy_with(json!({"globalExtends": ["files"], "configFileNames": null})),
            false,
        ),
    ] {
        let slow = [("ci/slow.json", r#"{"minimumReleaseAge": "2 weeks"}"#)];
        assert_eq!(held(&slow, &policy), holds, "{policy}");
    }
}
