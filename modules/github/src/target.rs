//! What a token is for: an account (every repo of the App's installation on
//! it), or one repo of it. Names are checked as GitHub spells them, so one
//! is always safe as a file name in the cache and as a path in a URL.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub owner: String,
    pub repo: Option<String>,
}

impl Target {
    /// `OWNER` or `OWNER/REPO`.
    pub fn parse(s: &str) -> Option<Self> {
        let (owner, repo) = match s.split_once('/') {
            Some((o, r)) => (o, Some(r)),
            None => (s, None),
        };
        if !is_owner(owner) || repo.is_some_and(|r| !is_repo(r)) {
            return None;
        }
        Some(Self {
            owner: owner.to_owned(),
            repo: repo.map(str::to_owned),
        })
    }

    /// The cache's name for it. GitHub's names are case-blind.
    pub fn key(&self) -> String {
        match &self.repo {
            Some(r) => format!("{}@{}", self.owner, r).to_lowercase(),
            None => self.owner.to_lowercase(),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.repo {
            Some(r) => write!(f, "{}/{r}", self.owner),
            None => f.write_str(&self.owner),
        }
    }
}

/// An account's login: letters, digits and single hyphens, not at either end.
pub fn is_owner(s: &str) -> bool {
    (1..=39).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
}

/// A repo's name: letters, digits, `.`, `-` and `_`, never `.` or `..`.
pub fn is_repo(s: &str) -> bool {
    (1..=100).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && s != "."
        && s != ".."
}

/// The account a GitHub URL, a git remote or a `-R` value names:
/// `https://github.com/OWNER/REPO`, `git@github.com:OWNER/REPO.git`,
/// `ssh://git@github.com/OWNER/REPO`, `github.com/OWNER/REPO` or
/// `OWNER/REPO`. Another host names none.
pub fn owner_of(s: &str) -> Option<String> {
    let rest = ["https://", "http://", "ssh://"]
        .iter()
        .find_map(|scheme| s.strip_prefix(scheme))
        .unwrap_or(s);
    let rest = rest.strip_prefix("git@").unwrap_or(rest);
    let path = match rest.strip_prefix("github.com") {
        Some(p) => p.strip_prefix('/').or_else(|| p.strip_prefix(':'))?,
        // No scheme or user, so OWNER/REPO and nothing else: a path of more
        // parts is no repo of GitHub's.
        None if rest.len() == s.len() && s.matches('/').count() == 1 => rest,
        None => return None,
    };
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    (is_owner(owner) && !repo.is_empty()).then(|| owner.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_owner_or_a_repo() {
        assert_eq!(
            Target::parse("FieldmouseWorks/burrow"),
            Some(Target {
                owner: "FieldmouseWorks".into(),
                repo: Some("burrow".into())
            })
        );
        assert_eq!(Target::parse("sam").unwrap().repo, None);
        assert_eq!(Target::parse("Sam/R.git").unwrap().key(), "sam@r.git");
        assert_eq!(Target::parse("Sam").unwrap().key(), "sam");
        for bad in [
            "", "/", "sam/", "/r", "sam/r/x", "-sam", "sam-", "s--m", "sa m", "sam/..", "sam/.",
            "../x", "sam/r\n", "sam\0",
        ] {
            assert_eq!(Target::parse(bad), None, "{bad:?}");
        }
        assert!(Target::parse(&"a".repeat(40)).is_none());
    }

    #[test]
    fn finds_the_owner_in_urls_and_remotes() {
        for (s, owner) in [
            ("https://github.com/Sam/r", Some("Sam")),
            ("https://github.com/Sam/r.git", Some("Sam")),
            ("https://github.com/sam/r/pull/5", Some("sam")),
            ("git@github.com:sam/r.git", Some("sam")),
            ("ssh://git@github.com/sam/r", Some("sam")),
            ("github.com/sam/r", Some("sam")),
            ("sam/r", Some("sam")),
            ("https://gitlab.com/sam/r", None),
            ("git@gitlab.com:sam/r.git", None),
            ("https://github.com/sam", None),
            ("https://github.com.evil/sam/r", None),
            ("sam", None),
            ("a/b/c", None),
            ("./sam/r", None),
            ("https://sam/r", None),
        ] {
            assert_eq!(owner_of(s).as_deref(), owner, "{s}");
        }
    }
}
