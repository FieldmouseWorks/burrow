//! An activation kept from waiting behind a run that was already going
//! (queue.rs, burrow#811), the switch's and the revert's: systemd's queue
//! as busctl says it (machine.rs).

use std::fs;
use std::thread;
use std::time::Duration;

use crate::machine::{Machine, SWITCH, ok, said};

fn job(id: u32, unit: &str, kind: &str, state: &str) -> String {
    format!(
        r#"[{id},"{unit}","{kind}","{state}","/org/freedesktop/systemd1/job/{id}","/unit/{unit}"]"#
    )
}

/// A sweep restarted behind a long run that was going before the
/// activation, as burrow#811's: in systemd's queue, and in what
/// switch-to-configuration lists.
fn behind_a_long_run(m: &Machine) {
    m.set(
        "jobs",
        &format!(
            "{},{}",
            job(1, "renovate.service", "start", "running"),
            job(9, "renovate-sweep.service", "restart", "waiting")
        ),
    );
    m.set("before-9", &job(1, "renovate.service", "start", "running"));
    m.set("lists", "renovate-sweep.service\n");
    // Nothing to wait for if it isn't let go.
    m.set("lists-ticks", "30");
}

const LET_GO: &str = "renovate-sweep.service restart waited behind renovate.service (job 1, running since before the activation): queued again, not waited for\n";

fn live(two: &str) -> String {
    format!("live: {two}\nrun box-confirm within 10min to keep it (and make it the boot default)\n")
}

/// Whether the sweep was left waiting: nothing was cancelled or queued.
fn left_waiting(m: &Machine) {
    let two = m.sys("two");
    assert_eq!(said(&m.switch(&["--toplevel", &two])), ok(&live(&two)));
    assert_eq!(m.fake("busctl"), "");
}

#[test]
fn an_activation_doesnt_wait_behind_a_run_that_was_already_going() {
    let m = Machine::new(false);
    let (one, two) = (m.sys("one"), m.sys("two"));
    behind_a_long_run(&m);
    let out = m.switch(&["--toplevel", &two]);
    // Let go, and queued again, so it still restarts after the run.
    assert_eq!(said(&out), (Some(0), live(&two), LET_GO.into()));
    assert_eq!(
        m.fake("busctl"),
        "cancel 9\nRestartUnit renovate-sweep.service ignore-requirements\n"
    );
    assert_eq!(
        m.fake("journal"),
        format!("-t box-switch -p warning {LET_GO}")
    );
    // The revert's activation, too.
    behind_a_long_run(&m);
    let out = m.switch(&["--revert-to", &one]);
    assert_eq!(said(&out), (Some(0), String::new(), LET_GO.into()));
    assert_eq!(
        m.fake("busctl"),
        "cancel 9\nRestartUnit renovate-sweep.service ignore-requirements\n"
    );
    assert_eq!(m.running(), one);
    // A job that ended before it was let go is left be.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("fail-cancel", "");
    let two = m.sys("two");
    assert_eq!(said(&m.switch(&["--toplevel", &two])), ok(&live(&two)));
    assert_eq!(m.fake("busctl"), "");
}

#[test]
fn a_job_that_has_started_or_whose_run_has_ended_since_it_was_read_is_kept() {
    // Read as waiting, started by the time it's about to be let go.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("state-9", "running");
    left_waiting(&m);
    // The run ended meanwhile.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("state-1", "done");
    left_waiting(&m);
}

#[test]
fn what_requires_a_job_or_must_follow_it_keeps_it_waited_for() {
    // A queued unit that requires the sweep would fail with its cancelling.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set(
        "RequiredBy-renovate-sweep.service",
        r#"["consumer.service"]"#,
    );
    m.set(
        "jobs",
        &format!(
            "{},{},{}",
            job(1, "renovate.service", "start", "running"),
            job(9, "renovate-sweep.service", "restart", "waiting"),
            job(10, "consumer.service", "start", "waiting")
        ),
    );
    left_waiting(&m);
    // Bound to it, or its requisite, the same.
    for prop in ["BoundBy", "RequisiteOf"] {
        let m = Machine::new(false);
        behind_a_long_run(&m);
        m.set(
            "jobs",
            &format!(
                "{},{},{}",
                job(1, "renovate.service", "start", "running"),
                job(9, "renovate-sweep.service", "restart", "waiting"),
                job(10, "consumer.service", "start", "waiting")
            ),
        );
        m.set(
            &format!("{prop}-renovate-sweep.service"),
            r#"["consumer.service"]"#,
        );
        left_waiting(&m);
    }
    // One with no job queued is no reason.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("RequiredBy-renovate-sweep.service", r#"["idle.service"]"#);
    let two = m.sys("two");
    assert_eq!(
        said(&m.switch(&["--toplevel", &two])),
        (Some(0), live(&two), LET_GO.into())
    );
    // A service ordered after it would start before it, in the gap; a
    // target after it is let through.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("after-9", &job(10, "consumer.service", "start", "waiting"));
    left_waiting(&m);
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("after-9", &job(10, "multi-user.target", "start", "waiting"));
    let two = m.sys("two");
    assert_eq!(
        said(&m.switch(&["--toplevel", &two])),
        (Some(0), live(&two), LET_GO.into())
    );
}

#[test]
fn a_target_is_not_let_go_while_it_waits_on_something_that_may_hang() {
    // multi-user.target waits on the sweep (behind the run) and on a new
    // unit that is starting: only the sweep is let go, so the switch still
    // waits for the new unit.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    // The new unit starts after the activation has begun.
    m.set(
        "jobs-later",
        &format!(
            "{},{},{},{}",
            job(1, "renovate.service", "start", "running"),
            job(9, "renovate-sweep.service", "restart", "waiting"),
            job(10, "hung.service", "start", "running"),
            job(11, "multi-user.target", "start", "waiting")
        ),
    );
    m.set("lists", "renovate-sweep.service\nmulti-user.target\n");
    m.set(
        "before-11",
        &format!(
            "{},{}",
            job(9, "renovate-sweep.service", "restart", "waiting"),
            job(10, "hung.service", "start", "running")
        ),
    );
    let two = m.sys("two");
    assert_eq!(
        said(&m.switch(&["--toplevel", &two])),
        (Some(0), live(&two), LET_GO.into())
    );
    assert_eq!(
        m.fake("busctl"),
        "cancel 9\nRestartUnit renovate-sweep.service ignore-requirements\n"
    );
}

#[test]
fn a_job_that_was_running_only_after_the_activation_began_is_no_run_to_wait_behind() {
    // Job 2 waited as the activation began and runs now.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("jobs", &job(1, "renovate.service", "start", "waiting"));
    // The look reads the queue as it begins: nothing running, nothing to see.
    m.set("lists", "app.service\n");
    m.set("before-9", &job(1, "renovate.service", "start", "running"));
    left_waiting(&m);
}

#[test]
fn a_job_let_go_that_cant_be_queued_again_fails_the_activation() {
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("fail-requeue", "");
    let two = m.sys("two");
    let out = m.switch(&["--toplevel", &two]);
    let (code, stdout, stderr) = said(&out);
    assert_eq!(code, Some(1));
    assert!(!stdout.contains("live:"), "{stdout}");
    assert!(
        stderr.contains("renovate-sweep.service was let go and could not be queued again: its start would be lost"),
        "{stderr}"
    );
    // Never said to be queued again.
    assert!(!stderr.contains("queued again, not waited for"), "{stderr}");
    assert!(m.fake("journal").contains("could not be queued again"));
    // The lost start stays where the next activation reads it back.
    let kept = std::fs::read_to_string(m.path("lists").join("activation-restart-list")).unwrap();
    assert!(
        kept.lines().any(|l| l == "renovate-sweep.service"),
        "{kept}"
    );
    assert!(m.fake("busctl").starts_with("cancel 9\n"));
    // The revert's activation fails the same way: it says so, and in the
    // journal with the exit the revert records.
    let out = m.switch(&["--revert-to", &m.sys("one")]);
    assert!(said(&out).2.contains("its start would be lost"));
    assert!(m.fake("journal").contains("(activation exit 127)"));
}

/// A switch with a 4s window: its activation's deadline is 3s.
fn switch_soon(m: &Machine) -> (Option<i32>, String, String) {
    let two = m.sys("two");
    said(
        &m.command(SWITCH, &["--toplevel", &two])
            .env("BURROW_CONFIRM_WITHIN", "4s")
            .output()
            .unwrap(),
    )
}

#[test]
fn a_job_queued_again_that_doesnt_wait_behind_the_run_is_waited_for() {
    // The run ended between the last read and the Cancel: the job queued
    // again starts at once, and the activation waits for it, as it would
    // have for the job it let go. One that never finishes fails it by its
    // deadline.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("requeued-state", "running");
    let (code, stdout, stderr) = switch_soon(&m);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(!stdout.contains("live:"), "{stdout}");
    assert!(
        stderr.contains(
            "renovate-sweep.service was let go and queued again, and didn't finish in time"
        ),
        "{stderr}"
    );
    // One that ran and failed fails it, the switch's and the revert's.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("requeued-ran", "");
    m.set("ActiveState-renovate-sweep.service", r#""failed""#);
    let (code, stdout, stderr) = switch_soon(&m);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(!stdout.contains("live:"), "{stdout}");
    assert!(
        stderr.contains("renovate-sweep.service was let go and queued again, and failed"),
        "{stderr}"
    );
    assert!(m.fake("busctl").starts_with("cancel 9\n"));
    behind_a_long_run(&m);
    let out = m.switch(&["--revert-to", &m.sys("one")]);
    assert!(
        said(&out)
            .2
            .contains("renovate-sweep.service was let go and queued again, and failed")
    );
    assert!(m.fake("journal").contains("(activation exit 127)"));
    // One that is still starting is waited for until it has.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("requeued-ran", "");
    m.set("ActiveState-renovate-sweep.service", r#""activating""#);
    let fake = m.path("fake");
    let started = thread::spawn(move || {
        thread::sleep(Duration::from_millis(1500));
        fs::write(
            fake.join("ActiveState-renovate-sweep.service"),
            r#""active""#,
        )
        .unwrap();
    });
    let two = m.sys("two");
    assert_eq!(
        switch_soon(&m),
        (Some(0), live(&two).replace("10min", "4s"), LET_GO.into())
    );
    started.join().unwrap();
    // One that ran and is up is done.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("requeued-ran", "");
    m.set("ActiveState-renovate-sweep.service", r#""active""#);
    let two = m.sys("two");
    assert_eq!(
        said(&m.switch(&["--toplevel", &two])),
        (Some(0), live(&two), LET_GO.into())
    );
}

#[test]
fn a_job_whose_kind_changed_since_it_was_read_is_kept() {
    // A reload merged into it after the queue was read, before the Cancel.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("type-9", "reload");
    left_waiting(&m);
    // Or between the look that found it and the read just before letting
    // it go: what's queued again is what it is now, never what it was.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set(
        "jobs-once-asked",
        &format!(
            "{},{}",
            job(1, "renovate.service", "start", "running"),
            job(9, "renovate-sweep.service", "reload", "waiting")
        ),
    );
    m.set("type-9", "reload");
    let two = m.sys("two");
    assert_eq!(
        said(&m.switch(&["--toplevel", &two])),
        (Some(0), live(&two), LET_GO.replace(" restart ", " reload ")),
    );
    assert_eq!(
        m.fake("busctl"),
        "cancel 9\nReloadUnit renovate-sweep.service ignore-requirements\n"
    );
}

#[test]
fn a_run_is_the_one_from_before_only_while_its_the_same_start() {
    // The unit's invocation changed: a new run of it, which may hang.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("InvocationID-renovate.service-later", "[2]");
    left_waiting(&m);
    // An invocation that names none is no run from before.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("InvocationID-renovate.service", "[0,0]");
    left_waiting(&m);
    // A reload running as the activation began: a later reload merged
    // into it can't be told from it.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set(
        "jobs",
        &format!(
            "{},{}",
            job(1, "renovate.service", "reload", "running"),
            job(9, "renovate-sweep.service", "restart", "waiting")
        ),
    );
    m.set("before-9", &job(1, "renovate.service", "reload", "running"));
    left_waiting(&m);
}

#[test]
fn what_a_job_requires_with_a_job_queued_keeps_it_waited_for() {
    // Queued again without its requirements, it would no longer fail with
    // a requirement's job that does.
    for prop in ["Requires", "BindsTo", "Requisite"] {
        let m = Machine::new(false);
        behind_a_long_run(&m);
        m.set(
            "jobs",
            &format!(
                "{},{},{}",
                job(1, "renovate.service", "start", "running"),
                job(9, "renovate-sweep.service", "restart", "waiting"),
                job(10, "prep.service", "start", "waiting")
            ),
        );
        m.set(
            &format!("{prop}-renovate-sweep.service"),
            r#"["prep.service"]"#,
        );
        left_waiting(&m);
    }
    // One with no job queued is no reason.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set(
        "Requires-renovate-sweep.service",
        r#"["system.slice","sysinit.target"]"#,
    );
    let two = m.sys("two");
    assert_eq!(
        said(&m.switch(&["--toplevel", &two])),
        (Some(0), live(&two), LET_GO.into())
    );
    assert_eq!(
        m.fake("busctl"),
        "cancel 9\nRestartUnit renovate-sweep.service ignore-requirements\n"
    );
}

#[test]
fn a_job_no_one_the_activation_waits_on_waits_for_is_left_be() {
    // `systemctl start unrelated.service` during the switch, queued
    // behind the same run: new, but not the activation's.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set(
        "jobs-later",
        &format!(
            "{},{},{}",
            job(1, "renovate.service", "start", "running"),
            job(9, "renovate-sweep.service", "restart", "waiting"),
            job(12, "unrelated.service", "start", "waiting")
        ),
    );
    m.set("before-12", &job(1, "renovate.service", "start", "running"));
    let two = m.sys("two");
    assert_eq!(
        said(&m.switch(&["--toplevel", &two])),
        (Some(0), live(&two), LET_GO.into())
    );
    assert_eq!(
        m.fake("busctl"),
        "cancel 9\nRestartUnit renovate-sweep.service ignore-requirements\n"
    );
}

#[test]
fn a_job_let_go_is_kept_for_recovery_until_its_queued_again() {
    let m = Machine::new(false);
    behind_a_long_run(&m);
    let list = m.path("lists").join("activation-restart-list");
    fs::write(&list, "other.service\n").unwrap();
    let two = m.sys("two");
    assert_eq!(
        said(&m.switch(&["--toplevel", &two])),
        (Some(0), live(&two), LET_GO.into())
    );
    // In the list the next activation (the revert's too) reads back, as it
    // was cancelled: a switch stopped there leaves it to be started again.
    assert_eq!(
        m.fake("recovery-at-cancel"),
        "other.service\nrenovate-sweep.service\n"
    );
    // Taken out once it's queued again; what else was there stays.
    assert_eq!(fs::read_to_string(&list).unwrap(), "other.service\n");
    // A list it can't write keeps the job waited for.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    fs::create_dir(m.path("lists").join("activation-restart-list")).unwrap();
    let two = m.sys("two");
    let (code, stdout, stderr) = said(&m.switch(&["--toplevel", &two]));
    assert_eq!((code, stdout), (Some(0), live(&two)));
    assert!(
        stderr.contains("renovate-sweep.service kept waiting: it could not be kept for recovery"),
        "{stderr}"
    );
    assert_eq!(m.fake("busctl"), "");
}
