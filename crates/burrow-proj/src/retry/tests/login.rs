//! A turn whose run was refused for Claude Code's login waits in line with
//! its own prompt, and its next run is asked it again.

use super::*;

#[test]
fn a_turn_refused_for_its_login_runs_its_own_prompt_again() {
    let r = rig();
    let job = Job::new(&r.cfg, "login");
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Automated,
    )
    .unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("project"), "/original").unwrap();
    fs::write(job.path("engine"), "claude").unwrap();
    fs::write(job.path("prompt.1"), "the original prompt").unwrap();
    let stream = job.turn("1", "jsonl");
    fs::write(&stream, crate::engine::claude::tests::LOGIN).unwrap();
    // How turn.rs reads the run's end: refused for its login, so held.
    let last = engine::last_result(&stream, 0).unwrap();
    let refusal =
        engine::refused_file(engine::Engine::Claude, &last, &stream, 0, crate::now()).unwrap();
    assert!(refusal.login);
    usage::login(&r.cfg, job.id, &refusal.said, crate::now()).unwrap();
    assert!(usage::out(&r.cfg).is_some_and(|s| s.login));
    Checkpoint::retry(&job, "1", false, now_nanos(), &refusal.said).unwrap();
    // Once it's let go, the turn is asked what it never ran.
    let state = Checkpoint::load(&job, "1").unwrap().unwrap();
    let prompt = state.resume(&job, "1", &job.path("prompt.1")).unwrap();
    let asked = fs::read_to_string(prompt).unwrap();
    assert!(asked.starts_with(turn::SPENT), "{asked}");
    assert!(asked.contains("OAuth session expired"), "{asked}");
    assert!(asked.ends_with("the original prompt"), "{asked}");
}
