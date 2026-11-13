//! The API waiter, run as the workflow runs it, against a `gh` stand-in on
//! PATH that serves `FAKE/runs.N.json` and `FAKE/caches.N.json` (the last one
//! once N runs out) and logs each request path.
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const GH: &str = r#"#!/bin/sh
printf '%s\n' "$5" >> "$FAKE/log"
[ -z "$STALL" ] || exec sleep 30
case $5 in
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

struct Case {
    dir: tempfile::TempDir,
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
        let git = |args: &[&str]| {
            let o = Command::new("git")
                .current_dir(p.join("repo"))
                .args(["-c", "user.name=t", "-c", "user.email=t@example.org"])
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{o:?}");
        };
        fs::create_dir_all(p.join("repo")).unwrap();
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&["commit", "-q", "--allow-empty", "-m", "head"]);
        Case { dir }
    }
    fn serve(&self, kind: &str, n: usize, body: &str) {
        fs::write(self.dir.path().join(format!("fake/{kind}.{n}.json")), body).unwrap();
    }
    fn tip(&self) -> String {
        let o = Command::new("git")
            .current_dir(self.dir.path().join("repo"))
            .args(["rev-parse", "HEAD^1"])
            .output()
            .unwrap();
        String::from_utf8(o.stdout).unwrap().trim().to_owned()
    }
    fn run(&self, max: &str, stall: bool) -> Output {
        let p = self.dir.path();
        let path = format!(
            "{}:{}",
            p.join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let mut c = Command::new(env!("CARGO_BIN_EXE_burrow-flake-wait"));
        c.current_dir(p.join("repo"))
            .env("PATH", path)
            .env("FAKE", p.join("fake"))
            .env("RUNNER_TEMP", p.join("tmp"))
            .env("GITHUB_OUTPUT", p.join("out"))
            .env("GITHUB_REPOSITORY", "o/r")
            .env("GITHUB_WORKFLOW", "CI +&%#")
            .env("DEFAULT_BRANCH", "main +&%#")
            .env("KEPT_KEY", "kept-CI +&%#-")
            .env("WAIT_MAX", max)
            .env("WAIT_POLL", "0");
        if stall {
            c.env("STALL", "1");
        }
        c.output().unwrap()
    }
    fn out(&self) -> String {
        fs::read_to_string(self.dir.path().join("out")).unwrap_or_default()
    }
    fn log(&self) -> String {
        fs::read_to_string(self.dir.path().join("fake/log")).unwrap_or_default()
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
        r#"{{"name":"{name}","event":"{event}","head_sha":"{tip}","head_branch":"{branch}","status":"{status}"}}"#
    )
}
fn runs(items: &[String]) -> String {
    format!(r#"{{"workflow_runs":[{}]}}"#, items.join(","))
}
fn cache(case: &Case, key: &str, reference: &str) -> String {
    let _ = case;
    format!(r#"{{"actions_caches":[{{"key":"{key}","ref":"{reference}"}}]}}"#)
}
const BRANCH: &str = "main +&%#";
const WORKFLOW: &str = "CI +&%#";

#[test]
fn nothing_cold_means_nothing_to_wait_for_and_no_request() {
    let c = Case::new("\n");
    let o = c.run("5", false);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("nothing to build cold"));
    assert_eq!(c.log(), "");
}

#[test]
fn a_finished_producer_that_saved_the_exact_key_is_restored_even_if_it_failed() {
    let c = Case::new("a\n");
    let tip = c.tip();
    c.serve(
        "runs",
        1,
        &runs(&[run(BRANCH, &tip, "completed", WORKFLOW, "push")]),
    );
    c.serve(
        "caches",
        1,
        &cache(&c, &format!("kept-CI +&%#-{tip}"), "refs/heads/main +&%#"),
    );
    let o = c.run("5", false);
    assert!(o.status.success(), "{}", text(&o));
    assert!(
        c.out()
            .contains(&format!("key=kept-CI +&%#-{tip}\nsaved=true")),
        "{}",
        c.out()
    );
    // Every dynamic value keeps its bytes on the wire.
    let log = c.log();
    assert!(
        log.contains(&format!(
            "repos/o/r/actions/runs?event=push&head_sha={tip}&branch=main%20%2B%26%25%23&per_page=100"
        )),
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "actions/caches?key=kept-CI%20%2B%26%25%23-{tip}&ref=refs%2Fheads%2Fmain%20%2B%26%25%23&per_page=100"
        )),
        "{log}"
    );
}

#[test]
fn only_the_exact_default_branch_push_justifies_waiting() {
    let c = Case::new("a\n");
    let tip = c.tip();
    let other = [
        run("other", &tip, "in_progress", WORKFLOW, "push"),
        run(BRANCH, &tip, "in_progress", "other", "push"),
        run(BRANCH, &tip, "in_progress", WORKFLOW, "pull_request"),
        run(BRANCH, "0000", "in_progress", WORKFLOW, "push"),
        run(BRANCH, &tip, "completed", WORKFLOW, "push"),
    ];
    c.serve("runs", 1, &runs(&other));
    c.serve("caches", 1, r#"{"actions_caches":[]}"#);
    let started = Instant::now();
    let o = c.run("30", false);
    assert!(o.status.success(), "{}", text(&o));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(text(&o).contains("Gone"), "{}", text(&o));
    assert_eq!(c.out(), "");
}

#[test]
fn a_cache_with_another_key_or_ref_is_not_a_save() {
    let c = Case::new("a\n");
    let tip = c.tip();
    c.serve("runs", 1, &runs(&[]));
    for (n, (key, reference)) in [
        (format!("kept-CI +&%#-{tip}-extra"), "refs/heads/main +&%#"),
        (format!("kept-CI +&%#-{tip}"), "refs/heads/other"),
    ]
    .into_iter()
    .enumerate()
    {
        c.serve("caches", n + 1, &cache(&c, &key, reference));
        let o = c.run("5", false);
        assert!(text(&o).contains("Gone"), "{}", text(&o));
        assert_eq!(c.out(), "");
    }
}

#[test]
fn a_producer_that_saves_and_finishes_between_polls_is_not_missed() {
    let c = Case::new("a\n");
    let tip = c.tip();
    c.serve(
        "runs",
        1,
        &runs(&[run(BRANCH, &tip, "in_progress", WORKFLOW, "push")]),
    );
    c.serve("caches", 1, r#"{"actions_caches":[]}"#);
    c.serve(
        "runs",
        2,
        &runs(&[run(BRANCH, &tip, "completed", WORKFLOW, "push")]),
    );
    c.serve(
        "caches",
        2,
        &cache(&c, &format!("kept-CI +&%#-{tip}"), "refs/heads/main +&%#"),
    );
    let o = c.run("5", false);
    assert!(o.status.success(), "{}", text(&o));
    assert!(c.out().contains("saved=true"), "{}", text(&o));
}

#[test]
fn a_producer_still_running_without_a_save_times_out() {
    let c = Case::new("a\n");
    let tip = c.tip();
    c.serve(
        "runs",
        1,
        &runs(&[run(BRANCH, &tip, "in_progress", WORKFLOW, "push")]),
    );
    c.serve("caches", 1, r#"{"actions_caches":[]}"#);
    let o = c.run("1", false);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("TimedOut"), "{}", text(&o));
    assert_eq!(c.out(), "");
}

#[test]
fn a_stalled_request_ends_at_the_deadline() {
    let c = Case::new("a\n");
    let started = Instant::now();
    let o = c.run("1", true);
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("TimedOut"), "{}", text(&o));
    assert!(started.elapsed() < Duration::from_secs(15));
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
    let o = c.run("5", false);
    assert!(!o.status.success());
    assert!(text(&o).contains("403"), "{}", text(&o));
    assert!(Path::new(c.dir.path()).exists());
}
