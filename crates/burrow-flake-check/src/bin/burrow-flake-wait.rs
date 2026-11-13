//! Wait for the exact default-branch push's kept builds. Planning is done by
//! the dependency-free runner; this binary uses the shared GitHub client.
use burrow_github::{Api, Gh, encode, pages};
use serde_json::Value;
use std::{
    env, fs,
    io::Write,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
fn var(k: &str) -> String {
    env::var(k).unwrap_or_default()
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Saved,
    Gone,
    TimedOut,
}

fn poll(
    max: Duration,
    every: Duration,
    mut state: impl FnMut() -> Result<(bool, bool)>,
) -> Result<Outcome> {
    let start = Instant::now();
    loop {
        // A request cut off by the deadline is the deadline, not a failure.
        let (saved, running) = match state() {
            Ok(s) => s,
            Err(_) if start.elapsed() >= max => return Ok(Outcome::TimedOut),
            Err(e) => return Err(e),
        };
        if saved {
            return Ok(Outcome::Saved);
        }
        if !running {
            return Ok(Outcome::Gone);
        }
        if start.elapsed() >= max {
            return Ok(Outcome::TimedOut);
        }
        std::thread::sleep(every.min(max.saturating_sub(start.elapsed())));
    }
}

struct Producer<'a> {
    repo: &'a str,
    workflow: &'a str,
    branch: &'a str,
    tip: &'a str,
    key: &'a str,
}
impl Producer<'_> {
    fn runs_path(&self) -> String {
        format!(
            "repos/{}/actions/runs?event=push&head_sha={}&branch={}&per_page=100",
            self.repo,
            encode(self.tip),
            encode(self.branch)
        )
    }
    fn caches_path(&self) -> String {
        format!(
            "repos/{}/actions/caches?key={}&ref={}&per_page=100",
            self.repo,
            encode(self.key),
            encode(&format!("refs/heads/{}", self.branch))
        )
    }
    fn running(&self, runs: &Value) -> bool {
        runs["workflow_runs"].as_array().is_some_and(|runs| {
            runs.iter().any(|r| {
                r["name"] == self.workflow
                    && r["event"] == "push"
                    && r["head_sha"] == self.tip
                    && r["head_branch"] == self.branch
                    && r["status"] != "completed"
            })
        })
    }
    fn cached(&self, caches: &Value) -> bool {
        let reference = format!("refs/heads/{}", self.branch);
        caches["actions_caches"].as_array().is_some_and(|caches| {
            caches
                .iter()
                .any(|c| c["key"] == self.key && c["ref"] == reference)
        })
    }
    fn state(&self, api: &mut dyn Api) -> Result<(bool, bool)> {
        let running = pages(api, &self.runs_path())
            .map_err(|e| e.to_string())?
            .iter()
            .any(|p| self.running(p));
        // Read the cache AFTER status, including on the first poll. A producer
        // that finished during planning or between polls has already saved
        // everything it can save, even if its final conclusion was failure.
        let saved = pages(api, &self.caches_path())
            .map_err(|e| e.to_string())?
            .iter()
            .any(|p| self.cached(p));
        Ok((saved, running))
    }
}
fn wait() -> Result<()> {
    let cold = fs::read_to_string(PathBuf::from(var("RUNNER_TEMP")).join("cold.txt"))?;
    if cold.trim().is_empty() {
        println!("nothing to build cold, so nothing to wait for");
        return Ok(());
    }
    let o = Command::new("git").args(["rev-parse", "HEAD^1"]).output()?;
    if !o.status.success() {
        return Err("could not read the merged base tip".into());
    }
    let tip = String::from_utf8(o.stdout)?
        .trim_end_matches('\n')
        .to_owned();
    let (repo, workflow, branch) = (
        var("GITHUB_REPOSITORY"),
        var("GITHUB_WORKFLOW"),
        var("DEFAULT_BRANCH"),
    );
    let key = format!("{}{tip}", var("KEPT_KEY"));
    let producer = Producer {
        repo: &repo,
        workflow: &workflow,
        branch: &branch,
        tip: &tip,
        key: &key,
    };
    let max: u64 = var("WAIT_MAX").parse().unwrap_or(3600);
    // One absolute deadline bounds every request, page and retry pause.
    let mut api = Gh {
        until: Some(Instant::now() + Duration::from_secs(max)),
        ..Gh::default()
    };
    println!(
        "checking {workflow}'s default-branch push at {tip} for the builds this leg would compile:\n{cold}"
    );
    let outcome = poll(
        Duration::from_secs(max),
        Duration::from_secs(var("WAIT_POLL").parse().unwrap_or(30)),
        || producer.state(&mut api),
    )?;
    println!("{outcome:?}");
    if outcome == Outcome::Saved
        && let Some(path) = env::var_os("GITHUB_OUTPUT")
    {
        let mut out = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)?;
        writeln!(out, "key={key}\nsaved=true")?;
    }
    Ok(())
}
fn main() {
    if let Err(e) = wait() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
