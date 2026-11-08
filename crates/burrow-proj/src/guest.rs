//! A guest declares intent only. The host owns job state and GitHub access.

use crate::{config::Config, github::target, job::Job};
use rustix::{
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    process::{Pid, Signal, kill_process_group},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read},
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    thread::sleep,
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
    collect_with(cfg, job, n, agent, "agent", remaining.min(COLLECTION_TIME))
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
    let declaration = bounded(&mut command, budget);
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
    register(cfg, job, &declaration?)
}

fn register(cfg: &Config, job: &Job, bytes: &[u8]) -> Result<(), String> {
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
            crate::cmd::record_wait(cfg, job, &found, &declaration.targets).map_err(|e| {
                e.say
                    .unwrap_or_else(|| "couldn't register sandbox wait".into())
            })
        }
        _ => Err("invalid sandbox next step".into()),
    }
}

/// Poll a nonblocking pipe: neither an SSH login profile nor a descendant
/// holding stdout open can hide the deadline. Only a fixed buffer is read,
/// and stdout is capped before appending. Kill the local process group on
/// every exit, including successful reads that left descendants behind.
pub(crate) fn bounded(command: &mut Command, budget: Duration) -> Result<Vec<u8>, String> {
    let until = Instant::now() + budget;
    let mut child = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let pid = Pid::from_child(&child);
    let result = (|| {
        let mut stdout = child.stdout.take().ok_or("missing collection stdout")?;
        let flags = fcntl_getfl(&stdout).map_err(|e| e.to_string())?;
        fcntl_setfl(&stdout, flags | OFlags::NONBLOCK).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 8192];
        let mut eof = false;
        loop {
            if Instant::now() >= until {
                return Err("sandbox next-step command exceeded its time limit".into());
            }
            if !eof {
                match stdout.read(&mut buffer) {
                    Ok(0) => eof = true,
                    Ok(n) => {
                        if bytes.len() + n > MAX_DECLARATION {
                            return Err("sandbox next-step output exceeded 64 KiB".into());
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.to_string()),
                }
            }
            if eof && let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return if status.success() {
                    Ok(bytes)
                } else {
                    Err(format!("sandbox next-step command failed: {status}"))
                };
            }
            sleep(Duration::from_millis(5).min(until.saturating_duration_since(Instant::now())));
        }
    })();
    let _ = kill_process_group(pid, Signal::KILL);
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(error.contains("64 KiB"), "{error}");
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
