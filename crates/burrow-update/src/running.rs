//! What the machine runs, against what HEAD builds. Every system is stamped
//! with the commit it's built from (its configuration revision), so two
//! builds of the same tree from different commits are different store
//! paths, and the stamp alone doesn't say which commits are live.

use std::fs::{self, File, OpenOptions};
use std::process::{Command, Stdio};

use crate::{Config, last_line};

/// What the machine runs, as update.sh found it when its run began: the
/// system (`current`, a store path) and the commit it's stamped with, when
/// that's one (`rev`, which a build from a dirty checkout ends in -dirty).
pub struct Running {
    pub current: String,
    pub rev: Option<String>,
}

impl Running {
    /// What update.sh says runs: the system, and its stamp, or nothing when
    /// it isn't a commit. A stamp that isn't 40 hex digits, with -dirty or
    /// not, is none: update.sh never says one, and it goes into a Nix
    /// expression as it is.
    pub fn parse(current: &str, rev: &str) -> Option<Running> {
        let commit = rev.strip_suffix("-dirty").unwrap_or(rev);
        let rev = match rev {
            "" => None,
            _ if commit.len() == 40
                && commit
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) =>
            {
                Some(rev.to_string())
            }
            _ => return None,
        };
        Some(Running {
            current: current.to_string(),
            rev,
        })
    }
}

/// What to do with a system that runs already (`runs_already`).
#[derive(Debug, PartialEq, Eq)]
pub enum Already {
    /// A bump of HOST-update's own that it put live itself, through the
    /// switch and the health check: put it up (a PR in a train repo, main
    /// otherwise), once the VM tests pass on HEAD's tree.
    Up,
    /// One someone else put live: hold it until main has it.
    Hold,
    /// Riding its PR, or nothing new: nothing to do.
    Current,
}

impl Already {
    pub fn word(&self) -> &'static str {
        match self {
            Already::Up => "up",
            Already::Hold => "hold",
            Already::Current => "current",
        }
    }
}

/// HEAD's system, built: its store path, or nothing when it doesn't build.
/// What nix says goes to logs/build.log, in place of the last build's.
pub fn build(cfg: &Config) -> Option<String> {
    let log = File::create(cfg.state.join("logs/build.log"))
        .map_err(|e| eprintln!("{}: logs/build.log: {e}", cfg.update))
        .ok()?;
    let toplevel = format!(
        "{}#nixosConfigurations.{}.config.system.build.toplevel",
        cfg.work.display(),
        cfg.configuration
    );
    last_line(
        nix(cfg, log)
            .args(["build", "--no-link", "--print-out-paths", &toplevel])
            .output(),
    )
}

/// Whether HEAD's system is the one running, apart from the commit it's
/// stamped with. (A build from a dirty checkout, then committed, differs
/// only there.) A comparison that can't be made, HEAD unread or the build
/// failed, is no.
pub fn same_as_running(cfg: &Config, run: &Running) -> bool {
    let Ok(head) = cfg.git(&["rev-parse", "HEAD"]) else {
        return false;
    };
    let Ok(log) = OpenOptions::new()
        .append(true)
        .create(true)
        .open(cfg.state.join("logs/build.log"))
    else {
        return false;
    };
    let stamp = run
        .rev
        .as_ref()
        .map_or_else(|| "null".to_string(), |r| format!("\"{r}\""));
    let expr = format!(
        "
    let f = builtins.getFlake \"git+file://{work}?rev={head}\"; in
    (f.nixosConfigurations.{config}.extendModules {{
      modules = [ ({{ lib, ... }}: {{ system.configurationRevision = lib.mkForce {stamp}; }}) ];
    }}).config.system.build.toplevel",
        work = cfg.work.display(),
        config = cfg.configuration,
    );
    let out = nix(cfg, log)
        .args([
            "build",
            "--impure",
            "--no-link",
            "--print-out-paths",
            "--expr",
            &expr,
        ])
        .output();
    last_line(out).is_some_and(|out| out == run.current)
}

/// Whether main, `main` and checked out, is what runs: the system stamped
/// with it, the one a run put live as that main while it still runs, or one
/// that's the same apart from its stamp. Each asks of the system that runs:
/// a run's record of what it put live says nothing once anyone has switched
/// since (a rollback, or a trial boot that fell back).
pub fn main_runs(cfg: &Config, run: &Running, main: &str) -> bool {
    run.rev.as_deref() == Some(main)
        || (cfg.recorded("deployed-main") == main && cfg.recorded("deployed") == run.current)
        || same_as_running(cfg, run)
}

/// Whether `new`, HEAD's system, runs already, and if so what to do with it.
/// It runs already when it's that very system, or, when HEAD is a commit
/// this run made (`made`: a bump, or the merge that rides its PR on main),
/// the same one apart from its stamp. Each run makes that commit afresh, at
/// its own time, so its stamp never matches the one an earlier run put
/// live, even when nothing that runs differs. (Of HEAD as main itself, the
/// run asked before building, unless it's forced.)
///
/// What runs already is put up only once HOST-update put that system live
/// itself (its record in `deployed`), and not while it rides its PR
/// (`riding`).
pub fn runs_already(
    cfg: &Config,
    run: &Running,
    new: &str,
    made: bool,
    riding: bool,
) -> Option<Already> {
    if new != run.current && !(made && same_as_running(cfg, run)) {
        return None;
    }
    Some(if riding || !made {
        Already::Current
    } else if cfg.recorded("deployed") == run.current {
        Already::Up
    } else {
        Already::Hold
    })
}

/// How far main is live once a run has put HEAD live: up to `main`, the
/// main it started from, or, when HEAD is on main as it went live (pushed as
/// it was, or main itself), up to HEAD.
pub fn live_through(cfg: &Config, main: &str) -> String {
    match cfg.git(&["rev-parse", "HEAD"]) {
        Ok(head) if cfg.recorded("deployed-main") == head => head,
        _ => main.to_string(),
    }
}

/// That every commit of main up to `main` is in `running`, the system that
/// runs: what a run found, for burrow-dispatch, which counts main's commits
/// past it as not live yet (live.json). The stamp can't say it: a system
/// that runs already keeps an older commit's, and a ride's is a merge never
/// pushed. It names the system, so a switch to another since, by anyone,
/// leaves it saying nothing. Written whole, or not at all.
pub fn live_at(cfg: &Config, main: &str, running: &str) -> Result<(), String> {
    let live = cfg.state.join("live.json");
    let new = cfg.state.join("live.json.new");
    let text = serde_json::json!({"main": main, "running": running}).to_string() + "\n";
    fs::write(&new, text)
        .and_then(|()| fs::rename(&new, &live))
        .map_err(|e| format!("{}: {e}", live.display()))
}

/// nix as the user, its stderr to `log`.
fn nix(cfg: &Config, log: File) -> Command {
    let mut c = cfg.run_as_user("nix");
    c.stdin(Stdio::null()).stderr(log);
    c
}

#[cfg(test)]
#[path = "running_tests.rs"]
mod tests;
