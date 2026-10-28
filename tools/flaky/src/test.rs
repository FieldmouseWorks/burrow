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

/// The one app whose issues quarantine a test: a repo's own GitHub Actions,
/// which only the repo's workflows can act as. Any other app installed on it
/// can open issues too, and doesn't.
pub const ACTIONS: &str = "github-actions[bot]";

/// Whether a permission GitHub gives a person on a repo (`permission`, from
/// its collaborators API) lets them push: `admin`, or `write`, which
/// `maintain` maps to. Triage maps to `read`: it can label an issue, not
/// change the code a gate checks.
pub fn can_push(permission: &str) -> bool {
    matches!(permission, "admin" | "write")
}

/// The test an issue would quarantine: an issue (not a pull request)
/// labelled LABEL, titled PREFIX and a test, and never reopened. A closed
/// issue's quarantine stays lifted: anyone who can triage can reopen one,
/// and a test that flakes again gets an issue of its own. Whether it's open,
/// and whether its author may quarantine a test, are the caller's to ask.
pub fn quarantined_by(issue: &Value) -> Option<String> {
    let pr = issue.get("pull_request").is_some_and(|p| !p.is_null());
    let labelled = issue["labels"]
        .as_array()
        .is_some_and(|ls| ls.iter().any(|l| l["name"].as_str() == Some(LABEL)));
    let reopened = issue["state_reason"].as_str() == Some("reopened");
    let test = issue["title"].as_str()?.strip_prefix(PREFIX)?;
    (!pr && labelled && !reopened && valid(test)).then(|| test.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn issue(title: &str) -> Value {
        json!({"number": 7, "title": title, "labels": [{"name": "bug"}, {"name": LABEL}],
               "user": {"login": "x", "type": "User"}})
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
    fn which_issues() {
        let t = "flaky: browser/stream";
        assert_eq!(quarantined_by(&issue(t)).as_deref(), Some("browser/stream"));
        // Not this kind of issue.
        assert!(quarantined_by(&issue("Flaky: browser/stream")).is_none());
        assert!(quarantined_by(&issue("flaky: two words")).is_none());
        let mut unlabelled = issue(t);
        unlabelled["labels"] = json!([{"name": "bug"}]);
        assert!(quarantined_by(&unlabelled).is_none());
        let mut pr = issue(t);
        pr["pull_request"] = json!({"url": "u"});
        assert!(quarantined_by(&pr).is_none());
        pr["pull_request"] = Value::Null;
        assert!(quarantined_by(&pr).is_some());
        // Closed once, then reopened: it stays lifted.
        let mut reopened = issue(t);
        reopened["state_reason"] = json!("reopened");
        assert!(quarantined_by(&reopened).is_none());
        reopened["state_reason"] = Value::Null;
        assert!(quarantined_by(&reopened).is_some());
    }

    #[test]
    fn who_can_push() {
        assert!(can_push("admin") && can_push("write"));
        for no in ["read", "none", "triage", ""] {
            assert!(!can_push(no), "{no}");
        }
    }
}
