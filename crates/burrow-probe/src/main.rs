//! `burrow-probe CONFIG`: a pass over the repo's probe issues, which
//! burrow-events runs on the repo's issue events (modules/probe.nix).
//! `burrow-probe CONFIG ask QUESTION`: one answer here, as it is.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use burrow_github::Gh;
use burrow_probe::Config;
use burrow_probe::answer::{Commands, Machine, utc};
use burrow_probe::pass::pass;
use burrow_probe::query::Query;

const USAGE: &str = "usage:
  burrow-probe CONFIG             answer the open probe issues in the config's repo
  burrow-probe CONFIG ask WORDS   answer one question here, on stdout, unscrubbed";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((config, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let cfg: Config = match fs::read_to_string(config)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("burrow-probe: {config}: {e}");
            return ExitCode::from(2);
        }
    };
    let Some(uid) = uid(Path::new("/proc/self/status")) else {
        eprintln!("burrow-probe: /proc/self/status says no uid");
        return ExitCode::FAILURE;
    };
    let now = jiff::Timestamp::now().as_second();
    let m = Machine {
        cfg: &cfg,
        root: PathBuf::from("/"),
        now,
        uid,
        run: &Commands,
    };
    match rest.split_first() {
        None => match pass(&mut Gh::default(), &cfg, &mut |q| m.answer(q), &utc(now)) {
            Ok(done) => {
                for n in &done.answered {
                    eprintln!("answered {}#{n}", cfg.repo);
                }
                for n in &done.refused {
                    eprintln!(
                        "turned away {}#{n}: its author isn't one of {}",
                        cfg.repo,
                        cfg.authors.join(", ")
                    );
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("burrow-probe: {e}");
                ExitCode::FAILURE
            }
        },
        Some((ask, words)) if ask == "ask" => match Query::parse(&words.join(" ")) {
            Ok(q) => match m.answer(&q) {
                Ok(text) => {
                    print!("{text}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("burrow-probe: {q}: {e}");
                    ExitCode::FAILURE
                }
            },
            Err(e) => {
                eprintln!("burrow-probe: {e}");
                ExitCode::from(2)
            }
        },
        Some(_) => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// The real uid, from /proc/self/status (std has no getuid without libc).
fn uid(status: &Path) -> Option<u32> {
    fs::read_to_string(status)
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}
