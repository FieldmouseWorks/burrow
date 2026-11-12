//! One visibility path for inbox, sorting and steward jobs alike.
use super::load;
use crate::{config::Config, job::Job, read, swap};
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Deserialize, Serialize, PartialEq)]
struct Label {
    repo: String,
    number: String,
    name: String,
}

pub(super) fn issue(cfg: &Config, job: &Job) -> Option<(String, String)> {
    if let Some(target) =
        read(&super::host_path(job, "source")).and_then(|s| crate::github::target(&s))
    {
        return Some(target);
    }
    // Legacy inbox records are also host-owned. Never trust job/link.
    let inbox = cfg.inbox.as_ref()?;
    for entry in fs::read_dir(&inbox.state).ok()?.flatten() {
        let dir = entry.path();
        if read(&dir.join("job")).as_deref() == Some(job.id) {
            return read(&dir.join("issue")).and_then(|s| crate::github::target(&s));
        }
    }
    None
}

/// A failed write stays pending and the watcher books a retry. Remember exactly
/// which label we own, so an answered question clears that label alone.
pub(crate) fn tend(cfg: &Config, job: &Job) -> Result<(), String> {
    let _held = job.lock().map_err(|e| e.to_string())?;
    crate::question::recover(job)?;
    let needs_owner = super::host_path(job, "question-needs-owner").exists();
    let active = super::follows(job).is_none()
        && (needs_owner
            || (load(job).is_some() && !super::host_path(job, "question-reader").exists()));
    let desired = if active {
        issue(cfg, job).map(|(repo, number)| Label {
            repo,
            number,
            name: if needs_owner {
                &cfg.needs_owner_label
            } else {
                &cfg.waiting_owner_label
            }
            .clone(),
        })
    } else {
        None
    };
    let path = super::host_path(job, "label");
    let before: Option<Label> = read(&path)
        .map(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        .transpose()?;
    if before == desired {
        return Ok(());
    }
    super::ensure(job)?;
    let mut client = crate::github::client(cfg);
    if let Some(old) = before {
        let endpoint = format!("repos/{}/issues/{}", old.repo, old.number);
        let issue =
            crate::github::api(cfg, &endpoint).ok_or("could not read the question's labels")?;
        if issue["labels"]
            .as_array()
            .is_some_and(|ls| ls.iter().any(|l| l["name"] == old.name))
        {
            burrow_github::call_repeating(
                &mut client,
                "DELETE",
                &format!("{endpoint}/labels/{}", burrow_github::encode(&old.name)),
                None,
            )
            .map_err(|e| e.why)?;
        }
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    if let Some(label) = desired {
        burrow_github::call_repeating(
            &mut client,
            "POST",
            &format!("repos/{}/issues/{}/labels", label.repo, label.number),
            Some(&serde_json::json!({"labels":[label.name]})),
        )
        .map_err(|e| e.why)?;
        swap(
            &path,
            &serde_json::to_string(&label).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    crate::job::kick(cfg, vec!["autoland".into()]);
    Ok(())
}
