//! The tests a repo quarantines: one open issue each (test.rs). A list is
//! one request, and with a cache a conditional one, which GitHub doesn't
//! count against the login's rate limit when nothing changed.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::github::{Api, Error, call};
use crate::test::{LABEL, quarantined_by};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quarantined {
    pub test: String,
    pub issue: u64,
    /// Who opened the issue: their login (an app's ends in `[bot]`).
    pub author: String,
}

/// Every open issue that quarantines a test, by test, then issue (a test
/// with two has its oldest first).
pub fn list(
    api: &mut dyn Api,
    repo: &str,
    cache: Option<&Path>,
) -> Result<Vec<Quarantined>, Error> {
    let path = format!("repos/{repo}/issues?labels={LABEL}&state=open&per_page=100");
    let file = cache.map(|d| d.join(format!("{repo}.json")));
    let kept = file.as_deref().and_then(read);
    let etag = kept.as_ref().and_then(|k| k["etag"].as_str());
    let first = call(api, "GET", &path, None, etag)?;
    let issues = if first.status == 304 {
        kept.as_ref()
            .map(|k| k["issues"].clone())
            .unwrap_or_default()
    } else {
        let mut all = page(&first)?;
        let mut next = first.next();
        // A cached first page can't vouch for the others.
        let one = next.is_none();
        while let Some(p) = next {
            let r = call(api, "GET", &p, None, None)?;
            all.extend(page(&r)?);
            next = r.next();
        }
        let issues = Value::Array(all.iter().map(keep).collect());
        if let Some(f) = &file {
            match first.header("etag").filter(|_| one) {
                Some(tag) => write(f, &json!({"etag": tag, "issues": issues})),
                None => {
                    let _ = fs::remove_file(f);
                }
            }
        }
        issues
    };
    let mut out: Vec<Quarantined> = issues
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| {
            Some(Quarantined {
                test: quarantined_by(i)?,
                issue: i["number"].as_u64()?,
                author: i["user"]["login"].as_str()?.to_string(),
            })
        })
        .collect();
    out.sort_by(|a, b| (&a.test, a.issue).cmp(&(&b.test, b.issue)));
    Ok(out)
}

fn page(r: &crate::github::Response) -> Result<Vec<Value>, Error> {
    match r.json()? {
        Value::Array(a) => Ok(a),
        _ => Err(Error("GitHub's list of issues isn't a list".into())),
    }
}

/// What quarantined_by and list read of an issue, which is all a cache
/// keeps.
fn keep(i: &Value) -> Value {
    json!({
        "number": i["number"],
        "title": i["title"],
        "labels": i["labels"].as_array().into_iter().flatten().map(|l| json!({"name": l["name"]})).collect::<Vec<_>>(),
        "author_association": i["author_association"],
        "user": {"login": i["user"]["login"], "type": i["user"]["type"]},
        "pull_request": if i.get("pull_request").is_some_and(|p| !p.is_null()) { json!({}) } else { Value::Null },
    })
}

fn read(f: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(f).ok()?).ok()
}

/// Best effort: a cache that can't be written costs the next list a full
/// request, nothing more.
fn write(f: &Path, v: &Value) {
    let tmp = PathBuf::from(format!("{}.{}.tmp", f.display(), std::process::id()));
    let ok = f.parent().is_some_and(|d| fs::create_dir_all(d).is_ok())
        && fs::write(&tmp, v.to_string()).is_ok()
        && fs::rename(&tmp, f).is_ok();
    if !ok {
        let _ = fs::remove_file(&tmp);
    }
}

/// Where a cache goes by default: $XDG_CACHE_HOME/flaky, or ~/.cache/flaky.
pub fn default_cache() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".cache"))
        })?;
    Some(base.join("flaky"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::fake::{Fake, ok};

    fn issue(n: u64, title: &str) -> Value {
        json!({"number": n, "title": title, "labels": [{"name": "flaky", "color": "d93f0b"}],
               "author_association": "OWNER", "user": {"login": "sam", "type": "User"}, "body": "long"})
    }

    const PATH: &str = "repos/sam/game/issues?labels=flaky&state=open&per_page=100";

    #[test]
    fn lists_by_test() {
        let mut gh = Fake::default();
        let mut stranger = issue(3, "flaky: b");
        stranger["author_association"] = json!("NONE");
        gh.on(
            "GET",
            PATH,
            vec![ok(
                200,
                json!([
                    issue(9, "flaky: b"),
                    issue(4, "flaky: a"),
                    stranger,
                    issue(2, "flaky: b"),
                    issue(1, "not one")
                ]),
                &[],
            )],
        );
        let got = list(&mut gh, "sam/game", None).unwrap();
        let q = |t: &str, n| Quarantined {
            test: t.into(),
            issue: n,
            author: "sam".into(),
        };
        assert_eq!(got, vec![q("a", 4), q("b", 2), q("b", 9)]);
    }

    #[test]
    fn asks_again_cheaply() {
        let dir = std::env::temp_dir().join(format!("flaky-cache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut gh = Fake::default();
        gh.on(
            "GET",
            PATH,
            vec![
                ok(200, json!([issue(4, "flaky: a")]), &[("ETag", "W/\"one\"")]),
                ok(304, Value::Null, &[("ETag", "W/\"one\"")]),
            ],
        );
        let first = list(&mut gh, "sam/game", Some(&dir)).unwrap();
        let second = list(&mut gh, "sam/game", Some(&dir)).unwrap();
        assert_eq!(first, second);
        assert_eq!(gh.asked[0].3, None);
        assert_eq!(gh.asked[1].3.as_deref(), Some("W/\"one\""));
        // Only what quarantined_by reads is kept.
        let kept = fs::read_to_string(dir.join("sam/game.json")).unwrap();
        assert!(!kept.contains("long") && !kept.contains("d93f0b"), "{kept}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pages() {
        let dir = std::env::temp_dir().join(format!("flaky-pages-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut gh = Fake::default();
        let link =
            "<https://api.github.com/repositories/1/issues?labels=flaky&page=2>; rel=\"next\"";
        gh.on(
            "GET",
            "repositories/1/issues",
            vec![ok(200, json!([issue(2, "flaky: z")]), &[])],
        );
        gh.on(
            "GET",
            PATH,
            vec![ok(
                200,
                json!([issue(1, "flaky: a")]),
                &[("ETag", "W/\"p1\""), ("Link", link)],
            )],
        );
        let got = list(&mut gh, "sam/game", Some(&dir)).unwrap();
        assert_eq!(got.len(), 2);
        // A first page's etag says nothing of the second.
        assert!(!dir.join("sam/game.json").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
