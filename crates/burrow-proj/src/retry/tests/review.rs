use super::*;

pub(super) fn saved(r: &Rig, id: &str) -> Checkpoint {
    let job = Job::new(&r.cfg, id);
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Automated,
    )
    .unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("project"), "/ordinary\n").unwrap();
    fs::write(job.path("prompt.1"), "original\n").unwrap();
    fs::write(job.turn("1", "jsonl"), "original stream\n").unwrap();
    Identity::capture(&job, "1").unwrap();
    let state = Checkpoint {
        waited: now_nanos() - 10_000_000_000,
        said: Some("refused".into()),
        deadline: Some(now_nanos() - 1_000_000_000),
        name: format!("2-100-{id}"),
        first: false,
        retained: Some(true),
    };
    swap(
        &job.turn("1", "retry-migration"),
        &serde_json::to_string(&state).unwrap(),
    )
    .unwrap();
    state
}

#[test]
fn cancelled_recovery_uses_the_latest_locked_turn_and_cannot_replay() {
    let r = rig();
    saved(&r, "cancelled");
    let job = Job::new(&r.cfg, "cancelled");
    let lock = job.lock().unwrap();
    // Another test's fork can retain this open description until exec. Keep
    // a copy alive to prove recovery doesn't depend on that child closing it.
    let _cancelled_copy = lock.as_fd().try_clone_to_owned().unwrap();
    assert_eq!(pass(&r.cfg).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    fs::write(job.turn("1", "stopped"), "").unwrap();
    fs::remove_file(job.turn("1", "retry-migration")).unwrap();
    // Dropping the transaction unlocks even while the child retains its copy.
    drop(lock);
    pass(&r.cfg).unwrap();
    assert!(!job.path("pid").exists());
    assert_eq!(
        read(&job.turn("1", "jsonl")).as_deref(),
        Some("original stream")
    );
    // A successor turn selected under the same lock cannot inherit turn 1's
    // recovery state, even though its old checkpoint is still on disk.
    fs::remove_file(job.turn("1", "stopped")).unwrap();
    let state = saved(&r, "cancelled");
    let lock = job.lock().unwrap();
    let _successor_copy = lock.as_fd().try_clone_to_owned().unwrap();
    assert!(pass(&r.cfg).is_err());
    job.set_turns(2).unwrap();
    drop(lock);
    pass(&r.cfg).unwrap();
    assert!(!job.path("pid").exists());
    assert!(Checkpoint::load(&job, "1").unwrap().is_some());
    assert!(state.deadline.is_some());
}

#[test]
fn failed_stop_and_withdrawal_preserve_checkpoint_and_remaining_budget() {
    for withdraw in [false, true] {
        let mut r = rig();
        usage::record(
            &r.cfg,
            "hold",
            Some(crate::now() + 3600),
            "refused",
            crate::now(),
        )
        .unwrap();
        start(&mut r, "retry", "new-retry", "/ordinary");
        let job = Job::new(&r.cfg, "retry");
        until(|| job.turn("1", "withdrawable").exists());
        let checkpoint = fs::read(job.turn("1", "retry-migration")).unwrap();
        let stream = fs::read(job.turn("1", "jsonl")).unwrap();
        let script = fs::read_to_string(&r.cfg.tools.systemctl)
            .unwrap()
            .replace(" stop:) [ -z", " stop:) exit 1 ;;\n ignored:) [ -z")
            .replace(
                " show:LoadState)",
                " show:ActiveState) echo active ;;\n show:LoadState)",
            );
        burrow_test::executable(&r.cfg.tools.systemctl, script);
        assert!(crate::cmd::stop(&r.cfg, "retry", withdraw).is_err());
        assert_eq!(
            fs::read(job.turn("1", "retry-migration")).unwrap(),
            checkpoint
        );
        assert_eq!(fs::read(job.turn("1", "jsonl")).unwrap(), stream);
        assert!(!job.turn("1", "stopped").exists());
        usage::back(&r.cfg, None);
        until(|| job.path("took").exists());
    }
}

#[test]
fn pending_transfers_are_queued_in_lifecycle_and_watchdog_even_with_stale_slots() {
    let r = rig();
    saved(&r, "pending");
    let job = Job::new(&r.cfg, "pending");
    for stale in [false, true] {
        if stale {
            fs::write(job.turn("1", "slot"), "1\n").unwrap();
            fs::write(job.turn("1", "retry-startup-failed"), "").unwrap();
            fs::write(job.turn("1", "outcome"), "failed startup\n").unwrap();
        }
        assert_eq!(job.state(), "queued");
        let (state, holds) = job.lifecycle().unwrap();
        assert_eq!(state, "queued");
        assert!(holds.iter().any(|h| h == "retry"));
        assert_eq!(job.ended("1"), None);
        let facts = job.facts().unwrap();
        assert_eq!(facts.state, "queued");
        assert_eq!(facts.slot, 0);
        assert_eq!(facts.ended, 0);
    }
    fs::remove_file(job.turn("1", "retry-startup-failed")).unwrap();
    fs::write(job.turn("1", "outcome"), "done\n").unwrap();
    pass(&r.cfg).unwrap();
    assert!(!job.turn("1", "retry-migration").exists());
    assert!(!job.path("pid").exists());
    assert_eq!(job.state(), "done");
}

#[test]
fn recovery_skipped_for_a_busy_lock_is_explicitly_retried() {
    let r = rig();
    saved(&r, "pending");
    let job = Job::new(&r.cfg, "pending");
    let lock = job.lock().unwrap();
    let _inherited_copy = lock.as_fd().try_clone_to_owned().unwrap();
    assert_eq!(pass(&r.cfg).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    assert!(!job.path("pid").exists());
    drop(lock);
    pass(&r.cfg).unwrap();
    until(|| job.path("took").exists());
}

#[test]
fn old_parent_skipped_for_a_busy_lock_is_explicitly_retried() {
    let mut r = rig();
    usage::record(
        &r.cfg,
        "hold",
        Some(crate::now() + 3600),
        "refused",
        crate::now(),
    )
    .unwrap();
    start(&mut r, "retry", "old", "/ordinary");
    until(|| r.cfg.jobs.join("retry/1.withdrawable").exists());
    let job = Job::new(&r.cfg, "retry");
    let lock = job.lock().unwrap();
    let _inherited_copy = lock.as_fd().try_clone_to_owned().unwrap();
    assert_eq!(pass(&r.cfg).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    drop(lock);
    native_waits(&r);
    pass(&r.cfg).unwrap();
    assert!(Checkpoint::load(&job, "1").unwrap().is_some());
    usage::back(&r.cfg, None);
    until(|| job.path("took").exists());
}

#[test]
fn failed_replacements_keep_the_old_cohort_visible_and_its_priority() {
    let mut r = rig();
    usage::record(
        &r.cfg,
        "hold",
        Some(crate::now() + 3600),
        "refused",
        crate::now() - 20,
    )
    .unwrap();
    for id in ["p0", "p1", "p2"] {
        saved(&r, id);
    }
    burrow_test::executable(&r.cfg.tools.systemd_run, "#!/bin/sh\nexit 1\n");
    assert!(pass(&r.cfg).is_err());
    usage::back(&r.cfg, None);
    for id in ["fresh0", "fresh1", "fresh2"] {
        start(&mut r, id, "new", "/ordinary");
    }
    until(|| r.cfg.jobs.join("fresh2/1.withdrawable").exists());
    sleep(Duration::from_secs(3));
    assert_eq!(
        took(&r.cfg),
        0,
        "dead parents' older places still own admission priority"
    );
    for id in ["p0", "p1", "p2"] {
        assert!(r.cfg.jobs.join(format!(".queue/2-100-{id}")).exists());
        crate::cmd::stop(&r.cfg, id, false).unwrap();
    }
    // Fresh arrivals came after release: once the old cohort is cancelled,
    // the ordinary capacity returns, rather than leaving a permanent cap.
    until(|| took(&r.cfg) == 3);
}

#[test]
fn codex_checkpoint_never_hands_over_to_unaware_rollback_startup() {
    let mut r = rig();
    let old = r.tmp.path().join("old-startup");
    let wrong = r.tmp.path().join("wrong-handover");
    burrow_test::executable(
        &old,
        format!(
            "#!/bin/sh\ncase $1 in\n_handover_format) echo 1 ;;\n_exec) touch '{}' ;;\n*) exit 2 ;;\nesac\n",
            wrong.display()
        ),
    );
    let path = r.tmp.path().join("config.json");
    let mut cfg: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    cfg["deployed"] = old.to_str().unwrap().into();
    fs::write(path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    saved(&r, "codex");
    fs::remove_file(Job::new(&r.cfg, "codex").turn("1", "execution")).unwrap();
    start(&mut r, "codex", "new-codex", "/ordinary");
    until(|| r.cfg.jobs.join("codex/took").exists() || wrong.exists());
    assert!(
        !wrong.exists(),
        "retained Codex turn handed over to destructive startup"
    );
    assert!(
        !Job::new(&r.cfg, "codex")
            .turn("1", "retry-migration")
            .exists()
    );
}

#[test]
fn initial_transfers_start_a_budget_and_unlimited_retries_stay_unlimited() {
    for slot in ["1", "codex-1", "unstick"] {
        let r = rig();
        let mut state = saved(&r, "initial");
        state.deadline = None;
        state.said = None;
        state.retained = Some(false);
        let job = Job::new(&r.cfg, "initial");
        fs::write(job.turn("1", "slot"), slot).unwrap();
        let acquired = crate::mtime_nanos(&job.turn("1", "slot")).unwrap();
        crate::turn::initialize_deadline(&job, "1", Some(&state), 30_000_000_000).unwrap();
        assert_eq!(
            read(&job.turn("1", "deadline"))
                .unwrap()
                .parse::<i64>()
                .unwrap(),
            acquired + 30_000_000_000
        );
        state.resume(&job, "1", &job.path("prompt.1")).unwrap();
        assert_eq!(
            read(&job.turn("1", "deadline"))
                .unwrap()
                .parse::<i64>()
                .unwrap(),
            acquired + 30_000_000_000
        );
        let mut unlimited = saved(&r, "unlimited");
        unlimited.deadline = None;
        let job = Job::new(&r.cfg, "unlimited");
        assert!(!unlimited.initial());
        fs::remove_file(job.turn("1", "deadline")).ok();
        crate::turn::initialize_deadline(&job, "1", Some(&unlimited), 30_000_000_000).unwrap();
        assert!(!job.turn("1", "deadline").exists());
        unlimited.resume(&job, "1", &job.path("prompt.1")).unwrap();
        assert!(!job.turn("1", "deadline").exists());
    }
}

#[test]
fn completed_checkpoint_artifacts_never_replay_any_terminal_outcome() {
    for outcome in ["done", "error", "failed", "timed out after 30s"] {
        let r = rig();
        saved(&r, "complete");
        let job = Job::new(&r.cfg, "complete");
        fs::write(job.turn("1", "outcome"), outcome).unwrap();
        pass(&r.cfg).unwrap();
        assert!(!job.turn("1", "retry-migration").exists());
        assert_eq!(read(&job.turn("1", "outcome")).as_deref(), Some(outcome));
        assert!(!job.path("pid").exists());
    }
}

#[test]
fn successful_stop_or_withdrawal_cancels_a_pending_transfer_even_when_inactive() {
    for withdraw in [false, true] {
        let r = rig();
        let state = saved(&r, "cancel");
        let job = Job::new(&r.cfg, "cancel");
        fs::write(job.turn("1", "slot"), "stale\n").unwrap();
        crate::cmd::stop(&r.cfg, "cancel", withdraw).unwrap();
        assert!(!job.turn("1", "retry-migration").exists());
        assert!(state.resume(&job, "1", &job.path("prompt.1")).is_err());
        pass(&r.cfg).unwrap();
        assert!(!job.path("pid").exists());
        assert_eq!(
            read(&job.turn("1", "jsonl")).as_deref(),
            Some("original stream")
        );
    }
}

#[test]
fn checkpoint_creation_yields_to_emergency_promotion_during_census_contention() {
    let mut r = rig();
    let census = File::create(r.cfg.jobs.join(".slots/.admission-lock")).unwrap();
    census.lock().unwrap();
    start(&mut r, "escape", "new-retry", "/ordinary");
    sleep(Duration::from_millis(100));
    assert!(!r.cfg.jobs.join("escape/1.retry-migration").exists());
    crate::job::Schedule::unstick(&Job::new(&r.cfg, "escape")).unwrap();
    until(|| r.cfg.jobs.join("escape/took").exists());
    assert_eq!(
        read(&r.cfg.jobs.join("escape/1.slot")).as_deref(),
        Some("unstick")
    );
}

#[test]
fn explicit_initial_transfer_does_not_restore_a_stale_prestartup_deadline() {
    let r = rig();
    let mut state = saved(&r, "initial");
    state.retained = Some(false);
    state.deadline = Some(1);
    state.said = None;
    let job = Job::new(&r.cfg, "initial");
    fs::write(job.turn("1", "slot"), "1\n").unwrap();
    let acquired = crate::mtime_nanos(&job.turn("1", "slot")).unwrap();
    crate::turn::initialize_deadline(&job, "1", Some(&state), 30_000_000_000).unwrap();
    state.resume(&job, "1", &job.path("prompt.1")).unwrap();
    assert_eq!(
        read(&job.turn("1", "deadline"))
            .unwrap()
            .parse::<i64>()
            .unwrap(),
        acquired + 30_000_000_000
    );
}
