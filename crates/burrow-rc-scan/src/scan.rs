//! The scan, and the two words it is started with after it.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::decide::{self, Folder, Server};
use crate::machine::{self, Tools};
use crate::{Broke, Config};

const SCAN: &str = "claude-rc-scan.service";
const TIMER: &str = "claude-rc-scan.timer";

/// What the scan runs in: the environment systemd gives its units.
pub struct Env {
    /// ~/.claude/sessions' parent.
    pub home: PathBuf,
    /// $XDG_RUNTIME_DIR.
    pub runtime: PathBuf,
    /// $INVOCATION_ID: this run of the unit.
    pub invocation: String,
}

impl Env {
    pub fn from_env() -> Result<Env, Broke> {
        let var = |name: &str| std::env::var(name).map_err(|_| Broke(format!("{name} isn't set")));
        Ok(Env {
            home: PathBuf::from(var("HOME")?),
            runtime: PathBuf::from(var("XDG_RUNTIME_DIR")?),
            invocation: var("INVOCATION_ID")?,
        })
    }

    fn sessions(&self) -> PathBuf {
        self.home.join(".claude/sessions")
    }

    fn notes(&self) -> PathBuf {
        self.runtime.join("claude-rc-scan")
    }
}

/// The text of a note, or nothing: what `$(cat NOTE 2>/dev/null || true)`
/// gave, which has no trailing newline.
fn read_note(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    text.trim_end_matches('\n').to_string()
}

/// `claude-rc-scan done`: as the scan stops, failed or not, ask for the look
/// after it. Never returns on success.
pub fn done(cfg: &Config, env: &Env) -> Broke {
    let err = Command::new(&cfg.tools.systemctl)
        .args(["--user", "start", "--no-block"])
        .arg(format!("claude-rc-again@{}.service", env.invocation))
        .exec();
    Broke(format!("run {}: {err}", cfg.tools.systemctl.display()))
}

/// `claude-rc-scan again ID`: scan again if anything changed since scan ID
/// began or a spell it noted is over.
pub fn again(cfg: &Config, env: &Env, id: &str) -> Result<(), Broke> {
    let note = env.notes().join(id);
    let seen = note.with_extension("seen");
    let wake = note.with_extension("wake");
    let (before, wake_at) = (read_note(&seen), read_note(&wake));
    let over = wake_at.parse::<i64>().is_ok_and(|w| w <= machine::now_ms());
    let changed =
        machine::seen(&env.sessions(), &cfg.projects_dir).trim_end_matches('\n') != before;
    if changed || over {
        Tools { cfg }.no_block("start", OsStr::new(SCAN))?;
    }
    let _ = fs::remove_file(&seen);
    let _ = fs::remove_file(&wake);
    Ok(())
}

/// `only GLOB UNIT...`: start each UNIT, and stop every other GLOB matches.
fn only(tools: &Tools, glob: &str, keep: &[OsString]) -> Result<(), Broke> {
    for unit in keep {
        tools.no_block("start", unit)?;
    }
    let keep: HashSet<&OsString> = keep.iter().collect();
    for unit in tools.units(glob) {
        if !keep.contains(&unit) {
            tools.no_block("stop", &unit)?;
        }
    }
    Ok(())
}

/// A scan: start a server for every project, stop the ones whose folder is
/// gone or no longer a project, and restart the stale ones once they hold no
/// session.
pub fn scan(cfg: &Config, env: &Env) -> Result<(), Broke> {
    let tools = Tools { cfg };
    let sessions = env.sessions();
    let notes = env.notes();
    fs::create_dir_all(&notes)?;
    let note = notes.join(&env.invocation);
    fs::write(
        note.with_extension("seen"),
        machine::seen(&sessions, &cfg.projects_dir),
    )?;

    // What makes a project: decide::folder.
    let flake = machine::realpath_m(&cfg.flake);
    let (mut want, mut waiting) = (Vec::new(), Vec::new());
    for (name, dir) in machine::folders(&cfg.projects_dir) {
        let is_flake = fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone()) == flake;
        let origin = || tools.has_origin(&dir.join(".git/config"));
        match decide::folder(is_flake, machine::git_of(&dir), origin) {
            Folder::Project => want.push(tools.escape("claude-rc@.service", &name)?),
            Folder::Waiting => waiting.push(tools.escape("claude-rc-origin@.path", &name)?),
            Folder::Neither => {}
        }
    }
    only(&tools, "claude-rc@*.service", &want)?;
    only(&tools, "claude-rc-origin@*.path", &waiting)?;

    // A switch leaves running servers alone (restartIfChanged), so a server
    // keeps the tmux and the claude-rc-serve it started with: its Claude
    // Code, its permission mode, where its sessions run. One that runs any
    // other is stale, and restarts here once none of its sessions is in use.
    // A session (a child of its Claude) stays open as long as claude.ai keeps
    // it, so being open is not being in use: Claude Code's
    // ~/.claude/sessions/PID.json says what it's doing, and only one that is
    // working, or did something in the last idle_minutes, holds the server.
    // Nobody is cut off mid-turn, and an open but idle session doesn't keep a
    // stale server forever. Its transcript stays on disk, and claude.ai
    // resumes it on the new one.
    //
    // A held server's scan comes again when what holds it changes: a
    // session's status (the path unit on ~/.claude/sessions), or the end of
    // its quiet spell, which this scan books (claude-rc-scan.timer), at the
    // latest of its sessions' and the soonest of the servers'. One that is
    // working, or says nothing, is looked at again a quiet spell from now: it
    // may end without a word.
    let now = machine::now_ms();
    let quiet = cfg.idle_minutes as i64 * 60_000;
    let (tmux, serve) = (cfg.tmux.to_string_lossy(), cfg.serve.to_string_lossy());
    let mut outcomes = Vec::new();
    for unit in &want {
        let Some(pid) = tools.main_pid(unit) else {
            continue;
        };
        let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        if decide::current(&cmdline, &tmux, &serve) {
            continue;
        }
        let mut holds = Vec::new();
        for (rc, _) in machine::children(pid) {
            // Any state but Z: a zombie is a session that already ended.
            for (session, state) in machine::children(rc) {
                if decide::live(state) {
                    // Two reads, as each is a `jq` of its own.
                    let file = sessions.join(format!("{session}.json"));
                    let status = tools.jq(r#".status // """#, &file);
                    let at = tools.jq(".statusUpdatedAt // 0", &file);
                    holds.push(decide::hold(status.as_deref(), at.as_deref(), now, quiet));
                }
            }
        }
        match decide::server(holds.clone()) {
            Server::Restart => tools.no_block("restart", unit)?,
            held => outcomes.push(held),
        }
    }

    // The wake-up: a timer of its own for this unit, gone once it fires.
    let wake = decide::wake(outcomes);
    tools.stop_quietly(TIMER);
    let text = wake.map_or(String::new(), |w| w.to_string());
    fs::write(note.with_extension("wake"), format!("{text}\n"))?;
    if let Some(wake) = wake {
        // From the clock now, not the scan's start: a slow scan books no
        // later. A spell over already is the look after this scan's to see.
        if let Some(left) = decide::left(wake, machine::now_ms()) {
            tools.book(left)?;
        }
    }
    Ok(())
}
