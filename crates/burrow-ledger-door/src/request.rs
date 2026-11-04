//! What an issue asks for, in cn's words, each word checked for its shape.

use std::fmt;

/// One undo, as `cn undo` takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The change a PR is: OWNER/REPO#N.
    Change { repo: String, pr: u64 },
    /// Everything an agent session landed.
    Actor { session: String },
    /// Everything that landed on `repo` after `since` (ms since the epoch).
    Since { since: i64, repo: String },
}

/// What a request takes, for a line that isn't one.
pub const WORDS: &str = "ask for one undo: `undo OWNER/REPO#N`, `undo --actor https://claude.ai/code/session_ID` or `undo --since 2026-11-04T18:00:00Z --repo OWNER/REPO`";

const SESSION: &str = "https://claude.ai/code/session_";

impl Request {
    /// The request a line makes. Which repos it may reach is the ledger's
    /// to say: cn refuses a change, or a repo, the ledger keeps nothing of,
    /// and a session's undo reaches only the repos the ledger has it
    /// landing in.
    pub fn parse(line: &str) -> Result<Self, String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["undo", what] if !what.starts_with('-') => {
                let not = || format!("`{}` isn't OWNER/REPO#N: {WORDS}", echo(what));
                let (repo, pr) = what.split_once('#').ok_or_else(not)?;
                let pr = pr.parse::<u64>().ok().filter(|&n| n > 0).ok_or_else(not)?;
                Ok(Self::Change {
                    repo: repo_of(repo)?,
                    pr,
                })
            }
            ["undo", "--actor", session] => {
                let id = session.strip_prefix(SESSION).unwrap_or("");
                if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(format!(
                        "`{}` isn't a session's link: {WORDS}",
                        echo(session)
                    ));
                }
                Ok(Self::Actor {
                    session: (*session).to_owned(),
                })
            }
            ["undo", "--since", time, "--repo", repo]
            | ["undo", "--repo", repo, "--since", time] => {
                let at: jiff::Timestamp = time
                    .parse()
                    .map_err(|_| format!("`{}` isn't an RFC 3339 time: {WORDS}", echo(time)))?;
                Ok(Self::Since {
                    since: at.as_millisecond(),
                    repo: repo_of(repo)?,
                })
            }
            _ => Err(format!("`{}` isn't one undo; {WORDS}", echo(line))),
        }
    }

    /// Its arguments to `cn`.
    pub fn args(&self) -> Vec<String> {
        match self {
            Self::Change { repo, pr } => vec!["undo".into(), format!("{repo}#{pr}")],
            Self::Actor { session } => vec!["undo".into(), "--actor".into(), session.clone()],
            Self::Since { since, repo } => vec![
                "undo".into(),
                "--since".into(),
                since.to_string(),
                "--repo".into(),
                repo.clone(),
            ],
        }
    }
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.args().join(" "))
    }
}

/// OWNER/NAME, as GitHub allows them (burrow_github::repo_name).
fn repo_of(repo: &str) -> Result<String, String> {
    if burrow_github::repo_name(repo) {
        Ok(repo.to_owned())
    } else {
        Err(format!("`{}` isn't OWNER/REPO: {WORDS}", echo(repo)))
    }
}

/// Words of the issue's, as a refusal shows them: clipped, and in no
/// backticks of their own.
fn echo(said: &str) -> String {
    burrow_door::clip(said, 200).replace('`', "'")
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
