//! Renovate's two programs, each run by path (their configs' `tools`):
//!
//! - run.rs: renovate-run, Renovate over every repo it covers, or over one
//! - sweep.rs: renovate-sweep, the Renovate PRs that can't merge themselves
//!   handed to a Claude job, and the owner told of the majors waiting on
//!   them
//!
//! Both are started for every repo, or (their units' instances) for one.

use std::io::{self, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

pub mod run;
pub mod sweep;

/// What a program that couldn't be run exits with, as a shell says it.
pub(crate) const NOT_RUN: u8 = 127;

/// What either is asked, after its config: one repo (OWNER/REPO), or every
/// one (None); nothing when that isn't one.
pub fn words(words: &[String]) -> Option<Option<&str>> {
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

/// Text a command printed, as a shell's `$(...)` has it: without its
/// trailing newlines.
pub(crate) fn trimmed(out: Vec<u8>) -> String {
    String::from_utf8_lossy(&out)
        .trim_end_matches('\n')
        .to_string()
}

/// An exit as a shell says it: its code, or 128 and the signal that ended it.
pub(crate) fn exit_of(status: ExitStatus) -> u8 {
    match (status.code(), status.signal()) {
        (Some(code), _) => u8::try_from(code).unwrap_or(1),
        (None, Some(signal)) => u8::try_from(128 + signal).unwrap_or(1),
        (None, None) => 1,
    }
}

pub(crate) fn say(line: &str) {
    let _ = writeln!(io::stdout().lock(), "{line}");
}

pub(crate) fn complain(line: &str) {
    let _ = writeln!(io::stderr().lock(), "{line}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_words_are_none_or_one_repo() {
        let parsed = |w: &[&str]| {
            let w: Vec<String> = w.iter().map(|s| s.to_string()).collect();
            words(&w).map(|o| o.map(str::to_string))
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

    #[test]
    fn an_exit_is_what_a_shell_would_say_it_was() {
        assert_eq!(exit_of(ExitStatus::from_raw(3 << 8)), 3);
        assert_eq!(exit_of(ExitStatus::from_raw(9)), 137);
    }

    #[test]
    fn what_a_command_printed_loses_its_trailing_newlines() {
        assert_eq!(trimmed(b"Sam/R\n\n".to_vec()), "Sam/R");
        assert_eq!(trimmed(b"a\nb\n".to_vec()), "a\nb");
    }
}
