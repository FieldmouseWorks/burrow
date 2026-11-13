//! Which PRs go first: by who put `first` on them, or on an issue they
//! close; a connection GitHub cut short read whole first.

use serde_json::{Value, json};

use super::*;
use crate::testing::{Hub, States, answer, config, look};

/// Accounts as GitHub's events name them: 1 is an author's, 2 the
/// steward's App's (config's first_by); 99 the machinery's App's, 98 one
/// spelled like an author's login.
fn labeled(label: &str, id: u64) -> Value {
    let login = match id {
        1 | 98 => "sam",
        2 => "steward[bot]",
        _ => "workers[bot]",
    };
    json!({"event": "labeled", "label": {"name": label}, "actor": {"login": login, "id": id}})
}

/// GitHub's events for issue or PR `n` of `repo`: the steward's stage
/// labels after whoever put `first` on (`by`, none: nobody).
fn events(hub: &mut Hub, repo: &str, n: u64, by: Option<u64>) {
    let mut all: Vec<Value> = by.map(|id| labeled("first", id)).into_iter().collect();
    all.push(labeled("waiting: review", 2));
    all.push(json!({"event": "unlabeled", "label": {"name": "waiting: review"}, "actor": {"login": "steward[bot]", "id": 2}}));
    hub.on(
        "GET",
        &format!("repos/{repo}/issues/{n}/events?per_page=100$"),
        answer(200, json!(all)),
    );
}

fn labels(names: &[&str], more: bool) -> Value {
    json!({"pageInfo": {"hasNextPage": more}, "nodes": names.iter().map(|n| json!({"name": n})).collect::<Vec<_>>()})
}

fn pr(n: u64, names: &[&str], closes: &[Value]) -> Value {
    json!({"number": n, "labels": labels(names, false),
           "closingIssuesReferences": {"pageInfo": {"hasNextPage": false}, "nodes": closes}})
}

fn issue(repo: &str, n: u64, names: &[&str]) -> Value {
    json!({"number": n, "repository": {"nameWithOwner": repo}, "labels": labels(names, false)})
}

fn order(hub: &mut Hub, prs: Vec<Value>) -> Firsts {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let ask: Ask = serde_json::from_value(json!({ "prs": prs })).unwrap();
    look(&cfg, hub, &mut States::new("running"), 0.0, |p| {
        Ok(firsts(p, &ask))
    })
    .1
    .unwrap()
}

fn label_writes(hub: &Hub) -> Vec<(String, Value)> {
    hub.asked
        .iter()
        .filter(|a| a.0 == "POST" && a.1.ends_with("/labels"))
        .map(|a| (a.1.clone(), a.3.clone().unwrap_or_default()))
        .collect()
}

// An author's or the steward's `first` on a PR counts; the machinery's
// App's, a spoofed login's or nobody's doesn't. A PR closing an issue an
// author marked first goes first and gets the label; one closing an issue
// the machinery's App marked doesn't. Only PRs and issues that carry the
// label are asked about.
#[test]
fn only_a_trusted_first_counts_on_the_pr_and_its_issue() {
    let mut hub = Hub::default();
    events(&mut hub, "sam/r", 9, Some(1));
    events(&mut hub, "sam/r", 8, Some(2));
    events(&mut hub, "sam/r", 7, Some(99));
    events(&mut hub, "sam/r", 6, Some(98));
    events(&mut hub, "sam/r", 5, None);
    events(&mut hub, "sam/r", 11, Some(1));
    events(&mut hub, "sam/other", 12, Some(1));
    events(&mut hub, "sam/r", 13, Some(99));
    hub.on("POST", "repos/sam/r/issues/", answer(200, json!([])));
    let got = order(
        &mut hub,
        vec![
            pr(2, &[], &[]),
            pr(9, &["first"], &[]),
            pr(8, &["first"], &[]),
            pr(7, &["first"], &[]),
            pr(6, &["first"], &[]),
            pr(5, &["first"], &[]),
            pr(1, &[], &[issue("sam/r", 11, &["first"])]),
            pr(3, &[], &[issue("sam/other", 12, &["ready", "first"])]),
            pr(4, &[], &[issue("sam/r", 13, &["first"])]),
            pr(10, &[], &[issue("sam/r", 14, &["ready"])]),
        ],
    );
    assert_eq!(got.firsts, [9, 8, 1, 3]);
    assert_eq!(got.labelled, [1, 3]);
    assert_eq!(
        label_writes(&hub),
        [
            (
                "repos/sam/r/issues/1/labels".into(),
                json!({"labels": ["first"]})
            ),
            (
                "repos/sam/r/issues/3/labels".into(),
                json!({"labels": ["first"]})
            ),
        ]
    );
    // Nothing asked about #2, #10 or issue 14: none carries the label.
    for n in ["/2/", "/10/", "/14/"] {
        assert!(
            !hub.paths().iter().any(|p| p.contains(n)),
            "{:?}",
            hub.paths()
        );
    }
}

// A PR that carries a label the machinery's App put on and closes an
// author's first issue goes first, without the label put on again.
#[test]
fn a_pr_with_the_label_already_is_not_labelled_again() {
    let mut hub = Hub::default();
    events(&mut hub, "sam/r", 7, Some(99));
    events(&mut hub, "sam/r", 11, Some(1));
    let got = order(
        &mut hub,
        vec![pr(7, &["first"], &[issue("sam/r", 11, &["first"])])],
    );
    assert_eq!(
        got,
        Firsts {
            firsts: vec![7],
            labelled: vec![]
        }
    );
    assert!(label_writes(&hub).is_empty());
}

// The last time the label went on is what counts.
#[test]
fn the_last_labelling_counts() {
    let at = |ids: &[u64]| {
        let all: Vec<Value> = ids.iter().map(|id| labeled("first", *id)).collect();
        last_by(&all, "first")
    };
    assert_eq!(at(&[1, 99]), Some(99));
    assert_eq!(at(&[99, 1]), Some(1));
    assert_eq!(last_by(&[labeled("priority: high", 1)], "first"), None);
}

// A PR's labels, its closing issues and an issue's labels cut short are
// read whole before anything's concluded: a first past the query's pages
// still counts.
#[test]
fn cut_connections_are_read_whole() {
    let mut hub = Hub::default();
    // The PR's own labels, `first` past the query's.
    let mut cut = pr(9, &["a"], &[]);
    cut["labels"] = labels(&["a"], true);
    hub.on(
        "GET",
        "repos/sam/r/issues/9/labels?per_page=100$",
        answer(200, json!([{"name": "a"}, {"name": "first"}])),
    );
    events(&mut hub, "sam/r", 9, Some(1));
    // A PR whose sixth closing issue is the one marked first.
    let mut many = pr(
        1,
        &[],
        &(20..25).map(|n| issue("sam/r", n, &[])).collect::<Vec<_>>(),
    );
    many["closingIssuesReferences"]["pageInfo"] = json!({"hasNextPage": true, "endCursor": "c1"});
    let mut first_page = (20..25).map(|n| issue("sam/r", n, &[])).collect::<Vec<_>>();
    first_page.push(issue("sam/r", 25, &["first"]));
    hub.graphql.push((
        "closingIssuesReferences".into(),
        answer(200, json!({"data": {"repository": {"pullRequest": {"closingIssuesReferences": {"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": first_page}}}}})),
    ));
    events(&mut hub, "sam/r", 25, Some(1));
    // An issue whose labels were cut short, `first` past them.
    let mut short = issue("sam/r", 31, &["x"]);
    short["labels"] = labels(&["x"], true);
    hub.on(
        "GET",
        "repos/sam/r/issues/31/labels?per_page=100$",
        answer(200, json!([{"name": "x"}, {"name": "first"}])),
    );
    events(&mut hub, "sam/r", 31, Some(1));
    hub.on("POST", "repos/sam/r/issues/", answer(200, json!([])));
    let got = order(&mut hub, vec![cut, many, pr(3, &[], &[short])]);
    assert_eq!(got.firsts, [9, 1, 3]);
    assert_eq!(got.labelled, [1, 3]);
}

// GitHub not saying who labelled it: not first, and nothing put on.
#[test]
fn an_unanswered_look_is_not_first() {
    let mut hub = Hub::default();
    let mut gone = answer(0, Value::Null);
    gone.why = "no answer".into();
    hub.on("GET", "repos/sam/r/issues/9/events", gone);
    let got = order(&mut hub, vec![pr(9, &["first"], &[])]);
    assert_eq!(got, Firsts::default());
}
