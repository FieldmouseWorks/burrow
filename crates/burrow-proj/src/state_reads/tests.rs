use crate::{
    config::{Config, tests::example},
    job::Job,
};
use std::{fs, os::unix::fs::symlink};

#[test]
fn host_reads_reject_symlinks_and_special_files() {
    let temp = tempfile::tempdir().unwrap();
    let p = temp.path();
    fs::write(p.join("denied"), "ordinary confidential text").unwrap();
    symlink(p.join("denied"), p.join("agent")).unwrap();
    assert!(crate::read(&p.join("agent")).is_none());
    fs::create_dir(p.join("private")).unwrap();
    fs::write(p.join("private/result"), "ordinary confidential text").unwrap();
    symlink(p.join("private"), p.join("job")).unwrap();
    assert!(crate::read(&p.join("job/result")).is_none());
    rustix::fs::mkfifoat(rustix::fs::CWD, p.join("fifo"), rustix::fs::Mode::RUSR).unwrap();
    assert!(crate::read(&p.join("fifo")).is_none());
    assert!(crate::read(std::path::Path::new("/dev/zero")).is_none());
}

#[test]
fn host_reads_cannot_be_redirected_by_directory_replacement() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let temp = tempfile::tempdir().unwrap();
    let p = temp.path();
    fs::create_dir(p.join("live")).unwrap();
    fs::write(p.join("live/value"), "public").unwrap();
    fs::create_dir(p.join("denied")).unwrap();
    fs::write(p.join("denied/value"), "ordinary confidential text").unwrap();
    symlink(p.join("denied"), p.join("redirect")).unwrap();
    let finished = Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    let root = p.to_owned();
    let thread = std::thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            rustix::fs::renameat_with(
                rustix::fs::CWD,
                root.join("live"),
                rustix::fs::CWD,
                root.join("redirect"),
                rustix::fs::RenameFlags::EXCHANGE,
            )
            .unwrap();
        }
    });
    let mut leaked = false;
    for _ in 0..2000 {
        if let Some(value) = crate::read(&p.join("live/value")) {
            leaked |= value != "public";
        }
    }
    finished.store(true, Ordering::Relaxed);
    thread.join().unwrap();
    assert!(!leaked);
}

#[test]
fn status_snapshots_and_notices_do_not_read_outside_job_state() {
    let temp = tempfile::tempdir().unwrap();
    let p = temp.path();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = p.join("jobs");
    cfg.tools.systemctl = p.join("systemctl");
    burrow_test::executable(&cfg.tools.systemctl, "#!/bin/sh\nexit 3\n");
    let job = Job::new(&cfg, "j1");
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    let secret = "ordinary confidential text";
    fs::write(p.join("denied"), secret).unwrap();
    for name in ["agent", "1.result"] {
        symlink(p.join("denied"), job.path(name)).unwrap();
    }
    assert!(!crate::snapshot::row(&job).line().contains(secret));
    crate::ask::record(
        &job,
        &crate::ask::Recommendation {
            default: "continue".into(),
            one_way: false,
            about: None,
        },
    )
    .unwrap();
    crate::ask::finish(&cfg, &job, "ask").unwrap();
    assert!(
        !crate::read(&crate::ask::host_path(&job, "question-text"))
            .unwrap()
            .contains(secret)
    );
    fs::write(
        p.join("stream"),
        serde_json::json!({"type":"result","result":secret}).to_string(),
    )
    .unwrap();
    symlink(p.join("stream"), job.turn("1", "jsonl")).unwrap();
    assert!(crate::cmd::told(&cfg, "j1", "1").is_err());
    // The selector is data, never another pathname, for reads or writes.
    fs::write(job.path("turns"), p.join("denied").display().to_string()).unwrap();
    assert!(job.turns().is_none());
    for selector in ["../../denied", p.to_str().unwrap()] {
        assert_eq!(
            job.turn(selector, "result").parent(),
            Some(job.dir.as_path())
        );
    }
}

#[test]
fn turn_selectors_never_name_paths() {
    let temp = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = temp.path().join("jobs");
    let job = Job::new(&cfg, "j1");
    fs::create_dir_all(&job.dir).unwrap();
    for selector in ["../../denied", temp.path().to_str().unwrap(), "0", "1/../2"] {
        fs::write(job.path("turns"), selector).unwrap();
        assert!(job.turns().is_none(), "{selector}");
        assert_eq!(
            job.turn(selector, "result").parent(),
            Some(job.dir.as_path())
        );
    }
}

#[test]
fn configured_app_environment_may_be_a_system_symlink_without_allowing_job_symlinks() {
    let temp = tempfile::tempdir().unwrap();
    let p = temp.path();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    fs::write(
        p.join("immutable-env"),
        "GH_TOKEN_SOCKET=/run/gh-token/workers.sock\n",
    )
    .unwrap();
    symlink(p.join("immutable-env"), p.join("system-env")).unwrap();
    cfg.apps.insert("workers".into(), p.join("system-env"));
    assert_eq!(
        cfg.app_environment("workers").as_deref(),
        Some("GH_TOKEN_SOCKET=/run/gh-token/workers.sock")
    );
    assert!(crate::read(&p.join("system-env")).is_none());
    assert!(cfg.app_environment("../workers").is_none());
}

#[test]
fn recording_a_choice_never_follows_job_state_links_and_reports_failures() {
    use crate::engine::{Choice, Engine};
    let temp = tempfile::tempdir().unwrap();
    let p = temp.path();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = p.join("jobs");
    let dir = cfg.jobs.join("j");
    fs::create_dir_all(&dir).unwrap();
    let job = Job::new(&cfg, "j");
    let set = Choice {
        engine: Engine::Codex,
        model: Some("m".into()),
        effort: Some("e".into()),
    };
    let unset = Choice {
        engine: Engine::Codex,
        model: None,
        effort: None,
    };
    for kind in ["model", "effort"] {
        let outside = p.join(format!("outside-{kind}"));
        fs::write(&outside, "host").unwrap();
        symlink(&outside, dir.join(format!("1.{kind}"))).unwrap();
        // Writing replaces the link; removing unlinks it. Neither touches the target.
        job.record_choice("1", &set).unwrap();
        assert_eq!(fs::read_to_string(&outside).unwrap(), "host");
        assert_eq!(
            fs::read_to_string(dir.join(format!("1.{kind}"))).unwrap(),
            if kind == "model" { "m" } else { "e" }
        );
        fs::remove_file(dir.join(format!("1.{kind}"))).unwrap();
        symlink(&outside, dir.join(format!("1.{kind}"))).unwrap();
        job.record_choice("1", &unset).unwrap();
        assert_eq!(fs::read_to_string(&outside).unwrap(), "host");
        assert!(!dir.join(format!("1.{kind}")).exists());
    }
    // A hard link to a host file is replaced, not written through.
    let outside = p.join("outside-hard");
    fs::write(&outside, "host").unwrap();
    fs::hard_link(&outside, dir.join("2.model")).unwrap();
    job.record_choice("2", &set).unwrap();
    assert_eq!(fs::read_to_string(&outside).unwrap(), "host");
    // A redirected job directory is refused.
    let real = p.join("real");
    fs::create_dir(&real).unwrap();
    fs::remove_dir_all(&dir).unwrap();
    symlink(&real, &dir).unwrap();
    assert!(job.record_choice("1", &set).is_err());
    assert!(fs::read_dir(&real).unwrap().next().is_none());
    // A failed engine record is an error, not a silent default to Claude.
    fs::remove_file(&dir).unwrap();
    fs::create_dir_all(dir.join("3.engine")).unwrap();
    assert!(job.record_choice("3", &set).is_err());
}
