//! Exercise the source as a service, through its real gh boundary and kick
//! socket. API failures preserve the last watch; attempts have separate
//! observations; a new clean default run clears the repeated failure.
use serde_json::json;
use std::{fs, process::Command, sync::Arc};

#[test]
fn degradation_source_reads_runs_and_preserves_stalls_on_api_failure() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (port, _) = burrow_events::http::listen(0, Arc::new(|_| 202)).unwrap();
    let watch = d.join("watch.json");
    let cfg = d.join("config.json");
    fs::write(
        &cfg,
        json!({"file":watch,"repos":{"sam/repo":["ci.yml"]},"prThreshold":3,"authors":["sam"],
        "kick":format!("http://127.0.0.1:{port}/kick/watchdog")})
        .to_string(),
    )
    .unwrap();
    let at = jiff::Timestamp::now().to_string();
    let runs = json!({"workflow_runs":[
        {"id":2,"run_attempt":1,"created_at":at,"html_url":"https://example.org/2"},
        {"id":1,"run_attempt":1,"created_at":at,"html_url":"https://example.org/1"}
    ]});
    fs::write(d.join("runs.json"), runs.to_string()).unwrap();
    let script = r#"#!/bin/sh
case "$2" in
  repos/sam/repo) echo '{"default_branch":"main"}' ;;
  *event=pull_request*) echo '{"workflow_runs":[]}' ;;
  *workflows/ci.yml/runs*) cat "$FIXTURES/runs.json" ;;
  *runs/*/jobs*) echo '{"jobs":[{"steps":[{"name":"degraded/cache","conclusion":"success"}]}]}' ;;
  *) exit 1 ;;
esac
"#;
    burrow_test::executable(d.join("gh"), script);
    let path = std::env::join_paths(
        std::iter::once(d.to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_burrow-degradations"))
            .env("PATH", &path)
            .env("FIXTURES", d)
            .env("DEGRADATIONS_CONFIG", &cfg)
            .output()
            .unwrap()
    };
    let out = run();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let old = fs::read(&watch).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&old).unwrap();
    assert!(value["items"]["sam/repo/ci.yml/cache"].is_object());
    burrow_test::executable(d.join("gh"), "#!/bin/sh\nexit 1\n");
    assert!(!run().status.success());
    assert_eq!(fs::read(&watch).unwrap(), old);
    // A clean, later run is a distinct ID. Previously cached observations
    // don't leak across it or turn a skipped reporter into a degradation.
    burrow_test::executable(
        d.join("gh"),
        script.replace(
            "*runs/*/jobs*)",
            "*runs/3/jobs*) echo '{\"jobs\":[]}' ;;\n  *runs/*/jobs*)",
        ),
    );
    let clean = json!({"workflow_runs":[
        {"id":3,"run_attempt":1,"created_at":at,"html_url":"https://example.org/3"},
        {"id":2,"run_attempt":1,"created_at":at,"html_url":"https://example.org/2"}
    ]});
    fs::write(d.join("runs.json"), clean.to_string()).unwrap();
    assert!(run().status.success());
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&watch).unwrap()).unwrap();
    assert!(value["items"].as_object().unwrap().is_empty());
}

/// PR runs by outsiders (or forks) never supply keys, even three times;
/// a trusted run's degradation survives a partial rerun of another leg.
#[test]
fn pr_runs_need_a_configured_author_and_partial_reruns_keep_signals() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (port, _) = burrow_events::http::listen(0, Arc::new(|_| 202)).unwrap();
    let watch = d.join("watch.json");
    let cfg = d.join("config.json");
    fs::write(
        &cfg,
        json!({"file":watch,"repos":{"sam/repo":["ci.yml"]},"prThreshold":3,"authors":["sam"],
        "kick":format!("http://127.0.0.1:{port}/kick/watchdog")})
        .to_string(),
    )
    .unwrap();
    let at = jiff::Timestamp::now().to_string();
    let pr = |id: u64, actor: &str, head: &str| {
        json!({"id":id,"run_attempt":2,"created_at":at,"html_url":format!("https://example.org/{id}"),
        "actor":{"login":actor},"triggering_actor":{"login":"sam"},
        "head_repository":{"full_name":head}})
    };
    let write_runs = |runs: Vec<serde_json::Value>| {
        fs::write(
            d.join("prs.json"),
            json!({"workflow_runs":runs}).to_string(),
        )
        .unwrap()
    };
    // Leg a degraded in attempt 1 and wasn't rerun; leg b was.
    let script = r#"#!/bin/sh
case "$2" in
  repos/sam/repo) echo '{"default_branch":"main"}' ;;
  *event=pull_request*) cat "$FIXTURES/prs.json" ;;
  *workflows/ci.yml/runs*) echo '{"workflow_runs":[]}' ;;
  *runs/*/jobs*) echo '{"jobs":[
    {"name":"a","run_attempt":1,"steps":[{"name":"degraded/cache","conclusion":"success"}]},
    {"name":"b","run_attempt":1,"steps":[]},{"name":"b","run_attempt":2,"steps":[]}]}' ;;
  *) exit 1 ;;
esac
"#;
    burrow_test::executable(d.join("gh"), script);
    let path = std::env::join_paths(
        std::iter::once(d.to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let run = || {
        let out = Command::new(env!("CARGO_BIN_EXE_burrow-degradations"))
            .env("PATH", &path)
            .env("FIXTURES", d)
            .env("DEGRADATIONS_CONFIG", &cfg)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value = serde_json::from_slice(&fs::read(&watch).unwrap()).unwrap();
        v["items"].as_object().unwrap().len()
    };
    write_runs(vec![
        pr(1, "eve", "sam/repo"),
        pr(2, "eve", "sam/repo"),
        pr(3, "sam", "eve/repo"),
        pr(4, "sam", "sam/repo"),
        pr(5, "sam", "sam/repo"),
    ]);
    assert_eq!(run(), 0, "outsider and fork runs don't count");
    write_runs(vec![
        pr(1, "eve", "sam/repo"),
        pr(4, "sam", "sam/repo"),
        pr(5, "sam", "sam/repo"),
        pr(6, "sam", "sam/repo"),
    ]);
    assert_eq!(run(), 1, "three trusted runs, signal kept past the rerun");
}
