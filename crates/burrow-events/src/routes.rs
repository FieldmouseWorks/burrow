//! What each port takes: GitHub's deliveries, and kicks from this machine.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::handler::Handler;
use crate::http::{Request, signed};
use crate::line::{DEBOUNCE, Line, Pass};
use crate::outbox::Outbox;
use burrow_github::repo_name;

use crate::now;

pub type Handlers = Arc<BTreeMap<String, Handler>>;
pub type Lines = Arc<BTreeMap<String, Line>>;

/// The webhook's deliveries, nginx passes them on: each goes to the handlers
/// that take it (`Handler::takes`). A commit status goes to none whose own
/// passes posted it (`ownStatuses`), which is no news to them. One about a
/// repo conaryd lands (the landing record, read now) goes on to conaryd
/// too: held in its outbox before GitHub hears 202, and sent from there
/// until conaryd takes it. One that couldn't be held is answered 502, so
/// GitHub's log shows it and it can be sent again.
pub fn delivery(
    handlers: &Handlers,
    lines: &Lines,
    outbox: Option<&Outbox>,
    secret: &str,
    req: &Request,
) -> u16 {
    let event = req.header("X-GitHub-Event").unwrap_or("");
    if req.body.is_empty() || !signed(secret, &req.body, req.header("X-Hub-Signature-256")) {
        // A forgery, a probe, or a hook whose secret isn't this one: say
        // which, and whose, so it can be found at its owner.
        eprintln!("{}", turned_away(|n| req.header(n), req.body.is_empty()));
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
            match h.issue(event, &payload) {
                Some(n) => lines[name].issue(h.place(repo), n, now()),
                None => lines[name].event(h.place(repo), now()),
            }
        }
    }
    if let Some(outbox) = outbox
        && let Err(e) = outbox.hold(repo, req)
    {
        let id = req.header("X-GitHub-Delivery").unwrap_or("?");
        eprintln!("{repo}: delivery {id} ({event}) not held for conaryd: {e}");
        return 502;
    }
    202
}

/// The line for a delivery turned away: its event, the hook that sent it
/// (GitHub names it in X-GitHub-Hook-ID and the installation target headers;
/// a request without them is no hook's, with its User-Agent to say whose),
/// and why: an empty body, no signature, or another secret's. Every value is
/// the sender's and unchecked, so each is held to what GitHub sends.
fn turned_away<'a>(header: impl Fn(&str) -> Option<&'a str>, empty: bool) -> String {
    let event = match header("X-GitHub-Event") {
        Some(e) => format!("a {} delivery", word(e, 40)),
        None => "a delivery with no X-GitHub-Event".into(),
    };
    let from = match header("X-GitHub-Hook-ID") {
        Some(id) => {
            let mut from = format!("from hook {}", digits(id));
            if let Some(kind) = header("X-GitHub-Hook-Installation-Target-Type") {
                let target =
                    header("X-GitHub-Hook-Installation-Target-ID").map_or("?".into(), digits);
                from += &format!(" ({} {target})", word(kind, 40));
            }
            if let Some(guid) = header("X-GitHub-Delivery") {
                from += &format!(", delivery {}", word(guid, 40));
            }
            from
        }
        None => format!(
            "from no GitHub hook (no X-GitHub-Hook-ID; User-Agent {})",
            header("User-Agent").map_or("none".into(), printable)
        ),
    };
    let why = if empty {
        "an empty body"
    } else if header("X-Hub-Signature-256").is_none() {
        "no signature"
    } else {
        "a signature that isn't the hook secret's"
    };
    format!("{event} {from}, with {why}; turned away")
}

/// An ID GitHub sends: digits, or "(garbled)".
fn digits(v: &str) -> String {
    if !v.is_empty() && v.len() <= 20 && v.bytes().all(|b| b.is_ascii_digit()) {
        v.into()
    } else {
        "(garbled)".into()
    }
}

/// A name or GUID GitHub sends (an event, a target type, a delivery): up to
/// MAX letters, digits, `_`, `-` and `:`, or "(garbled)".
fn word(v: &str, max: usize) -> String {
    let ok = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':');
    if !v.is_empty() && v.len() <= max && v.bytes().all(ok) {
        v.into()
    } else {
        "(garbled)".into()
    }
}

/// A free-form value (a User-Agent), quoted: its printable ASCII, cut at 80.
fn printable(v: &str) -> String {
    let kept: String = v.chars().filter(|c| matches!(c, ' '..='~')).collect();
    match kept.char_indices().nth(80) {
        Some((cut, _)) => format!("{:?}...", &kept[..cut]),
        None => format!("{kept:?}"),
    }
}

/// POST /kick/HANDLER[/OWNER/REPO], from this machine. A repo the handler
/// doesn't take (`repos`, or the steward's while conaryd lands it) is no
/// kick.
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
    } else if repo_name(&repo) && h.covers(&repo) && h.holds(&repo) {
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

    use super::{Handlers, Lines, delivery, kick, turned_away};
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
            Arc::new(move |req| delivery(&hs, &ls, None, crate::test_secret(), req)),
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
        deliver(port, &body, &sign(crate::test_secret(), &body), event)
    }

    // Deliveries go to the handlers that take their event; forged ones, and
    // ones about no repo, to none.
    #[test]
    fn deliveries() {
        let (_h, lines, hooks, _k) = setup();
        let body = br#"{"repository": {"full_name": "sam/r"}}"#;
        let sig = sign(crate::test_secret(), body);
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
            deliver(hooks, junk, &sign(crate::test_secret(), junk), "push"),
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
            lines["inbox"].issues()[&Pass::repo("sam/r")].1,
            BTreeSet::from([7, 9])
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
                    ("X-Hub-Signature-256", &sign(crate::test_secret(), body)),
                    ("X-GitHub-Event", "push")
                ]
            ),
            404
        );
    }

    fn line(headers: &[(&str, &str)], empty: bool) -> String {
        turned_away(
            |n| {
                headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(n))
                    .map(|(_, v)| *v)
            },
            empty,
        )
    }

    // A hook's delivery names the hook, its target and the delivery, so a
    // stale secret can be found at its owner.
    #[test]
    fn turned_away_names_the_hook() {
        assert_eq!(
            line(
                &[
                    ("X-GitHub-Event", "ping"),
                    ("X-GitHub-Hook-ID", "512345678"),
                    ("X-GitHub-Hook-Installation-Target-Type", "integration"),
                    ("X-GitHub-Hook-Installation-Target-ID", "1046020"),
                    ("X-GitHub-Delivery", "72d3162e-cc78-11e3-81ab-4c9367dc0958"),
                    ("X-Hub-Signature-256", "sha256=00"),
                    ("User-Agent", "GitHub-Hookshot/044aadd"),
                ],
                false
            ),
            "a ping delivery from hook 512345678 (integration 1046020), \
             delivery 72d3162e-cc78-11e3-81ab-4c9367dc0958, \
             with a signature that isn't the hook secret's; turned away"
        );
    }

    // A request no hook sent (a probe by hand) says so, and whose it is.
    #[test]
    fn turned_away_names_no_hook() {
        assert_eq!(
            line(
                &[("X-GitHub-Event", "ping"), ("User-Agent", "curl/8.23.0")],
                false
            ),
            "a ping delivery from no GitHub hook (no X-GitHub-Hook-ID; \
             User-Agent \"curl/8.23.0\"), with no signature; turned away"
        );
        assert_eq!(
            line(&[("X-Hub-Signature-256", "sha256=00")], true),
            "a delivery with no X-GitHub-Event from no GitHub hook \
             (no X-GitHub-Hook-ID; User-Agent none), with an empty body; turned away"
        );
    }

    // The sender's values are held to what GitHub sends: nothing it makes
    // up reaches the journal as a line of its own or as a lie.
    #[test]
    fn turned_away_holds_the_sender_to_form() {
        let said = line(
            &[
                ("X-GitHub-Event", "push\nforged line"),
                ("X-GitHub-Hook-ID", "12; rm"),
                ("X-GitHub-Hook-Installation-Target-Type", "repository"),
                ("X-GitHub-Delivery", &"a".repeat(41)),
            ],
            false,
        );
        assert_eq!(
            said,
            "a (garbled) delivery from hook (garbled) (repository ?), \
             delivery (garbled), with no signature; turned away"
        );
        let ua = format!("x\u{1b}[31m\n{}", "y".repeat(100));
        let said = line(&[("User-Agent", &ua)], false);
        assert!(!said.contains(['\n', '\u{1b}']), "{said}");
        assert!(
            said.contains(&format!("\"x[31m{}\"...", "y".repeat(75))),
            "{said}"
        );
    }
}
