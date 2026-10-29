//! What each port takes: GitHub's deliveries, and kicks from this machine.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::handler::Handler;
use crate::http::{Request, signed};
use crate::line::{DEBOUNCE, Line, Pass};
use crate::{now, repo_name};

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
            lines[name].event(h.place(repo), now());
        }
    }
    202
}

/// POST /kick/HANDLER[/OWNER/REPO], from this machine. A repo the handler
/// doesn't take (`repos`) is no kick.
pub fn kick(handlers: &Handlers, lines: &Lines, req: &Request) -> u16 {
    let path = req.target.split('?').next().unwrap_or("");
    let parts: Vec<&str> = path.split('/').collect();
    if !matches!(parts.len(), 3 | 5) || parts[..2] != ["", "kick"] || !lines.contains_key(parts[2])
    {
        return 404;
    }
    let h = &handlers[parts[2]];
    let repo = parts[3..].join("/");
    if repo.is_empty() {
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
    use crate::line::{Line, Pass};

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
                h(&["issues", "pull_request", "status"], "proj-inbox"),
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
}
