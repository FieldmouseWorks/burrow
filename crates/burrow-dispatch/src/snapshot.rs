//! One look at what every PR waits on together: each runner, the jobs queued
//! for it and whether it's at one; each watched repo's commits the running
//! system wasn't built from; and HOST-update and HOST-switch.

use std::collections::BTreeMap;
use std::fs;

use serde_json::Value;

use crate::config::{Config, Runner};
use crate::world::{Unit, World};

/// The labels every self-hosted runner has besides its own.
const IMPLICIT: [&str; 3] = ["self-hosted", "linux", "x64"];

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub switching: bool,
    /// Each runner, by unit.
    pub runners: BTreeMap<String, Seen>,
    /// Each watched repo with a running commit to compare, by OWNER/REPO.
    pub deploys: BTreeMap<String, Behind>,
    /// HOST-update, and what its last run said.
    pub update: Option<(Unit, String)>,
    /// The repos GitHub couldn't be read for, and why: what's known of them
    /// is what the last pass saw.
    pub unread: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seen {
    pub repo: String,
    pub unit: Unit,
    /// The jobs queued that it could take, oldest first.
    pub queued: Vec<Job>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Job {
    pub name: String,
    pub url: String,
    /// When it was queued.
    pub at: f64,
}

/// A watched repo's default branch, past the commit the system runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Behind {
    pub branch: String,
    pub running: String,
    /// How many commits it has that the running system doesn't, the oldest
    /// one's time and the newest one's id: none when it's live.
    pub count: u64,
    pub oldest: f64,
    pub newest: String,
}

pub fn take(cfg: &Config, world: &dyn World) -> Snapshot {
    let mut snap = Snapshot {
        switching: world.switching(),
        ..Snapshot::default()
    };
    let mut queues: BTreeMap<&str, Vec<Queued>> = BTreeMap::new();
    for r in cfg.runners.values() {
        if !queues.contains_key(r.repo.as_str()) && !snap.unread.contains_key(&r.repo) {
            match queued(world, &r.repo) {
                Ok(q) => {
                    queues.insert(&r.repo, q);
                }
                Err(e) => {
                    snap.unread.insert(r.repo.clone(), e);
                }
            }
        }
    }
    for (unit, r) in &cfg.runners {
        let Some(q) = queues.get(r.repo.as_str()) else {
            continue;
        };
        let mut takes: Vec<Job> = q
            .iter()
            .filter(|j| fits(&j.labels, r))
            .map(|j| j.job.clone())
            .collect();
        takes.sort_by(|a, b| a.at.total_cmp(&b.at));
        snap.runners.insert(
            unit.clone(),
            Seen {
                repo: r.repo.clone(),
                unit: world.unit(unit),
                queued: takes,
            },
        );
    }
    if let Some(d) = &cfg.deploy {
        let said = fs::read(&d.status)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|v| {
                let s = |k| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
                format!("{} at {}: {}", s("result"), s("time"), s("message"))
            })
            .unwrap_or_else(|| "nothing yet".to_owned());
        snap.update = Some((world.unit(&d.unit), said));
        for (repo, running) in &d.running {
            let Some(running) = running else { continue };
            match behind(world, repo, running) {
                Ok(b) => {
                    snap.deploys.insert(repo.clone(), b);
                }
                Err(e) => {
                    snap.unread.insert(repo.clone(), e);
                }
            }
        }
    }
    snap
}

/// A job queued in a repo, and the labels it asks for.
struct Queued {
    job: Job,
    labels: Vec<String>,
}

/// Every job queued in the repo, in its runs that are queued or under way
/// (a run's later jobs queue once the ones they need are done).
fn queued(world: &dyn World, repo: &str) -> Result<Vec<Queued>, String> {
    let mut out = Vec::new();
    for status in ["queued", "in_progress"] {
        let runs = world.github(&format!(
            "repos/{repo}/actions/runs?status={status}&per_page=100"
        ))?;
        for run in list(&runs, "workflow_runs") {
            let id = run
                .get("id")
                .and_then(Value::as_u64)
                .ok_or("a run without an id")?;
            let jobs = world.github(&format!(
                "repos/{repo}/actions/runs/{id}/jobs?filter=latest&per_page=100"
            ))?;
            for job in list(&jobs, "jobs") {
                if job.get("status").and_then(Value::as_str) != Some("queued") {
                    continue;
                }
                let text = |k| job.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
                out.push(Queued {
                    job: Job {
                        name: text("name"),
                        url: text("html_url"),
                        at: time(job.get("created_at")).ok_or("a job without its time")?,
                    },
                    labels: job
                        .get("labels")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_lowercase)
                                .collect()
                        })
                        .unwrap_or_default(),
                });
            }
        }
    }
    Ok(out)
}

/// Whether the runner can take a job asking for these labels: every one of
/// them is its (GitHub's labels don't care about case). A job asking for
/// none runs nowhere.
fn fits(labels: &[String], r: &Runner) -> bool {
    !labels.is_empty()
        && labels.iter().all(|l| {
            IMPLICIT.contains(&l.as_str()) || r.labels.iter().any(|m| m.eq_ignore_ascii_case(l))
        })
}

/// What the repo's default branch has that `running` doesn't.
fn behind(world: &dyn World, repo: &str, running: &str) -> Result<Behind, String> {
    let info = world.github(&format!("repos/{repo}"))?;
    let branch = info
        .get("default_branch")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{repo} has no default branch"))?
        .to_owned();
    let cmp = world.github(&format!("repos/{repo}/compare/{running}...{branch}"))?;
    let count = cmp.get("ahead_by").and_then(Value::as_u64).unwrap_or(0);
    let commits = list(&cmp, "commits");
    let date = |c: &Value| time(c.pointer("/commit/committer/date"));
    let mut b = Behind {
        branch,
        running: running.to_owned(),
        ..Behind::default()
    };
    if count > 0 {
        // Oldest first; GitHub lists at most 250.
        let (Some(first), Some(last)) = (commits.first(), commits.last()) else {
            return Err(format!("{repo}: {count} commits ahead, and none listed"));
        };
        b.count = count;
        b.oldest = date(first).ok_or("a commit without its time")?;
        b.newest = last
            .get("sha")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
    }
    Ok(b)
}

fn list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// One of GitHub's times, in seconds since the epoch.
fn time(v: Option<&Value>) -> Option<f64> {
    let t: jiff::Timestamp = v?.as_str()?.parse().ok()?;
    Some(t.as_second() as f64)
}

#[cfg(test)]
pub mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::{Job, take};
    use crate::config::Config;
    use crate::world::{Unit, World};

    /// A stand-in for the machine: GitHub's answers by path, the units, and
    /// what was done.
    #[derive(Default)]
    pub struct Fake {
        pub github: BTreeMap<String, Result<Value, String>>,
        pub units: BTreeMap<String, Unit>,
        pub switching: bool,
        pub did: RefCell<Vec<String>>,
        pub fails: Option<String>,
    }

    impl World for Fake {
        fn github(&self, path: &str) -> Result<Value, String> {
            self.github
                .get(path)
                .cloned()
                .unwrap_or_else(|| Err(format!("GET {path}: HTTP 404")))
        }
        fn unit(&self, unit: &str) -> Unit {
            self.units.get(unit).cloned().unwrap_or_default()
        }
        fn switching(&self) -> bool {
            self.switching
        }
        fn restart(&self, unit: &str, verb: &str) -> Result<(), String> {
            self.did.borrow_mut().push(format!("{verb} {unit}"));
            self.fails.clone().map_or(Ok(()), Err)
        }
        fn kick(&self, path: &str) {
            self.did.borrow_mut().push(format!("kick {path}"));
        }
    }

    pub fn config(dir: &std::path::Path) -> Config {
        serde_json::from_value(json!({
            "stateDir": dir,
            "kick": "http://127.0.0.1:9/kick",
            "watchdog": "watchdog",
            "handler": "dispatch",
            "runners": {
                "ci": {"repo": "sam/r", "labels": ["sam"]},
                "light": {"repo": "sam/r", "labels": ["Light"]},
                "other": {"repo": "sam/o", "labels": ["o"]},
            },
            "quiet": 300.0,
            "deploy": {
                "unit": "box-update", "handler": "update", "status": dir.join("status.json"),
                "running": {"sam/box": "aaa", "sam/kit": "bbb", "sam/dirty": null},
                "after": 1800.0, "run": 5400.0,
            },
            "switch": {"state": dir.join("switch"), "revert": "box-revert"},
        }))
        .unwrap()
    }

    pub fn runs(ids: &[u64]) -> Result<Value, String> {
        Ok(json!({"workflow_runs": ids.iter().map(|id| json!({"id": id})).collect::<Vec<_>>()}))
    }

    pub fn job(name: &str, status: &str, at: &str, labels: &[&str]) -> Value {
        json!({"name": name, "status": status, "created_at": at, "labels": labels,
               "html_url": format!("https://github.com/sam/r/actions/runs/1/job/{name}")})
    }

    // Each runner gets the queued jobs its labels take, oldest first, from
    // runs that are queued or under way; one GitHub can't be read for is
    // left out, with why.
    #[test]
    fn runners() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let mut w = Fake::default();
        w.github.insert(
            "repos/sam/r/actions/runs?status=queued&per_page=100".into(),
            runs(&[1]),
        );
        w.github.insert(
            "repos/sam/r/actions/runs?status=in_progress&per_page=100".into(),
            runs(&[2]),
        );
        w.github.insert(
            "repos/sam/r/actions/runs/1/jobs?filter=latest&per_page=100".into(),
            Ok(json!({"jobs": [
                job("gate", "queued", "2026-10-29T17:20:00Z", &["self-hosted", "sam"]),
                job("lint", "queued", "2026-10-29T17:00:00Z", &["self-hosted", "light"]),
                job("hosted", "queued", "2026-10-29T17:00:00Z", &["ubuntu-latest"]),
                job("nowhere", "queued", "2026-10-29T17:00:00Z", &[]),
            ]})),
        );
        w.github.insert(
            "repos/sam/r/actions/runs/2/jobs?filter=latest&per_page=100".into(),
            Ok(json!({"jobs": [
                job("build", "in_progress", "2026-10-29T16:00:00Z", &["self-hosted", "sam"]),
                job("test", "queued", "2026-10-29T17:10:00Z", &["Self-Hosted", "Linux", "SAM"]),
            ]})),
        );
        w.units.insert(
            "ci".into(),
            Unit {
                state: "active".into(),
                since: Some(1.0),
                busy: false,
            },
        );
        let snap = take(&cfg, &w);
        let names = |u: &str| {
            snap.runners[u]
                .queued
                .iter()
                .map(|j| j.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("ci"), ["test", "gate"]);
        assert_eq!(names("light"), ["lint"]);
        assert_eq!(snap.runners["ci"].unit.state, "active");
        assert_eq!(
            snap.runners["ci"].queued[0],
            Job {
                name: "test".into(),
                url: "https://github.com/sam/r/actions/runs/1/job/test".into(),
                at: 1_793_293_800.0,
            }
        );
        assert!(!snap.runners.contains_key("other"));
        assert_eq!(
            snap.unread["sam/o"],
            "GET repos/sam/o/actions/runs?status=queued&per_page=100: HTTP 404"
        );
    }

    // A watched repo's commits past the running one: how many, the oldest's
    // time and the newest; none when it's live. One built from no commit
    // isn't compared.
    #[test]
    fn deploys() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        std::fs::write(
            dir.path().join("status.json"),
            r#"{"time": "2026-10-29T15:52:00+00:00", "result": "ok", "message": "deployed", "running": "/nix/store/x"}"#,
        )
        .unwrap();
        let mut w = Fake::default();
        for r in ["box", "kit"] {
            w.github.insert(
                format!("repos/sam/{r}"),
                Ok(json!({"default_branch": "main"})),
            );
        }
        let commit =
            |sha: &str, at: &str| json!({"sha": sha, "commit": {"committer": {"date": at}}});
        w.github.insert(
            "repos/sam/box/compare/aaa...main".into(),
            Ok(json!({"ahead_by": 3, "commits": [
                commit("c1", "2026-10-29T16:01:00Z"), commit("c2", "2026-10-29T16:07:00Z"),
                commit("c3", "2026-10-29T16:27:00Z")]})),
        );
        w.github.insert(
            "repos/sam/kit/compare/bbb...main".into(),
            Ok(json!({"ahead_by": 0, "commits": []})),
        );
        w.units.insert(
            "box-update".into(),
            Unit {
                state: "inactive".into(),
                ..Unit::default()
            },
        );
        let snap = take(&cfg, &w);
        let b = &snap.deploys["sam/box"];
        assert_eq!(
            (b.count, b.oldest, b.newest.as_str(), b.running.as_str()),
            (3, 1_793_289_660.0, "c3", "aaa")
        );
        assert_eq!(snap.deploys["sam/kit"].count, 0);
        assert!(!snap.deploys.contains_key("sam/dirty") && !snap.unread.contains_key("sam/dirty"));
        assert_eq!(
            snap.update.as_ref().unwrap().1,
            "ok at 2026-10-29T15:52:00+00:00: deployed"
        );
    }
}
