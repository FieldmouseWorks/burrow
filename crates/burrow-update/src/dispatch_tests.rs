//! The fix jobs, with the stand-ins for runuser and proj (testing_tests.rs): pipeline
//! work, one job for a problem and its inputs while it's at work, and none
//! again while a run handed it on recently.

use std::fs::File;
use std::time::{Duration, SystemTime};

use super::*;
use crate::testing::Machine;

impl Machine {
    fn job(&self, key: &str) -> Option<String> {
        self.recorded(&format!("jobs/{key}"))
    }

    /// Its job, at work under `key`, ends.
    fn job_ends(&self, key: &str) {
        fs::remove_file(self.home(&format!("keys/{}", key.replace('/', "+")))).unwrap();
    }

    fn job_made(&self, key: &str, when: SystemTime) {
        File::options()
            .write(true)
            .open(self.cfg.state.join("jobs").join(key))
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
}

#[test]
fn a_problem_gets_one_pipeline_job_while_its_at_work() {
    // A fix to what deploys everything goes ahead of everyone's jobs, in
    // the slot kept for it, never behind a project's long ones, under the
    // problem's key.
    let mut m = Machine::new();
    assert_eq!(
        dispatch(&m.cfg, "build-main", &[], "main doesn't build"),
        Some("homelab-1".into())
    );
    assert_eq!(
        m.kept("proj.log"),
        "run --pipeline --role update --key box-update/build-main --join homelab main doesn't build\n"
    );
    assert_eq!(m.job("build-main"), Some("homelab-1".into()));

    // The same problem again while a run handed it on recently: no job.
    assert_eq!(
        dispatch(&m.cfg, "build-main", &[], "main doesn't build"),
        None
    );
    assert_eq!(m.kept("proj.log").lines().count(), 1);

    // 6 hours on, its job still waiting for a slot (a usage hold): the run
    // joins it, never a second job beside it (#1211).
    m.job_made(
        "build-main",
        SystemTime::now() - Duration::from_secs(7 * 3600),
    );
    assert_eq!(
        dispatch(&m.cfg, "build-main", &[], "main doesn't build"),
        Some("homelab-1".into())
    );
    assert_eq!(m.job("build-main"), Some("homelab-1".into()));

    // Once its job has ended, and 6 hours on, the problem gets another.
    m.job_ends("box-update/build-main");
    m.job_made(
        "build-main",
        SystemTime::now() - Duration::from_secs(7 * 3600),
    );
    assert_eq!(
        dispatch(&m.cfg, "build-main", &[], "main doesn't build"),
        Some("homelab-3".into())
    );
    assert_eq!(m.job("build-main"), Some("homelab-3".into()));

    // With a machine App, the job acts as it.
    m.cfg.app = Some("workers".into());
    dispatch(&m.cfg, "switch", &[], "the switch failed");
    assert_eq!(
        m.kept("proj.log").lines().last(),
        Some(
            "run --pipeline --role update --key box-update/switch --join --app workers homelab the switch failed"
        )
    );
}

#[test]
fn inputs_that_dont_build_are_one_job_a_set() {
    let m = Machine::new();
    let of = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // Sorted, so the same set is the same key in whatever order a run
    // tried them.
    assert_eq!(
        job_key(&m.cfg, "build-inputs", &of(&["nixpkgs-unstable", "burrow"])),
        "box-update/build-inputs/burrow:nixpkgs-unstable"
    );
    assert_eq!(
        job_key(&m.cfg, "build-inputs", &of(&["burrow", "nixpkgs-unstable"])),
        "box-update/build-inputs/burrow:nixpkgs-unstable"
    );
    // Too many for a key, or a name a key can't take: their hash, the same
    // every run.
    let many: Vec<String> = (0..40).map(|i| format!("input-number-{i}")).collect();
    let k = job_key(&m.cfg, "build-inputs", &many);
    assert!(
        k.len() <= 200 && k.starts_with("box-update/build-inputs/"),
        "{k}"
    );
    assert_eq!(k, job_key(&m.cfg, "build-inputs", &many));
    assert_ne!(k, job_key(&m.cfg, "build-inputs", &many[1..]));
    let odd = job_key(&m.cfg, "build-inputs", &of(&["a b"]));
    assert_eq!(odd.len(), "box-update/build-inputs/".len() + 16, "{odd}");
}

#[test]
fn a_job_proj_doesnt_take_is_no_job() {
    let m = Machine::new();
    fs::write(m.home("proj-down"), "").unwrap();
    assert_eq!(dispatch(&m.cfg, "health", &[], "it broke"), None);
    assert_eq!(m.job("health"), None);
    // So the next run hands it over.
    fs::remove_file(m.home("proj-down")).unwrap();
    assert_eq!(
        dispatch(&m.cfg, "health", &[], "it broke"),
        Some("homelab-2".into())
    );

    // An empty record is no job; one from a clock set back is recent.
    fs::write(m.cfg.state.join("jobs/activation"), "").unwrap();
    assert!(dispatch(&m.cfg, "activation", &[], "it failed").is_some());
    m.job_made("activation", SystemTime::now() + Duration::from_secs(3600));
    assert_eq!(dispatch(&m.cfg, "activation", &[], "it failed"), None);
}
