//! burrow-confirm CONFIG [SYSTEM]: HOST-confirm (confirm.rs), as
//! modules/switch.nix builds it in with its config. The system HOST-switch
//! put live is kept, and booted into from now on. Told SYSTEM, it keeps only
//! that, and exits 4 when another is pending in its place (HOST-update
//! holds by it).
//!
//! It needs root, so one started as anyone else starts again through sudo.
//! It exits 1 when there was a switch it couldn't keep, and 2 on a usage or
//! a config it can't take.

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use burrow_switch::{Broke, Config, THROUGH_SUDO, as_root, confirm};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((config, words)) = args.split_first() else {
        eprintln!("usage: burrow-confirm CONFIG");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-confirm: {e}");
            return ExitCode::from(2);
        }
    };
    let Some((through_sudo, mine)) = parse(words) else {
        eprintln!("usage: {} [SYSTEM]", cfg.confirm);
        return ExitCode::from(2);
    };
    if let Some(why) = as_root(&cfg.tools.sudo, config, through_sudo, mine.as_slice()) {
        eprintln!("{}: {why}", cfg.confirm);
        return ExitCode::FAILURE;
    }
    match confirm::confirm(&cfg, mine.as_deref()) {
        Ok(code) => ExitCode::from(code),
        Err(Broke(why)) => {
            eprintln!("{}: {why}", cfg.confirm);
            ExitCode::FAILURE
        }
    }
}

/// What it's asked, after CONFIG: whether it came through sudo, and the
/// system it means (a path from the root), if it's told one; nothing when
/// that isn't it.
fn parse(words: &[String]) -> Option<(bool, Option<PathBuf>)> {
    let (through_sudo, words) = match words {
        [w, rest @ ..] if w == THROUGH_SUDO => (true, rest),
        _ => (false, words),
    };
    match words {
        [] => Some((through_sudo, None)),
        [s] if s.starts_with('/') => Some((through_sudo, Some(s.into()))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(words: &[&str]) -> Option<(bool, Option<PathBuf>)> {
        parse(&words.iter().map(|w| w.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn it_takes_the_system_it_means_through_sudo_too() {
        assert_eq!(parsed(&[]), Some((false, None)));
        assert_eq!(parsed(&[THROUGH_SUDO]), Some((true, None)));
        let s = Some(PathBuf::from("/nix/store/s"));
        assert_eq!(parsed(&["/nix/store/s"]), Some((false, s.clone())));
        assert_eq!(parsed(&[THROUGH_SUDO, "/nix/store/s"]), Some((true, s)));
        for words in [
            &[""][..],
            &["--force"],
            &["now"],
            &["result"],
            &["/nix/store/s", "/nix/store/t"],
            &[THROUGH_SUDO, ""],
        ] {
            assert_eq!(parsed(words), None, "{words:?}");
        }
    }
}
