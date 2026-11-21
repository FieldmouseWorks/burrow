//! What a stuck-role job is shown of a PR's discussion: the review
//! findings and the answers to them, by the accounts the repo's flow reads
//! them from alone (its `review_authors` and `flow_authors`), read by the
//! broker before the job starts. Anyone else's comment, review or thread
//! is left out, and the job is told not to look for more: a job acts on
//! text from its configured authors alone.
use burrow_github::{Api, pages};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

/// The accounts the flow in `repo` reads findings and their answers from,
/// from its configuration in `flow` (`OWNER/NAME.json`, lowercased, as the
/// machine renders them): none, and nothing of the discussion is shown.
pub fn authors(flow: &Path, repo: &str) -> Result<BTreeSet<String>, String> {
    let path = flow.join(format!("{}.json", repo.to_lowercase()));
    let text = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let cfg: conary_github::flow::Config =
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut authors = cfg.settings.review_authors;
    authors.extend(cfg.settings.flow_authors);
    if authors.is_empty() {
        return Err(format!(
            "the flow in {repo} reads findings from no account, so a stuck-role job would have none to read"
        ));
    }
    Ok(authors)
}

/// PR `pr`'s review threads and review bodies by `authors`, each comment
/// quoted under who wrote it, and how many by anyone else were left out.
pub fn read(
    api: &mut dyn Api,
    repo: &str,
    pr: u64,
    authors: &BTreeSet<String>,
) -> Result<String, String> {
    let mut all = |path: String| -> Result<Vec<Value>, String> {
        Ok(pages(&mut *api, &path)
            .map_err(|e| format!("{repo}#{pr}'s discussion can't be read: {e}"))?
            .into_iter()
            .flat_map(|page| page.as_array().cloned().unwrap_or_default())
            .collect())
    };
    let comments = all(format!("repos/{repo}/pulls/{pr}/comments?per_page=100"))?;
    let reviews = all(format!("repos/{repo}/pulls/{pr}/reviews?per_page=100"))?;
    Ok(shown(&comments, &reviews, authors))
}

fn login(v: &Value) -> Option<&str> {
    v["user"]["login"].as_str()
}

/// What [`read`] shows of `comments` (review comments, REST's) and
/// `reviews`.
pub fn shown(comments: &[Value], reviews: &[Value], authors: &BTreeSet<String>) -> String {
    let by = |v: &Value| login(v).is_some_and(|l| authors.contains(l));
    let said = |v: &Value| {
        format!(
            "{} wrote:\n{}\n",
            login(v).unwrap_or("(someone deleted)"),
            burrow_brief::quote(v["body"].as_str().unwrap_or(""))
        )
    };
    let mut left = 0;
    // Threads by their first comment's id, in order.
    let mut threads: BTreeMap<u64, Vec<&Value>> = BTreeMap::new();
    for c in comments {
        if !by(c) {
            left += 1;
            continue;
        }
        let id = c["in_reply_to_id"]
            .as_u64()
            .or_else(|| c["id"].as_u64())
            .unwrap_or(0);
        threads.entry(id).or_default().push(c);
    }
    let mut out = String::new();
    for thread in threads.values() {
        let first = thread[0];
        let line = first["line"]
            .as_u64()
            .or_else(|| first["original_line"].as_u64())
            .map_or(String::new(), |l| format!(":{l}"));
        let _ = writeln!(
            out,
            "Review thread on {}{line}:",
            first["path"].as_str().unwrap_or("?")
        );
        for c in thread {
            out.push_str(&said(c));
        }
        out.push('\n');
    }
    for r in reviews {
        if r["body"].as_str().is_none_or(|b| b.trim().is_empty()) {
            continue;
        }
        if !by(r) {
            left += 1;
            continue;
        }
        let _ = writeln!(out, "A review's body:");
        out.push_str(&said(r));
        out.push('\n');
    }
    if out.is_empty() {
        out.push_str("(none)\n");
    }
    if left > 0 {
        let _ = writeln!(
            out,
            "({left} comments by other accounts are left out: they're not yours to act on.)"
        );
    }
    out.trim_end().to_owned()
}
