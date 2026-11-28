//! cargo-follow CONFIG [status] [OWNER/REPO]: a pass (see lib.rs); `status`
//! only says what it would do.
//! cargo-follow CONFIG config: what the config says it keeps.
//! cargo-follow changes MANIFEST GIT_DIR HEAD: what each of the manifest's
//! follows counts from its pin to HEAD, in a clone of its upstream.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use burrow_github::{Gh, deferred};
use cargo_follow::config::Config;
use cargo_follow::local::{Git, Work};
use cargo_follow::pass::{self, Ask};
use cargo_follow::{changes, manifest};

const USAGE: &str = "usage: cargo-follow CONFIG [status] [OWNER/REPO] | CONFIG config | changes MANIFEST GIT_DIR HEAD";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let ran = match words.as_slice() {
        ["changes", manifest, dir, head] => show_changes(manifest, dir, head),
        [config, "config"] => read(config).map(|cfg| {
            for m in &cfg.manifests {
                match cfg.first_for(&m.repo, &m.repo) {
                    Some(label) => println!("{}:{} [{label}]", m.repo, m.path),
                    None => println!("{}:{}", m.repo, m.path),
                }
            }
            true
        }),
        [config] => pass(config, false, None),
        [config, "status"] => pass(config, true, None),
        [config, "status", only] => pass(config, true, Some(only)),
        [config, only] if !only.starts_with('-') => pass(config, false, Some(only)),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match ran {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("cargo-follow: {e}");
            ExitCode::FAILURE
        }
    }
}

fn read(config: &str) -> Result<Config, String> {
    Config::read(Path::new(config))
}

fn pass(config: &str, dry: bool, only: Option<&str>) -> Result<bool, String> {
    let cfg = read(config)?;
    // One pass at a time: two would share the clones, and could each close
    // or push over the other's PR.
    let lock = cfg.work.join("lock");
    let held = File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock)
        .and_then(|f| f.lock().map(|()| f))
        .map_err(|e| format!("{}: {e}", lock.display()))?;
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let mut work = Work {
        root: cfg.work.clone(),
    };
    let ran = pass::run(
        &cfg,
        &Ask { only, dry },
        &mut Gh::default(),
        &mut work,
        &mut std::io::stdout(),
    );
    drop(held);
    if dry {
        return Ok(ran.ok);
    }
    // What this pass is booked as in the wake file: GitHub's rate limit is
    // a wait, not a failure, so a held-off pass is due again when it lifts.
    let key = only.unwrap_or_default();
    match ran.held {
        Some(at) => {
            deferred::rebook(&cfg.wake, key, at)?;
            println!("booked again for {at}, when GitHub's rate limit lifts");
        }
        None => deferred::ran(&cfg.wake, key, started)?,
    }
    Ok(ran.ok)
}

fn show_changes(path: &str, dir: &str, head: &str) -> Result<bool, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let git = Git::new(PathBuf::from(dir));
    for f in manifest::follows(&text)? {
        let counted = changes::changes(&git, &f.rules, &f.pin, head)?;
        if counted.is_empty() {
            println!("{}: nothing counts from {} to {head}", f.upstream, f.pin);
        }
        for c in counted {
            println!("{}: {c}", f.upstream);
        }
    }
    Ok(true)
}
