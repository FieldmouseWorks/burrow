use super::*;

#[test]
fn queued_automation_reloads_scope_but_triage_can_sort_everywhere() {
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
    fs::write(job.path("role"), "triage").unwrap();
    assert!(!crate::slot::automation_paused(&cfg, &job));
    fs::write(job.path("role"), "planner").unwrap();
    assert!(!crate::slot::automation_paused(&cfg, &job));
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
