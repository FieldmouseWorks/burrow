//! burrow-guard CONFIG, as modules/engines/anthropic/guard.nix's hook runs it: the
//! hook's JSON on stdin, and nothing (allow) or a decision on stdout: a
//! question for the owner, or a refusal for what has a safe spelling of its
//! own ([`Guard::refusal`]).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, SystemTime};

use burrow_guard::config::Config;
use burrow_guard::{Guard, Hook, ask, deny};
use serde_json::Value;

/// The one answer, whichever comes first: the guard's, or its deadline's.
static ANSWER: Once = Once::new();

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // A guard that can't read what it guards asks, rather than let the command through with no undo and no question.
    let decision = check(&args).unwrap_or_else(|e| {
        Some(ask(&format!(
            "The guard couldn't check this command ({e}), so it asks."
        )))
    });
    ANSWER.call_once(|| {
        if let Some(d) = decision {
            println!("{d}");
        }
    });
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
            println!("{}", ask(&q));
            std::process::exit(0);
        });
    });
}

fn check(args: &[String]) -> Result<Option<Value>, String> {
    let [path] = args else {
        return Err("usage: burrow-guard CONFIG < HOOK-JSON".to_owned());
    };
    let config = Config::load(Path::new(path))?;
    ask_after(config.ask_after_seconds);
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
        mark: runtime.join("claude-guard-last-snapshot"),
    };
    if let Some(reason) = guard.refusal(&hook) {
        return Ok(Some(deny(&reason)));
    }
    Ok(guard.check(&hook, SystemTime::now()).map(|q| ask(&q)))
}
