//! A major in a repo burrow-landing's record gives conaryd: the verdicts
//! the sweep hands its job are Conary's v2 issuances, which conaryd's own
//! read (`conary_github::flow::read`, on a scripted GitHub here) counts on
//! the PR's revision under the sweep's key, and nothing else. In a repo
//! the steward lands they're the v1 markers they always were.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use conary_github::api::{
    Annotation, ApiError, CheckRun, Comment, Commit, Event, History, PullRequest, Pulls, Review,
    Status, User, WorkflowRun,
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

    /// `repo`'s flow configuration in the flow directory, as the machine
    /// renders it (`OWNER/NAME.json`, lowercased).
    fn flow(&mut self, repo: &str, config: Value) {
        let dir = self.path("flow");
        let path = dir.join(format!("{}.json", repo.to_lowercase()));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, config.to_string()).unwrap();
        self.flow = Some(dir);
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
    fn annotations(&self, _: &str, _: u64) -> Result<Vec<Annotation>, ApiError> {
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
            diff: None,
            open_threads: 0,
            threads: BTreeMap::new(),
            resolved_reviews: BTreeSet::new(),
            updated_at: 0,
            suites: BTreeMap::new(),
            rebase_removed: vec![],
            timeline_cut_at: None,
            since: Default::default(),
        })
    }
    fn pushes(&self, _: &str, _: &str) -> Result<BTreeMap<String, User>, ApiError> {
        Ok(BTreeMap::new())
    }
    fn dispatched_run(&self, _: &str, _: u64) -> Result<Option<WorkflowRun>, ApiError> {
        Err(ApiError::new("not scripted"))
    }
    fn workflow_runs(&self, _: &str, _: &str, _: u64) -> Result<Vec<WorkflowRun>, ApiError> {
        Err(ApiError::new("not scripted"))
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
    // The v1 marker, all the sweep sealed before, is none to conaryd.
    hub.say(BOT_REST, &marker(1, "ok", "main", &mac(1, "ok", "main")));
    assert_eq!(hub.major(KEY), None);
    hub.say(BOT_REST, &ok);
    assert_eq!(hub.major(KEY), Some(Major::Cleared));
    // The sweep reads it as conaryd does: an ok major, which isn't handed
    // over again, nor listed for the owner, as conaryd lands it.
    m.comments(1, &[marked(BOT, &ok)]);
    let p = m.pass();
    assert!(p.jobs.is_empty(), "{:#?}", p.jobs);
    assert_eq!(p.majors(), ("close", "No major updates waiting."));
    // Held, posted after it (a generation on), holds it, and the owner
    // hears of it.
    hub.say(BOT_REST, &held);
    assert_eq!(hub.major(KEY), Some(Major::Held));
    m.comments(1, &[marked(BOT, &ok), marked(BOT, &held)]);
    let p = m.pass();
    assert!(
        p.majors().1.contains("Update foo1 to v2"),
        "{:?}",
        p.majors()
    );
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
    // none, and nor does the sweep, which lists no held one and hands it
    // over once its description moves on.
    m.comments(1, &[m.verdict(1, "held", "main", BOT)]);
    m.prs(&[described(1, "three")]);
    let p = m.pass();
    assert_eq!(p.jobs.len(), 1);
    assert_eq!(p.majors(), ("close", "No major updates waiting."));
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

/// A conaryd-held repo is covered from the landing record alone, without
/// repos.json (or when repos.json doesn't list it or is stale); a
/// steward-held repo (or one being handed back) still reads repos.json.
#[test]
fn a_conaryd_held_repo_is_covered_from_the_landing_record_alone() {
    let body = "This PR contains the following updates: foo3 to v2.";
    for (record, steward) in [
        // No repos.json at all (no steward, or after D deletes it).
        (conaryd(), None),
        // Handover to conaryd under way, no repos.json.
        (
            json!({"sam/r": {"owner": "steward", "to": "conaryd", "generation": 2}}),
            None,
        ),
        // Fresh repos.json that omits this repo.
        (conaryd(), Some((vec![("sam/other", lands("steward"))], 0))),
        // Stale repos.json that omits this repo.
        (
            conaryd(),
            Some((vec![("sam/other", lands("steward"))], 7 * 3600)),
        ),
    ] {
        let mut m = Machine::as_app();
        m.landing(record.clone());
        if let Some((repos, age)) = steward {
            m.steward(&repos, age, json!({}));
        }
        m.prs(&[described(3, body)]);
        let (ok, _) = handed(m.pass().task(), 3);
        m.comments(3, &[marked(BOT, &ok)]);
        let p = m.pass();
        assert!(p.jobs.is_empty(), "{record}: {:#?}", p.jobs);
        assert_eq!(
            p.majors(),
            ("close", "No major updates waiting."),
            "{record}"
        );
    }
    // Where the steward holds it (or is taking it back) and repos.json
    // omits it, nothing lands it and the owner hears of an ok major.
    for record in [
        json!({}),
        json!({"sam/r": {"owner": "steward", "generation": 1}}),
        json!({"sam/r": {"owner": "conaryd", "to": "steward", "generation": 2}}),
    ] {
        let mut m = Machine::as_app();
        m.landing(record.clone());
        m.steward(&[("sam/other", lands("steward"))], 0, json!({}));
        m.prs(&[described(3, body)]);
        let v = m.verdict(3, "ok", "main", BOT);
        m.comments(3, &[v]);
        let p = m.pass();
        assert!(p.jobs.is_empty(), "{record}: {:#?}", p.jobs);
        assert_eq!(p.majors().0, "open", "{record}");
    }
}

/// A conaryd-held repo takes its proofs from its flow configuration, not
/// repos.json: pending proofs aren't CI to wait for, their completion
/// doesn't re-hand the PR, red proofs are red CI, and the job is told how
/// each recipe runs. A steward-held repo still reads repos.json.
#[test]
fn a_conaryd_held_repo_takes_its_proofs_from_its_flow_config() {
    let flow_proofs = json!({
        "settings": {
            "proofs": {
                "agents": {"Build": "checks.x86_64-linux.agents"},
                "gate": {"Run": ["/bin/bash", "-c", "make gate"]},
                "hil": {"Job": "Run `just hil` on the bench box."},
            }
        }
    });
    let pending_proof = json!({
        "__typename": "CheckRun",
        "name": "agents",
        "status": "IN_PROGRESS",
        "conclusion": "",
    });
    let mut m = Machine::as_app();
    m.landing(conaryd());
    m.flow(REPO, flow_proofs.clone());
    // repos.json has a different proof: ignored for a conaryd-held repo.
    m.steward(
        &[(REPO, lands("conaryd"))],
        0,
        json!({"old-proof": "ssh old"}),
    );
    m.prs(&[with(described(16, "body"), pending_proof.clone())]);
    let p = m.pass();
    let task = p.task();
    assert!(
        task.contains("agents isn't CI: it runs on a machine, like this: `nix build .#checks.x86_64-linux.agents -L` on the commit. conaryd builds it itself for each new head and posts the `agents` check: never post that check yourself. Rerun it that way on what you push."),
        "{task}"
    );
    assert!(
        task.contains("gate isn't CI: it runs on a machine, like this: `/bin/bash -c 'make gate'` in a worktree at the commit, with PROOF_REPO, PROOF_HEAD and PROOF_CHECK set. conaryd runs it itself for each new head and posts the `gate` check: never post that check yourself. Rerun it that way on what you push."),
        "{task}"
    );
    assert!(
        task.contains("hil isn't CI: it runs on a machine, like this: Run `just hil` on the bench box. Rerun it that way on what you push."),
        "{task}"
    );
    assert!(!task.contains("old-proof"), "{task}");
    // Its completion doesn't hand the PR over again.
    let mut done_proof = pending_proof.clone();
    done_proof["status"] = "COMPLETED".into();
    done_proof["conclusion"] = "SUCCESS".into();
    done_proof["completedAt"] = "2099-01-02T00:00:00Z".into();
    m.prs(&[with(described(16, "body"), done_proof)]);
    assert!(m.pass().jobs.is_empty());
    // A red proof hands the PR over to fix.
    let mut failed_proof = pending_proof;
    failed_proof["status"] = "COMPLETED".into();
    failed_proof["conclusion"] = "FAILURE".into();
    m.prs(&[with(described(16, "body"), failed_proof)]);
    assert!(
        m.pass()
            .task()
            .contains("a major version bump, and its CI fails")
    );

    // For a steward-held repo, repos.json's proofs are still the ones used.
    let mut m = Machine::as_app();
    m.landing(json!({"sam/r": {"owner": "steward", "generation": 1}}));
    m.flow(REPO, flow_proofs);
    m.steward(
        &[(REPO, lands("steward"))],
        0,
        json!({"old-proof": "ssh old"}),
    );
    m.prs(&[red(6)]);
    let task = m.pass().task().to_owned();
    assert!(task.contains("old-proof isn't CI"), "{task}");
    assert!(!task.contains("agents isn't CI"), "{task}");
}

/// When a flow directory is configured, a missing or malformed flow config
/// for a conaryd-held repo fails closed: no job is started and no cleared
/// major is listed as uncovered.
#[test]
fn a_missing_or_malformed_flow_config_fails_closed() {
    let mut m = Machine::as_app();
    m.landing(conaryd());
    let dir = m.path("flow");
    fs::create_dir_all(&dir).unwrap();
    m.flow = Some(dir.clone());
    m.prs(&[described(1, "body")]);
    let p = m.sweep(&[]);
    assert_eq!(p.code, Some(1));
    assert!(p.jobs.is_empty(), "{:#?}", p.jobs);
    assert_eq!(p.majors().0, "close");

    fs::create_dir_all(dir.join("sam")).unwrap();
    fs::write(dir.join("sam/r.json"), "not json").unwrap();
    let p = m.sweep(&[]);
    assert_eq!(p.code, Some(1));
    assert!(p.jobs.is_empty(), "{:#?}", p.jobs);
    assert_eq!(p.majors().0, "close");
}

/// `--key` makes the key the flow takes, as a seal would, before any major
/// is sealed: one where there's none, a new one for one that isn't a key,
/// and a good one left as it is. It sweeps nothing.
#[test]
fn its_key_is_there_for_the_flow_before_any_seal() {
    use std::os::unix::fs::PermissionsExt;
    let m = Machine::new();
    m.prs(&[pr(1)]);
    let key = || fs::read_to_string(m.state("verdict.key")).unwrap();
    let made = |p: Pass| {
        assert_eq!(p.code, Some(0), "{}", p.err);
        assert!(p.log.is_empty() && p.jobs.is_empty(), "{:?}", p.log);
        assert!(SealKey::from_hex(&key()).is_ok(), "{}", key());
    };
    made(m.sweep(&["--key"]));
    assert_eq!(key(), KEY);
    fs::remove_file(m.state("verdict.key")).unwrap();
    made(m.sweep(&["--key"]));
    let mode = fs::metadata(m.state("verdict.key"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let first = key();
    made(m.sweep(&["--key"]));
    assert_eq!(key(), first);
    fs::write(m.state("verdict.key"), "short").unwrap();
    made(m.sweep(&["--key"]));
    assert_ne!(key(), first);
    // Only alone.
    assert_eq!(m.sweep(&["--key", "sam/r"]).code, Some(2));
}
