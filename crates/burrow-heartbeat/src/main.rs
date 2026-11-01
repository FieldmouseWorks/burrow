//! `burrow-heartbeat CONFIG`, which is HOST-heartbeat (lib.rs) once
//! modules/heartbeat.nix builds the config in. It runs as the heartbeat
//! unit: the ping key is its credential, the streaks its runtime directory.
//! A ping that doesn't land never fails the unit: the watcher already alerts
//! when pings stop, and a failed unit would make HOST-update's health compare
//! revert whatever it was switching to.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use burrow_heartbeat::{Config, Health, agent, send, verdict};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let [config] = args.as_slice() else {
        eprintln!("usage: burrow-heartbeat CONFIG");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-heartbeat: {e}");
            return ExitCode::from(2);
        }
    };
    let name = &cfg.name;
    let unit_dir = |var: &str| {
        env::var_os(var)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| format!("{name}: {var} is unset: run it as the {name} unit"))
    };
    let (creds, streaks) = match (
        unit_dir("CREDENTIALS_DIRECTORY"),
        unit_dir("RUNTIME_DIRECTORY"),
    ) {
        (Ok(c), Ok(s)) => (c, s),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let key_file = creds.join("ping-key");
    let key: String = match fs::read_to_string(&key_file) {
        Ok(k) => k.chars().filter(|c| !c.is_whitespace()).collect(),
        Err(e) => {
            eprintln!("{name}: {}: {e}", key_file.display());
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
