//! What a job awaits on GitHub: a PR or issue ([`target`]), and what about
//! it is news ([`fingerprint`]).

use std::collections::BTreeMap;
use std::process::{Command, Stdio};

use serde_json::{Map, Value, json};

use crate::config::Config;

/// OWNER/REPO and N from a PR or issue URL, or OWNER/REPO#N.
pub fn target(t: &str) -> Option<(String, String)> {
    let t = t.strip_prefix("https://github.com/").unwrap_or(t);
    let part = |s: &str| !s.is_empty() && !s.contains(['/', '#']);
    let mut f = t.splitn(4, '/');
    if let (Some(owner), Some(repo), Some("pull" | "issues"), Some(rest)) =
        (f.next(), f.next(), f.next(), f.next())
        && part(owner)
        && part(repo)
    {
        let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !n.is_empty() {
            return Some((format!("{owner}/{repo}"), n));
        }
    }
    let (repo, n) = t.split_once('#')?;
    let (owner, name) = repo.split_once('/')?;
    (part(owner) && part(name) && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .then(|| (repo.to_owned(), n.to_owned()))
}

/// What a job waiting on a PR or issue would want to hear about: a change in
/// it is news. Checks count only once they've all finished, so a run of them
/// wakes the job once. REST, not GraphQL, whose budget is shared with
/// everything else under the login. None when GitHub can't be asked.
pub fn fingerprint(cfg: &Config, repo: &str, n: &str) -> Option<Value> {
    let gh = |args: &[&str]| -> Option<Value> {
        let out = Command::new(&cfg.tools.gh)
            .arg("api")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    };
    // Every page, as one list: a comment, review or check past the first
    // hundred is news too.
    let pages = |path: String| -> Option<Vec<Value>> {
        let got = gh(&["--paginate", "--slurp", &path])?;
        Some(
            got.as_array()?
                .iter()
                .flat_map(|page| page.as_array().cloned().unwrap_or_default())
                .collect(),
        )
    };
    let issue = gh(&[&format!("repos/{repo}/issues/{n}")])?;
    // Comments on the issue or PR, but not proj-inbox's own: a job awaiting
    // its own issue would wake on every answer the inbox posts there.
    let comments = pages(format!("repos/{repo}/issues/{n}/comments?per_page=100"))?
        .iter()
        .filter(|c| {
            !c["body"]
                .as_str()
                .unwrap_or_default()
                .contains(&cfg.inbox_mark)
        })
        .count();
    if issue.get("pull_request").is_none() {
        return Some(json!({"state": issue["state"], "comments": comments}));
    }
    let pr = gh(&[&format!("repos/{repo}/pulls/{n}")])?;
    let sha = pr["head"]["sha"].as_str().unwrap_or_default().to_owned();
    // Reviews are counted by state, so a dismissed one is news as well as a
    // new one.
    let mut reviews: BTreeMap<String, usize> = BTreeMap::new();
    for r in pages(format!("repos/{repo}/pulls/{n}/reviews?per_page=100"))? {
        *reviews.entry(text(&r["state"])).or_default() += 1;
    }
    let status = gh(&[&format!("repos/{repo}/commits/{sha}/status")])?;
    let statuses = status["total_count"].as_u64().unwrap_or(0) > 0;
    let runs: Vec<Value> = gh(&[
        "--paginate",
        "--slurp",
        &format!("repos/{repo}/commits/{sha}/check-runs?per_page=100"),
    ])?
    .as_array()?
    .iter()
    .flat_map(|page| page["check_runs"].as_array().cloned().unwrap_or_default())
    .collect();
    let checks = if runs.iter().any(|r| r["status"] != "completed")
        || statuses && status["state"] == "pending"
    {
        "running".to_owned()
    } else {
        let mut all: Vec<String> = runs
            .iter()
            .map(|r| format!("{}={}", text(&r["name"]), text(&r["conclusion"])))
            .collect();
        if statuses {
            all.push(format!("status={}", text(&status["state"])));
        }
        all.sort();
        all.join(" ")
    };
    Some(json!({
        "state": pr["state"],
        "merged": pr["merged"],
        "head": pr["head"]["sha"],
        "comments": comments,
        "review_comments": pr["review_comments"],
        "reviews": reviews,
        "checks": checks,
    }))
}

/// A value as a string sits in text: null as "null".
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

/// What changed between two fingerprints, key by key; "" when nothing did.
pub fn changes(was: &Map<String, Value>, now: &Map<String, Value>) -> String {
    let mut keys: Vec<&String> = now.keys().collect();
    keys.sort();
    keys.into_iter()
        .filter(|k| was.get(*k) != now.get(*k))
        .map(|k| {
            let old = was.get(k).cloned().unwrap_or(Value::Null);
            format!("{k} {old} → {}", now[k])
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{changes, target};

    #[test]
    fn targets() {
        let t = |s: &str| target(s).map(|(r, n)| format!("{r} {n}"));
        assert_eq!(t("https://github.com/o/r/pull/5").as_deref(), Some("o/r 5"));
        assert_eq!(
            t("https://github.com/O/R/issues/12#issuecomment-3").as_deref(),
            Some("O/R 12")
        );
        assert_eq!(
            t("https://github.com/o/r/pull/5/files").as_deref(),
            Some("o/r 5")
        );
        assert_eq!(t("o/r#7").as_deref(), Some("o/r 7"));
        assert_eq!(t("o/r/pull/7").as_deref(), Some("o/r 7"));
        for bad in [
            "https://github.com/o/r",
            "o/r#",
            "o/r#7x",
            "o#7",
            "o/r/s#7",
            "https://github.com/o/r/pulls/5",
            "o/r#3/pull/5",
            "",
        ] {
            assert_eq!(t(bad), None, "{bad}");
        }
    }

    // Each key that changed, as it was and is now; one only there before
    // isn't news.
    #[test]
    fn what_changed() {
        let was = json!({"state": "open", "checks": "running", "comments": 1, "gone": 1});
        let now = json!({"state": "open", "checks": "ci=success", "comments": 2, "reviews": {"APPROVED": 1}});
        assert_eq!(
            changes(was.as_object().unwrap(), now.as_object().unwrap()),
            r#"checks "running" → "ci=success", comments 1 → 2, reviews null → {"APPROVED":1}"#
        );
        assert_eq!(
            changes(now.as_object().unwrap(), now.as_object().unwrap()),
            ""
        );
    }
}
