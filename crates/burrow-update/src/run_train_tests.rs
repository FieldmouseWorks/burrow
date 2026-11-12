//! Whole runs in a train repo, where what a run would push goes up as its
//! PR (train.rs): proposed and never switched to, replaced when stuck, and
//! main itself going live whatever PR is open.

use burrow_github::fake::ok;

use super::*;
use crate::testing::{BRANCH, checked, listed};

#[test]
fn in_a_train_main_goes_live_with_nothing_put_up() {
    let mut m = machine();
    m.cfg.train = Some(train());
    let main = merged(&m);
    let none = || {
        let mut gh = Fake::default();
        gh.on(
            "GET",
            "repos/sam/homelab/pulls?",
            vec![ok(200, json!([]), &[])],
        );
        gh
    };
    let mut gh = none();
    assert_eq!(once(&m.cfg, &mut gh, false), 0);
    assert_eq!(
        said(&m),
        (
            "ok".into(),
            format!("live and confirmed: main {}", &main[..7])
        )
    );
    assert_eq!(m.recorded("deployed-main"), Some(main.clone()));
    assert!(gh.asked("POST").is_empty() && gh.asked("PATCH").is_empty());
    assert_eq!(m.main(), main);
    // The next run finds main running.
    assert_eq!(once(&m.cfg, &mut none(), false), 0);
    assert_eq!(said(&m), ("current".into(), "up to date".into()));
}

/// A train repo, where its last run proposed `nixpkgs` at `c` as #8.
fn proposed() -> (Machine, String) {
    let mut m = machine();
    m.cfg.train = Some(train());
    m.upstream("nixpkgs", &sha('c'));
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/sam/homelab/pulls?",
        vec![ok(200, json!([]), &[])],
    );
    gh.on(
        "POST",
        "repos/sam/homelab/pulls",
        vec![ok(
            201,
            json!({"html_url": "https://github.com/sam/homelab/pull/8"}),
            &[],
        )],
    );
    let main = m.main();
    assert_eq!(once(&m.cfg, &mut gh, false), 0);
    assert_eq!(
        said(&m),
        (
            "ok".into(),
            "tested, not live (it goes live after it lands): nixpkgs aaaaaaa -> ccccccc (up as https://github.com/sam/homelab/pull/8)".into()
        )
    );
    assert!(!switched(&m), "a bump is never switched to");
    assert_eq!(m.recorded("deployed"), None);
    assert_eq!(m.main(), main, "nothing but the train moves main");
    let head = m.origin(&["rev-parse", &format!("refs/heads/{BRANCH}")]);
    assert_eq!(m.recorded("proposed"), Some(head.clone()));
    assert_eq!(
        gh.asked("POST")[0].2.as_ref().unwrap()["head"],
        json!(BRANCH)
    );
    let _ = fs::remove_file(m.path("calls"));
    (m, head)
}

/// GitHub with #8 open at `head`, its checks `red` or not each time it's
/// asked, and its PATCH answered.
fn open_at(head: &str, red: &[bool]) -> Fake {
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/sam/homelab/pulls?",
        vec![ok(
            200,
            listed(&[(8, head.into(), json!("sam/homelab"))]),
            &[],
        )],
    );
    let run = |red: bool| json!({"__typename": "CheckRun", "status": "COMPLETED", "conclusion": if red { "FAILURE" } else { "SUCCESS" }});
    gh.on(
        "POST",
        "graphql",
        red.iter()
            .map(|&r| ok(200, checked(head, &[run(r)], false), &[]))
            .collect(),
    );
    gh.on(
        "PATCH",
        "repos/sam/homelab/pulls/8",
        vec![ok(
            200,
            json!({"html_url": "https://github.com/sam/homelab/pull/8"}),
            &[],
        )],
    );
    gh
}

#[test]
fn its_open_pr_is_not_ridden_and_brings_in_nothing_newer() {
    let (m, head) = proposed();
    let live = m.running();
    m.upstream("nixpkgs", &sha('d'));
    fs::remove_file(m.home("nix.log")).unwrap();
    let mut gh = open_at(&head, &[false]);
    assert_eq!(once(&m.cfg, &mut gh, false), 0);
    assert_eq!(
        said(&m),
        (
            "current".into(),
            "up to date (#8 waits for the train)".into()
        )
    );
    assert_eq!(m.status()["behind"], json!(8));
    assert_ne!(m.head(), head, "the PR's head is never checked out to run");
    assert_eq!(m.running(), live);
    assert!(!m.kept("nix.log").contains("flake update"), "nothing fresh");
}

#[test]
fn a_held_bump_never_holds_main_and_the_box_never_runs_a_pin_main_lacks() {
    // #8 is held by a finding about an input's own code. Unrelated changes
    // landed on main since: they go live, without the PR's pin.
    let (m, held) = proposed();
    m.upstream("nixpkgs", &sha('d'));
    let found = || {
        let mut said = checked(&held, &[], false);
        said["data"]["repository"]["pullRequest"]["reviewThreads"] = json!({"nodes": [
            {"isResolved": false, "comments": {"nodes": [{"originalCommit": {"oid": held}}]}}
        ]});
        ok(200, said, &[])
    };
    let open = |gh: &mut Fake| {
        gh.on(
            "GET",
            "repos/sam/homelab/pulls?",
            vec![ok(
                200,
                listed(&[(8, held.clone(), json!("sam/homelab"))]),
                &[],
            )],
        );
    };
    let before = m.running();
    m.landed("system.nix", "unrelated");
    let main = m.main();
    let mut gh = Fake::default();
    open(&mut gh);
    gh.on("POST", "graphql", vec![found()]);
    // The held PR is stuck, but main isn't live yet: main deploys first.
    assert_eq!(once(&m.cfg, &mut gh, false), 0);
    assert_eq!(
        said(&m),
        (
            "ok".into(),
            format!("live and confirmed: main {}", &main[..7])
        )
    );
    assert_ne!(m.running(), before);
    assert!(
        !running_lock(&m).contains(&sha('c')),
        "the held pin never ran"
    );
    assert!(!running_lock(&m).contains(&sha('d')));
    assert_eq!(m.recorded("deployed-main"), Some(main));
    assert!(gh.asked("PATCH").is_empty() && gh.asked("POST").len() == 1);
    assert_eq!(
        m.origin(&["rev-parse", &format!("refs/heads/{BRANCH}")]),
        held,
        "its PR is untouched"
    );
}

#[test]
fn a_red_pr_of_its_own_gets_a_fresh_bump_as_its_head_without_going_live() {
    let (m, red_head) = proposed();
    let live = m.running();
    m.upstream("nixpkgs", &sha('d'));
    let mut gh = open_at(&red_head, &[true, true]);
    assert_eq!(once(&m.cfg, &mut gh, false), 0);
    let head = m.origin(&["rev-parse", &format!("refs/heads/{BRANCH}")]);
    assert_ne!(head, red_head);
    assert_eq!(head, m.head());
    assert_eq!(
        said(&m),
        (
            "ok".into(),
            format!(
                "tested, not live (it goes live after it lands): nixpkgs aaaaaaa -> ddddddd (up as https://github.com/sam/homelab/pull/8, in place of its head {}, which was red)",
                &red_head[..7]
            )
        )
    );
    assert_eq!(m.running(), live);
    assert!(!switched(&m));
    assert_eq!(gh.asked("PATCH").len(), 1);
}

#[test]
fn a_pr_of_its_own_held_by_a_finding_gets_a_fresh_bump_as_its_head() {
    // Green, but a reviewer's finding on its head is open: the steward won't
    // land it, so the fix for the finding is a fresh bump in its place.
    let (m, held) = proposed();
    let live = m.running();
    m.upstream("nixpkgs", &sha('d'));
    let found = || {
        let mut said = checked(&held, &[], false);
        said["data"]["repository"]["pullRequest"]["reviewThreads"] = json!({"nodes": [
            {"isResolved": false, "comments": {"nodes": [{"originalCommit": {"oid": held}}]}}
        ]});
        ok(200, said, &[])
    };
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/sam/homelab/pulls?",
        vec![ok(
            200,
            listed(&[(8, held.clone(), json!("sam/homelab"))]),
            &[],
        )],
    );
    gh.on("POST", "graphql", vec![found(), found()]);
    gh.on(
        "PATCH",
        "repos/sam/homelab/pulls/8",
        vec![ok(
            200,
            json!({"html_url": "https://github.com/sam/homelab/pull/8"}),
            &[],
        )],
    );
    assert_eq!(once(&m.cfg, &mut gh, false), 0);
    let head = m.origin(&["rev-parse", &format!("refs/heads/{BRANCH}")]);
    assert_ne!(head, held);
    assert_eq!(head, m.head());
    assert_eq!(
        said(&m),
        (
            "ok".into(),
            format!(
                "tested, not live (it goes live after it lands): nixpkgs aaaaaaa -> ddddddd (up as https://github.com/sam/homelab/pull/8, in place of its head {}, which was held by an open finding on it)",
                &held[..7]
            )
        )
    );
    assert_eq!(m.running(), live);
    assert_eq!(gh.asked("PATCH").len(), 1);
}

#[test]
fn a_fresh_bump_its_pr_wont_take_changes_nothing_live() {
    let (m, red_head) = proposed();
    let live = m.running();
    m.upstream("nixpkgs", &sha('d'));
    // Red when the run began, green (a rerun) by the time it's up.
    let mut gh = open_at(&red_head, &[true, false]);
    assert_eq!(once(&m.cfg, &mut gh, false), 0);
    assert_eq!(
        said(&m),
        (
            "held".into(),
            "nixpkgs aaaaaaa -> ddddddd built and passed, but its PR didn't go up; the next run tries again".into()
        )
    );
    assert_eq!(m.running(), live);
    assert!(!switched(&m));
    assert_eq!(
        m.origin(&["rev-parse", &format!("refs/heads/{BRANCH}")]),
        red_head
    );
}

#[test]
fn a_fresh_bump_short_of_an_input_leaves_its_red_pr_alone() {
    let (m, red_head) = proposed();
    let live = m.running();
    m.upstream("nixpkgs", &sha('d'));
    fs::write(m.home("unfetchable-burrow"), "").unwrap();
    let mut gh = open_at(&red_head, &[true]);
    assert_eq!(once(&m.cfg, &mut gh, false), 0);
    assert_eq!(
        said(&m),
        (
            "held".into(),
            "its PR #8 is red, and a fresh bump is short of burrow; holding for the Claude jobs on it"
                .into()
        )
    );
    assert_eq!(m.running(), live);
    assert!(!switched(&m));
    assert_eq!(
        m.origin(&["rev-parse", &format!("refs/heads/{BRANCH}")]),
        red_head
    );
}

#[test]
fn main_not_building_with_a_pr_open_never_falls_back_to_a_bump() {
    // A newer burrow would build, but it isn't on main: main's own failure
    // is main's to fix, and nothing unmerged goes live or is recorded as main.
    let (m, head) = proposed();
    let live = m.running();
    m.landed("system.nix", "unrelated");
    m.upstream("burrow", &sha('e'));
    fs::write(m.home("build-breaks"), sha('b')).unwrap();
    let mut gh = open_at(&head, &[false]);
    assert_eq!(once(&m.cfg, &mut gh, false), 1);
    assert_eq!(
        said(&m),
        ("failed".into(), "main itself doesn't build".into())
    );
    assert_eq!(m.running(), live);
    assert!(!switched(&m));
    assert_eq!(m.recorded("deployed-main"), None);
    assert!(jobs(&m).contains("doesn't build; the log is "));
    assert!(gh.asked("PATCH").is_empty());
}

#[test]
fn the_first_main_deploy_may_leave_a_bump_the_old_updater_put_live() {
    // The old updater's bump (newer nixpkgs than main) is live; main's
    // system is a downgrade from it. Once, that is allowed; after, it isn't.
    let (m, _) = proposed();
    m.landed("system.nix", "unrelated");
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/sam/homelab/pulls?",
        vec![ok(200, json!([]), &[])],
    );
    assert_eq!(once(&m.cfg, &mut gh, false), 0);
    assert!(m.did("calls").contains("--allow-downgrade"));
    assert!(m.cfg.state.join("main-only").exists());
    m.landed("system.nix", "unrelated again");
    fs::remove_file(m.path("calls")).unwrap();
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/sam/homelab/pulls?",
        vec![ok(200, json!([]), &[])],
    );
    assert_eq!(once(&m.cfg, &mut gh, false), 0);
    assert!(switched(&m));
    assert!(!m.did("calls").contains("--allow-downgrade"));
}
