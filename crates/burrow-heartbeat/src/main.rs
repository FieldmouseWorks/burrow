//! `burrow-heartbeat CONFIG`, which is HOST-heartbeat (lib.rs) once
//! modules/heartbeat.nix builds the config in, and `burrow-heartbeat --site
//! CONFIG`, which is one site's watch, HOST-heartbeat-site-SLUG (site.rs).
//! Each runs as its unit: the ping key is its credential, the beat's streaks
//! its runtime directory. A ping that doesn't land never fails the unit: the
//! watcher already alerts when pings stop, and a failed unit would make
//! HOST-update's health compare revert whatever it was switching to.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use burrow_heartbeat::site::{self, SiteConfig};
use burrow_heartbeat::{Config, Health, agent, send, verdict};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [flag, config] if flag == "--site" => watch(Path::new(config)),
        [config] if !config.starts_with("--") => beat(Path::new(config)),
        _ => {
            eprintln!("usage: burrow-heartbeat CONFIG | burrow-heartbeat --site CONFIG");
            ExitCode::from(2)
        }
    }
}

/// A directory the unit gives it, by its variable.
fn unit_dir(name: &str, var: &str) -> Result<PathBuf, String> {
    env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("{name}: {var} is unset: run it as the {name} unit"))
}

/// The ping key, from the unit's credential, without its whitespace.
fn ping_key(name: &str) -> Result<String, String> {
    let key_file = unit_dir(name, "CREDENTIALS_DIRECTORY")?.join("ping-key");
    fs::read_to_string(&key_file)
        .map(|k| k.chars().filter(|c| !c.is_whitespace()).collect())
        .map_err(|e| format!("{name}: {}: {e}", key_file.display()))
}

fn beat(config: &Path) -> ExitCode {
    let cfg = match Config::read(config) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-heartbeat: {e}");
            return ExitCode::from(2);
        }
    };
    let name = &cfg.name;
    let (key, streaks) = match (ping_key(name), unit_dir(name, "RUNTIME_DIRECTORY")) {
        (Ok(k), Ok(s)) => (k, s),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let ping = verdict(&cfg, &Health::run(&cfg.health), &streaks);
    println!(
        "{} {}",
        if ping.suffix.is_empty() {
            "/"
        } else {
            ping.suffix
        },
        ping.body
    );
    if let Err(e) = send(
        &agent(),
        &cfg.url,
        &key,
        &cfg.check,
        &ping,
        &mut std::thread::sleep,
    ) {
        eprintln!(
            "the ping to {} didn't land ({e}); Healthchecks alerts once the check's grace runs out",
            cfg.check
        );
    }
    ExitCode::SUCCESS
}

fn watch(config: &Path) -> ExitCode {
    let cfg = match SiteConfig::read(config) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-heartbeat: {e}");
            return ExitCode::from(2);
        }
    };
    let key = match ping_key(&cfg.name) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let said = site::watch(&agent(), &cfg, &key, &mut std::thread::sleep);
    println!("{}", said.out);
    if let Some(err) = said.err {
        eprintln!("{err}");
    }
    ExitCode::SUCCESS
}
