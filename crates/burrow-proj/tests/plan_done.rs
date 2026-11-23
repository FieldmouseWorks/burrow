//! `proj plan done` is the host's, for an author: from inside a job it
//! refuses both forms before it reads anything (inbox/plan_done.rs).
use serde_json::json;
use std::fs;

#[test]
fn a_job_never_marks_a_child_done() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("proj.json");
    let value = json!({
        "projects":"/unused", "jobs":"/unused", "scratch":"/unused",
        "slice":"unused", "maxJobs":1, "pipelineSlots":1, "maxJobsPerProject":1,
        "minFreeMemory":0, "jobMemoryMax":"1G", "timeout":"1m", "maxWait":1,
        "stallAfterMinutes":1, "awaitHours":1, "kick":"", "wake":"/unused",
        "waits":null, "inboxMark":"", "pipelineRepos":[], "leadRepos":[],
        "flake":"/unused", "deployed":"/unused", "apps":{}, "machinery":null,
        "notify":null, "meters":null, "inbox":null,
        "tools":{"gh":"false", "git":"git", "bash":"unused", "flock":"unused",
            "systemctl":"unused", "systemdRun":"unused", "timeout":"unused"}
    });
    fs::write(&config, value.to_string()).unwrap();
    for form in [
        &["https://github.com/sam/box/pull/10"][..],
        &["--no-pr", "the", "record", "is", "the", "deliverable"][..],
    ] {
        let out = burrow_test::command(burrow_test::program!("burrow-proj"))
            .args(["plan", "done", "sam/box#8"])
            .args(form)
            .env("PROJ_JOB", "j1")
            .env("PROJ_CONFIG", &config)
            .env("PROJ_SELF", burrow_test::program!("burrow-proj"))
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{form:?}: {said}");
        assert!(
            said.contains("a job never records a child done"),
            "{form:?}: {said}"
        );
    }
}
