//! A parked handoff is released by the inbox pass that holds the lock it
//! would otherwise wait for (#1009): passes run back to back, so a release
//! that only ever tries for that lock almost never gets it.
use super::resumption::{Permit, Wait};
use super::sorting_tests::Box;
use crate::job::Job;
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;

/// Two drained workers, "waiting" and "active", with "active" holding the
/// one drain slot while the `active` file is there. Returns the file
/// systemd-run touches when a turn launches.
fn drained_pair(fixture: &mut Box) -> PathBuf {
    fixture.cfg.inbox.as_mut().unwrap().wake = fixture.path("wake.json");
    fixture.cfg.kick.clear();
    fixture.cfg.notify = None;
    let active = fixture.path("active");
    burrow_test::executable(
        &fixture.cfg.tools.systemctl,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n*is-active*) test -e '{}';;\n*LoadState*) echo not-found;;\n*ActiveState*) echo inactive;;\nesac\n",
            active.display()
        ),
    );
    let launched = fixture.path("launched");
    fixture.cfg.tools.systemd_run = fixture.path("systemd-run");
    burrow_test::executable(
        &fixture.cfg.tools.systemd_run,
        format!("#!/bin/sh\ntouch '{}'\n", launched.display()),
    );
    for id in ["waiting", "active"] {
        fixture.job_ending(id, "done", Some("await"), "kept");
        let d = fixture.cfg.inbox.as_ref().unwrap().state.join(id);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("drained"), "sam/box#1").unwrap();
        fs::write(d.join("job"), id).unwrap();
    }
    fs::write(&active, "").unwrap();
    fs::write(fixture.cfg.jobs.join("active/1.slot"), "1").unwrap();
    launched
}

fn wake_lock(fixture: &Box) -> File {
    OpenOptions::new()
        .append(true)
        .create(true)
        .open(fixture.path("wake.json.lock"))
        .unwrap()
}

fn row(job: &Job) -> String {
    crate::snapshot::row(job, crate::host_jobs::View::Owner).line()
}

#[test]
fn a_pass_holding_the_lock_throughout_releases_a_parked_drained_turn() {
    let mut fixture = Box::new();
    let launched = drained_pair(&mut fixture);
    let job = Job::new(&fixture.cfg, "waiting");
    crate::turn::start(&fixture.cfg, job.id, "the await changed", false).unwrap();
    assert!(crate::policy::parked(&job));
    assert!(!launched.exists());
    assert!(
        row(&job).contains("[waiting for a drain slot]"),
        "{}",
        row(&job)
    );

    // The slot frees while a pass runs: the policy unit's own try for the
    // lock loses to it, and the park says so rather than a bare parked.
    fs::remove_file(fixture.path("active")).unwrap();
    let other = wake_lock(&fixture);
    other.lock().unwrap();
    crate::policy::reconcile(&fixture.cfg).unwrap();
    assert!(crate::policy::parked(&job));
    assert!(!launched.exists());
    assert_eq!(
        crate::policy::waits_for(&job).as_deref(),
        Some(Wait::Pass.says())
    );
    assert!(
        row(&job).contains("[waiting for the inbox pass]"),
        "{}",
        row(&job)
    );
    drop(other);

    // The next pass holds the lock from start to end, as passes back to back
    // do: it releases the parked turn before it lets go.
    let lock = wake_lock(&fixture);
    lock.lock().unwrap();
    let pass = Permit::pass(lock);
    super::release_parked(&fixture.cfg, pass);
    assert!(launched.exists(), "the pass must release the parked turn");
    assert!(!crate::policy::parked(&job));
    assert_eq!(
        job.turns().as_deref(),
        Some("2"),
        "resume the queued turn itself"
    );
    // And it let go.
    wake_lock(&fixture).try_lock().unwrap();
}

#[test]
fn a_reply_the_pass_hands_on_counts_capacity_under_its_lock() {
    let mut fixture = Box::new();
    let launched = drained_pair(&mut fixture);
    fs::remove_file(fixture.path("active")).unwrap();
    let lock = wake_lock(&fixture);
    lock.lock().unwrap();
    let pass = Permit::pass(lock);
    let job = Job::new(&fixture.cfg, "waiting");
    crate::turn::start_answer(&fixture.cfg, job.id, "the owner replied", false).unwrap();
    assert!(
        !crate::policy::parked(&job),
        "the pass's own reply must not wait for the pass's own lock"
    );
    assert!(launched.exists());
    drop(pass);
}
