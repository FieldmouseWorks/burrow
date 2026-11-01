//! HOST-heartbeat: one beat. It tells the watcher off this machine how the
//! machine is. modules/heartbeat.nix runs it every few minutes, builds its
//! config in, and hands it the ping key as the unit's credential.
//!
//! | What HOST-health says                    | The ping                              |
//! |------------------------------------------|---------------------------------------|
//! | no check has failed `failAfter` beats    | success (a check failing for fewer is named in it: a blip) |
//! | one has failed `failAfter` beats running | fail, naming it: alerts at once       |
//! | HOST-health itself fails                 | log: no success, no change, every streak kept |
//!
//! Silence alerts too, once the check's period and grace run out: the
//! machine is down, hung or cut off. Failed units ride along in the body for
//! whoever looks, but never fail a beat: a job's unit failing isn't the
//! machine failing.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;

/// What modules/heartbeat.nix builds in.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// The unit's name, HOST-heartbeat.
    pub name: String,
    /// The Healthchecks instance's ping endpoint.
    pub url: String,
    /// The check's slug in the ping key's project.
    pub check: String,
    /// How many beats running a health check fails before the beat says so.
    pub fail_after: u32,
    /// Health checks that don't count here.
    pub skip: BTreeSet<String>,
    /// HOST-health.
    pub health: PathBuf,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// What HOST-health said, or how it failed.
pub enum Health {
    Said(String),
    Failed(String),
}

impl Health {
    /// Runs HOST-health and keeps its whole report or none of it: a run that
    /// dies halfway can't say the machine is healthy.
    pub fn run(health: &Path) -> Health {
        let name = health.file_name().map_or_else(
            || health.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        match Command::new(health)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
        {
            Err(e) => Health::Failed(format!("{name} couldn't run ({e})")),
            Ok(out) => match exit_code(out.status) {
                0 => Health::Said(String::from_utf8_lossy(&out.stdout).into_owned()),
                code => Health::Failed(format!("{name} failed (exit {code})")),
            },
        }
    }
}

/// The status as the shell gives it: a signal's is 128 and its number.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// One ping: where on the check it goes and what it says.
#[derive(Debug, PartialEq, Eq)]
pub struct Ping {
    /// `""` for success, `"/fail"` or `"/log"`.
    pub suffix: &'static str,
    pub body: String,
}

/// The ping for what HOST-health said. `streaks` holds, per check, how many
/// beats running it has failed (lost on a reboot, which is fine).
pub fn verdict(cfg: &Config, health: &Health, streaks: &Path) -> Ping {
    let report = match health {
        Health::Failed(why) => {
            return Ping {
                suffix: "/log",
                body: format!("{why}, so this beat can't tell how the machine is"),
            };
        }
        Health::Said(report) => report,
    };
    let (mut pass, mut down, mut blips, mut units) = (0, Vec::new(), Vec::new(), Vec::new());
    // HOST-health prints `check:NAME ok|fail` per check and `unit:NAME fail`
    // or `user-unit:NAME fail` per failed unit.
    for line in report.lines() {
        let line = line.trim();
        let (what, state) = line
            .split_once(char::is_whitespace)
            .map_or((line, ""), |(w, s)| (w, s.trim()));
        if let Some(name) = what.strip_prefix("check:") {
            if cfg.skip.contains(name) {
                continue;
            }
            if state == "ok" {
                pass += 1;
                forget(streaks, name);
            } else if failed_for(streaks, name).is_none_or(|n| n >= cfg.fail_after) {
                // A streak it can't keep can't run out: it counts as down.
                down.push(name);
            } else {
                blips.push(name);
            }
        } else if let Some(unit) = what
            .strip_prefix("unit:")
            .or_else(|| what.strip_prefix("user-unit:"))
        {
            units.push(unit);
        }
    }
    let (suffix, mut body) = if down.is_empty() {
        // A blip still counts as alive: only pings that change nothing would
        // let blips taking turns run the check's grace out with every beat
        // arriving.
        ("", format!("healthy: {pass} checks pass"))
    } else {
        (
            "/fail",
            format!(
                "failing for {} beats or more: {}",
                cfg.fail_after,
                down.join(" ")
            ),
        )
    };
    if !blips.is_empty() {
        body.push_str(&format!("\nfailing for less: {}", blips.join(" ")));
    }
    if !units.is_empty() {
        body.push_str(&format!(
            "\nfailed units (not counted): {}",
            units.join(" ")
        ));
    }
    Ping { suffix, body }
}

/// A check's streak file, for a name that can be a file's.
fn streak(streaks: &Path, name: &str) -> Option<PathBuf> {
    let plain = !name.is_empty() && name != "." && name != ".." && !name.contains('/');
    plain.then(|| streaks.join(name))
}

/// Counts one more failed beat for `name`: how many it has failed running,
/// or None when it can't keep the count.
fn failed_for(streaks: &Path, name: &str) -> Option<u32> {
    let path = streak(streaks, name)?;
    let before = fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let now = before.saturating_add(1);
    match fs::write(&path, format!("{now}\n")) {
        Ok(()) => Some(now),
        Err(e) => {
            eprintln!("{}: {e}, so {name} counts as down", path.display());
            None
        }
    }
}

fn forget(streaks: &Path, name: &str) {
    if let Some(path) = streak(streaks, name) {
        match fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                eprintln!("{}: {e}", path.display());
            }
            _ => {}
        }
    }
}

/// How long one try may take, and the waits before each try again.
pub const TRY_FOR: Duration = Duration::from_secs(15);
pub const AGAIN: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];

/// The agent the beat pings through: the machine's own trust (its CA
/// bundle) and `TRY_FOR` a try.
pub fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};
    let tls = TlsConfig::builder()
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(TRY_FOR))
        .http_status_as_error(false)
        .build()
        .into()
}

/// Sends `ping` to `url/key/check`, trying again after a timeout, a 408, a
/// 429, a 500, 502, 503 or 504, three times at most, as `curl --retry 3`
/// did. The error names neither the URL nor the key (ureq's errors don't),
/// since any user on the machine can read the journal.
pub fn send(
    agent: &ureq::Agent,
    url: &str,
    key: &str,
    check: &str,
    ping: &Ping,
    pause: &mut dyn FnMut(Duration),
) -> Result<(), String> {
    if key.is_empty() {
        return Err("the ping key is empty".to_owned());
    }
    let to = format!("{}/{key}/{check}{}", url.trim_end_matches('/'), ping.suffix);
    let body = format!("{}\n", ping.body);
    let mut waits = AGAIN.iter();
    loop {
        let (again, why) = match agent.post(&to).send(&body) {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) => {
                let code = r.status().as_u16();
                (
                    matches!(code, 408 | 429 | 500 | 502 | 503 | 504),
                    format!("HTTP {code}"),
                )
            }
            Err(e) => (matches!(e, ureq::Error::Timeout(_)), e.to_string()),
        };
        match waits.next() {
            Some(wait) if again => pause(*wait),
            _ => return Err(why),
        }
    }
}
