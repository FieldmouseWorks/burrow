//! burrow-switch CONFIG [WORDS]: HOST-switch (switch.rs), as
//! modules/switch.nix builds it in with its config.
//!
//!   HOST-switch [FLAKE_DIR]        build FLAKE_DIR#CONFIG (default: the
//!                                  machine's flake) and put it live
//!   HOST-switch --toplevel PATH    an already-built system instead
//!   HOST-switch --status           what's pending
//!   HOST-switch --abort            a pending switch reverted now instead of
//!                                  waiting
//!
//! `--allow-downgrade`, anywhere on the line, puts live a system built from
//! an older nixpkgs than the running one; without it the refusal exits 3.
//! BURROW_CONFIRM_WITHIN says how long the switch waits for HOST-confirm
//! (a systemd time span; the config's own otherwise).
//!
//! It needs root, so one started as anyone else starts again through sudo,
//! with the window it was given. It exits 1 when what it was asked didn't
//! happen, and 2 on a usage or a config it can't take.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use burrow_switch::switch::{self, Me, Word};
use burrow_switch::{Broke, Config, THROUGH_SUDO, as_root};

/// What it's asked, after CONFIG: the window it came through sudo with, if
/// it did, and the word; nothing when that isn't one.
fn parse(words: &[String]) -> Option<(Option<String>, Word)> {
    let (within, words) = match words {
        [w, within, rest @ ..] if w == THROUGH_SUDO => (Some(within.clone()), rest),
        [w] if w == THROUGH_SUDO => return None,
        _ => (None, words),
    };
    Some((within, switch::parse(words)?))
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((config, words)) = args.split_first() else {
        eprintln!("usage: burrow-switch CONFIG [WORDS]");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-switch: {e}");
            return ExitCode::from(2);
        }
    };
    let Some((through_sudo, word)) = parse(words) else {
        eprintln!(
            "usage: {} [FLAKE_DIR | --toplevel PATH | --status | --abort] [--allow-downgrade]",
            cfg.switch
        );
        return ExitCode::from(2);
    };
    let within = through_sudo.clone().unwrap_or_else(|| {
        env::var("BURROW_CONFIRM_WITHIN")
            .ok()
            .filter(|w| !w.is_empty())
            .unwrap_or_else(|| cfg.within.clone())
    });
    let again: Vec<&str> = [within.as_str()]
        .into_iter()
        .chain(words.iter().map(String::as_str))
        .collect();
    if let Some(why) = as_root(&cfg.tools.sudo, config, through_sudo.is_some(), &again) {
        eprintln!("{}: {why}", cfg.switch);
        return ExitCode::FAILURE;
    }
    let me = match (env::current_exe(), std::fs::canonicalize(config)) {
        (Ok(exe), Ok(config)) => Me { exe, config },
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{}: its own path: {e}", cfg.switch);
            return ExitCode::FAILURE;
        }
    };
    match switch::run(&cfg, &me, &within, &word) {
        Ok(code) => ExitCode::from(code),
        Err(Broke(why)) => {
            eprintln!("{}: {why}", cfg.switch);
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(words: &[&str]) -> Option<(Option<String>, Word)> {
        parse(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn through_sudo_it_brings_its_window_and_its_words() {
        assert_eq!(parsed(&["--status"]), Some((None, Word::Status)));
        assert_eq!(
            parsed(&[THROUGH_SUDO, "15min", "--status"]),
            Some((Some("15min".into()), Word::Status))
        );
        for words in [
            &[THROUGH_SUDO][..],
            &[THROUGH_SUDO, "15min", "--go"],
            &["--status", THROUGH_SUDO],
        ] {
            assert_eq!(parsed(words), None, "{words:?}");
        }
    }
}
