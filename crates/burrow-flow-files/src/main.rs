//! `burrow-flow-files flow|policy|events FILE...`: each file read as the
//! flow, `cn policy` or conaryd reads it, with Conary's own loaders
//! (`flow::Config::load`, `Policy::load`, and `flow::schema::load` of
//! conaryd's events file, `events::Runtime`), so a key Conary doesn't
//! know, a value of the wrong type or a malformed file is refused here as it
//! would be by the machine's cn. burrow's checks have no Conary flake, so
//! this is how they hold the files its modules render to Conary's types.
use conary_core::Policy;
use conary_github::events::Runtime;
use conary_github::flow::Config;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let kind = args.next();
    let files: Vec<String> = args.collect();
    let read: fn(&Path) -> Result<(), String> = match kind.as_deref() {
        Some("flow") => |p| Config::load(p).map(drop).map_err(|e| e.to_string()),
        Some("policy") => |p| Policy::load(p).map(drop).map_err(|e| e.to_string()),
        Some("events") => |p| {
            let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
            conary_github::flow::schema::load::<Runtime>(&bytes)
                .map(drop)
                .map_err(|e| e.to_string())
        },
        _ => return usage(),
    };
    if files.is_empty() {
        return usage();
    }
    let mut ok = true;
    for file in &files {
        if let Err(e) = read(Path::new(file)) {
            eprintln!("{file}: {e}");
            ok = false;
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: burrow-flow-files flow|policy|events FILE...");
    ExitCode::from(2)
}
