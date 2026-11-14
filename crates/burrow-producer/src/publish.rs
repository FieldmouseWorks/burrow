//! A signed result's check run (Conary's `docs/m6-plugins.md`, "Who
//! produces what"): started in progress, completed with the statement as
//! its output text, or completed as `cancelled` with no statement when its
//! work is abandoned. It goes out through the machine's GitHub door
//! ([`burrow_github::Api`]); that App isn't the trust root, the signature
//! is.
use crate::serve::Work;
use crate::sign::Signed;
use burrow_github::{Api, call};
use serde_json::{Value, json};

/// A check run a producer started.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Run {
    pub repo: String,
    pub id: u64,
    pub name: String,
}

/// Start check run `name` on `head`, in progress, carrying `external_id`
/// (its record's id), so a crash's run can be found again ([`find`]).
pub fn start(
    api: &mut dyn Api,
    repo: &str,
    head: &str,
    name: &str,
    external_id: &str,
) -> Result<Run, String> {
    let body = json!({"name": name, "head_sha": head, "status": "in_progress",
        "external_id": external_id});
    let got = call(
        api,
        "POST",
        &format!("repos/{repo}/check-runs"),
        Some(&body),
        None,
    )?;
    let id = got.json()?["id"]
        .as_u64()
        .ok_or("GitHub's answer names no check run")?;
    Ok(Run {
        repo: repo.into(),
        id,
        name: name.into(),
    })
}

/// The check run with `external_id` that a producer started as `name` on
/// `head`, if there is one: what startup finishes after a crash.
pub fn find(
    api: &mut dyn Api,
    repo: &str,
    head: &str,
    name: &str,
    external_id: &str,
) -> Result<Option<Run>, String> {
    let path = format!(
        "repos/{repo}/commits/{head}/check-runs?check_name={}&filter=all&per_page=100",
        burrow_github::encode(name)
    );
    let pages = burrow_github::pages(api, &path)?;
    Ok(pages
        .iter()
        .flat_map(|p| p["check_runs"].as_array().cloned().unwrap_or_default())
        .find(|r| r["external_id"] == external_id && r["name"] == name)
        .and_then(|r| r["id"].as_u64())
        .map(|id| Run {
            repo: repo.into(),
            id,
            name: name.into(),
        }))
}

/// Complete `run` with `signed`, unless `work` was abandoned: then it's
/// completed as `cancelled`, publishing nothing, and that's an error.
pub fn complete(api: &mut dyn Api, work: &Work, run: &Run, signed: &Signed) -> Result<(), String> {
    if signed.check() != run.name {
        return Err(format!(
            "a statement for {} can't complete check run {}",
            signed.check(),
            run.name
        ));
    }
    if work.abandoned() {
        cancel(api, run)?;
        return Err("its work was abandoned, so it publishes nothing".into());
    }
    patch(
        api,
        run,
        json!({"status": "completed", "conclusion": signed.conclusion(),
            "output": {"title": signed.check(), "summary": "A producer's signed result.",
                "text": signed.text()}}),
    )
}

/// Complete `run` as `cancelled`, with no statement: its work stopped.
pub fn cancel(api: &mut dyn Api, run: &Run) -> Result<(), String> {
    patch(
        api,
        run,
        json!({"status": "completed", "conclusion": "cancelled",
            "output": {"title": "Cancelled", "summary": "Its work stopped before it finished, so it says nothing."}}),
    )
}

fn patch(api: &mut dyn Api, run: &Run, body: Value) -> Result<(), String> {
    call(
        api,
        "PATCH",
        &format!("repos/{}/check-runs/{}", run.repo, run.id),
        Some(&body),
        None,
    )?;
    Ok(())
}
