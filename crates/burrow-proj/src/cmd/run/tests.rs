use super::{
    Command, Fail, Job, New, find, fs, id_name, key_lock, key_name, kick_name, reply, reply_answer,
    run, run_with_cgroup, since_time, start, start_job,
};

#[test]
fn id_names() {
    for (project, name) in [
        ("web", "web"),
        ("my.repo", "my.repo"),
        (".github", "dot.github"),
        ("..x", "dot..x"),
        (".-x", "dot.-x"),
        ("-x", "dot-x"),
    ] {
        assert_eq!(id_name(project), name, "{project}");
    }
}

// Use child processes so inherited systemd environments cannot race with
// other tests. Reaching project validation proves CLI admission succeeded,
// without launching a real job or contacting systemd.
#[test]
fn roleless_starts_from_sessions_and_jobs_reach_project_validation() {
    const CHILD: &str = "PROJ_ROLELESS_START_TEST";
    if let Ok(context) = std::env::var(CHILD) {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = keyed(tmp.path(), "not-found");
        for flags in [vec![], vec!["--role", "flaky"]] {
            let job = context == "proj-project-123.service";
            let machine = context != "shell" && !context.starts_with("claude-rc@") && !job;
            let needs_role = machine && flags.is_empty();
            let args: Vec<String> = flags
                .into_iter()
                .chain(["missing-project", "task"])
                .map(str::to_owned)
                .collect();
            let fail = if context == "shell" || job {
                run(&cfg, false, &args)
            } else {
                // Kernel cgroup fixture: test the CLI parser and inherited
                // environment without creating a real service.
                let cgroup =
                    format!("0::/user.slice/user@1001.service/claude-rc.slice/{context}\n");
                run_with_cgroup(&cfg, false, &args, || Ok(cgroup))
            }
            .unwrap_err();
            if needs_role {
                assert_eq!(fail.code, 2, "{context}: {:?}", fail.say);
                assert_eq!(
                    fail.say.as_deref(),
                    Some("proj run from a machine unit needs --role NAME")
                );
                assert!(!cfg.jobs.exists());
                continue;
            }
            assert_eq!(fail.code, 1);
            assert_eq!(
                fail.say,
                Some(format!(
                    "no such project: {}/missing-project",
                    cfg.projects.display()
                ))
            );
            assert!(!cfg.jobs.exists(), "no job created by validation");
        }
        return;
    }
    for unit in [
        None,
        Some("claude-rc@project.service"),
        Some("proj-project-123.service"),
        Some("advisory-watch.service"),
        Some("claude-rc-scan.service"),
        Some("proj-inbox.service"),
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "cmd::run::tests::roleless_starts_from_sessions_and_jobs_reach_project_validation",
                "--nocapture",
            ])
            .env(CHILD, unit.unwrap_or("shell"))
            .env_remove("INVOCATION_ID")
            .env_remove("SYSTEMD_EXEC_PID")
            .env_remove("PROJ_JOB");
        if let Some(unit) = unit {
            child
                .env("INVOCATION_ID", "0123456789abcdef0123456789abcdef")
                .env("SYSTEMD_EXEC_PID", "123");
            if unit == "proj-project-123.service" {
                child.env("PROJ_JOB", "project-123");
            } else if unit == "proj-inbox.service" {
                child.env("PROJ_JOB", "");
            }
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{unit:?}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn since_times() {
    for ok in ["1792800000", "2000000000", "1000000000"] {
        assert!(since_time(ok, 2000000000), "{ok}");
    }
    for bad in [
        "",
        "0",
        "soon",
        "179280000",
        "17928000000",
        "0792800000",
        "179280000x",
        "-792800000",
        "2000000001",
    ] {
        assert!(!since_time(bad, 2000000000), "{bad}");
    }
}

#[test]
fn kicks() {
    for ok in [
        "autoland",
        "proj-watch",
        "autoland/o/r",
        "proj-inbox/Sam/my.repo_x-1",
    ] {
        assert!(kick_name(ok), "{ok}");
    }
    for bad in ["", "Autoland", "a/b", "a/b/c/d", "a//c", "a/b/", "a b"] {
        assert!(!kick_name(bad), "{bad}");
    }
}

#[test]
fn keys() {
    for ok in ["upkeep/o/r/dashboard/1800000000.5", "a", "x:y-z_1"] {
        assert!(key_name(ok), "{ok}");
    }
    let long = "k".repeat(201);
    for bad in [
        "",
        ".a",
        "/a",
        "-a",
        "a b",
        "a\nb",
        "a+b",
        "ü",
        long.as_str(),
    ] {
        assert!(!key_name(bad), "{bad}");
    }
}

/// A config on `tmp`'s jobs and projects, whose systemctl says each
/// unit's LoadState is `load`, and that none is active.
fn keyed(tmp: &std::path::Path, load: &str) -> crate::config::Config {
    let ctl = tmp.join(format!("systemctl-{load}"));
    let script = format!(
        "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3 ;; *LoadState*) echo {load} ;; esac\n"
    );
    burrow_test::executable(&ctl, script);
    let mut v = crate::config::tests::example();
    v["jobs"] = tmp.join("jobs").to_str().unwrap().into();
    v["projects"] = tmp.join("projects").to_str().unwrap().into();
    v["tools"]["systemctl"] = ctl.to_str().unwrap().into();
    serde_json::from_value(v).unwrap()
}

fn hold(tmp: &std::path::Path, key: &str, id: &str, turns: bool) {
    let jobs = tmp.join("jobs");
    std::fs::create_dir_all(jobs.join(".keys")).unwrap();
    std::fs::create_dir_all(jobs.join(id)).unwrap();
    if turns {
        std::fs::write(jobs.join(id).join("turns"), "1\n").unwrap();
    }
    std::os::unix::fs::symlink(id, jobs.join(".keys").join(key.replace('/', "+"))).unwrap();
}

// A key finds the job holding it once that job has started a turn, or
// its unit is loaded; one that never started lets the key go. None in
// a jobs dir that isn't there yet, and what can't be read fails.
#[test]
fn a_key_finds_its_job() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = keyed(tmp.path(), "not-found");
    assert_eq!(find(&cfg, "k").unwrap(), None);
    hold(tmp.path(), "a/k", "a", true);
    hold(tmp.path(), "b/k", "b", false);
    assert_eq!(find(&cfg, "a/k").unwrap().as_deref(), Some("a"));
    assert_eq!(find(&cfg, "b/k").unwrap(), None);
    let link = |k: &str| std::fs::symlink_metadata(tmp.path().join("jobs/.keys").join(k));
    assert!(link("b+k").is_err(), "its key goes");
    assert!(tmp.path().join("jobs/b").exists(), "its record stays");
    let loaded = keyed(tmp.path(), "loaded");
    hold(tmp.path(), "c/k", "c", false);
    assert_eq!(find(&loaded, "c/k").unwrap().as_deref(), Some("c"));
    std::fs::write(tmp.path().join("jobs/.keys/d+k"), "d").unwrap();
    assert!(find(&cfg, "d/k").is_err(), "a key that isn't a link");
    let mute = keyed(tmp.path(), "");
    hold(tmp.path(), "e/k", "e", false);
    assert!(find(&mute, "e/k").is_err(), "a systemd that can't say");
    assert!(link("e+k").is_ok(), "and the key is kept");
}

// A key is judged and let go under its own lock: one a starter takes
// over meanwhile, for a job that has started, is that job's, and stays.
#[test]
fn a_key_is_judged_under_its_own_lock() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = keyed(tmp.path(), "not-found");
    hold(tmp.path(), "k", "p-1", false);
    std::fs::create_dir_all(tmp.path().join("jobs/p-2")).unwrap();
    std::fs::write(tmp.path().join("jobs/p-2/turns"), "1\n").unwrap();
    let held = key_lock(&cfg, "k").unwrap();
    let finder = {
        let cfg = cfg.clone();
        std::thread::spawn(move || find(&cfg, "k").unwrap())
    };
    std::thread::sleep(std::time::Duration::from_millis(300));
    let link = tmp.path().join("jobs/.keys/k");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink("p-2", &link).unwrap();
    drop(held);
    assert_eq!(finder.join().unwrap().as_deref(), Some("p-2"));
    assert!(std::fs::symlink_metadata(&link).is_ok(), "and it keeps it");
}

// A key is taken under its own lock too: a starter waits out a judgment
// under way, and finds the job that took the key over meanwhile.
#[test]
fn a_key_is_taken_under_its_own_lock() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = keyed(tmp.path(), "not-found");
    std::fs::create_dir_all(tmp.path().join("projects/p")).unwrap();
    hold(tmp.path(), "k", "p-1", false);
    std::fs::create_dir_all(tmp.path().join("jobs/p-2")).unwrap();
    std::fs::write(tmp.path().join("jobs/p-2/turns"), "1\n").unwrap();
    let held = key_lock(&cfg, "k").unwrap();
    let starter = {
        let cfg = cfg.clone();
        std::thread::spawn(move || {
            let new = New {
                key: Some("k".into()),
                ..New::default()
            };
            start_job(&cfg, &new, "p", "task").unwrap_err().say
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(300));
    let link = tmp.path().join("jobs/.keys/k");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink("p-2", &link).unwrap();
    drop(held);
    assert_eq!(
        starter.join().unwrap().as_deref(),
        Some("proj run: p-2 has the key k")
    );
    assert_eq!(std::fs::read_link(&link).unwrap().to_str(), Some("p-2"));
}

// A job still starting, its starter holding its lock, is waited for:
// what finds its key once the lock goes is the job, turn and all.
#[test]
fn a_start_under_way_is_waited_for() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = keyed(tmp.path(), "not-found");
    hold(tmp.path(), "k", "p-1", false);
    let job = crate::job::Job::new(&cfg, "p-1");
    let held = job.lock().unwrap();
    let finder = {
        let cfg = cfg.clone();
        std::thread::spawn(move || find(&cfg, "k").unwrap())
    };
    std::thread::sleep(std::time::Duration::from_millis(300));
    job.set_turns(1).unwrap();
    drop(held);
    assert_eq!(finder.join().unwrap().as_deref(), Some("p-1"));
}

// A key a started job has is refused, before any job is made; one a job
// that never started had is taken over.
#[test]
fn a_key_a_job_has_is_refused() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = keyed(tmp.path(), "not-found");
    std::fs::create_dir_all(tmp.path().join("projects/p")).unwrap();
    hold(tmp.path(), "k", "p-1", true);
    let new = New {
        key: Some("k".into()),
        ..New::default()
    };
    let fail = start_job(&cfg, &new, "p", "task").unwrap_err();
    assert_eq!(
        (fail.code, fail.say.as_deref()),
        (1, Some("proj run: p-1 has the key k"))
    );
    let jobs: Vec<_> = std::fs::read_dir(tmp.path().join("jobs"))
        .unwrap()
        .collect();
    assert_eq!(jobs.len(), 2, "p-1 and .keys, no new job");
    let bad = New {
        key: Some("a b".into()),
        ..New::default()
    };
    assert_eq!(start_job(&cfg, &bad, "p", "task").unwrap_err().code, 2);
    hold(tmp.path(), "j", "p-2", false);
    let new = New {
        key: Some("j".into()),
        ..New::default()
    };
    // Its turn can't start here (no systemd-run), but the key is its.
    assert!(start_job(&cfg, &new, "p", "task").is_err());
    let held = std::fs::read_link(tmp.path().join("jobs/.keys/j")).unwrap();
    assert!(held.to_str() != Some("p-2") && held.to_str().unwrap().starts_with("p-"));
}

#[test]
fn question_state_follows_resume_source_and_only_successful_starts() {
    for source in ["operator", "issue-author", "update", "watcher", "cutoff"] {
        for succeeds in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let mut cfg = keyed(tmp.path(), "not-found");
            cfg.tools.systemd_run = if succeeds { "true" } else { "false" }.into();
            let job = Job::new(&cfg, "j1");
            fs::create_dir_all(&job.dir).unwrap();
            crate::policy::bind(
                &job,
                crate::policy::Identity::Local,
                crate::policy::Kind::Manual,
            )
            .unwrap();
            job.set_turns(1).unwrap();
            fs::write(job.path("session"), "s1\n").unwrap();
            fs::write(job.turn("1", "end"), "ask\n").unwrap();
            fs::write(job.path("open-question"), "ask\n").unwrap();
            if source == "cutoff" {
                fs::write(
                    job.turn("1", "jsonl"),
                    "{\"type\":\"system\",\"session_id\":\"s1\"}\n",
                )
                .unwrap();
            } else {
                fs::write(job.turn("1", "outcome"), "done\n").unwrap();
            }
            let result = match source {
                "operator" => {
                    crate::cmd::dispatch(&cfg, &["reply".into(), "j1".into(), "answer".into()])
                }
                "issue-author" => reply_answer(&cfg, "j1", "answer", true),
                "update" => reply(&cfg, "j1", "update", true, None),
                "cutoff" => reply(&cfg, "j1", "retry", true, Some("1")),
                "watcher" => {
                    let _held = job.lock().unwrap();
                    start(&cfg, "j1", "watcher update", false).map_err(|e| Fail::new(1, e))
                }
                _ => unreachable!(),
            };
            assert_eq!(result.is_ok(), succeeds, "{source}: {result:?}");
            let answered = matches!(source, "operator" | "issue-author") && succeeds;
            assert_eq!(
                job.path("question-answered-through").exists(),
                answered,
                "{source}, succeeds={succeeds}"
            );
            assert_eq!(
                job.path("open-question").exists(),
                !answered,
                "{source}, succeeds={succeeds}"
            );
            assert_eq!(
                job.turns().as_deref(),
                Some(if succeeds { "2" } else { "1" })
            );
            let response = crate::cmd::declaration_tests::response(&cfg, "j1", "ask");
            assert_eq!(
                response.contains("with your question"),
                answered,
                "{source}: {response}"
            );
            if succeeds {
                let prompt = fs::read_to_string(job.path("prompt.2")).unwrap();
                assert_eq!(
                    prompt.contains("Do not repeat the question"),
                    !answered,
                    "{source}: {prompt}"
                );
            } else {
                assert_eq!(
                    fs::read_to_string(job.path("open-question")).unwrap(),
                    "ask\n"
                );
                assert!(!job.path("prompt.2").exists());
            }
        }
    }
}

#[path = "tests/shared.rs"]
mod shared;
