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
        json!({"file":watch,"repos":{"sam/repo":["ci.yml"]},"prThreshold":3,
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
  *attempts/1/jobs*) echo '{"jobs":[{"steps":[{"name":"degraded/cache","conclusion":"success"}]}]}' ;;
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
            "*attempts/1/jobs*)",
            "*runs/3/attempts/1/jobs*) echo '{\"jobs\":[]}' ;;\n  *attempts/1/jobs*)",
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
