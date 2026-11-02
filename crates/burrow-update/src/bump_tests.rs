//! What's new upstream, brought into a real clone by nix's and curl's
//! stand-ins (testing.rs).

use std::fs;

use serde_json::json;

use super::*;
use crate::testing::{Machine, lock, sha};

fn machine() -> Machine {
    Machine::running_main(&[("nixpkgs", &sha('a')), ("burrow", &sha('b'))])
}

/// Claude Code pinned at `version` in the clone, as the flake pins it.
fn pinned(m: &mut Machine, version: &str) {
    m.cfg.manifest = Some("claude-code.json".into());
    m.commit(
        "claude-code.json",
        &json!({"version": version, "platforms": {"linux-x64": {"checksum": "c1"}}}).to_string(),
    );
}

/// Claude Code's releases, with `latest` the newest and `manifest` its
/// manifest.
fn released(m: &Machine, latest: &str, manifest: Option<serde_json::Value>) {
    let releases = m.path("releases");
    fs::create_dir_all(releases.join(latest)).unwrap();
    fs::write(releases.join("latest"), format!("{latest}\n")).unwrap();
    if let Some(manifest) = manifest {
        fs::write(
            releases.join(latest).join("manifest.zst.json"),
            manifest.to_string(),
        )
        .unwrap();
    }
}

#[test]
fn the_inputs_are_the_locks_roots_in_order() {
    let m = machine();
    assert_eq!(
        inputs(&m.cfg),
        Ok(vec!["burrow".to_string(), "nixpkgs".to_string()])
    );
    assert_eq!(
        m.kept("nix.log"),
        format!("flake metadata --json {}\n", m.cfg.work.display())
    );
    fs::write(m.cfg.work.join("flake.lock"), "{\"nodes\": {}}").unwrap();
    assert_eq!(
        inputs(&m.cfg),
        Err(Broke("nix flake metadata: no root inputs".into()))
    );
    fs::write(m.cfg.work.join("flake.lock"), "{").unwrap();
    assert!(
        inputs(&m.cfg)
            .unwrap_err()
            .0
            .starts_with("nix flake metadata: ")
    );
    fs::write(m.home("nix-down"), "").unwrap();
    assert!(inputs(&m.cfg).unwrap_err().0.contains("exit status: 1"));
}

#[test]
fn an_input_is_brought_up_to_upstream() {
    let m = machine();
    m.upstream("nixpkgs", &sha('c'));
    assert!(update_one(&m.cfg, "nixpkgs"));
    assert!(update_one(&m.cfg, "burrow"), "nothing new is no failure");
    assert_eq!(
        fs::read_to_string(m.cfg.work.join("flake.lock")).unwrap(),
        lock(&[("nixpkgs", &sha('c')), ("burrow", &sha('b'))]) + "\n"
    );
    // What nix says, after what the run's said already.
    fs::write(m.cfg.state.join("logs/update.log"), "before\n").unwrap();
    fs::write(m.home("unfetchable-burrow"), "").unwrap();
    assert!(!update_one(&m.cfg, "burrow"));
    assert_eq!(
        fs::read_to_string(m.cfg.state.join("logs/update.log")).unwrap(),
        "before\nerror: unable to download burrow\n"
    );
}

#[test]
fn claude_code_is_brought_up_to_its_latest_release() {
    let mut m = machine();
    // A flake that doesn't pin it has nothing to bring in.
    assert!(update_one(&m.cfg, CLAUDE));
    assert_eq!(m.did("calls"), "");

    pinned(&mut m, "2.1.0");
    let manifest = json!({"version": "2.1.5", "platforms": {"linux-x64": {"checksum": "c5"}}});
    released(&m, "2.1.5", Some(manifest.clone()));
    assert!(update_one(&m.cfg, CLAUDE));
    let put: serde_json::Value =
        serde_json::from_slice(&fs::read(m.cfg.work.join("claude-code.json")).unwrap()).unwrap();
    assert_eq!(put, manifest);
    assert_eq!(
        m.did("calls"),
        format!(
            "curl 30 https://downloads.example/claude/latest\ncurl 60 https://downloads.example/claude/2.1.5/manifest.zst.json\ninstall -o sam -g users -m 644 {} {}\n",
            m.cfg.state.join("manifest.json").display(),
            m.cfg.work.join("claude-code.json").display()
        )
    );
    // Pinned at the latest already: nothing fetched but what's latest.
    fs::remove_file(m.path("calls")).unwrap();
    assert!(update_one(&m.cfg, CLAUDE));
    assert_eq!(
        m.did("calls"),
        "curl 30 https://downloads.example/claude/latest\n"
    );
}

#[test]
fn a_claude_code_release_it_cant_trust_isnt_brought_in() {
    let mut m = machine();
    pinned(&mut m, "2.1.0");
    let was = fs::read(m.cfg.work.join("claude-code.json")).unwrap();
    // No releases at all, a latest that isn't a version (it goes into a
    // URL), one with no manifest, and manifests without a version or a
    // checksum.
    assert!(!update_one(&m.cfg, CLAUDE));
    for latest in ["", "../../etc", "2.1.5?x", "2.1.5 2"] {
        fs::create_dir_all(m.path("releases")).unwrap();
        fs::write(m.path("releases/latest"), format!("{latest}\n")).unwrap();
        assert!(!update_one(&m.cfg, CLAUDE), "{latest:?}");
    }
    released(&m, "2.1.6", None);
    assert!(!update_one(&m.cfg, CLAUDE));
    for manifest in [
        json!({"platforms": {"linux-x64": {"checksum": "c6"}}}),
        json!({"version": "2.1.6", "platforms": {"linux-x64": {"checksum": null}}}),
        json!({"version": false, "platforms": {"linux-x64": {"checksum": "c6"}}}),
    ] {
        released(&m, "2.1.6", Some(manifest.clone()));
        assert!(!update_one(&m.cfg, CLAUDE), "{manifest}");
    }
    assert_eq!(fs::read(m.cfg.work.join("claude-code.json")).unwrap(), was);
    assert!(!m.did("calls").contains("install"));
}

#[test]
fn a_bump_says_what_moved_and_is_committed() {
    let mut m = machine();
    pinned(&mut m, "2.1.0");
    let base = m.head();
    let every = inputs(&m.cfg).unwrap();
    assert!(!commit(&m.cfg, &every), "nothing moved");
    assert_eq!(m.head(), base);

    m.upstream("nixpkgs", &sha('c'));
    assert!(update_one(&m.cfg, "nixpkgs"));
    released(
        &m,
        "2.1.5",
        Some(json!({"version": "2.1.5", "platforms": {"linux-x64": {"checksum": "c5"}}})),
    );
    assert!(update_one(&m.cfg, CLAUDE));
    let moved = format!("nixpkgs aaaaaaa -> ccccccc, {CLAUDE} 2.1.0 -> 2.1.5");
    assert_eq!(said(&describe(&m.cfg, &every, "HEAD")), moved);
    assert!(commit(&m.cfg, &every));
    assert_eq!(
        m.work(&["log", "-1", "--format=%B"]),
        format!(
            "deps: update {moved}\n\nPushed by box-update after the build, the VM tests and the health check passed."
        )
    );
    assert_eq!(m.work(&["rev-parse", "HEAD~1"]), base);
    assert_eq!(said(&describe(&m.cfg, &every, &base)), moved);
    assert_eq!(said(&describe(&m.cfg, &every, "HEAD")), "");
}

#[test]
fn an_input_that_follows_another_or_wasnt_there_moves_from_nothing() {
    let m = machine();
    fs::write(
        m.cfg.work.join("flake.lock"),
        lock(&[
            ("nixpkgs", &sha('a')),
            ("burrow", &sha('b')),
            ("fresh", &sha('f')),
        ])
        .replace("\"burrow\": \"burrow\"", "\"burrow\": [\"nixpkgs\"]"),
    )
    .unwrap();
    let every = inputs(&m.cfg).unwrap();
    assert_eq!(
        said(&describe(&m.cfg, &every, "HEAD")),
        "burrow bbbbbbb -> , fresh  -> fffffff"
    );
}
