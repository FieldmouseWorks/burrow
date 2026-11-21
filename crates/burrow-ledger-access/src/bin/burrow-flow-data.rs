//! burrow-flow-data DATA USER OTHER: carry the flow's state into DATA,
//! USER's, from OTHER, where it was kept before. See the library's `flow`.

use std::path::Path;
use std::process::ExitCode;

use burrow_ledger_access::flow::{Carried, Owner, Refused, carry};
use burrow_ledger_access::ids_of;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let [_, data, user, other] = &args[..] else {
        eprintln!(
            "burrow-flow-data DATA USER OTHER: carry the flow's state into DATA, USER's,\n\
             from OTHER, where it was kept before; refused when both hold some."
        );
        return ExitCode::from(2);
    };
    let passwd = match std::fs::read_to_string("/etc/passwd") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("/etc/passwd: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some((uid, gid)) = ids_of(&passwd, user) else {
        eprintln!("no user {user}");
        return ExitCode::FAILURE;
    };
    match carry(Path::new(data), Path::new(other), Owner { uid, gid }) {
        Ok(done) => {
            match done {
                Carried::Kept => {}
                Carried::Made => println!("{data}: made, {user}'s"),
                Carried::Moved => {
                    println!("{data}: the flow's state carried from {other}, {user}'s")
                }
                Carried::Handed => println!("{data}: the flow's state handed to {user}"),
            }
            ExitCode::SUCCESS
        }
        Err(e @ Refused::Busy(_)) => {
            eprintln!("{e}");
            // EX_TEMPFAIL: again once the process has exited.
            ExitCode::from(75)
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
