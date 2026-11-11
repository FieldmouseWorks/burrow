//! Event-driven Actions reader; errors leave the previous watch untouched
//! so an unavailable API cannot clear a stall or masquerade as health.
use burrow_watchdog::{
    degradations::{Run, evaluate, keys, latest_jobs, trusted},
    watch::File,
};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf, process::Command, time::Duration};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    file: PathBuf,
    repos: BTreeMap<String, Vec<String>>,
    pr_threshold: usize,
    authors: Vec<String>,
    kick: String,
}
fn api(path: &str) -> Result<Value, String> {
    let out = Command::new("gh")
        .args(["api", path])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("GitHub request failed: {path}: {}", out.status));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}
fn timestamp(s: &str) -> Result<f64, String> {
    s.parse::<jiff::Timestamp>()
        .map(|t| t.as_second() as f64)
        .map_err(|e| e.to_string())
}
fn collect(
    repo: &str,
    workflow: &str,
    query: &str,
    default: bool,
    authors: &[String],
    now: f64,
    cache: &mut BTreeMap<String, Run>,
) -> Result<Vec<Run>, String> {
    let mut runs = Vec::new();
    let mut page = 1;
    loop {
        let got = api(&format!(
            "repos/{repo}/actions/workflows/{workflow}/runs?status=completed&{query}&per_page=100&page={page}"
        ))?;
        let list = got["workflow_runs"]
            .as_array()
            .ok_or("missing workflow_runs")?;
        let mut end = false;
        for r in list {
            let at = timestamp(r["created_at"].as_str().ok_or("missing created_at")?)?;
            if !default && at < now - 86400.0 {
                end = true;
                break;
            }
            // PR-controlled text reaches a trusted fix job: authenticate it
            // before it is read or cached.
            if !default && !trusted(r, repo, authors) {
                continue;
            }
            let id = r["id"].as_u64().ok_or("missing run id")?;
            let attempt = r["run_attempt"].as_u64().ok_or("missing run attempt")?;
            let key = format!("{repo}/{id}/{attempt}");
            let run = if let Some(run) = cache.get(&key) {
                run.clone()
            } else {
                let mut all = Vec::new();
                let mut job_page = 1;
                loop {
                    let jobs = api(&format!(
                        "repos/{repo}/actions/runs/{id}/jobs?filter=all&per_page=100&page={job_page}"
                    ))?;
                    let list = jobs["jobs"].as_array().ok_or("missing jobs")?;
                    all.extend(list.iter().cloned());
                    if list.len() < 100 {
                        break;
                    }
                    job_page += 1;
                }
                let found = keys(&latest_jobs(&all));
                Run {
                    id,
                    at,
                    keys: found,
                    link: r["html_url"].as_str().ok_or("missing run URL")?.into(),
                }
            };
            cache.insert(key, run.clone());
            runs.push(run);
            if default && runs.len() == 2 {
                end = true;
                break;
            }
        }
        if end || list.len() < 100 {
            break;
        }
        page += 1;
    }
    Ok(runs)
}
fn pass() -> Result<(), String> {
    let path = std::env::var_os("DEGRADATIONS_CONFIG").ok_or("DEGRADATIONS_CONFIG isn't set")?;
    let cfg: Config = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if cfg.pr_threshold == 0 {
        return Err("prThreshold must be positive".into());
    }
    if cfg.authors.is_empty() {
        return Err("authors must name who may supply PR signals".into());
    }
    if std::env::args().nth(1).as_deref() == Some("--check") {
        return Ok(());
    }
    let old: File = match std::fs::read(&cfg.file) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
        Err(e) => return Err(e.to_string()),
    };
    let cache_file = cfg.file.with_extension("runs.json");
    let mut cache: BTreeMap<String, Run> = match std::fs::read(&cache_file) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => return Err(e.to_string()),
    };
    let now = burrow_watchdog::now();
    let mut file = File {
        at: now,
        ..File::default()
    };
    for (repo, workflows) in cfg.repos {
        let metadata = api(&format!("repos/{repo}"))?;
        let branch = metadata["default_branch"]
            .as_str()
            .ok_or("missing default branch")?;
        // Branches can contain slash or punctuation: never interpolate raw
        // ref names into a query string.
        let branch: String = branch.bytes().map(|b| format!("%{b:02X}")).collect();
        for workflow in workflows {
            let defaults = collect(
                &repo,
                &workflow,
                &format!("event=push&branch={branch}"),
                true,
                &cfg.authors,
                now,
                &mut cache,
            )?;
            let prs = collect(
                &repo,
                &workflow,
                "event=pull_request",
                false,
                &cfg.authors,
                now,
                &mut cache,
            )?;
            file.items.extend(
                evaluate(
                    &format!("{repo}/{workflow}"),
                    &defaults,
                    &prs,
                    cfg.pr_threshold,
                    now,
                    &old,
                )
                .items,
            );
        }
    }
    cache.retain(|_, r| r.at >= now - 86400.0);
    write(&cache_file, &cache)?;
    write(&cfg.file, &file)?;
    burrow_events::kick::kick(&cfg.kick, Duration::from_secs(10))
}
fn write(path: &std::path::Path, value: &impl serde::Serialize) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(tmp, path).map_err(|e| e.to_string())
}
fn main() {
    if let Err(e) = pass() {
        eprintln!("burrow-degradations: {e}");
        std::process::exit(1);
    }
}
