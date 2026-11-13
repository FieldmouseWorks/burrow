//! The live snapshot: what proj's own records say is doing what, at one
//! moment, as one typed, versioned tree. `proj state --json` prints it whole,
//! `proj status` and `proj status --line` print it as text, and anything else
//! that wants the picture (the watchdog, the probe, a dashboard) reads this
//! rather than collecting its own copy.
//!
//! - **queue**: the ready issues in the drain's order ([`crate::inbox`]'s
//!   ranking), with the priority, pipeline repo and age that put each there.
//! - **line**: every turn waiting in line for a slot ([`crate::slot`]).
//! - **slots**: every slot proj knows of: the shared line's (`claude`), the
//!   one `unstick` slot, Codex's pool, the drain's `drainJobs` and the
//!   sorting jobs' `atOnce`, and the machine's heavy slots. Each says what
//!   holds it (job, repo, issue or PR, role, engine, model, effort), since
//!   when, and how that holder stands: running, queued, parked on an await,
//!   asking a person, or ended with nothing held and still counted.
//! - **meters**: each engine and GitHub installation's latest reading,
//!   however old (its age says), and their holds.
//! - **holds**: what's configured to hold work back: paused repos, the
//!   pipeline's own repos and slots, and the drain and sorting.
//! - **jobs**: the jobs most recently changed, as `proj status` lists them.
//!
//! It never holds a secret, a prompt or an issue's text: only names, numbers,
//! states and times, and a job's own answer's first line as status shows it.
//! Its JSON schema is generated from these types and checked in beside the
//! crate (`state.schema.json`); a test fails when the two differ.

use std::fs;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::engine::Engine;
use crate::job::{Job, awaits};
use crate::pick::demoted;
use crate::turn::lines;
use crate::{Fail, io_fail, mtime, now, read, usage};

/// The shape's version: a reader refuses one it doesn't know.
pub const VERSION: u32 = 2;

/// How many recently changed jobs it lists unless asked for another number.
pub const JOBS: usize = 15;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    /// [`VERSION`].
    pub version: u32,
    /// When it was taken, in seconds since the epoch.
    pub at: i64,
    pub queue: Vec<Ready>,
    pub line: Vec<Waiting>,
    pub slots: Vec<Slot>,
    pub meters: Vec<Meter>,
    pub holds: Holds,
    pub jobs: Vec<JobRow>,
}

/// A ready issue the drain will hand on, next first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ready {
    /// OWNER/REPO.
    pub repo: String,
    pub number: u64,
    pub priority: Priority,
    /// In one of the pipeline's own repos, which go ahead of the rest.
    pub pipeline: bool,
    /// A repair child, which takes no ordinary drain slot.
    pub repair: bool,
    /// When it was opened, as GitHub says it.
    pub opened: String,
    /// Seconds since then; none when GitHub's time can't be read.
    pub age: Option<i64>,
}

/// In the drain's order: high first.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    High,
    Medium,
    Low,
}

/// A turn waiting in line for a slot, in the order the line takes them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Waiting {
    pub job: String,
    /// lead, pipeline, steward, first or normal.
    pub class: String,
    /// Its project's folder.
    pub project: String,
    /// When its place in line was made.
    pub since: i64,
    /// Seconds it has waited.
    pub wait: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SlotKind {
    /// One of the `maxJobs` the line shares.
    Claude,
    /// The one slot past `maxJobs` for jobs run --unstick.
    Unstick,
    /// One of Codex's own pool.
    Codex,
    /// One of the drain's `drainJobs`: an issue it handed on, until that
    /// issue's record goes.
    Drain,
    /// One of the inbox's `atOnce` sorting jobs.
    Sorting,
    /// One of the machine's heavy-work slots (`heavy`).
    Heavy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SlotState {
    /// Nothing holds it.
    Idle,
    /// Its holder's turn is running.
    Running,
    /// Its holder's turn waits in line for a turn slot.
    Queued,
    /// Its holder's turn has ended on `proj await`: it holds the slot while
    /// it waits for a PR or issue to change.
    Parked,
    /// Its holder's turn has ended on a question for a person.
    Asking,
    /// Its holder's turn has ended and holds nothing open, yet the slot is
    /// still counted as taken: doing nothing.
    Ended,
    /// Handed on, its job not started yet.
    Starting,
    /// Taken, by something proj has no record of.
    Busy,
    /// Its lock couldn't be looked at: neither free nor known to be taken.
    Unknown,
}

/// One slot, and what holds it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Slot {
    pub kind: SlotKind,
    /// Its name within its kind: 1, 2, ..., or unstick.
    pub name: String,
    pub state: SlotState,
    pub holder: Option<Holder>,
    /// Since when it has been in that state, in seconds since the epoch.
    pub since: Option<i64>,
    /// Seconds since then.
    pub age: Option<i64>,
}

/// What holds a slot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Holder {
    pub job: Option<String>,
    /// Its project's folder.
    pub project: Option<String>,
    /// The issue or PR it's about: a link, or OWNER/REPO#N.
    pub issue: Option<String>,
    pub role: Option<String>,
    /// lead, pipeline, steward, first, normal or unstick: what took a shared
    /// slot, as it says.
    pub class: Option<String>,
    pub engine: Option<Engine>,
    /// None: the engine's own.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// What it's parked on, each PR or issue.
    pub awaits: Vec<String>,
}

/// Engine and installation meters share the live snapshot. Engine entries
/// retain their original JSON fields; installation entries identify the App
/// and owner whose token supplied the free rate-limit reading.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Meter {
    Engine(EngineMeter),
    GitHub(GitHubMeter),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Installation {
    pub app: String,
    pub owner: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitHubMeter {
    pub installation: Installation,
    pub remaining: Option<u64>,
    pub reset: Option<u64>,
    /// Primary, secondary or GraphQL deadline, independently of REST budget.
    pub retry_at: Option<u64>,
    pub read_at: u64,
    pub age: u64,
    pub held: bool,
}

/// One engine's meter and hold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineMeter {
    pub engine: Engine,
    /// Its latest reading, none when there's none to read.
    pub reading: Option<Reading>,
    /// Work held back on it: Claude's usage hold.
    pub hold: Option<UsageHold>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Reading {
    pub five_hour: Option<Window>,
    pub weekly: Option<Window>,
    /// Paid usage that carries on past the windows.
    pub credits: bool,
    pub read_at: i64,
    /// Seconds since it was read.
    pub age: i64,
    /// Young enough to move a turn (role overflow reads only those).
    pub fresh: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Window {
    /// Percent used, 0 to 100.
    pub used: f64,
    pub resets_at: i64,
}

/// No turn of the engine starts while it holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageHold {
    /// The owner paused it (`proj usage hold`), rather than a refusal.
    pub owner: bool,
    pub since: i64,
    pub until: i64,
    /// The owner's reason, or the engine's own words when it refused.
    pub reason: String,
}

/// What's configured to hold work back, where it lives today (the config
/// modules/proj.nix writes).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Holds {
    /// Repos whose automated work is paused.
    pub paused: Vec<String>,
    pub pipeline: Pipeline,
    /// The inbox's triage; none when it's off.
    pub triage: Option<Triage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Pipeline {
    pub repos: Vec<String>,
    pub lead: Vec<String>,
    /// The shared slots only pipeline jobs in its own repos may take.
    pub slots: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Triage {
    /// The engine ready issues are handed to; none hands none on.
    pub drain: Option<Engine>,
    pub drain_jobs: usize,
    pub sorting_at_once: usize,
}

/// A job, as `proj status` lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JobRow {
    pub id: String,
    /// What `proj state` says.
    pub state: String,
    /// Its sandbox, with --vm.
    pub vm: Vec<String>,
    /// The App it acts as.
    pub apps: Vec<String>,
    /// Why it no longer goes as pipeline work.
    pub demoted: Option<String>,
    /// What a running turn's command waits in line for.
    pub waiting_in_line: Vec<String>,
    pub awaits: Vec<String>,
    /// How its latest turn said it ends.
    pub ends: Vec<String>,
    /// Its latest answer's first 100 bytes, one line.
    pub answer: String,
}

impl Snapshot {
    /// Redact fields separately before rendering a structured view as text.
    pub(crate) fn redacted(self) -> Result<Self, Fail> {
        let value = serde_json::to_value(self).map_err(|e| Fail::new(1, e.to_string()))?;
        let value = burrow_probe::scrub::independent_json(&value).map_err(|e| Fail::new(1, e))?;
        serde_json::from_value(value).map_err(|e| Fail::new(1, e.to_string()))
    }
    /// Now, with the `jobs` most recently changed.
    pub fn take(cfg: &Config, jobs: usize) -> Result<Snapshot, Fail> {
        let at = now();
        let inbox = crate::inbox::view(cfg, at);
        let mut slots = crate::slot::view(cfg, at);
        slots.extend(inbox.slots);
        Ok(Snapshot {
            version: VERSION,
            at,
            queue: inbox.queue,
            line: crate::slot::line(cfg),
            slots,
            meters: meters(cfg, at),
            holds: holds(cfg),
            jobs: recent(cfg, jobs)?,
        })
    }

    /// `proj status`: Claude's hold, jobs, drain occupancy and the next five ready.
    pub fn status(&self) -> String {
        let mut out = String::new();
        if let Some(h) = self
            .meters
            .iter()
            .filter_map(|m| match m {
                Meter::Engine(m) => Some(m),
                Meter::GitHub(_) => None,
            })
            .find(|m| m.engine == Engine::Claude)
            .and_then(|m| m.hold.as_ref())
        {
            out += &usage::hold_line(h.owner, h.until, &h.reason);
            out.push('\n');
        }
        for j in &self.jobs {
            out += &j.line();
        }
        if let Some(t) = self.holds.triage.as_ref().filter(|t| t.drain.is_some()) {
            let used: Vec<_> = self
                .slots
                .iter()
                .filter(|s| s.kind == SlotKind::Drain && s.state != SlotState::Idle)
                .collect();
            out += &format!("drain: {}/{} used\n", used.len(), t.drain_jobs);
            for slot in used {
                if let Some(h) = &slot.holder {
                    let issue = h.issue.as_deref().unwrap_or("unknown");
                    if let Some(job) = &h.job {
                        out += &format!(
                            "  {issue} ({job}: {})\n",
                            match slot.state {
                                SlotState::Running => "running",
                                SlotState::Queued => "queued",
                                _ => "starting",
                            }
                        );
                    } else {
                        out += &format!("  {issue} (enrolling)\n");
                    }
                }
            }
        }
        for (i, r) in self.queue.iter().take(5).enumerate() {
            out += &format!("ready {}: {}#{}\n", i + 1, r.repo, r.number);
        }
        out
    }

    /// `proj status --line`: every turn in line, next first.
    pub fn in_line(&self) -> String {
        self.line
            .iter()
            .map(|w| {
                format!(
                    "{:<40} {:<8} {:>4}m{:02}s  {}\n",
                    w.job,
                    w.class,
                    w.wait / 60,
                    w.wait % 60,
                    w.project
                )
            })
            .collect()
    }

    /// The schema its JSON follows, as the checked-in file has it.
    pub fn schema() -> String {
        let schema = schemars::schema_for!(Snapshot);
        format!("{}\n", serde_json::to_string_pretty(&schema).unwrap())
    }
}

impl JobRow {
    /// Its line in `proj status`.
    pub fn line(&self) -> String {
        let tagged = |items: &[String], f: &dyn Fn(&str) -> String| {
            items.iter().map(|i| f(i)).collect::<Vec<_>>().join("\n")
        };
        let mut notes = String::new();
        notes += &tagged(&self.vm, &|l| format!("[vm {l}] "));
        notes += &tagged(&self.apps, &|l| format!("[as {l}] "));
        if let Some(why) = &self.demoted {
            notes += &format!("[{why}] ");
        }
        notes += &tagged(&self.waiting_in_line, &|w| {
            format!("[waiting in line: {w}] ")
        });
        if !self.awaits.is_empty() {
            notes += &format!("[awaits {}] ", self.awaits.join(" "));
        }
        let ends: Vec<String> = self
            .ends
            .iter()
            .filter_map(|l| match l.as_str() {
                "ask" => Some("[asks] ".to_owned()),
                "undeclared" => Some("[no next step] ".to_owned()),
                _ => None,
            })
            .collect();
        notes += &ends.join("\n");
        notes += &self.answer;
        format!("{:<40} {:<8} {notes}\n", self.id, self.state)
    }
}

/// The `limit` jobs most recently changed, latest first.
fn recent(cfg: &Config, limit: usize) -> Result<Vec<JobRow>, Fail> {
    fs::create_dir_all(&cfg.jobs).map_err(|e| io_fail(&cfg.jobs, e))?;
    let mut jobs: Vec<(std::time::SystemTime, String)> = fs::read_dir(&cfg.jobs)
        .map_err(|e| io_fail(&cfg.jobs, e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| {
            (
                e.metadata().and_then(|m| m.modified()),
                e.file_name().to_string_lossy().into_owned(),
            )
        })
        .filter(|(_, id)| !id.starts_with('.'))
        .map(|(t, id)| (t.unwrap_or(std::time::UNIX_EPOCH), id))
        .collect();
    jobs.sort_by(|a, b| b.cmp(a));
    Ok(jobs
        .into_iter()
        .take(limit)
        .map(|(_, id)| row(&Job::new(cfg, &id)))
        .collect())
}

/// A job's row, from its own records alone.
pub(crate) fn row(job: &Job) -> JobRow {
    let state = job.state();
    let n = job.turns().unwrap_or_default();
    let waiting_in_line = if state == "running" {
        lines(&job.turn(&n, "wait"))
            .iter()
            .map(|l| l.split('\t').nth(1).unwrap_or(l).to_owned())
            .filter(|w| !w.is_empty())
            .collect()
    } else {
        Vec::new()
    };
    let result = crate::state_reads::bytes(&job.turn(&n, "result")).unwrap_or_default();
    // A shortened credential may lose its closing delimiter. Redact the
    // complete answer first, while it still has its logical boundaries.
    let result = burrow_probe::scrub::Redactor::default().plain(&String::from_utf8_lossy(&result));
    let result = result.as_bytes();
    let mut cut = result.len().min(100);
    while cut > 0 && std::str::from_utf8(&result[..cut]).is_err() {
        cut -= 1;
    }
    JobRow {
        id: job.id.to_owned(),
        vm: lines(&job.path("agent")),
        apps: lines(&job.path("app")),
        demoted: demoted(job.cfg, &job.dir),
        waiting_in_line,
        awaits: awaits(&read(&job.path("await")).unwrap_or_default())
            .into_iter()
            .map(|a| a.target)
            .collect(),
        ends: lines(&job.turn(&n, "end")),
        answer: String::from_utf8_lossy(&result[..cut]).replace('\n', " "),
        state,
    }
}

/// A slot of `kind` that job `id` holds, as its records say it stands; `issue`
/// for one whose job has no link of its own.
pub(crate) fn job_slot(
    cfg: &Config,
    kind: SlotKind,
    name: String,
    id: &str,
    issue: Option<String>,
    at: i64,
) -> Slot {
    let job = Job::new(cfg, id);
    let Some(facts) = job.facts() else {
        // Made, its first turn not written yet.
        return Slot {
            holder: Some(Holder {
                job: Some(id.to_owned()),
                issue,
                ..Holder::default()
            }),
            ..held(kind, name, SlotState::Starting, mtime(&job.dir), at)
        };
    };
    let asked = job
        .holds()
        .unwrap_or_default()
        .iter()
        .any(|h| h == "ask" || h == "owner");
    let since_await = facts
        .awaits
        .iter()
        .filter_map(|(_, s)| s.parse::<i64>().ok())
        .min();
    let (state, since) = match facts.state.as_str() {
        "running" => (SlotState::Running, Some(facts.slot)),
        "queued" => (SlotState::Queued, Some(facts.queued)),
        _ if asked => (SlotState::Asking, Some(facts.ended)),
        _ if !facts.awaits.is_empty() => (SlotState::Parked, since_await.or(Some(facts.ended))),
        _ => (SlotState::Ended, Some(facts.ended)),
    };
    let n = &facts.turn;
    Slot {
        holder: Some(Holder {
            job: Some(id.to_owned()),
            project: read(&job.path("project")).filter(|p| !p.is_empty()),
            issue: facts.link.clone().or(issue),
            role: read(&job.path("role")).filter(|r| !r.is_empty()),
            class: None,
            engine: read(&job.turn(n, "engine")).and_then(|e| Engine::parse(&e).ok()),
            model: read(&job.turn(n, "model")).filter(|m| !m.is_empty()),
            effort: read(&job.turn(n, "effort")).filter(|e| !e.is_empty()),
            awaits: facts.awaits.iter().map(|(t, _)| t.clone()).collect(),
        }),
        ..held(kind, name, state, since.filter(|s| *s > 0), at)
    }
}

/// A slot in a state since a time, its holder to fill in.
pub(crate) fn held(
    kind: SlotKind,
    name: String,
    state: SlotState,
    since: Option<i64>,
    at: i64,
) -> Slot {
    Slot {
        kind,
        name,
        state,
        holder: None,
        since,
        age: since.map(|s| (at - s).max(0)),
    }
}

/// Latest engine and installation readings and their holds.
fn meters(cfg: &Config, at: i64) -> Vec<Meter> {
    use burrow_usage_reset::meter::{FRESH, Snapshot as Read, Span};
    [Engine::Claude, Engine::Codex]
        .into_iter()
        .map(|engine| {
            let reading = cfg
                .meters
                .as_deref()
                .and_then(|dir| Read::load(dir, engine.name()))
                .map(|r| {
                    let window = |span: Span| {
                        r.windows
                            .iter()
                            .find(|(name, _)| Span::of(name) == Some(span))
                            .map(|(_, w)| Window {
                                used: w.used,
                                resets_at: w.resets_at as i64,
                            })
                    };
                    let age = at as f64 - r.read_at;
                    Reading {
                        five_hour: window(Span::FiveHour),
                        weekly: window(Span::Week),
                        credits: r.credits,
                        read_at: r.read_at as i64,
                        age: age.max(0.0) as i64,
                        fresh: (0.0..FRESH).contains(&age),
                    }
                });
            let hold = (engine == Engine::Claude)
                .then(|| usage::out(cfg))
                .flatten()
                .map(|s| UsageHold {
                    owner: s.owner,
                    since: s.at,
                    until: s.until,
                    reason: s.said,
                });
            Meter::Engine(EngineMeter {
                engine,
                reading,
                hold,
            })
        })
        .chain(crate::github_quota::meters(cfg).into_iter().map(|m| {
            let held = m.until(at.max(0) as u64).is_some();
            Meter::GitHub(GitHubMeter {
                installation: Installation {
                    app: m.app,
                    owner: m.owner,
                },
                remaining: m.remaining,
                reset: m.reset,
                retry_at: m.retry_at,
                read_at: m.at,
                age: (at.max(0) as u64).saturating_sub(m.at),
                held,
            })
        }))
        .collect()
}

fn holds(cfg: &Config) -> Holds {
    Holds {
        paused: cfg.paused_repos(),
        pipeline: Pipeline {
            repos: cfg.pipeline_repos.clone(),
            lead: cfg.lead_repos.clone(),
            slots: cfg.pipeline_slots,
        },
        triage: cfg
            .inbox
            .as_ref()
            .and_then(|i| i.triage.as_ref())
            .map(|t| Triage {
                drain: t.drain,
                drain_jobs: t.drain_jobs,
                sorting_at_once: t.at_once,
            }),
    }
}

#[cfg(test)]
pub(crate) mod tests;
