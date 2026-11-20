//! A first job's awaited PR gets the first label; nothing else does.
use std::fs;
use std::path::Path;

use crate::config::Config;
use crate::job::Job;
use crate::record::TurnFile;

/// A config whose gh (the host's, for baselines) and job gh are one fake:
/// it answers the PR as open, says whether the asker opened it (`authored`
/// in the dir), when (`created`, else long after now), with which labels
/// (`labels`, a JSON array), and logs each call with the App socket it ran
/// under to `calls`.
fn fixture() -> (tempfile::TempDir, Config) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().display();
    let gh = d.path().join("gh");
    burrow_test::executable(
        &gh,
        format!(
            r#"#!/bin/sh
d='{p}'
body=''
case "$*" in *--input*) body=$(cat) ;; esac
echo "$* socket=${{GH_TOKEN_SOCKET:-none}} repo=${{GH_REPO:-none}} $body" >> "$d/calls"
case "$*" in
  */labels*)
    if [ -f "$d/refused" ]; then printf 'HTTP/2.0 403 Forbidden\n\n{{"message":"Resource not accessible by integration"}}'; exit 1; fi
    if [ -f "$d/limited" ]; then printf 'HTTP/2.0 403 Forbidden\nx-ratelimit-remaining: 0\nx-ratelimit-reset: 1\n\n{{"message":"API rate limit exceeded"}}'; exit 1; fi ;;
esac
printf 'HTTP/2.0 200 OK\n\n'
case "$*" in
  *graphql*)
    if [ -f "$d/authored" ]; then mine=true; else mine=false; fi
    created=$(cat "$d/created" 2>/dev/null || echo 2999-01-01T00:00:00Z)
    labels=$(cat "$d/labels" 2>/dev/null || echo '[]')
    echo '{{"data":{{"repository":{{"issueOrPullRequest":{{"__typename":"PullRequest","id":"P","viewerDidAuthor":'$mine',"createdAt":"'$created'","labels":{{"nodes":'$labels'}}}}}}}}}}' ;;
  */labels*) echo '[]' ;;
  */comments*|*/reviews*) echo '[]' ;;
  */check-runs*) echo '{{"check_runs":[]}}' ;;
  */status*) echo '{{"total_count":0}}' ;;
  */pulls/*) echo '{{"state":"open","merged":false,"head":{{"sha":"abc"}}}}' ;;
  *) echo '{{"state":"open","pull_request":{{}}}}' ;;
esac
"#
        ),
    );
    fs::write(
        d.path().join("workers.env"),
        format!("export GH_TOKEN_SOCKET={p}/workers.sock\n"),
    )
    .unwrap();
    let mut v = crate::config::tests::example();
    v["jobs"] = serde_json::json!(d.path().join("jobs"));
    v["tools"]["gh"] = serde_json::json!(gh);
    v["tools"]["jobGh"] = serde_json::json!(gh);
    v["tools"]["bash"] = serde_json::json!("sh");
    v["apps"] = serde_json::json!({"workers": d.path().join("workers.env")});
    v["firstLabel"] = "first".into();
    (d, serde_json::from_value(v).unwrap())
}

/// A job of one turn, given PRIORITY, with its first prompt written.
fn job<'a>(cfg: &'a Config, priority: &str) -> Job<'a> {
    let job = Job::new(cfg, "j");
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.prompt("1"), "the task").unwrap();
    fs::write(job.turn_file("1", TurnFile::Priority), priority).unwrap();
    job
}

fn await_on(cfg: &Config, job: &Job) {
    crate::cmd::record_wait(
        cfg,
        job,
        &[("o/r".into(), "7".into())],
        &["https://github.com/o/r/pull/7".into()],
        None,
    )
    .unwrap();
}

fn labelled(d: &Path) -> Vec<String> {
    fs::read_to_string(d.join("calls"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("repos/o/r/issues/7/labels"))
        .map(str::to_owned)
        .collect()
}

// Fails on the code before crate::first: no await ever labelled its PR.
#[test]
fn a_first_jobs_own_pr_is_labelled_first_when_it_awaits_it() {
    for how in ["pipeline", "unstick", "first"] {
        let (d, cfg) = fixture();
        let job = job(&cfg, if how == "first" { "first" } else { "normal" });
        match how {
            "pipeline" => fs::write(job.dir.join("pipeline"), "").unwrap(),
            "unstick" => crate::record::mark(&job, crate::record::Marker::Unstick)
                .put("")
                .unwrap(),
            _ => {}
        }
        fs::write(d.path().join("authored"), "").unwrap();
        await_on(&cfg, &job);
        let calls = labelled(d.path());
        assert_eq!(calls.len(), 1, "{how}: {calls:?}");
        assert!(calls[0].contains("-X POST"), "{how}: {calls:?}");
        assert!(
            calls[0].contains(r#"{"labels":["first"]}"#),
            "{how}: {calls:?}"
        );
        // As the user's own login: no App's socket.
        assert!(calls[0].contains("socket=none"), "{how}: {calls:?}");
        // Its look named the repo, as gh-token needs for a GraphQL call.
        let calls = fs::read_to_string(d.path().join("calls")).unwrap();
        let look = calls.lines().find(|l| l.contains("graphql")).unwrap();
        assert!(look.contains("repo=o/r"), "{how}: {look}");
    }
}

#[test]
fn an_app_jobs_label_goes_as_its_app() {
    let (d, cfg) = fixture();
    let job = job(&cfg, "first");
    fs::write(job.file(crate::record::JobFile::App), "workers\n").unwrap();
    fs::write(d.path().join("authored"), "").unwrap();
    await_on(&cfg, &job);
    let calls = labelled(d.path());
    let socket = format!("socket={}/workers.sock", d.path().display());
    assert!(calls.len() == 1 && calls[0].contains(&socket), "{calls:?}");
}

#[test]
fn nothing_else_is_labelled() {
    // A normal job, though it opened the PR.
    let (d, cfg) = fixture();
    let normal = job(&cfg, "normal");
    fs::write(d.path().join("authored"), "").unwrap();
    await_on(&cfg, &normal);
    assert!(labelled(d.path()).is_empty());
    assert!(
        !fs::read_to_string(d.path().join("calls"))
            .unwrap()
            .contains("graphql")
    );

    // A first job's await on someone else's PR.
    let (d, cfg) = fixture();
    await_on(&cfg, &job(&cfg, "first"));
    assert!(labelled(d.path()).is_empty());

    // Its identity's, but opened before the job began.
    let (d, cfg) = fixture();
    fs::write(d.path().join("authored"), "").unwrap();
    fs::write(d.path().join("created"), "2001-01-01T00:00:00Z").unwrap();
    await_on(&cfg, &job(&cfg, "first"));
    assert!(labelled(d.path()).is_empty());

    // Its own, labelled already: asked once, not again.
    let (d, cfg) = fixture();
    fs::write(d.path().join("authored"), "").unwrap();
    fs::write(d.path().join("labels"), r#"[{"name":"first"}]"#).unwrap();
    await_on(&cfg, &job(&cfg, "first"));
    assert!(labelled(d.path()).is_empty());
}

#[test]
fn no_label_configured_asks_nothing() {
    let (d, mut cfg) = fixture();
    cfg.first_label = None;
    let job = job(&cfg, "first");
    fs::write(d.path().join("authored"), "").unwrap();
    await_on(&cfg, &job);
    assert!(
        !fs::read_to_string(d.path().join("calls"))
            .unwrap()
            .contains("graphql")
    );
}

#[test]
fn a_label_github_refuses_never_fails_the_await() {
    for why in ["refused", "limited"] {
        let (d, cfg) = fixture();
        let job = job(&cfg, "first");
        fs::write(d.path().join("authored"), "").unwrap();
        fs::write(d.path().join(why), "").unwrap();
        await_on(&cfg, &job);
        assert_eq!(labelled(d.path()).len(), 1, "{why}: asked once");
        // Awaited, with nothing left for quota recovery to do.
        assert!(
            job.file(crate::record::JobFile::AwaitNext).exists(),
            "{why}"
        );
        assert!(
            !job.file(crate::record::JobFile::GithubPending).exists(),
            "{why}"
        );
        assert_eq!(crate::github::take_limit(), None, "{why}");
    }
}
