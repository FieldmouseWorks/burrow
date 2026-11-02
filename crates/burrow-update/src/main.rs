//! burrow-update CONFIG [--force | status]: HOST-update (lib.rs), as
//! modules/updates.nix builds it in with its config.
//!
//!   HOST-update            one run; burrow-events starts one for each push
//!                          to main (or a watched input), and hourly
//!   HOST-update --force    the whole loop even if nothing upstream moved,
//!                          with nothing remembered of what failed before
//!   HOST-update status     what the last run did, and what's queued
//!
//! It needs root, for its state and to switch, so one started as anyone
//! else starts again through sudo. A run exits 1 when it failed, which is
//! its own to fix (a Claude job is on it), and 0 otherwise; 2 is a usage or
//! a config it can't take.

use std::env;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};

use burrow_update::{Config, run, status};

/// The word a HOST-update that isn't root starts again with, through sudo.
/// sudo makes it root. Where it doesn't (a runas default, or the tests'
/// stand-in), it runs as whoever it made it, rather than starting again for
/// ever.
const THROUGH_SUDO: &str = "__through-sudo";

#[derive(Debug, PartialEq)]
enum Word {
    Run { force: bool },
    Status,
}

/// What it's asked, after CONFIG: whether it came through sudo, and the
/// word; nothing when that isn't one.
fn parse(words: &[String]) -> Option<(bool, Word)> {
    let (through_sudo, words) = match words {
        [w, rest @ ..] if w == THROUGH_SUDO => (true, rest),
        _ => (false, words),
    };
    let word = match words {
        [] => Word::Run { force: false },
        [w] if w == "--force" => Word::Run { force: true },
        [w] if w == "status" => Word::Status,
        _ => return None,
    };
    Some((through_sudo, word))
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((config, words)) = args.split_first() else {
        eprintln!("usage: burrow-update CONFIG [--force | status]");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-update: {e}");
            return ExitCode::from(2);
        }
    };
    let Some((through_sudo, word)) = parse(words) else {
        eprintln!("usage: {} [--force | status]", cfg.update);
        return ExitCode::from(2);
    };
    if !through_sudo && !rustix::process::geteuid().is_root() {
        let exe = match env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                eprintln!("{}: its own path: {e}", cfg.update);
                return ExitCode::FAILURE;
            }
        };
        let e = Command::new(&cfg.tools.sudo)
            .arg("--")
            .arg(exe)
            .arg(config)
            .arg(THROUGH_SUDO)
            .args(words)
            .exec();
        eprintln!("{}: {}: {e}", cfg.update, cfg.tools.sudo.display());
        return ExitCode::FAILURE;
    }
    match word {
        Word::Status => {
            let mut out = std::io::stdout().lock();
            match out
                .write_all(status::status(&cfg).as_bytes())
                .and_then(|()| out.flush())
            {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("{}: stdout: {e}", cfg.update);
                    ExitCode::FAILURE
                }
            }
        }
        Word::Run { force } => ExitCode::from(run::run(&cfg, &mut cfg.gh(), force)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(words: &[&str]) -> Option<(bool, Word)> {
        parse(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn its_words_are_a_run_forced_or_not_or_its_status() {
        assert_eq!(parsed(&[]), Some((false, Word::Run { force: false })));
        assert_eq!(
            parsed(&["--force"]),
            Some((false, Word::Run { force: true }))
        );
        assert_eq!(parsed(&["status"]), Some((false, Word::Status)));
        // Through sudo, the same words after its own.
        assert_eq!(
            parsed(&[THROUGH_SUDO, "--force"]),
            Some((true, Word::Run { force: true }))
        );
        assert_eq!(
            parsed(&[THROUGH_SUDO]),
            Some((true, Word::Run { force: false }))
        );
        for words in [
            &["go"][..],
            &["status", "--force"],
            &["--force", THROUGH_SUDO],
            &[THROUGH_SUDO, THROUGH_SUDO],
        ] {
            assert_eq!(parsed(words), None, "{words:?}");
        }
    }
}
