//! burrow-guard [--codex-user USER] CONFIG, as modules/claude/guard.nix's
//! hooks run it: the hook's JSON on stdin, and nothing (allow) or a decision on
//! stdout. Claude Code's asks the owner; Codex's (`--codex-user`) refuses,
//! since a Codex job has no one to ask and Codex runs what its hook asks
//! about.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};
use std::time::{Duration, SystemTime};

use burrow_guard::config::{Config, current_user};
use burrow_guard::{Guard, Hook, Namespace, ask, deny};
use serde_json::Value;

/// The one answer, whichever comes first: the guard's, or its deadline's.
static ANSWER: Once = Once::new();

/// Whether this is Codex's hook, read before anything else: a guard that
/// can't read its config must still answer as its engine needs.
static CODEX: OnceLock<bool> = OnceLock::new();

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let _ = CODEX.set(args.first().is_some_and(|a| a == "--codex-user"));
    // A guard that can't read what it guards asks (or, Codex's, refuses),
    // rather than let the command through with no undo and no question.
    let question = check(&args).unwrap_or_else(|e| {
        Some(format!(
            "The guard couldn't check this command ({e}), so it asks."
        ))
    });
    ANSWER.call_once(|| {
        if let Some(q) = question {
            println!("{}", answer(&q));
        }
    });
}

/// The decision that `question` is, for this guard's engine.
fn answer(question: &str) -> Value {
    if CODEX.get().copied().unwrap_or(true) {
        deny(question)
    } else {
        ask(question)
    }
}

/// Past `seconds`, the answer is a question, whatever the guard is stuck on
/// (a long command, git or zfs hanging, a loaded machine): the hook's timeout
/// would let the command run unchecked.
fn ask_after(seconds: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(seconds));
        ANSWER.call_once(|| {
            let q = format!(
                "The guard couldn't check this command (no answer after {seconds} s), so it asks."
            );
            println!("{}", answer(&q));
            std::process::exit(0);
        });
    });
}

fn check(args: &[String]) -> Result<Option<String>, String> {
    let (user, path) = match args {
        [path] => (None, path),
        [flag, user, path] if flag == "--codex-user" => (Some(user), path),
        _ => return Err("usage: burrow-guard [--codex-user USER] CONFIG < HOOK-JSON".to_owned()),
    };
    let config = Config::load(Path::new(path))?;
    ask_after(config.ask_after_seconds);
    // Codex's hook is every Codex's on the machine; only the jobs' user's
    // commands are guarded (the reviewer's run read-only, and the undo
    // snapshot isn't theirs to take). A user it can't name is guarded.
    if let Some(user) = user
        && current_user().is_some_and(|me| me != *user)
    {
        return Ok(None);
    }
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|e| format!("stdin: {e}"))?;
    let hook: Hook =
        serde_json::from_slice(&input).map_err(|e| format!("the hook's input: {e}"))?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let guard = Guard {
        config: &config,
        codex: user.is_some(),
        job: in_a_job(),
        mark: runtime.join("claude-guard-last-snapshot"),
        namespace: Namespace::here(),
    };
    Ok(guard.check(&hook, SystemTime::now()))
}

/// Whether this guard runs for a proj job, whichever engine runs it: proj
/// names the job (PROJ_JOB) in its engine's environment, which the engine's
/// hook inherits. The jobs' user runs Codex and Claude Code of its own too,
/// the owner's, which proj didn't start and which name none. Where the
/// guard's environment says nothing (an engine that gives its hook an
/// environment of its own), the processes it runs under say: the engine's
/// own environment, as proj started it, which no command of the job's can
/// change.
fn in_a_job() -> bool {
    if let Some(job) = std::env::var_os("PROJ_JOB") {
        return !job.is_empty();
    }
    let mut pid = std::os::unix::process::parent_id();
    // Far more than any engine's hook runs under.
    for _ in 0..64 {
        if pid <= 1 {
            return false;
        }
        let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
        if let Some(job) = environ
            .split(|b| *b == 0)
            .find_map(|var| var.strip_prefix(b"PROJ_JOB="))
        {
            return !job.is_empty();
        }
        let Some(parent) = parent_of(pid) else {
            return false;
        };
        pid = parent;
    }
    false
}

/// A process's parent, from /proc: the field after its state, past its
/// name, which may hold anything but ends at the last `)`.
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = &stat[stat.rfind(')')? + 1..];
    after_name.split_whitespace().nth(1)?.parse().ok()
}
