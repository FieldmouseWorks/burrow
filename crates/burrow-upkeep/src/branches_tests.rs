//! The branch watch, on the GitHub and git of branches_world.rs.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use super::world::*;
use super::*;
use crate::pass::{DAY, Out, Pass};
use crate::testing::{States, T};

#[test]
fn a_first_look_judges_every_branch_and_deletes_what_has_sat_its_wait() {
    let mut r = fixture();
    let young = r.ago(1.0);
    // The first look judges every branch it may delete, and deletes none
    // before GRACE: its pass wakes when that's over.
    let out = r.look(&["open"], true);
    assert!(out.stuck.is_empty() && r.deleted().is_empty(), "{out:?}");
    let want = [
        "archived",
        "closed",
        "in-main",
        "moved",
        "old",
        "refused",
        "renovate/again",
        "renovate/closed",
        "renovate/x",
        "young",
    ];
    assert_eq!(r.looked(), names(&want));
    assert_eq!(out.wake, Some(T + GRACE));
    let keys: Vec<String> = r.state().as_object().unwrap().keys().cloned().collect();
    let want = [
        "base",
        "branchesBy",
        "branchesEtag",
        "closedEtag",
        "closedSince",
        "tips",
    ];
    assert_eq!(keys, names(&want));
    // The next pass, a kicked one: the unchanged listings cost nothing
    // against the rate limit, and only what's due is judged again. What
    // goes next is the unmerged one from today, STALE_DAYS from its last
    // commit.
    r.now += GRACE;
    let out = r.look(&["open"], false);
    assert_eq!(
        out.stuck.iter().map(|l| l.1.as_str()).collect::<Vec<_>>(),
        ["sam/r: `refused`"]
    );
    assert_eq!(out.stuck[0].0, unkept());
    {
        let w = r.world.borrow();
        assert_eq!((w.fresh.clone(), w.fed.clone()), (vec![304], vec![304]));
    }
    let want = [
        "archived",
        "closed",
        "in-main",
        "old",
        "refused",
        "renovate/closed",
    ];
    assert_eq!(r.looked(), names(&want));
    let want = [
        ("archived", "d"),
        ("closed", "2"),
        ("in-main", "1"),
        ("old", "7"),
        ("renovate/closed", "9"),
    ];
    let want: Vec<(String, String)> = want.iter().map(|(n, t)| (n.to_string(), sha(t))).collect();
    assert_eq!(r.deleted(), want);
    let mut archived: BTreeMap<String, String> = [("archived", "d"), ("old", "7")]
        .iter()
        .map(|(n, t)| (format!("refs/archive/{n}/{}", sha(t)), sha(t)))
        .collect();
    archived.insert(format!("refs/archive/refused/{}", sha("e")), sha("0"));
    assert_eq!(r.world.borrow().archived, archived);
    assert_eq!(out.wake, Some(young + f64::from(STALE_DAYS) * DAY));
    // Nothing changed since: a pass judges nothing (after the one that
    // sees the deletes), and the one GitHub won't archive is all it lists.
    // A full pass tries that one again.
    r.look(&["open"], false);
    assert_eq!(r.lists(&["open"], false), ["sam/r: `refused`"]);
    assert_eq!(r.world.borrow().fresh, [304]);
    assert!(r.looked().is_empty() && r.deleted().is_empty());
    assert_eq!(r.lists(&["open"], true), ["sam/r: `refused`"]);
    assert_eq!(r.looked(), ["refused"]);
}

#[test]
fn a_prs_closing_starts_its_branchs_wait_and_a_reopen_inside_it_keeps_it() {
    let mut r = fixture();
    r.look(&["open"], true);
    r.now += GRACE;
    r.look(&["open"], false);
    // Its PR closing starts the look: an hour later, it goes. Reopened
    // inside that hour, it stays.
    r.close("open", 2, "4");
    let out = r.look(&[], false);
    assert!(r.deleted().is_empty());
    assert_eq!(out.wake, Some(r.now + GRACE));
    r.now += GRACE;
    r.look(&["open"], false);
    assert!(r.deleted().is_empty());
    let open = Tip {
        sha: sha("4"),
        pr: true,
        ..Tip::default()
    };
    assert_eq!(r.tip("open"), open);
    assert_eq!(
        r.state()["tips"]["open"],
        json!({"sha": sha("4"), "pr": true})
    );
    r.look(&[], false);
    r.now += GRACE;
    r.look(&[], false);
    assert_eq!(r.deleted(), [("open".into(), sha("4"))]);
}

#[test]
fn a_pr_opened_and_closed_between_passes_is_read_from_the_closed_ones() {
    let mut r = settled();
    // Every pass reads the PRs closed since the last (their events came as
    // one, or went missing), at no cost while there are none. Not one from
    // a fork's branch of that name, or closed at another tip than the
    // branch's now.
    let now = r.now;
    r.closed_at("renovate/x", 7, "5", now, "eve/r");
    r.close("renovate/again", 8, "b");
    r.look(&[], false);
    assert_eq!(r.world.borrow().fed, [200]);
    assert!(r.looked().is_empty());
    r.close("renovate/x", 9, "5");
    let out = r.look(&[], false);
    assert_eq!(r.world.borrow().fed, [200]);
    assert_eq!(r.looked(), ["renovate/x"]);
    assert_eq!(out.wake, Some(r.now + GRACE));
    r.look(&[], false);
    assert_eq!(r.world.borrow().fed, [304]);
    assert!(r.looked().is_empty());
    r.now += GRACE;
    r.look(&[], false);
    assert_eq!(r.deleted(), [("renovate/x".into(), sha("5"))]);
}

#[test]
fn more_closed_since_the_last_look_than_a_page_holds_judges_every_branch_again() {
    let mut r = settled();
    // Every branch is judged again, from when it was first seen at its
    // tip, since reading on by page number could skip one a reopen moves
    // up a page. Only the one a PR closed at gets a new wait, an hour from
    // that close.
    let young = r.ago(1.0);
    r.branch("paged", "ef", 1, young);
    r.look(&[], false);
    let before: BTreeMap<String, Option<f64>> =
        r.tips().into_iter().map(|(n, t)| (n, t.due)).collect();
    r.now += 1.0;
    r.close("paged", 11, "ef");
    for n in 1000..1100 {
        r.now += 1.0;
        r.close(&format!("gone-{n}"), n, "0");
    }
    assert_eq!(r.lists(&[], false), ["sam/r: `refused`"]);
    assert_eq!(r.world.borrow().fed, [200]);
    assert_eq!(r.looked(), r.tips().into_keys().collect::<Vec<_>>());
    assert_eq!(r.tip("paged").due, Some(r.now - 100.0 + GRACE));
    let after: BTreeMap<String, Option<f64>> =
        r.tips().into_iter().map(|(n, t)| (n, t.due)).collect();
    let without = |m: BTreeMap<String, Option<f64>>| -> BTreeMap<String, Option<f64>> {
        m.into_iter().filter(|(n, _)| n != "paged").collect()
    };
    assert_eq!(without(after), without(before));
    r.look(&[], false);
    assert_eq!(r.world.borrow().fed, [304]);
    assert!(r.looked().is_empty());
    r.now += GRACE;
    r.look(&[], false);
    assert_eq!(r.deleted(), [("paged".into(), sha("ef"))]);
}

#[test]
fn a_close_in_the_second_a_branch_was_first_seen_still_judges_it() {
    let mut r = settled();
    // GitHub's times are whole seconds, the pass's clock isn't: a PR
    // closed in the same second its branch (a bot's, with no wait of its
    // own) was first seen still has it judged, and deleted an hour on.
    let young = r.ago(1.0);
    r.branch("renovate/quick", "e3", 1, young);
    r.now += 0.25;
    r.look(&[], false);
    assert_eq!(r.tip("renovate/quick").due, None);
    r.close("renovate/quick", 15, "e3");
    r.look(&[], false);
    assert_eq!(r.looked(), ["renovate/quick"]);
    assert_eq!(r.tip("renovate/quick").due, Some(r.now + GRACE));
    r.now += GRACE;
    r.look(&[], false);
    assert_eq!(r.deleted(), [("renovate/quick".into(), sha("e3"))]);
    // A PR opened and closed between passes, at the tip an older one
    // closed at too: its hour runs from the newer close.
    r.now += 0.75;
    r.branch("twice", "e4", 1, young);
    let five = r.ago(5.0);
    r.closed_at("twice", 16, "e4", five, "sam/r");
    r.look(&[], false);
    r.now += 1800.0;
    r.close("twice", 17, "e4");
    r.look(&[], false);
    assert_eq!(r.looked(), ["twice"]);
    assert_eq!(r.tip("twice").due, Some(r.now + GRACE));
    r.now += GRACE;
    r.look(&[], false);
    assert_eq!(r.deleted(), [("twice".into(), sha("e4"))]);
}

#[test]
fn a_pass_that_stops_partway_judges_the_closes_it_read_the_same_again() {
    let mut r = settled();
    // Each branch's hour runs from its PR's close, not from the pass, so
    // failing passes can't keep putting it off.
    let young = r.ago(1.0);
    r.branch("first", "f1", 1, young);
    r.branch("second", "f2", 1, young);
    r.look(&[], false);
    r.now += 1.0;
    let closing = r.now;
    r.close("first", 12, "f1");
    r.close("second", 13, "f2");
    r.world
        .borrow_mut()
        .closes
        .insert("second".into(), (502, "Bad Gateway"));
    for _ in 0..3 {
        r.now += 600.0;
        r.look(&[], false);
        assert_eq!(r.looked(), ["first", "second"]);
        assert_eq!(r.tip("first").due, Some(closing + GRACE));
    }
    r.world.borrow_mut().closes.clear();
    r.look(&[], false);
    assert_eq!(r.looked(), ["first", "second"]);
    assert_eq!(r.tip("second").due, Some(closing + GRACE));
    r.look(&[], false);
    assert!(r.looked().is_empty()); // read to the end once: those closes are behind it
    r.now = closing + GRACE;
    r.look(&[], false);
    let want = [
        ("first".to_string(), sha("f1")),
        ("second".to_string(), sha("f2")),
    ];
    assert_eq!(r.deleted(), want);
}

#[test]
fn a_push_to_the_tip_a_pr_closed_at_has_it_go_an_hour_on() {
    let mut r = settled();
    let young = r.ago(1.0);
    r.branch("moved", "f", 1, young);
    r.look(&[], false);
    assert_eq!(r.looked(), ["moved"]);
    assert!(r.deleted().is_empty());
    r.now += GRACE;
    r.look(&[], false);
    assert_eq!(r.deleted(), [("moved".into(), sha("f"))]);
}

#[test]
fn a_delete_github_refuses_is_listed_and_tried_again_on_the_full_pass_alone() {
    let mut r = settled();
    // Listed with what GitHub said, and tried again on the full pass
    // alone: not on every pass until then, nor when the default moves or a
    // PR closes at it. It goes once GitHub lets it.
    let old = r.ago(4.0);
    r.branch("empty", "0", 0, old);
    r.world.borrow_mut().undeletable.insert("empty".into());
    r.look(&[], false);
    r.now += GRACE;
    assert_eq!(r.lists(&[], false), ["sam/r: `empty`", "sam/r: `refused`"]);
    assert_eq!(r.deleted_names(), ["empty"]);
    let out = r.look(&[], false);
    let line = (
        UNDELETED.to_string(),
        "sam/r: `empty`".to_string(),
        REFUSAL.to_string(),
    );
    assert!(out.stuck.contains(&line), "{out:?}");
    assert!(r.deleted().is_empty());
    let young = r.ago(1.0);
    r.branch("main", "bc", 0, young);
    assert_eq!(r.lists(&[], false), ["sam/r: `empty`", "sam/r: `refused`"]);
    assert!(r.deleted().is_empty());
    r.branch("main", "ab", 0, young);
    r.look(&[], false);
    // A PR closed at it days ago, changed now (a label), is in the closed
    // ones.
    let five = r.ago(5.0);
    r.closed_at("empty", 18, "0", five, "sam/r");
    r.world.borrow_mut().closed.last_mut().unwrap().updated = r.now.floor();
    assert_eq!(r.lists(&[], false), ["sam/r: `empty`", "sam/r: `refused`"]);
    assert_eq!(r.world.borrow().fed, [200]);
    assert!(r.deleted().is_empty());
    r.world.borrow_mut().undeletable.clear();
    r.look(&[], true);
    assert_eq!(r.deleted_names(), ["empty"]);
    assert_eq!(r.lists(&[], false), ["sam/r: `refused`"]);
    assert!(!r.tips().contains_key("empty"));
}

#[test]
fn a_stale_branch_is_archived_and_deleted_once_its_wait_ends() {
    let mut r = settled();
    let young = r.ago(1.0) + GRACE;
    r.now = young + f64::from(STALE_DAYS) * DAY;
    r.look(&[], false);
    assert_eq!(r.deleted_names(), ["moved", "young"]);
    let at = format!("refs/archive/young/{}", sha("8"));
    assert_eq!(r.world.borrow().archived.get(&at), Some(&sha("8")));
}

#[test]
fn the_default_moving_judges_every_branch_again() {
    let mut r = settled();
    // At the tip it has sat at: one merged into it since goes as one
    // already in it, unarchived, without waiting out STALE_DAYS. Those with
    // no wait of their own (a bot's) are looked at too.
    let now = r.now.floor();
    r.branch("later", "1a", 2, now);
    r.look(&[], false);
    assert_eq!(
        r.tip("later").due,
        Some(r.now + f64::from(STALE_DAYS) * DAY)
    );
    r.now += GRACE;
    let young = r.ago(1.0);
    r.branch("main", "ba", 0, young);
    r.branch("later", "1a", 0, now);
    r.look(&[], false);
    assert_eq!(r.world.borrow().fresh, [200]);
    assert_eq!(
        r.looked(),
        r.tips()
            .into_keys()
            .chain(["later".into()])
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert!(r.looked().contains(&"renovate/again".to_string()));
    assert_eq!(r.deleted(), [("later".into(), sha("1a"))]);
    assert!(
        !r.world
            .borrow()
            .archived
            .keys()
            .any(|k| k.contains("/later/"))
    );
    r.look(&[], false);
    assert!(r.looked().is_empty()); // the listing without it: the default is where it was
    // A new default: the old one is judged like any other branch, against
    // the new one, though the listing hasn't changed.
    r.branch("dev/trunk", "cd", 1, now);
    r.look(&[], false);
    r.world.borrow_mut().default = "dev/trunk".into();
    r.look(&[], false);
    assert_eq!(r.world.borrow().fresh, [200]);
    assert!(r.looked().contains(&"main".to_string()));
    assert!(!r.tips().contains_key("dev/trunk"));
    r.now += GRACE;
    r.look(&[], false);
    assert_eq!(r.deleted(), [("main".into(), sha("ba"))]);
}

#[test]
fn github_not_listing_the_branches_or_the_closed_prs_changes_nothing() {
    let mut r = settled();
    r.look(&[], false);
    let was = r.state();
    for why in [
        (403, "API rate limit exceeded for installation"),
        (403, "Resource not accessible by integration"),
    ] {
        for feed in [false, true] {
            {
                let mut w = r.world.borrow_mut();
                w.listing = (!feed).then_some(why);
                w.feeding = feed.then_some(why);
            }
            let out = r.look(&[], false);
            assert_eq!(out.stuck.len(), 1, "{why:?}");
            assert_eq!(r.state(), was, "{why:?}");
            assert!(out.failed.is_empty(), "{out:?}");
            // A rate limit is noted for the pass, as any call's is.
            let limited = why.1.contains("rate limit").then_some(T + 7200.0);
            assert_eq!(out.limited, limited, "{why:?}");
        }
    }
}

#[test]
fn github_not_answering_partway_loses_no_branch_or_closed_pr() {
    let mut r = settled();
    r.look(&[], false);
    let was = r.state();
    // What it judged is kept, and the next pass lists both again (no etag
    // kept) and judges the rest. Archiving one then isn't one GitHub
    // wouldn't archive.
    let now = r.now.floor();
    r.branch("new", "0", 1, now);
    r.branch("newer", "b", 1, now);
    r.closed_at("elsewhere", 10, "0", now, "eve/r");
    r.world
        .borrow_mut()
        .compares
        .insert("newer".into(), (502, "Bad Gateway"));
    r.look(&[], false);
    let st = r.state();
    assert_eq!(r.looked(), ["new", "newer"]);
    assert_eq!(
        (&st["branchesEtag"], &st["closedEtag"]),
        (&was["branchesEtag"], &was["closedEtag"])
    );
    assert!(r.tips().contains_key("new") && !r.tips().contains_key("newer"));
    r.world.borrow_mut().compares.clear();
    r.look(&[], false);
    {
        let w = r.world.borrow();
        assert_eq!((w.fresh.clone(), w.fed.clone()), (vec![200], vec![200]));
    }
    assert_eq!(r.looked(), ["newer"]);
    let old = r.ago(4.0);
    r.branch("quiet", "9", 2, old);
    r.look(&[], false);
    r.now += GRACE;
    r.world
        .borrow_mut()
        .archiving
        .insert("quiet".into(), (502, "Bad Gateway"));
    assert_eq!(r.lists(&[], false), ["sam/r: `refused`"]);
    assert!(r.deleted().is_empty());
    r.world.borrow_mut().archiving.clear();
    r.look(&[], false);
    assert_eq!(r.deleted(), [("quiet".into(), sha("9"))]);
}

#[test]
fn a_branch_github_said_no_about_waits_for_a_full_pass() {
    let mut r = settled();
    // Its no, to its closed PRs or its comparison, may be for now (its
    // access) or for good (no history in common with the default, which
    // reads the same). Once GitHub answers, it's judged as any other, from
    // when it was first seen.
    let old = r.ago(4.0);
    r.branch("orphan", "c", 1, old);
    r.branch("shy", "e1", 1, old);
    r.branch("renovate/shy", "e2", 1, old);
    {
        let mut w = r.world.borrow_mut();
        w.compares.insert(
            "orphan".into(),
            (404, "No common ancestor between main and ccccccc"),
        );
        w.compares.insert(
            "shy".into(),
            (403, "Resource not accessible by integration"),
        );
        w.closes.insert(
            "renovate/shy".into(),
            (403, "Resource not accessible by integration"),
        );
    }
    r.look(&[], false);
    let seen = r.now;
    assert!(r.deleted().is_empty());
    let orphan = Tip {
        sha: sha("c"),
        seen: Some(seen),
        retry: true,
        ..Tip::default()
    };
    assert_eq!(r.tip("orphan"), orphan);
    assert!(r.tip("shy").retry && r.tip("renovate/shy").retry);
    r.look(&[], false);
    assert!(r.looked().is_empty());
    let five = r.ago(5.0);
    {
        let mut w = r.world.borrow_mut();
        w.compares.remove("shy");
        w.closes.clear();
    }
    r.closed_at("renovate/shy", 11, "e2", five, "sam/r");
    r.now += GRACE;
    r.look(&[], true);
    assert_eq!(r.looked(), ["orphan", "refused", "renovate/shy", "shy"]);
    let want = [
        ("renovate/shy".to_string(), sha("e2")),
        ("shy".to_string(), sha("e1")),
    ];
    assert_eq!(r.deleted(), want);
    assert!(r.tip("orphan").retry);
    let at = format!("refs/archive/shy/{}", sha("e1"));
    assert!(r.world.borrow().archived.contains_key(&at));
}

#[test]
fn a_first_look_that_stops_partway_keeps_no_base_and_the_next_judges_all() {
    let mut r = settled();
    // It kept no base, so every branch is judged again from when it was
    // first seen (a closed PR's from its close), and what it judged keeps
    // its wait.
    std::fs::write(r.cfg.state_dir.join("repo-sam_r.json"), "{}").unwrap();
    let young = r.ago(1.0);
    r.branch("third", "f3", 1, young);
    r.world
        .borrow_mut()
        .compares
        .insert("third".into(), (502, "Bad Gateway"));
    r.look(&[], false);
    let first = r.now;
    assert!(!r.tips().contains_key("third"));
    assert!(r.state().get("closedSince").is_none());
    r.now += 600.0;
    r.close("renovate/again", 14, "a");
    r.world.borrow_mut().compares.clear();
    r.look(&[], false);
    assert_eq!(r.tip("renovate/again").due, Some(r.now + GRACE));
    assert_eq!(r.tip("refused").seen, Some(first));
    assert_eq!(r.tip("third").seen, Some(r.now));
    assert!(r.state().get("closedSince").is_some());
}

#[test]
fn a_branch_kept_no_longer_is_judged_though_the_listing_hasnt_changed() {
    let mut r = settled();
    r.cfg.branches.as_mut().unwrap().keep.clear();
    r.look(&[], false);
    assert_eq!(r.world.borrow().fresh, [200]);
    assert_eq!(r.looked(), ["gh-pages"]);
    // And a repo's own glob keeps it.
    r.cfg
        .branches
        .as_mut()
        .unwrap()
        .repos
        .insert("sam/r".into(), vec!["gh-*".into()]);
    r.look(&[], false);
    assert_eq!(r.world.borrow().fresh, [200]);
    assert!(!r.tips().contains_key("gh-pages"));
}

#[test]
fn a_dependency_bots_branch_with_no_pr_closed_at_its_tip_is_left_to_it() {
    // Renovate's and Dependabot's alike, however long they've sat; a name
    // that only starts like one is anyone's.
    let mut r = SamR::new();
    let old = r.ago(30.0);
    r.branch("main", "ab", 0, old);
    for (name, tip) in [
        ("renovate/x", "1"),
        ("dependabot/npm/x", "2"),
        ("renovate-x", "3"),
    ] {
        r.branch(name, tip, 1, old);
    }
    r.look(&[], true);
    r.now += GRACE;
    r.look(&[], true);
    assert_eq!(r.deleted(), [("renovate-x".into(), sha("3"))]);
    for name in ["renovate/x", "dependabot/npm/x"] {
        assert_eq!(r.tip(name).due, None, "{name}");
    }
}

#[test]
fn a_closed_feed_with_nothing_in_it_starts_the_next_look_where_this_one_did() {
    let mut r = SamR::new();
    let young = r.ago(1.0);
    r.branch("main", "ab", 0, young);
    r.close("gone", 1, "1");
    r.look(&[], false);
    let since = r.state()["closedSince"].clone();
    assert_eq!(since, json!(r.now));
    r.world.borrow_mut().closed.clear();
    r.now += GRACE;
    r.look(&[], false);
    assert_eq!(r.world.borrow().fed, [200]);
    assert_eq!(r.state()["closedSince"], since);
}

#[test]
fn the_trains_candidates_are_cns_while_its_train_is_on() {
    for (on, want) in [
        (true, vec!["old"]),
        (false, vec!["conary/train/1-2", "old"]),
    ] {
        let mut r = SamR::new();
        r.cfg.settle.defaults.train = on;
        let young = r.ago(1.0);
        r.branch("main", "ab", 0, young);
        r.branch("conary/train/1-2", "e", 0, young);
        r.branch("old", "f", 0, young);
        r.look(&[], false);
        r.now += GRACE; // each goes once it has sat that long
        r.look(&[], false);
        assert_eq!(r.deleted_names(), names(&want), "{on}");
    }
}

#[test]
fn more_than_a_page_of_branches_is_read_whole_with_no_etag_to_ask_by() {
    let mut r = SamR::new();
    let young = r.ago(1.0);
    r.branch("main", "2", 0, young);
    r.world.borrow_mut().protected.insert("main".into());
    // A protected one isn't the steward's to delete, the default too.
    r.branch("kept", "3", 0, young);
    r.world.borrow_mut().protected.insert("kept".into());
    for i in 0..101 {
        r.branch(&format!("b{i:03}"), "1", 0, young);
    }
    r.look(&[], false);
    let st = r.state();
    assert!(st.get("branchesEtag").is_none(), "{st}");
    assert_eq!(st["base"], json!(["main", sha("2")]));
    let tips = r.tips();
    assert_eq!(tips.len(), 101);
    assert!(!tips.contains_key("main") && !tips.contains_key("kept"));
    assert!(tips.contains_key("b100"));
    // Read whole again on the next look, since no etag says it's unchanged.
    r.look(&[], false);
    assert_eq!(r.world.borrow().fresh, [200]);
}

#[test]
fn git_that_cant_run_fails_the_look() {
    let mut r = SamR::new();
    let young = r.ago(1.0);
    r.branch("main", "ab", 0, young);
    r.branch("in-main", "1", 0, young);
    r.look(&[], false);
    r.now += GRACE;
    let mut api = Hub(r.world.clone());
    let mut jobs = States::new("running");
    let mut git = Git {
        bin: "/nonexistent/git".into(),
    };
    let mut p = Pass {
        cfg: &r.cfg,
        repo: "sam/r",
        now: r.now,
        dry: false,
        searched: 0,
        api: &mut api,
        jobs: &mut jobs,
        out: Out::default(),
    };
    let open = BTreeSet::new();
    let repo = Repo {
        default: "main",
        open: &open,
        full: false,
    };
    watch(&mut p, r.cfg.branches.as_ref().unwrap(), &repo, &mut git);
    assert_eq!(p.out.failed.len(), 1, "{:?}", p.out);
    assert!(p.out.failed[0].starts_with("its branches: "), "{:?}", p.out);
    // And it's due again: the next look tries it.
    assert_eq!(r.tip("in-main").due, Some(T + GRACE));
}

#[test]
fn what_settle_and_the_steward_keep_in_the_file_stays() {
    let mut r = SamR::new();
    let young = r.ago(1.0);
    r.branch("main", "ab", 0, young);
    r.branch("feat", "1", 2, young);
    // As settle and the steward leave it, and the branch watch's own as the
    // Python it was wrote it: an open PR's tip with no `seen`, a null etag.
    let kept = json!({"settings": T - 10.0, "app": 7, "labels": ["hold"], "databaseId": 12, "hookId": 9,
                      "hook": "abc", "tips": {"feat": {"sha": sha("1"), "pr": true},
                                              "gone": {"sha": sha("2"), "seen": T - 50.0, "due": T + 5.0}},
                      "branchesEtag": null, "base": ["main", sha("ab")]});
    std::fs::write(r.cfg.state_dir.join("repo-sam_r.json"), kept.to_string()).unwrap();
    r.look(&[], false);
    let st = r.state();
    for k in ["settings", "app", "labels", "databaseId", "hookId", "hook"] {
        assert_eq!(st[k], kept[k], "{k}");
    }
    // Its PR gone, it's judged from now; the branch gone, its record too.
    assert_eq!(r.tip("feat").seen, Some(T));
    assert!(!r.tips().contains_key("gone"));
}

#[test]
fn a_status_run_deletes_and_archives_nothing_and_keeps_state_of_its_own() {
    let mut r = SamR::new();
    let (young, old) = (r.ago(1.0), r.ago(4.0));
    r.branch("main", "ab", 0, young);
    r.branch("in-main", "1", 0, young);
    r.branch("stale", "2", 3, old);
    r.look(&[], false);
    let was = r.state();
    std::fs::create_dir_all(r.cfg.state_dir.join("dry")).unwrap();
    let dry = |r: &mut SamR| {
        let mut api = Hub(r.world.clone());
        let mut jobs = States::new("running");
        let mut git = Hands(r.world.clone());
        let mut p = Pass {
            cfg: &r.cfg,
            repo: "sam/r",
            now: r.now,
            dry: true,
            searched: 0,
            api: &mut api,
            jobs: &mut jobs,
            out: Out::default(),
        };
        let open = BTreeSet::new();
        let repo = Repo {
            default: "main",
            open: &open,
            full: false,
        };
        watch(&mut p, r.cfg.branches.as_ref().unwrap(), &repo, &mut git);
        p.out
    };
    dry(&mut r);
    r.now += GRACE;
    let out = dry(&mut r);
    // It would delete both, archiving the stale one first: neither is.
    assert!(out.stuck.is_empty() && out.failed.is_empty(), "{out:?}");
    let w = r.world.borrow();
    assert!(w.deleted.is_empty() && w.archived.is_empty());
    assert!(!w.asked_to_change);
    drop(w);
    assert_eq!(r.state(), was);
    let text = std::fs::read_to_string(r.cfg.state_dir.join("dry/repo-sam_r.json")).unwrap();
    let st: State = serde_json::from_str(&text).unwrap();
    assert_eq!(st.tips["in-main"].due, Some(T + GRACE));
    assert_eq!(st.tips["stale"].due, Some(T + GRACE));
    assert!(st.tips.values().all(|t| !t.unkept && t.undeleted.is_none()));
}
