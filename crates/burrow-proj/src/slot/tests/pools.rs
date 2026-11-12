use super::*;

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
