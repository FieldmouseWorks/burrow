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
    assert!(!crate::cmd::status_line(&job).contains(secret));
    crate::ask::record(
        &job,
        &crate::ask::Recommendation {
            default: "continue".into(),
            one_way: false,
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
