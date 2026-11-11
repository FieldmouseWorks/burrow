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
