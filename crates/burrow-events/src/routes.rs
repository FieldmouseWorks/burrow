//! What each port takes: GitHub's deliveries, and kicks from this machine.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::handler::Handler;
use crate::http::{Request, signed};
use crate::line::{DEBOUNCE, Line, Pass};
use crate::told::Told;
use burrow_github::repo_name;

use crate::now;

pub type Handlers = Arc<BTreeMap<String, Handler>>;
pub type Lines = Arc<BTreeMap<String, Line>>;

/// The webhook's deliveries, nginx passes them on: each goes to the handlers
/// that take it (`Handler::takes`). A commit status goes to none whose own
/// passes posted it (`ownStatuses`), which is no news to them.
pub fn delivery(handlers: &Handlers, lines: &Lines, secret: &str, req: &Request) -> u16 {
    let event = req.header("X-GitHub-Event").unwrap_or("");
    if req.body.is_empty() || !signed(secret, &req.body, req.header("X-Hub-Signature-256")) {
        // GitHub's secret and this one differ (a forgery, or a secret
        // changed and not yet restarted for): say so.
        eprintln!("a {event} delivery whose signature isn't the hook secret's; turned away");
        return 401;
    }
    let Ok(payload) = serde_json::from_slice::<Value>(&req.body) else {
        return 204; // nothing about a repo
    };
    let Some(repo) = payload
        .get("repository")
        .and_then(|r| r.get("full_name"))
        .and_then(Value::as_str)
    else {
        return 204;
    };
    if !repo_name(repo) {
        return 204;
    }
    for (name, h) in handlers.iter() {
        if h.takes(event, &payload) {
            let only_first = h.first_only.iter().any(|e| e == event);
            match h.told(event, &payload) {
                Some(told) => {
                    lines[name].told(h.place(repo), told, now(), only_first);
                }
                // One that counts only for a first PR is about none.
                None if only_first => {}
                None => lines[name].event(h.place(repo), now()),
            }
        }
    }
    202
}

/// POST /kick/HANDLER[/OWNER/REPO], from this machine. A repo the handler
/// doesn't take (`repos`) is no kick. A body, told.rs's lines, tells the
/// repo's pass what the kick is about, as an event about them would (a
/// review back for a PR, a pass that couldn't start putting its own back):
/// for a handler told that, and nothing else.
pub fn kick(handlers: &Handlers, lines: &Lines, req: &Request) -> u16 {
    let path = req.target.split('?').next().unwrap_or("");
    let parts: Vec<&str> = path.split('/').collect();
    if !matches!(parts.len(), 3 | 5) || parts[..2] != ["", "kick"] || !lines.contains_key(parts[2])
    {
        return 404;
    }
    let h = &handlers[parts[2]];
    let repo = parts[3..].join("/");
    let Ok(body) = std::str::from_utf8(&req.body) else {
        return 400;
    };
    let told: Option<Vec<Told>> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Told::parse(l).filter(|t| h.accepts(t)))
        .collect();
    let Some(told) = told else {
        return 400;
    };
    if !told.is_empty() {
        if !(repo_name(&repo) && h.covers(&repo)) {
            return if repo.is_empty() { 400 } else { 404 };
        }
        lines[parts[2]].told(h.place(&repo), told, now(), false);
    } else if repo.is_empty() {
        // Due at once: nothing follows a full pass's kick to wait for.
        lines[parts[2]].event(Pass::Full, now() - DEBOUNCE);
    } else if repo_name(&repo) && h.covers(&repo) {
        lines[parts[2]].event(h.place(&repo), now());
    } else {
        return 404;
    }
    202
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use serde_json::json;

    use super::{Handlers, Lines, delivery, kick};
    use crate::handler::{Handler, Pushes};
    use crate::http::listen;
    use crate::http::tests::{post, sign};
    use crate::line::{Line, Pass, Scope};
    use crate::told::{First, Told};

    fn setup() -> (Handlers, Lines, u16, u16) {
        let h = |events: &[&str], unit: &str| Handler {
            events: events.iter().map(|e| e.to_string()).collect(),
            unit: unit.into(),
            per_repo: true,
            ..Handler::default()
        };
        let handlers: Handlers = Arc::new(BTreeMap::from([
            (
                "autoland".into(),
                Handler {
                    own_statuses: vec!["review-settled".into()],
                    ..h(&["push", "pull_request", "status"], "autoland")
                },
            ),
            (
                "inbox".into(),
                Handler {
                    issue_dir: Some("/run/burrow-events/inbox".into()),
                    ..h(&["issues", "pull_request", "status"], "proj-inbox")
                },
            ),
            (
                "update".into(),
                Handler {
                    per_repo: false,
                    repos: Some(vec!["sam/r".into()]),
                    pushes: Pushes::Default,
                    ..h(&["push"], "remi-update")
                },
            ),
        ]));
        let lines: Lines = Arc::new(
            handlers
                .keys()
                .map(|k| (k.clone(), Line::default()))
                .collect(),
        );
        let (hs, ls) = (handlers.clone(), lines.clone());
        let (hooks, _) = listen(
            0,
            Arc::new(move |req| delivery(&hs, &ls, "hook-secret", req)),
        )
        .unwrap();
        let (hs, ls) = (handlers.clone(), lines.clone());
        let (kicks, _) = listen(0, Arc::new(move |req| kick(&hs, &ls, req))).unwrap();
        (handlers, lines, hooks, kicks)
    }

    fn due(lines: &Lines, name: &str) -> BTreeSet<Pass> {
        lines[name].due().into_keys().collect()
    }

    fn repos(names: &[&str]) -> BTreeSet<Pass> {
        names
            .iter()
            .map(|n| {
                if n.is_empty() {
                    Pass::Full
                } else {
                    Pass::repo(n)
                }
            })
            .collect()
    }

    fn deliver(port: u16, body: &[u8], signature: &str, event: &str) -> u16 {
        post(
            port,
            "/burrow/hook",
            body,
            &[
                ("X-Hub-Signature-256", signature),
                ("X-GitHub-Event", event),
            ],
        )
    }

    fn signed_delivery(port: u16, payload: serde_json::Value, event: &str) -> u16 {
        let body = payload.to_string().into_bytes();
        deliver(port, &body, &sign("hook-secret", &body), event)
    }

    // Deliveries go to the handlers that take their event; forged ones, and
    // ones about no repo, to none.
    #[test]
    fn deliveries() {
        let (_h, lines, hooks, _k) = setup();
        let body = br#"{"repository": {"full_name": "sam/r"}}"#;
        let sig = sign("hook-secret", body);
        assert_eq!(
            deliver(hooks, body, &format!("sha256={}", "0".repeat(64)), "push"),
            401
        );
        assert_eq!(deliver(hooks, b"", &sig, "push"), 401);
        assert_eq!(deliver(hooks, body, &sig, "ping"), 202);
        assert_eq!(
            signed_delivery(
                hooks,
                json!({"repository": {"full_name": "--help"}}),
                "push"
            ),
            204
        );
        assert_eq!(
            signed_delivery(hooks, json!({"repository": {"full_name": 5}}), "push"),
            204
        );
        assert_eq!(signed_delivery(hooks, json!(["sam/r"]), "push"), 204);
        let junk = b"not json";
        assert_eq!(
            deliver(hooks, junk, &sign("hook-secret", junk), "push"),
            204
        );
        assert_eq!(
            (due(&lines, "autoland"), due(&lines, "inbox")),
            (repos(&[]), repos(&[]))
        );
        assert_eq!(deliver(hooks, body, &sig, "push"), 202);
        assert_eq!(
            (due(&lines, "autoland"), due(&lines, "inbox")),
            (repos(&["sam/r"]), repos(&[]))
        );
        assert_eq!(deliver(hooks, body, &sig, "pull_request"), 202);
        assert_eq!(
            (due(&lines, "autoland"), due(&lines, "inbox")),
            (repos(&["sam/r"]), repos(&["sam/r"]))
        );
        assert_eq!(due(&lines, "update"), repos(&[]));
    }

    // An issue's or a comment's event books the inbox a look at that issue
    // alone, a burst's together; anything else, a look at everything.
    #[test]
    fn issue_events() {
        let (_h, lines, hooks, _k) = setup();
        let issue = |n: u64| json!({"repository": {"full_name": "Sam/R"}, "action": "edited", "issue": {"number": n}});
        assert_eq!(signed_delivery(hooks, issue(7), "issues"), 202);
        assert_eq!(signed_delivery(hooks, issue(9), "issues"), 202);
        assert_eq!(due(&lines, "inbox"), repos(&[]));
        assert_eq!(
            lines["inbox"].told_now()[&Pass::repo("sam/r")].1,
            BTreeSet::from([Told::Issue(7), Told::Issue(9)])
        );
        let pr = json!({"repository": {"full_name": "sam/r"}});
        assert_eq!(signed_delivery(hooks, pr, "pull_request"), 202);
        assert_eq!(due(&lines, "inbox"), repos(&["sam/r"]));
    }

    // A push to the default branch of a repo the update handler takes is
    // its full pass; one to another branch, or another repo, isn't.
    #[test]
    fn default_pushes() {
        let (_h, lines, hooks, _k) = setup();
        for (repo, branch, want) in [
            ("sam/q", "main", repos(&[])),
            ("sam/r", "fix", repos(&[])),
            ("Sam/R", "main", repos(&[""])),
        ] {
            let p = json!({"repository": {"full_name": repo, "default_branch": "main"}, "ref": format!("refs/heads/{branch}")});
            assert_eq!(signed_delivery(hooks, p, "push"), 202);
            assert_eq!(due(&lines, "update"), want, "{repo} {branch}");
        }
    }

    // The steward's own status is no news to it; to another handler (a job
    // awaiting the PR) it is.
    #[test]
    fn own_status() {
        let (_h, lines, hooks, _k) = setup();
        let status = |context| json!({"repository": {"full_name": "sam/r"}, "context": context});
        assert_eq!(
            signed_delivery(hooks, status("review-settled"), "status"),
            202
        );
        assert_eq!(
            (due(&lines, "autoland"), due(&lines, "inbox")),
            (repos(&[]), repos(&["sam/r"]))
        );
        lines["inbox"].clear();
        assert_eq!(signed_delivery(hooks, status("proof"), "status"), 202);
        assert_eq!(
            (due(&lines, "autoland"), due(&lines, "inbox")),
            (repos(&["sam/r"]), repos(&["sam/r"]))
        );
    }

    // Kicks name a handler, and a repo or none (a full pass).
    #[test]
    fn kicks() {
        let (_h, lines, _d, kicks) = setup();
        assert_eq!(post(kicks, "/kick/inbox/sam/q", b"", &[]), 202);
        assert_eq!(
            (due(&lines, "autoland"), due(&lines, "inbox")),
            (repos(&[]), repos(&["sam/q"]))
        );
        assert_eq!(post(kicks, "/kick/autoland", b"", &[]), 202);
        assert_eq!(
            (due(&lines, "autoland"), due(&lines, "inbox")),
            (repos(&[""]), repos(&["sam/q"]))
        );
        // A full pass's kick is due at once.
        assert!(lines["autoland"].due()[&Pass::Full] <= crate::now());
        for bad in [
            "/kick/nope/sam/r",
            "/kick/inbox/--help/x",
            "/kick/inbox/sam",
            "/kick",
            "/burrow/hook",
            "/kick/inbox//",
        ] {
            assert_eq!(post(kicks, bad, b"", &[]), 404, "{bad}");
        }
        assert_eq!(
            (due(&lines, "autoland"), due(&lines, "inbox")),
            (repos(&[""]), repos(&["sam/q"]))
        );
        // A kick for a repo the handler doesn't take is none; for one it
        // does, a handler with no pass over one repo gets its full pass.
        assert_eq!(post(kicks, "/kick/update/sam/q", b"", &[]), 404);
        assert_eq!(due(&lines, "update"), repos(&[]));
        assert_eq!(post(kicks, "/kick/update/sam/r?why=test", b"", &[]), 202);
        assert_eq!(due(&lines, "update"), repos(&[""]));
        // The kick port takes no deliveries, however they're signed.
        let body = br#"{"repository": {"full_name": "sam/r"}}"#;
        assert_eq!(
            post(
                kicks,
                "/burrow/hook",
                body,
                &[
                    ("X-Hub-Signature-256", &sign("hook-secret", body)),
                    ("X-GitHub-Event", "push")
                ]
            ),
            404
        );
    }

    /// The steward as modules/autoland.nix has it: told PRs, with its first
    /// PRs listed, and check runs only for those.
    fn steward() -> (Handlers, Lines, u16, u16) {
        let handlers: Handlers = Arc::new(BTreeMap::from([(
            "autoland".into(),
            Handler {
                events: [
                    "push",
                    "pull_request",
                    "pull_request_review",
                    "pull_request_review_comment",
                    "pull_request_review_thread",
                    "issue_comment",
                    "check_suite",
                    "check_run",
                    "status",
                    "repository",
                ]
                .map(String::from)
                .to_vec(),
                unit: "autoland".into(),
                per_repo: true,
                pr_dir: Some("/run/burrow-events/autoland".into()),
                firsts: Some("/nowhere".into()),
                first_only: vec!["check_run".into()],
                ..Handler::default()
            },
        )]));
        let lines: Lines = Arc::new(BTreeMap::from([("autoland".into(), Line::default())]));
        lines["autoland"].firsts(BTreeMap::from([(
            "sam/r".into(),
            vec![First {
                number: 4,
                branch: Some("fix".into()),
                head: Some("abcdef1".into()),
            }],
        )]));
        let (hs, ls) = (handlers.clone(), lines.clone());
        let (hooks, _) = listen(
            0,
            Arc::new(move |req| delivery(&hs, &ls, "hook-secret", req)),
        )
        .unwrap();
        let (hs, ls) = (handlers.clone(), lines.clone());
        let (kicks, _) = listen(0, Arc::new(move |req| kick(&hs, &ls, req))).unwrap();
        (handlers, lines, hooks, kicks)
    }

    // Each event the steward takes about a first PR (a push to its branch,
    // its own events, a review, a comment, its checks and statuses, a
    // label) starts that PR's pass ahead of every other, told what it's
    // about; one about an ordinary PR waits its turn, told the same; one
    // about no PR is a look at everything. Check runs count only for a
    // first PR. Two of the same are one.
    #[test]
    fn steward_events() {
        let (_h, lines, hooks, _k) = steward();
        let pr = json!({"repository": {"full_name": "sam/r", "default_branch": "main"}});
        let with = |extra: serde_json::Value| {
            let mut p = pr.clone();
            p.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            p
        };
        let first = [
            (
                "push",
                with(json!({"ref": "refs/heads/fix"})),
                Told::Branch("fix".into()),
            ),
            (
                "pull_request",
                with(
                    json!({"action": "labeled", "label": {"name": "hold"}, "pull_request": {"number": 4}}),
                ),
                Told::Pr(4),
            ),
            (
                "pull_request_review",
                with(json!({"pull_request": {"number": 4}})),
                Told::Pr(4),
            ),
            (
                "pull_request_review_comment",
                with(json!({"pull_request": {"number": 4}})),
                Told::Pr(4),
            ),
            (
                "pull_request_review_thread",
                with(json!({"pull_request": {"number": 4}})),
                Told::Pr(4),
            ),
            (
                "issue_comment",
                with(json!({"issue": {"number": 4, "pull_request": {}}})),
                Told::Pr(4),
            ),
            (
                "check_suite",
                with(
                    json!({"check_suite": {"pull_requests": [], "head_branch": "fix", "head_sha": "abcdef1"}}),
                ),
                Told::Branch("fix".into()),
            ),
            (
                "check_run",
                with(
                    json!({"action": "completed", "check_run": {"pull_requests": [], "head_sha": "abcdef1", "check_suite": {"head_branch": null}}}),
                ),
                Told::Commit("abcdef1".into()),
            ),
            (
                "status",
                with(json!({"sha": "abcdef1", "context": "ci"})),
                Told::Commit("abcdef1".into()),
            ),
        ];
        for (i, (event, payload, want)) in first.into_iter().enumerate() {
            lines["autoland"].clear();
            assert_eq!(signed_delivery(hooks, payload.clone(), event), 202);
            assert_eq!(signed_delivery(hooks, payload, event), 202);
            let other =
                json!({"repository": {"full_name": "sam/q"}, "pull_request": {"number": 9}});
            assert_eq!(signed_delivery(hooks, other, "pull_request_review"), 202);
            let at = crate::now() + 10.0 + 30.0 * i as f64;
            assert_eq!(
                lines["autoland"].take(at).0,
                vec![(Pass::repo("sam/r"), Scope::Told([want].into()))],
                "{event}"
            );
            assert_eq!(
                lines["autoland"].take(at).0,
                vec![(Pass::repo("sam/q"), Scope::Told([Told::Pr(9)].into()))],
                "{event}"
            );
        }
        // An ordinary PR's: told, but no first; its check runs, nothing.
        lines["autoland"].clear();
        let run = with(
            json!({"action": "completed", "check_run": {"pull_requests": [], "head_sha": "1234567", "check_suite": {"head_branch": "other"}}}),
        );
        assert_eq!(signed_delivery(hooks, run, "check_run"), 202);
        let started = with(
            json!({"action": "created", "check_run": {"pull_requests": [], "head_sha": "abcdef1", "check_suite": {"head_branch": "fix"}}}),
        );
        assert_eq!(signed_delivery(hooks, started, "check_run"), 202);
        assert!(lines["autoland"].told_now().is_empty() && lines["autoland"].due().is_empty());
        let other = with(json!({"pull_request": {"number": 9}}));
        assert_eq!(signed_delivery(hooks, other, "pull_request_review"), 202);
        assert_eq!(
            lines["autoland"].told_now()[&Pass::repo("sam/r")].1,
            BTreeSet::from([Told::Pr(9)])
        );
        // The repo's own event, or a push to its default branch: everything.
        lines["autoland"].clear();
        assert_eq!(
            signed_delivery(hooks, with(json!({"ref": "refs/heads/main"})), "push"),
            202
        );
        assert_eq!(due(&lines, "autoland"), repos(&["sam/r"]));
    }

    // A kick may tell a repo's pass what it's about, in told.rs's lines,
    // for a handler told that: a review back for a first PR goes first;
    // a line it can't be told, or a full pass's kick with lines, is turned
    // away and books nothing.
    #[test]
    fn told_kicks() {
        let (_h, lines, _d, kicks) = steward();
        assert_eq!(post(kicks, "/kick/autoland/sam/r", b"pr 4\n", &[]), 202);
        assert_eq!(
            lines["autoland"].told_now()[&Pass::repo("sam/r")].1,
            BTreeSet::from([Told::Pr(4)])
        );
        assert!(due(&lines, "autoland").is_empty());
        for (path, body) in [
            ("/kick/autoland/sam/r", &b"7\n"[..]),
            ("/kick/autoland/sam/r", b"pr x\n"),
            ("/kick/autoland", b"pr 4\n"),
            ("/kick/autoland/sam/r", b"\xff"),
        ] {
            lines["autoland"].clear();
            assert_eq!(post(kicks, path, body, &[]), 400, "{body:?}");
            assert!(lines["autoland"].told_now().is_empty() && due(&lines, "autoland").is_empty());
        }
        // An empty body is the kick it always was.
        assert_eq!(post(kicks, "/kick/autoland/sam/r", b"\n", &[]), 202);
        assert_eq!(due(&lines, "autoland"), repos(&["sam/r"]));
        // The inbox is told issues by kick too, and nothing else.
        let (_h, lines, _d, kicks) = setup();
        assert_eq!(post(kicks, "/kick/inbox/sam/r", b"7\n", &[]), 202);
        assert_eq!(
            lines["inbox"].told_now()[&Pass::repo("sam/r")].1,
            BTreeSet::from([Told::Issue(7)])
        );
        assert_eq!(post(kicks, "/kick/inbox/sam/r", b"pr 7\n", &[]), 400);
        assert_eq!(post(kicks, "/kick/autoland/sam/r", b"pr 7\n", &[]), 400);
    }
}
