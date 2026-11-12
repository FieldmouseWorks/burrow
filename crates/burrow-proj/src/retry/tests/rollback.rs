use super::*;

#[test]
fn initial_identity_requires_aware_handover_for_each_engine() {
    for (mode, old_format) in [
        ("new", "absent"),
        ("new", "2"),
        ("new-codex", "absent"),
        ("new-codex", "2"),
    ] {
        let mut r = rig();
        let old = r.tmp.path().join("old-startup");
        let wrong = r.tmp.path().join("unaware-executed");
        let checked = r.tmp.path().join("identity-format-checked");
        let old_retry = if old_format == "2" {
            "echo 2"
        } else {
            "exit 2"
        };
        burrow_test::executable(
            &old,
            format!(
                "#!/bin/sh\ncase $1 in\n_handover_format) echo 1 ;;\n_retry_format) touch '{}'; {old_retry} ;;\n_exec) touch '{}' ;;\n*) exit 2 ;;\nesac\n",
                checked.display(),
                wrong.display()
            ),
        );
        let config_path = r.tmp.path().join("config.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
        value["deployed"] = old.to_str().unwrap().into();
        fs::write(config_path, serde_json::to_vec(&value).unwrap()).unwrap();
        let held = if mode == "new-codex" {
            fs::create_dir_all(r.cfg.jobs.join(".codex-slots")).unwrap();
            (1..=r.cfg.codex_slots)
                .map(|i| {
                    let file = File::create(r.cfg.jobs.join(format!(".codex-slots/{i}"))).unwrap();
                    file.lock().unwrap();
                    file
                })
                .collect::<Vec<_>>()
        } else {
            usage::record(
                &r.cfg,
                "hold",
                Some(crate::now() + 3600),
                "refused",
                crate::now(),
            )
            .unwrap();
            Vec::new()
        };
        start(&mut r, "initial", mode, "/ordinary");
        let job = Job::new(&r.cfg, "initial");
        let choice = Identity::load(&job, "1").unwrap().unwrap().choice;
        assert!(Checkpoint::load(&job, "1").unwrap().is_none());
        // A later settings/role change must not let unaware startup execute
        // this turn and leave its predecessor's protected choice behind.
        fs::remove_file(job.path("engine")).unwrap();
        fs::write(job.path("role"), "changed-after-start").unwrap();
        until(|| checked.exists() || wrong.exists());
        assert!(
            !wrong.exists(),
            "{mode}: unaware rollback executed an initial turn"
        );
        assert_eq!(Identity::load(&job, "1").unwrap().unwrap().choice, choice);
        drop(held);
        if mode == "new" {
            usage::back(&r.cfg, None);
        }
        until(|| job.path("took").exists());
        assert!(!wrong.exists());
        assert_eq!(Identity::load(&job, "1").unwrap().unwrap().choice, choice);
    }
}

#[test]
fn handover_recognizes_configured_wrappers_and_retained_nix_tools() {
    let mut r = rig();
    r.cfg.tools.bash = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("bash"))
        .find(|path| path.is_file())
        .unwrap();
    let wrapper_dir = r.tmp.path().join("generation/bin");
    fs::create_dir_all(&wrapper_dir).unwrap();
    let wrapper = wrapper_dir.join("proj");
    burrow_test::executable(&wrapper, "#!/bin/sh\nexit 0\n");
    r.cfg.deployed = wrapper.clone();
    let lock = r.cfg.jobs.join(".slots/.lock");
    let script = r#"exec 9>"$0"; "$1" 9; shift; exec "$@""#;
    let check = |exe: &Path, flock: &Path, target: &Path| {
        let values = [
            "bash".into(),
            "-c".into(),
            script.into(),
            lock.to_string_lossy().into_owned(),
            flock.to_string_lossy().into_owned(),
            target.to_string_lossy().into_owned(),
            "_exec".into(),
            "id".into(),
            "normal".into(),
        ];
        let args = values
            .iter()
            .map(|s: &String| s.as_bytes())
            .collect::<Vec<_>>();
        authority::legacy_handover(&r.cfg, exe, &args)
    };
    assert!(check(&r.cfg.tools.bash, &r.cfg.tools.flock, &wrapper));
    assert!(check(
        Path::new("/nix/store/previous-bash/bin/bash"),
        Path::new("/nix/store/previous-tools/bin/flock"),
        Path::new("/nix/store/previous-proj/bin/proj")
    ));
    assert!(!check(
        &r.cfg.tools.bash,
        &r.cfg.tools.flock,
        &r.tmp.path().join("unconfigured/proj")
    ));
    assert!(!check(
        &r.cfg.tools.bash,
        &r.cfg.tools.flock,
        Path::new("/nix/store/package/../../tmp/proj")
    ));
}

#[test]
fn legacy_refusals_stay_in_the_cohort_when_queue_publication_is_delayed() {
    for late_entry in [false, true] {
        let mut r = rig();
        usage::record(
            &r.cfg,
            "hold",
            Some(crate::now() + 3600),
            "refused",
            crate::now(),
        )
        .unwrap();
        let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
        barrier.lock().unwrap();
        for i in 0..5 {
            start(&mut r, &format!("w{i}"), "old-delayed", "/ordinary");
        }
        usage::back(&r.cfg, None);
        let cutoff = usage::admission_cutoff(&r.cfg).unwrap().unwrap();
        for i in 0..5 {
            let id = format!("w{i}");
            let job = Job::new(&r.cfg, &id);
            fs::write(job.path("publish"), "").unwrap();
            let pid = read(&job.path("pid")).unwrap().parse().unwrap();
            until(|| authority::legacy_waiting(&r.cfg, pid).unwrap());
            if late_entry {
                let name = format!("2-{}-{id}", cutoff + 1);
                let path = r.cfg.jobs.join(".queue").join(name);
                fs::write(&path, pid.to_string()).unwrap();
                File::open(&path)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(
                        std::time::UNIX_EPOCH + Duration::from_nanos((cutoff + 1) as u64),
                    ))
                    .unwrap();
            }
            assert!(checkpoint(&r.cfg, &job, "1", pid, false).unwrap().waited <= cutoff);
        }
        let launcher = fs::read(&r.cfg.tools.systemd_run).unwrap();
        burrow_test::executable(&r.cfg.tools.systemd_run, "#!/bin/sh\nexit 1\n");
        assert_eq!(pass(&r.cfg).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        start(&mut r, "fresh", "new", "/ordinary");
        let note = r.cfg.jobs.join(".slots/.next");
        until(|| fs::read_to_string(&note).is_ok_and(|s| s.ends_with('\n')));
        assert!(
            !read(&note)
                .unwrap()
                .split('\t')
                .any(|name| name.ends_with("-fresh"))
        );
        assert!(!r.cfg.jobs.join("fresh/took").exists());
        burrow_test::executable(&r.cfg.tools.systemd_run, launcher);
        sleep(Duration::from_secs(2));
        pass(&r.cfg).unwrap();
        until(|| {
            let count = took(&r.cfg);
            assert!(count <= 2, "reset budget bypassed: {count}");
            count == 2
        });
    }
}
