//! What a job awaits on GitHub: a PR or issue ([`target`]), and what about
//! it is news ([`fingerprint`]).

use burrow_github::{Gh, call};
use std::collections::BTreeMap;
use std::time::Instant;

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
    fingerprint_until(cfg, repo, n, None)
}

/// One absolute deadline covers every request, including gh's pagination.
pub(crate) fn fingerprint_until(
    cfg: &Config,
    repo: &str,
    n: &str,
    until: Option<Instant>,
) -> Option<Value> {
    let gh = |path: &str| api_until(cfg, path, until);
    // A comment, review or check past the first hundred is news too.
    let pages = |path: String| every_until(cfg, &path, until);
    let issue = gh(&format!("repos/{repo}/issues/{n}"))?;
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
    let pr = gh(&format!("repos/{repo}/pulls/{n}"))?;
    let sha = pr["head"]["sha"].as_str().unwrap_or_default().to_owned();
    // Reviews are counted by state, so a dismissed one is news as well as a
    // new one.
    let mut reviews: BTreeMap<String, usize> = BTreeMap::new();
    for r in pages(format!("repos/{repo}/pulls/{n}/reviews?per_page=100"))? {
        *reviews.entry(text(&r["state"])).or_default() += 1;
    }
    let status = gh(&format!("repos/{repo}/commits/{sha}/status"))?;
    let statuses = status["total_count"].as_u64().unwrap_or(0) > 0;
    let runs: Vec<Value> = pages_until(
        cfg,
        &format!("repos/{repo}/commits/{sha}/check-runs?per_page=100"),
        until,
    )?
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

/// What a [`fingerprint`] says its PR or issue is once it's over: merged, or
/// closed without; None while it's open. Nothing a job awaits can come of
/// one that's over, so `proj await` refuses it: awaited, it would hold the
/// job until awaitHours ran out.
pub fn over(fp: &Value) -> Option<&'static str> {
    if fp["merged"] == true {
        Some("merged")
    } else if fp["state"] == "closed" {
        Some("closed")
    } else {
        None
    }
}

/// The configured GitHub client, with the caller's authentication routing.
pub(crate) fn client(cfg: &Config) -> Gh {
    Gh {
        program: Some(cfg.tools.gh.clone().into_os_string()),
        ..Default::default()
    }
}

/// One REST answer, or None when GitHub cannot be read.
pub fn api(cfg: &Config, path: &str) -> Option<Value> {
    api_until(cfg, path, None)
}

fn api_until(cfg: &Config, path: &str, until: Option<Instant>) -> Option<Value> {
    let mut gh = client(cfg);
    gh.until = until;
    call(&mut gh, "GET", path, None, None).ok()?.json().ok()
}

/// GraphQL fields are data, not authentication routing. Pin the repository.
pub fn query_for(cfg: &Config, repo: &str, query: &str, variables: Value) -> Option<Value> {
    let mut gh = client(cfg);
    gh.env.push(("GH_REPO".into(), repo.into()));
    // REST's repository routing takes precedence over the account default.
    // Use call_repeating for this read-only POST, preserving that routing.
    let body = json!({"query": query, "variables": variables});
    let got = burrow_github::call_repeating(&mut gh, "POST", "graphql", Some(&body))
        .ok()?
        .json()
        .ok()?;
    if got.get("errors").is_some() {
        return None;
    }
    Some(got["data"].clone())
}

fn pages_until(cfg: &Config, path: &str, until: Option<Instant>) -> Option<Vec<Value>> {
    let mut gh = client(cfg);
    gh.until = until;
    burrow_github::pages(&mut gh, path).ok()
}

/// Every page of a list, as one list.
pub fn every(cfg: &Config, path: &str) -> Option<Vec<Value>> {
    every_until(cfg, path, None)
}

fn every_until(cfg: &Config, path: &str, until: Option<Instant>) -> Option<Vec<Value>> {
    let mut all = Vec::new();
    for page in pages_until(cfg, path, until)? {
        all.extend(page.as_array()?.iter().cloned());
    }
    Some(all)
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
    use std::{fs, os::unix::fs::PermissionsExt};

    use super::{changes, over, target};

    #[test]
    fn github_recovers_after_502() {
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        fs::write(
            &gh,
            r#"#!/bin/sh
cd "$(dirname "$0")"
# Both CLI shapes let this test run against the unported client too.
headers=0; [ "$2" != -i ] || headers=1
case "$*" in
  */comments*)
    echo asked >> attempts
    if [ ! -e failed ]; then
      touch failed
      printf 'HTTP/2.0 502 Bad Gateway\n\n{}'; exit 1
    fi
    [ "$headers" = 0 ] || printf 'HTTP/2.0 200 OK\n\n'
    if [ "$headers" = 1 ]; then echo '[{"body":"reply"}]'; else echo '[[{"body":"reply"}]]'; fi ;;
  *)
    [ "$headers" = 0 ] || printf 'HTTP/2.0 200 OK\n\n'
    echo '{"state":"open"}' ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        let mut config = crate::config::tests::example();
        config["tools"]["gh"] = json!(gh);
        let cfg = serde_json::from_value(config).unwrap();
        assert_eq!(
            super::fingerprint(&cfg, "sam/r", "1"),
            Some(json!({"state":"open","comments":1}))
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("attempts"))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }

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

    // Merged, closed without, or open: a PR's fingerprint and an issue's.
    #[test]
    fn what_is_over() {
        let pr = |state: &str, merged: bool| json!({"state": state, "merged": merged, "checks": "ci=success"});
        assert_eq!(over(&pr("closed", true)), Some("merged"));
        assert_eq!(over(&pr("closed", false)), Some("closed"));
        assert_eq!(over(&pr("open", false)), None);
        assert_eq!(
            over(&json!({"state": "closed", "comments": 3})),
            Some("closed")
        );
        assert_eq!(over(&json!({"state": "open", "comments": 3})), None);
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
