//! burrow-claude-login CLAUDE: exits 0 when Claude Code can authenticate as
//! the user running it (a live reading of its usage meter, claude::check_login),
//! and says why not on stderr otherwise. HOST-health's claude-login check.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use burrow_usage_reset::claude::check_login;

fn main() -> ExitCode {
    let Some(claude) = env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: burrow-claude-login CLAUDE");
        return ExitCode::from(2);
    };
    // A folder of its own, so Claude Code's project state lands nowhere shared:
    // created exclusively and private (0700), so nobody else's directory is
    // ever adopted, and removed on drop, only if this made it.
    let result = tempfile::Builder::new()
        .prefix("burrow-claude-login-")
        .tempdir()
        .map_err(|e| format!("a temporary folder: {e}"))
        .and_then(|dir| check_login(&claude, dir.path()));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-claude-login: {e}");
            ExitCode::FAILURE
        }
    }
}
