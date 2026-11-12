//! `burrow-guidance [ROOT]`: every Markdown file under ROOT (the current
//! directory when there's none) held to the owned facts; each finding is a
//! line, and any fails it. A tree with house/ has its AGENTS.md held to it
//! too (`house`).

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = match args.as_slice() {
        [] => PathBuf::from("."),
        [root] => PathBuf::from(root),
        _ => {
            eprintln!("usage: burrow-guidance [ROOT]");
            return ExitCode::FAILURE;
        }
    };
    match burrow_guidance::check(&root, &burrow_guidance::facts()) {
        Ok(found) if found.is_empty() => ExitCode::SUCCESS,
        Ok(found) => {
            for f in &found {
                println!("{f}");
            }
            eprintln!(
                "burrow-guidance: {} fact(s) stated outside their owner, or copied other than it has them",
                found.len()
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("burrow-guidance: {e}");
            ExitCode::FAILURE
        }
    }
}
