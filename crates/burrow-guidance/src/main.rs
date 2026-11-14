//! `burrow-guidance [ROOT]`: every Markdown file under ROOT (the current
//! directory when there's none) held to the owned facts; each finding is a
//! line, and any fails it. A tree with house/ has its AGENTS.md held to it
//! too (`house`).
//!
//! `burrow-guidance paths [--burrow DIR] [ROOT]`: every path ROOT's
//! Markdown cites is there (`paths`), burrow's against DIR, the burrow ROOT
//! pins; without it, ROOT is burrow.

use std::path::PathBuf;
use std::process::ExitCode;

use burrow_guidance::paths::{self, Trees};

const USAGE: &str = "usage: burrow-guidance [ROOT] | burrow-guidance paths [--burrow DIR] [ROOT]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, rest @ ..] if cmd == "paths" => cited(rest),
        [] => owned(PathBuf::from(".")),
        [root] => owned(PathBuf::from(root)),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn owned(root: PathBuf) -> ExitCode {
    report(
        burrow_guidance::check(&root, &burrow_guidance::facts()),
        "fact(s) stated outside their owner, or copied other than it has them",
    )
}

fn cited(args: &[String]) -> ExitCode {
    let (burrow, root) = match args {
        [flag, burrow, rest @ ..] if flag == "--burrow" => (Some(PathBuf::from(burrow)), rest),
        rest => (None, rest),
    };
    let root = match root {
        [] => PathBuf::from("."),
        [root] => PathBuf::from(root),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let burrow = burrow.unwrap_or_else(|| root.clone());
    report(
        paths::check(&Trees {
            own: &root,
            burrow: &burrow,
        }),
        "cited path(s) that aren't there",
    )
}

fn report<F: std::fmt::Display>(found: Result<Vec<F>, String>, what: &str) -> ExitCode {
    match found {
        Ok(found) if found.is_empty() => ExitCode::SUCCESS,
        Ok(found) => {
            for f in &found {
                println!("{f}");
            }
            eprintln!("burrow-guidance: {} {what}", found.len());
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("burrow-guidance: {e}");
            ExitCode::FAILURE
        }
    }
}
