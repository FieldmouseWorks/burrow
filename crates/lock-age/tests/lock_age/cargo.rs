//! Cargo.lock: a refresh's too-new crates go back, or fail it.

use crate::rig::*;

#[test]
fn nothing_changed_is_nothing_to_do() {
    let mut rig = Rig::repo(&[("Cargo.lock", &crates(&[("b", "2.0.0")]))]);
    assert_eq!(rig.lock_age(), 0);
    assert!(rig.ran_nothing(), "{:?}", rig.w.ran);
}

/// A refresh that took a too-new crate takes the newest one old enough
/// instead; one old enough stays, as do a git dependency's commit and a path
/// crate of the workspace's own.
#[test]
fn a_too_new_crate_takes_the_newest_old_enough() {
    let mut rig = Rig::repo(&[(
        "Cargo.lock",
        &cargo_lock(&[
            ("a", "1.0.0", Some(CRATES), &[]),
            ("b", "2.0.0", Some(CRATES), &[]),
            ("me", "0.1.0", None, &[]),
        ]),
    )]);
    rig.w
        .crate_("a", &[("1.0.0", old()), ("1.0.1", old()), ("1.0.2", new())]);
    rig.write(
        "Cargo.lock",
        &cargo_lock(&[
            ("a", "1.0.2", Some(CRATES), &[]),
            ("b", "2.1.0", Some(CRATES), &[]),
            ("g", "0.3.0", Some("git+https://example.org/g#abc"), &[]),
            ("me", "0.1.0", None, &[]),
        ]),
    );
    assert_eq!(rig.lock_age(), 0, "{:?}", rig.said);
    assert_eq!(
        rig.versions("Cargo.lock"),
        map(&[
            ("a", "1.0.1"),
            ("b", "2.1.0"),
            ("g", "0.3.0"),
            ("me", "0.1.0")
        ])
    );
}

/// In a member's directory: cargo runs there.
#[test]
fn cargo_runs_where_the_lock_file_is() {
    let mut rig = Rig::repo(&[(
        "server/Cargo.lock",
        &crates(&[("b", "2.0.0"), ("c", "0.4.0")]),
    )]);
    rig.write(
        "server/Cargo.lock",
        &crates(&[("b", "2.1.0"), ("c", "0.4.1")]),
    );
    assert_eq!(rig.lock_age(), 0, "{:?}", rig.said);
    assert_eq!(
        rig.versions("server/Cargo.lock"),
        map(&[("b", "2.1.0"), ("c", "0.4.0")])
    );
    let dirs: Vec<&str> = rig
        .w
        .ran
        .iter()
        .filter(|(c, _)| c[0] == "cargo")
        .map(|(_, d)| d.as_str())
        .collect();
    assert_eq!(dirs, ["server"]);
}

/// None older than the new one is old enough, but the base's is (as a base
/// can hold a too-new one, merged on purpose): back to it.
#[test]
fn none_old_enough_goes_back_to_the_bases() {
    let mut rig = Rig::repo(&[("Cargo.lock", &crates(&[("b", "2.0.0"), ("d", "0.9.0")]))]);
    rig.w
        .crate_("d", &[("0.9.0", new()), ("1.0.0", new()), ("1.1.0", new())]);
    rig.write("Cargo.lock", &crates(&[("b", "2.1.0"), ("d", "1.1.0")]));
    assert_eq!(rig.lock_age(), 0, "{:?}", rig.said);
    assert_eq!(
        rig.versions("Cargo.lock"),
        map(&[("b", "2.1.0"), ("d", "0.9.0")])
    );
}

/// A crate the refresh moved needs the too-new one: that crate goes back to
/// the base's version too, and then the too-new one can.
#[test]
fn what_needs_it_goes_back_too() {
    let lock = |e, q| {
        cargo_lock(&[
            ("b", "2.0.0", Some(CRATES), &[]),
            ("e", e, Some(CRATES), &[]),
            ("q", q, Some(CRATES), &["e"]),
        ])
    };
    let mut rig = Rig::repo(&[("Cargo.lock", &lock("1.0.0", "1.0.0"))]);
    rig.w.crate_("e", &[("1.0.0", old()), ("1.0.1", new())]);
    rig.w.crate_("q", &[("1.0.0", old()), ("1.1.0", old())]);
    rig.write(
        "Cargo.lock",
        &lock("1.0.1", "1.1.0").replace("2.0.0", "2.1.0"),
    );
    rig.w.refuse.push(Box::new(|text, name, _| {
        name == "e" && text.contains("name = \"q\"\nversion = \"1.1.0\"")
    }));
    assert_eq!(rig.lock_age(), 0, "{:?}", rig.said);
    assert_eq!(
        rig.versions("Cargo.lock"),
        map(&[("b", "2.1.0"), ("e", "1.0.0"), ("q", "1.0.0")])
    );
}

/// Only what the refresh moved goes back for it: a crate the base had as it
/// is stays, though the base had another version of it too.
#[test]
fn what_the_refresh_didnt_move_stays() {
    let lock = |e, q2| {
        cargo_lock(&[
            ("e", e, Some(CRATES), &[]),
            ("q", "1.0.0", Some(CRATES), &["e"]),
            ("q", q2, Some(CRATES), &[]),
        ])
    };
    let mut rig = Rig::repo(&[("Cargo.lock", &lock("1.0.0", "2.0.0"))]);
    rig.w.crate_("e", &[("1.0.0", old()), ("1.0.1", new())]);
    rig.w
        .crate_("q", &[("1.0.0", old()), ("2.0.0", old()), ("2.1.0", old())]);
    rig.write("Cargo.lock", &lock("1.0.1", "2.1.0"));
    rig.w.refuse.push(Box::new(|_, name, _| name == "e"));
    assert_eq!(rig.lock_age(), 1);
    let asked: Vec<&str> = rig.w.ran.iter().map(|(c, _)| c[4].as_str()).collect();
    assert!(!asked.contains(&"q@1.0.0"), "{asked:?}");
}

/// Taken all the way back to the base's, the lock file would look
/// unchanged, and Renovate would commit its own refresh instead: that fails
/// it, saying last when its first too young release is old enough, for the
/// steward to wait for; the first of several; even when another lock file
/// did change.
#[test]
fn all_the_way_back_waits_for_the_first_to_be_old_enough() {
    let mut rig = Rig::repo(&[("Cargo.lock", &crates(&[("c", "0.4.0")]))]);
    rig.write("Cargo.lock", &crates(&[("c", "0.4.1")]));
    assert_eq!(rig.waiting(&policy()), (1, Some(ripe(&new()))));
    assert_eq!(rig.versions("Cargo.lock"), map(&[("c", "0.4.0")]));

    let mut rig = Rig::repo(&[("Cargo.lock", &crates(&[("c", "0.4.0"), ("g", "1.0.0")]))]);
    rig.w
        .crate_("g", &[("1.0.0", old()), ("1.1.0", new()), ("1.2.0", mid())]);
    rig.write("Cargo.lock", &crates(&[("c", "0.4.1"), ("g", "1.2.0")]));
    let week = serde_json::json!({"minimumReleaseAge": "1 weeks"});
    let at = ripe(&mid()) + jiff::SignedDuration::from_hours(4 * 24);
    assert_eq!(rig.waiting(&week), (1, Some(at)), "{:?}", rig.said);

    let mut rig = Rig::repo(&[
        ("Cargo.lock", &crates(&[("c", "0.4.0")])),
        ("x/Cargo.lock", &crates(&[("b", "2.0.0")])),
    ]);
    rig.write("Cargo.lock", &crates(&[("c", "0.4.1")]));
    rig.write("x/Cargo.lock", &crates(&[("b", "2.1.0")]));
    assert_eq!(rig.waiting(&policy()), (1, Some(ripe(&new()))));
}

/// Anything else wrong with the refresh, and it isn't only a wait.
#[test]
fn a_wait_and_something_else_is_no_wait() {
    let mut rig = Rig::repo(&[
        ("Cargo.lock", &crates(&[("c", "0.4.0")])),
        ("x/Cargo.lock", &crates(&[("unknown", "1.0.0")])),
    ]);
    rig.write("Cargo.lock", &crates(&[("c", "0.4.1")]));
    rig.write("x/Cargo.lock", &crates(&[("unknown", "1.1.0")]));
    assert_eq!(rig.waiting(&policy()), (1, None));
}

/// Paths come through whole: a space, a tab, a quote, non-ASCII.
#[test]
fn paths_come_through_whole() {
    for d in ["my dir", "caf\u{e9}", "a\t\"b"] {
        let path = format!("{d}/Cargo.lock");
        let mut rig = Rig::repo(&[(&path, &crates(&[("b", "2.0.0"), ("c", "0.4.0")]))]);
        rig.write(&path, &crates(&[("b", "2.1.0"), ("c", "0.4.1")]));
        assert_eq!(rig.lock_age(), 0, "{d}: {:?}", rig.said);
        assert_eq!(
            rig.versions(&path),
            map(&[("b", "2.1.0"), ("c", "0.4.0")]),
            "{d}"
        );
    }
}

/// A git that can't say what changed (no base commit here) fails it.
#[test]
fn no_base_commit_fails_it() {
    let other = Rig::repo(&[]);
    let mut rig = Rig::bare();
    git(
        rig.path(),
        &[
            "remote",
            "set-url",
            "origin",
            other.origin.path().to_str().unwrap(),
        ],
    );
    rig.write("Cargo.lock", &crates(&[("c", "0.4.1")]));
    assert_eq!(rig.lock_age(), 1);
    assert!(
        rig.said.iter().any(|l| l.contains("git diff")),
        "{:?}",
        rig.said
    );
}

/// A version the base had from another source isn't taken as checked: here
/// the base's f 1.0.0 was a git dependency, the refresh's is from crates.io.
#[test]
fn the_same_version_from_elsewhere_is_checked() {
    let mut rig = Rig::repo(&[(
        "Cargo.lock",
        &cargo_lock(&[
            ("b", "2.0.0", Some(CRATES), &[]),
            ("f", "1.0.0", Some("git+https://example.org/f#abc"), &[]),
        ]),
    )]);
    rig.w.crate_("f", &[("0.9.0", old()), ("1.0.0", new())]);
    rig.write("Cargo.lock", &crates(&[("b", "2.1.0"), ("f", "1.0.0")]));
    assert_eq!(rig.lock_age(), 0, "{:?}", rig.said);
    assert_eq!(
        rig.versions("Cargo.lock"),
        map(&[("b", "2.1.0"), ("f", "0.9.0")])
    );
}

/// One that can't go back at all fails the refresh; so does a crate from a
/// registry whose publish times it can't read, and a registry that doesn't
/// answer.
#[test]
fn what_cant_be_checked_or_taken_back_fails_it() {
    let mut rig = Rig::repo(&[("Cargo.lock", &crates(&[("e", "1.0.0")]))]);
    rig.w.crate_("e", &[("1.0.0", old()), ("1.0.1", new())]);
    rig.w.refuse.push(Box::new(|_, name, _| name == "e"));
    rig.write("Cargo.lock", &crates(&[("e", "1.0.1")]));
    assert_eq!(rig.lock_age(), 1);
    assert_eq!(rig.versions("Cargo.lock"), map(&[("e", "1.0.1")]));

    let other = Some("registry+https://example.org/index");
    let mut rig = Rig::repo(&[("Cargo.lock", &cargo_lock(&[("f", "1.0.0", other, &[])]))]);
    rig.write("Cargo.lock", &cargo_lock(&[("f", "1.1.0", other, &[])]));
    assert_eq!(rig.lock_age(), 1);

    let mut rig = Rig::repo(&[("Cargo.lock", &crates(&[("unknown", "1.0.0")]))]);
    rig.write("Cargo.lock", &crates(&[("unknown", "1.1.0")]));
    assert_eq!(rig.lock_age(), 1);
}

/// A lock file the refresh added, still untracked, is checked too.
#[test]
fn a_new_lock_file_is_checked() {
    let mut rig = Rig::repo(&[("Cargo.lock", &crates(&[("b", "2.0.0")]))]);
    rig.write("x/Cargo.lock", &crates(&[("c", "0.4.1")]));
    assert_eq!(rig.lock_age(), 0, "{:?}", rig.said);
    assert_eq!(rig.versions("x/Cargo.lock"), map(&[("c", "0.4.0")]));
}

/// The newest older version old enough, in one go: not one still too new.
#[test]
fn it_skips_older_ones_still_too_new() {
    let mut rig = Rig::repo(&[("Cargo.lock", &crates(&[("z", "1.0.0")]))]);
    rig.w
        .crate_("z", &[("1.0.0", old()), ("1.1.0", new()), ("1.2.0", new())]);
    rig.write("Cargo.lock", &crates(&[("z", "1.2.0")]));
    assert_eq!(rig.lock_age(), 1, "{:?}", rig.said);
    let ran: Vec<String> = rig.w.ran.iter().map(|(c, _)| c.join(" ")).collect();
    assert_eq!(ran, ["cargo update --quiet -p z@1.2.0 --precise 1.0.0"]);
}

/// A lock file the refresh changed but whose text is the base's (its mode,
/// say) has nothing to wait for: that fails it.
#[test]
fn the_bases_text_with_nothing_too_young_fails_it() {
    use std::os::unix::fs::PermissionsExt;
    let mut rig = Rig::repo(&[("Cargo.lock", &crates(&[("b", "2.0.0")]))]);
    let file = rig.path().join("Cargo.lock");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(rig.waiting(&policy()), (1, None));
}
