use super::*;

fn legacy_nb_reset(r: &Rig) {
    usage::back(&r.cfg, None);
    until(|| {
        parents(&r.cfg)
            .unwrap()
            .iter()
            .any(|(_, id, _)| Job::new(&r.cfg, id).path("pid").exists())
    });
}

#[test]
fn nonblocking_predecessor_retries_transfer_under_the_reset_budget() {
    let mut r = rig();
    usage::record(
        &r.cfg,
        "limit",
        Some(crate::now() + 3600),
        "refused",
        crate::now(),
    )
    .unwrap();
    for i in 0..5 {
        start(&mut r, &format!("w{i}"), "old-nb", "/ordinary");
    }
    until(|| fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count() == 5);
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    let stream = fs::read(r.cfg.jobs.join("w0/1.jsonl")).unwrap();
    legacy_nb_reset(&r);
    // The polling parents never block on the barrier, so only the released
    // hold and the missing slot lock say they are waiting.
    until(|| pass(&r.cfg).is_ok() || { took(&r.cfg) > 0 });
    until(|| took(&r.cfg) == 2);
    sleep(Duration::from_secs(3));
    assert_eq!(took(&r.cfg), 2, "reset budget bypassed");
    assert_eq!(fs::read(r.cfg.jobs.join("w0/1.jsonl")).unwrap(), stream);
    assert_eq!(
        read(&r.cfg.jobs.join("w0/session")).unwrap(),
        "conversation-123"
    );
}

#[test]
fn nonblocking_running_turn_refused_after_deploy_transfers_and_keeps_its_slot_until_then() {
    let mut r = rig();
    start(&mut r, "running", "old-running-nb", "/ordinary");
    until(|| r.cfg.jobs.join("running/running").exists());
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    pass(&r.cfg).unwrap();
    let job = Job::new(&r.cfg, "running");
    assert!(
        !job.turn("1", "retry-migration").exists(),
        "active work was stopped"
    );
    fs::write(job.path("refuse"), "").unwrap();
    until(|| !job.turn("1", "slot").exists());
    for i in 0..4 {
        start(&mut r, &format!("w{i}"), "old-nb", "/ordinary");
    }
    legacy_nb_reset(&r);
    until(|| pass(&r.cfg).is_ok());
    until(|| {
        let count = took(&r.cfg);
        assert!(count <= 2, "reset budget bypassed: {count}");
        count == 2
    });
    sleep(Duration::from_secs(3));
    assert_eq!(took(&r.cfg), 2);
}
