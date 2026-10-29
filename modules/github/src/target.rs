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

/// The repo a GitHub URL, a git remote or a `-R` value names:
/// `https://github.com/OWNER/REPO`, `git@github.com:OWNER/REPO.git`,
/// `ssh://git@github.com/OWNER/REPO`, `github.com/OWNER/REPO` or
/// `OWNER/REPO`, and anything under it (`/pull/5`, `.git/info/lfs`).
/// Another host names none.
pub fn target_of(s: &str) -> Option<Target> {
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
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    (is_owner(owner) && is_repo(repo)).then(|| Target {
        owner: owner.to_owned(),
        repo: Some(repo.to_owned()),
    })
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
            assert_eq!(target_of(s).map(|t| t.owner).as_deref(), owner, "{s}");
        }
    }

    #[test]
    fn finds_the_repo_without_its_git_suffix() {
        for (s, repo) in [
            ("https://github.com/Sam/R.git", Some("Sam/R")),
            ("github.com/sam/r.git/info/lfs", Some("sam/r")),
            ("git@github.com:sam/r.git", Some("sam/r")),
            ("https://github.com/sam/r/pull/5", Some("sam/r")),
            ("sam/r", Some("sam/r")),
            ("github.com/sam/.git", None),
            ("github.com/sam/..", None),
            ("github.com/sam/r%20x", None),
        ] {
            assert_eq!(target_of(s).map(|t| t.to_string()).as_deref(), repo, "{s}");
        }
    }
}
