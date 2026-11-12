//! The drain's dependency ranking: facts read afresh from GitHub's issue
//! dependencies and from `Blocks:` / `Blocked by:` lines, against the
//! stand-ins in [`super::super::sorting_tests`].

use std::fs;

use super::*;
use crate::inbox::deps::{Links, Ref, parse};

const OLD: &str = "2026-10-01T00:00:00Z";
const NEW: &str = "2026-11-01T00:00:00Z";

fn ready(repo: &str, n: u64, labels: &[&str], made: &str, body: &str) -> Value {
    let mut all = vec!["ready"];
    all.extend(labels);
    let mut i = found(repo, n, "sam", &all, made);
    i["body"] = json!(body);
    i
}

/// The drain's order of what the repos' last passes listed, by `repo#n`.
fn drained(b: &Box, lists: &[(&str, Vec<Value>)]) -> Vec<String> {
    for (repo, items) in lists {
        b.pass().note_ready(repo, items).unwrap();
        items.iter().for_each(|i| on_github(b, i));
    }
    order(b)
}

fn order(b: &Box) -> Vec<String> {
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    b.pass()
        .next_ready_ordered(&t)
        .into_iter()
        .map(|c| format!("{}#{}", c.repo, c.n))
        .collect()
}

/// What `sam/game#N` blocks, as the API lists it, in pages.
fn blocks(b: &Box, repo: &str, n: u64, pages: &[&[(&str, u64)]]) {
    let pages: Vec<Vec<Value>> = pages
        .iter()
        .map(|p| {
            p.iter()
                .map(|(r, m)| {
                    json!({"number": m, "repository_url": format!("https://api.github.com/repos/{r}")})
                })
                .collect()
        })
        .collect();
    fs::write(
        b.path(&format!(
            "repos_{}_issues_{n}_dependencies_blocking",
            repo.replace('/', "_")
        )),
        json!(pages).to_string(),
    )
    .unwrap();
}

fn close(b: &Box, repo: &str, n: u64) {
    let f = b.path(&format!("repos_{}_issues_{n}", repo.replace('/', "_")));
    let mut i: Value = serde_json::from_str(&fs::read_to_string(&f).unwrap()).unwrap();
    i["state"] = json!("closed");
    fs::write(f, i.to_string()).unwrap();
}

// A newer blocker of an open ready issue goes before an older high-priority
// one; the API is the source.
#[test]
fn a_newer_blocker_beats_an_older_high_priority_item() {
    let b = Box::new();
    let high = ready("sam/game", 1, &["priority: high"], OLD, "");
    let blocker = ready("sam/web", 2, &[], NEW, "");
    let target = ready("sam/game", 3, &[], NEW, "");
    blocks(&b, "sam/web", 2, &[&[("sam/game", 3)]]);
    let got = drained(
        &b,
        &[("sam/game", vec![high, target]), ("sam/web", vec![blocker])],
    );
    assert_eq!(got, ["sam/web#2", "sam/game#1", "sam/game#3"]);
}

// Without a blocker the plain order holds, and a target that is done stops
// counting, as read from GitHub on this decision.
#[test]
fn a_completed_target_ceases_to_count() {
    let b = Box::new();
    let high = ready("sam/game", 1, &["priority: high"], OLD, "");
    let blocker = ready("sam/web", 2, &[], NEW, "");
    let target = ready("sam/game", 3, &[], NEW, "");
    blocks(&b, "sam/web", 2, &[&[("sam/game", 3)]]);
    let lists = [("sam/game", vec![high, target]), ("sam/web", vec![blocker])];
    assert_eq!(drained(&b, &lists)[0], "sam/web#2");
    close(&b, "sam/game", 3);
    assert_eq!(order(&b)[0], "sam/game#1");
}

// With no dependency API, the bodies say it, in either direction.
#[test]
fn both_explicit_reference_directions_work() {
    for (blocker_body, target_body) in [("Blocks: #3", ""), ("", "- **Blocked by:** #2")] {
        let b = Box::new();
        let high = ready("sam/game", 1, &["priority: high"], OLD, "");
        let blocker = ready("sam/game", 2, &[], NEW, blocker_body);
        let target = ready("sam/game", 3, &[], NEW, target_body);
        let got = drained(&b, &[("sam/game", vec![high, blocker, target])]);
        assert_eq!(got[0], "sam/game#2", "{blocker_body:?} / {target_body:?}");
    }
}

// Repository-qualified and URL references resolve across repositories.
#[test]
fn cross_repository_references_resolve() {
    for line in [
        "Blocks: sam/game#3",
        "Blocks: https://github.com/sam/game/issues/3",
        "Blocks: nobody/else#3, sam/game#3",
    ] {
        let b = Box::new();
        let high = ready("sam/game", 1, &["priority: high"], OLD, "");
        let blocker = ready("sam/web", 2, &[], NEW, line);
        let target = ready("sam/game", 3, &[], NEW, "");
        let got = drained(
            &b,
            &[("sam/game", vec![high, target]), ("sam/web", vec![blocker])],
        );
        assert_eq!(got[0], "sam/web#2", "{line}");
    }
    // `#3` in sam/web's body means sam/web#3, not sam/game's.
    let b = Box::new();
    let high = ready("sam/game", 1, &["priority: high"], OLD, "");
    let blocker = ready("sam/web", 2, &[], NEW, "Blocks: #3");
    let target = ready("sam/game", 3, &[], NEW, "");
    let got = drained(
        &b,
        &[("sam/game", vec![high, target]), ("sam/web", vec![blocker])],
    );
    assert_eq!(got[0], "sam/game#1");
}

// A target past the first page of the API's answer counts.
#[test]
fn the_dependency_api_is_read_to_its_last_page() {
    let b = Box::new();
    let high = ready("sam/game", 1, &["priority: high"], OLD, "");
    let blocker = ready("sam/web", 2, &[], NEW, "");
    let target = ready("sam/game", 3, &[], NEW, "");
    blocks(
        &b,
        "sam/web",
        2,
        &[&[("sam/game", 77)], &[("sam/game", 78)], &[("sam/game", 3)]],
    );
    let got = drained(
        &b,
        &[("sam/game", vec![high, target]), ("sam/web", vec![blocker])],
    );
    assert_eq!(got[0], "sam/web#2");
    assert!(b.log().contains("dependencies/blocking"), "{}", b.log());
}

// A blocker of an open pull request from an urgent branch (a red default
// branch's fix) is ranked first, though nothing in the ready lists is blocked.
#[test]
fn the_blocker_of_a_red_fix_goes_first() {
    let b = Box::new();
    let high = ready("sam/game", 1, &["priority: high"], OLD, "");
    let blocker = ready("sam/web", 2, &[], NEW, "");
    fs::write(
        b.path("repos_sam_game_pulls"),
        json!([
            {"number": 9, "body": "Blocked by: sam/web#2", "head": {"ref": "autoland/red-abc"}},
            {"number": 10, "body": "Blocked by: sam/web#1", "head": {"ref": "feature"}},
        ])
        .to_string(),
    )
    .unwrap();
    let got = drained(&b, &[("sam/game", vec![high]), ("sam/web", vec![blocker])]);
    assert_eq!(got[0], "sam/web#2");
    // The other pull request is not urgent, so blocking it counts for nothing.
    fs::write(
        b.path("repos_sam_game_pulls"),
        json!([{"number": 10, "body": "Blocked by: sam/web#2", "head": {"ref": "feature"}}])
            .to_string(),
    )
    .unwrap();
    assert_eq!(order(&b)[0], "sam/game#1");
}

// Equal in every rule, repository then number decide, whatever order the
// lists came in.
#[test]
fn ties_end_on_repository_and_number() {
    let b = Box::new();
    let one = ready("sam/web", 12, &[], OLD, "");
    let two = ready("sam/web", 9, &[], OLD, "");
    let three = ready("sam/game", 20, &[], OLD, "");
    let forward = drained(
        &b,
        &[
            ("sam/web", vec![one.clone(), two.clone()]),
            ("sam/game", vec![three.clone()]),
        ],
    );
    assert_eq!(forward, ["sam/game#20", "sam/web#9", "sam/web#12"]);
    let backward = drained(
        &b,
        &[("sam/web", vec![two, one]), ("sam/game", vec![three])],
    );
    assert_eq!(forward, backward);
}

// The rules and their order are the live setting's.
#[test]
fn the_live_setting_orders_the_rules() {
    let mut b = Box::new();
    b.cfg.settings = Some(burrow_settings::Config {
        overlay: b.path("overlay.json"),
        allowed: vec!["sam/game".into(), "sam/web".into()],
        pipeline_repos: vec![],
    });
    let high = ready("sam/game", 1, &["priority: high"], OLD, "");
    let blocker = ready("sam/web", 2, &[], NEW, "Blocks: sam/game#3");
    let target = ready("sam/game", 3, &[], NEW, "");
    let lists = [("sam/game", vec![high, target]), ("sam/web", vec![blocker])];
    assert_eq!(drained(&b, &lists)[0], "sam/web#2");
    fs::write(
        b.path("overlay.json"),
        r#"{"drainOrder":["priority","blockers"]}"#,
    )
    .unwrap();
    assert_eq!(drained(&b, &lists)[0], "sam/game#1");
    fs::write(b.path("overlay.json"), r#"{"drainOrder":["age"]}"#).unwrap();
    assert_eq!(drained(&b, &lists)[0], "sam/game#1");
}

#[test]
fn bodies_are_read_mechanically() {
    let r = |repo: &str, n| Ref::new(repo, n);
    let got = parse(
        "Intro\n- Blocks: #1, Sam/Box#2 (https://github.com/sam/web/pull/3).\n**Blocked by:** other/repo#4\nblocked by: #5\nThis blocks: nothing\nBlocks #6\nBlocks: #x",
        "sam/game",
    );
    assert_eq!(
        got,
        Links {
            blocks: [r("sam/game", 1), r("sam/box", 2), r("sam/web", 3)].into(),
            blocked_by: [r("other/repo", 4), r("sam/game", 5)].into(),
        }
    );
}
