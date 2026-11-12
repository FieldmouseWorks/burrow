use super::*;
use serde_json::json;

thread_local! {
    static FLUSH_FAILURES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PUBLICATION_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(super) fn publication_failure() -> Result<(), String> {
    PUBLICATION_FAILURE.with(|armed| {
        if armed.replace(false) {
            Err("injected failure after rename before directory sync".into())
        } else {
            Ok(())
        }
    })
}

pub(super) fn flush_failure() -> Result<(), String> {
    FLUSH_FAILURES.with(|remaining| {
        if remaining.get() == 0 {
            return Ok(());
        }
        remaining.set(remaining.get() - 1);
        Err("injected receipt flush failure".into())
    })
}

#[test]
fn fresh_cached_and_adopted_receipts_retry_flush_without_replaying() {
    let d = tempfile::tempdir().unwrap();
    let cfg = config(d.path());
    say(&cfg, "role work claude").unwrap();
    say(&cfg, "engines save one").unwrap();
    say(&cfg, "role work codex").unwrap();
    say(&cfg, "engines save two").unwrap();
    say(&cfg, "engines use one").unwrap();
    say(&cfg, "engines use two").unwrap();
    let receipt = d.path().join("receipt.operation");
    let args = ["engines".into(), "use".into(), "back".into()];
    PUBLICATION_FAILURE.with(|armed| armed.set(true));
    FLUSH_FAILURES.with(|n| n.set(2));
    for _ in 0..3 {
        assert!(cfg.request(&receipt, &args, "test").is_err());
        assert_eq!(cfg.engines().unwrap().roles["work"], "claude");
    }
    let body = cfg.request(&receipt, &args, "test").unwrap();
    say(&cfg, "engines use two").unwrap();
    FLUSH_FAILURES.with(|n| n.set(1));
    assert!(cfg.receipt(&receipt).is_err());
    assert_eq!(cfg.request(&receipt, &args, "test").unwrap(), body);
    assert_eq!(cfg.engines().unwrap().roles["work"], "codex");
    let legacy = d.path().join("legacy.operation");
    fs::write(&legacy, json!({"result":"Already switched."}).to_string()).unwrap();
    FLUSH_FAILURES.with(|n| n.set(2));
    for _ in 0..2 {
        assert!(cfg.request(&legacy, &args, "test").is_err());
    }
    assert_eq!(
        cfg.request(&legacy, &args, "test").unwrap(),
        "Already switched."
    );
    assert_eq!(cfg.engines().unwrap().roles["work"], "codex");
}

fn config(dir: &std::path::Path) -> Config {
    Config {
        overlay: dir.join("live/overlay.json"),
        legacy: Some(dir.join("legacy")),
        ..Default::default()
    }
}

fn say(cfg: &Config, words: &str) -> Result<String, String> {
    cfg.command(
        &words
            .split_whitespace()
            .map(String::from)
            .collect::<Vec<_>>(),
        "test",
    )
}

#[test]
fn legacy_readers_need_no_writer_and_first_writers_import_once() {
    for first in [
        "offer codex small",
        "role extra claude",
        "engines save kept",
    ] {
        let d = tempfile::tempdir().unwrap();
        let cfg = config(d.path());
        let old = cfg.legacy.as_ref().unwrap();
        fs::create_dir_all(old.join("roles.d")).unwrap();
        fs::write(old.join("roles"), "work codex\n").unwrap();
        fs::write(old.join("models"), "work codex small low\n").unwrap();
        fs::write(old.join("roles.d/usual"), "work claude\n").unwrap();
        fs::write(old.join("roles.previous"), "usual\n").unwrap();
        // A directory at the lock path makes any attempt to open it fail,
        // even when tests run as root. Reads must not create the live directory.
        assert_eq!(cfg.engines().unwrap().roles["work"], "codex");
        assert!(!d.path().join("live").exists());
        fs::create_dir_all(cfg.overlay.with_extension("lock")).unwrap();
        assert!(say(&cfg, "engines list").unwrap().contains("work codex"));
        fs::remove_dir(cfg.overlay.with_extension("lock")).unwrap();
        say(&cfg, first).unwrap();
        let state = cfg.engines().unwrap();
        assert_eq!(state.roles["work"], "codex");
        assert_eq!(state.models["work codex"].model.as_deref(), Some("small"));
        assert!(state.saved.contains_key("usual"));
        assert_eq!(state.previous.as_deref(), Some("usual"));
        say(&cfg, "unrole work").unwrap();
        cfg.migrate().unwrap();
        assert!(!cfg.engines().unwrap().roles.contains_key("work"));
    }
}

#[test]
fn every_legacy_link_failure_leaves_migration_unpublished_and_retryable() {
    use std::os::unix::fs::symlink;
    for name in [
        "roles",
        "models",
        "roles.previous",
        "roles.d",
        "roles.d/usual",
    ] {
        let d = tempfile::tempdir().unwrap();
        let cfg = config(d.path());
        let old = cfg.legacy.as_ref().unwrap();
        fs::create_dir_all(old.join("roles.d")).unwrap();
        let path = old.join(name);
        if name == "roles.d" {
            fs::remove_dir(&path).unwrap();
        }
        symlink(old.join("missing"), &path).unwrap();
        assert!(cfg.migrate().is_err(), "{name}");
        assert!(!cfg.engines_path().exists(), "{name}");
        assert!(cfg.engines().is_err(), "{name}");
        fs::remove_file(&path).unwrap();
        if name == "roles.d" {
            fs::create_dir(&path).unwrap();
        }
        fs::write(old.join("roles"), "work codex\n").unwrap();
        assert!(cfg.migrate().unwrap(), "{name}");
        assert_eq!(cfg.engines().unwrap().roles["work"], "codex");
    }
}

#[test]
fn dangling_setup_directory_is_never_published_as_an_empty_import() {
    let d = tempfile::tempdir().unwrap();
    let cfg = config(d.path());
    let old = cfg.legacy.as_ref().unwrap();
    fs::create_dir(old).unwrap();
    fs::write(old.join("roles"), "work codex\n").unwrap();
    std::os::unix::fs::symlink(old.join("missing"), old.join("roles.d")).unwrap();
    assert!(cfg.migrate().is_err());
    assert!(!cfg.engines_path().exists());
    fs::create_dir(old.join("missing")).unwrap();
    fs::write(old.join("missing/kept"), "work claude\n").unwrap();
    assert!(cfg.migrate().unwrap());
    assert!(cfg.engines().unwrap().saved.contains_key("kept"));
}

#[test]
fn legacy_read_errors_leave_the_original_state_and_no_published_store() {
    for name in ["roles", "models", "roles.previous", "roles.d"] {
        let d = tempfile::tempdir().unwrap();
        let cfg = config(d.path());
        let old = cfg.legacy.as_ref().unwrap();
        fs::create_dir(old).unwrap();
        let bad = old.join(name);
        // Directory/file mismatches cause real I/O failures even under root.
        if name == "roles.d" {
            fs::write(&bad, "not a directory").unwrap();
        } else {
            fs::create_dir(&bad).unwrap();
        }
        assert!(cfg.migrate().is_err(), "{name}");
        assert!(cfg.engines().is_err(), "{name}");
        assert!(!cfg.engines_path().exists());
        assert!(bad.exists());
        if name == "roles.d" {
            fs::remove_file(&bad).unwrap();
        } else {
            fs::remove_dir(&bad).unwrap();
        }
        fs::write(old.join("roles"), "work codex\n").unwrap();
        cfg.migrate().unwrap();
        assert_eq!(cfg.engines().unwrap().roles["work"], "codex");
    }
}

#[test]
fn first_loaders_and_writers_share_one_migration_decision() {
    let d = tempfile::tempdir().unwrap();
    let cfg = config(d.path());
    let old = cfg.legacy.as_ref().unwrap();
    fs::create_dir(old).unwrap();
    fs::write(old.join("roles"), "work codex\n").unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    std::thread::scope(|scope| {
        for i in 0..8 {
            let barrier = barrier.clone();
            let cfg = &cfg;
            scope.spawn(move || {
                barrier.wait();
                if i == 0 {
                    say(cfg, "unrole work").unwrap();
                } else {
                    cfg.migrate().unwrap();
                }
            });
        }
    });
    assert!(!cfg.engines().unwrap().roles.contains_key("work"));
    let log = fs::read_to_string(cfg.overlay.with_extension("log")).unwrap();
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("\"actor\":\"migration\""))
            .count(),
        1
    );
}

#[test]
fn engine_status_and_save_validate_the_whole_state_without_publication() {
    let bad = [
        json!({"roles":{"work":"claude:chosen-model"}}),
        json!({"roles":{"work@Sam/Code":"codex"}}),
        json!({"roles":{"work":"claude over 101 codex"}}),
        json!({"models":{"work codex":{"model":"unoffered","effort":null}}}),
        json!({"models":{"work claude":{"model":null,"effort":"enormous"}}}),
        json!({"models":{"work@Sam/Code codex":{"model":null,"effort":null}}}),
        json!({"offered":{"codex":["bad model"]},"models":{"work codex":{"model":"bad model","effort":null}}}),
        json!({"saved":{"bad":{"roles":{"work":"claude:chosen-model"},"models":{}}}}),
    ];
    for mut state in bad {
        let d = tempfile::tempdir().unwrap();
        let cfg = config(d.path());
        fs::create_dir_all(cfg.overlay.parent().unwrap()).unwrap();
        state["saved"]["valid"] = json!({"roles":{},"models":{}});
        let bytes = state.to_string();
        fs::write(cfg.engines_path(), &bytes).unwrap();
        for command in [
            "engines",
            "engines list",
            "engines save new",
            "engines use valid",
        ] {
            assert!(say(&cfg, command).is_err(), "{command}: {state}");
            assert_eq!(fs::read_to_string(cfg.engines_path()).unwrap(), bytes);
        }
    }
}
