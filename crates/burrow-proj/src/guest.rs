//! A guest declares intent only. The host owns job state and GitHub access.

use crate::{config::Config, github::target, job::Job};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const MAX_DECLARATION: usize = 64 * 1024;
const COLLECTION_TIME: Duration = Duration::from_secs(30);

#[derive(Default, Serialize, Deserialize)]
struct Declaration {
    end: String,
    targets: Vec<String>,
}

/// The guest's deliberately small `proj`: no host config or credentials.
pub fn declare(path: &Path, args: &[String]) -> Result<(), String> {
    let lock = fs::File::options()
        .create(true)
        .append(true)
        .open(path.with_extension("lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    let mut declaration = fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Declaration>(&b).ok())
        .unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("await") if args.len() > 1 => {
            if args[1..].iter().any(|s| target(s).is_none()) {
                return Err("expected GitHub PR or issue URLs".into());
            }
            declaration.end = "await".into();
            declaration.targets.extend_from_slice(&args[1..]);
        }
        Some(how @ ("ask" | "done")) if args.len() == 1 => {
            declaration.end = how.into();
            declaration.targets.clear();
        }
        _ => return Err("usage: proj await URL... | proj ask | proj done".into()),
    }
    let next = path.with_extension("next");
    fs::write(
        &next,
        serde_json::to_vec(&declaration).map_err(|e| e.to_string())?,
    )
    .and_then(|()| fs::rename(next, path))
    .map_err(|e| e.to_string())?;
    println!("Noted. End your turn now. The host registers GitHub waits when this run ends.");
    Ok(())
}

pub(crate) fn path(job: &Job, n: &str) -> String {
    format!("/tmp/proj-{}-{n}.json", job.id)
}

/// Read only data through agent sh; never execute a guest's declaration.
pub(crate) fn collect(cfg: &Config, job: &Job, n: &str, agent: &str) -> Result<(), String> {
    let remaining = crate::read(&job.turn(n, "deadline"))
        .and_then(|s| s.parse::<i64>().ok())
        .map(|d| Duration::from_nanos(d.saturating_sub(crate::now_nanos()).max(0) as u64))
        .unwrap_or(COLLECTION_TIME);
    collect_with(cfg, job, n, agent, "agent", remaining)
}

fn collect_with(
    cfg: &Config,
    job: &Job,
    n: &str,
    agent: &str,
    executable: &str,
    budget: Duration,
) -> Result<(), String> {
    let mut command = Command::new(executable);
    command.args([
        "sh",
        agent,
        "sh",
        "-c",
        "if [ -f \"$1\" ]; then cat -- \"$1\"; fi",
        "sh",
        &path(job, n),
    ]);
    let until = Instant::now() + budget;
    let declaration = bounded(&mut command, budget.min(COLLECTION_TIME));
    // Stop even if SSH blocked, the output was malformed or too large, or
    // there was no declaration. Nothing in the guest survives collection.
    let stopped = bounded(
        Command::new(executable).args(["stop", agent]),
        Duration::from_secs(60),
    );
    if let Err(e) = stopped {
        return Err(format!(
            "couldn't stop sandbox after collecting its next step: {e}"
        ));
    }
    register(cfg, job, &declaration?, until)
}

fn register(cfg: &Config, job: &Job, bytes: &[u8], until: Instant) -> Result<(), String> {
    if bytes.is_empty() {
        return Ok(());
    }
    let declaration: Declaration = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    match declaration.end.as_str() {
        "ask" | "done" if declaration.targets.is_empty() => {
            let _ = fs::remove_file(job.path("await.next"));
            fs::write(job.path("end.next"), declaration.end).map_err(|e| e.to_string())
        }
        "await" if !declaration.targets.is_empty() => {
            let found = declaration
                .targets
                .iter()
                .map(|s| target(s).ok_or("invalid GitHub target"))
                .collect::<Result<Vec<_>, _>>()?;
            crate::cmd::record_wait(cfg, job, &found, &declaration.targets, Some(until)).map_err(
                |e| {
                    e.say
                        .unwrap_or_else(|| "couldn't register sandbox wait".into())
                },
            )
        }
        _ => Err("invalid sandbox next step".into()),
    }
}

/// The guest transport has a smaller output cap than GitHub pagination.
pub(crate) fn bounded(command: &mut Command, budget: Duration) -> Result<Vec<u8>, String> {
    crate::subprocess::output(
        command.stderr(Stdio::null()),
        Instant::now() + budget,
        MAX_DECLARATION,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // A local gh fixture, so the deadline is exercised through baseline
    // registration and the real API/pagination path, not a mocked timer.
    fn registration_fixture(script: &str) -> (tempfile::TempDir, Config) {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let gh = d.path().join("gh");
        fs::write(
            &gh,
            format!(
                "#!/bin/sh\necho $$ >>{}/pids\necho \"$*\" >>{}/calls\n{script}\n",
                d.path().display(),
                d.path().display()
            ),
        )
        .unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        let mut value = crate::config::tests::example();
        value["jobs"] = serde_json::json!(d.path().join("jobs"));
        value["tools"]["gh"] = serde_json::json!(gh);
        (d, serde_json::from_value(value).unwrap())
    }

    fn registration_failure(cfg: &Config, declaration: &[u8], budget: Duration) {
        let job = Job::new(cfg, "vm");
        fs::create_dir_all(&job.dir).unwrap();
        fs::write(job.path("await.next"), "previous wait\n").unwrap();
        fs::write(job.path("end.next"), "ask\n").unwrap();
        let from = Instant::now();
        assert!(register(cfg, &job, declaration, from + budget).is_err());
        assert!(from.elapsed() < Duration::from_secs(3));
        assert_eq!(
            fs::read_to_string(job.path("await.next")).unwrap(),
            "previous wait\n"
        );
        assert_eq!(fs::read_to_string(job.path("end.next")).unwrap(), "ask\n");
        // Each outstanding gh process was killed and reaped before returning.
        let dir = cfg.tools.gh.parent().unwrap();
        for pid in fs::read_to_string(dir.join("pids")).unwrap().lines() {
            assert!(
                !Command::new("sh")
                    .args(["-c", "kill -0 \"$1\" 2>/dev/null", "sh", pid])
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    #[test]
    fn registration_bounds_stalled_github_and_pagination_without_publishing() {
        for script in [
            "exec sleep 60",
            "printf 'HTTP/2.0 200 OK\\n\\n'\ncase \"$*\" in */comments*) printf '['; sleep 60 ;; *) echo '{\"state\":\"open\"}' ;; esac",
        ] {
            let (_dir, cfg) = registration_fixture(script);
            registration_failure(
                &cfg,
                br#"{"end":"await","targets":["o/r#1"]}"#,
                Duration::from_millis(80),
            );
        }
    }

    #[test]
    fn registration_uses_one_deadline_across_slow_requests_and_targets() {
        let (dir, cfg) = registration_fixture(
            "sleep 0.15\nprintf 'HTTP/2.0 200 OK\\n\\n'\ncase \"$*\" in */comments*) echo '[]' ;; *) echo '{\"state\":\"open\"}' ;; esac",
        );
        registration_failure(
            &cfg,
            br#"{"end":"await","targets":["o/r#1","o/r#2","o/r#3","o/r#4"]}"#,
            Duration::from_millis(650),
        );
        let calls = fs::read_to_string(dir.path().join("calls")).unwrap();
        assert!(calls.lines().count() >= 3, "{calls}");
        assert!(calls.contains("repos/o/r/issues/2"), "{calls}");
    }

    #[test]
    fn registration_commits_complete_baselines_before_the_deadline() {
        let (_dir, cfg) = registration_fixture(
            "printf 'HTTP/2.0 200 OK\\n\\n'\ncase \"$*\" in */comments*) echo '[]' ;; *) echo '{\"state\":\"open\"}' ;; esac",
        );
        let job = Job::new(&cfg, "vm");
        fs::create_dir_all(&job.dir).unwrap();
        register(
            &cfg,
            &job,
            br#"{"end":"await","targets":["o/r#1","o/r#2"]}"#,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        let waits = fs::read_to_string(job.path("await.next")).unwrap();
        assert_eq!(waits.lines().count(), 2);
        assert!(
            waits.contains("o/r#1\t") && waits.contains("o/r#2\t"),
            "{waits}"
        );
        assert_eq!(fs::read_to_string(job.path("end.next")).unwrap(), "await\n");
    }

    #[test]
    fn concurrent_declarations_preserve_every_target() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("end.json");
        let barrier = std::sync::Barrier::new(32);
        std::thread::scope(|scope| {
            for n in 0..32 {
                let (p, barrier) = (&p, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    declare(p, &["await".into(), format!("o/r#{n}")]).unwrap();
                });
            }
        });
        let declaration: Declaration = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        let targets: std::collections::BTreeSet<_> = declaration.targets.into_iter().collect();
        assert_eq!(targets, (0..32).map(|n| format!("o/r#{n}")).collect());
        assert_eq!(declaration.end, "await");
    }

    #[test]
    fn ask_and_done_take_the_same_lock_before_clearing_waits() {
        for how in ["ask", "done"] {
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("end.json");
            declare(&p, &["await".into(), "o/r#1".into()]).unwrap();
            let lock = fs::File::options()
                .append(true)
                .open(p.with_extension("lock"))
                .unwrap();
            lock.lock().unwrap();
            std::thread::scope(|scope| {
                let (started, running) = std::sync::mpsc::channel();
                let (ended, result) = std::sync::mpsc::channel();
                let p = &p;
                scope.spawn(move || {
                    started.send(()).unwrap();
                    ended.send(declare(p, &[how.into()])).unwrap();
                });
                running.recv().unwrap();
                assert!(result.recv_timeout(Duration::from_millis(50)).is_err());
                drop(lock);
                result
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
            });
            let declaration: Declaration = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
            assert_eq!(declaration.end, how);
            assert!(declaration.targets.is_empty());
        }
    }

    #[test]
    fn collection_bounds_blocked_commands_and_inherited_pipes() {
        for script in ["exec sleep 60", "sleep 60 & exit 0"] {
            let from = Instant::now();
            let error = bounded(
                Command::new("sh").args(["-c", script]),
                Duration::from_millis(80),
            )
            .unwrap_err();
            assert!(error.contains("time limit"), "{error}");
            assert!(from.elapsed() < Duration::from_secs(5));
        }
    }

    #[test]
    fn collection_caps_output_before_buffering_it() {
        let error = bounded(
            Command::new("head").args(["-c", "65537", "/dev/zero"]),
            Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(error.contains("65536 bytes"), "{error}");
    }

    #[test]
    fn every_collection_failure_stops_the_vm_before_returning() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let mut value = crate::config::tests::example();
        value["jobs"] = serde_json::json!(d.path().join("jobs"));
        let cfg: Config = serde_json::from_value(value).unwrap();
        let job = Job::new(&cfg, "vm");
        fs::create_dir_all(&job.dir).unwrap();
        let agent = d.path().join("agent");
        // A throwaway transport fixture: stop records its invocation even
        // when sh blocks in startup, emits too much, or returns bad data.
        for command in [
            "exec sleep 60",
            "head -c 65537 /dev/zero",
            "echo malformed",
            "exit 1",
            "echo '{\"end\":\"invalid\",\"targets\":[]}'",
            "echo '{\"end\":\"await\",\"targets\":[\"bad\"]}'",
        ] {
            let marker = d.path().join("stopped");
            let _ = fs::remove_file(&marker);
            fs::write(
                &agent,
                format!(
                    "#!/bin/sh\nif [ \"$1\" = stop ]; then touch {}; else {}; fi\n",
                    marker.display(),
                    command
                ),
            )
            .unwrap();
            fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(
                collect_with(
                    &cfg,
                    &job,
                    "1",
                    "demo",
                    agent.to_str().unwrap(),
                    Duration::from_millis(80)
                )
                .is_err()
            );
            assert!(marker.exists(), "VM wasn't stopped after {command}");
        }
    }

    #[test]
    fn declarations_accumulate_awaits_and_last_end_wins() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("end.json");
        let say =
            |args: &[&str]| declare(&p, &args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        say(&["await", "a/b#1"]).unwrap();
        say(&["await", "a/b#2"]).unwrap();
        let read = || serde_json::from_slice::<Declaration>(&fs::read(&p).unwrap()).unwrap();
        assert_eq!(read().targets.len(), 2);
        assert!(say(&["await", "bad"]).is_err());
        assert_eq!(read().targets.len(), 2);
        say(&["ask"]).unwrap();
        assert!(read().targets.is_empty());
        assert_eq!(read().end, "ask");
        say(&["done"]).unwrap();
        assert_eq!(read().end, "done");
    }
}
