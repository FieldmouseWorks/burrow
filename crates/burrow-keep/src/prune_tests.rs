use super::*;
use burrow_github::fake::{Fake, ok};
use serde_json::{Value, json};

const PREFIX: &str = "flake-kept-flake-Linux-leg-1-of-4-";
const OLD: &str = "1111111111111111111111111111111111111111";
const MID: &str = "2222222222222222222222222222222222222222";
const NEW: &str = "3333333333333333333333333333333333333333";
const FORK: &str = "4444444444444444444444444444444444444444";

fn leg(sha: &str) -> Leg<'_> {
    Leg {
        repo: "o/r",
        branch: "main",
        prefix: PREFIX,
        sha,
    }
}

fn entry(id: u64, key: &str, git_ref: &str) -> Value {
    json!({"id": id, "key": key, "ref": git_ref, "size_in_bytes": 1})
}

/// The cache's listing: an entry per commit, its id the commit's first
/// digit, under main; and others the leg isn't to touch.
fn listing(shas: &[&str]) -> Value {
    let mut caches: Vec<Value> = shas
        .iter()
        .map(|s| {
            entry(
                u64::from(s.as_bytes()[0] - b'0'),
                &format!("{PREFIX}{s}"),
                "refs/heads/main",
            )
        })
        .collect();
    // Another leg's, a PR's, and one whose key only starts like this leg's.
    caches.push(entry(
        91,
        &format!("flake-kept-flake-Linux-leg-2-of-4-{OLD}"),
        "refs/heads/main",
    ));
    caches.push(entry(92, &format!("{PREFIX}{OLD}"), "refs/pull/7/merge"));
    caches.push(entry(93, &format!("{PREFIX}{OLD}-x"), "refs/heads/main"));
    json!({"total_count": caches.len(), "actions_caches": caches})
}

/// The history OLD, MID, NEW, one after another on main, and FORK beside
/// it: how `base`'s commit stands to `head`'s, as GitHub's compare says.
fn status(base: &str, head: &str) -> &'static str {
    let at = |s: &str| [OLD, MID, NEW].iter().position(|x| *x == s);
    match (at(base), at(head)) {
        (Some(b), Some(h)) if b < h => "ahead",
        (Some(b), Some(h)) if b > h => "behind",
        (Some(_), Some(_)) => "identical",
        _ => "diverged",
    }
}

fn github(own: &str, listings: &[&[&str]]) -> Fake {
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/o/r/actions/caches?",
        listings.iter().map(|l| ok(200, listing(l), &[])).collect(),
    );
    for other in [OLD, MID, NEW, FORK] {
        gh.on(
            "GET",
            &format!("repos/o/r/compare/{other}...{own}"),
            vec![ok(200, json!({"status": status(other, own)}), &[])],
        );
    }
    gh.on(
        "DELETE",
        "repos/o/r/actions/caches/",
        vec![ok(204, Value::Null, &[])],
    );
    gh
}

fn deleted(gh: &Fake) -> Vec<String> {
    gh.asked("DELETE").iter().map(|a| a.1.clone()).collect()
}

#[test]
fn a_push_s_save_deletes_its_ancestors_entries_for_its_leg_and_nothing_else() {
    let mut gh = github(NEW, &[&[OLD, MID, NEW, FORK]]);
    let pruned = leg(NEW).prune(&mut gh).unwrap();
    assert_eq!(
        pruned,
        Pruned {
            deleted: vec![format!("{PREFIX}{OLD}"), format!("{PREFIX}{MID}")],
            own: false,
        }
    );
    assert_eq!(
        deleted(&gh),
        ["repos/o/r/actions/caches/1", "repos/o/r/actions/caches/2"]
    );
    // The listing asked for this leg's entries under main only.
    let listed = &gh.asked("GET")[0].1;
    assert!(
        listed.contains(&format!("key={PREFIX}")) && listed.contains("ref=refs%2Fheads%2Fmain"),
        "{listed}"
    );
}

#[test]
fn an_older_push_finishing_late_deletes_its_own_entry_not_the_newer_one() {
    let mut gh = github(MID, &[&[OLD, MID, NEW]]);
    let pruned = leg(MID).prune(&mut gh).unwrap();
    assert_eq!(
        pruned,
        Pruned {
            deleted: vec![format!("{PREFIX}{OLD}"), format!("{PREFIX}{MID}")],
            own: true,
        }
    );
    assert_eq!(
        deleted(&gh),
        ["repos/o/r/actions/caches/1", "repos/o/r/actions/caches/2"]
    );
}

#[test]
fn an_older_push_finishing_late_doesnt_save_at_all() {
    let mut gh = github(MID, &[&[OLD, NEW]]);
    assert_eq!(
        leg(MID).superseded(&mut gh).unwrap(),
        Some(format!("{PREFIX}{NEW}"))
    );
    // The newest push, an unrelated one, or none: it saves.
    for (own, there) in [(NEW, &[OLD, MID][..]), (NEW, &[FORK][..]), (OLD, &[][..])] {
        let mut gh = github(own, &[there]);
        assert_eq!(
            leg(own).superseded(&mut gh).unwrap(),
            None,
            "{own} {there:?}"
        );
    }
    assert!(deleted(&gh).is_empty());
}

#[test]
fn nothing_goes_until_the_leg_s_own_save_is_listed() {
    // Listed on the third read: it waited, then pruned.
    let mut gh = github(NEW, &[&[OLD], &[OLD], &[OLD, NEW]]);
    assert_eq!(
        leg(NEW).prune(&mut gh).unwrap().deleted,
        [format!("{PREFIX}{OLD}")]
    );
    assert_eq!(gh.paused, [10, 10]);
    // Never listed: its predecessor stays, the one PR legs still restore.
    let mut gh = github(NEW, &[&[OLD, MID]]);
    let e = leg(NEW).prune(&mut gh).unwrap_err();
    assert!(e.contains("isn't listed"), "{e}");
    assert!(deleted(&gh).is_empty());
    assert_eq!(gh.paused.len(), 11);
}

#[test]
fn an_entry_whose_commit_github_doesnt_know_stays() {
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/o/r/actions/caches?",
        vec![ok(200, listing(&[OLD, NEW]), &[])],
    );
    gh.on(
        "GET",
        &format!("repos/o/r/compare/{OLD}...{NEW}"),
        vec![ok(404, json!({"message": "Not Found"}), &[])],
    );
    assert_eq!(leg(NEW).prune(&mut gh).unwrap(), Pruned::default());
    assert!(deleted(&gh).is_empty());
}
