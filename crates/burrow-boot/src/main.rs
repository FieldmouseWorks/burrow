//! burrow-boot CONFIG WORD: HOST-boot (lib.rs), as modules/switch.nix builds
//! it in with its config.
//!
//!   HOST-boot default SYSTEM   make SYSTEM the boot default if its boot bits
//!                              came up here before; otherwise keep the
//!                              default and queue a trial (HOST-confirm says
//!                              this)
//!   HOST-boot trial [--force]  reboot into the queued trial once, if the box
//!                              is idle (--force: whatever jobs and sandboxes
//!                              are running)
//!   HOST-boot check            after a boot: settle the trial, remember what
//!                              came up
//!   HOST-boot [status]
//!
//! It needs root, for its state and GRUB's, so one started as anyone else
//! starts again through sudo. It exits 1 when something it needed broke, and
//! 2 on a usage or a config it can't take.

use std::env;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use burrow_boot::Config;
use burrow_boot::boot::{self, Word};

/// The word a HOST-boot that isn't root starts again with, through sudo.
/// sudo makes it root. Where it doesn't (a runas default, or the tests'
/// stand-in), it runs as whoever it made it, rather than starting again for
/// ever.
const THROUGH_SUDO: &str = "__through-sudo";

/// What it's asked, after CONFIG: whether it came through sudo, and the
/// word; nothing when that isn't one.
fn parse(words: &[String]) -> Option<(bool, Word)> {
    let (through_sudo, words) = match words {
        [w, rest @ ..] if w == THROUGH_SUDO => (true, rest),
        _ => (false, words),
    };
    let word = match words {
        [] => Word::Status,
        [w] if w == "status" => Word::Status,
        [w] if w == "check" => Word::Check,
        [w] if w == "trial" => Word::Trial { force: false },
        [w, f] if w == "trial" && f == "--force" => Word::Trial { force: true },
        [w, system] if w == "default" && !system.is_empty() => Word::Default(PathBuf::from(system)),
        _ => return None,
    };
    Some((through_sudo, word))
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((config, words)) = args.split_first() else {
        eprintln!("usage: burrow-boot CONFIG WORD");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-boot: {e}");
            return ExitCode::from(2);
        }
    };
    let Some((through_sudo, word)) = parse(words) else {
        eprintln!(
            "usage: {} default SYSTEM | trial [--force] | check | status",
            cfg.boot
        );
        return ExitCode::from(2);
    };
    if !through_sudo && !rustix::process::geteuid().is_root() {
        let exe = match env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                eprintln!("{}: its own path: {e}", cfg.boot);
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
        eprintln!("{}: {}: {e}", cfg.boot, cfg.tools.sudo.display());
        return ExitCode::FAILURE;
    }
    match boot::run(&cfg, &word) {
        Ok(code) => ExitCode::from(code),
        Err(burrow_boot::Broke(why)) => {
            eprintln!("{}: {why}", cfg.boot);
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
    fn its_words_are_default_trial_check_and_status() {
        assert_eq!(parsed(&[]), Some((false, Word::Status)));
        assert_eq!(parsed(&["status"]), Some((false, Word::Status)));
        assert_eq!(parsed(&["check"]), Some((false, Word::Check)));
        assert_eq!(
            parsed(&["trial"]),
            Some((false, Word::Trial { force: false }))
        );
        assert_eq!(
            parsed(&["trial", "--force"]),
            Some((false, Word::Trial { force: true }))
        );
        assert_eq!(
            parsed(&["default", "/nix/store/x"]),
            Some((false, Word::Default("/nix/store/x".into())))
        );
        // Through sudo, the same words after its own.
        assert_eq!(
            parsed(&[THROUGH_SUDO, "trial", "--force"]),
            Some((true, Word::Trial { force: true }))
        );
        assert_eq!(parsed(&[THROUGH_SUDO]), Some((true, Word::Status)));
        for words in [
            &["go"][..],
            &["default"],
            &["default", ""],
            &["default", "/a", "/b"],
            &["trial", "-f"],
            &["trial", "--force", "--force"],
            &["status", "now"],
            &["check", "--force"],
            &["--force", "trial"],
            &[THROUGH_SUDO, THROUGH_SUDO],
        ] {
            assert_eq!(parsed(words), None, "{words:?}");
        }
    }
}
