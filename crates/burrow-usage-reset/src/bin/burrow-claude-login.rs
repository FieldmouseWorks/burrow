//! burrow-claude-login CLAUDE USER [--meters DIR]: exits 0 when Claude Code
//! can authenticate as USER, the user running it (a live reading of its usage
//! meter, claude::check_login), and says why not on stderr otherwise: to log
//! in again only when the login itself failed. With --meters DIR, every live
//! reading is also kept there for proj and usage-reset. HOST-health's
//! claude-login check.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use burrow_usage_reset::claude::{Unshown, check_login};
use burrow_usage_reset::{meter, now};

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let (Some(claude), Some(user)) = (args.next().map(PathBuf::from), args.next()) else {
        eprintln!("usage: burrow-claude-login CLAUDE USER [--meters DIR]");
        return ExitCode::from(2);
    };
    let meters = match (args.next().as_deref(), args.next(), args.next()) {
        (None, None, None) => None,
        (Some(flag), Some(dir), None) if flag == "--meters" => Some(PathBuf::from(dir)),
        _ => {
            eprintln!("usage: burrow-claude-login CLAUDE USER [--meters DIR]");
            return ExitCode::from(2);
        }
    };
    let user = user.to_string_lossy();
    // A folder of its own, so Claude Code's project state lands nowhere shared:
    // created exclusively and private (0700), so nobody else's directory is
    // ever adopted, and removed on drop, only if this made it.
    let result = tempfile::Builder::new()
        .prefix("burrow-claude-login-")
        .tempdir()
        .map_err(|e| Unshown::Unknown(format!("a temporary folder: {e}")))
        .and_then(|dir| check_login(&claude, dir.path()));
    match result {
        Ok(checked) => {
            if let (Some(checked), Some(meters)) = (checked, meters) {
                let mut snapshot = meter::claude(&checked.reading, now());
                snapshot.requested_at = Some(checked.requested_at);
                if let Err(e) = snapshot.record(&meters, "claude") {
                    eprintln!("couldn't record Claude's meter: {e}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(Unshown::LoginFailed(why)) => {
            eprintln!(
                "burrow-claude-login: Claude Code can't authenticate as {user}: its login expired, was revoked or is missing ({why}). Log in again on this machine as {user} (run claude, then /login)."
            );
            ExitCode::FAILURE
        }
        Err(Unshown::Unknown(why)) => {
            eprintln!("burrow-claude-login: {why}");
            ExitCode::FAILURE
        }
    }
}
