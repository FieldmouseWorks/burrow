//! The tests a repo quarantines: one open issue each (test.rs), opened, and
//! if retitled or labelled since, changed by the repo's own GitHub Actions or
//! by someone who can push to it. And the tests whose quarantine has run
//! out, which goes by every open `flaky` issue, whoever opened or changed
//! it. A list is one request, one more for each issue's history, and one
//! for each person in the histories of the issues that quarantine; with a
//! cache, each is a conditional one, which GitHub doesn't count against the
//! login's rate limit when nothing changed.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use std::time::{SystemTime, UNIX_EPOCH};

use crate::test::{ACTIONS, EXPIRES, LABEL, PREFIX, can_push, quarantined_by, seconds, valid};
use burrow_github::{Api, Error, call, call_allowing};

/// What a cache holds: one written in another shape is read as none.
const CACHE: u64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quarantined {
    pub test: String,
    pub issue: u64,
    /// Who opened the issue: their login (an app's ends in `[bot]`).
    pub author: String,
}

/// A repo's quarantine, as of when it was listed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Quarantine {
    /// Every open issue that quarantines a test, by test, then issue (a
    /// test with two has its oldest first).
    pub held: Vec<Quarantined>,
    /// The tests whose quarantine has run out (test::EXPIRES), each once,
    /// sorted. This fails closed: it counts every test an open `flaky` issue
    /// names or has named, whoever opened, retitled or relabelled it, so
    /// nobody who can't push can give a test back its second try that way.
    /// An issue whose opening time can't be read has run out.
    pub expired: Vec<String>,
}

/// The repo's quarantine as of `now`.
pub fn list(
    api: &mut dyn Api,
    repo: &str,
    cache: Option<&Path>,
    now: u64,
) -> Result<Quarantine, Error> {
    let path = format!("repos/{repo}/issues?labels={LABEL}&state=open&per_page=100");
    let file = cache.map(|d| d.join(format!("{repo}.json")));
    let kept = file
        .as_deref()
        .and_then(read)
        .filter(|k| k["v"] == CACHE)
        .unwrap_or(Value::Null);
    let (etag, issues) = all(api, &path, &kept["issues"], keep)?;
    let mut who = Who {
        repo,
        kept: &kept["pushers"],
        pushers: Map::new(),
    };
    let mut histories = Map::new();
    let mut out = Quarantine::default();
    for i in issues.as_array().into_iter().flatten() {
        let Some(issue) = i["number"].as_u64() else {
            continue;
        };
        if i["pull_request"].is_object() {
            continue;
        }
        let n = issue.to_string();
        let path = format!("repos/{repo}/issues/{n}/events?per_page=100");
        let (tag, changes) = all(api, &path, &kept["histories"][n.as_str()], change)?;
        histories.insert(n, json!({"etag": tag, "items": changes}));
        let changes: Vec<&Value> = changes
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| !c.is_null())
            .collect();
        let opened = i["created_at"].as_str().and_then(seconds).unwrap_or(0);
        if now >= opened.saturating_add(EXPIRES) {
            out.expired.extend(names(i, &changes));
        }
        let (Some(test), Some(author)) = (quarantined_by(i), i["user"]["login"].as_str()) else {
            continue;
        };
        if !who.may(api, &i["user"])? {
            continue;
        }
        let mut vouched = true;
        for c in &changes {
            vouched = vouched && who.may(api, &c["actor"])?;
        }
        if vouched {
            out.held.push(Quarantined {
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
                &json!({"v": CACHE, "issues": {"etag": tag, "items": issues}, "pushers": who.pushers, "histories": histories}),
            ),
            None => {
                let _ = fs::remove_file(f);
            }
        }
    }
    out.held
        .sort_by(|a, b| (&a.test, a.issue).cmp(&(&b.test, b.issue)));
    out.expired.sort();
    out.expired.dedup();
    Ok(out)
}

/// Every test an issue names or has named: by its title, and by each title
/// a retitle took from it or gave it.
fn names(issue: &Value, changes: &[&Value]) -> Vec<String> {
    std::iter::once(&issue["title"])
        .chain(
            changes
                .iter()
                .flat_map(|c| [&c["rename"]["from"], &c["rename"]["to"]]),
        )
        .filter_map(|t| t.as_str()?.strip_prefix(PREFIX))
        .filter(|t| valid(t))
        .map(String::from)
        .collect()
}

/// The tests quarantined, each once.
pub fn tests(q: &[Quarantined]) -> Vec<&str> {
    let mut t: Vec<&str> = q.iter().map(|x| x.test.as_str()).collect();
    t.dedup();
    t
}

/// Whether `r` is OWNER/REPO as GitHub allows them: it goes into the cache's
/// paths. A repo's name may start with a dot (`.github`), but isn't `.` or
/// `..`.
pub fn repo_name(r: &str) -> bool {
    let ok = |s: &str, extra: &str| {
        !s.is_empty()
            && !s.starts_with('-')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-".contains(c) || extra.contains(c))
    };
    r.split_once('/').is_some_and(|(o, n)| {
        ok(o, "") && !o.starts_with('.') && ok(n, "_.") && n != "." && n != ".."
    })
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
        let r = call_allowing(api, "GET", &path, None, kept["etag"].as_str(), &[404])?;
        let now = match r.status {
            304 => kept.clone(),
            // No such person any more.
            404 => json!({"permission": "none"}),
            _ => json!({"etag": r.header("etag"), "permission": r.json()?["permission"]}),
        };
        let can = now["permission"].as_str().is_some_and(can_push);
        self.pushers.insert(login.to_string(), now);
        Ok(can)
    }
}

/// Of an issue's history, what changes the test it quarantines: a retitle,
/// or its label put on. Who did it, and a retitle's titles, are all a cache
/// keeps; anything else is null.
fn change(e: &Value) -> Value {
    let actor = json!({"login": e["actor"]["login"], "type": e["actor"]["type"]});
    if e["event"] == "renamed" {
        json!({"actor": actor, "rename": {"from": e["rename"]["from"], "to": e["rename"]["to"]}})
    } else if e["event"] == "labeled" && e["label"]["name"] == LABEL {
        json!({"actor": actor})
    } else {
        Value::Null
    }
}

fn page(r: &burrow_github::Response) -> Result<Vec<Value>, Error> {
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
    use burrow_github::fake::{Fake, ok};
    use burrow_github::{Failed, Response};

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
    fn a_repo_may_start_with_a_dot() {
        for r in ["FieldmouseWorks/.github", "o/a.b_c-d", "o-1/r"] {
            assert!(repo_name(r), "{r}");
        }
    }

    #[test]
    fn what_could_leave_the_cache_is_refused() {
        for r in [
            "o/.", "o/..", ".o/r", "-o/r", "o/-r", "o/r/x", "o", "/r", "o/", "o_x/r",
        ] {
            assert!(!repo_name(r), "{r}");
        }
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
        gh.on(
            "GET",
            "repos/sam/game/issues/",
            vec![ok(200, json!([]), &[])],
        );
        let got = list(&mut gh, "sam/game", None, NOW).unwrap();
        let q = |t: &str, n, a: &str| Quarantined {
            test: t.into(),
            issue: n,
            author: a.into(),
        };
        assert_eq!(
            got,
            Quarantine {
                held: vec![
                    q("a", 4, ACTIONS),
                    q("b", 2, "wri"),
                    q("b", 9, "sam"),
                    q("e", 8, "sam")
                ],
                expired: vec![]
            }
        );
        // Every issue's history, since any of them can run a test's
        // quarantine out; each person once, and no app's permission.
        let asked: Vec<&str> = gh.asked.iter().map(|a| a.1.as_str()).collect();
        assert_eq!(
            asked,
            [
                PATH,
                "repos/sam/game/issues/9/events?per_page=100",
                "repos/sam/game/collaborators/sam/permission",
                "repos/sam/game/issues/4/events?per_page=100",
                "repos/sam/game/issues/3/events?per_page=100",
                "repos/sam/game/collaborators/tri/permission",
                "repos/sam/game/issues/2/events?per_page=100",
                "repos/sam/game/collaborators/wri/permission",
                "repos/sam/game/issues/1/events?per_page=100",
                "repos/sam/game/issues/5/events?per_page=100",
                "repos/sam/game/issues/6/events?per_page=100",
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
            .held
            .iter()
            .map(|q| q.issue)
            .collect();
        assert_eq!(got, [6, 7]);
    }

    #[test]
    fn running_out_fails_closed() {
        let mut gh = Fake::default();
        // Three days old, all of them.
        let old = |n, title: &str, login: &str| {
            let mut i = issue(n, title, login);
            i["created_at"] = json!("2026-10-25T23:28:42Z");
            i
        };
        let mut reopened = old(7, "flaky: reopened", ACTIONS);
        reopened["state_reason"] = json!("reopened");
        let mut pr = old(9, "flaky: a-pr", ACTIONS);
        pr["pull_request"] = json!({"url": "x"});
        gh.on(
            "GET",
            PATH,
            vec![ok(
                200,
                json!([
                    old(4, "flaky: b-now", ACTIONS),
                    old(5, "flaky: relabelled", ACTIONS),
                    old(6, "flaky: by-triage", "tri"),
                    reopened,
                    old(8, "not a test", ACTIONS),
                    pr,
                    old(10, "flaky: no good!", ACTIONS),
                ]),
                &[],
            )],
        );
        may(&mut gh, "tri", vec![can("read")]);
        let renamed = |from: &str, to: &str| json!({"event": "renamed", "actor": user("tri"), "rename": {"from": from, "to": to}});
        // Triage retitled it to name another test.
        gh.on(
            "GET",
            "repos/sam/game/issues/4/events",
            vec![ok(
                200,
                json!([renamed("flaky: a-before", "flaky: b-now")]),
                &[],
            )],
        );
        // Triage took its label off and put it back.
        history(
            &mut gh,
            5,
            &[("unlabeled", "tri", "flaky"), ("labeled", "tri", "flaky")],
        );
        // Triage retitled it to name none.
        gh.on(
            "GET",
            "repos/sam/game/issues/8/events",
            vec![ok(
                200,
                json!([renamed("flaky: was-a-test", "not a test")]),
                &[],
            )],
        );
        gh.on(
            "GET",
            "repos/sam/game/issues/",
            vec![ok(200, json!([]), &[])],
        );
        let got = list(&mut gh, "sam/game", None, NOW).unwrap();
        // None of them quarantines, and nobody who can't push gave a test
        // its second try back.
        assert_eq!(got.held, vec![]);
        assert_eq!(
            got.expired,
            [
                "a-before",
                "b-now",
                "by-triage",
                "relabelled",
                "reopened",
                "was-a-test"
            ]
        );
        assert!(!gh.asked.iter().any(|a| a.1.contains("issues/9/")));
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
        let held: Vec<u64> = got.held.iter().map(|q| q.issue).collect();
        assert_eq!(held, [4, 2, 5, 6, 3, 1]);
        assert_eq!(got.expired, ["odd", "old", "twice", "unknown"]);
    }

    #[test]
    fn a_permission_it_cant_read_is_a_failure() {
        let mut gh = Fake::default();
        gh.on(
            "GET",
            PATH,
            vec![ok(200, json!([issue(9, "flaky: b", "sam")]), &[])],
        );
        history(&mut gh, 9, &[]);
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
        assert_eq!(first.held.len(), 1);
        assert_eq!(first, second);
        let etags: Vec<Option<&str>> = gh.asked.iter().map(|a| a.3.as_deref()).collect();
        assert_eq!(
            etags,
            [
                None,
                None,
                None,
                Some("W/\"one\""),
                Some("W/\"h\""),
                Some("W/\"w\"")
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
                .held
                .is_empty()
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_cache_keeps_what_a_test_was_called() {
        let dir = std::env::temp_dir().join(format!("flaky-names-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        // A cache in an older shape, which kept no titles: never read.
        fs::create_dir_all(dir.join("sam")).unwrap();
        fs::write(
            dir.join("sam/game.json"),
            json!({"issues": {"etag": "W/\"old\"", "items": []}}).to_string(),
        )
        .unwrap();
        let mut i = issue(4, "flaky: b", ACTIONS);
        i["created_at"] = json!("2026-10-25T23:28:42Z");
        let mut gh = Fake::default();
        gh.on(
            "GET",
            PATH,
            vec![
                ok(200, json!([i]), &[("ETag", "W/\"one\"")]),
                ok(304, Value::Null, &[]),
            ],
        );
        gh.on(
            "GET",
            "repos/sam/game/issues/4/events",
            vec![
                ok(
                    200,
                    json!([{"event": "renamed", "actor": user(ACTIONS), "rename": {"from": "flaky: a", "to": "flaky: b"}}]),
                    &[("ETag", "W/\"h\"")],
                ),
                ok(304, Value::Null, &[]),
            ],
        );
        let first = list(&mut gh, "sam/game", Some(&dir), NOW).unwrap();
        let second = list(&mut gh, "sam/game", Some(&dir), NOW).unwrap();
        assert_eq!(first.expired, ["a", "b"]);
        assert_eq!(first, second);
        assert_eq!(gh.asked[0].3, None);
        assert_eq!(gh.asked[3].3.as_deref(), Some("W/\"h\""));
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
        assert_eq!(got.held.len(), 2);
        // A first page's etag says nothing of the second.
        assert!(!dir.join("sam/game.json").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
