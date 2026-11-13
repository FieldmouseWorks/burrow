//! burrow-claude-login CLAUDE: exits 0 when Claude Code can authenticate as
//! the user running it (a live reading of its usage meter, claude::check_login),
//! and says why not on stderr otherwise. HOST-health's claude-login check.

use std::path::PathBuf;
use std::process::ExitCode;
use std::{env, fs, process};

use burrow_usage_reset::claude::check_login;

fn main() -> ExitCode {
    let Some(claude) = env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: burrow-claude-login CLAUDE");
        return ExitCode::from(2);
    };
    // A folder of its own, so Claude Code's project state lands nowhere shared.
    let dir = env::temp_dir().join(format!("burrow-claude-login-{}", process::id()));
    let result = fs::create_dir_all(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))
        .and_then(|()| check_login(&claude, &dir));
    let _ = fs::remove_dir_all(&dir);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-claude-login: {e}");
            ExitCode::FAILURE
        }
    }
}
