//! burrow-firmware CONFIG [--review]: firmware-check (lib.rs), as
//! modules/updates.nix builds it in with its config.
//!
//!   firmware-check            daily: what fwupd (LVFS) offers for the machine
//!   firmware-check --review   monthly: a Claude job looks at what LVFS
//!                             doesn't carry (board BIOS, NVMe, Intel ME or
//!                             AMD PSP)
//!
//! It needs root, for dmidecode and fwupd's state, so one started as anyone
//! else starts again through sudo. It exits 1 when something it needed
//! broke, and 2 on a usage or a config it can't take.

use std::env;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};

use burrow_firmware::{Broke, Config, check, review};

/// The word a firmware-check that isn't root starts again with, through
/// sudo. sudo makes it root. Where it doesn't (a runas default, or the
/// tests' stand-in), it runs as whoever it made it, rather than starting
/// again for ever.
const THROUGH_SUDO: &str = "__through-sudo";

#[derive(Debug, PartialEq, Eq)]
enum Word {
    Check,
    Review,
}

/// What it's asked, after CONFIG: whether it came through sudo, and the
/// word; nothing when that isn't one.
fn parse(words: &[String]) -> Option<(bool, Word)> {
    let (through_sudo, words) = match words {
        [w, rest @ ..] if w == THROUGH_SUDO => (true, rest),
        _ => (false, words),
    };
    let word = match words {
        [] => Word::Check,
        [w] if w == "--review" => Word::Review,
        _ => return None,
    };
    Some((through_sudo, word))
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((config, words)) = args.split_first() else {
        eprintln!("usage: burrow-firmware CONFIG [--review]");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-firmware: {e}");
            return ExitCode::from(2);
        }
    };
    let Some((through_sudo, word)) = parse(words) else {
        eprintln!("usage: {} [--review]", cfg.name);
        return ExitCode::from(2);
    };
    if !through_sudo && !rustix::process::geteuid().is_root() {
        let exe = match env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                eprintln!("{}: its own path: {e}", cfg.name);
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
        eprintln!("{}: {}: {e}", cfg.name, cfg.tools.sudo.display());
        return ExitCode::FAILURE;
    }
    let done = match word {
        Word::Check => check::run(&cfg),
        Word::Review => review::run(&cfg),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(Broke(why)) => {
            eprintln!("{}: {why}", cfg.name);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(words: &[&str]) -> Option<(bool, Word)> {
        parse(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn its_words_are_none_or_review() {
        assert_eq!(parsed(&[]), Some((false, Word::Check)));
        assert_eq!(parsed(&["--review"]), Some((false, Word::Review)));
        // Through sudo, the same words after its own.
        assert_eq!(parsed(&[THROUGH_SUDO]), Some((true, Word::Check)));
        assert_eq!(
            parsed(&[THROUGH_SUDO, "--review"]),
            Some((true, Word::Review))
        );
        for words in [
            &["review"][..],
            &["--review", "now"],
            &["check"],
            &[""],
            &[THROUGH_SUDO, THROUGH_SUDO],
            &["--review", THROUGH_SUDO],
        ] {
            assert_eq!(parsed(words), None, "{words:?}");
        }
    }
}
