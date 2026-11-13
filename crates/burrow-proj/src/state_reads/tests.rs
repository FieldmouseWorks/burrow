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
    assert!(
        !crate::snapshot::row(&job, crate::host_jobs::View::Owner)
            .line()
            .contains(secret)
    );
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

/// A job's directory is writable inside its sandbox. Links it plants for its
/// turn's records must not carry the host's end-of-turn writes elsewhere.
#[test]
fn a_turns_end_never_writes_through_links_the_job_planted() {
    let temp = tempfile::tempdir().unwrap();
    let p = temp.path();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = p.join("jobs");
    cfg.scratch = p.join("scratch");
    cfg.wake = p.join("wake.json");
    cfg.roles = p.join("roles");
    cfg.models = p.join("models");
    cfg.kick.clear();
    cfg.notify = None;
    cfg.meters = None;
    cfg.tools.systemctl = p.join("systemctl");
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3 ;; *LoadState*) echo not-found ;; *ActiveState*) echo inactive ;; esac\n",
    );
    cfg.tools.systemd_run = "true".into();
    let job = Job::new(&cfg, "j1");
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Manual,
    )
    .unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("project"), format!("{}\n", p.display())).unwrap();
    fs::write(job.path("prompt.1"), "the task\n").unwrap();
    let denied = p.join("denied");
    fs::write(&denied, "host only\n").unwrap();
    // The engine's run: the job plants its links, says it's done and answers.
    let mut plant = String::new();
    for kind in ["result", "end", "outcome", "result.new", "end.new"] {
        plant.push_str(&format!(
            "ln -s '{}' '{}'\n",
            denied.display(),
            job.turn("1", kind).display()
        ));
    }
    let engine = p.join("engine");
    burrow_test::executable(
        &engine,
        format!(
            "#!/bin/sh\n{plant}echo done > '{next}'\n\
             echo '{{\"type\":\"system\",\"session_id\":\"s\"}}'\n\
             echo '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"chosen by the job\"}}'\n",
            next = job.path("end.next").display()
        ),
    );
    let mut run = cfg.clone();
    run.tools.timeout = engine;
    let _ = crate::turn::exec(&run, "j1", "normal");
    assert_eq!(fs::read_to_string(&denied).unwrap(), "host only\n");
    assert_eq!(
        crate::read(&job.turn("1", "result")).as_deref(),
        Some("chosen by the job")
    );
    assert_eq!(crate::read(&job.turn("1", "end")).as_deref(), Some("done"));
}

/// A one-way question past its deadline ends the job with a turn of its own;
/// links already waiting at that turn's names stay links to untouched files.
#[test]
fn a_one_way_timeout_never_writes_through_links_the_job_planted() {
    let temp = tempfile::tempdir().unwrap();
    let p = temp.path();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = p.join("jobs");
    cfg.scratch = p.join("scratch");
    cfg.wake = p.join("wake.json");
    cfg.kick.clear();
    cfg.notify = None;
    cfg.tools.systemctl = p.join("systemctl");
    burrow_test::executable(
        &cfg.tools.systemctl,
        "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3 ;; *LoadState*) echo not-found ;; *ActiveState*) echo inactive ;; esac\n",
    );
    cfg.tools.systemd_run = "true".into();
    let job = Job::new(&cfg, "j1");
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Manual,
    )
    .unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("session"), "s1\n").unwrap();
    fs::write(job.turn("1", "end"), "ask\n").unwrap();
    fs::write(job.turn("1", "outcome"), "done\n").unwrap();
    let control = crate::ask::host_path(&job, "");
    fs::create_dir_all(&control).unwrap();
    fs::write(
        control.join("question"),
        serde_json::json!({"recommendation":{"default":"delete the branch","one_way":true},"due":crate::now() - 1}).to_string(),
    )
    .unwrap();
    let denied = p.join("denied");
    fs::write(&denied, "host only\n").unwrap();
    for kind in ["result", "end", "outcome", "jsonl"] {
        symlink(&denied, job.turn("2", kind)).unwrap();
        symlink(&denied, job.turn("2", &format!("{kind}.new"))).unwrap();
    }
    crate::watch::look(&cfg, "").unwrap();
    assert_eq!(job.turns().as_deref(), Some("2"));
    assert_eq!(fs::read_to_string(&denied).unwrap(), "host only\n");
    assert_eq!(crate::read(&job.turn("2", "end")).as_deref(), Some("done"));
    for kind in ["result", "end", "outcome", "jsonl"] {
        assert!(!job.turn("2", kind).is_symlink(), "{kind}");
    }
}

/// Whether `text` starts with a path built in a job's directory.
fn is_job_dir_path(text: &str) -> bool {
    let text = text.strip_prefix('&').unwrap_or(text);
    ["job.", "self.", "run.job."].iter().any(|owner| {
        ["path(", "turn("]
            .iter()
            .any(|f| text.starts_with(&format!("{owner}{f}")))
    })
}

/// The records `Job::turn` keeps in the host's control root, not the job's.
fn is_host_record(call: &str) -> bool {
    call.contains("\"retry-") && !call.contains("\"retry-wake\"") || call.contains("\"execution\"")
}

/// Every host write into a job's directory goes through `state_reads`: the
/// plain writers follow a link the job planted at the name, or at `NAME.new`.
/// A record `Job::turn` keeps in the host's control root is not the job's.
#[test]
fn host_writes_into_job_directories_go_through_the_confined_writer() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                if !matches!(name.as_str(), "tests" | "fixtures") {
                    stack.push(path);
                }
                continue;
            }
            if !name.ends_with(".rs") || name.contains("test") || name.starts_with("regression_") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap();
            let flat: String = code.chars().filter(|c| !c.is_whitespace()).collect();
            // Locals bound to a job-directory path: `let f = job.path("await")`.
            let mut aliases = Vec::new();
            for (at, _) in flat.match_indices("let") {
                let rest = &flat[at + 3..];
                let rest = rest.strip_prefix("mut").unwrap_or(rest);
                let Some((name, value)) = rest.split_once('=') else { continue };
                if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let value = &value[..value.len().min(80)];
                if is_job_dir_path(value) && !is_host_record(value) {
                    aliases.push(name.to_owned());
                }
            }
            for writer in ["fs::write(", "swap(", "File::create(", ".open("] {
                for (at, _) in flat.match_indices(writer) {
                    let args = &flat[at + writer.len()..];
                    let args = args.strip_prefix('&').unwrap_or(args);
                    let call = &args[..args.len().min(80)];
                    let host = is_host_record(call);
                    let aliased = aliases.iter().any(|a| {
                        args.strip_prefix(a.as_str()).is_some_and(|r| {
                            r.starts_with([',', ')', '.'])
                        })
                    });
                    let into_job = is_job_dir_path(args) || aliased;
                    if into_job && !host {
                        found.push(format!(
                            "{}: {writer}{call}",
                            path.strip_prefix(&src).unwrap().display()
                        ));
                    }
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "unconfined job-dir writes:\n{}",
        found.join("\n")
    );
}
