//! References in the brief of an inbox job belong to that job while it runs.

use std::collections::BTreeSet;
use std::fs;

use super::{Pass, dirs};
use crate::job::Job;
use crate::{filled, read};

impl Pass<'_> {
    /// Issue numbers handed to queued, running or awaiting inbox jobs in this repo.
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
            if !job.dir.is_dir() {
                continue;
            }
            let state = job.state();
            // A finished turn parked on GitHub still owns its brief. Only
            // the live await counts, as in the watcher; await.next is a
            // declaration that has not been published by the turn's end.
            if !matches!(state.as_str(), "queued" | "running")
                && !(state == "done" && filled(&job.path("await")))
            {
                continue;
            }
            let input = fs::read_to_string(d.join("input")).unwrap_or_default();
            claimed.extend(references(repo, &input));
        }
        claimed
    }
}

/// Link labels describe their destinations; they are never independent claims.
fn references(repo: &str, input: &str) -> BTreeSet<String> {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};

    let mut found = BTreeSet::new();
    let mut linked = 0;
    for event in Parser::new(input) {
        match event {
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                issue_url(repo, &dest_url, &mut found);
                linked += 1;
            }
            Event::End(TagEnd::Link | TagEnd::Image) => linked -= 1,
            Event::Text(text) | Event::Code(text) if linked == 0 => {
                for token in text.split(|c: char| c.is_whitespace() || "[]()<>`\"',;*".contains(c))
                {
                    let token = token
                        .trim_end_matches(['.', '!', '?', ':'])
                        .to_ascii_lowercase();
                    if token.contains("://") {
                        issue_url(repo, &token, &mut found);
                    } else {
                        shorthand(repo, &token, &mut found);
                    }
                }
            }
            _ => {}
        }
    }
    found
}

fn issue_url(repo: &str, url: &str, found: &mut BTreeSet<String>) {
    let url = url.to_ascii_lowercase();
    // target also understands PRs; only issue URLs own issue work.
    if let Some(path) = url.strip_prefix("https://github.com/")
        && path.split('/').nth(2) == Some("issues")
    {
        record(repo, &url, found);
    }
}

/// Consume complete #N or OWNER/REPO#N members, separated by slashes.
/// The slash inside a qualified member is consumed before its number.
fn shorthand(repo: &str, mut text: &str, found: &mut BTreeSet<String>) {
    let mut members = BTreeSet::new();
    loop {
        let Some((of, after)) = text.split_once('#') else {
            return;
        };
        let end = after.bytes().take_while(u8::is_ascii_digit).count();
        let (n, rest) = after.split_at(end);
        let of = if of.is_empty() { repo } else { of };
        if !record(repo, &format!("{of}#{n}"), &mut members) {
            return;
        }
        if rest.is_empty() {
            found.extend(members);
            return;
        }
        let Some(next) = rest.strip_prefix('/') else {
            return;
        };
        text = next;
    }
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
    fn markdown_destinations_own_the_reference() {
        for text in [
            "[#7](https://github.com/sam/game/issues/7)",
            "[#7](https://example.org/docs/#7)",
            "[#7](https://github.com/sam/box/pull/7)",
            "[#7][work]\n\n[work]: https://github.com/sam/game/issues/7",
            "[#7][]\n\n[#7]: https://example.org/docs/#7",
            "[#7]\n\n[#7]: https://github.com/sam/game/issues/7",
            "![#7](https://example.org/image.png)",
        ] {
            assert!(references("sam/box", text).is_empty(), "{text}");
        }
        for text in [
            "[#99](https://github.com/sam/box/issues/7)",
            "[#99][work]\n\n[work]: https://github.com/sam/box/issues/7",
            "[#99][]\n\n[#99]: https://github.com/sam/box/issues/7",
            "[#99]\n\n[#99]: https://github.com/sam/box/issues/7",
        ] {
            assert_eq!(
                references("sam/box", text),
                std::collections::BTreeSet::from(["7".into()]),
                "{text}"
            );
        }
    }

    #[test]
    fn every_slash_separated_member_is_a_reference() {
        for text in [
            "#7/#8",
            "#7/SAM/BOX#8",
            "sam/box#7/#8",
            "SAM/BOX#7/sam/box#8",
            "sam/game#9/#7/sam/box#8",
            "#7/sam/game#9/SAM/BOX#8",
        ] {
            assert_eq!(
                references("sam/box", text),
                std::collections::BTreeSet::from(["7".into(), "8".into()]),
                "{text}"
            );
        }
    }

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
