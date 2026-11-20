//! burrow-ledger-access DIR USER [CONTROLLER]: see the library's docs.

use std::path::Path;
use std::process::ExitCode;

use burrow_ledger_access::{Access, set, uid_of};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let (dir, user, controller) = match &args[..] {
        [_, dir, user] => (dir, user, None),
        [_, dir, user, controller] => (dir, user, Some(controller)),
        _ => {
            eprintln!(
                "burrow-ledger-access DIR USER [CONTROLLER]: make the ledger in DIR USER's,\n\
                 with the flow's controller let in to sign it, and no one else."
            );
            return ExitCode::from(2);
        }
    };
    let passwd = match std::fs::read_to_string("/etc/passwd") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("/etc/passwd: {e}");
            return ExitCode::FAILURE;
        }
    };
    let uid = |name: &str| uid_of(&passwd, name).ok_or(format!("no user {name}"));
    let access = match (uid(user), controller.map(|c| uid(c)).transpose()) {
        (Ok(owner), Ok(controller)) => Access { owner, controller },
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let failed = set(Path::new(dir), &access, &mut |line| eprintln!("{line}"));
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
