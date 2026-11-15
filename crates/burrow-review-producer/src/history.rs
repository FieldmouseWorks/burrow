//! A role's earlier findings on a PR, each with the replies it got, as the
//! review brief quotes them (burrow-brief's `prior`), read from GitHub's
//! REST API: the findings the flow published, by the markers it gives each
//! one's target (Conary's `conary-finding:v1`, on a review thread's first
//! comment or a PR comment) and each answer it posts (`conary-answer:v1`).
//! Only the flow's review accounts' markers count, so a marker anyone else
//! quotes names nothing, and only they and the trusted authors are quoted
//! as replies: anyone else's text never steers a look.
use burrow_github::{Api, pages};
use conary_github::flow::FindingMark;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;

const FINDING: &str = "conary-finding:v1";
const ANSWER: &str = "conary-answer:v1";

/// An answer's marker, as Conary's flow writes it (`answer_body`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerMark {
    role: String,
    head: String,
    issuance: String,
    finding: usize,
    #[allow(dead_code)]
    fix: String,
}

impl AnswerMark {
    fn answers(&self, mark: &FindingMark) -> bool {
        self.role == mark.role
            && self.head == mark.head
            && self.issuance == mark.issuance
            && self.finding == mark.finding
    }
}

/// A body's text without its marker, and the marker named `name` on its
/// last line, if it has one.
fn marked<T: for<'a> Deserialize<'a>>(name: &str, body: &str) -> (String, Option<T>) {
    let body = body.trim_end();
    let (text, last) = body.rsplit_once('\n').unwrap_or(("", body));
    match last
        .strip_prefix(&format!("<!-- {name} "))
        .and_then(|m| m.strip_suffix(" -->"))
        .and_then(|m| serde_json::from_str(m).ok())
    {
        Some(mark) => (text.trim().to_owned(), Some(mark)),
        None => (body.trim().to_owned(), None),
    }
}

/// Who may steer a look: the flow's review accounts, and the trusted
/// authors.
pub struct Voices<'a> {
    pub review: &'a BTreeSet<String>,
    pub trusted: &'a [String],
}

impl Voices<'_> {
    fn flow(&self, c: &Value) -> bool {
        self.review.contains(login(c))
    }
    fn heard(&self, c: &Value) -> bool {
        self.flow(c) || self.trusted.iter().any(|t| t == login(c))
    }
}

fn login(c: &Value) -> &str {
    c["user"]["login"].as_str().unwrap_or_default()
}

fn body(c: &Value) -> &str {
    c["body"].as_str().unwrap_or_default()
}

/// Every item of every page.
fn all(api: &mut dyn Api, path: &str) -> Result<Vec<Value>, String> {
    Ok(pages(api, path)
        .map_err(String::from)?
        .into_iter()
        .flat_map(|page| page.as_array().cloned().unwrap_or_default())
        .collect())
}

/// When `c` was made, in seconds since the epoch.
fn made(c: &Value) -> Result<u64, String> {
    let at = c["created_at"].as_str().unwrap_or_default();
    at.parse::<jiff::Timestamp>()
        .ok()
        .and_then(|t| u64::try_from(t.as_second()).ok())
        .ok_or_else(|| format!("a comment made at {at:?}"))
}

/// `role`'s earlier findings on PR `pr`, with their replies, as they were
/// said by `until`: every finding the flow published for it but those of
/// the publication `except` names (the one being checked, which came after
/// the look that quoted the rest). A comment made later is left out, so
/// what's said after a look (an answer, a reply) doesn't change what it
/// quoted, and one of those edited since does. Each finding's line is the
/// one it was made on, which a push doesn't move. Read whole, or not at
/// all.
pub fn read(
    api: &mut dyn Api,
    repo: &str,
    pr: u64,
    role: &str,
    voices: &Voices,
    until: u64,
    except: Option<&str>,
) -> Result<String, String> {
    let lines = all(
        api,
        &format!("repos/{repo}/pulls/{pr}/comments?per_page=100"),
    )
    .map_err(|e| format!("its earlier findings: {e}"))?;
    let talk = all(
        api,
        &format!("repos/{repo}/issues/{pr}/comments?per_page=100"),
    )
    .map_err(|e| format!("its earlier findings: {e}"))?;
    let said_by = |list: Vec<Value>| -> Result<Vec<Value>, String> {
        let mut kept = vec![];
        for c in list {
            if made(&c)? <= until {
                kept.push(c);
            }
        }
        Ok(kept)
    };
    let (lines, talk) = (said_by(lines)?, said_by(talk)?);
    let ours = |c: &Value| -> Option<(String, FindingMark)> {
        if !voices.flow(c) {
            return None;
        }
        let (text, mark) = marked::<FindingMark>(FINDING, body(c));
        mark.filter(|m| m.role == role && Some(m.issuance.as_str()) != except)
            .map(|m| (text, m))
    };
    let reply = |c: &Value, text: &str| format!("  Reply from {}: {text}\n", login(c));
    let mut out = vec![];
    for root in lines.iter().filter(|c| c["in_reply_to_id"].is_null()) {
        let Some((text, _)) = ours(root) else {
            continue;
        };
        let at = root["original_line"]
            .as_u64()
            .map(|n| format!(":{n}"))
            .unwrap_or_default();
        let mut said = format!(
            "- {}{at}: {text}\n",
            root["path"].as_str().unwrap_or_default()
        );
        for c in lines
            .iter()
            .filter(|c| c["in_reply_to_id"] == root["id"] && voices.heard(c))
        {
            said += &reply(c, &marked::<AnswerMark>(ANSWER, body(c)).0);
        }
        out.push(said);
    }
    for comment in &talk {
        let Some((text, mark)) = ours(comment) else {
            continue;
        };
        let mut said = format!("- {text}\n");
        for c in talk.iter().filter(|c| voices.flow(c)) {
            if let (text, Some(a)) = marked::<AnswerMark>(ANSWER, body(c))
                && a.answers(&mark)
            {
                said += &reply(c, &text);
            }
        }
        out.push(said);
    }
    Ok(out.join("\n"))
}
