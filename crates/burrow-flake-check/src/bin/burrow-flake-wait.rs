//! Wait for the exact default-branch push's kept builds, or for a newer
//! push's that makes that push skip its save. Planning is done by the
//! dependency-free runner; this binary uses the shared GitHub client.
use burrow_github::{Api, Gh, encode, pages};
use burrow_keep::prune::Leg;
use jiff::{SignedDuration, Timestamp};
use serde_json::Value;
use std::{
    env, fs,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
/// How long a base push's leg is worth waiting for from its start, in
/// seconds, unless `WAIT_DEADLINE` says otherwise: not at all. The wait was
/// for codex, which took 2 to 3 hours to build cold on a hosted runner;
/// #1204 took it off the machine. On 2026-11-19, across 45 PR legs, the 18
/// that waited spent 12.3 minutes waiting and 4.9 building (17.2), and the
/// 27 that didn't built in 7.6. So a leg running now is never waited for.
/// Builds a base push has already saved are still restored, without
/// waiting (`State::Saved` is read before `State::Late`), and #989 retires
/// this wait with the kept builds.
const DEADLINE: i64 = 0;
/// How often, in seconds, the leg's kept entries are read while the base
/// push's leg builds, unless `WAIT_NEWER` says otherwise: a newer push's
/// entry turning up means the base push won't save (#1164). Each read is
/// one listing and one comparison an entry's commit, kept to every five
/// minutes so an hours-long wait stays well inside the token's requests.
const NEWER: u64 = 5 * 60;
fn var(k: &str) -> String {
    env::var(k).unwrap_or_default()
}

/// Where the base push's leg stands, as one poll found it.
#[derive(Debug, PartialEq, Eq, Clone)]
enum State {
    /// Its kept builds are saved under the exact key.
    Saved,
    /// A newer push's leg saved its kept builds first, under this key. The
    /// base push's leg skips its own save when it finds one
    /// (burrow_keep::prune), or deletes it once saved, so it's what there
    /// is to restore: a superset of every build of the base's that the
    /// newer push didn't change (#1164).
    Newer(String),
    /// Its leg is running on a runner, inside its deadline: it ends, saved
    /// or not, without anything this leg holds.
    Building,
    /// Its leg has run past its deadline, longer than even a cold build of
    /// a large package takes: it has stopped making progress, and a leg its
    /// job cuts off keeps nothing, so it's no better a bet than building
    /// cold now (#1102).
    Late,
    /// Its leg hasn't got a runner yet, and may be waiting for this one.
    Queued,
    /// No run of it is going, or its leg ended without saving.
    Gone,
}

/// Poll until the producer's leg stops building. The waiter has no cap of
/// its own, which would only add a wait cut off part way to a cold build
/// (#929): the deadline is the producer leg's, from when it started
/// (`WAIT_DEADLINE`), so a leg past it is never waited for, however long
/// this one has waited or not (#1102).
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
    /// The leg's kept entries, as the base push's own keep steps read them.
    leg: Leg<'a>,
    /// The leg's job name, as the workflow names it; a caller's reusable
    /// workflow puts its own job's name and " / " before it.
    job: &'a str,
    /// How long after it started the producer's leg is still worth waiting
    /// for.
    deadline: SignedDuration,
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
    /// This leg's jobs in one page of a run's jobs.
    fn legs<'v>(&self, jobs: &'v Value) -> impl Iterator<Item = &'v Value> {
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
    }
    /// What the leg's kept entries hold for this leg: the base push's own
    /// save, else a newer push's that supersedes it.
    fn kept(&self, api: &mut dyn Api) -> Result<Option<State>> {
        let entries = self.leg.entries(api)?;
        if entries.contains_key(self.tip) {
            return Ok(Some(State::Saved));
        }
        Ok(self.leg.newer(api, &entries)?.map(State::Newer))
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
    /// One poll at `now`: one request a run while its leg builds, the
    /// leg's kept entries too when `look` (every `WAIT_NEWER`), and always
    /// once it doesn't, which keeps an hours-long wait well inside the
    /// token's hourly requests.
    fn state(&self, api: &mut dyn Api, runs: &[u64], now: Timestamp, look: bool) -> Result<State> {
        let mut statuses = Vec::new();
        let (mut building, mut late) = (false, false);
        for &id in runs {
            let mut found = false;
            for page in pages(api, &self.jobs_path(id)).map_err(|e| e.to_string())? {
                for leg in self.legs(&page) {
                    found = true;
                    let status = leg["status"].as_str().unwrap_or("");
                    if status == "in_progress" {
                        let started: Timestamp = leg["started_at"]
                            .as_str()
                            .unwrap_or("")
                            .parse()
                            .map_err(|e| {
                            format!("its leg's start, {}: {e}", leg["started_at"])
                        })?;
                        if now.duration_since(started) < self.deadline {
                            building = true;
                        } else {
                            late = true;
                        }
                    }
                    statuses.push(status.to_owned());
                }
            }
            // A run whose leg isn't there yet (its lane job still running)
            // has that leg still to queue.
            if !found {
                statuses.push("queued".to_owned());
            }
        }
        if building {
            // Its save is skipped once a newer push's is there, so that
            // is waited for no longer.
            if look && let Some(State::Newer(key)) = self.kept(api)? {
                return Ok(State::Newer(key));
            }
            return Ok(State::Building);
        }
        // Read the cache AFTER status, including on the first poll. A leg
        // that finished during planning or between polls has already saved
        // everything it can save, even if its final conclusion was failure.
        Ok(if let Some(kept) = self.kept(api)? {
            kept
        } else if late {
            State::Late
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
    let deadline = SignedDuration::from_secs(var("WAIT_DEADLINE").parse().unwrap_or(DEADLINE));
    let prefix = var("KEPT_KEY");
    let producer = Producer {
        repo: &repo,
        workflow: &workflow,
        branch: &branch,
        tip: &tip,
        leg: Leg {
            repo: &repo,
            branch: &branch,
            prefix: &prefix,
            sha: &tip,
        },
        job: &job,
        deadline,
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
    let newer = Duration::from_secs(var("WAIT_NEWER").parse().unwrap_or(NEWER));
    let mut looked: Option<Instant> = None;
    let state = poll(
        Duration::from_secs(var("WAIT_POLL").parse().unwrap_or(60)),
        || {
            api.until = Some(Instant::now() + request);
            let look = looked.is_none_or(|t| t.elapsed() >= newer);
            if look {
                looked = Some(Instant::now());
            }
            producer.state(&mut api, &runs, Timestamp::now(), look)
        },
    )?;
    let late = format!(
        "its leg has run past its deadline ({}s from its start) without saving, so it has stopped making progress: building cold",
        deadline.as_secs()
    );
    println!(
        "{state:?} after {}s: {}",
        started.elapsed().as_secs(),
        match &state {
            State::Saved => "restoring its kept builds",
            State::Newer(_) =>
                "a newer push's kept builds are saved, so its own won't be: restoring those",
            State::Queued => "its leg has no runner yet, and may want this one, so building cold",
            State::Late => &late,
            State::Gone =>
                "no run of it is going, or its leg ended without saving, so building cold",
            State::Building => unreachable!("poll returns once it stops building"),
        }
    );
    let restore = match state {
        State::Saved => Some(key.as_str()),
        State::Newer(ref newer) => Some(newer.as_str()),
        _ => None,
    };
    if let Some(restore) = restore
        && let Some(path) = env::var_os("GITHUB_OUTPUT")
    {
        let mut out = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)?;
        writeln!(out, "key={restore}\nsaved=true")?;
    }
    Ok(())
}
fn main() {
    if let Err(e) = wait() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
