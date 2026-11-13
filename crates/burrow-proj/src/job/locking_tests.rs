use super::*;
use std::os::fd::AsFd;

#[test]
fn releasing_a_job_lock_unlocks_descriptors_inherited_before_exec() {
    for mode in ["exclusive", "shared", "try"] {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().to_owned();
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        let held = match mode {
            "exclusive" => job.lock().unwrap(),
            "shared" => job.read_lock().unwrap(),
            "try" => job.try_lock().unwrap().unwrap(),
            _ => unreachable!(),
        };
        // A dup shares the open file description just as a descriptor in a
        // concurrently forked child does, until that child reaches exec.
        let inherited = held.as_fd().try_clone_to_owned().unwrap();
        assert!(
            job.try_lock().unwrap().is_none(),
            "{mode} must exclude writers"
        );
        drop(held);
        assert!(
            job.try_lock().unwrap().is_some(),
            "{mode} must release before the child reaches exec"
        );
        drop(inherited);
    }
}

#[test]
fn failed_multi_job_merges_release_even_inherited_lock_descriptors() {
    let (_dir, cfg) = crate::ask::about_tests::fixture();
    for id in ["a", "z"] {
        crate::ask::about_tests::asks(&cfg, id, None, false);
    }
    let first = Job::new(&cfg, "a");
    let busy = Job::new(&cfg, "z");
    let held = busy.lock().unwrap();
    assert!(crate::ask::merge(&cfg, "subject", &["a".into(), "z".into()]).is_err());
    assert!(first.try_lock().unwrap().is_some());
    assert!(busy.try_lock().unwrap().is_none());
    let inherited = held.as_fd().try_clone_to_owned().unwrap();
    drop(held);
    crate::ask::merge(&cfg, "subject", &["a".into(), "z".into()]).unwrap();
    drop(inherited);
}
