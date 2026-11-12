use super::*;
use std::os::unix::fs::symlink;

fn retained(r: &Rig, id: &str) {
    let job = Job::new(&r.cfg, id);
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Automated,
    )
    .unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("project"), "/original").unwrap();
    fs::write(job.path("engine"), "codex").unwrap();
    fs::write(job.path("prompt.1"), "original prompt").unwrap();
    fs::write(job.turn("1", "jsonl"), "original transcript").unwrap();
    fs::write(
        job.turn("1", "deadline"),
        (now_nanos() + 60_000_000_000).to_string(),
    )
    .unwrap();
    Checkpoint::retry(&job, "1", false, now_nanos(), "refused").unwrap();
}

#[test]
fn recovery_replaces_records_without_following_guest_staging_or_destination_links() {
    let r = rig();
    retained(&r, "links");
    let job = Job::new(&r.cfg, "links");
    let secret = r.tmp.path().join("outside-sandbox");
    fs::write(&secret, "must remain unchanged").unwrap();
    for name in ["project", "1.engine", "1.spent", "1.deadline"] {
        symlink(&secret, job.path(&format!("{name}.new"))).unwrap();
        let _ = fs::remove_file(job.path(name));
        symlink(&secret, job.path(name)).unwrap();
    }
    Identity::restore(&job, "1").unwrap();
    let state = Checkpoint::load(&job, "1").unwrap().unwrap();
    state.resume(&job, "1", &job.path("prompt.1")).unwrap();
    assert_eq!(fs::read_to_string(secret).unwrap(), "must remain unchanged");
    assert_eq!(read(&job.path("project")).as_deref(), Some("/original"));
    assert!(
        read(&job.turn("1", "spent"))
            .unwrap()
            .contains("original prompt")
    );
    assert_eq!(
        read(&job.turn("1", "jsonl")).as_deref(),
        Some("original transcript")
    );
}

#[test]
fn recovery_never_follows_prompt_or_stream_links_for_reading_writing_or_touching() {
    for target in ["1.jsonl", "prompt.1"] {
        let r = rig();
        retained(&r, "stream");
        let job = Job::new(&r.cfg, "stream");
        let secret = r.tmp.path().join("outside-stream");
        fs::write(&secret, "secret transcript").unwrap();
        let before = fs::metadata(&secret).unwrap().modified().unwrap();
        fs::remove_file(job.path(target)).unwrap();
        symlink(&secret, job.path(target)).unwrap();
        let state = Checkpoint::load(&job, "1").unwrap().unwrap();
        assert!(state.resume(&job, "1", &job.path("prompt.1")).is_err());
        assert!(Checkpoint::load(&job, "1").unwrap().is_some());
        assert_eq!(fs::read_to_string(&secret).unwrap(), "secret transcript");
        assert_eq!(fs::metadata(secret).unwrap().modified().unwrap(), before);
    }
}

#[test]
fn a_saved_identity_and_deleted_slot_cannot_authorize_stopping_active_work() {
    for pool in ["claude", "codex", "unstick"] {
        let mut r = rig();
        start(
            &mut r,
            "active",
            &format!("old-running-{pool}"),
            "/ordinary",
        );
        until(|| r.cfg.jobs.join("active/running").exists());
        let job = Job::new(&r.cfg, "active");
        fs::write(job.turn("1", "engine"), pool).unwrap();
        if pool == "unstick" {
            fs::write(job.path("unstick"), "").unwrap();
        }
        let pid = read(&job.path("pid")).unwrap();
        fs::remove_file(job.turn("1", "slot")).unwrap();
        let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
        barrier.lock().unwrap();
        let _ = pass(&r.cfg);
        assert_eq!(
            read(&job.path("pid")).as_deref(),
            Some(pid.as_str()),
            "active {pool} parent replaced"
        );
        assert!(systemd::active(&r.cfg, &job.unit()));
        assert!(!job.turn("1", "retry-migration").exists());
    }
}

#[test]
fn startup_saves_the_resolved_choice_despite_later_settings_changes() {
    let mut r = rig();
    r.cfg.roles = r.tmp.path().join("roles");
    fs::write(&r.cfg.roles, "worker claude\n").unwrap();
    let job = Job::new(&r.cfg, "choice");
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("role"), "worker").unwrap();
    let choice = engine::choice(&r.cfg, &job).unwrap();
    fs::write(&r.cfg.roles, "worker codex\n").unwrap();
    Identity::capture_choice(&job, "1", &choice).unwrap();
    assert_eq!(Identity::load(&job, "1").unwrap().unwrap().choice, choice);
    assert_eq!(engine::choice(&r.cfg, &job).unwrap(), choice);
}
