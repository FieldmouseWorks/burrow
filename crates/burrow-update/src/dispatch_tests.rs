//! The fix jobs, with the stand-ins for runuser and proj (testing.rs): pipeline
//! work, and one job for a problem while it's recent.

use std::fs::File;
use std::time::{Duration, SystemTime};

use super::*;
use crate::testing::Machine;

impl Machine {
    fn job(&self, key: &str) -> Option<String> {
        self.recorded(&format!("jobs/{key}"))
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
fn a_problem_gets_one_pipeline_job_while_its_recent() {
    // A fix to what deploys everything goes ahead of everyone's jobs, in
    // the slot kept for it, never behind a project's long ones.
    let mut m = Machine::new();
    assert_eq!(
        dispatch(&m.cfg, "build-main", "main doesn't build"),
        Some("homelab-1".into())
    );
    assert_eq!(
        m.kept("proj.log"),
        "run --pipeline homelab main doesn't build\n"
    );
    assert_eq!(m.job("build-main"), Some("homelab-1".into()));

    // The same problem again while its job is recent: no second job.
    assert_eq!(dispatch(&m.cfg, "build-main", "main doesn't build"), None);
    assert_eq!(m.kept("proj.log").lines().count(), 1);

    // Once it's 6 hours old, the problem gets a job again.
    m.job_made(
        "build-main",
        SystemTime::now() - Duration::from_secs(7 * 3600),
    );
    assert_eq!(
        dispatch(&m.cfg, "build-main", "main doesn't build"),
        Some("homelab-2".into())
    );
    assert_eq!(m.job("build-main"), Some("homelab-2".into()));

    // With a machine App, the job acts as it.
    m.cfg.app = Some("workers".into());
    dispatch(&m.cfg, "switch", "the switch failed");
    assert_eq!(
        m.kept("proj.log").lines().last(),
        Some("run --pipeline --app workers homelab the switch failed")
    );
}

#[test]
fn a_job_proj_doesnt_take_is_no_job() {
    let m = Machine::new();
    fs::write(m.home("proj-down"), "").unwrap();
    assert_eq!(dispatch(&m.cfg, "health", "it broke"), None);
    assert_eq!(m.job("health"), None);
    // So the next run hands it over.
    fs::remove_file(m.home("proj-down")).unwrap();
    assert_eq!(
        dispatch(&m.cfg, "health", "it broke"),
        Some("homelab-2".into())
    );

    // An empty record is no job; one from a clock set back is recent.
    fs::write(m.cfg.state.join("jobs/activation"), "").unwrap();
    assert!(dispatch(&m.cfg, "activation", "it failed").is_some());
    m.job_made("activation", SystemTime::now() + Duration::from_secs(3600));
    assert_eq!(dispatch(&m.cfg, "activation", "it failed"), None);
}
