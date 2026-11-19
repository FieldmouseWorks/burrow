//! `burrow-brief texts --owner OWNER`: the fix job's and the judge's
//! policy text, as one JSON object by name, for the steward's tasks
//! (modules/autoland/tasks.py), which hold no copy of it.
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["texts", "--owner", owner] if !owner.trim().is_empty() => {
            println!("{}", burrow_brief::texts(owner));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: burrow-brief texts --owner OWNER");
            ExitCode::from(2)
        }
    }
}
