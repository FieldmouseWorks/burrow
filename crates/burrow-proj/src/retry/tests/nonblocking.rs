//! Parents running 3bdabc2's scheduler (fixtures/nonblocking), which retries
//! a refused turn through `take(false)`: it polls the census with try_lock
//! and never blocks on it, so the barrier leaves no `->` entry to adopt it by.
use super::*;

/// One pass that transfers every attested parent. The barrier holds the
/// census for good, so a nonblocking retry that is never adopted stays busy.
fn transfer(r: &Rig) {
    let began = Instant::now();
    loop {
        match pass(&r.cfg) {
            Ok(()) => return,
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    && began.elapsed() < Duration::from_secs(10) =>
            {
                sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("the nonblocking retry was never transferred: {e}"),
        }
    }
}

fn spent(r: &Rig) {
    usage::record(
        &r.cfg,
        "limit",
        Some(crate::now() + 3600),
        "refused",
        crate::now(),
    )
    .unwrap();
}

fn barrier(r: &Rig) -> File {
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    barrier
}

fn queued(r: &Rig) -> Vec<String> {
    fs::read_dir(r.cfg.jobs.join(".queue"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect()
}

#[test]
fn nonblocking_retry_refused_after_the_deploy_is_transferred_with_its_turn() {
    let mut r = rig();
    start(&mut r, "running", "nb-running", "/ordinary");
    until(|| r.cfg.jobs.join("running/running").exists());
    let _barrier = barrier(&r);
    // Running work holds its slot: left alone, and nothing is busy.
    pass(&r.cfg).unwrap();
    let job = Job::new(&r.cfg, "running");
    assert!(!job.turn("1", "retry-migration").exists());
    let old = read(&job.path("pid")).unwrap();
    let stream = fs::read(job.turn("1", "jsonl")).unwrap();
    let deadline: i64 = read(&job.turn("1", "deadline")).unwrap().parse().unwrap();
    fs::write(job.path("refuse"), "").unwrap();
    until(|| !job.turn("1", "slot").exists());
    usage::back(&r.cfg, None);
    transfer(&r);
    until(|| job.path("took").exists());
    assert_ne!(read(&job.path("pid")).unwrap(), old, "the new proj took it");
    assert_eq!(fs::read(job.turn("1", "jsonl")).unwrap(), stream);
    assert_eq!(read(&job.path("session")).unwrap(), "conversation-123");
    assert_eq!(read(&job.path("turns")).unwrap(), "1");
    assert!(
        read(&job.turn("1", "spent"))
            .unwrap()
            .contains("the original prompt")
    );
    let moved: i64 = read(&job.turn("1", "deadline")).unwrap().parse().unwrap();
    assert!(moved >= deadline, "waiting spent none of its budget");
    assert!(!job.turn("1", "retry-migration").exists());
}

#[test]
fn nonblocking_retries_waiting_before_the_deploy_keep_their_places_and_the_reset_cap() {
    let mut r = rig();
    spent(&r);
    for i in 0..3 {
        start(&mut r, &format!("w{i}"), "nb", "/ordinary");
    }
    // Published under the old census, before the barrier took it.
    until(|| queued(&r).len() == 3);
    let places = queued(&r);
    let _barrier = barrier(&r);
    usage::back(&r.cfg, None);
    transfer(&r);
    until(|| took(&r.cfg) == 2);
    sleep(Duration::from_secs(3));
    assert_eq!(took(&r.cfg), 2, "the reset's trickle holds the third");
    let left = (0..3)
        .map(|i| format!("w{i}"))
        .find(|id| !r.cfg.jobs.join(id).join("took").exists())
        .unwrap();
    let left = Job::new(&r.cfg, &left);
    let state = Checkpoint::load(&left, "1").unwrap().unwrap();
    assert!(
        places.iter().any(|p| p == state.queue().0),
        "{} kept its place from {places:?}",
        state.queue().0
    );
}

#[test]
fn nonblocking_emergency_and_codex_turns_keep_their_own_pools() {
    let mut r = rig();
    spent(&r);
    for (id, mode) in [
        ("c0", "nb-codex"),
        ("emergency", "nb-unstick"),
        ("w0", "nb"),
        ("w1", "nb"),
        ("w2", "nb"),
    ] {
        start(&mut r, id, mode, "/ordinary");
    }
    until(|| r.cfg.jobs.join("c0/took").exists());
    until(|| queued(&r).len() == 3);
    let _barrier = barrier(&r);
    usage::back(&r.cfg, None);
    transfer(&r);
    until(|| took(&r.cfg) == 4);
    sleep(Duration::from_secs(3));
    assert_eq!(took(&r.cfg), 4, "two ordinary, one Codex, one emergency");
    for id in ["c0", "emergency"] {
        assert!(r.cfg.jobs.join(id).join("took").exists());
        assert!(
            !Job::new(&r.cfg, id).turn("1", "retry-migration").exists(),
            "{id} stayed in its own pool"
        );
    }
}

#[test]
fn a_handover_descriptor_and_forged_job_files_do_not_attest_a_wait() {
    let mut r = rig();
    // Outside any wait for a slot, holding a handover's inherited fd 9 on
    // the census, with what a sandbox could write: a refusal in its stream
    // (start's), a withdrawable turn and a stale slot cleared.
    start(&mut r, "idle", "nb-inherited", "/ordinary");
    let job = Job::new(&r.cfg, "idle");
    until(|| job.path("inherited").exists());
    let _barrier = barrier(&r);
    fs::write(job.turn("1", "withdrawable"), "").unwrap();
    let pid = read(&job.path("pid")).unwrap();
    let err = pass(&r.cfg).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    assert!(!job.turn("1", "retry-migration").exists());
    assert_eq!(read(&job.path("pid")).unwrap(), pid, "not stopped");
    assert!(Path::new("/proc").join(&pid).exists());
    // Its own wait, once it is in it, is what the kernel attests.
    fs::write(job.path("publish"), "").unwrap();
    transfer(&r);
    until(|| job.path("took").exists());
    assert_ne!(read(&job.path("pid")).unwrap(), pid);
}
#[test]
fn close_on_exec_is_read_from_fdinfo_flags() {
    assert!(authority::close_on_exec(
        "pos:\t0\nflags:\t02100001\nmnt_id:\t30\n"
    ));
    assert!(!authority::close_on_exec(
        "pos:\t0\nflags:\t0100001\nmnt_id:\t30\n"
    ));
    assert!(!authority::close_on_exec("pos:\t0\n"));
}
