//! A guest declares intent only. The host owns job state and GitHub access.

use crate::{config::Config, github::target, job::Job};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path, process::Command};

#[derive(Default, Serialize, Deserialize)]
struct Declaration {
    end: String,
    targets: Vec<String>,
}

/// The guest's deliberately small `proj`: no host config or credentials.
pub fn declare(path: &Path, args: &[String]) -> Result<(), String> {
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
    let out = Command::new("agent")
        .args([
            "sh",
            agent,
            "sh",
            "-c",
            "if [ -f \"$1\" ]; then cat -- \"$1\"; fi",
            "sh",
            &path(job, n),
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("couldn't read the sandbox's next step".into());
    }
    if out.stdout.is_empty() {
        return Ok(());
    }
    let declaration: Declaration =
        serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;
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
