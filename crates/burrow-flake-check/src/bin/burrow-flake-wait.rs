//! Wait for the exact default-branch push's kept builds. Planning is done by
//! the dependency-free runner; this binary uses the shared GitHub client.
use burrow_github::{Api, Gh, encode, pages};
use serde_json::Value;
use std::{
    env, fs,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
fn var(k: &str) -> String {
    env::var(k).unwrap_or_default()
}

/// Where the base push's leg stands, as one poll found it.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum State {
    /// Its kept builds are saved under the exact key.
    Saved,
    /// Its leg is running on a runner: it ends, saved or not, without
    /// anything this leg holds.
    Building,
    /// Its leg hasn't got a runner yet, and may be waiting for this one.
    Queued,
    /// No run of it is going, or its leg ended without saving.
    Gone,
}

/// Poll until the producer's leg stops building. No cap: a leg on a runner
/// ends by itself (its job's own timeout at the latest), and every minute it
/// has run brings its save closer, so giving up part way would only add the
/// wait to a cold build.
fn poll(every: Duration, mut state: impl FnMut() -> Result<State>) -> Result<State> {
    loop {
        match state()? {
            State::Building => std::thread::sleep(every),
            done => return Ok(done),
        }
    }
}

struct Producer<'a> {
    repo: &'a str,
    workflow: &'a str,
    branch: &'a str,
    tip: &'a str,
    key: &'a str,
    /// The leg's job name, as the workflow names it; a caller's reusable
    /// workflow puts its own job's name and " / " before it.
    job: &'a str,
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
    fn jobs_path(&self, run: u64) -> String {
        format!("repos/{}/actions/runs/{run}/jobs?per_page=100", self.repo)
    }
    fn caches_path(&self) -> String {
        format!(
            "repos/{}/actions/caches?key={}&ref={}&per_page=100",
            self.repo,
            encode(self.key),
            encode(&format!("refs/heads/{}", self.branch))
        )
    }
    /// The ids of the producer's runs still going.
    fn running(&self, runs: &Value) -> Vec<u64> {
        runs["workflow_runs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| {
                r["name"] == self.workflow
                    && r["event"] == "push"
                    && r["head_sha"] == self.tip
                    && r["head_branch"] == self.branch
                    && r["status"] != "completed"
            })
            .filter_map(|r| r["id"].as_u64())
            .collect()
    }
    /// The statuses of this leg's jobs in one page of a run's jobs.
    fn legs<'v>(&self, jobs: &'v Value) -> impl Iterator<Item = &'v str> {
        let nested = format!(" / {}", self.job);
        jobs["jobs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(move |j| {
                j["name"]
                    .as_str()
                    .is_some_and(|n| n == self.job || n.ends_with(&nested))
            })
            .map(|j| j["status"].as_str().unwrap_or(""))
    }
    fn cached(&self, caches: &Value) -> bool {
        let reference = format!("refs/heads/{}", self.branch);
        caches["actions_caches"].as_array().is_some_and(|caches| {
            caches
                .iter()
                .any(|c| c["key"] == self.key && c["ref"] == reference)
        })
    }
    /// The producer's runs still going, read once: a run's id outlives
    /// its attempts, and its jobs say the rest.
    fn runs(&self, api: &mut dyn Api) -> Result<Vec<u64>> {
        Ok(pages(api, &self.runs_path())
            .map_err(|e| e.to_string())?
            .iter()
            .flat_map(|p| self.running(p))
            .collect())
    }
    /// One poll: one request a run while its leg builds, and the exact
    /// cache only once it doesn't, which keeps an hours-long wait well
    /// inside the token's hourly requests.
    fn state(&self, api: &mut dyn Api, runs: &[u64]) -> Result<State> {
        let mut statuses = Vec::new();
        for &id in runs {
            let mut found = false;
            for page in pages(api, &self.jobs_path(id)).map_err(|e| e.to_string())? {
                for status in self.legs(&page) {
                    found = true;
                    statuses.push(status.to_owned());
                }
            }
            // A run whose leg isn't there yet (its lane job still running)
            // has that leg still to queue.
            if !found {
                statuses.push("queued".to_owned());
            }
        }
        if statuses.iter().any(|s| s == "in_progress") {
            return Ok(State::Building);
        }
        // Read the cache AFTER status, including on the first poll. A leg
        // that finished during planning or between polls has already saved
        // everything it can save, even if its final conclusion was failure.
        let saved = pages(api, &self.caches_path())
            .map_err(|e| e.to_string())?
            .iter()
            .any(|p| self.cached(p));
        Ok(if saved {
            State::Saved
        } else if statuses.iter().any(|s| s != "completed") {
            State::Queued
        } else {
            State::Gone
        })
    }
}
fn wait() -> Result<()> {
    let cold = fs::read_to_string(PathBuf::from(var("RUNNER_TEMP")).join("cold.txt"))?;
    if cold.trim().is_empty() {
        println!("nothing to build cold, so nothing to wait for");
        return Ok(());
    }
    let (repo, workflow, branch, tip, job) = (
        var("GITHUB_REPOSITORY"),
        var("GITHUB_WORKFLOW"),
        var("DEFAULT_BRANCH"),
        var("TIP"),
        var("LEG_JOB"),
    );
    if tip.is_empty() || job.is_empty() {
        return Err(
            "the merge step's base tip (TIP) and the leg's job name (LEG_JOB) are both needed"
                .into(),
        );
    }
    let key = format!("{}{tip}", var("KEPT_KEY"));
    let producer = Producer {
        repo: &repo,
        workflow: &workflow,
        branch: &branch,
        tip: &tip,
        key: &key,
        job: &job,
    };
    // Each poll's requests, pages and retry pauses share one deadline: a
    // request still going past it fails the step, which says so.
    let request = Duration::from_secs(var("WAIT_REQUEST").parse().unwrap_or(120));
    let mut api = Gh::default();
    println!(
        "checking {workflow}'s default-branch push at {tip}, its {job}, for the builds this leg would compile:\n{cold}"
    );
    let started = Instant::now();
    api.until = Some(Instant::now() + request);
    let runs = producer.runs(&mut api)?;
    let state = poll(
        Duration::from_secs(var("WAIT_POLL").parse().unwrap_or(60)),
        || {
            api.until = Some(Instant::now() + request);
            producer.state(&mut api, &runs)
        },
    )?;
    println!(
        "{state:?} after {}s: {}",
        started.elapsed().as_secs(),
        match state {
            State::Saved => "restoring its kept builds",
            State::Queued => "its leg has no runner yet, and may want this one, so building cold",
            State::Gone =>
                "no run of it is going, or its leg ended without saving, so building cold",
            State::Building => unreachable!("poll returns once it stops building"),
        }
    );
    if state == State::Saved
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
