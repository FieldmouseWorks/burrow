use std::os::unix::fs::PermissionsExt;

struct BlockWrites(std::path::PathBuf, std::fs::Permissions);
impl BlockWrites {
    fn new(path: &std::path::Path) -> Self {
        // Keep locking available so the failure is in receipt publication.
        crate::state_reads::append(&path.join("lock")).unwrap();
        let before = fs::metadata(path).unwrap().permissions();
        burrow_test::permissions(path, before.mode() & !0o222);
        Self(path.to_owned(), before)
    }
}
impl Drop for BlockWrites {
    fn drop(&mut self) {
        burrow_test::permissions(&self.0, self.1.mode());
    }
}

use super::*;

#[test]
fn watcher_recovers_before_expiry_and_owner_wait_at_every_cleanup_prefix() {
    for one_way in [false, true] {
        for temporary_failure in [false, true] {
            for prefix in 0..=METADATA.len() + 2 {
                let mut f = Fixture::new();
                f.cfg.wake = f.dir.path().join("wake.json");
                f.cfg.tools.systemctl = "false".into();
                f.cfg.tools.systemd_run = "true".into();
                f.cfg.waits = None;
                f.cfg.kick.clear();
                f.cfg.inbox = None;
                f.ending(1, "ask");
                notify(&f.cfg, &f.job()).unwrap();
                crate::ask::ensure(&f.job()).unwrap();
                for name in METADATA {
                    fs::write(crate::ask::host_path(&f.job(), name), "old question").unwrap();
                }
                fs::write(
                    crate::ask::host_path(&f.job(), "question"),
                    serde_json::json!({
                        "recommendation": {"default":"old default", "one_way":one_way},
                        "due":crate::now()-1,
                    })
                    .to_string(),
                )
                .unwrap();
                if !one_way {
                    fs::remove_file(crate::ask::host_path(&f.job(), "question-needs-owner"))
                        .unwrap();
                }
                std::mem::forget(Resume::prepare(&f.job(), true).unwrap());
                f.job().set_turns(2).unwrap();
                save_receipt(
                    &receipt_path(&f.job(), "2"),
                    &Receipt {
                        through: 1,
                        answered: true,
                        clear: true,
                        accepted: true,
                        applied: false,
                    },
                )
                .unwrap();
                if prefix > 0 {
                    fs::write(f.job().path("question-answered-through"), "1").unwrap();
                }
                for name in METADATA.iter().take(prefix.saturating_sub(1)) {
                    let path = crate::ask::host_path(&f.job(), name);
                    if path.exists() {
                        fs::remove_file(path).unwrap();
                    }
                }
                if prefix > METADATA.len() + 1 {
                    fs::remove_file(f.job().path("open-question")).unwrap();
                }
                if temporary_failure {
                    // Fail publication without removing an already committed
                    // answer boundary or depending on an implementation's temp name.
                    let blocked = BlockWrites::new(&f.job().dir);
                    crate::watch::look(&f.cfg, "").unwrap();
                    assert_eq!(f.job().turns().as_deref(), Some("2"));
                    assert!(!f.job().path("prompt.3").exists());
                    assert!(!f.job().turn("1", "question-closed").exists());
                    let wake = fs::read_to_string(&f.cfg.wake).unwrap();
                    assert!(
                        wake.contains("\"\""),
                        "recovery failure books a retry: {wake}"
                    );
                    drop(blocked);
                }
                crate::watch::look(&f.cfg, "").unwrap();
                assert_eq!(f.job().turns().as_deref(), Some("2"));
                assert!(
                    !f.job().path("prompt.3").exists(),
                    "old default must not start"
                );
                assert!(!crate::ask::host_path(&f.job(), "question-needs-owner").exists());
                assert!(
                    f.job().turn("1", "question-closed").exists(),
                    "{one_way} {temporary_failure} {prefix}"
                );
                for name in METADATA {
                    assert!(!crate::ask::host_path(&f.job(), name).exists());
                }
            }
        }
    }
}

#[test]
fn accepted_launchers_keep_identity_on_receipt_and_cleanup_errors() {
    for source in ["operator", "issue-author", "timeout", "initial"] {
        for fault in ["receipt", "cleanup"] {
            let mut f = Fixture::new();
            f.cfg.tools.systemctl = "false".into();
            f.cfg.waits = None;
            f.cfg.inbox = None;
            f.cfg.wake = f.dir.path().join("wake.json");
            let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = calls.clone();
            let (port, _) = burrow_events::http::listen(
                0,
                std::sync::Arc::new(move |req| {
                    seen.lock().unwrap().push(req.target.clone());
                    202
                }),
            )
            .unwrap();
            f.cfg.kick = format!("http://127.0.0.1:{port}/kick");
            f.cfg.projects = f.dir.path().join("projects");
            fs::create_dir_all(f.cfg.projects.join("p")).unwrap();
            f.ending(1, "ask");
            fs::write(f.job().path("session"), "s1").unwrap();
            crate::ask::ensure(&f.job()).unwrap();
            fs::write(
                crate::ask::host_path(&f.job(), "question"),
                serde_json::json!({
                    "recommendation": {"default":"old default", "one_way":false},
                    "due":crate::now()-1,
                })
                .to_string(),
            )
            .unwrap();
            let runner = f.dir.path().join("runner");
            burrow_test::executable(
                &runner,
                format!(
                    r#"#!/bin/sh
for dir in '{jobs}'/*; do
  test -d "$dir" || continue
  id=$(basename "$dir")
  n=$(cat "$dir/turns")
  host='{host}'/"$id"
  if test '{fault}' = receipt; then mkdir -p "$host/resume-$n.new";
  else mkdir -p "$host/question-text"; fi
done
exit 0
"#,
                    jobs = f.cfg.jobs.display(),
                    host = crate::ask::control_root(&f.cfg).display()
                ),
            );
            f.cfg.tools.systemd_run = runner;
            let id = match source {
                "operator" => {
                    crate::cmd::dispatch(&f.cfg, &["reply".into(), "j1".into(), "answer".into()])
                        .unwrap();
                    "j1".to_owned()
                }
                "issue-author" => {
                    crate::cmd::reply_answer(&f.cfg, "j1", "answer", false).unwrap();
                    "j1".to_owned()
                }
                "timeout" => {
                    let _held = f.job().lock().unwrap();
                    crate::ask::expire(&f.cfg, &f.job()).unwrap();
                    "j1".to_owned()
                }
                "initial" => crate::cmd::start_job(
                    &f.cfg,
                    &crate::cmd::New {
                        local_work: true,
                        ..Default::default()
                    },
                    "p",
                    "task",
                )
                .unwrap(),
                _ => unreachable!(),
            };
            assert!(
                calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|p| p == "/kick/proj-watch"),
                "{source} {fault}: accepted reconciliation errors retain their recovery kick"
            );
            let job = Job::new(&f.cfg, &id);
            let n = job.turns().unwrap();
            assert_eq!(n, if source == "initial" { "1" } else { "2" });
            assert!(job.path(&format!("prompt.{n}")).exists());
            let blocked = if fault == "receipt" {
                PathBuf::from(format!("{}.new", receipt_path(&job, &n).display()))
            } else {
                crate::ask::host_path(&job, "question-text")
            };
            fs::remove_dir(blocked).unwrap();
            if source != "initial" {
                // A watcher can run before _exec records acceptance after a
                // receipt-write error. It must not execute the old default.
                crate::watch::look(&f.cfg, "").unwrap();
                assert_eq!(job.turns().as_deref(), Some(n.as_str()));
                assert!(!job.path("prompt.3").exists());
            }
            // The accepted unit's _exec entry reconciles even if the launcher
            // could not record its acceptance, and retains the same turn.
            accept(&job, &n).unwrap();
            recover(&job).unwrap();
            assert_eq!(job.turns().as_deref(), Some(n.as_str()));
            assert!(!crate::ask::host_path(&job, "question").exists());
        }
    }
}
