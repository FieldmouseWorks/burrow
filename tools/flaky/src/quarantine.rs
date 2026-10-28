//! The tests a repo quarantines: one open issue each (test.rs), opened, and
//! if retitled or labelled since, changed by the repo's own GitHub Actions or
//! by someone who can push to it. A list is one request, one more for each
//! such issue's history, and one for each person in it; with a cache, each is
//! a conditional one, which GitHub doesn't count against the login's rate
//! limit when nothing changed.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use std::time::{SystemTime, UNIX_EPOCH};

use crate::github::{Api, Error, call};
use crate::test::{ACTIONS, EXPIRES, LABEL, can_push, quarantined_by, seconds};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quarantined {
    pub test: String,
    pub issue: u64,
    /// Who opened the issue: their login (an app's ends in `[bot]`).
    pub author: String,
    /// Whether its quarantine had run out (test::EXPIRES) when it was listed.
    pub expired: bool,
}

/// Every open issue that quarantines a test, by test, then issue (a test
/// with two has its oldest first), as of `now`.
pub fn list(
    api: &mut dyn Api,
    repo: &str,
    cache: Option<&Path>,
    now: u64,
) -> Result<Vec<Quarantined>, Error> {
    let path = format!("repos/{repo}/issues?labels={LABEL}&state=open&per_page=100");
    let file = cache.map(|d| d.join(format!("{repo}.json")));
    let kept = file.as_deref().and_then(read).unwrap_or(Value::Null);
    let (etag, issues) = all(api, &path, &kept["issues"], keep)?;
    let mut who = Who {
        repo,
        kept: &kept["pushers"],
        pushers: Map::new(),
    };
    let mut histories = Map::new();
    let mut out = Vec::new();
    for i in issues.as_array().into_iter().flatten() {
        let (Some(test), Some(issue), Some(author)) = (
            quarantined_by(i),
            i["number"].as_u64(),
            i["user"]["login"].as_str(),
        ) else {
            continue;
        };
        if !who.may(api, &i["user"])? {
            continue;
        }
        let n = issue.to_string();
        let path = format!("repos/{repo}/issues/{n}/events?per_page=100");
        let (tag, changes) = all(api, &path, &kept["histories"][n.as_str()], change)?;
        histories.insert(n, json!({"etag": tag, "items": changes}));
        let mut vouched = true;
        for c in changes
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| !c.is_null())
        {
            vouched = vouched && who.may(api, &c["actor"])?;
        }
        if vouched {
            // One whose opening can't be read has run out: its failures fail.
            let opened = i["created_at"].as_str().and_then(seconds).unwrap_or(0);
            out.push(Quarantined {
                test,
                issue,
                author: author.to_string(),
                expired: now >= opened.saturating_add(EXPIRES),
            });
        }
    }
    if let Some(f) = &file {
        match etag {
            Some(tag) => write(
                f,
                &json!({"issues": {"etag": tag, "items": issues}, "pushers": who.pushers, "histories": histories}),
            ),
            None => {
                let _ = fs::remove_file(f);
            }
        }
    }
    out.sort_by(|a, b| (&a.test, a.issue).cmp(&(&b.test, b.issue)));
    Ok(out)
}

/// The tests quarantined, each once.
pub fn tests(q: &[Quarantined]) -> Vec<&str> {
    let mut t: Vec<&str> = q.iter().map(|x| x.test.as_str()).collect();
    t.dedup();
    t
}

/// The tests whose quarantine had run out, each once: one with an issue
/// that has, though a newer one hasn't, is as old as its oldest.
pub fn expired(q: &[Quarantined]) -> Vec<&str> {
    let mut t: Vec<&str> = q
        .iter()
        .filter(|x| x.expired)
        .map(|x| x.test.as_str())
        .collect();
    t.dedup();
    t
}

/// Seconds since 1970, by this machine's clock.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Every page of a list: what `kept` (the last answer: `etag` and `items`)
/// had if GitHub says nothing changed, else each item as `pick` keeps it,
/// with the etag to ask with next time (none for more than one page: a
/// cached first page can't vouch for the others).
fn all(
    api: &mut dyn Api,
    path: &str,
    kept: &Value,
    pick: fn(&Value) -> Value,
) -> Result<(Option<String>, Value), Error> {
    let tag = kept["etag"].as_str();
    let first = call(api, "GET", path, None, tag)?;
    if first.status == 304 {
        return Ok((tag.map(String::from), kept["items"].clone()));
    }
    let mut items = page(&first)?;
    let mut next = first.next();
    let one = next.is_none();
    while let Some(p) = next {
        let r = call(api, "GET", &p, None, None)?;
        items.extend(page(&r)?);
        next = r.next();
    }
    let etag = first.header("etag").filter(|_| one).map(String::from);
    Ok((etag, Value::Array(items.iter().map(pick).collect())))
}

/// Who may quarantine a test, asked of GitHub once a list.
struct Who<'a> {
    repo: &'a str,
    /// The last list's answers, by login.
    kept: &'a Value,
    /// This one's.
    pushers: Map<String, Value>,
}

impl Who<'_> {
    /// Whether `user` (a login and a type, as GitHub gives them) may: the
    /// repo's own GitHub Actions, or a person who can push to it. Being an
    /// owner, a member of its organization or a collaborator isn't enough:
    /// a member can have only triage, which labels and retitles an issue
    /// but pushes nothing.
    fn may(&mut self, api: &mut dyn Api, user: &Value) -> Result<bool, Error> {
        let Some(login) = user["login"].as_str() else {
            return Ok(false); // a deleted account
        };
        if login == ACTIONS {
            return Ok(true);
        }
        if user["type"].as_str() == Some("Bot") {
            return Ok(false);
        }
        if let Some(p) = self.pushers.get(login) {
            return Ok(p["permission"].as_str().is_some_and(can_push));
        }
        let path = format!("repos/{}/collaborators/{login}/permission", self.repo);
        let kept = &self.kept[login];
        let now = match call(api, "GET", &path, None, kept["etag"].as_str()) {
            Ok(r) if r.status == 304 => kept.clone(),
            Ok(r) => json!({"etag": r.header("etag"), "permission": r.json()?["permission"]}),
            // No such person any more.
            Err(e) if e.0.contains("HTTP 404") => json!({"permission": "none"}),
            Err(e) => return Err(e),
        };
        let can = now["permission"].as_str().is_some_and(can_push);
        self.pushers.insert(login.to_string(), now);
        Ok(can)
    }
}

/// Of an issue's history, what changes the test it quarantines: a retitle,
/// or its label put on. Who did it is all a cache keeps; anything else is
/// null.
fn change(e: &Value) -> Value {
    let labelled = e["event"] == "labeled" && e["label"]["name"] == LABEL;
    if e["event"] == "renamed" || labelled {
        json!({"actor": {"login": e["actor"]["login"], "type": e["actor"]["type"]}})
    } else {
        Value::Null
    }
}

fn page(r: &crate::github::Response) -> Result<Vec<Value>, Error> {
    match r.json()? {
        Value::Array(a) => Ok(a),
        _ => Err(Error("GitHub's list isn't a list".into())),
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
        "created_at": i["created_at"],
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
    use crate::github::{Failed, Response};

    fn issue(n: u64, title: &str, login: &str) -> Value {
        json!({"number": n, "title": title, "labels": [{"name": "flaky", "color": "d93f0b"}],
               "user": user(login), "body": "long", "created_at": "2026-10-28T12:00:00Z"})
    }

    /// Eleven and a half hours after the issues above opened.
    const NOW: u64 = 1_793_230_122;

    fn user(login: &str) -> Value {
        let kind = if login.ends_with("[bot]") {
            "Bot"
        } else {
            "User"
        };
        json!({"login": login, "type": kind, "id": 1})
    }

    const PATH: &str = "repos/sam/game/issues?labels=flaky&state=open&per_page=100";

    fn may(gh: &mut Fake, login: &str, answers: Vec<Result<Response, Failed>>) {
        gh.on(
            "GET",
            &format!("repos/sam/game/collaborators/{login}/permission"),
            answers,
        );
    }

    fn can(permission: &str) -> Result<Response, Failed> {
        ok(
            200,
            json!({"permission": permission, "role_name": "x"}),
            &[],
        )
    }

    /// Issue n's history: (event, by whom, the label it's about).
    fn history(gh: &mut Fake, n: u64, events: &[(&str, &str, &str)]) {
        let events: Vec<Value> = events
            .iter()
            .map(|(e, by, label)| json!({"event": e, "actor": user(by), "label": {"name": label}, "rename": {"from": "a", "to": "b"}}))
            .collect();
        gh.on(
            "GET",
            &format!("repos/sam/game/issues/{n}/events"),
            vec![ok(200, Value::Array(events), &[])],
        );
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
        for n in [2, 4, 8, 9] {
            history(&mut gh, n, &[("labeled", "sam", "flaky")]);
        }
        let got = list(&mut gh, "sam/game", None, NOW).unwrap();
        let q = |t: &str, n, a: &str| Quarantined {
            test: t.into(),
            issue: n,
            author: a.into(),
            expired: false,
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
        // Each person once, and each history of an issue someone who may
        // opened; no app's permission.
        let asked: Vec<&str> = gh.asked.iter().map(|a| a.1.as_str()).collect();
        assert_eq!(
            asked,
            [
                PATH,
                "repos/sam/game/collaborators/sam/permission",
                "repos/sam/game/issues/9/events?per_page=100",
                "repos/sam/game/issues/4/events?per_page=100",
                "repos/sam/game/collaborators/tri/permission",
                "repos/sam/game/collaborators/wri/permission",
                "repos/sam/game/issues/2/events?per_page=100",
                "repos/sam/game/collaborators/gone/permission",
                "repos/sam/game/issues/8/events?per_page=100",
            ]
        );
    }

    #[test]
    fn who_changed_it_counts_too() {
        let mut gh = Fake::default();
        gh.on(
            "GET",
            PATH,
            vec![ok(
                200,
                json!([
                    issue(4, "flaky: retitled", ACTIONS),
                    issue(5, "flaky: relabelled", ACTIONS),
                    issue(6, "flaky: fixed-up", ACTIONS),
                    issue(7, "flaky: other-label", ACTIONS),
                    issue(8, "flaky: by-an-app", ACTIONS),
                ]),
                &[],
            )],
        );
        may(&mut gh, "tri", vec![can("read")]);
        may(&mut gh, "wri", vec![can("write")]);
        // Triage retitled it to name another test.
        history(
            &mut gh,
            4,
            &[("labeled", ACTIONS, "flaky"), ("renamed", "tri", "")],
        );
        // Triage took its label off and put it back.
        history(
            &mut gh,
            5,
            &[("unlabeled", "tri", "flaky"), ("labeled", "tri", "flaky")],
        );
        // Someone who can push retitled it; triage only closed and labelled
        // it otherwise.
        history(
            &mut gh,
            6,
            &[
                ("renamed", "wri", ""),
                ("labeled", "tri", "bug"),
                ("assigned", "tri", ""),
            ],
        );
        history(&mut gh, 7, &[("labeled", "tri", "needs-owner")]);
        history(&mut gh, 8, &[("labeled", "renovate[bot]", "flaky")]);
        let got: Vec<u64> = list(&mut gh, "sam/game", None, NOW)
            .unwrap()
            .iter()
            .map(|q| q.issue)
            .collect();
        assert_eq!(got, [6, 7]);
    }

    #[test]
    fn runs_out() {
        let mut gh = Fake::default();
        let at = |n, title, opened: Option<&str>| {
            let mut i = issue(n, title, ACTIONS);
            i["created_at"] = json!(opened);
            i
        };
        gh.on(
            "GET",
            PATH,
            vec![ok(
                200,
                json!([
                    // Two days, less a second, and exactly two days.
                    at(1, "flaky: young", Some("2026-10-26T23:28:43Z")),
                    at(2, "flaky: old", Some("2026-10-26T23:28:42Z")),
                    // No time it can read.
                    at(3, "flaky: unknown", None),
                    at(4, "flaky: odd", Some("yesterday")),
                    // A test with an old issue and a new one is as old as
                    // its oldest.
                    at(5, "flaky: twice", Some("2026-10-20T00:00:00Z")),
                    at(6, "flaky: twice", Some("2026-10-28T23:00:00Z")),
                ]),
                &[],
            )],
        );
        gh.on(
            "GET",
            "repos/sam/game/issues/",
            vec![ok(200, json!([]), &[])],
        );
        let got = list(&mut gh, "sam/game", None, NOW).unwrap();
        let flags: Vec<(u64, bool)> = got.iter().map(|q| (q.issue, q.expired)).collect();
        assert_eq!(
            flags,
            [
                (4, true),
                (2, true),
                (5, true),
                (6, false),
                (3, true),
                (1, false)
            ]
        );
        assert_eq!(expired(&got), ["odd", "old", "twice", "unknown"]);
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
        let e = list(&mut gh, "sam/game", None, NOW).unwrap_err();
        assert!(e.0.contains("HTTP 403"), "{e}");
        // And a history.
        let mut gh = Fake::default();
        gh.on(
            "GET",
            PATH,
            vec![ok(200, json!([issue(9, "flaky: b", ACTIONS)]), &[])],
        );
        gh.on(
            "GET",
            "repos/sam/game/issues/9/events",
            vec![ok(403, json!({"message": "Resource not accessible"}), &[])],
        );
        let e = list(&mut gh, "sam/game", None, NOW).unwrap_err();
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
        gh.on(
            "GET",
            "repos/sam/game/issues/4/events",
            vec![
                ok(
                    200,
                    json!([{"event": "labeled", "actor": user("sam"), "label": {"name": "flaky"}, "url": "long"}]),
                    &[("ETag", "W/\"h\"")],
                ),
                ok(304, Value::Null, &[]),
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
        let first = list(&mut gh, "sam/game", Some(&dir), NOW).unwrap();
        let second = list(&mut gh, "sam/game", Some(&dir), NOW).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first, second);
        let etags: Vec<Option<&str>> = gh.asked.iter().map(|a| a.3.as_deref()).collect();
        assert_eq!(
            etags,
            [
                None,
                None,
                None,
                Some("W/\"one\""),
                Some("W/\"w\""),
                Some("W/\"h\"")
            ]
        );
        // Only what quarantined_by and list read is kept.
        let kept = fs::read_to_string(dir.join("sam/game.json")).unwrap();
        assert!(
            !kept.contains("long") && !kept.contains("d93f0b") && kept.contains("W/\\\"h\\\""),
            "{kept}"
        );
        assert!(
            list(&mut gh, "sam/game", Some(&dir), NOW)
                .unwrap()
                .is_empty()
        );
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
        gh.on(
            "GET",
            "repos/sam/game/issues/",
            vec![ok(200, json!([]), &[])],
        );
        let got = list(&mut gh, "sam/game", Some(&dir), NOW).unwrap();
        assert_eq!(got.len(), 2);
        // A first page's etag says nothing of the second.
        assert!(!dir.join("sam/game.json").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
