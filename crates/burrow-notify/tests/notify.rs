//! HOST-notify against a GitHub that keeps its issues, can be down, refuse
//! some calls, or lose its answer to an issue it opened: one issue a notice,
//! a comment at most a day, closed when cleared, never a second one while it
//! can't say which are open, and pending until GitHub has shown it closed.

use std::fs;

use burrow_github::{Api, Failed, Response};
use burrow_notify::{Ask, Config, DAY, Journal, Priority, pending, run};
use serde_json::{Value, json};

const REPO: &str = "repos/sam/homelab/";
const NOW: u64 = 1_800_000_000;

#[derive(Debug, Default, Clone)]
struct Issue {
    number: u64,
    title: String,
    body: String,
    labels: Vec<String>,
    assignees: Vec<String>,
    open: bool,
    comments: Vec<String>,
    pull_request: bool,
}

/// GitHub as gh finds it.
#[derive(Default)]
struct GitHub {
    issues: Vec<Issue>,
    labels: Vec<String>,
    /// A login without push access: its new issues' labels are dropped, and
    /// it may neither make a label nor add one.
    no_push: bool,
    /// Drops the labels of a new issue, though it may add them after.
    drop_on_create: bool,
    down: bool,
    /// The calls it answers with a 503: "list", "label", "create",
    /// "comment", "close".
    fails: Vec<&'static str>,
    /// It opens the next issue and loses its answer.
    lose: bool,
    /// It makes the next label and answers with a 503.
    lose_label: bool,
    asked: Vec<(String, String, Option<Value>)>,
}

fn answer(status: u16, body: Value, link: Option<String>) -> Result<Response, Failed> {
    Ok(Response {
        status,
        headers: link.map(|l| ("Link".into(), l)).into_iter().collect(),
        body: if body.is_null() {
            String::new()
        } else {
            body.to_string()
        },
    })
}

fn label_objects(labels: &[String]) -> Value {
    labels.iter().map(|l| json!({"name": l})).collect()
}

impl GitHub {
    fn add(&mut self, title: &str, labels: &[&str]) -> u64 {
        let number = self.issues.len() as u64 + 1;
        self.issues.push(Issue {
            number,
            title: title.into(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            open: true,
            ..Issue::default()
        });
        number
    }

    fn open_titles(&self) -> Vec<&str> {
        self.issues
            .iter()
            .filter(|i| i.open)
            .map(|i| i.title.as_str())
            .collect()
    }

    fn asked(&self, method: &str, path: &str) -> usize {
        self.asked
            .iter()
            .filter(|(m, p, _)| m == method && p.starts_with(path))
            .count()
    }

    fn fails(&self, call: &str) -> bool {
        self.fails.contains(&call)
    }

    fn list(&self, query: &str) -> Result<Response, Failed> {
        let params: Vec<(&str, &str)> = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .collect();
        let param = |k: &str| params.iter().find(|(p, _)| *p == k).map(|(_, v)| *v);
        assert_eq!(param("state"), Some("open"), "{query}");
        let per_page: usize = param("per_page").unwrap().parse().unwrap();
        let page: usize = param("page").map_or(1, |p| p.parse().unwrap());
        // Newest first, as GitHub sorts them, PRs among them.
        let open: Vec<&Issue> = self.issues.iter().rev().filter(|i| i.open).collect();
        let shown: Vec<Value> = open
            .iter()
            .skip((page - 1) * per_page)
            .take(per_page)
            .map(|i| {
                json!({"number": i.number, "title": i.title,
                       "labels": label_objects(&i.labels),
                       "pull_request": if i.pull_request { json!({"url": "x"}) } else { Value::Null }})
            })
            .collect();
        let link = (page * per_page < open.len()).then(|| {
            format!(
                "<https://api.github.com/{REPO}issues?state=open&per_page={per_page}&page={}>; rel=\"next\"",
                page + 1
            )
        });
        answer(200, Value::Array(shown), link)
    }
}

impl Api for GitHub {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        _etag: Option<&str>,
    ) -> Result<Response, Failed> {
        self.asked.push((method.into(), path.into(), body.cloned()));
        if self.down {
            return Err(Failed {
                why: "gh: error connecting to api.github.com".into(),
                transient: true,
            });
        }
        let rest = path
            .strip_prefix(REPO)
            .unwrap_or_else(|| panic!("another repo: {path}"));
        let body = body.cloned().unwrap_or(Value::Null);
        let refused = || answer(503, json!({"message": "Service Unavailable"}), None);
        let number = |s: &str| -> usize { s.parse::<usize>().unwrap() - 1 };
        match (method, rest.split('/').collect::<Vec<_>>().as_slice()) {
            ("GET", [issues]) if issues.starts_with("issues?") => {
                if self.fails("list") {
                    return refused();
                }
                self.list(&issues["issues?".len()..])
            }
            ("GET", ["labels", name]) => {
                if self.fails("read label") {
                    return refused();
                }
                if self.labels.iter().any(|l| l == name) {
                    answer(200, json!({"name": name}), None)
                } else {
                    answer(404, json!({"message": "Not Found"}), None)
                }
            }
            ("POST", ["labels"]) => {
                if self.fails("label") {
                    return refused();
                }
                let name = body["name"].as_str().unwrap().to_string();
                assert_eq!(body["description"], "box-notify's notices");
                if self.no_push {
                    return answer(403, json!({"message": "Forbidden"}), None);
                }
                if self.lose_label {
                    self.lose_label = false;
                    self.labels.push(name);
                    return refused();
                }
                if self.labels.contains(&name) {
                    return answer(422, json!({"message": "Validation Failed"}), None);
                }
                self.labels.push(name);
                answer(201, json!({}), None)
            }
            ("POST", ["issues"]) => {
                if self.fails("create") {
                    return refused();
                }
                let n = self.add(body["title"].as_str().unwrap(), &[]);
                let (no_push, drop) = (self.no_push, self.drop_on_create);
                let known = self.labels.clone();
                let issue = self.issues.last_mut().unwrap();
                issue.body = body["body"].as_str().unwrap().into();
                for (field, into) in [
                    ("labels", &mut issue.labels),
                    ("assignees", &mut issue.assignees),
                ] {
                    *into = body[field]
                        .as_array()
                        .map(|a| a.iter().map(|v| v.as_str().unwrap().into()).collect())
                        .unwrap_or_default();
                }
                // GitHub keeps only labels it knows, and for a login without
                // push access none, and says 201 all the same.
                issue
                    .labels
                    .retain(|l| !no_push && !drop && known.contains(l));
                let labels = label_objects(&issue.labels);
                if self.lose {
                    self.lose = false;
                    return Err(Failed {
                        why: "gh: connection reset by peer".into(),
                        transient: true,
                    });
                }
                answer(201, json!({"number": n, "labels": labels}), None)
            }
            ("POST", ["issues", n, "labels"]) => {
                if self.no_push {
                    return answer(403, json!({"message": "Forbidden"}), None);
                }
                let i = number(n);
                for l in body["labels"].as_array().unwrap() {
                    self.issues[i].labels.push(l.as_str().unwrap().into());
                }
                answer(200, json!([]), None)
            }
            ("POST", ["issues", n, "comments"]) => {
                if self.fails("comment") {
                    return refused();
                }
                let i = number(n);
                self.issues[i]
                    .comments
                    .push(body["body"].as_str().unwrap().into());
                answer(201, json!({}), None)
            }
            ("PATCH", ["issues", n]) => {
                if self.fails("close") {
                    return refused();
                }
                assert_eq!(body, json!({"state": "closed"}));
                let i = number(n);
                self.issues[i].open = false;
                answer(200, json!({}), None)
            }
            _ => panic!("unexpected {method} {path}"),
        }
    }

    fn pause(&mut self, _secs: u64) {}
}

#[derive(Default)]
struct Lines(Vec<(Priority, String)>);

impl Journal for Lines {
    fn log(&mut self, priority: Priority, line: &str) {
        self.0.push((priority, line.into()));
    }
}

struct Machine {
    _dir: tempfile::TempDir,
    cfg: Config,
    github: GitHub,
    journal: Lines,
}

fn machine(repo: Option<&str>) -> Machine {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config {
        name: "box-notify".into(),
        repo: repo.map(String::from),
        assignee: None,
        label: "updates".into(),
        dir: dir.path().join("notices"),
        user: "sam".into(),
        home: dir.path().into(),
    };
    fs::create_dir(&cfg.dir).unwrap();
    Machine {
        _dir: dir,
        cfg,
        github: GitHub::default(),
        journal: Lines::default(),
    }
}

impl Machine {
    fn notify(&mut self, words: &[&str], now: u64) {
        let words: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        let ask = Ask::parse(&words).unwrap();
        run(&self.cfg, &ask, &mut self.github, &mut self.journal, now);
    }

    fn open(&mut self, key: &str, now: u64) {
        self.notify(&["open", key, "Stuck", "It's stuck."], now);
    }

    fn close(&mut self, key: &str) {
        self.notify(&["close", key, "Fixed."], NOW);
    }

    fn pending(&self, key: &str) -> bool {
        pending(&self.cfg, key)
    }
}

#[test]
fn one_issue_a_notice_and_a_comment_at_most_a_day() {
    let mut b = machine(Some("sam/homelab"));
    b.open("k1", NOW);
    assert_eq!(b.github.open_titles(), ["[k1] Stuck"]);
    let issue = &b.github.issues[0];
    assert_eq!(issue.body, "It's stuck.");
    assert_eq!(issue.labels, ["updates"]);
    assert!(issue.assignees.is_empty());
    assert_eq!(b.github.labels, ["updates"]);
    assert!(b.pending("k1"));
    assert!(b.journal.0.is_empty(), "{:?}", b.journal.0);

    b.open("k1", NOW + 60);
    b.open("k1", NOW + DAY);
    assert!(b.github.issues[0].comments.is_empty());
    b.open("k1", NOW + DAY + 1);
    assert_eq!(
        b.github.issues[0].comments,
        ["Still happening: It's stuck."]
    );
    b.open("k1", NOW + 2 * DAY);
    assert_eq!(b.github.issues[0].comments.len(), 1);
    b.open("k1", NOW + 2 * DAY + 2);
    assert_eq!(b.github.issues[0].comments.len(), 2);
    assert_eq!(b.github.open_titles(), ["[k1] Stuck"]);
    // The label is made for a new issue only.
    assert_eq!(b.github.asked("POST", &format!("{REPO}labels")), 1);
}

#[test]
fn cleared_it_closes_and_a_close_for_nothing_asks_nothing() {
    let mut b = machine(Some("sam/homelab"));
    b.open("k1", NOW);
    b.close("k1");
    assert_eq!(b.github.open_titles(), Vec::<&str>::new());
    assert_eq!(b.github.issues[0].comments, ["Fixed."]);
    assert!(!b.pending("k1"));
    let asked = b.github.asked.len();
    b.close("k1");
    b.notify(&["close", "never"], NOW);
    assert_eq!(b.github.asked.len(), asked);

    // Cleared, with no word on what did it.
    b.open("k2", NOW);
    b.notify(&["close", "k2"], NOW);
    assert_eq!(b.github.issues[1].comments, ["Cleared."]);
}

#[test]
fn while_github_is_down_nothing_is_opened_and_a_close_stays_pending() {
    let mut b = machine(Some("sam/homelab"));
    b.github.down = true;
    b.open("k2", NOW);
    assert!(b.github.issues.is_empty());
    assert!(!b.pending("k2"));
    assert_eq!(b.journal.0.len(), 1);
    let (priority, line) = &b.journal.0[0];
    assert_eq!(*priority, Priority::Warning);
    assert!(
        line.starts_with(
            "box-notify open k2: couldn't reach GitHub (GET repos/sam/homelab/issues?"
        ),
        "{line}"
    );
    assert!(
        line.ends_with("error connecting to api.github.com, 3 times)"),
        "{line}"
    );

    b.github.down = false;
    b.open("k2", NOW);
    b.github.down = true;
    b.close("k2");
    assert_eq!(b.github.open_titles(), ["[k2] Stuck"]);
    assert!(b.pending("k2"));
    b.github.down = false;
    b.close("k2");
    assert_eq!(b.github.open_titles(), Vec::<&str>::new());
    assert!(!b.pending("k2"));
}

#[test]
fn an_issue_whose_answer_was_lost_is_opened_once_pending_and_closes() {
    let mut b = machine(Some("sam/homelab"));
    b.github.lose = true;
    b.open("k3", NOW);
    assert_eq!(b.github.open_titles(), ["[k3] Stuck"]);
    // Asking again would have opened a second.
    assert_eq!(b.github.asked("POST", &format!("{REPO}issues")), 1);
    assert!(b.pending("k3"));
    assert!(b.journal.0[0].1.contains("connection reset by peer"));
    b.close("k3");
    assert_eq!(b.github.open_titles(), Vec::<&str>::new());
    assert!(!b.pending("k3"));
}

#[test]
fn a_close_github_refuses_stays_pending() {
    for refused in ["comment", "close"] {
        let mut b = machine(Some("sam/homelab"));
        b.open("k6", NOW);
        b.github.fails = vec![refused];
        b.close("k6");
        assert_eq!(b.github.open_titles(), ["[k6] Stuck"], "{refused}");
        assert!(b.pending("k6"), "{refused}");
        b.github.fails.clear();
        b.close("k6");
        assert!(!b.pending("k6"), "{refused}");
    }
}

#[test]
fn while_it_cant_list_none_is_opened_twice_and_twins_close_together() {
    let mut b = machine(Some("sam/homelab"));
    b.open("k4", NOW);
    b.github.fails = vec!["list"];
    b.open("k4", NOW + 2 * DAY);
    assert_eq!(b.github.issues.len(), 1);
    assert!(b.github.issues[0].comments.is_empty());
    // A twin left from before closes with the rest.
    b.github.fails.clear();
    b.github.add("[k4] Stuck", &["updates"]);
    b.close("k4");
    assert_eq!(b.github.open_titles(), Vec::<&str>::new());
    assert!(!b.pending("k4"));
}

#[test]
fn only_its_own_issues_count() {
    let mut b = machine(Some("sam/homelab"));
    b.github.add("[k10] Stuck", &["updates"]);
    b.github.add("k1 Stuck", &["updates"]);
    b.github.add("[k1] Stuck", &["other"]);
    let pr = b.github.add("[k1] Stuck", &["updates"]);
    b.github.issues[pr as usize - 1].pull_request = true;
    let closed = b.github.add("[k1] Stuck", &["updates"]);
    b.github.issues[closed as usize - 1].open = false;
    b.open("k1", NOW);
    assert_eq!(b.github.issues.len(), 6);
    let ours = &b.github.issues[5];
    assert_eq!((ours.title.as_str(), ours.open), ("[k1] Stuck", true));
    b.close("k1");
    let still: Vec<u64> = b
        .github
        .issues
        .iter()
        .filter(|i| i.open)
        .map(|i| i.number)
        .collect();
    assert_eq!(still, [1, 2, 3, 4]);
}

#[test]
fn every_page_of_issues_is_read() {
    let mut b = machine(Some("sam/homelab"));
    let ours = b.github.add("[k7] Stuck", &["updates"]);
    for n in 0..150 {
        b.github.add(&format!("[other-{n}] Stuck"), &["updates"]);
    }
    b.open("k7", NOW);
    assert_eq!(b.github.issues.len(), 151, "opened a second");
    assert_eq!(
        b.github
            .asked
            .iter()
            .filter(|(m, _, _)| m == "GET")
            .map(|(_, p, _)| p.as_str())
            .collect::<Vec<_>>(),
        [
            "repos/sam/homelab/issues?state=open&per_page=100",
            "repos/sam/homelab/issues?state=open&per_page=100&page=2",
        ]
    );
    // Opened elsewhere, so not said here today: the problem is said again.
    assert_eq!(
        b.github.issues[ours as usize - 1].comments,
        ["Still happening: It's stuck."]
    );
    b.close("k7");
    assert!(!b.github.issues[ours as usize - 1].open);
}

#[test]
fn the_newest_of_twins_hears_again() {
    let mut b = machine(Some("sam/homelab"));
    b.github.add("[k14] Stuck", &["updates"]);
    b.github.add("[k14] Stuck", &["updates"]);
    b.open("k14", NOW);
    let comments: Vec<usize> = b.github.issues.iter().map(|i| i.comments.len()).collect();
    assert_eq!(comments, [0, 1]);
}

#[test]
fn a_notice_file_that_cant_say_when_counts_as_never() {
    let mut b = machine(Some("sam/homelab"));
    b.open("k8", NOW);
    fs::write(b.cfg.dir.join("k8"), "soon\n").unwrap();
    b.open("k8", NOW + 60);
    assert_eq!(b.github.issues[0].comments.len(), 1);
    assert_eq!(
        fs::read_to_string(b.cfg.dir.join("k8")).unwrap(),
        format!("{}\n", NOW + 60)
    );
}

#[test]
fn an_issue_is_assigned_to_the_owner() {
    let mut b = machine(Some("sam/homelab"));
    b.cfg.assignee = Some("sam".into());
    b.open("k9", NOW);
    assert_eq!(b.github.issues[0].assignees, ["sam"]);
    // Nobody, said as an empty name.
    b.cfg.assignee = Some(String::new());
    b.open("k13", NOW);
    assert_eq!(
        b.github
            .asked
            .last()
            .unwrap()
            .2
            .as_ref()
            .unwrap()
            .get("assignees"),
        None
    );
}

#[test]
fn without_its_notice_file_nothing_is_opened() {
    // Opened unrecorded, it could never be closed.
    let mut b = machine(Some("sam/homelab"));
    fs::remove_dir(&b.cfg.dir).unwrap();
    b.open("k11", NOW);
    assert!(b.github.issues.is_empty());
    let line = &b.journal.0[0].1;
    assert!(
        line.starts_with("box-notify open k11: couldn't reach GitHub ("),
        "{line}"
    );
    assert!(line.contains("notices/k11"), "{line}");
}

#[test]
fn a_label_the_repo_already_has_is_used() {
    let mut b = machine(Some("sam/homelab"));
    b.github.labels = vec!["updates".into()];
    b.open("k12", NOW);
    assert_eq!(b.github.issues[0].labels, ["updates"]);
    assert!(b.journal.0.is_empty(), "{:?}", b.journal.0);
}

/// Absent, and it can't be made: no issue that would come out unlabelled and
/// no record of one, and the journal says why; the next try starts afresh.
#[test]
fn a_label_it_cant_make_opens_nothing() {
    for (no_push, fails, why) in [
        (true, vec![], "HTTP 403"),
        (false, vec!["label"], "HTTP 503"),
    ] {
        let mut b = machine(Some("sam/homelab"));
        b.github.no_push = no_push;
        b.github.fails = fails;
        b.open("k12", NOW);
        assert!(b.github.issues.is_empty());
        assert!(!b.pending("k12"));
        let (priority, line) = &b.journal.0[0];
        assert_eq!(*priority, Priority::Warning);
        assert!(
            line.starts_with("box-notify open k12: couldn't reach GitHub ("),
            "{line}"
        );
        assert!(line.contains(why), "{line}");
        b.github.no_push = false;
        b.github.fails = vec![];
        b.open("k12", NOW);
        assert_eq!(b.github.issues[0].labels, ["updates"]);
        assert!(b.pending("k12"));
    }
}

/// GitHub made the label and lost its answer: asked, it says it's there.
#[test]
fn a_label_whose_answer_was_lost_is_used() {
    let mut b = machine(Some("sam/homelab"));
    b.github.lose_label = true;
    b.open("k12", NOW);
    assert_eq!(b.github.issues[0].labels, ["updates"]);
    assert!(b.journal.0.is_empty(), "{:?}", b.journal.0);
}

/// The make failed and GitHub can't say whether it's there: nothing's opened.
#[test]
fn a_label_github_cant_vouch_for_opens_nothing() {
    let mut b = machine(Some("sam/homelab"));
    b.github.lose_label = true;
    b.github.fails = vec!["read label"];
    b.open("k12", NOW);
    assert!(b.github.issues.is_empty());
    assert!(!b.pending("k12"));
    assert_eq!(b.journal.0.len(), 1);
}

/// The label's there but the login has no push access: GitHub drops it from
/// the new issue, says 201, and refuses to add it. The issue is still the
/// notice's: no twin, the journal says so, and it closes.
#[test]
fn an_issue_whose_label_was_dropped_is_still_found() {
    let mut b = machine(Some("sam/homelab"));
    b.github.labels = vec!["updates".into()];
    b.github.no_push = true;
    b.open("k13", NOW);
    assert_eq!(b.github.open_titles(), ["[k13] Stuck"]);
    assert!(b.github.issues[0].labels.is_empty());
    assert!(b.pending("k13"));
    assert_eq!(b.journal.0.len(), 1);
    assert!(
        b.journal.0[0].1.contains("issues/1/labels"),
        "{:?}",
        b.journal.0
    );
    b.open("k13", NOW + 1);
    assert_eq!(b.github.issues.len(), 1, "a twin");
    b.close("k13");
    assert!(!b.github.issues[0].open);
    assert_eq!(b.github.issues[0].comments, ["Fixed."]);
    assert!(!b.pending("k13"));
}

/// As above, with the answer lost too: only the title can find it.
#[test]
fn a_dropped_label_and_a_lost_answer_still_make_one_issue() {
    let mut b = machine(Some("sam/homelab"));
    b.github.labels = vec!["updates".into()];
    b.github.no_push = true;
    b.github.lose = true;
    b.open("k14", NOW);
    b.open("k14", NOW + 1);
    assert_eq!(b.github.issues.len(), 1, "a twin");
    assert!(b.pending("k14"));
    b.close("k14");
    assert!(!b.github.issues[0].open);
    assert!(!b.pending("k14"));
}

/// Dropped only from the issue: a login that may add it, adds it.
#[test]
fn a_label_dropped_from_a_new_issue_is_added() {
    let mut b = machine(Some("sam/homelab"));
    // A repo where the create's labels are dropped though adding one works.
    b.github.drop_on_create = true;
    b.open("k15", NOW);
    assert_eq!(b.github.issues[0].labels, ["updates"]);
    assert!(b.journal.0.is_empty(), "{:?}", b.journal.0);
}

#[test]
fn with_no_repo_every_notice_goes_to_the_journal() {
    for repo in [None, Some("")] {
        let mut b = machine(repo);
        b.open("k5", NOW);
        b.close("k5");
        assert!(b.github.asked.is_empty());
        assert_eq!(
            b.journal.0,
            [
                (
                    Priority::Warning,
                    "box-notify open k5 Stuck It's stuck.".into()
                ),
                (Priority::Notice, "box-notify close k5 Fixed.".into()),
            ]
        );
        assert!(!b.pending("k5"));
    }
}
