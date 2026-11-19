//! The API waiter, run as the workflow runs it, against a `gh` stand-in on
//! PATH that serves `FAKE/runs.N.json`, `FAKE/jobs.N.json` and
//! `FAKE/caches.N.json` (the last one once N runs out) and logs each request
//! path.
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const GH: &str = r#"#!/bin/sh
printf '%s\n' "$5" >> "$FAKE/log"
[ -z "$STALL" ] || exec sleep 30
case $5 in
  */jobs\?*) kind=jobs ;;
  */actions/runs\?*) kind=runs ;;
  */actions/caches\?*) kind=caches ;;
  *) exit 2 ;;
esac
n=$(cat "$FAKE/$kind.count" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "$FAKE/$kind.count"
f="$FAKE/$kind.$n.json"
[ -f "$f" ] || f=$(ls "$FAKE/$kind".*.json | sort -V | tail -1)
if [ -f "$FAKE/status" ]; then
  printf 'HTTP/2.0 %s X\n\n' "$(cat "$FAKE/status")"; cat "$f"; exit 1
fi
printf 'HTTP/2.0 200 OK\n\n'
cat "$f"
"#;

/// The base tip the merge step says it merged onto.
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";
const BRANCH: &str = "main +&%#";
const WORKFLOW: &str = "CI +&%#";
/// This leg's job, as the workflow names it.
const LEG: &str = "leg 2 of 4";

struct Case {
    dir: tempfile::TempDir,
    env: Vec<(&'static str, String)>,
}
impl Case {
    fn new(cold: &str) -> Case {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        fs::create_dir_all(p.join("fake")).unwrap();
        fs::create_dir_all(p.join("bin")).unwrap();
        fs::create_dir_all(p.join("tmp")).unwrap();
        burrow_test::executable(p.join("bin/gh"), GH);
        fs::write(p.join("tmp/cold.txt"), cold).unwrap();
        Case {
            dir,
            env: Vec::new(),
        }
    }
    fn serve(&self, kind: &str, n: usize, body: &str) {
        fs::write(self.dir.path().join(format!("fake/{kind}.{n}.json")), body).unwrap();
    }
    fn set(&mut self, k: &'static str, v: &str) {
        self.env.push((k, v.to_owned()));
    }
    fn run(&self) -> Output {
        let p = self.dir.path();
        let path = format!(
            "{}:{}",
            p.join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let mut c = Command::new(env!("CARGO_BIN_EXE_burrow-flake-wait"));
        c.current_dir(p)
            .env("PATH", path)
            .env("FAKE", p.join("fake"))
            .env("RUNNER_TEMP", p.join("tmp"))
            .env("GITHUB_OUTPUT", p.join("out"))
            .env("GITHUB_REPOSITORY", "o/r")
            .env("GITHUB_WORKFLOW", WORKFLOW)
            .env("DEFAULT_BRANCH", BRANCH)
            .env("TIP", TIP)
            .env("LEG_JOB", LEG)
            .env("KEPT_KEY", "kept-CI +&%#-")
            .env("WAIT_POLL", "0")
            .env("WAIT_REQUEST", "5")
            .envs(self.env.iter().map(|(k, v)| (k, v)));
        c.output().unwrap()
    }
    fn out(&self) -> String {
        fs::read_to_string(self.dir.path().join("out")).unwrap_or_default()
    }
    fn log(&self) -> String {
        fs::read_to_string(self.dir.path().join("fake/log")).unwrap_or_default()
    }
    /// How many requests it made for `kind`.
    fn asked(&self, kind: &str) -> usize {
        self.log().lines().filter(|l| l.contains(kind)).count()
    }
}
fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}
fn run(branch: &str, tip: &str, status: &str, name: &str, event: &str) -> String {
    format!(
        r#"{{"id":7,"name":"{name}","event":"{event}","head_sha":"{tip}","head_branch":"{branch}","status":"{status}"}}"#
    )
}
fn runs(items: &[String]) -> String {
    format!(r#"{{"workflow_runs":[{}]}}"#, items.join(","))
}
/// A run's jobs, each a name and a status, started just now.
fn jobs(items: &[(&str, &str)]) -> String {
    let now: Vec<_> = items.iter().map(|&(n, s)| (n, s, 0)).collect();
    jobs_since(&now)
}
/// A run's jobs, each a name, a status and how many seconds ago it
/// started, as GitHub writes the time.
fn jobs_since(items: &[(&str, &str, i64)]) -> String {
    let jobs: Vec<String> = items
        .iter()
        .map(|(name, status, ago)| {
            let started = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(*ago);
            let started = started.strftime("%Y-%m-%dT%H:%M:%SZ");
            format!(r#"{{"name":"{name}","status":"{status}","started_at":"{started}"}}"#)
        })
        .collect();
    format!(r#"{{"jobs":[{}]}}"#, jobs.join(","))
}
fn cache(key: &str, reference: &str) -> String {
    format!(r#"{{"actions_caches":[{{"key":"{key}","ref":"{reference}"}}]}}"#)
}
fn saved() -> String {
    cache(&format!("kept-CI +&%#-{TIP}"), "refs/heads/main +&%#")
}
const NONE: &str = r#"{"actions_caches":[]}"#;

/// The base push, running.
fn going(c: &Case) {
    c.serve(
        "runs",
        1,
        &runs(&[run(BRANCH, TIP, "in_progress", WORKFLOW, "push")]),
    );
}

#[test]
fn nothing_cold_means_nothing_to_wait_for_and_no_request() {
    let c = Case::new("\n");
    let o = c.run();
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("nothing to build cold"));
    assert_eq!(c.log(), "");
}

#[test]
fn a_finished_producer_that_saved_the_exact_key_is_restored_even_if_it_failed() {
    let c = Case::new("a\n");
    c.serve(
        "runs",
        1,
        &runs(&[run(BRANCH, TIP, "completed", WORKFLOW, "push")]),
    );
    c.serve("caches", 1, &saved());
    let o = c.run();
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        c.out()
            .contains(&format!("key=kept-CI +&%#-{TIP}\nsaved=true")),
        "{}",
        c.out()
    );
    // Every dynamic value keeps its bytes on the wire.
    let log = c.log();
    assert!(
        log.contains(&format!(
            "repos/o/r/actions/runs?event=push&head_sha={TIP}&branch=main%20%2B%26%25%23&per_page=100"
        )),
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "actions/caches?key=kept-CI%20%2B%26%25%23-{TIP}&ref=refs%2Fheads%2Fmain%20%2B%26%25%23&per_page=100"
        )),
        "{log}"
    );
    assert_eq!(c.asked("/jobs?"), 0, "{log}");
}

#[test]
fn only_the_exact_default_branch_push_justifies_waiting() {
    let c = Case::new("a\n");
    let other = [
        run("other", TIP, "in_progress", WORKFLOW, "push"),
        run(BRANCH, TIP, "in_progress", "other", "push"),
        run(BRANCH, TIP, "in_progress", WORKFLOW, "pull_request"),
        run(BRANCH, "0000", "in_progress", WORKFLOW, "push"),
        run(BRANCH, TIP, "completed", WORKFLOW, "push"),
    ];
    c.serve("runs", 1, &runs(&other));
    c.serve("jobs", 1, &jobs(&[(LEG, "in_progress")]));
    c.serve("caches", 1, NONE);
    let started = Instant::now();
    let o = c.run();
    assert!(o.status.success(), "{}", text(&o));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(text(&o).contains("Gone"), "{}", text(&o));
    assert_eq!(c.out(), "");
    assert_eq!(c.asked("/jobs?"), 0, "{}", c.log());
}

#[test]
fn a_cache_with_another_key_or_ref_is_not_a_save() {
    let c = Case::new("a\n");
    c.serve("runs", 1, &runs(&[]));
    for (n, (key, reference)) in [
        (format!("kept-CI +&%#-{TIP}-extra"), "refs/heads/main +&%#"),
        (format!("kept-CI +&%#-{TIP}"), "refs/heads/other"),
    ]
    .into_iter()
    .enumerate()
    {
        c.serve("caches", n + 1, &cache(&key, reference));
        let o = c.run();
        assert!(text(&o).contains("Gone"), "{}", text(&o));
        assert_eq!(c.out(), "");
    }
}

/// The base push's leg on a runner is waited for to its end inside its
/// deadline, however long this leg has waited: the waiter had a cap of its
/// own (WAIT_MAX, an hour by default) and, past it, built cold anyway, the
/// hour lost (#929). Its jobs are what it polls; the cache, only once the
/// leg has stopped.
#[test]
fn a_leg_on_a_runner_is_waited_for_to_its_end_and_restored() {
    let mut c = Case::new("a\n");
    going(&c);
    for n in 1..=3 {
        c.serve(
            "jobs",
            n,
            &jobs(&[
                ("flake / leg 1 of 4", "completed"),
                (&format!("flake / {LEG}"), "in_progress"),
            ]),
        );
    }
    c.serve(
        "jobs",
        4,
        &jobs(&[(&format!("flake / {LEG}"), "completed")]),
    );
    c.serve("caches", 1, &saved());
    c.set("WAIT_POLL", "1");
    c.set("WAIT_MAX", "1");
    let o = c.run();
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("Saved"), "{}", text(&o));
    assert!(c.out().contains("saved=true"), "{}", text(&o));
    assert_eq!(c.asked("/jobs?"), 4, "{}", c.log());
    assert_eq!(c.asked("/caches?"), 1, "{}", c.log());
    assert_eq!(c.asked("/runs?"), 1, "{}", c.log());
    assert!(
        c.log()
            .contains("repos/o/r/actions/runs/7/jobs?per_page=100")
    );
}

/// A leg still queued may be queued for the runner this one holds, and one
/// not there yet (its run's lane job still going) the same: neither is
/// waited for, and nothing else's leg counts.
#[test]
fn a_leg_without_a_runner_is_not_waited_for() {
    for served in [
        jobs(&[
            ("flake / leg 1 of 4", "in_progress"),
            (&format!("flake / {LEG}"), "queued"),
        ]),
        jobs(&[("flake / lane into main", "in_progress")]),
        jobs(&[
            ("flake / leg 2 of 40", "in_progress"),
            ("flake / leg 12 of 4", "in_progress"),
        ]),
        jobs(&[(LEG, "waiting")]),
    ] {
        let c = Case::new("a\n");
        going(&c);
        c.serve("jobs", 1, &served);
        c.serve("caches", 1, NONE);
        let o = c.run();
        assert!(o.status.success(), "{}", text(&o));
        assert!(text(&o).contains("Queued"), "{served}: {}", text(&o));
        assert_eq!(c.asked("/jobs?"), 1, "{served}");
        assert_eq!(c.out(), "");
    }
}

#[test]
fn a_leg_that_ended_without_saving_is_gone() {
    let c = Case::new("a\n");
    going(&c);
    c.serve("jobs", 1, &jobs(&[(LEG, "completed")]));
    c.serve("caches", 1, NONE);
    let o = c.run();
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("Gone"), "{}", text(&o));
    assert_eq!(c.out(), "");
}

/// A request cut off by its deadline fails the soft step, so the cut-off
/// is reported rather than passed off as a wait that ended (#929).
#[test]
fn a_stalled_request_fails_the_soft_step_at_its_deadline() {
    let mut c = Case::new("a\n");
    c.set("STALL", "1");
    c.set("WAIT_REQUEST", "1");
    let started = Instant::now();
    let o = c.run();
    assert!(!o.status.success(), "{}", text(&o));
    assert!(started.elapsed() < Duration::from_secs(15));
    assert_eq!(c.out(), "");
}

#[test]
fn without_the_merged_base_it_fails_rather_than_guess() {
    let mut c = Case::new("a\n");
    c.set("TIP", "");
    let o = c.run();
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("TIP"), "{}", text(&o));
    assert_eq!(c.log(), "");
}

#[test]
fn denied_api_permission_fails_the_soft_step() {
    let c = Case::new("a\n");
    c.serve(
        "runs",
        1,
        r#"{"message":"Resource not accessible by integration"}"#,
    );
    fs::write(c.dir.path().join("fake/status"), "403").unwrap();
    let o = c.run();
    assert!(!o.status.success());
    assert!(text(&o).contains("403"), "{}", text(&o));
    assert!(Path::new(c.dir.path()).exists());
}

/// #1102, replayed: main went red at a push whose leg 2 sat compiling for
/// hours, and the red fix's PR, merged onto it, waited on that leg with no
/// end in sight. A base push's leg past its deadline (4 hours from its own
/// start) isn't waited for: the PR's leg builds cold at once, with nothing
/// to restore, and says why. The push's other legs, failed, change nothing.
#[test]
fn a_red_fix_on_a_push_whose_leg_is_past_its_deadline_builds_cold_at_once() {
    let c = Case::new("codex-0.167.0\nburrow-proj\n");
    going(&c);
    let late = 4 * 60 * 60 + 60;
    c.serve(
        "jobs",
        1,
        &jobs_since(&[
            ("check / leg 1 of 4", "completed", late),
            (&format!("check / {LEG}"), "in_progress", late),
            ("check / leg 3 of 4", "completed", late),
            ("check / leg 4 of 4", "completed", late),
        ]),
    );
    c.serve("caches", 1, NONE);
    let started = Instant::now();
    let o = c.run();
    assert!(o.status.success(), "{}", text(&o));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(text(&o).contains("Late"), "{}", text(&o));
    assert!(text(&o).contains("building cold"), "{}", text(&o));
    // Nothing for the restore after it: the gate builds cold.
    assert_eq!(c.out(), "");
    assert_eq!(c.asked("/jobs?"), 1, "{}", c.log());
    assert_eq!(c.asked("/caches?"), 1, "{}", c.log());
}

/// The deadline is the producer leg's, from its start: a leg waited for
/// while inside it is given up on once past it, poll by poll, not after
/// the waiter's own time.
#[test]
fn a_leg_that_runs_past_its_deadline_while_waited_for_is_given_up() {
    let mut c = Case::new("a\n");
    going(&c);
    c.serve(
        "jobs",
        1,
        &jobs(&[(&format!("flake / {LEG}"), "in_progress")]),
    );
    c.serve("caches", 1, NONE);
    c.set("WAIT_POLL", "1");
    c.set("WAIT_DEADLINE", "3");
    let started = Instant::now();
    let o = c.run();
    assert!(o.status.success(), "{}", text(&o));
    assert!(started.elapsed() >= Duration::from_secs(2), "{}", text(&o));
    assert!(started.elapsed() < Duration::from_secs(15), "{}", text(&o));
    assert!(text(&o).contains("Late"), "{}", text(&o));
    assert!(c.asked("/jobs?") >= 3, "{}", c.log());
    assert_eq!(c.out(), "");
}

/// A late leg's run may still have saved the exact key (an earlier
/// attempt): that save is restored, not built again.
#[test]
fn a_late_legs_exact_save_is_still_restored() {
    let c = Case::new("a\n");
    going(&c);
    c.serve("jobs", 1, &jobs_since(&[(LEG, "in_progress", 5 * 60 * 60)]));
    c.serve("caches", 1, &saved());
    let o = c.run();
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("Saved"), "{}", text(&o));
    assert!(c.out().contains("saved=true"), "{}", c.out());
}

/// A running leg whose start can't be read can't be held to its deadline,
/// so the soft step fails and says so rather than wait without one.
#[test]
fn a_running_leg_with_no_start_fails_the_soft_step() {
    let c = Case::new("a\n");
    going(&c);
    c.serve(
        "jobs",
        1,
        &format!(r#"{{"jobs":[{{"name":"{LEG}","status":"in_progress","started_at":null}}]}}"#),
    );
    c.serve("caches", 1, NONE);
    let o = c.run();
    assert!(!o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("start"), "{}", text(&o));
    assert_eq!(c.out(), "");
}
