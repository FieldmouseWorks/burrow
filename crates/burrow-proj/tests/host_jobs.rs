//! A job sees no host configuration or history in its own view. Its normal
//! CLI must still inspect and resume a different timed-out job on the host.
use burrow_proj::{config::Config, job::Job, probe::Bridge};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::symlink,
    path::Path,
    process::{Command, Output},
};

fn config(root: &Path) -> Config {
    let p = |name: &str| root.join(name);
    let path = p("host.json");
    let value = json!({
        "projects":p("projects"), "jobs":p("host-jobs"), "scratch":p("scratch"),
        "slice":"test.slice", "maxJobs":4, "pipelineSlots":1,
        "maxJobsPerProject":2, "minFreeMemory":0, "jobMemoryMax":"infinity",
        "timeout":"4h", "maxWait":1800, "stallAfterMinutes":120, "awaitHours":24,
        "kick":"", "wake":p("wake.json"), "waits":null, "inboxMark":"test",
        "pipelineRepos":[], "leadRepos":[], "flake":p("projects/machine"),
        "deployed":env!("CARGO_BIN_EXE_burrow-proj"), "apps":{}, "notify":null,
        "roles":p("roles"), "models":p("models"), "meters":null,
        "codexSlots":2, "codexHooks":p("hooks"), "inbox":null,
        "tools":{"codex":"false", "bwrap":"false", "bash":"sh", "flock":"flock",
                 "gh":"false", "git":"git", "systemctl":p("systemctl"),
                 "systemdRun":p("systemd-run"), "timeout":"timeout"}
    });
    fs::write(&path, value.to_string()).unwrap();
    let mut sandbox = value.clone();
    sandbox["jobs"] = json!(p("sandbox-jobs"));
    sandbox["projects"] = json!(p("sandbox-projects"));
    fs::create_dir_all(p("sandbox-jobs/recovery")).unwrap();
    fs::create_dir_all(p("sandbox-projects")).unwrap();
    fs::write(p("sandbox.json"), sandbox.to_string()).unwrap();
    let mut cfg: Config = serde_json::from_value(value).unwrap();
    cfg.path = path;
    cfg.me = env!("CARGO_BIN_EXE_burrow-proj").into();
    fs::create_dir_all(&cfg.jobs).unwrap();
    fs::create_dir_all(&cfg.projects).unwrap();
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3;; *LoadState*) echo not-found;; *ActiveState*) echo inactive;; esac\n",
    );
    burrow_test::executable(&cfg.tools.systemd_run, "#!/bin/sh\nexit 0\n");
    cfg
}

fn cli(cfg: &Config, socket: Option<&Path>, args: &[&str]) -> Output {
    let mut command = Command::new(&cfg.me);
    command
        .args(args)
        .env("PROJ_JOB", "recovery")
        .env("PROJ_CONFIG", cfg.path.with_file_name("sandbox.json"))
        .env("PROJ_SELF", &cfg.me)
        .env_remove("PROJ_PROBE_SOCKET")
        .env_remove("PROJ_HOST_JOBS_SOCKET");
    if let Some(socket) = socket {
        command.env("PROJ_HOST_JOBS_SOCKET", socket);
    }
    command.output().unwrap()
}
fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn isolated_recovery_reads_resumes_and_stops_a_host_job() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let target = Job::new(&cfg, "stalled");
    fs::create_dir(&target.dir).unwrap();
    burrow_proj::policy::bind(
        &target,
        burrow_proj::policy::Identity::Local,
        burrow_proj::policy::Kind::Manual,
    )
    .unwrap();
    target.set_turns(1).unwrap();
    fs::write(target.path("session"), "original-conversation").unwrap();
    fs::write(target.path("project"), cfg.projects.to_str().unwrap()).unwrap();
    fs::write(target.turn("1", "outcome"), "timed out after 60m").unwrap();
    fs::write(target.turn("1", "late"), "").unwrap();
    let token = format!("ghp_{}", "x".repeat(36));
    fs::write(
        target.turn("1", "jsonl"),
        json!({"type":"result", "subtype":"success",
        "result":format!("host history {token}"), "is_error":false})
        .to_string(),
    )
    .unwrap();
    let caller = Job::new(&cfg, "recovery");
    let _bridge = Bridge::start(&cfg, &caller, "1", None).unwrap();
    let socket = burrow_proj::scratch::dir(&cfg, caller.id).join("host-jobs-1.sock");
    fs::create_dir_all(cfg.jobs.join(".keys")).unwrap();
    symlink("stalled", cfg.jobs.join(".keys/recovery-case")).unwrap();
    fs::create_dir(cfg.projects.join("only-host")).unwrap();
    let run = |args: &[&str]| cli(&cfg, Some(&socket), args);
    let mut unmounted = cfg.clone();
    unmounted.path = tmp.path().join("unmounted/config");
    assert!(
        success(cli(&unmounted, Some(&socket), &["log", "stalled", "--raw"]))
            .contains("host history")
    );

    assert!(success(run(&["status", "100"])).contains("stalled"));
    assert!(success(run(&[])).contains("stalled"));
    assert_eq!(success(run(&["find", "recovery-case"])), "stalled\n");
    assert!(success(run(&["list"])).contains("only-host"));
    assert_eq!(success(run(&["state", "stalled"])), "timed out after 60m\n");
    assert!(success(run(&["lifecycle", "stalled"])).contains("timed out after 60m"));
    for args in [
        vec!["log", "stalled"],
        vec!["log", "stalled", "--turn", "1", "--raw"],
    ] {
        let history = success(run(&args));
        assert!(history.contains("host history"), "{history}");
        assert!(history.contains("[redacted]"));
        assert!(!history.contains(&token));
    }
    let message = "continue this conversation\nkeep spaces, $(), and 'quotes' literal";
    success(run(&["reply", "--first", "stalled", message]));
    assert_eq!(target.turns().as_deref(), Some("2"));
    assert_eq!(
        fs::read_to_string(target.path("session")).unwrap(),
        "original-conversation"
    );
    assert!(
        fs::read_to_string(target.path("prompt.2"))
            .unwrap()
            .contains(message)
    );
    assert_eq!(
        fs::read_to_string(target.turn("2", "priority")).unwrap(),
        "first"
    );
    // The bridge preserves CLI failure codes, not just the error text.
    assert_eq!(
        run(&["reply", "--cut-off", "2", "stalled", "again"])
            .status
            .code(),
        Some(3)
    );
    // Recovery text must not count as the owner's answer to an open question.
    let question = cfg.jobs.join(".question-control/stalled/question");
    fs::write(
        &question,
        json!({"recommendation":{"default":"leave it waiting", "one_way":true},
        "due":burrow_proj::now() + 3600})
        .to_string(),
    )
    .unwrap();
    success(run(&["reply", "stalled", "continue independent work"]));
    assert_eq!(target.turns().as_deref(), Some("3"));
    assert!(
        question.exists(),
        "a machinery continuation answered the owner question"
    );
    success(run(&["stop", "stalled"]));
    assert!(target.turn("3", "stopped").exists());
}

#[test]
fn missing_host_access_is_explicit_and_never_reports_missing_history() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    for args in [
        vec!["log", "stalled"],
        vec!["reply", "stalled", "resume"],
        vec!["status"],
    ] {
        let output = cli(&cfg, None, &args);
        assert!(!output.status.success());
        let text = String::from_utf8_lossy(&output.stderr);
        assert!(text.contains("can't reach host jobs"), "{text}");
        assert!(!text.contains("has no turns"));
        assert!(!text.contains("has no session"));
    }
    let output = cli(
        &cfg,
        Some(&tmp.path().join("missing.sock")),
        &["state", "stalled"],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("can't reach host jobs"));
}

#[test]
fn host_rejects_traversal_new_execution_options_and_probe_mutations() {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
    };
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let caller = Job::new(&cfg, "recovery");
    let _bridge = Bridge::start(&cfg, &caller, "1", None).unwrap();
    let root = burrow_proj::scratch::dir(&cfg, caller.id);
    // These raw requests bypass client validation and exercise the server.
    let exchange = |name: &str, words: &[&str]| {
        // Temp paths here fit sockaddr_un; production also supports long scratch paths.
        let mut stream = UnixStream::connect(root.join(name)).unwrap();
        stream
            .write_all(serde_json::to_string(words).unwrap().as_bytes())
            .unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        text
    };
    for args in [
        vec!["log", "../outside"],
        vec!["log", "stalled", "--turn", "../../outside"],
        vec!["reply", "--app", "owner", "stalled", "go"],
        vec!["run", "web", "go"],
        vec!["_exec", "stalled", "first"],
        vec!["stop", "/outside"],
        vec!["stop", "a/../outside"],
        vec!["stop", ".question-control"],
        vec!["stop", "--first"],
        vec!["stop", "bad\0name"],
    ] {
        let response = exchange("host-jobs-1.sock", &args);
        assert!(response.contains("unsupported command"), "{response}");
    }
    assert!(exchange("probe-1.sock", &["stop", "stalled"]).contains("Err"));
}

#[test]
fn redacted_snapshot_and_transcript_keep_json_framing_and_public_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let target = Job::new(&cfg, "structured");
    fs::create_dir(&target.dir).unwrap();
    target.set_turns(1).unwrap();
    fs::write(target.turn("1", "outcome"), "done").unwrap();
    fs::write(
        target.turn("1", "result"),
        r#"public prefix "quotes" Authorization: header-fixture"#,
    )
    .unwrap();
    let token = format!("ghp_{}", "x".repeat(36));
    let records = [
        json!({"type":"system", "Authorization":"field-header-fixture",
            "client_secret":"quoted \"credential\"\nsecond credential line",
            "nested":[{"api_key":r#"escaped \"credential"#, "public":17}],
            "credentials":{"value":"nested credential"}, "public":"keep this field"}),
        json!({"type":"result", "subtype":"success", "is_error":false, "num_turns":1,
            "result":format!("Authorization: header-fixture\npublic suffix \"quotes\"\npassword=\"first \\\"second\\\"; tail\"\n{token}"),
            "public":true}),
    ];
    fs::write(
        target.turn("1", "jsonl"),
        records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let caller = Job::new(&cfg, "recovery");
    let _bridge = Bridge::start(&cfg, &caller, "1", None).unwrap();
    let socket = burrow_proj::scratch::dir(&cfg, caller.id).join("host-jobs-1.sock");
    let run = |args: &[&str]| success(cli(&cfg, Some(&socket), args));

    let snapshot = run(&["state", "--json", "10"]);
    let value: serde_json::Value = serde_json::from_str(&snapshot).expect("snapshot remains JSON");
    let row = value["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "structured")
        .unwrap();
    assert!(
        row["answer"]
            .as_str()
            .unwrap()
            .starts_with("public prefix \"quotes\"")
    );
    assert!(!snapshot.contains("header-fixture"));
    assert!(!snapshot.contains(&token));

    let raw = run(&["log", "structured", "--raw"]);
    let decoded: Vec<serde_json::Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).expect("each transcript record remains JSON"))
        .collect();
    assert_eq!(decoded.len(), records.len());
    assert_eq!(decoded[0]["public"], "keep this field");
    assert_eq!(decoded[0]["nested"][0]["public"], 17);
    assert_eq!(decoded[1]["public"], true);
    assert!(
        decoded[1]["result"]
            .as_str()
            .unwrap()
            .contains("public suffix \"quotes\"")
    );
    for field in ["Authorization", "client_secret", "credentials"] {
        assert_eq!(decoded[0][field], "[redacted]");
    }
    assert_eq!(decoded[0]["nested"][0]["api_key"], "[redacted]");
    for secret in [
        "header-fixture",
        "credential line",
        "nested credential",
        "second",
        "; tail",
        &token,
    ] {
        assert!(!raw.contains(secret), "{secret:?} survived redaction");
    }
    serde_json::from_str::<serde_json::Value>(&run(&["state", "--schema"])).unwrap();
}

#[test]
fn every_launcher_name_is_usable_through_host_job_operations() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let caller = Job::new(&cfg, "recovery");
    let _bridge = Bridge::start(&cfg, &caller, "1", None).unwrap();
    let socket = burrow_proj::scratch::dir(&cfg, caller.id).join("host-jobs-1.sock");
    let run = |args: &[&str]| success(cli(&cfg, Some(&socket), args));
    let names = [
        "my app".into(),
        "web%test".into(),
        "_worker".into(),
        "日本語🐭".into(),
        " leading".into(),
        "with\ttab".into(),
        "with\nnewline".into(),
        "back\\slash".into(),
        "\"quoted\"".into(),
        ".github".into(),
        "-worker".into(),
        "x".repeat(205),
    ];
    for (index, name) in names.iter().enumerate() {
        fs::create_dir(cfg.projects.join(name)).unwrap();
        let key = format!("launcher-{index}");
        let id = burrow_proj::cmd::start_job(
            &cfg,
            &burrow_proj::cmd::New {
                local_work: true,
                key: Some(key.clone()),
                ..Default::default()
            },
            name,
            "seed original task",
        )
        .unwrap();
        let job = Job::new(&cfg, &id);
        fs::write(job.path("session"), "saved-conversation").unwrap();
        fs::write(job.turn("1", "outcome"), "timed out after 60m").unwrap();
        fs::write(
            job.turn("1", "jsonl"),
            json!({"type":"result", "result":"original answer", "num_turns":1}).to_string(),
        )
        .unwrap();
        let found = run(&["find", &key]);
        assert_eq!(found, format!("{id}\n"));
        let snapshot: serde_json::Value =
            serde_json::from_str(&run(&["state", "--json", "100"])).unwrap();
        assert!(
            snapshot["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["id"] == id)
        );
        assert_eq!(run(&["state", &id]), "timed out after 60m\n");
        assert!(run(&["lifecycle", &id]).contains("timed out after 60m"));
        assert!(run(&["log", &id]).contains("original answer"));
        assert!(run(&["log", &id, "--raw"]).contains("original answer"));
        assert_eq!(run(&["withdraw", &id]), "over\n");
        run(&["reply", &id, "continue this task"]);
        assert_eq!(job.turns().as_deref(), Some("2"));
        run(&["stop", &id]);
        assert!(job.turn("2", "stopped").exists());
    }
}
