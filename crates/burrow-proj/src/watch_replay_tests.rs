//! The 2026-11-13 CI queue (#1043), replayed through registration and the
//! watcher against a stand-in gh: a wait that declares what ends it gets a
//! turn when that happens, once, and for nothing else.

use std::fs;

use serde_json::{Value, json};

use super::look;
use crate::config::Config;
use crate::ends::{End, Ends};
use crate::job::Job;
use crate::now;

/// A PR as the stand-in gh serves it.
#[derive(Clone)]
struct Pr {
    state: &'static str,
    merged: bool,
    head: &'static str,
    comments: usize,
    review_comments: usize,
    reviews: Vec<Value>,
    /// Each check run: name, status, conclusion.
    runs: Vec<(&'static str, &'static str, Option<&'static str>)>,
}

impl Pr {
    fn open(head: &'static str) -> Pr {
        Pr {
            state: "open",
            merged: false,
            head,
            comments: 0,
            review_comments: 0,
            reviews: vec![],
            runs: vec![],
        }
    }

    fn serve(&self, dir: &std::path::Path) {
        let api = dir.join("api");
        let _ = fs::remove_dir_all(&api);
        fs::create_dir_all(&api).unwrap();
        let put = |path: &str, v: Value| {
            fs::write(api.join(path.replace('/', "_")), v.to_string()).unwrap()
        };
        put(
            "repos/o/r/issues/7",
            json!({"pull_request": {}, "state": self.state}),
        );
        put(
            "repos/o/r/issues/7/comments",
            json!(
                (0..self.comments)
                    .map(|i| json!({"body": format!("steward note {i}")}))
                    .collect::<Vec<_>>()
            ),
        );
        put(
            "repos/o/r/pulls/7",
            json!({"state": self.state, "merged": self.merged, "head": {"sha": self.head}, "review_comments": self.review_comments}),
        );
        put("repos/o/r/pulls/7/reviews", json!(self.reviews));
        put(
            &format!("repos/o/r/commits/{}/status", self.head),
            json!({"total_count": 0, "state": "pending", "statuses": []}),
        );
        put(
            &format!("repos/o/r/commits/{}/check-runs", self.head),
            json!({"check_runs": self.runs.iter().map(|(name, status, conclusion)| json!({"name": name, "status": status, "conclusion": conclusion})).collect::<Vec<_>>()}),
        );
    }
}

fn clean_review(head: &str) -> Value {
    json!({"state": "COMMENTED", "body": format!("**Codex review** of {head}: nothing found.\n\n<!-- codex-review:v1 {{\"commit_sha\":\"{head}\",\"findings\":0}} -->")})
}

fn findings_review(head: &str, k: u64) -> Value {
    json!({"state": "COMMENTED", "body": format!("**Codex review** of {head}: {k} findings.\n\n<!-- codex-review:v1 {{\"commit_sha\":\"{head}\",\"findings\":{k}}} -->")})
}

struct Fixture {
    dir: tempfile::TempDir,
    cfg: Config,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        // gh api -i -X GET PATH: PATH's file, without its query.
        burrow_test::executable(
            &gh,
            r#"#!/bin/sh
ep=${5%%\?*}
f="$(dirname "$0")/api/$(printf %s "$ep" | tr / _)"
[ -f "$f" ] || exit 1
printf 'HTTP/2.0 200 OK\n\n'
cat "$f"
"#,
        );
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        cfg.wake = dir.path().join("wake.json");
        cfg.waits = None;
        cfg.kick.clear();
        cfg.tools.gh = gh;
        cfg.tools.systemctl = "false".into();
        cfg.tools.systemd_run = "true".into();
        Fixture { dir, cfg }
    }

    fn serve(&self, pr: &Pr) {
        pr.serve(self.dir.path());
    }

    fn job(&self) -> Job<'_> {
        let job = Job::new(&self.cfg, "j");
        if !job.dir.exists() {
            fs::create_dir_all(&job.dir).unwrap();
            crate::policy::bind(
                &job,
                crate::policy::Identity::Local,
                crate::policy::Kind::Manual,
            )
            .unwrap();
            job.set_turns(1).unwrap();
        }
        job
    }

    /// The job's latest turn ends awaiting o/r#7 on `ends`, its baseline
    /// taken from what gh serves now, as `proj await` takes it.
    fn awaits(&self, ends: &[End]) {
        let job = self.job();
        let n = job.turns().unwrap();
        fs::write(job.turn(&n, "outcome"), "done\n").unwrap();
        fs::write(job.turn(&n, "end"), "await\n").unwrap();
        let ends = Ends(ends.iter().copied().collect());
        let line = crate::cmd::baselines(
            &self.cfg,
            &[("o/r".into(), "7".into())],
            &["o/r#7".into()],
            &ends,
            None,
        )
        .unwrap();
        fs::write(job.path("await"), line).unwrap();
    }

    /// One look by the watcher; how many turns it started. A turn it
    /// starts takes the job's waits as its run starts (turn.rs), and here
    /// ends on nothing more until the test awaits again.
    fn look(&self) -> usize {
        let job = self.job();
        let before: usize = job.turns().unwrap().parse().unwrap();
        look(&self.cfg, "").unwrap();
        let after: usize = job.turns().unwrap().parse().unwrap();
        if after > before {
            let _ = fs::remove_file(job.path("await"));
            let n = job.turns().unwrap();
            fs::write(job.turn(&n, "outcome"), "done\n").unwrap();
            fs::write(job.turn(&n, "end"), "done\n").unwrap();
        }
        after - before
    }

    /// The prompt of the latest turn.
    fn prompt(&self) -> String {
        let job = self.job();
        fs::read_to_string(job.path(&format!("prompt.{}", job.turns().unwrap()))).unwrap()
    }
}

// A job awaiting CI hears nothing of a clean review, the steward's comment,
// its base merged in, one leg passing, one cancelled or skipped; a red leg
// wakes it once while another still runs; all green on the next head wakes
// it once; and its merge, once.
#[test]
fn a_ci_wait_wakes_only_on_a_verdict_or_its_end() {
    let f = Fixture::new();
    let mut pr = Pr::open("aaa");
    pr.runs = vec![("build", "in_progress", None), ("test", "queued", None)];
    f.serve(&pr);
    f.awaits(&[End::Ci]);

    let mut quiet = vec![];
    pr.reviews.push(clean_review("aaa"));
    quiet.push(("a clean review", pr.clone()));
    pr.comments += 1;
    quiet.push(("the steward's comment", pr.clone()));
    pr.head = "bbb";
    pr.runs = vec![];
    quiet.push(("a base merge, its checks not started", pr.clone()));
    pr.runs = vec![("build", "queued", None), ("test", "queued", None)];
    quiet.push(("the merge's checks queued", pr.clone()));
    pr.runs[0] = ("build", "completed", Some("success"));
    quiet.push(("one leg passing", pr.clone()));
    pr.runs.push(("lint", "completed", Some("skipped")));
    pr.runs.push(("old", "completed", Some("cancelled")));
    quiet.push(("a leg skipped, one cancelled", pr.clone()));
    for (what, state) in quiet {
        f.serve(&state);
        assert_eq!(f.look(), 0, "{what} woke the job");
    }

    // Red while another leg runs: once.
    pr.runs[1] = ("test", "completed", Some("failure"));
    pr.runs.push(("e2e", "in_progress", None));
    f.serve(&pr);
    assert_eq!(f.look(), 1, "a red leg");
    assert!(
        f.prompt().contains("CI on bbb failed: test"),
        "{}",
        f.prompt()
    );
    assert_eq!(f.look(), 0, "the wait is over");
    // Awaited again on the same red head (a rerun, say): the red it has
    // seen isn't news, another leg's failure is.
    f.awaits(&[End::Ci]);
    assert_eq!(f.look(), 0, "the same red");
    pr.runs[4] = ("e2e", "completed", Some("success"));
    f.serve(&pr);
    assert_eq!(f.look(), 0, "the same red, the rest passed");
    pr.runs.push(("docs", "completed", Some("failure")));
    f.serve(&pr);
    assert_eq!(f.look(), 1, "another leg red");

    // It pushes a fix and awaits CI again: all green, once.
    pr.head = "ccc";
    pr.runs = vec![("build", "queued", None), ("test", "queued", None)];
    f.serve(&pr);
    f.awaits(&[End::Ci]);
    pr.runs = vec![
        ("build", "completed", Some("success")),
        ("test", "in_progress", None),
    ];
    f.serve(&pr);
    assert_eq!(f.look(), 0, "partly done");
    pr.runs[1] = ("test", "completed", Some("success"));
    pr.runs.push(("lint", "completed", Some("skipped")));
    f.serve(&pr);
    assert_eq!(f.look(), 1, "all green");
    assert!(
        f.prompt().contains("CI on ccc is all green"),
        "{}",
        f.prompt()
    );

    // Green already when awaited: a clean review and a comment are nothing,
    // the merge is the news.
    f.awaits(&[End::Ci]);
    pr.reviews.push(clean_review("ccc"));
    pr.comments += 1;
    f.serve(&pr);
    assert_eq!(f.look(), 0, "nothing new on a green head");
    pr.state = "closed";
    pr.merged = true;
    f.serve(&pr);
    assert_eq!(f.look(), 1, "merged");
    assert!(f.prompt().contains("o/r#7: merged"), "{}", f.prompt());
}

// Closed without merging ends a CI wait, once.
#[test]
fn a_ci_wait_ends_when_its_pr_closes() {
    let f = Fixture::new();
    let mut pr = Pr::open("aaa");
    pr.runs = vec![("build", "queued", None)];
    f.serve(&pr);
    f.awaits(&[End::Ci]);
    pr.state = "closed";
    f.serve(&pr);
    assert_eq!(f.look(), 1);
    assert!(f.prompt().contains("o/r#7: closed"), "{}", f.prompt());
}

// A review wait hears new findings or threads, and nothing of clean
// reviews, comments, pushes or CI.
#[test]
fn a_review_wait_wakes_only_on_findings() {
    let f = Fixture::new();
    let mut pr = Pr::open("aaa");
    pr.runs = vec![("build", "queued", None)];
    f.serve(&pr);
    f.awaits(&[End::Review]);
    pr.reviews.push(clean_review("aaa"));
    pr.comments += 1;
    pr.head = "bbb";
    pr.runs = vec![("build", "completed", Some("failure"))];
    f.serve(&pr);
    assert_eq!(f.look(), 0, "a clean review, a comment, a push, red CI");
    pr.reviews.push(findings_review("bbb", 2));
    pr.review_comments += 2;
    f.serve(&pr);
    assert_eq!(f.look(), 1, "findings");
    assert!(
        f.prompt().contains("1 new review(s) with findings"),
        "{}",
        f.prompt()
    );

    // Threads alone (a finding on a whole file, a reply) are news too.
    f.awaits(&[End::Review]);
    pr.review_comments += 1;
    f.serve(&pr);
    assert_eq!(f.look(), 1, "a thread");

    // Both: either ends it.
    f.awaits(&[End::Ci, End::Review]);
    pr.reviews.push(clean_review("bbb"));
    f.serve(&pr);
    assert_eq!(f.look(), 0, "a clean review on a red head already seen");
    pr.head = "ccc";
    pr.runs = vec![("build", "completed", Some("success"))];
    f.serve(&pr);
    assert_eq!(f.look(), 1, "green");
}

// A wait on any change, written as every proj before this one wrote it (no
// condition, none of the derived facts), still wakes on a comment, and
// isn't woken by the facts it lacks; one registered now on any change is
// the same. Each look reads the wait afresh from its file, as after a
// restart.
#[test]
fn a_wait_on_any_change_and_one_from_before_keep_their_meaning() {
    let f = Fixture::new();
    let mut pr = Pr::open("aaa");
    pr.runs = vec![("build", "queued", None)];
    f.serve(&pr);
    f.awaits(&[]);
    let job = f.job();
    let line = fs::read_to_string(job.path("await")).unwrap();
    assert!(
        !line.contains("\"ci\"") && !line.contains("\"ends\""),
        "{line}"
    );
    assert_eq!(f.look(), 0, "nothing changed");
    pr.comments += 1;
    f.serve(&pr);
    assert_eq!(f.look(), 1, "a comment");

    // As an older proj wrote it.
    let n = job.turns().unwrap();
    fs::write(job.turn(&n, "outcome"), "done\n").unwrap();
    fs::write(job.turn(&n, "end"), "await\n").unwrap();
    let old = json!({"state": "open", "merged": false, "head": "aaa", "comments": 1, "review_comments": 0, "reviews": {}, "checks": "running"});
    fs::write(job.path("await"), format!("o/r#7\t{}\t{old}\n", now())).unwrap();
    assert_eq!(f.look(), 0, "an old baseline isn't news for what it lacks");
    pr.reviews.push(clean_review("aaa"));
    f.serve(&pr);
    assert_eq!(f.look(), 1, "a review is news to it");
}

// A CI wait past awaitHours whose checks are still queued isn't resumed for
// its deadline: it's watched on. One whose checks finished without a
// verdict, or a wait on any change, is told nothing changed.
#[test]
fn a_queue_past_the_deadline_is_no_turn() {
    let f = Fixture::new();
    let mut pr = Pr::open("aaa");
    pr.runs = vec![("build", "queued", None)];
    f.serve(&pr);
    f.awaits(&[End::Ci]);
    let job = f.job();
    let stale = now() - f.cfg.await_hours as i64 * 3600 - 10;
    let age = |line: String| {
        let mut l = line.splitn(3, '\t');
        let t = l.next().unwrap().to_owned();
        let _ = l.next();
        format!("{t}\t{stale}\t{}", l.next().unwrap())
    };
    let line = fs::read_to_string(job.path("await")).unwrap();
    fs::write(job.path("await"), age(line)).unwrap();
    assert_eq!(f.look(), 0, "CI still queued");
    let since: i64 = fs::read_to_string(job.path("await"))
        .unwrap()
        .split('\t')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    assert!(since > stale, "watched on from now");

    // Cancelled, with nothing left to come: told so at the deadline.
    pr.runs = vec![("build", "completed", Some("cancelled"))];
    f.serve(&pr);
    let line = fs::read_to_string(job.path("await")).unwrap();
    fs::write(job.path("await"), age(line)).unwrap();
    assert_eq!(f.look(), 1);
    assert!(f.prompt().contains("nothing changed in"), "{}", f.prompt());
}

// An await GitHub's quota held up keeps its condition through the reset:
// the saved intent carries it to the baseline the replay takes.
#[test]
fn a_saved_await_keeps_its_condition() {
    let f = Fixture::new();
    let mut pr = Pr::open("aaa");
    pr.runs = vec![("build", "queued", None)];
    f.serve(&pr);
    let job = f.job();
    let pending = crate::github_quota::Pending {
        targets: vec!["o/r#7".into()],
        retry_at: 1,
        turn: job.turns().unwrap(),
        baselines: String::new(),
        ends: [("o/r#7".to_owned(), Ends([End::Ci].into_iter().collect()))].into(),
    };
    fs::write(
        job.path(crate::github_quota::PENDING),
        serde_json::to_vec(&pending).unwrap(),
    )
    .unwrap();
    crate::github_quota::replay(&f.cfg, &job).unwrap();
    let line = fs::read_to_string(job.path("await")).unwrap();
    assert!(line.contains(r#""ends":["ci"]"#), "{line}");
    pr.reviews.push(clean_review("aaa"));
    pr.comments += 1;
    f.serve(&pr);
    assert_eq!(f.look(), 0, "a clean review and a comment");
    pr.runs = vec![("build", "completed", Some("success"))];
    f.serve(&pr);
    assert_eq!(f.look(), 1, "green");
}
