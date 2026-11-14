use super::*;

#[test]
fn paused_retries_keep_their_turn_but_release_admission_until_ordered_restoration() {
    let mut r = rig();
    r.cfg.settings.as_mut().unwrap().overlay = r.tmp.path().join("settings.json");
    let settings = r.cfg.settings.as_ref().unwrap().clone();
    pending(&r, "retained", true);
    let job = Job::new(&r.cfg, "retained");
    crate::policy::bind(
        &job,
        crate::policy::Identity::Repository("sam/web".into()),
        crate::policy::Kind::Automated,
    )
    .unwrap();
    fs::write(job.path("session"), "same-conversation").unwrap();
    fs::write(job.turn("1", "jsonl"), "kept transcript").unwrap();
    // The original execution identity, rather than worker-writable changes,
    // must be restored when the ordered policy consumer launches this retry.
    fs::write(job.path("project"), "/worker-changed").unwrap();
    let checkpoint = fs::read(job.turn("1", "retry-migration")).unwrap();
    let execution = fs::read(job.turn("1", "execution")).unwrap();
    let starts = r.tmp.path().join("starts");
    burrow_test::executable(
        &r.cfg.tools.systemd_run,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >>'{}'\n",
            starts.display()
        ),
    );
    burrow_test::executable(
        &r.cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in\n*is-active*) exit 1;;\n*ActiveState*) echo inactive;;\n*LoadState*) echo not-found;;\nesac\n",
    );
    settings
        .command(&["pause".into(), "sam/web".into()], "test")
        .unwrap();
    pass(&r.cfg).unwrap();
    assert!(
        !starts.exists(),
        "retry recovery bypassed the owner's pause"
    );
    assert!(crate::policy::parked(&job));
    assert_eq!(
        read(&crate::policy::path(&job, "order")).as_deref(),
        Some("2-100-retained")
    );
    assert!(admission(&r.cfg).unwrap().is_empty());
    crate::policy::reconcile(&r.cfg).unwrap();
    assert!(!starts.exists());
    assert!(!r.cfg.jobs.join(".queue/2-100-retained").exists());

    settings
        .command(&["allow".into(), "sam/web".into()], "test")
        .unwrap();
    pass(&r.cfg).unwrap();
    assert!(
        !starts.exists(),
        "recovery overtook ordered policy restoration"
    );
    crate::policy::reconcile(&r.cfg).unwrap();
    let launched = read(&starts).unwrap();
    assert!(launched.contains("_exec retained normal"));
    assert_eq!(launched.lines().count(), 1);
    assert_eq!(job.turns().as_deref(), Some("1"));
    assert!(
        !job.path("project").exists(),
        "restore the captured absent project"
    );
    assert_eq!(
        read(&job.path("session")).as_deref(),
        Some("same-conversation")
    );
    assert_eq!(
        read(&job.turn("1", "jsonl")).as_deref(),
        Some("kept transcript")
    );
    assert_eq!(fs::read(job.turn("1", "execution")).unwrap(), execution);
    assert_eq!(
        fs::read(job.turn("1", "retry-migration")).unwrap(),
        checkpoint
    );
    assert!(crate::policy::path(&job, "restoring").exists());
    assert_eq!(admission(&r.cfg).unwrap().len(), 1);
}

#[test]
fn parking_a_retry_without_a_live_queue_entry_keeps_its_priority() {
    let r = rig();
    let (mut checkpoint, _) = pending(&r, "first", true);
    checkpoint.name = "1-100-first".into();
    checkpoint.first = true;
    let job = Job::new(&r.cfg, "first");
    swap(
        &job.turn("1", "retry-migration"),
        &serde_json::to_string(&checkpoint).unwrap(),
    )
    .unwrap();
    crate::policy::mark(&job, "queued", false, None).unwrap();
    let parked: crate::policy::Park =
        serde_json::from_slice(&fs::read(crate::policy::path(&job, "park")).unwrap()).unwrap();
    assert!(parked.first);
    assert_eq!(
        read(&crate::policy::path(&job, "order")).as_deref(),
        Some("1-100-first")
    );
}
