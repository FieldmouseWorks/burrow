use super::*;

#[test]
fn queued_automation_reloads_scope_and_explicitly_vacates_sorting() {
    let tmp = TempDir::new().unwrap();
    clones(&tmp);
    let jobs = jobs_dir(&tmp);
    let mut cfg = config(&jobs, 4);
    cfg.projects = tmp.path().to_path_buf();
    let settings = burrow_settings::Config {
        overlay: tmp.path().join("overlay.json"),
        allowed: vec!["sam/box".into(), "sam/web".into()],
        pipeline_repos: vec!["sam/box".into()],
    };
    cfg.settings = Some(settings.clone());
    let job = Job::new(&cfg, "queued");
    fs::create_dir_all(&job.dir).unwrap();
    fs::write(
        job.path("project"),
        tmp.path().join("web").to_str().unwrap(),
    )
    .unwrap();
    fs::write(job.path("role"), "steward-judged").unwrap();
    assert!(!crate::slot::automation_paused(&cfg, &job));
    settings
        .command(&["pipeline".into(), (now() + 60).to_string()], "test")
        .unwrap();
    assert!(crate::slot::automation_paused(&cfg, &job));
    assert!(cfg.settings_line().contains("paused: sam/burrow, sam/web"));
    crate::policy::bind(
        &job,
        crate::policy::Identity::Repository("sam/web".into()),
        crate::policy::Kind::Sorting,
    )
    .unwrap();
    fs::write(job.path("role"), "triage").unwrap();
    assert!(crate::slot::automation_paused(&cfg, &job));
    fs::write(job.path("role"), "planner").unwrap();
    assert!(crate::slot::automation_paused(&cfg, &job));
    fs::remove_file(job.path("role")).unwrap();
    fs::write(job.path("usage-kind"), "inbox").unwrap();
    assert!(
        crate::slot::automation_paused(&cfg, &job),
        "roleless inbox also pauses"
    );
    fs::write(job.path("role"), "steward-judged").unwrap();
    fs::write(
        job.path("project"),
        tmp.path().join("box").to_str().unwrap(),
    )
    .unwrap();
    assert!(
        crate::slot::automation_paused(&cfg, &job),
        "mutable project metadata cannot replace bound identity"
    );
    crate::policy::bind(
        &job,
        crate::policy::Identity::Repository("sam/box".into()),
        crate::policy::Kind::Automated,
    )
    .unwrap();
    assert!(!crate::slot::automation_paused(&cfg, &job));
    settings
        .command(&["pause".into(), "sam/box".into()], "test")
        .unwrap();
    assert!(crate::slot::automation_paused(&cfg, &job));
}

#[test]
fn a_missing_policy_never_opts_a_repository_in() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = config(&jobs_dir(&tmp), 4);
    cfg.settings = None;
    assert!(cfg.paused("sam/new"));
}

#[test]
fn paused_turns_follow_a_deployed_policy_on_both_engines() {
    for mode in ["paused-hand-over", "paused-codex-hand-over"] {
        let tmp = TempDir::new().unwrap();
        clones(&tmp);
        let jobs = jobs_dir(&tmp);
        let _kid = waiter_to(mode, &jobs, &tmp, "paused", 1);
        let handed = jobs.join("paused/handed");
        until("a paused turn follows the deployed policy", || {
            handed.exists()
        });
        assert_eq!(fs::read_to_string(handed).unwrap(), "unmarked\n");
        assert!(!jobs.join("paused/took").exists());
    }
}

pub(super) fn bind_fixture(jobs: &Path, max_jobs: usize) {
    let cfg = config(jobs, max_jobs);
    for entry in fs::read_dir(jobs)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('.'))
    {
        let id = entry.file_name().to_string_lossy().into_owned();
        let job = Job::new(&cfg, &id);
        if !crate::policy::path(&job, "admission").exists() {
            let identity =
                crate::policy::identify(&cfg, &crate::pick::project(&job.dir), false, true)
                    .unwrap();
            crate::policy::bind(&job, identity, crate::policy::Kind::Automated).unwrap();
        }
    }
}

#[test]
fn unresolved_automation_is_closed_and_local_work_is_explicit() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let cfg = config(&jobs, 4);
    let job = Job::new(&cfg, "unknown");
    fs::create_dir_all(&job.dir).unwrap();
    fs::write(job.path("project"), "/missing/project").unwrap();
    fs::write(job.path("role"), "steward-judged").unwrap();
    assert!(crate::policy::identify(&cfg, "/missing/project", true, false).is_err());
    assert!(crate::policy::denied(&job));
    assert!(!crate::policy::path(&job, "admission").exists());
    let local = crate::policy::identify(&cfg, "/missing/project", true, true).unwrap();
    crate::policy::bind(&job, local, crate::policy::Kind::Automated).unwrap();
    assert!(!crate::policy::denied(&job));
}

#[test]
fn an_emergency_waiter_rechecks_policy_after_its_lock_is_released() {
    let tmp = TempDir::new().unwrap();
    clones(&tmp);
    let jobs = jobs_dir(&tmp);
    let cfg = config(&jobs, 1);
    let held = File::create(jobs.join(".slots/unstick")).unwrap();
    held.lock().unwrap();
    let job = Job::new(&cfg, "u");
    fs::create_dir_all(&job.dir).unwrap();
    fs::write(
        job.path("project"),
        tmp.path().join("web").to_str().unwrap(),
    )
    .unwrap();
    fs::write(job.path("unstick"), "").unwrap();
    let _kid = spawn_child(
        "take",
        &jobs,
        1,
        &["_exec".into(), "u".into(), "1".into(), "normal".into()],
    );
    sleep(Duration::from_secs(2));
    cfg.settings
        .as_ref()
        .unwrap()
        .command(&["pause".into(), "sam/web".into()], "test")
        .unwrap();
    held.unlock().unwrap();
    sleep(Duration::from_secs(3));
    assert!(!job.turn("1", "slot").exists());
    assert_eq!(took(&jobs, "u"), None);
    cfg.settings
        .as_ref()
        .unwrap()
        .command(&["allow".into(), "sam/web".into()], "test")
        .unwrap();
    until("an emergency waiter resumes", || took(&jobs, "u").is_some());
}

#[test]
fn an_unlisted_repository_can_sort_until_explicitly_paused() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let cfg = config(&jobs, 1);
    let job = Job::new(&cfg, "sorting");
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Repository("sam/new".into()),
        crate::policy::Kind::Sorting,
    )
    .unwrap();
    assert!(!crate::policy::denied(&job));
    cfg.settings
        .as_ref()
        .unwrap()
        .command(&["pause".into(), "sam/new".into()], "test")
        .unwrap();
    assert!(crate::policy::denied(&job));
}

// A waiter whose repository the settings pause keeps its place but is neither
// in the line status shows nor offered a slot by a look, so it reserves nothing.
#[test]
fn a_paused_waiter_holds_no_place_in_the_line() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    clones(&tmp);
    let mut cfg = config(&jobs, 6);
    let _holder = line_of_size(&tmp, &jobs, 4);
    for j in ["job1", "job2"] {
        fs::write(jobs.join(j).join("role"), "work\n").unwrap();
    }
    cfg.settings
        .as_mut()
        .unwrap()
        .allowed
        .retain(|r| r != "sam/box");
    let got = in_line(&cfg);
    let ids: Vec<&str> = got
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    assert!(!ids.contains(&"job1"), "{got}");
    assert!(ids.contains(&"job2"), "{got}");
    let seen = look(&cfg, "none", &jobs.join("job0"), Class::Normal).unwrap();
    assert!(seen.line.iter().all(|w| !w.name.ends_with("job1")));
    assert!(seen.line.iter().any(|w| w.name.ends_with("job2")));
    assert_eq!(in_queue(&jobs), 4, "its place is kept");
}

#[test]
fn a_bound_repository_survives_a_lost_origin_and_cannot_change_identity() {
    let tmp = TempDir::new().unwrap();
    clones(&tmp);
    let jobs = jobs_dir(&tmp);
    let mut cfg = config(&jobs, 1);
    let dir = jobs.join("identity");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("project"),
        tmp.path().join("box").to_str().unwrap(),
    )
    .unwrap();
    let job = Job::new(&cfg, "identity");
    assert!(!crate::policy::denied(&job));
    fs::write(dir.join("project"), "/missing/project").unwrap();
    assert!(
        !crate::policy::denied(&job),
        "bound repository survives a lost origin"
    );
    drop(job);
    cfg.settings
        .as_mut()
        .unwrap()
        .allowed
        .retain(|r| r != "sam/box");
    let job = Job::new(&cfg, "identity");
    assert!(
        crate::policy::denied(&job),
        "paused repo with lost origin stays paused"
    );
    fs::write(
        dir.join("project"),
        tmp.path().join("web").to_str().unwrap(),
    )
    .unwrap();
    assert!(
        crate::policy::denied(&job),
        "metadata cannot change the repository"
    );
}

#[test]
fn an_unparsed_nonempty_origin_never_grants_a_local_exemption() {
    let tmp = TempDir::new().unwrap();
    clones(&tmp);
    let jobs = jobs_dir(&tmp);
    let cfg = config(&jobs, 1);
    let project = tmp.path().join("box");
    git(&[
        "-C",
        project.to_str().unwrap(),
        "remote",
        "set-url",
        "origin",
        "https://github.com:443/sam/box.git",
    ]);
    assert!(!matches!(
        crate::policy::identify(&cfg, project.to_str().unwrap(), true, false),
        Ok(crate::policy::Identity::Local)
    ));
}

#[test]
fn legacy_inbox_identity_comes_from_its_host_owned_source() {
    let tmp = TempDir::new().unwrap();
    clones(&tmp);
    let jobs = jobs_dir(&tmp);
    let cfg = config(&jobs, 1);
    let job = Job::new(&cfg, "legacy");
    fs::create_dir_all(&job.dir).unwrap();
    crate::ask::ensure(&job).unwrap();
    fs::write(crate::ask::host_path(&job, "source"), "sam/web#7").unwrap();
    fs::write(
        job.path("project"),
        tmp.path().join("box").to_str().unwrap(),
    )
    .unwrap();
    cfg.settings
        .as_ref()
        .unwrap()
        .command(&["pause".into(), "sam/web".into()], "test")
        .unwrap();
    assert!(
        crate::policy::denied(&job),
        "writable metadata cannot replace trusted legacy source"
    );
}

#[test]
fn restoration_reserves_order_until_the_earlier_worker_registers() {
    for codex in [false, true] {
        let tmp = TempDir::new().unwrap();
        clones(&tmp);
        let jobs = jobs_dir(&tmp);
        let cfg = config(&jobs, 1);
        for (id, order) in [("earlier", "1-1-earlier"), ("later", "2-2-later")] {
            let job = Job::new(&cfg, id);
            fs::create_dir_all(&job.dir).unwrap();
            fs::write(
                job.path("project"),
                tmp.path().join("web").to_str().unwrap(),
            )
            .unwrap();
            job.set_turns(1).unwrap();
            crate::policy::bind(
                &job,
                crate::policy::Identity::Local,
                crate::policy::Kind::Manual,
            )
            .unwrap();
            fs::write(crate::policy::path(&job, "order"), order).unwrap();
            fs::write(crate::policy::path(&job, "restoring"), "reserved").unwrap();
            if codex {
                fs::write(job.path("engine"), "codex").unwrap();
            }
        }
        let later = Job::new(&cfg, "later");
        assert!(crate::policy::earlier_pending(&later));
        let _child = spawn_child(
            if codex { "codex" } else { "take" },
            &jobs,
            1,
            &["_exec".into(), "later".into(), "1".into(), "normal".into()],
        );
        sleep(Duration::from_secs(3));
        assert!(
            !later.path("took").exists(),
            "later restoration overtook unpublished earlier work"
        );
        crate::policy::accepted(&Job::new(&cfg, "earlier")).unwrap();
        until("the registered restoration's slot", || {
            later.path("took").exists()
        });
        assert!(!crate::policy::path(&later, "restoring").exists());
    }
}

#[test]
fn an_unpause_mid_scan_restarts_order_before_resuming_later_work() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let mut cfg = config(&jobs, 1);
    cfg.tools.systemctl = tmp.path().join("systemctl");
    cfg.tools.systemd_run = tmp.path().join("systemd-run");
    let settings = cfg.settings.as_ref().unwrap();
    settings
        .command(&["allow".into(), "sam/web".into()], "test")
        .unwrap();
    let allowed = tmp.path().join("allowed.json");
    fs::copy(&settings.overlay, &allowed).unwrap();
    settings
        .command(&["pause".into(), "sam/web".into()], "test")
        .unwrap();
    for (id, first, order) in [
        ("earlier", true, "1-1-earlier"),
        ("later", false, "2-2-later"),
    ] {
        let job = Job::new(&cfg, id);
        fs::create_dir_all(&job.dir).unwrap();
        job.set_turns(1).unwrap();
        crate::policy::bind(
            &job,
            crate::policy::Identity::Repository("sam/web".into()),
            crate::policy::Kind::Automated,
        )
        .unwrap();
        crate::policy::mark(&job, "queued", first).unwrap();
        fs::write(crate::policy::path(&job, "order"), order).unwrap();
    }
    // The first job was observed paused. Release while checking its unit,
    // after it has been passed over but before the later job is examined.
    burrow_test::executable(
        &cfg.tools.systemctl,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n*ActiveState*proj-earlier.service*) cp '{}' '{}' ;;\nesac\ncase \"$*\" in\n*ActiveState*) echo inactive ;;\n*LoadState*) echo not-found ;;\nesac\n",
            allowed.display(),
            settings.overlay.display()
        ),
    );
    let starts = tmp.path().join("starts");
    burrow_test::executable(
        &cfg.tools.systemd_run,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >>'{}'\n",
            starts.display()
        ),
    );
    crate::policy::reconcile(&cfg).unwrap();
    let starts = fs::read_to_string(starts).unwrap();
    let first = starts
        .find("_exec earlier first")
        .expect("earlier worker resumes");
    let later = starts
        .find("_exec later normal")
        .expect("later worker resumes");
    assert!(first < later, "release crossed the ordered scan: {starts}");
}
