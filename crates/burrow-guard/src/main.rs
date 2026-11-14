//! burrow-guard [--codex-user USER] CONFIG, as modules/claude/guard.nix's
//! hooks run it: the hook's JSON on stdin, and nothing (allow) or a decision on
//! stdout. Claude Code's asks the owner; Codex's (`--codex-user`) refuses,
//! since a Codex job has no one to ask and Codex runs what its hook asks
//! about. What has a safe spelling of its own both refuse
//! ([`Guard::refusal`]).

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
    let decision = check(&args).unwrap_or_else(|e| {
        Some(answer(&format!(
            "The guard couldn't check this command ({e}), so it asks."
        )))
    });
    ANSWER.call_once(|| {
        if let Some(d) = decision {
            println!("{d}");
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

fn check(args: &[String]) -> Result<Option<Value>, String> {
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
        mark: runtime.join("claude-guard-last-snapshot"),
        namespace: Namespace::here(),
    };
    if let Some(reason) = guard.refusal(&hook) {
        return Ok(Some(deny(&reason)));
    }
    Ok(guard.check(&hook, SystemTime::now()).map(|q| answer(&q)))
}
