//! Exercise the operator migration and answer route through the real CLI.
//! This test also runs on the feature's base, where merge-questions is absent.
use burrow_proj::{
    config::Config,
    host_jobs,
    job::Job,
    policy::{self, Identity, Kind},
};
use serde_json::json;
use std::fs;

#[test]
fn migrating_open_questions_exposes_one_leader_and_one_reply_resumes_every_member() {
    let dir = tempfile::tempdir().unwrap();
    let systemctl = dir.path().join("systemctl");
    burrow_test::executable(
        &systemctl,
        "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3;; *LoadState*) echo not-found;; esac\n",
    );
    let raw = json!({
        "projects":dir.path(), "jobs":dir.path().join("jobs"), "scratch":dir.path().join("scratch"),
        "slice":"unused", "maxJobs":1, "pipelineSlots":1, "maxJobsPerProject":1,
        "minFreeMemory":0, "jobMemoryMax":"1G", "timeout":"1m", "maxWait":1,
        "stallAfterMinutes":1, "awaitHours":1, "kick":"", "wake":dir.path().join("wake"),
        "waits":null, "inboxMark":"", "pipelineRepos":[], "leadRepos":[],
        "flake":"/unused", "deployed":"/unused", "apps":{}, "machinery":null,
        "notify":null, "meters":null, "codexSlots":1,
        "codexHooks":"/unused", "inbox":null,
        "tools":{"gh":"unused", "git":"git", "codex":"unused", "bwrap":"unused",
            "bash":"unused", "flock":"unused", "systemctl":systemctl,
            "systemdRun":"true", "timeout":"unused"}
    });
    let cfg: Config = serde_json::from_value(raw.clone()).unwrap();
    let config = dir.path().join("config.json");
    fs::write(&config, raw.to_string()).unwrap();
    for (id, since) in [("first", "100"), ("newest", "200"), ("third", "50")] {
        let job = Job::new(&cfg, id);
        fs::create_dir_all(&job.dir).unwrap();
        policy::bind(&job, Identity::Local, Kind::Manual).unwrap();
        job.set_turns(1).unwrap();
        fs::write(job.path("session"), "conversation").unwrap();
        fs::write(job.turn("1", "end"), "ask").unwrap();
        fs::write(job.turn("1", "outcome"), "done").unwrap();
        let host = cfg.jobs.join(".question-control").join(id);
        fs::write(
            host.join("question"),
            json!({"due":4000000000_i64,
            "recommendation":{"default":"leave it", "one_way":false}})
            .to_string(),
        )
        .unwrap();
        fs::write(host.join("question-id"), format!("question-{id}")).unwrap();
        fs::write(host.join("question-since"), since).unwrap();
        fs::write(host.join("question-text"), "May I proceed?").unwrap();
        policy::mark(&job, "done", false, None).unwrap(); // accepted answers stay parked
    }
    let run = |args: &[&str]| {
        // Run from inside a job, a bare command's reply would go to the live
        // host through that job's bridge, not to this test's config.
        let out = host_jobs::owner(env!("CARGO_BIN_EXE_burrow-proj").as_ref(), &config)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "proj {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    let leader = run(&[
        "merge-questions",
        "same decision",
        "first",
        "newest",
        "third",
    ]);
    assert!(leader.contains("newest"), "{leader}");
    run(&["reply", "newest", "yes, proceed"]);
    for id in ["first", "newest", "third"] {
        let job = Job::new(&cfg, id);
        assert_eq!(job.turns().as_deref(), Some("2"), "{id}");
        assert!(
            fs::read_to_string(job.path("prompt.2"))
                .unwrap()
                .contains("yes, proceed")
        );
        assert!(
            !cfg.jobs
                .join(".question-control")
                .join(id)
                .join("question")
                .exists()
        );
    }
}
