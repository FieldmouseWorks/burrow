use super::*;

fn pending(r: &Rig, id: &str) {
    let job = Job::new(&r.cfg, id);
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("project"), "/ordinary").unwrap();
    Checkpoint::retry(&job, "1", false, now_nanos(), "refused").unwrap();
}

#[test]
fn sandbox_files_cannot_authorize_recovery_or_change_retained_identity() {
    let r = rig();
    pending(&r, "forged");
    let job = Job::new(&r.cfg, "forged");
    let bytes = fs::read(job.turn("1", "retry-migration")).unwrap();
    fs::remove_file(job.turn("1", "retry-migration")).unwrap();
    // These are precisely the paths the sandbox can write; none is authority.
    fs::write(job.dir.join("1.retry-migration"), bytes).unwrap();
    fs::write(job.dir.join("1.retry-startup-failed"), "").unwrap();
    fs::write(job.turn("1", "engine"), "claude").unwrap();
    fs::write(job.turn("1", "outcome"), "done").unwrap();
    pass(&r.cfg).unwrap();
    assert!(
        !job.path("pid").exists(),
        "sandbox authorized a host relaunch"
    );

    pending(&r, "pinned");
    let job = Job::new(&r.cfg, "pinned");
    // The trusted starter used the configured Claude engine. A guest cannot
    // substitute another execution identity or App during a retained resume.
    fs::write(job.path("engine"), "codex").unwrap();
    fs::write(job.turn("1", "engine"), "codex").unwrap();
    assert_eq!(
        engine::choice(&r.cfg, &job).unwrap().engine,
        engine::Engine::Claude
    );
}

#[test]
fn queue_capture_matches_job_identity_even_when_a_stale_pid_is_reused() {
    let r = rig();
    pending(&r, "capture");
    let job = Job::new(&r.cfg, "capture");
    let queue = r.cfg.jobs.join(".queue");
    fs::write(queue.join("0-100-another"), "12345\n").unwrap();
    let state = checkpoint(&r.cfg, &job, "1", 12345, false).unwrap();
    assert!(state.name.ends_with("-capture"));
    fs::write(queue.join("2-200-capture"), "12345\n").unwrap();
    let state = checkpoint(&r.cfg, &job, "1", 12345, false).unwrap();
    assert_eq!(state.name, "2-200-capture");
}

#[test]
fn startup_without_an_outcome_is_rate_limited_without_blocking_other_jobs() {
    let r = rig();
    pending(&r, "broken");
    let count = r.tmp.path().join("launches");
    burrow_test::executable(
        &r.cfg.tools.systemd_run,
        format!("#!/bin/sh\necho launched >>'{}'\n", count.display()),
    );
    pass(&r.cfg).unwrap();
    assert_eq!(pass(&r.cfg).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 1);
    pending(&r, "healthy");
    assert_eq!(pass(&r.cfg).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    assert_eq!(fs::read_to_string(&count).unwrap().lines().count(), 2);
}

#[test]
fn confirmed_cancellation_finalizes_even_when_the_end_hook_already_deferred() {
    let r = rig();
    pending(&r, "cancel");
    let job = Job::new(&r.cfg, "cancel");
    crate::cmd::finish(&r.cfg, job.id).unwrap();
    assert!(!job.turn("1", "ended").exists());
    crate::cmd::halt(&r.cfg, &job, false).unwrap();
    assert!(job.turn("1", "ended").exists());
    let ended = crate::mtime_nanos(&job.turn("1", "ended"));
    crate::cmd::finish(&r.cfg, job.id).unwrap();
    assert_eq!(crate::mtime_nanos(&job.turn("1", "ended")), ended);
}

#[test]
fn legacy_adoption_requires_kernel_census_waiting_not_sandbox_artifacts() {
    let mut r = rig();
    usage::record(
        &r.cfg,
        "limit",
        Some(crate::now() + 3600),
        "refused",
        crate::now(),
    )
    .unwrap();
    start(&mut r, "legacy", "old", "/ordinary");
    until(|| fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count() == 1);
    let job = Job::new(&r.cfg, "legacy");
    fs::remove_file(job.turn("1", "execution")).unwrap();
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    let pid = read(&job.path("pid")).unwrap().parse().unwrap();
    // An old parent sleeping through usage has no independent proof yet.
    assert!(!authority::legacy_waiting(&r.cfg, pid).unwrap());
    assert_eq!(pass(&r.cfg).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    assert!(!job.turn("1", "retry-migration").exists());
    usage::back(&r.cfg, None);
    until(|| authority::legacy_waiting(&r.cfg, pid).unwrap());
    pass(&r.cfg).unwrap();
    until(|| job.path("took").exists());
    assert!(!job.turn("1", "retry-migration").exists());
}
