//! A major in a repo burrow-landing's record gives conaryd: the verdicts
//! the sweep hands its job are Conary's v2 issuances, which conaryd's own
//! read (`conary_github::flow::read`, on a scripted GitHub here) counts on
//! the PR's revision under the sweep's key, and nothing else. In a repo
//! the steward lands they're the v1 markers they always were.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use conary_github::api::{
    ApiError, CheckRun, Comment, Commit, Event, History, PullRequest, Pulls, Review, Status,
};
use conary_github::branches::{Branches, Merged};
use conary_github::flow::{self, Major, Revision, SealKey, Settings, Snapshot};

use super::*;

/// The jobs' App, as GraphQL and REST spell its bot.
const BOT: &str = "sam-workers";
const BOT_REST: &str = "sam-workers[bot]";
/// Who opens Renovate's PRs, as REST spells it.
const RENOVATE: &str = "sam-renovate[bot]";

impl Machine {
    /// burrow-landing's record, each repo's holding as `repos` has it.
    fn landing(&mut self, repos: Value) {
        let path = self.path("landing.json");
        fs::write(&path, json!({ "repos": repos }).to_string()).unwrap();
        self.landing = Some(path);
    }
}

/// A record that gives sam/r to conaryd.
fn conaryd() -> Value {
    json!({"sam/r": {"owner": "conaryd", "generation": 1}})
}

/// PR `n`, a major, with a description.
fn described(n: u64, body: &str) -> Value {
    let mut p = pr(n);
    p["body"] = json!(body);
    p
}

/// The ok and held markers the task hands PR `n`'s job.
fn handed(task: &str, n: u64) -> (String, String) {
    let line = task
        .lines()
        .find(|l| l.starts_with(&format!("- #{n} on ")))
        .unwrap_or_else(|| panic!("no markers for #{n}: {task}"));
    let quoted: Vec<&str> = line.split('`').skip(1).step_by(2).collect();
    let [ok, held] = quoted[..] else {
        panic!("{line}")
    };
    assert!(
        line.contains(": ok `") && line.contains("`, held `"),
        "{line}"
    );
    (ok.to_owned(), held.to_owned())
}

/// The PR as GitHub has it, for Conary's read.
struct Pr {
    n: u64,
    title: String,
    body: String,
    head: String,
    base: String,
    /// Its comments, as REST has them.
    comments: Vec<Value>,
}

struct Hub(Mutex<Pr>);

impl Hub {
    fn new(n: u64, body: &str) -> Hub {
        Hub(Mutex::new(Pr {
            n,
            title: format!("Update foo{n} to v2"),
            body: body.into(),
            head: sha(n),
            base: "main".into(),
            comments: Vec::new(),
        }))
    }

    fn with<T>(&self, f: impl FnOnce(&mut Pr) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }

    /// The job's comment, ending with `marker`, by `by` as REST spells it.
    fn say(&self, by: &str, marker: &str) {
        self.with(|p| {
            p.comments.push(json!({
                "user": {"login": by, "type": "Bot"},
                "body": format!("Read it: nothing here breaks.\n\n{marker}"),
            }))
        });
    }

    fn pull_json(&self) -> Value {
        self.with(|p| {
            json!({
                "number": p.n, "state": "open", "title": p.title, "body": p.body,
                "user": {"login": RENOVATE, "type": "Bot"},
                "head": {"sha": p.head, "ref": format!("renovate/foo{}-2.x", p.n), "repo": null},
                "base": {"sha": "3".repeat(40), "ref": p.base, "repo": null},
                "labels": [{"name": "major"}],
            })
        })
    }

    /// The major's verdict as conaryd reads it, under `key`.
    fn major(&self, key: &str) -> Option<Major> {
        let settings = Settings {
            major_key: Some(SealKey::from_hex(key).unwrap()),
            verdict_authors: BTreeSet::from([BOT_REST.to_owned()]),
            ..Settings::default()
        };
        let n = self.with(|p| p.n);
        flow::read(
            self,
            &REPO.parse().unwrap(),
            n,
            &conary_core::Policy::parse(br#"{"r2":["^secrets/"],"docs":["^docs/"]}"#).unwrap(),
            settings,
            &[RENOVATE.to_owned()],
        )
        .unwrap()
        .major
    }
}

impl conary_github::Source for Hub {
    fn pull(&self, _: &str, _: u64) -> Result<PullRequest, ApiError> {
        Ok(serde_json::from_value(self.pull_json()).unwrap())
    }
    fn pulls(&self, _: &str, _: Pulls) -> Result<Vec<PullRequest>, ApiError> {
        Ok(vec![serde_json::from_value(self.pull_json()).unwrap()])
    }
    fn comments(&self, _: &str, _: u64) -> Result<Vec<Comment>, ApiError> {
        Ok(serde_json::from_value(json!(self.with(|p| p.comments.clone()))).unwrap())
    }
    fn reviews(&self, _: &str, _: u64) -> Result<Vec<Review>, ApiError> {
        Ok(Vec::new())
    }
    fn check_runs(&self, _: &str, _: &str) -> Result<Vec<CheckRun>, ApiError> {
        Ok(Vec::new())
    }
    fn statuses(&self, _: &str, _: &str) -> Result<Vec<Status>, ApiError> {
        Ok(Vec::new())
    }
    fn files(&self, _: &str, _: &str, _: &str) -> Result<conary_core::policy::Paths, ApiError> {
        Ok(conary_core::policy::Paths {
            files: Vec::new(),
            cut: false,
        })
    }
    fn diff(&self, _: &str, _: &str, _: &str) -> Result<String, ApiError> {
        Err(ApiError::new("not scripted"))
    }
    fn compare_commits(&self, _: &str, _: &str, _: &str) -> Result<History, ApiError> {
        Err(ApiError::new("not scripted"))
    }
    fn commit(&self, _: &str, _: &str) -> Result<Commit, ApiError> {
        Err(ApiError::new("not scripted"))
    }
    fn commit_pulls(&self, _: &str, _: &str) -> Result<Vec<u64>, ApiError> {
        Err(ApiError::new("not scripted"))
    }
    fn timeline(&self, _: &str, _: u64) -> Result<Vec<Event>, ApiError> {
        Err(ApiError::new("not scripted"))
    }
    fn merge_base(&self, _: &str, _: &str, _: &str) -> Result<String, ApiError> {
        Err(ApiError::new("not scripted"))
    }
    fn annotations(&self, _: &str, _: u64) -> Result<Vec<String>, ApiError> {
        Err(ApiError::new("not scripted"))
    }
}

impl Branches for Hub {
    fn branch(&self, _: &str, _: &str) -> Result<Option<String>, ApiError> {
        Ok(Some("3".repeat(40)))
    }
    fn tree(&self, _: &str, _: &str) -> Result<String, ApiError> {
        Err(ApiError::new("not scripted"))
    }
    fn default_branch(&self, _: &str) -> Result<String, ApiError> {
        Err(ApiError::new("not scripted"))
    }
    fn branches(&self, _: &str, _: &str) -> Result<Vec<String>, ApiError> {
        Err(ApiError::new("not scripted"))
    }
    fn create_branch(&self, _: &str, _: &str, _: &str) -> Result<(), ApiError> {
        Err(ApiError::new("not scripted"))
    }
    fn delete_branch(&self, _: &str, _: &str) -> Result<(), ApiError> {
        Err(ApiError::new("not scripted"))
    }
    fn merge(&self, _: &str, _: &str, _: &str, _: &str) -> Result<Merged, ApiError> {
        Err(ApiError::new("not scripted"))
    }
}

impl flow::Read for Hub {
    fn snapshot(&self, _: &str, _: u64) -> Result<Snapshot, ApiError> {
        let revision =
            self.with(|p| Revision::for_parts(p.head.clone(), p.base.clone(), &p.title, &p.body));
        Ok(Snapshot {
            revision,
            default_branch: None,
            draft: false,
            complete: true,
            merge_state: "CLEAN".into(),
            open_threads: 0,
            threads: BTreeMap::new(),
            resolved_reviews: BTreeSet::new(),
            updated_at: 0,
            suites: BTreeMap::new(),
            checks_complete: true,
            rebase_removed: vec![],
            timeline_cut_at: None,
            since: Default::default(),
        })
    }
    fn pushes(&self, _: &str, _: &str) -> Result<BTreeMap<String, String>, ApiError> {
        Ok(BTreeMap::new())
    }
}

/// The sweep's ok, posted by its job, is what conaryd lands a major on;
/// its held holds it. Before, the sweep sealed only v1, which conaryd
/// doesn't read: no verdict, and the major never landed.
#[test]
fn a_major_conaryd_lands_lands_on_the_sweeps_ok() {
    let body = "This PR contains the following updates: foo1 to v2.\r\n\r\n---\r\n";
    let mut m = Machine::as_app();
    m.landing(conaryd());
    m.prs(&[described(1, body)]);
    let p = m.pass();
    let (ok, held) = handed(p.task(), 1);
    assert!(ok.starts_with("<!-- major-verdict:v2 {"), "{ok}");
    let hub = Hub::new(1, body);
    assert_eq!(hub.major(KEY), None);
    hub.say(BOT_REST, &ok);
    assert_eq!(hub.major(KEY), Some(Major::Cleared));
    // The sweep reads it as conaryd does: an ok major no steward merges,
    // listed for the owner, and not handed over again.
    m.comments(1, &[marked(BOT, &ok)]);
    let p = m.pass();
    assert!(p.jobs.is_empty(), "{:#?}", p.jobs);
    assert!(
        p.majors().1.contains("Update foo1 to v2"),
        "{:?}",
        p.majors()
    );
    // Held, posted after it (a generation on), holds it.
    hub.say(BOT_REST, &held);
    assert_eq!(hub.major(KEY), Some(Major::Held));
    // Someone else's copy of a marker is no verdict at all.
    let other = Hub::new(1, body);
    other.say("mallory", &ok);
    assert_eq!(other.major(KEY), None);
}

/// It counts only on the revision it was sealed on, under the sweep's key,
/// and only while no later generation is posted.
#[test]
fn a_v2_verdict_counts_on_its_revision_alone() {
    let body = "Updates foo3.";
    let mut m = Machine::as_app();
    m.landing(conaryd());
    m.prs(&[described(3, body)]);
    let (ok, held) = handed(m.pass().task(), 3);
    let counts = |change: &dyn Fn(&mut Pr), key: &str| {
        let hub = Hub::new(3, body);
        hub.with(change);
        hub.say(BOT_REST, &ok);
        hub.major(key)
    };
    let same = |_: &mut Pr| {};
    assert_eq!(counts(&same, KEY), Some(Major::Cleared));
    // A push, a retarget, a description or title edit since: none.
    let changes: [&dyn Fn(&mut Pr); 4] = [
        &|p| p.head = sha(4),
        &|p| p.base = "release".into(),
        &|p| p.body.push_str(" Edited."),
        &|p| p.title.push('!'),
    ];
    for change in changes {
        assert_eq!(counts(change, KEY), None);
    }
    // Under another key: none.
    assert_eq!(counts(&same, &"cd".repeat(32)), None);
    // Handed over again (its CI finished again on the head), a later
    // handover's verdict overtakes an earlier one's, whichever was posted
    // last: the newer generation's is the verdict.
    let mut p = described(3, body);
    p["statusCheckRollup"][0]["completedAt"] = json!("2099-01-02T00:00:00Z");
    m.prs(&[p]);
    let (ok2, held2) = handed(m.pass().task(), 3);
    assert_ne!(ok, ok2);
    let hub = Hub::new(3, body);
    hub.say(BOT_REST, &ok2);
    hub.say(BOT_REST, &held);
    assert_eq!(hub.major(KEY), Some(Major::Cleared));
    let hub = Hub::new(3, body);
    hub.say(BOT_REST, &held2);
    hub.say(BOT_REST, &ok);
    assert_eq!(hub.major(KEY), Some(Major::Held));
}

/// A description edit, or the repo changing hands, hands a major over once
/// more, with markers for what it is now: one sealed before wouldn't count.
#[test]
fn a_description_edit_or_a_change_of_hands_hands_it_over_again() {
    let mut m = Machine::as_app();
    m.prs(&[described(1, "one")]);
    let v1 = m.pass();
    assert!(
        v1.task().contains("<!-- major-verdict: ok "),
        "{}",
        v1.task()
    );
    assert!(m.pass().jobs.is_empty());
    m.landing(conaryd());
    let (ok, _) = handed(m.pass().task(), 1);
    assert!(m.pass().jobs.is_empty());
    m.prs(&[described(1, "two")]);
    let (ok2, _) = handed(m.pass().task(), 1);
    assert_ne!(ok, ok2);
    let hub = Hub::new(1, "two");
    hub.say(BOT_REST, &ok2);
    assert_eq!(hub.major(KEY), Some(Major::Cleared));
    // A v1 verdict on its head doesn't count where conaryd lands: it reads
    // none, and nor does the sweep.
    m.comments(1, &[m.verdict(1, "ok", "main", BOT)]);
    m.prs(&[described(1, "three")]);
    let p = m.pass();
    assert_eq!(p.jobs.len(), 1);
    assert!(!p.majors().1.contains("Update foo1"), "{:?}", p.majors());
}

/// Where the steward lands (the record says so, or is handing the repo
/// back to it), the markers are v1, byte for byte as without a record.
#[test]
fn where_the_steward_lands_the_markers_are_v1_as_ever() {
    let task = |landing: Option<Value>| {
        let mut m = Machine::as_app();
        if let Some(l) = landing {
            m.landing(l);
        }
        m.prs(&[described(1, "one"), pr(3)]);
        m.pass().task().to_owned()
    };
    let before = task(None);
    assert!(before.contains(&marker(1, "ok", "main", &mac(1, "ok", "main"))));
    for record in [
        json!({}),
        json!({"sam/r": {"owner": "steward", "generation": 2}}),
        json!({"sam/r": {"owner": "conaryd", "to": "steward", "generation": 3}}),
        json!({"sam/q": {"owner": "conaryd", "generation": 1}}),
    ] {
        assert_eq!(task(Some(record.clone())), before, "{record}");
    }
    // Handed over to conaryd, not yet done: sealed for conaryd, which
    // lands it next.
    let to = json!({"sam/r": {"owner": "steward", "to": "conaryd", "generation": 2}});
    assert!(task(Some(to)).contains("<!-- major-verdict:v2 "));
}

/// A record it can't read seals nothing, in either form, and fails the
/// pass.
#[test]
fn an_unreadable_record_hands_nothing_over() {
    let mut m = Machine::as_app();
    m.landing(conaryd());
    fs::write(m.path("landing.json"), "not json").unwrap();
    m.prs(&[pr(1)]);
    let p = m.sweep(&[]);
    assert_eq!(p.code, Some(1));
    assert!(
        p.jobs.is_empty() && m.key(1, "").is_empty(),
        "{:#?}",
        p.jobs
    );
}
