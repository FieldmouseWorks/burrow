//! One look at what every PR waits on together: each runner, the jobs queued
//! for it and whether it's at one; the fresh runners and the jobs queued for
//! them; each runner repo's push runs on its default branch that haven't
//! finished; each watched repo's commits the running system wasn't built
//! from; and HOST-update and HOST-switch.

use std::collections::BTreeMap;
use std::fs;

use serde_json::Value;

use crate::config::{Config, Fresh, Runner};
use crate::rules::short;
use crate::world::{Unit, World};

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub switching: bool,
    /// Each runner, by unit.
    pub runners: BTreeMap<String, Seen>,
    /// The fresh runners, when the machine has them.
    pub fresh: Option<Pool>,
    /// Each runner repo's push runs on its default branch that haven't
    /// finished, by OWNER/REPO, oldest first.
    pub pushes: BTreeMap<String, Vec<Run>>,
    /// Each watched repo with a running commit to compare, by OWNER/REPO.
    pub deploys: BTreeMap<String, Behind>,
    /// The flake's own repo when HOST-update hasn't found its main live in
    /// the system that runs, and why: what's live is unknown until a run of
    /// HOST-update finds it.
    pub unknown: BTreeMap<String, String>,
    /// HOST-update, and what its last run said.
    pub update: Option<(Unit, String)>,
    /// HOST-update's own open PR, when its last run said it's waiting on
    /// that to land before it brings in anything newer.
    pub behind: Option<u64>,
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

/// The runners started fresh for one job each, and the jobs queued for
/// them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pool {
    /// Each one's unit, by its instance.
    pub units: BTreeMap<String, Unit>,
    /// The jobs queued that ask for their label, oldest first, in the repos
    /// that could be read.
    pub queued: Vec<Job>,
}

/// A queued job, by GitHub's own link to it: its name is the workflow's,
/// which anyone whose PR runs a workflow writes, so it's never passed on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Job {
    pub url: String,
    /// When it was queued.
    pub at: f64,
}

/// A push run on a repo's default branch that hasn't finished.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    pub id: u64,
    pub url: String,
    pub workflow: u64,
    pub branch: String,
    /// The commit it checks.
    pub sha: String,
    /// GitHub's: queued (no job started yet), in_progress, or pending (next
    /// in its concurrency group).
    pub status: String,
    /// Its attempt: past the first, someone asked for it again.
    pub attempt: u64,
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
    let fresh_repos = cfg.fresh.iter().flat_map(|f| &f.repos);
    for repo in cfg.runners.values().map(|r| &r.repo).chain(fresh_repos) {
        if !queues.contains_key(repo.as_str()) && !snap.unread.contains_key(repo) {
            match look(world, repo) {
                Ok((q, pushes)) => {
                    queues.insert(repo, q);
                    snap.pushes.insert(repo.clone(), pushes);
                }
                Err(e) => {
                    snap.unread.insert(repo.clone(), e);
                }
            }
        }
    }
    if let Some(f) = &cfg.fresh {
        snap.fresh = Some(pool(world, f, &queues));
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
        let status = fs::read(&d.status)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        snap.behind = status
            .as_ref()
            .and_then(|v| v.get("behind"))
            .and_then(Value::as_u64);
        let said = status
            .map(|v| {
                let s = |k| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
                format!("{} at {}: {}", s("result"), s("time"), s("message"))
            })
            .unwrap_or_else(|| "nothing yet".to_owned());
        snap.update = Some((world.unit(&d.unit), said));
        let mut compare: Vec<(&String, String)> = Vec::new();
        // Its own repo, at how far HOST-update found main live in the system
        // that runs; a record or a system that can't be read is a repo it
        // can't read.
        if let Some(own) = &d.own {
            match live(&d.live, &d.unit, world) {
                Ok(Live::At(main)) => compare.push((own, main)),
                Ok(Live::Unknown(why)) => {
                    snap.unknown.insert(own.clone(), why);
                }
                Err(e) => {
                    snap.unread.insert(own.clone(), e);
                }
            }
        }
        compare.extend(d.running.iter().map(|(r, c)| (r, c.clone())));
        for (repo, running) in compare {
            match behind(world, repo, &running) {
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

/// The fresh runners' units, and the jobs queued in their repos that ask
/// for their label and nothing else.
fn pool(world: &dyn World, f: &Fresh, queues: &BTreeMap<&str, Vec<Queued>>) -> Pool {
    let mut queued: Vec<Job> = f
        .repos
        .iter()
        .filter_map(|r| queues.get(r.as_str()))
        .flatten()
        .filter(|j| {
            !j.labels.is_empty() && j.labels.iter().all(|l| l.eq_ignore_ascii_case(&f.label))
        })
        .map(|j| j.job.clone())
        .collect();
    queued.sort_by(|a, b| a.at.total_cmp(&b.at));
    let units = world
        .instances(&f.template)
        .into_iter()
        .map(|i| {
            let unit = world.unit(&format!("{}{i}.service", f.template));
            (i, unit)
        })
        .collect();
    Pool { units, queued }
}

/// A job queued in a repo, and the labels it asks for.
struct Queued {
    job: Job,
    labels: Vec<String>,
}

/// Every job queued in the repo, in its runs that are queued or under way
/// (a run's later jobs queue once the ones they need are done); and its push
/// runs on its default branch that haven't finished, oldest first.
fn look(world: &dyn World, repo: &str) -> Result<(Vec<Queued>, Vec<Run>), String> {
    let branch = default_branch(world, repo)?;
    let mut out = Vec::new();
    let mut pushes: Vec<Run> = Vec::new();
    let mut read: Vec<u64> = Vec::new();
    for status in ["queued", "in_progress", "pending"] {
        let runs = every(
            world,
            &format!("repos/{repo}/actions/runs?status={status}"),
            "workflow_runs",
        )?;
        for run in &runs {
            let id = run
                .get("id")
                .and_then(Value::as_u64)
                .ok_or("a run without an id")?;
            let text = |k| run.get(k).and_then(Value::as_str).unwrap_or("");
            if text("event") == "push" && text("head_branch") == branch {
                match pushes.iter_mut().find(|p| p.id == id) {
                    // One that moved on between the lists is listed once,
                    // and one seen under way in either has started.
                    Some(p) => {
                        if status == "in_progress" {
                            p.status = text("status").to_owned();
                        }
                    }
                    None => pushes.push(Run {
                        id,
                        url: text("html_url").to_owned(),
                        workflow: run.get("workflow_id").and_then(Value::as_u64).unwrap_or(0),
                        branch: branch.clone(),
                        sha: text("head_sha").to_owned(),
                        status: text("status").to_owned(),
                        attempt: run.get("run_attempt").and_then(Value::as_u64).unwrap_or(1),
                        at: time(run.get("created_at")).ok_or("a run without its time")?,
                    }),
                }
            }
            // A pending run has no jobs yet, and one listed twice has had
            // them read.
            if status == "pending" || read.contains(&id) {
                continue;
            }
            read.push(id);
            let jobs = every(
                world,
                &format!("repos/{repo}/actions/runs/{id}/jobs?filter=latest"),
                "jobs",
            )?;
            for job in &jobs {
                if job.get("status").and_then(Value::as_str) != Some("queued") {
                    continue;
                }
                out.push(Queued {
                    job: Job {
                        url: job
                            .get("html_url")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
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
    pushes.sort_by(|a, b| a.at.total_cmp(&b.at));
    Ok((out, pushes))
}

/// Whether the runner can take a job asking for these labels: every one of
/// them is its (GitHub's labels don't care about case). A job asking for
/// none runs nowhere.
fn fits(labels: &[String], r: &Runner) -> bool {
    !labels.is_empty()
        && labels
            .iter()
            .all(|l| r.labels.iter().any(|m| m.eq_ignore_ascii_case(l)))
}

/// Every entry of one of GitHub's paged lists (`key` in each page), all of
/// it or an error: a page that can't be read fails the whole list, so what
/// it would have held is never taken for nothing.
fn every(world: &dyn World, path: &str, key: &str) -> Result<Vec<Value>, String> {
    const PER: usize = 100;
    let mut out = Vec::new();
    for page in 1.. {
        let got = world.github(&format!("{path}&per_page={PER}&page={page}"))?;
        let entries = list(&got, key);
        out.extend_from_slice(entries);
        let total = got.get("total_count").and_then(Value::as_u64);
        // A short page, or its total reached, is the end.
        if entries.len() < PER || total.is_some_and(|t| out.len() as u64 >= t) {
            break;
        }
    }
    Ok(out)
}

fn default_branch(world: &dyn World, repo: &str) -> Result<String, String> {
    world
        .github(&format!("repos/{repo}"))?
        .get("default_branch")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{repo} has no default branch"))
}

/// How far HOST-update found its own repo's main live.
enum Live {
    /// Up to this commit, in the system that runs.
    At(String),
    /// Not for the system that runs, and why: no run has recorded it yet,
    /// or the system was switched since, by anyone.
    Unknown(String),
}

/// What HOST-update's (`unit`'s) record at `path` says of the system that
/// runs. A record or a system that can't be read is an error, so a wait
/// isn't dropped on a failed read.
fn live(path: &std::path::Path, unit: &str, world: &dyn World) -> Result<Live, String> {
    let text = match fs::read(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Live::Unknown(format!(
                "{unit} hasn't recorded main live in any system yet"
            )));
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let v: Value = serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let field = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("{}: no {k}", path.display()))
    };
    let (main, recorded) = (field("main")?, field("running")?);
    let running = world.running()?;
    Ok(if recorded == running {
        Live::At(main.to_owned())
    } else {
        Live::Unknown(format!(
            "{unit} hasn't found main live in the system that runs ({running}) since it was switched to; it found it live up to {} in {recorded}",
            short(main)
        ))
    })
}

/// What the repo's default branch has that `running` doesn't.
fn behind(world: &dyn World, repo: &str, running: &str) -> Result<Behind, String> {
    let branch = default_branch(world, repo)?;
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

    use super::{Job, Run, take};
    use crate::config::Config;
    use crate::world::{Unit, World};

    /// A stand-in for the machine: GitHub's answers by path, the units (and
    /// what each has become by its second look, when that's set), and what
    /// was done. With `peek`, each move also notes that file as it was when
    /// the move was made.
    #[derive(Default)]
    pub struct Fake {
        pub github: BTreeMap<String, Result<Value, String>>,
        pub units: BTreeMap<String, Unit>,
        pub then: BTreeMap<String, Unit>,
        pub looks: RefCell<BTreeMap<String, usize>>,
        pub running: Option<String>,
        pub switching: bool,
        pub did: RefCell<Vec<String>>,
        pub fails: Option<String>,
        pub peek: Option<std::path::PathBuf>,
        /// The fresh runners' instances.
        pub instances: Vec<String>,
    }

    impl Fake {
        fn act(&self, what: String) -> Result<(), String> {
            self.did.borrow_mut().push(what);
            if let Some(p) = &self.peek {
                let kept = std::fs::read_to_string(p).unwrap_or_default();
                self.did.borrow_mut().push(kept);
            }
            self.fails.clone().map_or(Ok(()), Err)
        }
    }

    impl World for Fake {
        fn github(&self, path: &str) -> Result<Value, String> {
            self.github
                .get(path)
                .cloned()
                .unwrap_or_else(|| Err(format!("GET {path}: HTTP 404")))
        }
        fn unit(&self, unit: &str) -> Unit {
            let mut looks = self.looks.borrow_mut();
            let n = looks.entry(unit.to_owned()).or_default();
            *n += 1;
            match self.then.get(unit) {
                Some(u) if *n > 1 => u.clone(),
                _ => self.units.get(unit).cloned().unwrap_or_default(),
            }
        }
        fn running(&self) -> Result<String, String> {
            self.running
                .clone()
                .ok_or_else(|| "/run/current-system: No such file or directory".to_owned())
        }
        fn switching(&self) -> bool {
            self.switching
        }
        fn restart(&self, unit: &str, verb: &str) -> Result<(), String> {
            self.act(format!("{verb} {unit}"))
        }
        fn cancel(&self, repo: &str, run: u64) -> Result<(), String> {
            self.act(format!("cancel {repo}#{run}"))
        }
        fn kick(&self, path: &str) {
            self.did.borrow_mut().push(format!("kick {path}"));
        }
        fn instances(&self, _: &str) -> Vec<String> {
            self.instances.clone()
        }
        fn fresh(&self, f: &crate::config::Fresh, instance: &str) -> Result<(), String> {
            self.act(format!("fresh {}{instance} for {}", f.prefix, f.owner))
        }
        fn sweep(&self, f: &crate::config::Fresh) -> Vec<String> {
            self.did
                .borrow_mut()
                .push(format!("sweep {}", f.configs.display()));
            Vec::new()
        }
    }

    pub fn config(dir: &std::path::Path) -> Config {
        serde_json::from_value(json!({
            "stateDir": dir,
            "kick": "http://127.0.0.1:9/kick",
            "watchdog": "watchdog",
            "handler": "dispatch",
            "runners": {
                "ci": {"repo": "sam/r", "labels": ["self-hosted", "Linux", "X64", "sam"]},
                "light": {"repo": "sam/r", "labels": ["self-hosted", "Linux", "X64", "Light"]},
                "arm": {"repo": "sam/r", "labels": ["self-hosted", "Linux", "ARM64", "sam"]},
                "other": {"repo": "sam/o", "labels": ["self-hosted", "Linux", "X64", "o"]},
            },
            "quiet": 300.0,
            "gap": 120.0,
            "deploy": {
                "unit": "box-update", "handler": "update", "status": dir.join("status.json"),
                "own": "sam/box", "live": dir.join("live.json"),
                "running": {"sam/kit": "bbb"},
                "after": 1800.0, "run": 5400.0, "ride": 18000.0,
            },
            "switch": {"state": dir.join("switch"), "revert": "box-revert"},
        }))
        .unwrap()
    }

    /// The machine's fresh runners: for sam's sam/r and sam/f, two at most.
    pub fn fresh(dir: &std::path::Path) -> crate::config::Fresh {
        serde_json::from_value(json!({
            "owner": "sam", "group": 1, "repos": ["sam/r", "sam/f"], "label": "fresh-box",
            "prefix": "fresh-box-", "template": "fresh-runner@", "most": 2,
            "configs": dir.join("configs"),
        }))
        .unwrap()
    }

    pub fn runs(ids: &[u64]) -> Result<Value, String> {
        Ok(json!({"workflow_runs": ids.iter().map(|id| json!({"id": id})).collect::<Vec<_>>()}))
    }

    /// A push run's first attempt: its id, workflow, branch, commit, status
    /// and time.
    pub fn push(id: u64, workflow: u64, branch: &str, sha: &str, status: &str, at: &str) -> Value {
        json!({"id": id, "event": "push", "workflow_id": workflow, "head_branch": branch,
               "head_sha": sha, "status": status, "created_at": at, "run_attempt": 1,
               "html_url": format!("https://github.com/sam/r/actions/runs/{id}")})
    }

    /// GitHub's page `page` of a paged list's path, as `every` asks for it.
    pub fn page(path: &str, page: u32) -> String {
        format!("{path}&per_page=100&page={page}")
    }

    pub fn job(name: &str, status: &str, at: &str, labels: &[&str]) -> Value {
        json!({"name": name, "status": status, "created_at": at, "labels": labels,
               "html_url": format!("https://github.com/sam/r/actions/runs/1/job/{name}")})
    }

    // Each runner gets the queued jobs its labels take, its architecture's
    // among them, oldest first, from runs that are queued or under way, every
    // page of them; one GitHub can't be read for is left out, with why. Its
    // repo's push runs on its default branch that haven't finished are
    // listed, oldest first and each once, a run seen under way as started.
    #[test]
    fn runners() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let mut w = Fake::default();
        let runs = |status: &str| format!("repos/sam/r/actions/runs?status={status}");
        let jobs = |id: u64| format!("repos/sam/r/actions/runs/{id}/jobs?filter=latest");
        w.github
            .insert("repos/sam/r".into(), Ok(json!({"default_branch": "main"})));
        w.github.insert(
            page(&runs("queued"), 1),
            Ok(json!({"total_count": 3, "workflow_runs": [
                {"id": 1, "event": "pull_request", "head_branch": "main"},
                push(3, 7, "main", "c3", "queued", "2026-10-29T16:30:00Z"),
                push(4, 7, "fix", "c4", "queued", "2026-10-29T16:40:00Z"),
            ]})),
        );
        w.github.insert(
            page(&runs("in_progress"), 1),
            Ok(json!({"total_count": 2, "workflow_runs": [
                {"id": 2},
                // Started between the two lists.
                push(3, 7, "main", "c3", "in_progress", "2026-10-29T16:30:00Z"),
            ]})),
        );
        w.github.insert(
            page(&runs("pending"), 1),
            Ok(json!({"workflow_runs": [push(5, 7, "main", "c5", "pending", "2026-10-29T16:50:00Z")]})),
        );
        w.github.insert(page(&jobs(4), 1), Ok(json!({"jobs": []})));
        // Listed twice, read once.
        w.github.insert(
            page(&jobs(3), 1),
            Ok(json!({"jobs": [job("push", "queued", "2026-10-29T16:30:00Z", &["self-hosted", "sam"])]})),
        );
        w.github.insert(
            page(&jobs(1), 1),
            Ok(json!({"jobs": [
                job("gate", "queued", "2026-10-29T17:20:00Z", &["self-hosted", "X64", "sam"]),
                job("lint", "queued", "2026-10-29T17:00:00Z", &["self-hosted", "light"]),
                job("hosted", "queued", "2026-10-29T17:00:00Z", &["ubuntu-latest"]),
                job("nowhere", "queued", "2026-10-29T17:00:00Z", &[]),
            ]})),
        );
        // A hundred done, and the one queued on the next page.
        let done: Vec<Value> = (0..100)
            .map(|_| {
                job(
                    "done",
                    "completed",
                    "2026-10-29T16:00:00Z",
                    &["self-hosted", "sam"],
                )
            })
            .collect();
        w.github.insert(
            page(&jobs(2), 1),
            Ok(json!({"total_count": 101, "jobs": done})),
        );
        w.github.insert(
            page(&jobs(2), 2),
            Ok(json!({"total_count": 101, "jobs": [
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
                .map(|j| j.url.rsplit('/').next().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("ci"), ["push", "test", "gate"]);
        assert_eq!(names("arm"), ["push", "test"]);
        assert_eq!(names("light"), ["lint"]);
        assert_eq!(snap.runners["ci"].unit.state, "active");
        assert_eq!(
            snap.runners["ci"].queued[1],
            Job {
                url: "https://github.com/sam/r/actions/runs/1/job/test".into(),
                at: 1_793_293_800.0,
            }
        );
        assert!(!snap.runners.contains_key("other"));
        assert_eq!(snap.unread["sam/o"], "GET repos/sam/o: HTTP 404");
        let pushes: Vec<(u64, &str)> = snap.pushes["sam/r"]
            .iter()
            .map(|r| (r.id, r.status.as_str()))
            .collect();
        assert_eq!(pushes, [(3, "in_progress"), (5, "pending")]);
        assert_eq!(
            snap.pushes["sam/r"][1],
            Run {
                id: 5,
                url: "https://github.com/sam/r/actions/runs/5".into(),
                workflow: 7,
                branch: "main".into(),
                sha: "c5".into(),
                status: "pending".into(),
                attempt: 1,
                at: 1_793_292_600.0,
            }
        );

        // A page that can't be read: the repo isn't read, rather than read
        // short.
        w.github.remove(&page(&jobs(2), 2));
        let snap = take(&cfg, &w);
        assert!(!snap.runners.contains_key("ci"));
        assert!(snap.unread["sam/r"].contains("page=2"), "{:?}", snap.unread);
    }

    // The fresh runners' units, and the jobs queued in their repos, one
    // read only for them among them, that ask for their label (in any case)
    // and nothing else, oldest first.
    #[test]
    fn fresh_pool() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.deploy = None;
        cfg.fresh = Some(fresh(dir.path()));
        let mut w = Fake::default();
        for repo in ["r", "o", "f"] {
            w.github.insert(
                format!("repos/sam/{repo}"),
                Ok(json!({"default_branch": "main"})),
            );
            for status in ["queued", "in_progress", "pending"] {
                w.github.insert(
                    page(&format!("repos/sam/{repo}/actions/runs?status={status}"), 1),
                    runs(&[]),
                );
            }
        }
        w.github.insert(
            page("repos/sam/r/actions/runs?status=queued", 1),
            runs(&[1]),
        );
        w.github.insert(
            page("repos/sam/f/actions/runs?status=queued", 1),
            runs(&[2]),
        );
        w.github.insert(
            page("repos/sam/r/actions/runs/1/jobs?filter=latest", 1),
            Ok(json!({"jobs": [
                job("train", "queued", "2026-10-29T17:20:00Z", &["fresh-box"]),
                job("gate", "queued", "2026-10-29T17:00:00Z", &["self-hosted", "sam"]),
                job("both", "queued", "2026-10-29T17:00:00Z", &["fresh-box", "sam"]),
            ]})),
        );
        w.github.insert(
            page("repos/sam/f/actions/runs/2/jobs?filter=latest", 1),
            Ok(json!({"jobs": [job("other", "queued", "2026-10-29T17:10:00Z", &["Fresh-Box"])]})),
        );
        w.instances = vec!["17-0".into()];
        w.units.insert(
            "fresh-runner@17-0.service".into(),
            Unit {
                state: "active".into(),
                since: Some(1.0),
                busy: true,
            },
        );
        let pool = take(&cfg, &w).fresh.unwrap();
        let names: Vec<&str> = pool
            .queued
            .iter()
            .map(|j| j.url.rsplit('/').next().unwrap())
            .collect();
        assert_eq!(names, ["other", "train"]);
        assert_eq!(pool.units.keys().collect::<Vec<_>>(), ["17-0"]);
        assert!(pool.units["17-0"].busy);
        // Without them, nothing's read for them.
        cfg.fresh = None;
        assert!(take(&cfg, &w).fresh.is_none());
    }

    // A watched repo's commits past the running one: how many, the oldest's
    // time and the newest; none when it's live. The flake's own repo is
    // compared at the commit HOST-update last found main live up to; without
    // that record, or with one of a system other than the one that runs,
    // what's live is unknown; a record or a system that can't be read leaves
    // the repo unread.
    #[test]
    fn deploys() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        std::fs::write(
            dir.path().join("status.json"),
            r#"{"time": "2026-10-29T15:52:00+00:00", "result": "ok", "message": "deployed", "running": "/nix/store/x", "behind": 405}"#,
        )
        .unwrap();
        let live = dir.path().join("live.json");
        std::fs::write(&live, r#"{"main": "aaa", "running": "/nix/store/x"}"#).unwrap();
        let mut w = Fake {
            running: Some("/nix/store/x".into()),
            ..Fake::default()
        };
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
        assert_eq!(
            snap.update.as_ref().unwrap().1,
            "ok at 2026-10-29T15:52:00+00:00: deployed"
        );
        assert_eq!(snap.behind, Some(405));
        assert!(snap.unknown.is_empty());
        // A system switched to since, by anyone: unknown until HOST-update
        // looks, never taken for live.
        w.running = Some("/nix/store/y".into());
        let snap = take(&cfg, &w);
        assert!(!snap.deploys.contains_key("sam/box") && !snap.unread.contains_key("sam/box"));
        assert_eq!(
            snap.unknown["sam/box"],
            "box-update hasn't found main live in the system that runs (/nix/store/y) since it was switched to; it found it live up to aaa in /nix/store/x"
        );
        assert_eq!(snap.deploys["sam/kit"].count, 0);
        // No record yet: unknown too.
        w.running = Some("/nix/store/x".into());
        std::fs::remove_file(&live).unwrap();
        let snap = take(&cfg, &w);
        assert!(!snap.deploys.contains_key("sam/box") && !snap.unread.contains_key("sam/box"));
        assert_eq!(
            snap.unknown["sam/box"],
            "box-update hasn't recorded main live in any system yet"
        );
        // A record or a system that can't be read isn't taken for live.
        std::fs::write(&live, r#"{"main": "aaa"}"#).unwrap();
        let snap = take(&cfg, &w);
        assert!(
            snap.unread["sam/box"].ends_with("live.json: no running"),
            "{:?}",
            snap.unread
        );
        std::fs::write(&live, r#"{"main": "", "running": "/nix/store/x"}"#).unwrap();
        assert!(take(&cfg, &w).unread["sam/box"].ends_with("live.json: no main"));
        std::fs::write(&live, "{").unwrap();
        assert!(take(&cfg, &w).unread.contains_key("sam/box"));
        std::fs::remove_file(&live).unwrap();
        std::fs::create_dir(&live).unwrap();
        assert!(take(&cfg, &w).unread.contains_key("sam/box"));
        std::fs::remove_dir(&live).unwrap();
        std::fs::write(&live, r#"{"main": "aaa", "running": "/nix/store/x"}"#).unwrap();
        w.running = None;
        assert_eq!(
            take(&cfg, &w).unread["sam/box"],
            "/run/current-system: No such file or directory"
        );
    }
}
