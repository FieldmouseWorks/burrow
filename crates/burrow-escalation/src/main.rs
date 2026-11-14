//! `burrow-escalation CONFIG ...`, which is HOST-escalation once
//! modules/escalation.nix builds the config in:
//!
//! ```text
//! HOST-escalation route KIND DETAIL
//! HOST-escalation escalate KEY OWNER/NAME#N KIND DETAIL ASK
//! HOST-escalation pass
//! HOST-escalation show
//! ```
//!
//! `route` says where a request would go and changes nothing. `escalate`
//! sends it there: the operator's, or the owner's through their project and
//! then the owner label (crate docs). `pass` carries on every escalation a
//! failure or restart left part-way. `show` prints the records. Each prints
//! JSON; it exits 0 with an outcome, 1 when a step failed (the escalation
//! kept, for the next call or pass to carry on) and 2 on a usage error.

use std::env;
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use burrow_escalation::config::Config;
use burrow_escalation::reason::{Policy, Route};
use burrow_escalation::relay::{GitHub, GitHubLabels};
use burrow_escalation::{
    Dir, Error, Escalations, Project, Receipt, Records, RelayRef, Request, Target,
};
use burrow_github::Gh;
use serde_json::json;

const USAGE: &str = "route KIND DETAIL | escalate KEY OWNER/NAME#N KIND DETAIL ASK | pass | show";

/// Set in what it runs as the user.
const AS_USER: &str = "BURROW_ESCALATION_AS_USER";

fn main() -> ExitCode {
    let raw: Vec<OsString> = env::args_os().skip(1).collect();
    let Some(args) = raw
        .iter()
        .map(|a| a.clone().into_string().ok())
        .collect::<Option<Vec<String>>>()
    else {
        eprintln!("burrow-escalation: its words must be text");
        return ExitCode::from(2);
    };
    let Some((config, words)) = args.split_first() else {
        eprintln!("usage: burrow-escalation CONFIG {USAGE}");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-escalation: {e}");
            return ExitCode::from(2);
        }
    };
    let policy = match cfg.policy() {
        Ok(policy) => policy,
        Err(e) => {
            eprintln!("{}: {e}", cfg.name);
            return ExitCode::from(2);
        }
    };
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    // `route` reads nothing but the config, so anyone may ask it.
    if let ["route", kind, detail] = words.as_slice() {
        return route(&policy, kind, detail);
    }
    if !matches!(
        words.as_slice(),
        ["escalate", _, _, _, _, _] | ["pass"] | ["show"]
    ) {
        eprintln!("usage: {} {USAGE}", cfg.name);
        return ExitCode::from(2);
    }
    // The records and gh's login are the user's: anyone else runs it again
    // as them, or fails.
    if whoami().as_deref() != Some(cfg.user.as_str()) {
        if env::var_os(AS_USER).is_some() {
            eprintln!("{}: runuser didn't make it {}", cfg.name, cfg.user);
            return ExitCode::from(1);
        }
        eprintln!("{}: {}", cfg.name, as_user(&cfg, &raw));
        return ExitCode::from(1);
    }
    let mut records = Dir(cfg.dir.clone());
    if words == ["show"] {
        return show(&cfg, &mut records);
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut gh = Gh::default();
    let mut gh_labels = Gh::default();
    // With no relay, nothing new reaches the owner (the policy routes it to
    // the operator), and a record an earlier relay left part-way fails each
    // pass, saying so, rather than be read against another route.
    let mut none = NoRoute;
    let mut github;
    let project: &mut dyn Project = match &cfg.relay {
        Some(relay) => {
            github = GitHub {
                api: &mut gh,
                relay,
                owner: &cfg.owner,
                owner_label: &cfg.owner_label,
            };
            &mut github
        }
        None => &mut none,
    };
    let mut labels = GitHubLabels {
        api: &mut gh_labels,
    };
    let mut escalations = Escalations {
        policy: &policy,
        owner_label: &cfg.owner_label,
        records: &mut records,
        project,
        labels: &mut labels,
        now,
    };
    match words.as_slice() {
        ["escalate", key, target, kind, detail, ask] => {
            let target = match Target::parse(target) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("{}: {e}", cfg.name);
                    return ExitCode::from(2);
                }
            };
            match escalations.escalate(key, &target, kind, detail, ask) {
                Ok(outcome) => {
                    let mut out = outcome.json();
                    out["key"] = json!(key);
                    println!("{out}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    println!("{}", json!({"outcome": "failed", "key": key, "why": e}));
                    ExitCode::from(1)
                }
            }
        }
        _ => match escalations.pass() {
            Ok(all) => {
                let mut code = ExitCode::SUCCESS;
                for (key, outcome) in all {
                    let out = match outcome {
                        Ok(o) => {
                            let mut out = o.json();
                            out["key"] = json!(key);
                            out
                        }
                        Err(e) => {
                            code = ExitCode::from(1);
                            json!({"outcome": "failed", "key": key, "why": e})
                        }
                    };
                    println!("{out}");
                }
                code
            }
            Err(e) => {
                eprintln!("{}: {e}", cfg.name);
                ExitCode::from(1)
            }
        },
    }
}

/// The route when none is configured.
struct NoRoute;

impl Project for NoRoute {
    fn send(&mut self, request: &Request) -> Result<RelayRef, Error> {
        Err(format!(
            "{}: no relay is configured to reach the owner's project",
            request.key
        ))
    }

    fn acknowledged(&mut self, key: &str, relay: &RelayRef) -> Result<Option<Receipt>, Error> {
        Err(format!(
            "{key}: its request {}#{} waits on a relay that's no longer configured",
            relay.repo, relay.number
        ))
    }
}

fn route(policy: &Policy, kind: &str, detail: &str) -> ExitCode {
    let out = match policy.route(kind, detail) {
        Route::Owner(reason) => json!({"route": "owner", "reason": reason}),
        Route::Operator(why) => json!({"route": "operator", "why": why}),
    };
    println!("{out}");
    ExitCode::SUCCESS
}

fn show(cfg: &Config, records: &mut Dir) -> ExitCode {
    let shown = records.keys().and_then(|keys| {
        keys.iter()
            .filter_map(|k| records.load(k).transpose())
            .map(|r| r.map(|r| r.json()))
            .collect::<Result<Vec<_>, _>>()
    });
    match shown {
        Ok(all) => {
            for r in all {
                println!("{r}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}: {e}", cfg.name);
            ExitCode::from(1)
        }
    }
}

fn whoami() -> Option<String> {
    let out = Command::new("id")
        .arg("-un")
        .stderr(Stdio::inherit())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Runs this again, the same words and all, as the user. Returns only if it
/// can't (only root may).
fn as_user(cfg: &Config, args: &[OsString]) -> String {
    let exe = match env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return format!("its own path: {e}"),
    };
    let mut home = OsString::from("HOME=");
    home.push(&cfg.home);
    let e = Command::new("runuser")
        .args(["-u", &cfg.user, "--", "env"])
        .arg(home)
        .arg(format!("{AS_USER}=1"))
        .arg(exe)
        .args(args)
        .exec();
    format!("runs as {}, and runuser couldn't make it so: {e}", cfg.user)
}
