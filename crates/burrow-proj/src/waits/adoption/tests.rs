use super::*;
use crate::config::tests::example;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn fixture(tmp: &TempDir) -> Config {
    let jobs = tmp.path().join("jobs");
    fs::create_dir_all(&jobs).unwrap();
    let mut value = example();
    value["jobs"] = json!(jobs);
    value["hostState"] = json!(tmp.path().join("protected"));
    value["waits"] = json!(tmp.path().join("waits.json"));
    value["settings"]["overlay"] = json!(tmp.path().join("settings.json"));
    value["settings"]["allowed"] = json!(["sam/web", "sam/other"]);
    value["deployed"] = json!(tmp.path().join("deployed"));
    value["kick"] = json!("");
    let mut cfg: Config = serde_json::from_value(value).unwrap();
    cfg.tools.systemctl = tmp.path().join("systemctl");
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in\n*is-active*) exit 3 ;;\n*ActiveState*) echo inactive ;;\n*LoadState*) echo not-found ;;\nesac\n",
    );
    cfg
}

fn job(cfg: &Config, id: &str, repo: &str, end: Option<&str>) {
    let j = Job::new(cfg, id);
    fs::create_dir_all(&j.dir).unwrap();
    j.set_turns(1).unwrap();
    fs::write(j.path("prompt.1"), "work").unwrap();
    if let Some(end) = end {
        fs::write(j.turn("1", "outcome"), "done").unwrap();
        fs::write(j.turn("1", "end"), end).unwrap();
    }
    crate::policy::bind(
        &j,
        crate::policy::Identity::Repository(repo.into()),
        crate::policy::Kind::Automated,
    )
    .unwrap();
}

fn policy(cfg: &Config, text: &str) {
    fs::write(&cfg.settings.as_ref().unwrap().overlay, text).unwrap();
}

#[test]
fn all_adoption_state_reads_reject_aliases_special_files_and_oversized_records() {
    for name in ["turns", "1.outcome", "1.end"] {
        for kind in ["symlink", "fifo", "directory", "oversized", "parent-link"] {
            let tmp = TempDir::new().unwrap();
            let cfg = fixture(&tmp);
            policy(&cfg, r#"{"repos":{"sam/web":false}}"#);
            job(&cfg, "legacy", "sam/web", Some("undeclared"));
            let j = Job::new(&cfg, "legacy");
            let path = j.path(name);
            let original = fs::read(&path).unwrap();
            fs::remove_file(&path).unwrap();
            match kind {
                "symlink" => {
                    let outside = tmp.path().join("outside-state");
                    fs::write(&outside, &original).unwrap();
                    std::os::unix::fs::symlink(outside, &path).unwrap();
                }
                "fifo" => assert!(
                    Command::new("mkfifo")
                        .arg(&path)
                        .status()
                        .unwrap()
                        .success()
                ),
                "directory" => fs::create_dir(&path).unwrap(),
                "oversized" => File::create(&path)
                    .unwrap()
                    .set_len(64 * 1024 * 1024 + 1)
                    .unwrap(),
                "parent-link" => {
                    fs::write(&path, &original).unwrap();
                    let outside = tmp.path().join("outside");
                    fs::rename(&j.dir, &outside).unwrap();
                    std::os::unix::fs::symlink(outside, &j.dir).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                !adoptable(&cfg, &[cfg.settings.as_ref().unwrap().snapshot().ok()]).1,
                "{name}: {kind} must leave discovery incomplete"
            );
            if kind == "parent-link" {
                fs::remove_file(&j.dir).unwrap();
                fs::rename(tmp.path().join("outside"), &j.dir).unwrap();
            } else {
                if kind == "directory" {
                    fs::remove_dir(&path).unwrap();
                } else {
                    fs::remove_file(&path).unwrap();
                }
                fs::write(&path, original).unwrap();
            }
            let (found, complete) =
                adoptable(&cfg, &[cfg.settings.as_ref().unwrap().snapshot().ok()]);
            assert!(complete, "{name}: {kind} must recover");
            assert!(found.contains("legacy"));
        }
    }
}

#[test]
fn discovery_retries_keep_the_original_policy_and_scope_clock() {
    for change in ["allow", "pause", "back", "expiry"] {
        let tmp = TempDir::new().unwrap();
        let cfg = fixture(&tmp);
        // Web is denied in every initial snapshot, other is allowed.
        let initial = if matches!(change, "back" | "expiry") {
            format!(r#"{{"pipelineUntil":{}}}"#, now() + 60)
        } else {
            r#"{"repos":{"sam/web":false}}"#.into()
        };
        policy(&cfg, &initial);
        job(&cfg, "failed", "sam/web", None);
        job(&cfg, "undeclared", "sam/web", Some("undeclared"));
        job(&cfg, "settled", "sam/web", Some("done"));
        job(&cfg, "stopped", "sam/web", None);
        fs::write(cfg.jobs.join("stopped/1.stopped"), "").unwrap();
        job(&cfg, "owned", "sam/web", None);
        fs::write(cfg.jobs.join("owned/owned"), "").unwrap();
        if change == "pause" {
            job(&cfg, "previously-allowed", "sam/other", None);
        }
        // Freeze before discovering any jobs, then force a required read to
        // fail. A later retry must still find this unprocessed job.
        let held = cfg.jobs.join(HELD);
        fs::create_dir_all(&held).unwrap();
        let pending = held.join(PENDING);
        let mut migration = Adoption::load(&cfg, &pending).unwrap();
        if change == "expiry" {
            // The scope has now expired in wall time, but not at the saved
            // selection instant. Avoid a timing-dependent sleep in the test.
            let mut saved = serde_json::to_value(&migration).unwrap();
            saved["policies"][0]["state"]["pipelineUntil"] = json!(now() - 1);
            saved["policies"][0]["at"] = json!(now() - 2);
            migration = serde_json::from_value(saved).unwrap();
            migration.save(&pending).unwrap();
        }
        fs::remove_file(cfg.jobs.join("undeclared/1.end")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", cfg.jobs.join("undeclared/1.end")).unwrap();
        assert!(!rewatch_all(&cfg));
        assert!(!held.join(ADOPTED).exists());
        policy(
            &cfg,
            match change {
                "pause" => r#"{"repos":{"sam/web":true,"sam/other":false}}"#,
                "expiry" => r#"{"pipelineUntil":1}"#,
                _ => "{}",
            },
        );
        fs::remove_file(cfg.jobs.join("undeclared/1.end")).unwrap();
        fs::write(cfg.jobs.join("undeclared/1.end"), "undeclared").unwrap();
        assert!(rewatch_all(&cfg));
        for id in ["failed", "undeclared"] {
            assert!(cfg.jobs.join(".waits").join(id).exists(), "{change}: {id}");
        }
        for id in ["settled", "stopped", "owned", "previously-allowed"] {
            assert!(!cfg.jobs.join(".waits").join(id).exists(), "{change}: {id}");
            assert!(!held.join(id).exists(), "{change}: {id}");
        }
        assert!(held.join(ADOPTED).exists());
        assert!(!pending.exists());
    }
}

// Exercise actual command dispatch across separate configurations and chained
// retained binaries, rather than accepting a stub's successful no-op.
#[test]
fn deployed_child() {
    let Some(config) = std::env::var_os("BURROW_ADOPTION_CONFIG") else {
        return;
    };
    let mut cfg: Config = serde_json::from_slice(&fs::read(config).unwrap()).unwrap();
    cfg.me = fs::canonicalize(std::env::var_os("BURROW_ADOPTION_SELF").unwrap()).unwrap();
    let op = std::env::var("BURROW_ADOPTION_OP").unwrap();
    let args = if op == "_handover_format" {
        vec![op]
    } else {
        vec![op, "failed".into(), "undeclared".into()]
    };
    assert!(crate::cmd::dispatch(&cfg, &args).is_ok());
}

struct Kid(Child);
impl Drop for Kid {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn finished(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(Instant::now() < deadline, "deployed refresh deadlocked");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wrapper(tmp: &TempDir, cfg: &Config, name: &str, waits: bool) -> std::path::PathBuf {
    let script = tmp.path().join(name);
    let config = tmp.path().join(format!("{name}.json"));
    let mut value = example();
    value["jobs"] = json!(cfg.jobs);
    value["hostState"] = json!(cfg.host_state);
    value["waits"] = if waits { json!(cfg.waits) } else { Value::Null };
    value["settings"] = json!(cfg.settings);
    value["deployed"] = json!(cfg.deployed);
    value["tools"]["systemctl"] = json!(cfg.tools.systemctl);
    value["kick"] = json!("");
    fs::write(&config, serde_json::to_vec(&value).unwrap()).unwrap();
    // All paths are generated by TempDir, without shell metacharacters.
    burrow_test::executable(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = _handover_format ]; then echo 1; exit; fi\nexport BURROW_ADOPTION_CONFIG='{}' BURROW_ADOPTION_SELF='{}' BURROW_ADOPTION_OP=\"$1\"\nexec '{}' --exact waits::adoption::tests::deployed_child --nocapture\n",
            config.display(),
            script.display(),
            std::env::current_exe().unwrap().display()
        ),
    );
    script
}

#[test]
fn retained_callers_delegate_the_entire_migration_and_reset_across_chains() {
    for enabled in [false, true] {
        for chained in [false, true] {
            let tmp = TempDir::new().unwrap();
            let mut cfg = fixture(&tmp);
            policy(&cfg, r#"{"repos":{"sam/web":false}}"#);
            job(&cfg, "failed", "sam/web", None);
            job(&cfg, "undeclared", "sam/web", Some("undeclared"));
            let current = wrapper(&tmp, &cfg, "current", enabled);
            let old = wrapper(&tmp, &cfg, "old", !enabled);
            let middle = wrapper(&tmp, &cfg, "middle", !enabled);
            if chained {
                // old's saved deployment points to middle; middle points to
                // the live symlink, whose target is current.
                let path = tmp.path().join("old.json");
                let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                value["deployed"] = json!(middle);
                fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            std::os::unix::fs::symlink(&current, &cfg.deployed).unwrap();
            let held = cfg.jobs.join(HELD);
            fs::create_dir_all(&held).unwrap();
            if !enabled {
                fs::write(held.join(ADOPTED), "").unwrap();
            }
            let mut child = Kid(Command::new(&old)
                .arg("_rewatch_all")
                .stdin(Stdio::null())
                .spawn()
                .unwrap());
            finished(&mut child.0);
            assert_eq!(held.join(ADOPTED).exists(), enabled);
            if enabled {
                assert!(held.join("failed").exists());
                assert!(held.join("undeclared").exists());
            } else {
                // A later enabling owner discovers the disabled interval's
                // failed and undeclared waits too.
                let enabled_owner = wrapper(&tmp, &cfg, "enabled", true);
                fs::remove_file(&cfg.deployed).unwrap();
                std::os::unix::fs::symlink(&enabled_owner, &cfg.deployed).unwrap();
                cfg.me = enabled_owner;
                assert!(rewatch_all(&cfg));
                assert!(held.join("failed").exists());
                assert!(held.join("undeclared").exists());
            }
        }
    }
}

struct Gate(std::path::PathBuf);
impl Drop for Gate {
    fn drop(&mut self) {
        fs::write(&self.0, "").unwrap();
    }
}

fn block_systemctl(tmp: &TempDir, cfg: &Config, operation: &str) -> Gate {
    let reached = tmp.path().join("reached");
    let release = tmp.path().join("release");
    burrow_test::executable(
        &cfg.tools.systemctl,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n*{operation}*) touch '{}'; while [ ! -e '{}' ]; do sleep 0.01; done ;;\nesac\ncase \"$*\" in\n*is-active*) exit 3 ;;\n*ActiveState*) echo inactive ;;\n*LoadState*) echo not-found ;;\nesac\n",
            reached.display(),
            release.display()
        ),
    );
    Gate(release)
}

fn reached(tmp: &TempDir) {
    let until = Instant::now() + Duration::from_secs(15);
    while !tmp.path().join("reached").exists() {
        assert!(Instant::now() < until, "scan never reached systemctl");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_policy_change_mid_scan_uses_saved_selection_and_current_publication() {
    for change in ["allow", "pause", "back"] {
        let tmp = TempDir::new().unwrap();
        let cfg = fixture(&tmp);
        let initial = if change == "back" {
            format!(r#"{{"pipelineUntil":{}}}"#, now() + 60)
        } else {
            r#"{"repos":{"sam/web":false}}"#.to_owned()
        };
        policy(&cfg, &initial);
        job(&cfg, "failed", "sam/web", None);
        job(&cfg, "undeclared", "sam/web", Some("undeclared"));
        if change == "pause" {
            job(&cfg, "allowed-before-scan", "sam/other", None);
        }
        let gate = block_systemctl(&tmp, &cfg, "is-active");
        let scan_cfg = cfg.clone();
        let scan = std::thread::spawn(move || rewatch_all(&scan_cfg));
        reached(&tmp);
        policy(
            &cfg,
            if change == "pause" {
                r#"{"repos":{"sam/web":true,"sam/other":false}}"#
            } else {
                "{}"
            },
        );
        // A candidate that settles during discovery must not be resurrected.
        fs::write(cfg.jobs.join("failed/1.outcome"), "done").unwrap();
        fs::write(cfg.jobs.join("failed/1.end"), "done").unwrap();
        drop(gate);
        assert!(scan.join().unwrap());
        assert!(cfg.jobs.join(".waits/undeclared").exists());
        for id in ["failed", "allowed-before-scan"] {
            assert!(!cfg.jobs.join(".waits").join(id).exists());
            assert!(!cfg.jobs.join(HELD).join(id).exists());
        }
    }
}

#[test]
fn switching_watchdog_configuration_mid_pass_leaves_completion_to_the_new_owner() {
    for enabled in [false, true] {
        let tmp = TempDir::new().unwrap();
        let mut cfg = fixture(&tmp);
        policy(&cfg, r#"{"repos":{"sam/web":false}}"#);
        job(&cfg, "failed", "sam/web", None);
        job(&cfg, "undeclared", "sam/web", Some("undeclared"));
        let current = wrapper(&tmp, &cfg, "current", enabled);
        let old = wrapper(&tmp, &cfg, "old", !enabled);
        std::os::unix::fs::symlink(&old, &cfg.deployed).unwrap();
        cfg.me = old;
        if enabled {
            cfg.waits = None;
            fs::create_dir_all(cfg.jobs.join(HELD)).unwrap();
            fs::write(cfg.jobs.join(HELD).join(ADOPTED), "").unwrap();
        }
        let gate = block_systemctl(&tmp, &cfg, if enabled { "list-units" } else { "is-active" });
        let scan_cfg = cfg.clone();
        let scan = std::thread::spawn(move || rewatch_all(&scan_cfg));
        reached(&tmp);
        fs::remove_file(&cfg.deployed).unwrap();
        std::os::unix::fs::symlink(current, &cfg.deployed).unwrap();
        drop(gate);
        assert!(scan.join().unwrap());
        assert_eq!(cfg.jobs.join(HELD).join(ADOPTED).exists(), enabled);
        if enabled {
            assert!(cfg.jobs.join(HELD).join("failed").exists());
            assert!(cfg.jobs.join(HELD).join("undeclared").exists());
        }
    }
}

#[test]
fn concurrent_passes_share_the_completion_boundary() {
    let tmp = TempDir::new().unwrap();
    let cfg = fixture(&tmp);
    job(&cfg, "historical-failure", "sam/web", None);
    // Selection sees an allowed historical failure. Block after selection so
    // another caller can enter before the first commits completion.
    let gate = block_systemctl(&tmp, &cfg, "list-units");
    let first_cfg = cfg.clone();
    let first = std::thread::spawn(move || rewatch_all(&first_cfg));
    reached(&tmp);
    let second_cfg = cfg.clone();
    let second = std::thread::spawn(move || rewatch_all(&second_cfg));
    policy(&cfg, r#"{"repos":{"sam/web":false}}"#);
    drop(gate);
    assert!(first.join().unwrap());
    assert!(second.join().unwrap());
    assert!(!cfg.jobs.join(HELD).join("historical-failure").exists());
    assert!(!cfg.jobs.join(".waits/historical-failure").exists());
}

#[test]
fn a_failed_successor_never_authorizes_a_stale_local_reset_or_completion() {
    for enabled in [false, true] {
        let tmp = TempDir::new().unwrap();
        let mut cfg = fixture(&tmp);
        let successor = tmp.path().join("successor");
        burrow_test::executable(
            &successor,
            "#!/bin/sh\ncase $1 in\n_handover_format) echo 1 ;;\n*) exit 1 ;;\nesac\n",
        );
        std::os::unix::fs::symlink(successor, &cfg.deployed).unwrap();
        fs::create_dir_all(cfg.jobs.join(HELD)).unwrap();
        if !enabled {
            cfg.waits = None;
            fs::write(cfg.jobs.join(HELD).join(ADOPTED), "").unwrap();
        }
        assert!(!rewatch_all(&cfg));
        assert!(!super::super::rewatch(&cfg, &[]));
        assert_eq!(cfg.jobs.join(HELD).join(ADOPTED).exists(), !enabled);
        assert!(!cfg.jobs.join(HELD).join(PENDING).exists());
    }
}

#[test]
fn failed_outcomes_do_not_hide_rejected_end_records() {
    let tmp = TempDir::new().unwrap();
    let cfg = fixture(&tmp);
    policy(&cfg, r#"{"repos":{"sam/web":false}}"#);
    job(&cfg, "failed", "sam/web", None);
    let outside = tmp.path().join("outside");
    fs::write(&outside, "done").unwrap();
    std::os::unix::fs::symlink(outside, cfg.jobs.join("failed/1.end")).unwrap();
    assert!(!rewatch_all(&cfg));
    assert!(!cfg.jobs.join(HELD).join(ADOPTED).exists());
    fs::remove_file(cfg.jobs.join("failed/1.end")).unwrap();
    assert!(rewatch_all(&cfg));
    assert!(cfg.jobs.join(HELD).join("failed").exists());
}

#[test]
fn retained_single_job_publications_use_the_deployed_watchdog_setting() {
    for enabled in [false, true] {
        let tmp = TempDir::new().unwrap();
        let cfg = fixture(&tmp);
        policy(&cfg, r#"{"repos":{"sam/web":false}}"#);
        job(&cfg, "failed", "sam/web", None);
        job(&cfg, "undeclared", "sam/web", Some("undeclared"));
        let current = wrapper(&tmp, &cfg, "current", enabled);
        let old = wrapper(&tmp, &cfg, "old", !enabled);
        std::os::unix::fs::symlink(current, &cfg.deployed).unwrap();
        fs::create_dir_all(cfg.jobs.join(HELD)).unwrap();
        fs::write(cfg.jobs.join(HELD).join(ADOPTED), "").unwrap();
        let mut child = Kid(Command::new(old).arg("_rewatch").spawn().unwrap());
        finished(&mut child.0);
        assert_eq!(cfg.jobs.join(HELD).join(ADOPTED).exists(), enabled);
        assert_eq!(cfg.jobs.join(HELD).join("failed").exists(), enabled);
        assert_eq!(cfg.jobs.join(HELD).join("undeclared").exists(), enabled);
    }
}

#[test]
fn a_disabled_caller_waiting_for_the_lock_cannot_clear_a_new_owners_marker() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = fixture(&tmp);
    let current = wrapper(&tmp, &cfg, "current", true);
    let old = wrapper(&tmp, &cfg, "old", false);
    std::os::unix::fs::symlink(&old, &cfg.deployed).unwrap();
    cfg.me = old;
    cfg.waits = None;
    fs::create_dir_all(cfg.jobs.join(HELD)).unwrap();
    fs::write(cfg.jobs.join(HELD).join(ADOPTED), "").unwrap();
    let guard = lock(&cfg).unwrap();
    let old_cfg = cfg.clone();
    let caller = std::thread::spawn(move || super::super::rewatch(&old_cfg, &[]));
    fs::remove_file(&cfg.deployed).unwrap();
    std::os::unix::fs::symlink(current, &cfg.deployed).unwrap();
    drop(guard);
    assert!(caller.join().unwrap());
    assert!(cfg.jobs.join(HELD).join(ADOPTED).exists());
}

#[test]
fn a_disabled_interval_during_incomplete_adoption_preserves_both_intervals() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = fixture(&tmp);
    policy(&cfg, r#"{"repos":{"sam/web":false}}"#);
    job(&cfg, "old-interval", "sam/web", Some("undeclared"));
    job(&cfg, "new-interval", "sam/other", None);
    fs::remove_file(cfg.jobs.join("old-interval/1.end")).unwrap();
    std::os::unix::fs::symlink("/dev/zero", cfg.jobs.join("old-interval/1.end")).unwrap();
    assert!(!rewatch_all(&cfg));
    let waits = cfg.waits.take();
    assert!(super::super::rewatch(&cfg, &[]));
    policy(&cfg, r#"{"repos":{"sam/web":true,"sam/other":false}}"#);
    cfg.waits = waits;
    fs::remove_file(cfg.jobs.join("old-interval/1.end")).unwrap();
    fs::write(cfg.jobs.join("old-interval/1.end"), "undeclared").unwrap();
    assert!(rewatch_all(&cfg));
    assert!(cfg.jobs.join(".waits/old-interval").exists());
    assert!(cfg.jobs.join(HELD).join("new-interval").exists());
}

#[test]
fn completed_pending_cleanup_is_not_reused_for_a_later_disabled_interval() {
    let tmp = TempDir::new().unwrap();
    let mut cfg = fixture(&tmp);
    job(&cfg, "new-interval", "sam/other", None);
    assert!(rewatch_all(&cfg));
    // Model a crash after completion was committed but before its pending
    // file was removed. Its old allowed policy must not mask the new pause.
    let pending = cfg.jobs.join(HELD).join(PENDING);
    Adoption::load(&cfg, &pending).unwrap();
    let waits = cfg.waits.take();
    assert!(rewatch_all(&cfg));
    assert!(!pending.exists());
    policy(&cfg, r#"{"repos":{"sam/other":false}}"#);
    cfg.waits = waits;
    assert!(rewatch_all(&cfg));
    assert!(cfg.jobs.join(HELD).join("new-interval").exists());
}
