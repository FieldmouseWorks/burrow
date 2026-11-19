use super::*;
use crate::config::tests::example;

#[test]
fn invalid_roles_fail_with_an_outcome_before_taking_a_slot() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = dir.path().join("jobs");
    cfg.scratch = dir.path().join("scratch");
    crate::config::tests::set(&mut cfg, dir.path(), &[]);
    let overlay = cfg.settings.as_ref().unwrap().roles_file();
    let job = Job::new(&cfg, "job");
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("role"), "work").unwrap();
    fs::write(job.path("agent"), "sandbox").unwrap();
    // An overlay a hand edit broke: the door never writes one. An engine
    // the machine no longer runs is one of those.
    for (engine, expected) in [
        ("unknown", "unknown engine unknown"),
        ("codex", "unknown engine codex"),
    ] {
        let routing = serde_json::json!({"routing": {"roles": {"work": {"engine": engine}}}});
        fs::write(&overlay, routing.to_string()).unwrap();
        assert!(exec(&cfg, "job", "normal").is_err());
        assert!(read(&job.turn("1", "outcome")).unwrap().contains(expected));
        assert!(!job.turn("1", "slot").exists());
    }
}
