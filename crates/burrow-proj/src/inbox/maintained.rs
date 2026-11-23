//! Issues whose body a bot maintains: it rewrites the text on every run, so
//! the text is the bot's page, not a task. Such an issue is a tracker by
//! rule, never by a sorting job's verdict: it isn't sorted by a job, split
//! or planned ([`super::Pass::sorted`]), and a list split from one before
//! this held is ended ([`super::Pass::end_list`]) rather than paused on the
//! bot's next edit, which would hold it for good.

use serde_json::Value;

/// What maintains the issue's body, in words, when a bot does: the issue
/// is opened by a bot account and reads as that bot's page. None for
/// anything else, a person's issue that quotes the page included.
pub(super) fn maintained(issue: &Value) -> Option<&'static str> {
    if issue["user"]["type"].as_str() != Some("Bot") {
        return None;
    }
    let title = issue["title"].as_str().unwrap_or_default();
    let body = issue["body"].as_str().unwrap_or_default();
    PAGES
        .iter()
        .find(|page| (page.is)(title, body))
        .map(|page| page.name)
}

/// A bot's page: its name, and how its issue reads.
struct Page {
    name: &'static str,
    is: fn(&str, &str) -> bool,
}

const PAGES: &[Page] = &[Page {
    name: "Renovate's Dependency Dashboard",
    // Renovate's default title, or the docs link its default header opens
    // with (either can be configured away, rarely both).
    is: |title, body| {
        title == "Dependency Dashboard"
            || body.contains("docs.renovatebot.com/key-concepts/dashboard")
    },
}];

#[cfg(test)]
mod tests {
    use super::maintained;
    use serde_json::json;

    #[test]
    fn only_a_bots_page_is_maintained() {
        let issue = |login: &str, title: &str, body: &str| json!({"user": burrow_github::fake::actor(login), "title": title, "body": body});
        let renovate = "This issue lists Renovate updates and detected dependencies. Read the [Dependency Dashboard](https://docs.renovatebot.com/key-concepts/dashboard/) docs to learn more.";
        for (login, title, body, is) in [
            ("workers[bot]", "Dependency Dashboard", renovate, true),
            ("renovate[bot]", "Dependency Dashboard", "", true),
            ("workers[bot]", "Updates", renovate, true),
            ("sam", "Dependency Dashboard", renovate, false),
            ("workers[bot]", "Fix the dashboard", "It's broken.", false),
        ] {
            assert_eq!(
                maintained(&issue(login, title, body)).is_some(),
                is,
                "{login} {title}"
            );
        }
    }
}
