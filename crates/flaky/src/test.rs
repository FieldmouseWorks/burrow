//! What names a test, which issues quarantine one, and for how long.

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

/// How long an issue quarantines its test: two days from when it opened.
/// A flaky test passes when its second try does, quarantined or not; what
/// the quarantine adds is its issue, the fix job it gets, and this clock.
/// Once it runs out, the test's failures fail at once, with no second try,
/// until the fix closes the issue: nothing stays flaky for long unnoticed.
pub const EXPIRES: u64 = 2 * 24 * 3600;

/// Seconds since 1970 of a time as GitHub gives one, `2026-10-28T23:28:42Z`.
pub fn seconds(t: &str) -> Option<u64> {
    let b = t.as_bytes();
    let shape = b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        });
    if !shape {
        return None;
    }
    let n = |r: std::ops::Range<usize>| t[r].parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, min, s) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || min > 59 || s > 60 {
        return None;
    }
    // Days from 1970-01-01 to y-m-d, by Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + min * 60 + s).ok()
}

/// A time as GitHub gives one, for tests.
#[cfg(test)]
pub fn iso(secs: u64) -> String {
    let (days, rest) = (secs / 86400, secs % 86400);
    // Civil from days, by Howard Hinnant's algorithm.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
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
    fn times() {
        assert_eq!(seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(seconds("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(seconds("2026-10-28T23:28:42Z"), Some(1_793_230_122));
        for bad in [
            "",
            "2026-10-28 23:28:42Z",
            "2026-10-28T23:28:42",
            "2026-10-28T23:28:42+00:00",
            "2026-13-28T23:28:42Z",
            "1969-12-31T23:59:59Z",
            "2026-1a-28T23:28:42Z",
        ] {
            assert_eq!(seconds(bad), None, "{bad}");
        }
        for t in [0, 951_868_800, 1_793_230_122, crate::quarantine::now()] {
            assert_eq!(seconds(&iso(t)), Some(t), "{t}");
        }
    }

    #[test]
    fn who_can_push() {
        assert!(can_push("admin") && can_push("write"));
        for no in ["read", "none", "triage", ""] {
            assert!(!can_push(no), "{no}");
        }
    }
}
