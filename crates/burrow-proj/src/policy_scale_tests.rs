//! A policy pass on a machine with thousands of job dirs, most of them long
//! finished work of repositories it denies (FieldmouseWorks/burrow#786): it
//! must not ask systemd about each of them, or the pass outlasts its unit's
//! start timeout.
use crate::config::Config;
use crate::job::Job;
use std::fs;
use std::path::{Path, PathBuf};

const DENIED: usize = 3000;

fn calls(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A finished job of `repo`, its files written as its turn left them (plain
/// writes: thousands of synced ones would be most of the test).
fn job(cfg: &Config, id: &str, repo: &str, outcome: &str) {
    let job = Job::new(cfg, id);
    fs::create_dir_all(&job.dir).unwrap();
    let admission = crate::policy::path(&job, "admission");
    fs::create_dir_all(admission.parent().unwrap()).unwrap();
    fs::write(
        admission,
        serde_json::json!({
            "identity": {"kind": "repository", "repo": repo},
            "kind": "Automated",
            "agent": null,
        })
        .to_string(),
    )
    .unwrap();
    fs::write(job.path("turns"), "1\n").unwrap();
    fs::write(job.path("prompt.1"), "work").unwrap();
    fs::write(job.turn("1", "outcome"), outcome).unwrap();
}

/// A machine whose user manager has `up` (lines as `list-units` prints them),
/// all in `state`, which it also says of each unit asked after alone. Every
/// systemctl call goes in the log.
fn rig(up: &str, state: &str) -> (tempfile::TempDir, Config, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let mut v = crate::config::tests::example();
    v["jobs"] = serde_json::json!(tmp.path().join("jobs"));
    v["settings"]["overlay"] = serde_json::json!(tmp.path().join("overlay.json"));
    v["waits"] = serde_json::json!(tmp.path().join("waits.json"));
    let mut cfg: Config = serde_json::from_value(v).unwrap();
    let log = tmp.path().join("calls");
    cfg.tools.systemctl = tmp.path().join("systemctl");
    cfg.tools.systemd_run = tmp.path().join("systemd-run");
    burrow_test::executable(
        &cfg.tools.systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >>'{}'\ncase \"$*\" in\n*list-units*--state=*{state}*) printf '{up}' ;;\n*list-units*) ;;\n*is-active*) test {state} = active ;;\n*ActiveState*) echo {state} ;;\n*LoadState*) echo loaded ;;\nesac\n",
            log.display()
        ),
    );
    burrow_test::executable(&cfg.tools.systemd_run, "#!/bin/sh\nexit 1\n");
    (tmp, cfg, log)
}

#[test]
fn a_pass_asks_systemd_nothing_of_each_settled_denied_job() {
    let (_tmp, cfg, log) = rig("", "inactive");
    for n in 0..DENIED {
        job(&cfg, &format!("denied-{n:05}"), "sam/elsewhere", "done");
    }
    job(&cfg, "allowed", "sam/web", "failed");
    for _ in 0..2 {
        let _ = fs::remove_file(&log);
        crate::policy::reconcile(&cfg).unwrap();
        let named: Vec<String> = calls(&log)
            .into_iter()
            .filter(|c| c.contains("denied"))
            .collect();
        assert!(
            named.is_empty(),
            "{} systemctl calls named settled denied jobs, as {:?}",
            named.len(),
            named.first()
        );
        assert!(
            calls(&log).len() < 10,
            "a pass's systemd looks grow with job dirs: {:?}",
            calls(&log)
        );
    }
}

#[test]
fn a_denied_unit_still_stopping_is_vacated_and_parked() {
    // Deactivating can still hold descendants and inherited locks.
    let (_tmp, cfg, log) = rig(
        "proj-denied.service loaded deactivating stop-sigterm the turn\\n",
        "deactivating",
    );
    job(&cfg, "denied", "sam/elsewhere", "done");
    let job = Job::new(&cfg, "denied");
    assert_eq!(job.unit(), "proj-denied.service");
    // Its unit never goes in the stand-in, so the pass ends saying so.
    let _ = crate::policy::reconcile(&cfg);
    assert!(crate::policy::parked(&job), "{:?}", calls(&log));
    assert!(
        calls(&log)
            .iter()
            .any(|c| c.contains("--kill-whom=all") && c.contains("proj-denied.service")),
        "{:?}",
        calls(&log)
    );
}
