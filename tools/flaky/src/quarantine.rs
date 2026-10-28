//! The tests a repo quarantines: one open issue each (test.rs), opened by
//! the repo's own GitHub Actions or by someone who can push to it. A list is
//! one request, and one more for each person who opened one; with a cache,
//! each is a conditional one, which GitHub doesn't count against the login's
//! rate limit when nothing changed.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::github::{Api, Error, call};
use crate::test::{ACTIONS, LABEL, can_push, quarantined_by};

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
    let kept = file.as_deref().and_then(read).unwrap_or(Value::Null);
    let first = call(api, "GET", &path, None, kept["etag"].as_str())?;
    let (etag, issues) = if first.status == 304 {
        (
            kept["etag"].as_str().map(String::from),
            kept["issues"].clone(),
        )
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
        let etag = first.header("etag").filter(|_| one).map(String::from);
        (etag, Value::Array(all.iter().map(keep).collect()))
    };
    let mut pushers = Map::new();
    let mut out = Vec::new();
    for i in issues.as_array().into_iter().flatten() {
        let (Some(test), Some(issue), Some(author)) = (
            quarantined_by(i),
            i["number"].as_u64(),
            i["user"]["login"].as_str(),
        ) else {
            continue;
        };
        let trusted = if author == ACTIONS {
            true
        } else if i["user"]["type"].as_str() == Some("Bot") {
            false
        } else {
            pushes(api, repo, author, &kept["pushers"][author], &mut pushers)?
        };
        if trusted {
            out.push(Quarantined {
                test,
                issue,
                author: author.to_string(),
            });
        }
    }
    if let Some(f) = &file {
        match etag {
            Some(tag) => write(
                f,
                &json!({"etag": tag, "issues": issues, "pushers": pushers}),
            ),
            None => {
                let _ = fs::remove_file(f);
            }
        }
    }
    out.sort_by(|a, b| (&a.test, a.issue).cmp(&(&b.test, b.issue)));
    Ok(out)
}

/// Whether `login`, a person, can push to the repo: asked once a list (and
/// noted in `pushers`), conditionally with `kept`, the last answer. Being an
/// owner, a member of its organization or a collaborator isn't enough: a
/// member can have only triage, which labels an issue but pushes nothing.
fn pushes(
    api: &mut dyn Api,
    repo: &str,
    login: &str,
    kept: &Value,
    pushers: &mut Map<String, Value>,
) -> Result<bool, Error> {
    if let Some(p) = pushers.get(login) {
        return Ok(p["permission"].as_str().is_some_and(can_push));
    }
    let path = format!("repos/{repo}/collaborators/{login}/permission");
    let now = match call(api, "GET", &path, None, kept["etag"].as_str()) {
        Ok(r) if r.status == 304 => kept.clone(),
        Ok(r) => json!({"etag": r.header("etag"), "permission": r.json()?["permission"]}),
        // No such person any more.
        Err(e) if e.0.contains("HTTP 404") => json!({"permission": "none"}),
        Err(e) => return Err(e),
    };
    let can = now["permission"].as_str().is_some_and(can_push);
    pushers.insert(login.to_string(), now);
    Ok(can)
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
        "state_reason": i["state_reason"],
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

    fn issue(n: u64, title: &str, login: &str) -> Value {
        let kind = if login.ends_with("[bot]") {
            "Bot"
        } else {
            "User"
        };
        json!({"number": n, "title": title, "labels": [{"name": "flaky", "color": "d93f0b"}],
               "user": {"login": login, "type": kind}, "body": "long"})
    }

    const PATH: &str = "repos/sam/game/issues?labels=flaky&state=open&per_page=100";

    fn may(
        gh: &mut Fake,
        login: &str,
        answers: Vec<Result<crate::github::Response, crate::github::Failed>>,
    ) {
        gh.on(
            "GET",
            &format!("repos/sam/game/collaborators/{login}/permission"),
            answers,
        );
    }

    fn can(permission: &str) -> Result<crate::github::Response, crate::github::Failed> {
        ok(
            200,
            json!({"permission": permission, "role_name": "x"}),
            &[],
        )
    }

    #[test]
    fn who_quarantines() {
        let mut gh = Fake::default();
        gh.on(
            "GET",
            PATH,
            vec![ok(
                200,
                json!([
                    issue(9, "flaky: b", "sam"),
                    issue(4, "flaky: a", ACTIONS),
                    // A member with triage, or a collaborator with read: they
                    // can label an issue, not push.
                    issue(3, "flaky: b", "tri"),
                    issue(2, "flaky: b", "wri"),
                    issue(1, "not one", "sam"),
                    // Another app on the repo.
                    issue(5, "flaky: c", "renovate[bot]"),
                    // Someone who's gone.
                    issue(6, "flaky: d", "gone"),
                    issue(8, "flaky: e", "sam"),
                ]),
                &[],
            )],
        );
        may(&mut gh, "sam", vec![can("admin")]);
        may(&mut gh, "tri", vec![can("read")]);
        may(&mut gh, "wri", vec![can("write")]);
        may(
            &mut gh,
            "gone",
            vec![ok(404, json!({"message": "Not Found"}), &[])],
        );
        let got = list(&mut gh, "sam/game", None).unwrap();
        let q = |t: &str, n, a: &str| Quarantined {
            test: t.into(),
            issue: n,
            author: a.into(),
        };
        assert_eq!(
            got,
            vec![
                q("a", 4, ACTIONS),
                q("b", 2, "wri"),
                q("b", 9, "sam"),
                q("e", 8, "sam")
            ]
        );
        // Each person once; no app.
        let asked: Vec<&str> = gh.asked.iter().map(|a| a.1.as_str()).collect();
        assert_eq!(
            asked,
            [
                PATH,
                "repos/sam/game/collaborators/sam/permission",
                "repos/sam/game/collaborators/tri/permission",
                "repos/sam/game/collaborators/wri/permission",
                "repos/sam/game/collaborators/gone/permission"
            ]
        );
    }

    #[test]
    fn a_permission_it_cant_read_is_a_failure() {
        let mut gh = Fake::default();
        gh.on(
            "GET",
            PATH,
            vec![ok(200, json!([issue(9, "flaky: b", "sam")]), &[])],
        );
        may(
            &mut gh,
            "sam",
            vec![ok(403, json!({"message": "Resource not accessible"}), &[])],
        );
        let e = list(&mut gh, "sam/game", None).unwrap_err();
        assert!(e.0.contains("HTTP 403"), "{e}");
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
                ok(
                    200,
                    json!([issue(4, "flaky: a", "sam")]),
                    &[("ETag", "W/\"one\"")],
                ),
                ok(304, Value::Null, &[("ETag", "W/\"one\"")]),
            ],
        );
        may(
            &mut gh,
            "sam",
            vec![
                ok(200, json!({"permission": "write"}), &[("ETag", "W/\"w\"")]),
                ok(304, Value::Null, &[]),
                // Then made triage only.
                ok(200, json!({"permission": "read"}), &[("ETag", "W/\"r\"")]),
            ],
        );
        let first = list(&mut gh, "sam/game", Some(&dir)).unwrap();
        let second = list(&mut gh, "sam/game", Some(&dir)).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first, second);
        let etags: Vec<Option<&str>> = gh.asked.iter().map(|a| a.3.as_deref()).collect();
        assert_eq!(etags, [None, None, Some("W/\"one\""), Some("W/\"w\"")]);
        // Only what quarantined_by and list read is kept.
        let kept = fs::read_to_string(dir.join("sam/game.json")).unwrap();
        assert!(!kept.contains("long") && !kept.contains("d93f0b"), "{kept}");
        assert!(list(&mut gh, "sam/game", Some(&dir)).unwrap().is_empty());
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
            vec![ok(200, json!([issue(2, "flaky: z", ACTIONS)]), &[])],
        );
        gh.on(
            "GET",
            PATH,
            vec![ok(
                200,
                json!([issue(1, "flaky: a", ACTIONS)]),
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
