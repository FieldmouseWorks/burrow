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
    /// The request a line makes, with its repos among `owners`'.
    pub fn parse(line: &str, owners: &[String]) -> Result<Self, String> {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["undo", what] if !what.starts_with('-') => {
                let (repo, pr) = what
                    .split_once('#')
                    .ok_or_else(|| format!("`{what}` isn't OWNER/REPO#N: {WORDS}"))?;
                let pr = pr
                    .parse::<u64>()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| format!("`{what}` isn't OWNER/REPO#N: {WORDS}"))?;
                Ok(Self::Change {
                    repo: repo_of(repo, owners)?,
                    pr,
                })
            }
            ["undo", "--actor", session] => {
                let id = session.strip_prefix(SESSION).unwrap_or("");
                if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(format!("`{session}` isn't a session's link: {WORDS}"));
                }
                Ok(Self::Actor {
                    session: (*session).to_owned(),
                })
            }
            ["undo", "--since", time, "--repo", repo]
            | ["undo", "--repo", repo, "--since", time] => {
                let at: jiff::Timestamp = time
                    .parse()
                    .map_err(|_| format!("`{time}` isn't an RFC 3339 time: {WORDS}"))?;
                Ok(Self::Since {
                    since: at.as_millisecond(),
                    repo: repo_of(repo, owners)?,
                })
            }
            _ => Err(format!(
                "`{}` isn't one undo; {WORDS}",
                burrow_door::clip(line, 200).replace('`', "'")
            )),
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

/// OWNER/NAME, both a name GitHub could give, and OWNER one of `owners`.
fn repo_of(repo: &str, owners: &[String]) -> Result<String, String> {
    let named = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let Some((owner, name)) = repo.split_once('/').filter(|(o, n)| named(o) && named(n)) else {
        return Err(format!("`{repo}` isn't OWNER/REPO: {WORDS}"));
    };
    if !owners.iter().any(|o| o.eq_ignore_ascii_case(owner)) {
        return Err(format!(
            "{repo} isn't a repo of {}, whose changes the ledger keeps",
            owners.join(" or ")
        ));
    }
    Ok(format!("{owner}/{name}"))
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
