//! References in the brief of an inbox job belong to that job while it runs.

use std::collections::BTreeSet;
use std::fs;

use super::{Pass, dirs};
use crate::job::Job;
use crate::read;

impl Pass<'_> {
    /// Issue numbers handed to running or queued inbox jobs in this repo.
    /// Read the handed snapshot, never newer text that the job hasn't seen.
    pub(super) fn claimed(&self, repo: &str) -> BTreeSet<String> {
        let mut claimed = BTreeSet::new();
        for d in dirs(&self.inbox.state, "") {
            let issue = read(&d.join("issue")).unwrap_or_default();
            let Some((of, _)) = issue.rsplit_once('#') else {
                continue;
            };
            if !of.eq_ignore_ascii_case(repo) {
                continue;
            }
            let Some(id) = read(&d.join("job")) else {
                continue;
            };
            let job = Job::new(self.cfg, &id);
            if !job.dir.is_dir() || !matches!(job.state().as_str(), "queued" | "running") {
                continue;
            }
            let input = fs::read_to_string(d.join("input")).unwrap_or_default();
            claimed.extend(references(repo, &input));
        }
        claimed
    }
}

/// Parse whole reference tokens so a URL's fragment cannot become shorthand.
fn references(repo: &str, input: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for token in input.split(|c: char| c.is_whitespace() || "[]()<>`\"',;*".contains(c)) {
        let token = token
            .trim_end_matches(['.', '!', '?', ':'])
            .to_ascii_lowercase();
        if token.contains("://") {
            // target also understands PRs; only issue URLs own issue work.
            if let Some(path) = token.strip_prefix("https://github.com/") {
                let parts: Vec<_> = path.split('/').collect();
                if parts.get(2) == Some(&"issues") {
                    record(repo, &token, &mut found);
                }
            }
            continue;
        }
        // A slash between shorthand references is punctuation, while the
        // slash inside OWNER/REPO remains part of the qualified reference.
        for (index, part) in token.split("/#").enumerate() {
            let part = if index == 0 {
                part.to_owned()
            } else {
                format!("#{part}")
            };
            let reference = if part.starts_with('#') {
                format!("{repo}{part}")
            } else {
                part
            };
            if !record(repo, &reference, &mut found) {
                break;
            }
        }
    }
    found
}

// Whether this is a complete reference, including a valid foreign one.
fn record(repo: &str, reference: &str, found: &mut BTreeSet<String>) -> bool {
    let Some((of, n)) = crate::github::target(reference) else {
        return false;
    };
    let Ok(n) = n.parse::<u64>() else {
        return false;
    };
    if n == 0 {
        return false;
    }
    if of.eq_ignore_ascii_case(repo) {
        found.insert(n.to_string());
    }
    true
}

#[cfg(test)]
mod tests {
    use super::references;

    #[test]
    fn whole_issue_references() {
        for text in [
            "#7",
            "SAM/BOX#7",
            "#8/#7",
            "sam/box#8/#7",
            "https://github.com/sam/box/issues/7",
            "[deliver](HTTPS://GITHUB.COM/SAM/BOX/ISSUES/7#issuecomment-123)",
            "<https://github.com/Sam/Box/issues/7?x=y>",
        ] {
            assert!(references("sam/box", text).contains("7"), "{text}");
        }
        for text in [
            "https://example.org/docs/#7",
            "https://example.org/docs#7",
            "example.org/docs/#7",
            "https://github.com/sam/game/issues/7#7",
            "sam/game#7",
            "https://github.com/sam/box/pull/7",
            "word#7",
            "#7word",
            "#0",
        ] {
            assert!(references("sam/box", text).is_empty(), "{text}");
        }
        assert_eq!(
            references("sam/box", "https://github.com/sam/box/issues/8#7"),
            std::collections::BTreeSet::from(["8".into()])
        );
    }
}
