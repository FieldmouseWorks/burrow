//! What names a test, and which issues quarantine one.

use serde_json::Value;

/// Every flaky test's issue carries this label.
pub const LABEL: &str = "flaky";
/// And is titled this, then the test.
pub const PREFIX: &str = "flaky: ";
const MAX: usize = 200;

/// Whether `id` can name a test: one token of ASCII letters, digits and
/// `_ . : / @ + = - [ ]`, starting with a letter, a digit or `_`, at most 200
/// bytes. A gate names its own tests, and keeps each name the same from run
/// to run: the quarantine goes by it.
pub fn valid(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric() || c == '_')
        && id.len() <= MAX
        && chars.all(|c| c.is_ascii_alphanumeric() || "_.:/@+=-[]".contains(c))
}

pub fn title(test: &str) -> String {
    format!("{PREFIX}{test}")
}

/// Whether the issue's author may quarantine a test: someone who can push to
/// the repo (its owner, a member of its organization, a collaborator), or an
/// app, since a repo's own CI files issues as github-actions[bot]. Anyone can
/// open an issue on a public repo, and a template can label it.
pub fn trusted(issue: &Value) -> bool {
    matches!(
        issue["author_association"].as_str(),
        Some("OWNER" | "MEMBER" | "COLLABORATOR")
    ) || issue["user"]["type"].as_str() == Some("Bot")
}

/// The test an issue quarantines: an issue (not a pull request) labelled
/// LABEL, titled PREFIX and a test, by a trusted author. Whether it's open is
/// the caller's to ask.
pub fn quarantined_by(issue: &Value) -> Option<String> {
    let pr = issue.get("pull_request").is_some_and(|p| !p.is_null());
    let labelled = issue["labels"]
        .as_array()
        .is_some_and(|ls| ls.iter().any(|l| l["name"].as_str() == Some(LABEL)));
    let test = issue["title"].as_str()?.strip_prefix(PREFIX)?;
    (!pr && labelled && valid(test) && trusted(issue)).then(|| test.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn issue(title: &str, assoc: &str, kind: &str) -> Value {
        json!({"number": 7, "title": title, "labels": [{"name": "bug"}, {"name": LABEL}],
               "author_association": assoc, "user": {"login": "x", "type": kind}})
    }

    #[test]
    fn names() {
        for ok in [
            "browser/thief-terrain",
            "tests.test_lock.Units.test_churn",
            "psiv_core::scene::tests::walk[2]",
            "_x",
            "a=b+c@d",
        ] {
            assert!(valid(ok), "{ok}");
        }
        for bad in ["", "-x", "a b", "a\nb", "`x`", "x,y", &"a".repeat(201), "é"] {
            assert!(!valid(bad), "{bad:?}");
        }
        assert!(valid(&"a".repeat(200)));
    }

    #[test]
    fn who_quarantines() {
        let t = "flaky: browser/stream";
        assert_eq!(
            quarantined_by(&issue(t, "OWNER", "User")).as_deref(),
            Some("browser/stream")
        );
        assert!(quarantined_by(&issue(t, "MEMBER", "User")).is_some());
        assert!(quarantined_by(&issue(t, "COLLABORATOR", "User")).is_some());
        // github-actions[bot], filing from the repo's own CI.
        assert!(quarantined_by(&issue(t, "NONE", "Bot")).is_some());
        // A stranger, on a public repo, with a template that labels it.
        assert!(quarantined_by(&issue(t, "NONE", "User")).is_none());
        assert!(quarantined_by(&issue(t, "CONTRIBUTOR", "User")).is_none());
        assert!(quarantined_by(&issue(t, "FIRST_TIME_CONTRIBUTOR", "User")).is_none());
        // Not this kind of issue.
        assert!(quarantined_by(&issue("Flaky: browser/stream", "OWNER", "User")).is_none());
        assert!(quarantined_by(&issue("flaky: two words", "OWNER", "User")).is_none());
        let mut unlabelled = issue(t, "OWNER", "User");
        unlabelled["labels"] = json!([{"name": "bug"}]);
        assert!(quarantined_by(&unlabelled).is_none());
        let mut pr = issue(t, "OWNER", "User");
        pr["pull_request"] = json!({"url": "u"});
        assert!(quarantined_by(&pr).is_none());
        pr["pull_request"] = Value::Null;
        assert!(quarantined_by(&pr).is_some());
    }
}
