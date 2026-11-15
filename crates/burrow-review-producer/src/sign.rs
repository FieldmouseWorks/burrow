//! The sign pass (Conary's `docs/m6-plugins.md`, R, "Signing"): the flow
//! publishes each review itself, and burrow-events runs this pass when a
//! review is submitted (and again while a look's publication is still to
//! come, booked in its wake file). It's the one part of the producer that
//! holds the key, as a unit of its own.
//!
//! For each kept look, it finds the role's reviews on the look's head and
//! signs `review_receipt(head, body)` as `conary/review/ROLE` for one only
//! when the whole of it is what the look recorded:
//!
//! - posted by one of the flow's review accounts, on the look's head;
//! - its body exactly the one `flow::review_body` makes of the look's role,
//!   head, base branch, scope and findings, in order, at its `reviewed_at`:
//!   so its marker names them all, and its count is theirs;
//! - `reviewed_at` no earlier than the look ended and no later than now;
//! - the PR's description and the earlier findings and replies the look
//!   quoted read as they did then.
//!
//! A review that differs in any of these is never signed. A signed look is
//! let go, and so is one whose publication didn't come within `keep`.
//! Startup's half (burrow-producer's `reconcile`) finishes what a crash
//! left of a signature before anything else.
use crate::history::{self, Voices};
use crate::look::{Look, Looks};
use crate::{Config, now};
use burrow_github::{Api, call, pages};
use burrow_producer::publish;
use burrow_producer::records::Records;
use burrow_producer::sign::{Review, Signable, Signer};
use conary_github::flow::{Revision, fingerprint, issuance, review_body};
use serde_json::{Value, json};
use std::fs;

/// How long the pass waits for a publication still to come before it
/// looks again, when no review event comes first.
pub const AGAIN: u64 = 600;

/// What a pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Passed {
    /// Signatures a crash left, finished.
    pub finished: usize,
    /// Looks signed now, by id.
    pub signed: Vec<String>,
    /// Looks whose publication is still to come.
    pub waiting: Vec<String>,
    /// What failed, to be tried again on the next pass.
    pub failed: Vec<String>,
}

/// One pass over every kept look. Nothing that fails for one record or
/// look stops the rest: it's said, and the next pass is booked for it.
pub fn pass(cfg: &Config, api: &mut dyn Api, signer: &Signer) -> Result<Passed, String> {
    let records = Records::open(&cfg.sign.join("records"))
        .map_err(|e| format!("{}: {e}", cfg.sign.join("records").display()))?;
    let issued = cfg.sign.join("issued");
    fs::create_dir_all(&issued).map_err(|e| format!("{}: {e}", issued.display()))?;
    let looks = Looks::open(&cfg.state.join("looks"))?;
    let mut passed = Passed::default();
    match publish::reconcile::<Review>(api, &records, signer) {
        Ok(n) => passed.finished = n,
        Err(why) => passed.failed.push(format!("what a crash left: {why}")),
    }
    for look in looks.all()? {
        let id = look.id.clone();
        let tag = format!("{}#{}: look {id}", look.repo, look.pr);
        match one(cfg, api, signer, &records, &looks, &issued, look) {
            Ok(Some(true)) => passed.signed.push(id),
            Ok(Some(false)) => passed.waiting.push(id),
            Ok(None) => {}
            Err(why) => passed.failed.push(format!("{tag}: {why}")),
        }
    }
    for why in &passed.failed {
        eprintln!("{why}");
    }
    let next = if passed.waiting.is_empty() && passed.failed.is_empty() {
        json!({})
    } else {
        json!({"": now() + AGAIN})
    };
    let wake = cfg.sign.join("wake.json");
    let tmp = wake.with_extension("tmp");
    fs::write(&tmp, next.to_string()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, &wake).map_err(|e| format!("{}: {e}", wake.display()))?;
    Ok(passed)
}

/// One look: signed now (true), still waiting (false), or let go (None).
fn one(
    cfg: &Config,
    api: &mut dyn Api,
    signer: &Signer,
    records: &Records,
    looks: &Looks,
    issued: &std::path::Path,
    mut look: Look,
) -> Result<Option<bool>, String> {
    if look.signed.is_some() || now() > look.finished.saturating_add(cfg.keep) {
        if look.signed.is_none() {
            eprintln!(
                "{}#{}: no publication of look {} came; it's let go",
                look.repo, look.pr, look.id
            );
        }
        looks.drop(&look.id)?;
        return Ok(None);
    }
    let body = match published(cfg, api, &look) {
        Ok(Some(body)) => body,
        Ok(None) => return Ok(Some(false)),
        Err(why) => {
            eprintln!("{}#{}: look {}: {why}", look.repo, look.pr, look.id);
            return Ok(Some(false));
        }
    };
    // One publication is signed once, whichever of the looks that match it
    // gets there first: another look with the same findings on the same
    // head is let go.
    let publication = issuance(&look.head, &body);
    let mark = issued.join(crate::digest(&publication));
    if mark.exists() {
        looks.drop(&look.id)?;
        return Ok(None);
    }
    let review = Review {
        repo: look.repo.clone(),
        role: look.role.clone(),
        head: look.head.clone(),
        tree: look.tree.clone(),
        body,
    };
    let mut record = records
        .begin::<Review>(review.target())
        .map_err(|e| e.to_string())?;
    records.keep(&mut record, review)?;
    // Committed before anything says so: a crash after it publishes it at
    // the next pass's start, never withdraws it.
    records.commit(&mut record)?;
    fs::write(&mark, record.id()).map_err(|e| format!("{}: {e}", mark.display()))?;
    look.signed = Some(publication);
    looks.keep(&look)?;
    publish::publish(api, records, signer, &mut record)?;
    looks.drop(&look.id)?;
    Ok(Some(true))
}

/// The body of `look`'s publication, once there's one that holds all it
/// recorded: None while there's none (it hasn't come, or none that came
/// holds).
pub fn published(cfg: &Config, api: &mut dyn Api, look: &Look) -> Result<Option<String>, String> {
    let flow = cfg.flow(&look.repo)?;
    let authors = &flow.settings.review_authors;
    let (repo, pr) = (&look.repo, look.pr);
    let reviews: Vec<Value> = pages(
        api,
        &format!("repos/{repo}/pulls/{pr}/reviews?per_page=100"),
    )
    .map_err(String::from)?
    .into_iter()
    .flat_map(|page| page.as_array().cloned().unwrap_or_default())
    .collect();
    let mut found = None;
    for r in &reviews {
        let body = r["body"].as_str().unwrap_or_default();
        if r["commit_id"].as_str() != Some(look.head.as_str())
            || !authors.contains(r["user"]["login"].as_str().unwrap_or_default())
        {
            continue;
        }
        match holds(look, body) {
            Ok(()) => found = Some(body.to_owned()),
            Err(why) if why != NOT_ITS => {
                eprintln!("{repo}#{pr}: a review on {} isn't signed: {why}", look.head);
            }
            Err(_) => {}
        }
    }
    let Some(body) = found else {
        return Ok(None);
    };
    // What the look read, read again now.
    let now_pr = call(api, "GET", &format!("repos/{repo}/pulls/{pr}"), None, None)?.json()?;
    let scope = fingerprint(&format!(
        "{}\n{}",
        now_pr["title"].as_str().unwrap_or_default(),
        now_pr["body"].as_str().unwrap_or_default()
    ));
    if scope != look.scope {
        return Err("the PR's description changed since the look read it".into());
    }
    let voices = Voices {
        review: authors,
        trusted: &flow.trusted_authors,
    };
    let prior = history::read(
        api,
        repo,
        pr,
        &look.role,
        &voices,
        Some(&issuance(&look.head, &body)),
    )?;
    if crate::digest(&prior) != look.prior {
        return Err("the earlier findings and replies it quoted changed since the look".into());
    }
    Ok(Some(body))
}

/// Why a review isn't the look's, when it's no review of the role's at all.
const NOT_ITS: &str = "it isn't the role's review";

/// Whether `body` is the publication of `look`: exactly the body the flow
/// makes of it, at a time it could have been made.
pub fn holds(look: &Look, body: &str) -> Result<(), String> {
    let name = format!("<!-- {}-review:v2 ", look.role);
    let marker: Value = body
        .trim_end()
        .lines()
        .last()
        .and_then(|l| l.strip_prefix(&name))
        .and_then(|l| l.strip_suffix(" -->"))
        .and_then(|m| serde_json::from_str(m).ok())
        .ok_or(NOT_ITS)?;
    let at = marker["reviewed_at"]
        .as_u64()
        .ok_or("its marker has no reviewed_at")?;
    if at < look.finished {
        return Err(format!(
            "it says it was reviewed at {at}, before the look ended at {}",
            look.finished
        ));
    }
    if at > now() {
        return Err(format!(
            "it says it was reviewed at {at}, which hasn't come"
        ));
    }
    let revision = Revision {
        head: look.head.clone(),
        base: look.base_ref.clone(),
        description: look.scope.clone(),
    };
    if body != review_body(&look.role, &revision, at, &look.findings) {
        return Err(
            "it isn't the body the look's findings, base and scope make: a finding, its count, the base or the scope differs"
                .into(),
        );
    }
    Ok(())
}
