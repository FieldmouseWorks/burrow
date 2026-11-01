//! package-lock.json: made again from nothing with --before at the cutoff,
//! and every new release checked after.

use jiff::tz::{Offset, TimeZone};
use jiff::{SignedDuration, Timestamp};
use serde_json::json;

use crate::rig::*;

fn days(n: i64) -> SignedDuration {
    SignedDuration::from_hours(n * 24)
}

/// npm's before as npm prints it, in a zone `hours` off UTC.
fn js_date(at: Timestamp, minutes: i32) -> String {
    let zone = TimeZone::fixed(Offset::from_seconds(minutes * 60).unwrap());
    at.to_zoned(zone)
        .strftime("%a %b %d %Y %H:%M:%S GMT%z (Somewhere)")
        .to_string()
}

fn before(days_ago: i64) -> std::collections::BTreeMap<String, String> {
    map(&[("before", &js_date(now() - days(days_ago), 0))])
}

fn left_pad(version: &str) -> String {
    npm_lock(&[("node_modules/left-pad", version, None)])
}

/// A base with left-pad at `base`, refreshed to `refresh`, that npm makes
/// as `makes`.
fn refreshed(base: &str, refresh: &str, makes: &str) -> Rig {
    let mut rig = Rig::repo(&[("package-lock.json", &left_pad(base))]);
    rig.write("package-lock.json", &left_pad(refresh));
    rig.w.resolves.lock = Some(left_pad(makes));
    rig
}

#[test]
fn made_again_under_the_cutoff() {
    let lock = |lp, k| {
        npm_lock(&[
            ("node_modules/left-pad", lp, None),
            ("node_modules/@s/k", k, None),
        ])
    };
    let mut rig = Rig::repo(&[("web/package-lock.json", &lock("1.0.0", "2.0.0"))]);
    rig.write("web/package-lock.json", &lock("1.2.0", "2.0.1"));
    rig.w.resolves.lock = Some(lock("1.1.0", "2.0.1"));
    assert_eq!(rig.lock_age(), 0, "{:?}", rig.said);
    let (cmd, cwd) = rig.w.installs();
    assert_eq!(cwd, "web", "{cmd:?}");
    assert_eq!(rig.w.before(), now() - AGE);
    assert!(rig.read("web/package-lock.json").contains("\"1.1.0\""));
}

/// Made again, it comes out as the base's: the refresh waits for its first
/// too young release, as Cargo's does. npm's own longer age for the
/// directory is the one waited for; its fixed before is no wait at all, as
/// nothing then gets past it.
#[test]
fn as_the_bases_waits_for_the_first_too_young() {
    let mut rig = refreshed("1.1.0", "1.2.0", "1.1.0");
    assert_eq!(
        rig.waiting(&policy()),
        (1, Some(ripe(&new()))),
        "{:?}",
        rig.said
    );
    rig.w.npm_package("left-pad", &[("1.3.0", mid())]);
    rig.write("package-lock.json", &left_pad("1.3.0"));
    rig.w.npmrc = map(&[("min-release-age", "30")]);
    assert_eq!(rig.waiting(&policy()), (1, Some(ripe(&mid()) + days(27))));
    rig.w.npmrc = before(30);
    rig.write("package-lock.json", &left_pad("1.3.0"));
    assert_eq!(rig.waiting(&policy()), (1, None));
}

/// A fixed before later than the age's cutoff still holds for good what's
/// published after it: a refresh with only such releases is no wait, one
/// with a release before it too waits for that one, and so does every lock
/// file: one in a directory whose own before holds it for good is no wait
/// at all.
#[test]
fn a_later_before_still_holds_what_comes_after_it() {
    let two = ago(days(2)); // before yesterday, younger than 3 days
    let mut rig = refreshed("1.1.0", "1.2.0", "1.1.0");
    rig.w.npm_package("left-pad", &[("1.4.0", two.clone())]);
    rig.w.npmrc = before(1);
    assert_eq!(rig.waiting(&policy()), (1, None));

    let lock = |lp, k| {
        npm_lock(&[
            ("node_modules/left-pad", lp, None),
            ("node_modules/@s/k", k, None),
        ])
    };
    let mut rig = Rig::repo(&[("package-lock.json", &lock("1.1.0", "2.0.0"))]);
    rig.w.npm_package("@s/k", &[("2.1.0", two.clone())]);
    rig.w.npmrc = before(1);
    rig.write("package-lock.json", &lock("1.2.0", "2.1.0"));
    rig.w.resolves.lock = Some(lock("1.1.0", "2.0.0"));
    assert_eq!(rig.waiting(&policy()), (1, Some(ripe(&two))));

    let mut rig = Rig::repo(&[
        ("package-lock.json", &left_pad("1.1.0")),
        ("web/package-lock.json", &left_pad("1.1.0")),
    ]);
    rig.w.npm_package("left-pad", &[("1.4.0", two)]);
    rig.w.resolves.lock = Some(left_pad("1.1.0"));
    let again = |rig: &mut Rig| {
        rig.write("package-lock.json", &left_pad("1.4.0"));
        rig.write("web/package-lock.json", &left_pad("1.2.0"));
        rig.waiting(&policy())
    };
    assert_eq!(again(&mut rig), (1, Some(ripe(&new()))));
    rig.w.npmrc_in.insert("web".into(), before(1));
    assert_eq!(again(&mut rig), (1, None));
    rig.w.npmrc_in.clear();
    rig.w.npmrc_in.insert(".".into(), before(1));
    assert_eq!(again(&mut rig), (1, Some(ripe(&new()))));
}

/// npm can't resolve under the cutoff: the refresh fails, it isn't retried
/// without.
#[test]
fn npm_that_cant_resolve_fails_it() {
    let mut rig = refreshed("1.0.0", "1.2.0", "1.1.0");
    rig.w.resolves.code = 1;
    assert_eq!(rig.lock_age(), 1);
    let installs = rig
        .w
        .ran
        .iter()
        .filter(|(c, _)| c[..2] == ["npm", "install"])
        .count();
    assert_eq!(installs, 1, "{:?}", rig.w.ran);
}

/// npm's own config for the directory holds when it's stricter than the
/// age (--before would override it): in what npm resolves under, and in
/// what's checked after.
#[test]
fn npms_own_stricter_cutoff_holds() {
    for (npmrc, days_ago) in [
        (before(30), 30),
        (map(&[("min-release-age", "30")]), 30),
        (before(1), 3),
    ] {
        let mut rig = refreshed("1.0.0", "1.2.0", "1.1.0"); // 20 days old
        rig.w.npmrc = npmrc.clone();
        assert_eq!(rig.lock_age(), i32::from(days_ago == 30), "{npmrc:?}");
        assert_eq!(rig.w.before(), now() - days(days_ago), "{npmrc:?}");
    }
}

/// npm prints before in its local time zone: the same instant, whatever the
/// offset; and an npm without min-release-age (npm 10) says undefined. A
/// before it can't read fails the refresh before npm runs.
#[test]
fn npms_before_is_read_in_any_zone() {
    for minutes in [-8 * 60, 0, 5 * 60 + 30] {
        let mut rig = refreshed("1.0.0", "1.2.0", "1.0.1");
        rig.w.npm_package("left-pad", &[("1.0.1", ago(days(31)))]);
        rig.w.npmrc = map(&[
            ("before", &js_date(now() - days(30), minutes)),
            ("min-release-age", "undefined"),
        ]);
        assert_eq!(rig.lock_age(), 0, "{minutes}: {:?}", rig.said);
        assert_eq!(rig.w.before(), now() - days(30), "{minutes}");
    }
    let mut rig = refreshed("1.0.0", "1.2.0", "1.1.0");
    rig.w.npmrc = map(&[("before", "next tuesday")]);
    assert_eq!(rig.lock_age(), 1);
    assert!(!rig.w.ran.iter().any(|(c, _)| c[..2] == ["npm", "install"]));
}

/// npm's second pass fails, or takes something too new; npm resolved to
/// something too new anyway, or from somewhere else; or to just what the
/// base had: the refresh fails.
#[test]
fn what_npm_makes_is_checked() {
    let mut rig = refreshed("1.0.0", "1.2.0", "1.1.0");
    rig.w.resolves.second_code = 1;
    assert_eq!(rig.lock_age(), 1);
    let mut rig = refreshed("1.0.0", "1.2.0", "1.1.0");
    rig.w.resolves.second = Some(left_pad("1.2.0"));
    assert_eq!(rig.lock_age(), 1);

    for makes in [
        left_pad("1.2.0"),
        npm_lock(&[(
            "node_modules/left-pad",
            "1.1.0",
            Some("https://example.org/lp.tgz"),
        )]),
        left_pad("1.0.0"),
    ] {
        let mut rig = refreshed("1.0.0", "1.2.0", "1.0.0");
        rig.w.resolves.lock = Some(makes.clone());
        assert_eq!(rig.lock_age(), 1, "{makes}");
    }
}

/// A version the base had from another tarball isn't taken as checked.
#[test]
fn the_same_version_from_elsewhere_is_checked() {
    let from_elsewhere = npm_lock(&[(
        "node_modules/left-pad",
        "1.2.0",
        Some("https://example.org/lp.tgz"),
    )]);
    let mut rig = Rig::repo(&[("package-lock.json", &from_elsewhere)]);
    rig.write("package-lock.json", &left_pad("1.2.0"));
    rig.w.resolves.lock = Some(left_pad("1.2.0"));
    assert_eq!(rig.lock_age(), 1);
}

/// A git dependency's new commit is taken as it is, as Cargo's are; its
/// registry dependencies are still checked.
#[test]
fn a_git_dependencys_commit_is_taken_as_it_is() {
    let git = |c: &str| format!("git+ssh://git@github.com/o/g.git#{}", c.repeat(40));
    let lock = |c: &str, lp| {
        npm_lock(&[
            ("node_modules/g", "1.0.0", Some(&git(c))),
            ("node_modules/left-pad", lp, None),
        ])
    };
    let mut rig = Rig::repo(&[("package-lock.json", &lock("a", "1.0.0"))]);
    rig.write("package-lock.json", &lock("b", "1.2.0"));
    rig.w.resolves.lock = Some(lock("b", "1.1.0"));
    assert_eq!(rig.lock_age(), 0, "{:?}", rig.said);
    rig.write("package-lock.json", &lock("b", "1.2.0"));
    rig.w.resolves.lock = Some(lock("b", "1.2.0"));
    assert_eq!(rig.lock_age(), 1);
}

/// npm 6's lock files have no packages map to check, and a lock file it
/// can't check at all fails the refresh.
#[test]
fn what_it_cant_check_fails_it() {
    let mut rig = refreshed("1.0.0", "1.2.0", "1.1.0");
    rig.w.resolves.lock = Some(
        json!({"lockfileVersion": 1, "dependencies": {"left-pad": {"version": "1.1.0"}}})
            .to_string(),
    );
    assert_eq!(rig.lock_age(), 1);
    let mut rig = Rig::repo(&[("yarn.lock", "# old\n")]);
    rig.write("yarn.lock", "# new\n");
    assert_eq!(rig.lock_age(), 1);
}

/// What the base had, from the same place, counts as checked, however new.
#[test]
fn what_the_base_had_counts_as_checked() {
    let lock = |lp, k| {
        npm_lock(&[
            ("node_modules/left-pad", lp, None),
            ("node_modules/@s/k", k, None),
        ])
    };
    let mut rig = Rig::repo(&[("package-lock.json", &lock("1.2.0", "2.0.0"))]);
    rig.write("package-lock.json", &lock("1.2.0", "2.0.1"));
    rig.w.resolves.lock = Some(lock("1.2.0", "2.0.1"));
    assert_eq!(rig.lock_age(), 0, "{:?}", rig.said);
}
