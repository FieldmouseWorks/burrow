//! Whose config the refresh is checked against: the default branch's, as
//! the remote has it now, and never a fork's.

use std::process::Command;

use serde_json::json;

use crate::rig::*;

fn refreshed() -> Rig {
    let rig = Rig::repo(&[("Cargo.lock", &crates(&[("b", "2.0.0")]))]);
    rig.write("Cargo.lock", &crates(&[("b", "2.1.0")]));
    rig
}

/// A refresh for a branch besides the default (Renovate reads the default
/// branch's config for it, not the one checked out) fails.
#[test]
fn a_refresh_for_another_branch_fails() {
    let mut rig = refreshed();
    assert_eq!(rig.run(&policy(), "release/1"), 1);
    assert!(rig.ran_nothing());
}

/// The default is the remote's, as it is now, not a clone's stale
/// origin/HEAD from before the default changed to main.
#[test]
fn the_default_is_the_remotes_now() {
    let mut rig = refreshed();
    let d = rig.path().to_path_buf();
    git(&d, &["push", "-q", "origin", "HEAD:refs/heads/master"]);
    git(&d, &["update-ref", "refs/remotes/origin/master", "HEAD"]);
    git(
        &d,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
        ],
    );
    assert_eq!(rig.run(&policy(), "master"), 1);
    assert!(rig.ran_nothing());
    assert_eq!(rig.run(&policy(), "main"), 0, "{:?}", rig.said);
}

/// A machine that works in forks fails every refresh: the remote is the
/// fork. So does one where the default branch can't be told.
#[test]
fn a_fork_or_no_remote_fails_it() {
    let mut rig = refreshed();
    assert_eq!(rig.run(&policy_with(json!({"forkToken": "t"})), "main"), 1);
    assert!(rig.ran_nothing());
    git(rig.path(), &["remote", "remove", "origin"]);
    assert_eq!(rig.lock_age(), 1);
    assert!(rig.ran_nothing());
}

/// Arguments: the policy, release-ages and the branch.
#[test]
fn it_takes_three_arguments() {
    for args in [
        &[][..],
        &["policy.json"],
        &["policy.json", "release-ages"],
        &["a", "b", "c", "d"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_lock-age"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).starts_with("lock-age POLICY RELEASE-AGES BRANCH")
        );
    }
}
