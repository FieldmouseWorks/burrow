use super::*;

#[test]
fn pending_transfer_is_unfinished_work_until_stopped() {
    let r = rig();
    let job = Job::new(&r.cfg, "p");
    pending(&r, "p", true);
    // Unit inactive, outcome absent or left by a failed launch: not finished.
    assert_eq!(job.state(), "queued");
    fs::write(job.turn("1", "retry-startup-failed"), "").unwrap();
    fs::write(job.turn("1", "outcome"), "failed\n").unwrap();
    assert_eq!(job.state(), "queued");
    assert_eq!(job.ended("1"), None);
    assert_eq!(job.lifecycle().unwrap().0, "queued");
    fs::write(job.turn("1", "stopped"), "").unwrap();
    assert_eq!(job.state(), "failed");
}

#[test]
fn pending_transfer_keeps_its_place_in_the_line() {
    let r = rig();
    let (_, name) = pending(&r, "p", true);
    // Its old process is gone (no such pid).
    fs::write(r.cfg.jobs.join(".queue").join(&name), "99999999\n").unwrap();
    assert!(!slot::line(&r.cfg).is_empty());
    let job = Job::new(&r.cfg, "p");
    fs::write(job.turn("1", "stopped"), "").unwrap();
    assert!(slot::line(&r.cfg).is_empty());
}

#[test]
fn busy_job_lock_is_reported_for_retry_and_a_stopped_checkpoint_is_not_launched() {
    let r = rig();
    let job = Job::new(&r.cfg, "p");
    pending(&r, "p", true);
    let held = job.lock().unwrap();
    let _inherited_copy = held.as_fd().try_clone_to_owned().unwrap();
    assert_eq!(pass(&r.cfg).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    // A concurrent fork may still hold its copy when recovery runs again.
    drop(held);
    fs::write(job.turn("1", "stopped"), "").unwrap();
    assert!(pass(&r.cfg).is_ok());
    assert!(!job.turn("1", "retry-migration").exists());
    assert!(!job.path("took").exists());
}

#[test]
fn an_initial_waiter_keeps_no_deadline_but_a_retained_retry_does() {
    let r = rig();
    let job = Job::new(&r.cfg, "p");
    let (initial, _) = pending(&r, "p", false);
    assert!(initial.initial());
    assert!(initial.deadline.is_none());
    fs::write(job.turn("1", "deadline"), "5\n").unwrap();
    initial.resume(&job, "1", &job.path("prompt.1")).unwrap();
    assert_eq!(read(&job.turn("1", "deadline")).as_deref(), Some("5"));
    let (kept, _) = pending(&r, "p", true);
    let dl = kept.deadline.unwrap();
    kept.resume(&job, "1", &job.path("prompt.1")).unwrap();
    let moved: i64 = read(&job.turn("1", "deadline")).unwrap().parse().unwrap();
    assert!(moved >= dl);
}
