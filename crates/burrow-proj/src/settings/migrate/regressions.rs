use super::*;
use std::os::fd::AsFd;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

fn rig(dir: &Path, predecessor: &str, applied: bool) -> (burrow_settings::Config, PathBuf) {
    fs::create_dir_all(dir.join("roles.d")).unwrap();
    fs::write(dir.join("roles.d/one"), "work claude\n").unwrap();
    fs::write(dir.join("roles.d/two"), "work codex\n").unwrap();
    fs::write(
        dir.join("models"),
        "work claude tiny low\nwork codex - ultra\n",
    )
    .unwrap();
    let before = match predecessor {
        "named" => {
            symlink(dir.join("roles.d/one"), dir.join("roles")).unwrap();
            Some("one")
        }
        "plain" => {
            fs::write(dir.join("roles"), "work claude\nfix@Sam/Box codex\n").unwrap();
            None
        }
        _ => None,
    };
    if applied {
        if predecessor == "plain" {
            fs::write(
                dir.join("roles.d/unsaved-original"),
                fs::read(dir.join("roles")).unwrap(),
            )
            .unwrap();
            fs::write(dir.join("roles.previous"), "unsaved-original\n").unwrap();
        } else if let Some(before) = before {
            fs::write(dir.join("roles.previous"), before).unwrap();
        }
        if burrow_settings::path_exists(&dir.join("roles")).unwrap() {
            fs::remove_file(dir.join("roles")).unwrap();
        }
        symlink(dir.join("roles.d/two"), dir.join("roles")).unwrap();
    }
    fs::create_dir(dir.join("receipts")).unwrap();
    let receipt = dir.join("receipts/receipt.operation");
    fs::write(
        dir.join("roles.operation"),
        serde_json::json!({"receipt":receipt, "destination":"two", "before":before, "result":null})
            .to_string(),
    )
    .unwrap();
    let cfg = burrow_settings::Config {
        overlay: dir.join("settings/overlay.json"),
        legacy_roles: Some(dir.to_owned()),
        ..Default::default()
    };
    (cfg, receipt)
}

#[test]
fn recovery_preserves_named_plain_and_absent_predecessors_at_both_boundaries() {
    for predecessor in ["named", "plain", "absent"] {
        for applied in [false, true] {
            let d = tempfile::tempdir().unwrap();
            let dir = d.path();
            let (cfg, receipt) = rig(dir, predecessor, applied);
            run(&cfg, dir).unwrap();
            assert_eq!(cfg.roles().unwrap().route("work", None).engine, "codex");
            let say = |line: &str| {
                cfg.command(
                    &line
                        .split_whitespace()
                        .map(str::to_owned)
                        .collect::<Vec<_>>(),
                    "test",
                )
            };
            if predecessor == "absent" {
                assert_eq!(say("engines use back").unwrap_err(), "no previous setup");
            } else {
                say("engines use back").unwrap();
                let roles = cfg.roles().unwrap();
                assert_eq!(roles.route("work", None).engine, "claude");
                assert_eq!(
                    roles.tuning("work", None, "claude").model.as_deref(),
                    Some("tiny")
                );
                assert_eq!(
                    roles.tuning("work", None, "codex").effort.as_deref(),
                    Some("ultra")
                );
                if predecessor == "plain" {
                    assert_eq!(roles.route("fix", Some("sam/box")).engine, "codex");
                }
            }
            // The old generation can recover without executing this request
            // again: both its actual destination and its predecessor survive.
            assert_eq!(
                fs::canonicalize(dir.join("roles")).unwrap(),
                dir.join("roles.d/two")
            );
            let old: super::recovery::Operation =
                serde_json::from_slice(&fs::read(dir.join("roles.operation")).unwrap()).unwrap();
            assert!(old.result.is_some());
            assert!(fs::read(&receipt).is_ok());
        }
    }
}

#[test]
fn failed_publication_cannot_acknowledge_and_retry_keeps_the_original_result() {
    for applied in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        let (cfg, receipt) = rig(dir, "plain", applied);
        // Fail after the legacy switch and receipt, before live publication.
        let pending = cfg.roles_file().with_extension("pending");
        fs::create_dir_all(&pending).unwrap();
        assert!(run(&cfg, dir).is_err());
        assert!(!cfg.roles_migrated().unwrap());
        assert!(cfg.roles().is_err());
        assert!(cfg.legacy_answer(&receipt, None).is_err());
        assert!(
            cfg.command(
                &["role".into(), "work".into(), "claude".into()],
                "intervening"
            )
            .is_err()
        );
        fs::remove_dir(pending).unwrap();
        run(&cfg, dir).unwrap();
        let said = cfg.answered(&receipt).unwrap().unwrap();
        // A lost filesystem receipt and intervening live CLI change cannot
        // reinterpret use back, and migration never rereads retired inputs.
        fs::remove_file(&receipt).unwrap();
        fs::remove_file(dir.join("roles.operation")).unwrap();
        cfg.command(
            &["role".into(), "work".into(), "claude".into()],
            "intervening",
        )
        .unwrap();
        fs::write(dir.join("models"), "invalid\n").unwrap();
        run(&cfg, dir).unwrap();
        assert_eq!(
            cfg.request(
                &receipt,
                &["engines".into(), "use".into(), "back".into()],
                "retry"
            )
            .unwrap(),
            said
        );
        assert_eq!(cfg.roles().unwrap().route("work", None).engine, "claude");
    }
}

#[test]
fn receipt_failure_keeps_the_journal_and_the_plain_predecessor() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let (cfg, receipt) = rig(dir, "plain", false);
    burrow_test::permissions(receipt.parent().unwrap(), 0o500);
    assert!(run(&cfg, dir).is_err());
    assert!(dir.join("roles.operation").is_file());
    assert!(!cfg.roles_migrated().unwrap());
    burrow_test::permissions(receipt.parent().unwrap(), 0o700);
    run(&cfg, dir).unwrap();
    cfg.command(&["engines".into(), "use".into(), "back".into()], "test")
        .unwrap();
    assert_eq!(
        cfg.roles().unwrap().route("fix", Some("sam/box")).engine,
        "codex"
    );
}

#[test]
fn old_path_errors_fail_reads_migration_and_check_config() {
    for kind in [
        "denied",
        "dangling",
        "file-parent",
        "loop",
        "saved-dangling",
    ] {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("old");
        match kind {
            "denied" => {
                fs::create_dir(&dir).unwrap();
                burrow_test::permissions(&dir, 0o000);
            }
            "dangling" => {
                symlink(d.path().join("absent"), &dir).unwrap();
            }
            "file-parent" => {
                fs::write(&dir, "file").unwrap();
            }
            "loop" => {
                symlink(&dir, &dir).unwrap();
            }
            _ => {
                fs::create_dir_all(dir.join("roles.d")).unwrap();
                symlink(dir.join("absent"), dir.join("roles.d/broken")).unwrap();
            }
        }
        let settings = burrow_settings::Config {
            overlay: d.path().join("settings/overlay.json"),
            legacy_roles: Some(dir.clone()),
            ..Default::default()
        };
        let mut cfg: crate::config::Config =
            serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.settings = Some(settings.clone());
        cfg.old_config = Some(dir.clone());
        assert!(cfg.role_view().is_err(), "{kind}");
        assert!(cfg.check_config().is_err(), "{kind}");
        assert!(run(&settings, &dir).is_err(), "{kind}");
        if kind == "denied" {
            burrow_test::permissions(dir, 0o700);
        }
    }
}

#[test]
fn cutover_holds_both_retired_locks_until_the_generation_stops() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let (mut cfg, _) = rig(dir, "named", false);
    let door = dir.join("door/lock");
    cfg.legacy_engines_lock = Some(door.clone());
    let mut inherited = Vec::new();
    run_with(&cfg, dir, |answer, held| {
        assert!(answer.is_ok());
        // These are exactly the lock inodes a fetched legacy pass or a CLI
        // already waiting for its turn would use, not replacement files.
        for path in [&door, &dir.join("roles.lock")] {
            let waiter = File::options().write(true).open(path).unwrap();
            assert!(waiter.try_lock().is_err(), "{}", path.display());
        }
        // What a child another thread forks now holds until it execs: the
        // held locks' own open file descriptions, kept open past the drop.
        for lock in held {
            inherited.push(lock.as_fd().try_clone_to_owned().unwrap());
        }
        assert!(cfg.roles_migrated().unwrap());
        Ok(())
    })
    .unwrap();
    assert_eq!(inherited.len(), 2);
    for path in [door, dir.join("roles.lock")] {
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .try_lock()
            .unwrap_or_else(|e| panic!("{}: {e:?}", path.display()));
    }
}

#[test]
fn failed_directory_sync_is_recovered_without_rereading_retired_inputs() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let (cfg, receipt) = rig(dir, "plain", false);
    fs::create_dir_all(cfg.overlay.parent().unwrap()).unwrap();
    burrow_test::permissions(cfg.overlay.parent().unwrap(), 0o300);
    assert!(run(&cfg, dir).is_err());
    assert!(cfg.roles_migrated().unwrap(), "rename was visible");
    assert!(
        cfg.legacy_answer(&receipt, None).is_err(),
        "not durable yet"
    );
    assert!(
        run(&cfg, dir).is_err(),
        "retry must sync, not just read the marker"
    );
    burrow_test::permissions(cfg.overlay.parent().unwrap(), 0o700);
    fs::write(dir.join("models"), "invalid after publication\n").unwrap();
    run(&cfg, dir).unwrap();
    assert!(cfg.legacy_answer(&receipt, None).unwrap().is_some());
}

// Run the real fence in a separate process so its lifetime and systemd
// notification can be checked without changing this test runner's environment.
#[test]
fn deployed_fence_child() {
    let Some(dir) = std::env::var_os("BURROW_TEST_FENCE") else {
        return;
    };
    let dir = Path::new(&dir);
    let cfg = serde_json::from_slice(&fs::read(dir.join("fence-config.json")).unwrap()).unwrap();
    fenced(&cfg, dir).unwrap();
}

struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_deployed_fence_notifies_and_retains_waiters_on_success_and_failure() {
    use std::os::unix::net::UnixDatagram;
    use std::process::{Command, Stdio};
    for failed in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        let (mut cfg, _) = rig(dir, "plain", false);
        let lock_path = dir.join("legacy-door.lock");
        cfg.legacy_engines_lock = Some(lock_path.clone());
        // Open the same lock files before cutover, as a waiting legacy
        // command would. Replacing their names cannot make this test pass.
        let waiters: Vec<_> = [&lock_path, &dir.join("roles.lock")]
            .iter()
            .map(|p| {
                File::options()
                    .create(true)
                    .truncate(false)
                    .write(true)
                    .open(p)
                    .unwrap()
            })
            .collect();
        if failed {
            fs::write(dir.join("models"), "invalid\n").unwrap();
        }
        fs::write(
            dir.join("fence-config.json"),
            serde_json::to_vec(&cfg).unwrap(),
        )
        .unwrap();
        let socket_path = dir.join("notify");
        let socket = UnixDatagram::bind(&socket_path).unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(15)))
            .unwrap();
        let mut child = Child(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "settings::migrate::regressions::deployed_fence_child",
                    "--nocapture",
                ])
                .env("BURROW_TEST_FENCE", dir)
                .env("NOTIFY_SOCKET", &socket_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut body = [0u8; 32];
        let count = socket.recv(&mut body).unwrap();
        assert_eq!(&body[..count], b"READY=1");
        assert!(child.0.try_wait().unwrap().is_none());
        for waiter in &waiters {
            assert!(waiter.try_lock().is_err());
        }
        let mut proj: crate::config::Config =
            serde_json::from_value(crate::config::tests::example()).unwrap();
        proj.settings = Some(cfg);
        proj.old_config = Some(dir.to_owned());
        assert_eq!(
            crate::settings::command(&proj, &["_migration-ready".into()]).is_ok(),
            !failed
        );
        drop(child); // A rollback stops the generation and restores legacy access.
        for waiter in waiters {
            waiter.try_lock().unwrap();
        }
    }
}

#[test]
fn recovering_a_switch_to_the_active_setup_preserves_the_existing_back_pointer() {
    for previous in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        let (cfg, receipt) = rig(dir, "named", false);
        if previous {
            fs::write(dir.join("roles.previous"), "two\n").unwrap();
        }
        fs::write(
            dir.join("roles.operation"),
            serde_json::json!({"receipt":receipt,"destination":"one","before":"one","result":null})
                .to_string(),
        )
        .unwrap();
        run(&cfg, dir).unwrap();
        let back = cfg.command(&["engines".into(), "use".into(), "back".into()], "test");
        if previous {
            back.unwrap();
            assert_eq!(cfg.roles().unwrap().route("work", None).engine, "codex");
        } else {
            assert_eq!(back.unwrap_err(), "no previous setup");
        }
    }
}

#[test]
fn a_completed_legacy_request_does_not_undo_an_intervening_legacy_switch() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let (cfg, receipt) = rig(dir, "named", true);
    let said = "Was active: one\nActive: two\n";
    fs::write(
        dir.join("roles.operation"),
        serde_json::json!({"receipt":receipt,"destination":"two","before":"one","result":said})
            .to_string(),
    )
    .unwrap();
    fs::remove_file(dir.join("roles")).unwrap();
    symlink(dir.join("roles.d/one"), dir.join("roles")).unwrap();
    fs::write(dir.join("roles.previous"), "two\n").unwrap();
    run(&cfg, dir).unwrap();
    assert_eq!(cfg.roles().unwrap().route("work", None).engine, "claude");
    assert_eq!(
        cfg.request(
            &receipt,
            &["engines".into(), "use".into(), "back".into()],
            "retry"
        )
        .unwrap(),
        said
    );
    assert_eq!(cfg.roles().unwrap().route("work", None).engine, "claude");
}
