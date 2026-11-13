//! `issue REPO N`: who wrote an issue's text, as far as GitHub still keeps
//! it: its author, each retained edit's editor (and whoever deleted one),
//! whoever renamed it, how many edits GitHub kept, and its labels, each
//! writer marked with the deployed trust lists that name them. Never its
//! text: the query asks for no title, body or edit diff, so none reaches
//! the probe, and what's said comes from the fields named here only.
//!
//! GitHub keeps at most RETAINED body edits; once an issue has that many,
//! older ones are gone with their editors, and this says the writers are
//! the retained ones only. One whose editor GitHub no longer has (a deleted
//! account) is said to be unknown, not left out.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::Machine;

/// GitHub's retention of an issue's body edits.
const RETAINED: usize = 100;
/// The most labels named.
const MAX_LABELS: usize = 50;
/// The most renames read.
const MAX_RENAMES: usize = 100;
/// The longest error said.
const MAX_ERROR: usize = 500;

/// What's asked: metadata only. No title, body or diff.
pub(super) const QUERY: &str = "query($owner:String!,$name:String!,$n:Int!){repository(owner:$owner,name:$name){issue(number:$n){state createdAt author{...who} labels(first:50){totalCount nodes{name}} userContentEdits(first:100){totalCount nodes{editedAt deletedAt editor{...who} deletedBy{...who}}} timelineItems(itemTypes:[RENAMED_TITLE_EVENT],first:100){totalCount nodes{... on RenamedTitleEvent{createdAt actor{...who}}}}}}} fragment who on Actor{login __typename ... on User{databaseId} ... on Bot{databaseId} ... on Organization{databaseId} ... on Mannequin{databaseId}}";

/// One writer and what they did.
#[derive(Default)]
struct Writer {
    kind: String,
    id: Option<u64>,
    author: bool,
    edits: usize,
    deletions: usize,
    renames: usize,
}

pub fn answer(m: &Machine, repo: &str, number: u64) -> Result<String, String> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| format!("{repo:?} isn't OWNER/REPO"))?;
    let data = m
        .run
        .github(
            m.cfg.tools.gh.as_deref(),
            owner,
            QUERY,
            json!({"owner": owner, "name": name, "n": number}),
        )
        .map_err(|e| {
            let e: String = e.chars().take(MAX_ERROR).collect();
            format!("GitHub didn't say: {e}")
        })?;
    let issue = &data["repository"]["issue"];
    if data["repository"].is_null() {
        return Err(format!(
            "{repo} isn't there, or the probe's GitHub login can't see it"
        ));
    }
    if issue.is_null() {
        return Err(format!(
            "{repo}#{number} isn't an issue the probe's GitHub login can see (it may be a pull request, or not be there)"
        ));
    }
    let mut writers: BTreeMap<String, Writer> = BTreeMap::new();
    let mut unknown = Writer::default();
    let mut note = |actor: &Value, what: &mut dyn FnMut(&mut Writer)| match actor["login"].as_str()
    {
        Some(login) => {
            let w = writers.entry(login.to_owned()).or_default();
            w.kind = actor["__typename"].as_str().unwrap_or("").to_owned();
            w.id = w.id.or(actor["databaseId"].as_u64());
            what(w);
        }
        None => what(&mut unknown),
    };
    note(&issue["author"], &mut |w| w.author = true);

    let edits = &issue["userContentEdits"];
    let nodes = edits["nodes"].as_array().cloned().unwrap_or_default();
    let total = edits["totalCount"].as_u64();
    for e in &nodes {
        note(&e["editor"], &mut |w| w.edits += 1);
        if !e["deletedAt"].is_null() || !e["deletedBy"].is_null() {
            note(&e["deletedBy"], &mut |w| w.deletions += 1);
        }
    }
    let renames = &issue["timelineItems"];
    let rename_nodes = renames["nodes"].as_array().cloned().unwrap_or_default();
    let rename_total = renames["totalCount"].as_u64();
    for r in &rename_nodes {
        note(&r["actor"], &mut |w| w.renames += 1);
    }

    let mut out = format!(
        "{repo}#{number}: {}, opened {}\n",
        issue["state"]
            .as_str()
            .unwrap_or("in an unknown state")
            .to_lowercase(),
        issue["createdAt"].as_str().unwrap_or("at an unknown time"),
    );

    // How much of its history GitHub kept, and returned.
    out += &format!(
        "Body edits: {}\n",
        history(nodes.len(), total, RETAINED, "edits")
    );
    out += &format!(
        "Title renames: {}\n",
        history(rename_nodes.len(), rename_total, MAX_RENAMES, "renames")
    );

    out += "Writers retained (the author, each retained edit's editor, whoever deleted an edit, whoever renamed it):\n";
    for (login, w) in &writers {
        out += &format!("- {}\n", writer(m, login, w));
    }
    if unknown.author || unknown.edits + unknown.deletions + unknown.renames > 0 {
        out += &format!(
            "- unknown: an account GitHub no longer has (deleted, or hidden from the probe's login){}\n",
            did(&unknown)
        );
    }
    let labels = &issue["labels"];
    let names: Vec<&str> = labels["nodes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| l["name"].as_str())
                .take(MAX_LABELS)
                .collect()
        })
        .unwrap_or_default();
    let label_total = labels["totalCount"].as_u64().unwrap_or(names.len() as u64);
    out += &format!(
        "Labels ({label_total}): {}{}\n",
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        },
        if label_total > names.len() as u64 {
            format!(" and {} more", label_total - names.len() as u64)
        } else {
            String::new()
        }
    );
    Ok(out)
}

/// How much of a history GitHub returned: complete, capped at its
/// retention, or incomplete.
fn history(got: usize, total: Option<u64>, cap: usize, what: &str) -> String {
    match total {
        None => format!(
            "{got} {what} returned; GitHub didn't say how many it keeps, so the history may be incomplete"
        ),
        Some(t) if (got as u64) < t => format!(
            "{got} of the {t} {what} GitHub reports were returned: the history is incomplete, and the writers below are of those returned only"
        ),
        Some(t) if t as usize >= cap => format!(
            "{t} {what} retained, at GitHub's cap of {cap}: older ones may be gone with their writers, so the writers below are the retained ones only"
        ),
        Some(t) => format!("{t} {what}, all retained and returned"),
    }
}

fn writer(m: &Machine, login: &str, w: &Writer) -> String {
    let mut marks = Vec::new();
    if let Some(t) = &m.cfg.trust {
        if let Some(inbox) = &t.inbox
            && inbox.authors.iter().any(|a| a.matches(w.id))
        {
            marks.push("an inbox author".to_string());
        }
        if let Some(engines) = &t.engines
            && engines.iter().any(|a| a.matches(w.id))
        {
            marks.push("an engines door author".to_string());
        }
        if let Some(app) = t.apps.iter().find(|a| a.id > 0 && Some(a.id) == w.id) {
            marks.push(format!("the App {}", app.name));
        }
    }
    if m.cfg.authors.iter().any(|a| a.matches(w.id)) {
        marks.push("a probe author".into());
    }
    if m.cfg.machinery.as_ref().is_some_and(|a| a.matches(w.id))
        && !marks.iter().any(|x| x.starts_with("the App"))
    {
        marks.push("the machinery App".into());
    }
    format!(
        "{login} ({}{}){}; {}",
        if w.kind.is_empty() {
            "an account"
        } else {
            &w.kind
        },
        w.id.map_or(", no ID".to_string(), |id| format!(" {id}")),
        did(w),
        if marks.is_empty() {
            "on none of the deployed trust lists".to_string()
        } else {
            marks.join(", ")
        }
    )
}

fn did(w: &Writer) -> String {
    let mut what = Vec::new();
    if w.author {
        what.push("opened it".to_string());
    }
    for (n, one, many) in [
        (w.edits, "edit", "edits"),
        (w.deletions, "edit deleted", "edits deleted"),
        (w.renames, "rename", "renames"),
    ] {
        match n {
            0 => {}
            1 => what.push(format!("1 {one}")),
            n => what.push(format!("{n} {many}")),
        }
    }
    if what.is_empty() {
        String::new()
    } else {
        format!(": {}", what.join(", "))
    }
}
