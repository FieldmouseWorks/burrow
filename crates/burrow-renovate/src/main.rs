//! burrow-renovate CONFIG [OWNER/REPO]: renovate-run (lib.rs), as
//! modules/updates.nix builds it in with its config.
//!
//!   renovate-run              every repo (burrow-events' catch-up: new
//!                             releases upstream send no event)
//!   renovate-run OWNER/REPO   that repo, if a full run covers it: any repo's
//!                             events reach this unit
//!
//! It exits as Renovate's run did, 1 when something it needed broke, and 2
//! on a usage or a config it can't take.

use std::env;
use std::path::Path;
use std::process::ExitCode;

use burrow_renovate::Config;

/// What it's asked, after CONFIG: one repo, or every one (None); nothing
/// when that isn't one.
fn parse(words: &[String]) -> Option<Option<&str>> {
    match words {
        [] => Some(None),
        [repo] => {
            let (owner, name) = repo.split_once('/')?;
            let part = |p: &str| !p.is_empty() && !p.contains(['/', ' ']);
            (part(owner) && part(name)).then_some(Some(repo.as_str()))
        }
        _ => None,
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((config, words)) = args.split_first() else {
        eprintln!("usage: burrow-renovate CONFIG [OWNER/REPO]");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-renovate: {e}");
            return ExitCode::from(2);
        }
    };
    let Some(only) = parse(words) else {
        eprintln!("usage: {} [OWNER/REPO]", cfg.name);
        return ExitCode::from(2);
    };
    ExitCode::from(cfg.run(only))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_words_are_none_or_one_repo() {
        let parsed = |w: &[&str]| {
            let w: Vec<String> = w.iter().map(|s| s.to_string()).collect();
            parse(&w).map(|o| o.map(str::to_string))
        };
        assert_eq!(parsed(&[]), Some(None));
        assert_eq!(parsed(&["Sam/R"]), Some(Some("Sam/R".into())));
        for bad in [
            &["sam"][..],
            &["sam/"],
            &["/r"],
            &["sam/r/x"],
            &["sam/r x"],
            &[""],
            &["sam/r", "now"],
        ] {
            assert_eq!(parsed(bad), None, "{bad:?}");
        }
    }
}
