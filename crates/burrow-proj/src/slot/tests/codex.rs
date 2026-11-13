use super::*;

// Run in a child of its own (test `child`): the slot is open across exec
// (slot.rs, `held`), so a child another test's thread starts while it's
// held keeps the lock after the drop, and the drop's release can be seen only
// where nothing else forks.
#[test]
fn codex_slots_do_not_take_claude_slots_or_wait_on_claude_usage() {
    let tmp = TempDir::new().unwrap();
    let out = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "slot::tests::child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "codex-pool-release")
        .env(JOBS_ENV, tmp.path())
        .env(MAX_JOBS_ENV, "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && said.contains("test result: ok. 1 passed"),
        "{said}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// What codex_slots_do_not_take_claude_slots_or_wait_on_claude_usage runs in
/// its child.
pub(super) fn release_codex_pool() {
    use crate::engine::Engine;
    let d = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
    cfg.jobs = d.path().to_owned();
    cfg.codex_slots = 1;
    let job = Job::new(&cfg, "code");
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Automated,
    )
    .unwrap();
    fs::write(
        cfg.jobs.join(crate::usage::FILE),
        serde_json::json!({"until": crate::now()+10000,
        "said":"limit", "job":"claude", "at":crate::now()})
        .to_string(),
    )
    .unwrap();
    // A planner's unstick flag cannot escape the selected engine's cap.
    fs::write(job.path("unstick"), "").unwrap();
    let slot =
        crate::slot::take_engine(&cfg, "code", "1", Class::Normal, false, Engine::Codex).unwrap();
    assert_eq!(
        crate::read(&job.turn("1", "slot")).as_deref(),
        Some("codex-1")
    );
    assert!(!cfg.jobs.join(".slots/1").exists());
    let other = File::create(cfg.jobs.join(".codex-slots/1")).unwrap();
    assert!(other.try_lock().is_err());
    drop(slot);
    assert!(other.try_lock().is_ok());
}

// A Claude turn handed over to a proj that resolves its engine to Codex has
// no place in Claude's line any more: the one it left is gone before it waits
// for Codex's pool, where it would otherwise count as a live Claude waiter.
#[test]
fn a_codex_turn_leaves_the_place_a_hand_over_brought() {
    use crate::engine::Engine;
    let d = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
    cfg.jobs = d.path().to_owned();
    cfg.codex_slots = 1;
    fs::create_dir_all(cfg.jobs.join(".slots")).unwrap();
    let census = File::create(cfg.jobs.join(".slots/.admission-lock")).unwrap();
    census.lock().unwrap();
    let job = Job::new(&cfg, "code");
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Automated,
    )
    .unwrap();
    let queue = cfg.jobs.join(".queue");
    fs::create_dir_all(&queue).unwrap();
    let mine = format!("{}\n", std::process::id());
    fs::write(queue.join("2-5000000000-code"), &mine).unwrap();
    // Another job's place, and the same job's under another pid, stay.
    fs::write(queue.join("2-6000000000-other"), &mine).unwrap();
    fs::write(queue.join("2-7000000000-code-2"), "1\n").unwrap();
    // A planner's unstick flag cannot escape the selected engine's cap.
    fs::write(job.path("unstick"), "").unwrap();
    let slot =
        crate::slot::take_engine(&cfg, "code", "1", Class::Normal, false, Engine::Codex).unwrap();
    assert!(!queue.join("2-5000000000-code").exists());
    assert!(queue.join("2-6000000000-other").exists());
    assert!(queue.join("2-7000000000-code-2").exists());
    drop(slot);
}
