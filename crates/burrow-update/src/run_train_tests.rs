//! Whole runs in a train repo, where what a run would push goes up as its
//! PR (train.rs): proposed, ridden, replaced when red, and main itself
//! going live with nothing put up.

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
            "live and confirmed: nixpkgs aaaaaaa -> ccccccc (up as https://github.com/sam/homelab/pull/8)".into()
        )
    );
    assert_eq!(m.main(), main, "nothing but the train moves main");
    let head = m.origin(&["rev-parse", &format!("refs/heads/{BRANCH}")]);
    assert_eq!(m.recorded("proposed"), Some(head.clone()));
    assert_eq!(
        gh.asked("POST")[0].2.as_ref().unwrap()["head"],
        json!(BRANCH)
    );
    fs::remove_file(m.path("calls")).unwrap();
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
fn its_open_pr_is_ridden_until_it_lands() {
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
            "nothing new builds; up to date (riding #8)".into()
        )
    );
    assert_eq!(m.status()["behind"], json!(8));
    assert_eq!(m.head(), head);
    assert_eq!(m.running(), live);
    assert!(!m.kept("nix.log").contains("flake update"), "nothing fresh");
}

#[test]
fn a_red_pr_of_its_own_gets_a_fresh_bump_as_its_head() {
    let (m, red_head) = proposed();
    m.upstream("nixpkgs", &sha('d'));
    let mut gh = open_at(&red_head, &[true]);
    assert_eq!(once(&m.cfg, &mut gh, false), 0);
    let head = m.origin(&["rev-parse", &format!("refs/heads/{BRANCH}")]);
    assert_ne!(head, red_head);
    assert_eq!(head, m.head());
    assert_eq!(
        said(&m),
        (
            "ok".into(),
            format!(
                "live and confirmed: nixpkgs aaaaaaa -> ddddddd (up as https://github.com/sam/homelab/pull/8, in place of its red head {})",
                &red_head[..7]
            )
        )
    );
    assert!(running_lock(&m).contains(&sha('d')));
    // Up before it's confirmed.
    let calls = m.did("calls");
    assert!(calls.ends_with("confirm\n"), "{calls}");
    assert_eq!(gh.asked("PATCH").len(), 1);
    assert_eq!(
        gh.asked("POST").len(),
        2,
        "red when it began, and once more when it went up"
    );
}

#[test]
fn a_fresh_bump_its_pr_wont_take_goes_straight_back() {
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
            "its PR #8 isn't red any more, so it stays as it is; reverted".into()
        )
    );
    assert_eq!(m.running(), live);
    assert!(m.did("calls").ends_with("switch --abort\n"));
    assert!(!m.did("calls").contains("confirm"));
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
