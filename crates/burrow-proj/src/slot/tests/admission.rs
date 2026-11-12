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
    let got = crate::snapshot::Snapshot::take(&cfg, 0).unwrap().in_line();
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
    burrow_test::copy_file(&settings.overlay, &allowed);
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

#[test]
fn a_policy_pass_rewatches_what_the_watchdog_watches_not_every_job() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let mut cfg = config(&jobs, 1);
    cfg.waits = Some(tmp.path().join("waits.json"));
    cfg.tools.systemctl = tmp.path().join("systemctl");
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\n# As systemd answers for a unit that isn't running.\ncase \"$*\" in\n*is-active*) exit 3 ;;\n*ActiveState*) echo inactive ;;\n*LoadState*) echo not-found ;;\nesac\n",
    );
    // Two jobs whose turn failed: one the watchdog watches, and one long
    // done whose wait it settled (on remi, every job since October).
    for id in ["watched", "settled"] {
        let job = Job::new(&cfg, id);
        fs::create_dir_all(&job.dir).unwrap();
        job.set_turns(1).unwrap();
        fs::write(job.path("prompt.1"), "work").unwrap();
        crate::policy::bind(
            &job,
            crate::policy::Identity::Local,
            crate::policy::Kind::Manual,
        )
        .unwrap();
    }
    crate::waits::rewatch(&cfg, &["watched"]);
    let waits = jobs.join(".waits");
    assert!(waits.join("watched").exists());
    crate::policy::reconcile(&cfg).unwrap();
    assert!(waits.join("watched").exists());
    assert!(
        !waits.join("settled").exists(),
        "a policy pass put a settled job back in the watch"
    );
    let published: serde_json::Value =
        serde_json::from_slice(&fs::read(cfg.waits.as_ref().unwrap()).unwrap()).unwrap();
    let items: Vec<&String> = published["items"].as_object().unwrap().keys().collect();
    assert_eq!(items, ["watched"]);
}

#[test]
fn a_watch_a_pause_keeps_from_the_watchdog_comes_back_when_its_repo_does() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let mut cfg = config(&jobs, 1);
    cfg.waits = Some(tmp.path().join("waits.json"));
    cfg.tools.systemctl = tmp.path().join("systemctl");
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\n# As systemd answers for a unit that isn't running.\ncase \"$*\" in\n*is-active*) exit 3 ;;\n*ActiveState*) echo inactive ;;\n*LoadState*) echo not-found ;;\nesac\n",
    );
    let settings = cfg.settings.clone().unwrap();
    let say = |words: &[&str]| {
        settings
            .command(
                &words.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
                "test",
            )
            .unwrap()
    };
    say(&["allow", "sam/web"]);
    // A failed turn the watchdog watches, with nothing that parks it (no
    // question, await or retry), and a settled one it doesn't.
    for id in ["failed", "settled"] {
        let job = Job::new(&cfg, id);
        fs::create_dir_all(&job.dir).unwrap();
        job.set_turns(1).unwrap();
        fs::write(job.path("prompt.1"), "work").unwrap();
        crate::policy::bind(
            &job,
            crate::policy::Identity::Repository("sam/web".into()),
            crate::policy::Kind::Automated,
        )
        .unwrap();
    }
    crate::waits::rewatch(&cfg, &["failed"]);
    let watched = |id: &str| jobs.join(".waits").join(id).exists();
    assert!(watched("failed"));
    // Paused: the watchdog doesn't hear of it, but it isn't forgotten.
    say(&["pause", "sam/web"]);
    crate::policy::reconcile(&cfg).unwrap();
    assert!(!watched("failed"));
    assert!(jobs.join(".waits-held/failed").exists());
    // Allowed again: watched again, and the settled job still isn't.
    say(&["allow", "sam/web"]);
    crate::policy::reconcile(&cfg).unwrap();
    assert!(watched("failed"));
    assert!(!jobs.join(".waits-held/failed").exists());
    assert!(!watched("settled") && !jobs.join(".waits-held/settled").exists());
    // Stopped, it has no wait to keep or give back.
    say(&["pause", "sam/web"]);
    crate::policy::reconcile(&cfg).unwrap();
    fs::write(Job::new(&cfg, "failed").turn("1", "stopped"), "").unwrap();
    say(&["allow", "sam/web"]);
    crate::policy::reconcile(&cfg).unwrap();
    assert!(!watched("failed") && !jobs.join(".waits-held/failed").exists());
}

fn watch_config(tmp: &TempDir, jobs: &std::path::Path) -> Config {
    let mut cfg = config(jobs, 1);
    cfg.waits = Some(tmp.path().join("waits.json"));
    cfg.tools.systemctl = tmp.path().join("systemctl");
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\n# As systemd answers for a unit that isn't running.\ncase \"$*\" in\n*is-active*) exit 3 ;;\n*ActiveState*) echo inactive ;;\n*LoadState*) echo not-found ;;\nesac\n",
    );
    cfg
}

fn awaiting<'a>(cfg: &'a Config, id: &'a str, repo: &str) -> Job<'a> {
    let job = Job::new(cfg, id);
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("prompt.1"), "work").unwrap();
    fs::write(job.turn("1", "outcome"), "done").unwrap();
    fs::write(job.turn("1", "end"), "await").unwrap();
    fs::write(job.path("await"), "sam/web#1\t1000\t\n").unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Repository(repo.into()),
        crate::policy::Kind::Automated,
    )
    .unwrap();
    job
}

#[test]
fn a_held_watch_survives_a_failed_restore_until_it_is_committed() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let cfg = watch_config(&tmp, &jobs);
    let settings = cfg.settings.clone().unwrap();
    let say = |w: &[&str]| {
        settings
            .command(&w.iter().map(|w| w.to_string()).collect::<Vec<_>>(), "test")
            .unwrap()
    };
    say(&["allow", "sam/web"]);
    awaiting(&cfg, "held", "sam/web");
    crate::waits::rewatch(&cfg, &["held"]);
    say(&["pause", "sam/web"]);
    crate::policy::reconcile(&cfg).unwrap();
    let mark = jobs.join(".waits-held/held");
    assert!(mark.exists() && !jobs.join(".waits/held").exists());
    // The restored watch can't be written: the hold stays.
    fs::create_dir(jobs.join(".waits/held.new")).unwrap();
    say(&["allow", "sam/web"]);
    crate::policy::reconcile(&cfg).unwrap();
    assert!(mark.exists(), "the hold went before the watch was written");
    fs::remove_dir(jobs.join(".waits/held.new")).unwrap();
    crate::policy::reconcile(&cfg).unwrap();
    assert!(jobs.join(".waits/held").exists() && !mark.exists());
}

#[test]
fn a_parked_await_stays_held_until_its_release_is_committed() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let cfg = watch_config(&tmp, &jobs);
    let job = awaiting(&cfg, "parked", "sam/web");
    fs::write(
        crate::policy::path(&job, "park"),
        r#"{"state":"done","turn":"1","first":false,"at":1}"#,
    )
    .unwrap();
    crate::waits::rewatch(&cfg, &["parked"]);
    assert!(
        jobs.join(".waits-held/parked").exists(),
        "a parked job had no hold, so a failed release would lose it"
    );
}

#[test]
fn enabling_the_watchdog_registers_live_work_not_settled_jobs() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let cfg = watch_config(&tmp, &jobs);
    // systemd, noting what it was asked: one job's unit is up.
    let asked = tmp.path().join("asked");
    burrow_test::executable(
        &cfg.tools.systemctl,
        format!(
            "#!/bin/sh\necho \"$*\" >>'{}'\ncase \"$*\" in\n*list-units*) echo 'proj-running.service loaded active running proj job' ;;\n*is-active*proj-running.service) exit 0 ;;\n*is-active*) exit 3 ;;\n*ActiveState*proj-running.service) echo active ;;\n*ActiveState*) echo inactive ;;\n*LoadState*) echo not-found ;;\nesac\n",
            asked.display()
        ),
    );
    awaiting(&cfg, "live", "sam/web");
    let running = Job::new(&cfg, "running");
    fs::create_dir_all(&running.dir).unwrap();
    running.set_turns(1).unwrap();
    fs::write(running.path("prompt.1"), "work").unwrap();
    fs::write(running.turn("1", "slot"), "1").unwrap();
    fs::write(running.turn("1", "deadline"), "4000000000").unwrap();
    crate::policy::bind(
        &running,
        crate::policy::Identity::Local,
        crate::policy::Kind::Manual,
    )
    .unwrap();
    let settled = Job::new(&cfg, "settled");
    fs::create_dir_all(&settled.dir).unwrap();
    settled.set_turns(1).unwrap();
    fs::write(settled.path("prompt.1"), "work").unwrap();
    // Nothing was ever published for any (no watchdog then).
    assert!(!jobs.join(".waits").exists());
    crate::waits::rewatch_all(&cfg);
    assert!(jobs.join(".waits/live").exists());
    assert!(jobs.join(".waits/running").exists());
    assert!(!jobs.join(".waits/settled").exists());
    // The units that are up are asked once; a settled job's own never is.
    let asked = fs::read_to_string(&asked).unwrap();
    assert_eq!(asked.matches("list-units").count(), 1, "{asked}");
    assert!(!asked.contains("proj-settled.service"), "{asked}");
}
