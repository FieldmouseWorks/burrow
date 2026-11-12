use super::*;
use std::os::unix::fs::symlink;

#[test]
fn recovery_writes_never_follow_guest_symlinks() {
    let r = rig();
    let outside = r.tmp.path().join("host-file");
    fs::write(&outside, "host\n").unwrap();
    let dir = r.tmp.path().join("guest");
    fs::create_dir(&dir).unwrap();
    // A predictable staging name, and the destination itself, both links.
    symlink(&outside, dir.join("project.new")).unwrap();
    symlink(&outside, dir.join("project")).unwrap();
    crate::state_reads::write(dir.join("project"), "restored\n").unwrap();
    assert_eq!(read(&outside).unwrap(), "host");
    assert_eq!(read(&dir.join("project")).unwrap(), "restored");
    assert!(!dir.join("project").is_symlink());
    // The stream of a resumed turn is opened without following a link.
    symlink(&outside, dir.join("1.jsonl")).unwrap();
    assert!(crate::state_reads::append(&dir.join("1.jsonl")).is_err());
    assert_eq!(read(&outside).unwrap(), "host");
}

#[test]
fn restore_and_wake_leave_host_files_alone_when_the_job_dir_holds_links() {
    let r = rig();
    let outside = r.tmp.path().join("host-file");
    fs::write(&outside, "host\n").unwrap();
    pending(&r, "linked", false);
    let job = Job::new(&r.cfg, "linked");
    for name in ["project.new", "engine.new", "1.engine.new", "1.retry-wake"] {
        symlink(&outside, job.dir.join(name)).unwrap();
    }
    symlink(&outside, job.path("project")).unwrap();
    Identity::restore(&job, "1").unwrap();
    crate::cmd::finish(&r.cfg, job.id).unwrap();
    assert_eq!(read(&outside).unwrap(), "host");
}

#[test]
fn capture_saves_the_choice_startup_runs_not_a_second_resolution() {
    let r = rig();
    let job = Job::new(&r.cfg, "chosen");
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    let resolved = engine::choice(&r.cfg, &job).unwrap();
    assert_eq!(resolved.engine, engine::Engine::Claude);
    let ran = engine::Choice {
        engine: engine::Engine::Codex,
        model: Some("m".into()),
        effort: Some("high".into()),
    };
    Identity::capture_choice(&job, "1", &ran).unwrap();
    assert_eq!(Identity::load(&job, "1").unwrap().unwrap().choice, ran);
}

#[test]
fn an_identity_and_a_deleted_slot_file_do_not_make_a_running_turn_waiting() {
    let mut r = rig();
    start(&mut r, "running", "old-running", "/ordinary");
    until(|| r.cfg.jobs.join("running/running").exists());
    let job = Job::new(&r.cfg, "running");
    Identity::capture_choice(&job, "1", &engine::choice(&r.cfg, &job).unwrap()).unwrap();
    fs::remove_file(job.turn("1", "slot")).unwrap();
    let pid = read(&job.path("pid")).unwrap().parse().unwrap();
    assert!(authority::holds_slot(&r.cfg, pid).unwrap());
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    pass(&r.cfg).unwrap();
    assert!(!job.turn("1", "retry-migration").exists());
    assert!(r.cfg.jobs.join("running/running").exists());
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid as i32).unwrap())
            .is_ok()
    );
}

#[test]
fn only_the_exact_legacy_handover_shell_is_a_parent() {
    let mut r = rig();
    // The host example's /bin/bash need not exist in a Nix build sandbox.
    r.cfg.tools.bash = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("bash"))
        .find(|path| path.is_file())
        .unwrap();
    let cfg = &r.cfg;
    let lock = cfg.jobs.join(".slots/.lock");
    let script = r#"exec 9>"$0"; "$1" 9; shift; exec "$@""#;
    let argv = |script: &str, lock: &Path, proj: &str, mark: &str| -> Vec<Vec<u8>> {
        [
            "bash",
            "-c",
            script,
            &lock.to_string_lossy(),
            &cfg.tools.flock.to_string_lossy(),
            proj,
            mark,
            "id",
            "ordinary",
        ]
        .iter()
        .map(|s| s.as_bytes().to_vec())
        .collect()
    };
    let check = |a: Vec<Vec<u8>>| {
        let refs: Vec<&[u8]> = a.iter().map(Vec::as_slice).collect();
        authority::legacy_handover(cfg, &cfg.tools.bash, &refs)
    };
    assert!(check(argv(
        script,
        &lock,
        "/nix/store/x/bin/burrow-proj",
        "_exec"
    )));
    assert!(!check(argv(
        "exec evil",
        &lock,
        "/nix/store/x/bin/burrow-proj",
        "_exec"
    )));
    assert!(!check(argv(
        script,
        &cfg.jobs.join("other"),
        "/nix/store/x/bin/burrow-proj",
        "_exec"
    )));
    for class in ["ordinary", "pipeline", "unstick"] {
        for target in [
            "/nix/store/current-proj/bin/proj",
            "/nix/store/retained-proj/bin/proj",
            "/nix/store/retained-binary/bin/burrow-proj",
        ] {
            let mut args = argv(script, &lock, target, "_exec");
            args[8] = class.as_bytes().to_vec();
            assert!(check(args), "{class}: {target}");
        }
    }
    for target in [
        "/tmp/proj",
        "/tmp/burrow-proj",
        "/nix/store/../tmp/proj",
        "/nix/store/x/bin/other",
    ] {
        assert!(!check(argv(script, &lock, target, "_exec")));
    }
    assert!(!check(argv(script, &lock, "/tmp/evil", "_exec")));
    assert!(!check(argv(
        script,
        &lock,
        "/nix/store/x/bin/burrow-proj",
        "other"
    )));
    let refs: Vec<&[u8]> = vec![b"bash"];
    assert!(!authority::legacy_handover(
        cfg,
        Path::new("/bin/sh"),
        &refs
    ));
}

#[test]
fn initial_and_retained_waiters_keep_identity_when_rollback_ignores_protected_state() {
    for mode in ["new", "new-codex", "new-unstick"] {
        for retained in [false, true] {
            let mut r = rig();
            let old = r.tmp.path().join("unaware");
            let wrong = r.tmp.path().join("wrong-handover");
            burrow_test::executable(
                &old,
                format!(
                    "#!/bin/sh\ncase $1 in\n_handover_format) echo 1 ;;\n_exec) touch '{}' ;;\n*) exit 2 ;;\nesac\n",
                    wrong.display()
                ),
            );
            let path = r.tmp.path().join("config.json");
            let mut cfg: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            cfg["deployed"] = old.to_str().unwrap().into();
            fs::write(path, serde_json::to_vec(&cfg).unwrap()).unwrap();
            super::review::saved(&r, "waiter");
            let job = Job::new(&r.cfg, "waiter");
            if !retained {
                fs::remove_file(job.turn("1", "retry-migration")).unwrap();
            }
            fs::remove_file(job.turn("1", "execution")).unwrap();
            let choice = engine::Choice {
                engine: if mode.ends_with("codex") {
                    engine::Engine::Codex
                } else {
                    engine::Engine::Claude
                },
                model: Some("saved-choice".into()),
                effort: Some("high".into()),
            };
            Identity::capture_choice(&job, "1", &choice).unwrap();
            let identity = fs::read(job.turn("1", "execution")).unwrap();
            // An unaware startup could resolve changed settings rather than
            // honoring this identity. It must never get control of the turn.
            fs::write(job.path("role"), "changed-role\n").unwrap();
            drop(job);
            start(&mut r, "waiter", mode, "/ordinary");
            let job = Job::new(&r.cfg, "waiter");
            until(|| job.path("took").exists() || wrong.exists());
            assert!(!wrong.exists(), "{mode}, retained={retained}");
            assert_eq!(fs::read(job.turn("1", "execution")).unwrap(), identity);
        }
    }
}
