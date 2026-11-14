//! The route to the owner's project on GitHub, and the owner label.
//!
//! The coordinator (the Claude in the owner's project chat) reads the
//! machine's repo for its requests: each escalation is one issue there under
//! the relay label, titled `[escalation KEY]`, opened by the relay's own
//! sender and marked with the request's digest. An issue alone isn't
//! delivery: it counts once the coordinator, having raised it with the owner
//! in their project, comments `delivered KEY` on it as one of the
//! acknowledgers, matched by account id (a login is only a label).

use burrow_github::{Api, Author, call, call_repeating, encode, pages};
use jiff::Timestamp;
use serde_json::{Value, json};

use crate::config::Relay;
use crate::{Error, Labels, Project, Receipt, RelayRef, Request, Target};

/// The word an acknowledgement starts with.
pub const ACK: &str = "delivered";

/// What a request's issue is titled with, so it's found again.
pub fn tag(key: &str) -> String {
    format!("[escalation {key}]")
}

/// The coordinator's route, as `relay` configures it.
pub struct GitHub<'a> {
    pub api: &'a mut dyn Api,
    pub relay: &'a Relay,
    /// Who the owner is, as the request names them.
    pub owner: &'a str,
    /// The label the request asks to put on, as the request names it.
    pub owner_label: &'a str,
}

impl GitHub<'_> {
    /// The request already made for exactly this one: an issue (never a PR)
    /// opened by the relay's own sender, titled with its tag, marked with
    /// its digest, and created no earlier than its escalation was kept. Read
    /// from the issues themselves, not the search index, which lags a new
    /// one; and not by label, which GitHub can drop.
    fn find(&mut self, request: &Request) -> Result<Option<Value>, Error> {
        let repo = &self.relay.repo;
        let tag = tag(request.key);
        let marker = marker(request);
        // A minute's slack for clocks. Anything created since was updated
        // since, so `since` can't hide it.
        let floor = request.since.saturating_sub(60);
        let from =
            Timestamp::from_second(floor.try_into().unwrap_or(0)).map_err(|e| e.to_string())?;
        let path = format!(
            "repos/{repo}/issues?state=all&per_page=100&since={}",
            encode(&from.to_string())
        );
        let mut found: Vec<Value> = Vec::new();
        for page in pages(self.api, &path).map_err(String::from)? {
            for issue in page.as_array().into_iter().flatten() {
                let created = issue["created_at"]
                    .as_str()
                    .and_then(|t| t.parse::<Timestamp>().ok())
                    .map(|t| t.as_second());
                let ours = issue["pull_request"].is_null()
                    && self.relay.sender.matches(issue["user"]["id"].as_u64())
                    && issue["title"].as_str().is_some_and(|t| t.starts_with(&tag))
                    && issue["body"].as_str().is_some_and(|b| b.contains(&marker))
                    && created.is_some_and(|c| c >= i64::try_from(floor).unwrap_or(i64::MAX));
                if ours && issue["number"].as_u64().is_some() {
                    found.push(issue.clone());
                }
            }
        }
        // The first made is the one: a twin can't be made from here, but if
        // one were, every pass would still settle on the same issue.
        Ok(found.into_iter().min_by_key(|i| i["number"].as_u64()))
    }

    /// The relay label, made if the repo hasn't got it, so the request
    /// doesn't open without it. A make that fails may mean it's there.
    fn ensure_label(&mut self) -> Result<(), Error> {
        let repo = &self.relay.repo;
        let label = &self.relay.label;
        let made = call(
            self.api,
            "POST",
            &format!("repos/{repo}/labels"),
            Some(&json!({"name": label, "color": "B60205",
                         "description": "Requests for the owner's project, delivered once acknowledged"})),
            None,
        );
        let Err(why) = made else { return Ok(()) };
        call(
            self.api,
            "GET",
            &format!("repos/{repo}/labels/{}", encode(label)),
            None,
            None,
        )
        .map(drop)
        .map_err(|_| String::from(why))
    }

    /// The relay label on request `n` of `repo`, put back if GitHub dropped
    /// it: the coordinator watches that label, and the inbox leaves what
    /// carries it alone. Until it's on, this fails, and the next pass tries
    /// again on the same issue.
    fn keep_label(&mut self, repo: &str, n: u64, issue: &Value) -> Result<(), Error> {
        let label = &self.relay.label;
        if carries(issue, label) {
            return Ok(());
        }
        let now = call_repeating(
            self.api,
            "POST",
            &format!("repos/{repo}/issues/{n}/labels"),
            Some(&json!({"labels": [label]})),
        )
        .and_then(|r| r.json())
        .map_err(String::from)?;
        let on = now.as_array().into_iter().flatten().any(|l| {
            l["name"]
                .as_str()
                .is_some_and(|x| x.eq_ignore_ascii_case(label))
        });
        if on {
            Ok(())
        } else {
            Err(format!(
                "{repo}#{n}: GitHub didn't keep the relay label {label}"
            ))
        }
    }

    fn body(&self, request: &Request) -> String {
        let Request {
            key,
            target,
            reason,
            ask,
            ..
        } = request;
        let owner = self.owner;
        let label = self.owner_label;
        format!(
            "**For {owner}:** {says}: {detail}\n\n\
             On https://github.com/{repo}/issues/{n}\n\n\
             {ask}\n\n\
             ---\n\
             Coordinator: raise this with {owner} in their project, then comment here, \
             as an acknowledger, with this as the first line:\n\n\
             ```\n{ACK} {key}\n```\n\n\
             `{label}` goes on {target} only after that. Until then it waits here.\n\n\
             {marker}",
            says = reason.kind.says(),
            detail = reason.detail,
            repo = target.repo,
            n = target.number,
            marker = marker(request),
        )
    }
}

/// What marks a request as exactly this one.
fn marker(request: &Request) -> String {
    format!(
        "<!-- burrow-escalation {} {} {} -->",
        request.key,
        request.reason.kind.word(),
        request.digest()
    )
}

/// GitHub's label names ignore case.
fn carries(issue: &Value, label: &str) -> bool {
    issue["labels"].as_array().into_iter().flatten().any(|l| {
        l["name"]
            .as_str()
            .is_some_and(|n| n.eq_ignore_ascii_case(label))
    })
}

impl Project for GitHub<'_> {
    fn send(&mut self, request: &Request) -> Result<RelayRef, Error> {
        let repo = self.relay.repo.clone();
        if let Some(issue) = self.find(request)? {
            let n = issue["number"].as_u64().unwrap_or_default();
            self.keep_label(&repo, n, &issue)?;
            return Ok(RelayRef { repo, number: n });
        }
        self.ensure_label()?;
        let title = format!(
            "{} {}: {}",
            tag(request.key),
            request.reason.kind.says(),
            request.target
        );
        let body = self.body(request);
        // A POST isn't asked twice: its answer can be lost after GitHub made
        // the issue, and then the next send finds it.
        let made = call(
            self.api,
            "POST",
            &format!("repos/{repo}/issues"),
            Some(&json!({"title": title, "body": body, "labels": [self.relay.label]})),
            None,
        )
        .and_then(|r| r.json())
        .map_err(String::from)?;
        let n = made["number"]
            .as_u64()
            .ok_or_else(|| format!("POST repos/{repo}/issues: its answer has no number"))?;
        // GitHub can open it and drop its labels; the relay is kept either
        // way, and the label is put back here or on a later pass.
        let kept = RelayRef {
            repo: repo.clone(),
            number: n,
        };
        self.keep_label(&repo, n, &made).map(|()| kept)
    }

    fn acknowledged(&mut self, key: &str, relay: &RelayRef) -> Result<Option<Receipt>, Error> {
        let RelayRef { repo, number: n } = relay;
        let issue = call(
            self.api,
            "GET",
            &format!("repos/{repo}/issues/{n}"),
            None,
            None,
        )
        .and_then(|r| r.json())
        .map_err(String::from)?;
        let path = format!("repos/{repo}/issues/{n}/comments?per_page=100");
        for page in pages(self.api, &path).map_err(String::from)? {
            for comment in page.as_array().into_iter().flatten() {
                if let Some(receipt) = receipt(comment, key, &self.relay.acknowledgers) {
                    return Ok(Some(receipt));
                }
            }
        }
        // Not heard yet: its label is what gets it heard.
        self.keep_label(repo, *n, &issue)?;
        Ok(None)
    }
}

/// The receipt a comment is, if it's an acknowledgement of `key` by one of
/// `acknowledgers`.
fn receipt(comment: &Value, key: &str, acknowledgers: &[Author]) -> Option<Receipt> {
    let id = comment["user"]["id"].as_u64();
    let by = acknowledgers.iter().find(|a| a.matches(id))?;
    let first = comment["body"]
        .as_str()?
        .lines()
        .map(|l| l.trim().trim_matches('`').trim())
        .find(|l| !l.is_empty())?;
    let mut words = first.split_whitespace();
    let says = words.next() == Some(ACK) && words.next() == Some(key) && words.next().is_none();
    says.then(|| {
        Receipt::new(
            comment["user"]["login"]
                .as_str()
                .unwrap_or(&by.login)
                .to_owned(),
            by.id,
            comment["created_at"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            comment["html_url"].as_str().unwrap_or_default().to_owned(),
        )
    })
}

/// The owner label's writer on GitHub.
pub struct GitHubLabels<'a> {
    pub api: &'a mut dyn Api,
}

impl Labels for GitHubLabels<'_> {
    fn add(&mut self, target: &Target, label: &str, _delivered: &Receipt) -> Result<(), Error> {
        let path = format!("repos/{}/issues/{}/labels", target.repo, target.number);
        // Adding a label it has changes nothing, so it's asked again freely.
        let now = call_repeating(self.api, "POST", &path, Some(&json!({"labels": [label]})))
            .and_then(|r| r.json())
            .map_err(String::from)?;
        let on = now.as_array().into_iter().flatten().any(|l| {
            l["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(label))
        });
        if on {
            Ok(())
        } else {
            Err(format!("{target}: GitHub didn't keep {label}"))
        }
    }
}
