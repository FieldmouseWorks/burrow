//! burrow-guard CONFIG, as modules/claude.nix's PreToolUse hook runs it:
//! the hook's JSON on stdin, and nothing (allow) or a decision on stdout.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, SystemTime};

use burrow_guard::config::Config;
use burrow_guard::{Guard, Hook, ask};

/// The one answer, whichever comes first: the guard's, or its deadline's.
static ANSWER: Once = Once::new();

fn main() {
    // A guard that can't read what it guards asks, rather than let the
    // command through with no undo and no question.
    let question = check().unwrap_or_else(|e| {
        Some(format!(
            "The guard couldn't check this command ({e}), so it asks."
        ))
    });
    ANSWER.call_once(|| {
        if let Some(q) = question {
            println!("{}", ask(&q));
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

fn check() -> Result<Option<String>, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path] = args.as_slice() else {
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
    Ok(guard.check(&hook, SystemTime::now()))
}
