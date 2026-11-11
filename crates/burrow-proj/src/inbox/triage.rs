//! Triage: every open issue nobody has sorted is sorted by a job of its own,
//! which reads the issue and the repo and changes nothing. Its first line is
//! a verdict, which the pass then carries out:
//!
//!   - `ready`: labelled [`READY`], for the drain ([`super::drain`]) to hand
//!     to an idle slot;
//!   - `later`: labelled [`LATER`], real but not one to hand on yet;
//!   - `tracker`: labelled [`TRACKER`], a list of other work, never handed on;
//!   - `owner`: labelled with the owner label and assigned to the owner, with
//!     the call they're asked for;
//!   - `duplicate #M` or `done`: closed, with why.
//!
//! Sorted means it has one of those labels, or an engine's (someone already
//! handed it on), or one other machinery owns (`skip`: a probe, an undo, a
//! flaky test's issue). Only an issue one of the inbox's authors opened is
//! sorted, since the job reads it with the machine's logins: anyone else's is
//! text from a stranger, and so is one with the outside label, whoever filed
//! it (a game server filing its players' reports with an author's token,
//! say). A pull request is never an issue here.
//!
//! The jobs are `--owned`: the pass acts on their end, which kicks a full
//! pass (proj-inbox with no repo), so a backlog in a quiet repo is worked
//! through too, `at_once` at a time across every repo. Each waits its turn in
//! line behind the work someone asked for, and runs on the engine the
//! `triage` role names. Their records are `.triage/KEY-N` in the inbox's
//! state, a folder an issue holding the job's id, the issue's `updated_at`
//! when it started and what the job read of it (`input`: a verdict on an
//! issue whose text or authors' comments changed since is dropped, and the
//! issue sorted again), then the verdict's steps (`plan`) and how many of
//! them GitHub has taken (`did`). A job whose issue an agent may no longer
//! read (labelled outside text, or its opener no longer an author) is
//! stopped wherever it is, queued or running. A job that ends with no answer at all (its
//! engine refused it, say) leaves `failed` there instead: the issue is
//! sorted again once it changes, or once another issue's verdict shows
//! sorting works again, and nothing is said on it meanwhile.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Pass, dirs, key, raw, say, trimmed};
use crate::cmd::{New, start_job, stop, told};
use crate::config::Triage;
use crate::job::Job;
use crate::{Fail, io_fail, read};

/// An issue an agent can take and finish now.
pub(super) const READY: &str = "ready";
/// Real, but not one to hand on yet.
pub(super) const LATER: &str = "later";
/// A list of other work rather than work itself.
pub(super) const TRACKER: &str = "tracker";

/// The most of a sorting job's reason that goes on an issue.
const WHY_CHARS: usize = 4000;

/// What a sorting job decided.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Ready,
    Later,
    Tracker,
    Owner,
    Duplicate(u64),
    Done,
    Split,
}

/// The verdict on an answer's first line with words on it, as the task asks
/// for one: backticks, bold and a `verdict:` before it are taken off, and
/// case doesn't matter. None when it isn't one of them.
pub(super) fn verdict(answer: &str) -> Option<Verdict> {
    let line = answer.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.replace(['`', '*'], "").to_lowercase();
    let line = line.trim();
    let line = line.strip_prefix("verdict:").unwrap_or(line).trim();
    let mut words = line.split_whitespace();
    let word = words.next()?.trim_end_matches(['.', ':', ',']);
    Some(match word {
        "ready" => Verdict::Ready,
        "later" => Verdict::Later,
        "tracker" => Verdict::Tracker,
        "owner" => Verdict::Owner,
        "done" => Verdict::Done,
        "split" => Verdict::Split,
        "duplicate" => {
            let of = words.next()?.trim_start_matches('#');
            let of = of.trim_end_matches(|c: char| !c.is_ascii_digit());
            Verdict::Duplicate(of.parse().ok().filter(|m| *m > 0)?)
        }
        _ => return None,
    })
}

/// The answer after its verdict's line: why, as the issue is told it.
pub(super) fn why(answer: &str) -> String {
    let mut lines = answer.lines().skip_while(|l| l.trim().is_empty());
    lines.next();
    let rest = trimmed(lines.collect::<Vec<_>>().join("\n").trim());
    let mut rest: String = rest.chars().take(WHY_CHARS).collect();
    if rest.is_empty() {
        rest = "(No reason given.)".into();
    }
    rest
}

/// What a sorting job is asked: the issue, as a job sees an inbox issue, and
/// what each verdict means.
const SORT: &str = "Sort this issue for the machine's queue of open issues. Read the issue and the repo as much as you need to, but change nothing: no commits, branches, PRs, comments, labels or issues. The pass that started you acts on your answer.

Answer with the verdict followed by a priority (high, medium or low) on its first line, then a short paragraph on why. Examples: `ready high`, `duplicate #123 low`. Use high for serious correctness, safety or pipeline blockers, medium for ordinary tasks, low for minor improvements. Existing priority labels are preserved:

- `ready`: a clear task an agent can finish now and prove with the repo's own checks, with nothing in it only the owner can decide. It will be handed to an agent as it stands, so the issue has to say enough.
- `owner`: it waits on a call only the owner can make: spending money, a step that can't be undone, their own accounts or person, or a call the repo's own rules (its AGENTS.md) leave to them. Say exactly what the call is: they're asked it. A technical, design or stack choice is not one of these, even when the issue says it's the owner's: the agent that takes the issue makes it by the repo's rules, so on its own it doesn't keep an issue from `ready`.
- `later`: real, but too big, vague, or waiting on other work to hand an agent now. Say what it waits on.
- `tracker`: it tracks other work (a checklist, a milestone, a dashboard) rather than being work itself.
- `duplicate #M`: it asks for the same as open issue #M in this repo.
- `done`: the default branch already does what it asks, or what it reports is no longer true. Say where you saw that.";

impl Pass<'_> {
    fn sorting(&self) -> Option<&Triage> {
        self.inbox.triage.as_ref()
    }

    pub(super) fn sorting_dir(&self) -> PathBuf {
        self.inbox.state.join(".triage")
    }

    /// The labels triage puts on, as each repo is given them: name, colour,
    /// what it means.
    pub(super) fn triage_labels(&self) -> Vec<(String, &'static str, &'static str)> {
        let Some(t) = self.sorting() else {
            return Vec::new();
        };
        vec![
            (
                READY.into(),
                "0e8a16",
                "Sorted: an agent can take it now (proj-inbox's drain hands it on)",
            ),
            (
                LATER.into(),
                "c5def5",
                "Sorted: real, but not one to hand on yet",
            ),
            (TRACKER.into(), "bfd4f2", "Sorted: tracks other work"),
            (
                t.owner_label.clone(),
                "d93f0b",
                "Waits for the owner's call",
            ),
        ]
    }

    /// Whether an issue is sorted already, or isn't triage's to sort.
    pub(super) fn sorted(
        &mut self,
        repo: &str,
        t: &Triage,
        issue: &Value,
        claimed: &BTreeSet<String>,
    ) -> bool {
        if issue.get("pull_request").is_some()
            || !self.eligible(repo, issue)
            || claimed.contains(&raw(&issue["number"]))
        {
            return true;
        }
        let labels: Vec<&str> = issue["labels"]
            .as_array()
            .map(|ls| ls.iter().filter_map(|l| l["name"].as_str()).collect())
            .unwrap_or_default();
        if !t.sort_unlabelled
            && self.engine(issue).is_none()
            && !labels.contains(&READY)
            && !labels.contains(&TRACKER)
        {
            return true;
        }
        if labels
            .iter()
            .any(|l| *l == t.owner_label || t.skip.iter().any(|s| s == l))
        {
            return true;
        }
        if self.inbox.planning && self.started_issue_job(Some(repo), &raw(&issue["number"])) {
            return true;
        }
        if self.inbox.planning && self.worker_split_pending(Some(repo), &raw(&issue["number"])) {
            return true;
        }
        if self.inbox.planning && self.has_task_list(Some(repo), &raw(&issue["number"])) {
            return !self
                .sorting_dir()
                .join(format!("{}-{}", key(repo), raw(&issue["number"])))
                .join("plan")
                .exists()
                && self.verified_source(repo, issue).unwrap_or(false);
        }
        if self.inbox.planning
            && (labels.contains(&TRACKER)
                || (!labels.contains(&LATER)
                    && (labels.contains(&READY) || self.engine(issue).is_some())))
        {
            return self.planned(Some(repo), issue);
        }
        labels.iter().any(|l| {
            [READY, LATER, TRACKER, t.owner_label.as_str()].contains(l)
                || self.inbox.labels.contains_key(*l)
                || *l == self.inbox.unstick_label
                || t.skip.iter().any(|s| s == l)
        })
    }

    /// One repo's open issues, every one: each sorting job that has ended
    /// carried out, then a job for each issue nobody has sorted, while there's
    /// room.
    pub(super) fn triage(&mut self, repo: &str, all: &[Value]) -> Result<(), Fail> {
        if self.cfg.paused(repo) {
            return Ok(());
        }
        let Some(t) = self.sorting().cloned() else {
            return Ok(());
        };
        let root = self.sorting_dir();
        fs::create_dir_all(&root).map_err(|e| io_fail(&root, e))?;
        let handled = self.verdicts(repo, &t, all)?;
        let claimed = self.claimed(repo);
        let going = dirs(&root, "")
            .iter()
            .filter(|d| self.sorting_slot(d))
            .count();
        let mut room = t.at_once.saturating_sub(going);
        for issue in all {
            if room == 0 && !self.lane(issue) {
                continue;
            }
            let n = raw(&issue["number"]);
            if self.sorted(repo, &t, issue, &claimed) || handled.contains(&n) {
                continue;
            }
            let d = root.join(format!("{}-{n}", key(repo)));
            let updated = raw(&issue["updated_at"]);
            // Its last sorting ended without an answer: sorted again once
            // the issue changes, or once another's verdict shows sorting
            // works again.
            if let Some(failed) = read(&d.join("failed")) {
                if failed == updated {
                    continue;
                }
                let _ = fs::remove_dir_all(&d);
            }
            if d.exists() {
                continue;
            }
            let (id, input, seen) = match self.sort(repo, issue) {
                Ok(started) => started,
                Err(f) => {
                    say(f);
                    self.retry(repo);
                    break;
                }
            };
            fs::create_dir_all(&d).map_err(|e| io_fail(&d, e))?;
            for (f, v) in [("updated", &updated), ("job", &id)] {
                fs::write(d.join(f), format!("{v}\n")).map_err(|e| io_fail(&d.join(f), e))?;
            }
            Self::hand(&d, &input)?;
            fs::write(d.join("seen"), seen.to_string()).map_err(|e| io_fail(&d, e))?;
            room = room.saturating_sub(1);
        }
        Ok(())
    }

    /// What a sorting job reads of the issue: its text and its authors'
    /// comments, as they stand. None when GitHub didn't say.
    pub(super) fn input(&self, repo: &str, issue: &Value) -> Option<String> {
        let super::written::Written {
            issue, comments, ..
        } = self.written(repo, issue)?;
        Some(self.issue_text(repo, &issue, &comments))
    }

    /// A sorting job for the issue: its id, and what it was given to read
    /// ([`Pass::input`]).
    fn sort(&self, repo: &str, issue: &Value) -> Result<(String, String, i64), Fail> {
        let n = raw(&issue["number"]);
        let written = self.written(repo, issue).ok_or_else(|| {
            Fail::new(
                1,
                format!("proj-inbox: couldn't read {repo}#{n}'s comments"),
            )
        })?;
        let input = self.issue_text(repo, &written.issue, &written.comments);
        let seen = written
            .comments
            .iter()
            .filter_map(|c| c["id"].as_i64())
            .max()
            .unwrap_or(0);
        let labels: Vec<String> = issue["labels"]
            .as_array()
            .map(|ls| ls.iter().map(|l| raw(&l["name"])).collect())
            .unwrap_or_default();
        let labels = if labels.is_empty() {
            "none".to_owned()
        } else {
            labels.join(", ")
        };
        let instruction = if self.inbox.planning {
            crate::planning::INSTRUCTIONS
        } else {
            SORT
        };
        let given = if self.inbox.planning {
            crate::planning::prompt(&input, Path::new("@PROJ_TASK_SOURCE@"))
        } else {
            input.clone()
        };
        let task = format!("{given}\n\nIts labels: {labels}.\n\n{instruction}");
        let app = self
            .cfg
            .machinery
            .clone()
            .filter(|m| !m.is_empty() && !self.inbox.vm)
            .filter(|_| raw(&issue["user"]["login"]).ends_with("[bot]"));
        let new = New {
            unstick: self.lane(issue),
            task_source: (self.inbox.planning && input.len() > crate::planning::MAX_INPUT)
                .then(|| input.clone()),
            usage_kind: Some(super::usage::kind(self.cfg.steward_login.as_deref(), issue)),
            vm: self.inbox.vm && !self.inbox.planning,
            owned: true,
            kicks: vec!["proj-inbox".into()],
            link: Some(raw(&issue["html_url"])).filter(|l| !l.is_empty() && l != "null"),
            app,
            role: Some(
                if self.inbox.planning {
                    "planner"
                } else {
                    "triage"
                }
                .into(),
            ),
            ..New::default()
        };
        start_job(self.cfg, &new, &self.project[repo], &task).map(|id| (id, input, seen))
    }

    /// Each of the repo's sorting jobs that has ended, carried out: its
    /// verdict, then the job stopped and its record gone. One whose issue was
    /// closed or sorted meanwhile is only stopped, and one whose issue
    /// changed while it ran (an edit, a comment) is stopped and its issue
    /// sorted again as it stands. The issues whose verdicts were carried
    /// out, which the rest of this pass leaves be: its list of them is from
    /// before.
    pub(super) fn verdicts(
        &mut self,
        repo: &str,
        t: &Triage,
        all: &[Value],
    ) -> Result<BTreeSet<String>, Fail> {
        let mut handled = BTreeSet::new();
        let claimed = self.claimed(repo);
        let prefix = format!("{}-", key(repo));
        for d in dirs(&self.sorting_dir(), &prefix) {
            let name = d
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Some(n) = name
                .strip_prefix(&prefix)
                .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
            else {
                continue;
            };
            let Some(id) = read(&d.join("job")).filter(|j| !j.is_empty()) else {
                if !d.join("failed").exists() {
                    let _ = fs::remove_dir_all(&d);
                }
                continue;
            };
            let job = Job::new(self.cfg, &id);
            if !job.dir.is_dir() {
                let _ = fs::remove_dir_all(&d);
                continue;
            }
            let open = all.iter().find(|i| raw(&i["number"]) == n);
            let busy = matches!(job.state().as_str(), "queued" | "running");
            // An issue no agent may read now: its job goes, queued or
            // running, before it reads any more of it. One closed meanwhile
            // is asked about, while its job is still to finish.
            let barred = if open.is_some() || busy {
                let Some(barred) = self.barred(repo, n, open) else {
                    self.miss(repo);
                    continue;
                };
                barred
            } else {
                false
            };
            if !barred && self.sorting_question(repo, n, &d, &job, open, busy)? {
                handled.insert(n.to_owned());
                continue;
            }
            if !barred && busy {
                continue;
            }
            // A resolved one-way question's owner label was ours, not an
            // independent sorting verdict. Clear it and ignore a stale list
            // snapshot of that label when consuming the resumed verdict.
            let answered =
                read(&d.join("question-notice")).is_some_and(|n| n.ends_with("\tone-way"));
            let resolved = open.cloned().map(|mut issue| {
                if answered && let Some(labels) = issue["labels"].as_array_mut() {
                    labels.retain(|l| {
                        l["name"].as_str() != Some(self.cfg.needs_owner_label.as_str())
                    });
                }
                issue
            });
            let open = resolved.as_ref();
            if answered && let Err(e) = crate::ask::visibility(self.cfg, &job) {
                eprintln!("proj sorting question: {e}");
                self.retry(repo);
                continue;
            }
            // The issue as the job read it, its text and its authors'
            // comments (the inbox's own aren't among them): changed since,
            // even partway through carrying its verdict out, the verdict is
            // of something else, and the issue is sorted again as it stands.
            let mut stale = false;
            if !barred && let Some(i) = open.filter(|i| !self.sorted(repo, t, i, &claimed)) {
                let Some(now) = self.input(repo, i) else {
                    self.miss(repo);
                    continue;
                };
                stale = !Self::handed(&d, &now);
            }
            let mut failed = false;
            if !barred && !stale && open.is_some_and(|i| !self.sorted(repo, t, i, &claimed)) {
                match told(self.cfg, &id, &job.turns().unwrap_or_else(|| "1".into())) {
                    Ok(answer) => {
                        let Some(plan) = self.plan(&d, t, &id, &answer) else {
                            self.retry(repo);
                            continue;
                        };
                        if !self.carry_out(repo, n, &d, &plan, &job) {
                            self.miss(repo);
                            continue;
                        }
                        handled.insert(n.to_owned());
                        if verdict(&answer).is_some() {
                            self.sorting_works();
                        }
                    }
                    // No answer at all (the engine refused it, say): nothing
                    // said on the issue, and it isn't sorted again until it
                    // changes or sorting is seen to work.
                    Err(_) => failed = true,
                }
            }
            if let Err(f) = stop(self.cfg, &id, false) {
                say(f);
                self.retry(repo);
                continue;
            }
            if failed {
                let updated = read(&d.join("updated")).unwrap_or_default();
                let _ = fs::write(d.join("failed"), format!("{updated}\n"));
                let _ = fs::remove_file(d.join("job"));
            } else {
                let _ = fs::remove_dir_all(&d);
            }
        }
        Ok(handled)
    }

    /// A verdict came back, so sorting works: every issue whose sorting
    /// ended without an answer gets another go.
    fn sorting_works(&self) {
        for d in dirs(&self.sorting_dir(), "") {
            if d.join("failed").exists() && !d.join("job").exists() {
                let _ = fs::remove_dir_all(&d);
            }
        }
    }

    /// What a sorting job's answer comes to on its issue, in order. A label
    /// that sorts the issue always goes last, so the issue reads as unsorted
    /// until every step before it is done. An answer without a verdict is
    /// `later`, saying so, so the issue isn't sorted over and over.
    pub(super) fn steps(&self, t: &Triage, id: &str, answer: &str) -> Vec<Step> {
        match verdict(answer) {
            Some(Verdict::Split) => {
                match crate::planning::parse(answer.split_once('\n').map_or("", |(_, rest)| rest)) {
                    Ok(tasks) => vec![Step::Split(tasks), Step::Label(TRACKER.into())],
                    Err(e) => vec![
                        Step::Post(format!(
                            "Invalid split: {e}. No children created; needs replanning."
                        )),
                        Step::Label(LATER.into()),
                    ],
                }
            }
            Some(Verdict::Ready) => vec![Step::Label(READY.into())],
            Some(Verdict::Later) => vec![Step::Label(LATER.into())],
            Some(Verdict::Tracker) => vec![Step::Label(TRACKER.into())],
            Some(Verdict::Owner) => t
                .owner
                .iter()
                .map(|o| Step::Assign(o.clone()))
                .chain([
                    Step::Post(format!(
                        "Sorted: this waits on your call.\n\n{}",
                        why(answer)
                    )),
                    Step::Label(t.owner_label.clone()),
                ])
                .collect(),
            Some(Verdict::Duplicate(m)) => vec![
                Step::Post(format!(
                    "Sorted: a duplicate of #{m}, so closed. Reopen it if it isn't.\n\n{}",
                    why(answer)
                )),
                Step::Close("not_planned".into()),
            ],
            Some(Verdict::Done) => vec![
                Step::Post(format!(
                    "Sorted: already done, so closed. Reopen it if it isn't.\n\n{}",
                    why(answer)
                )),
                Step::Close("completed".into()),
            ],
            None => vec![
                Step::Post(format!(
                    "Couldn't sort this one, so it's `{LATER}`: its sorting job (`{id}` on {}) gave no verdict.",
                    self.inbox.host
                )),
                Step::Label(LATER.into()),
            ],
        }
    }

    /// The verdict's steps, as its record keeps them: worked out once, the
    /// first time it's carried out, so a pass that comes back to it finishes
    /// the same steps whatever the settings say by then. None when the
    /// record can't keep them.
    fn plan(&self, d: &Path, t: &Triage, id: &str, answer: &str) -> Option<Vec<Step>> {
        let kept = d.join("plan");
        if let Some(plan) = fs::read(&kept)
            .ok()
            .and_then(|p| serde_json::from_slice(&p).ok())
        {
            return Some(plan);
        }
        let mut plan = self.steps(t, id, answer);
        let priority = answer
            .lines()
            .find(|l| !l.trim().is_empty())
            .and_then(|l| l.split_whitespace().last())
            .map(|s| s.trim_matches(['`', '*', '.', ',']).to_lowercase())
            .filter(|s| matches!(s.as_str(), "high" | "medium" | "low"))
            .unwrap_or_else(|| "medium".into());
        plan.insert(0, Step::Priority(format!("priority: {priority}")));
        fs::write(&kept, serde_json::to_vec(&plan).ok()?).ok()?;
        Some(plan)
    }

    /// A verdict's steps carried out on the issue, from the first its record
    /// (`did`, in `d`) doesn't have done: whether GitHub took all of them.
    /// Each one GitHub takes is counted there, so a pass that comes back to
    /// it carries on from the next and says nothing twice.
    fn carry_out(&self, repo: &str, n: &str, d: &Path, steps: &[Step], job: &Job) -> bool {
        let did: usize = read(&d.join("did"))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        for (i, step) in steps.iter().enumerate().skip(did) {
            let took = match step {
                Step::Priority(l) => {
                    let Some(issue) =
                        crate::github::api(self.cfg, &format!("repos/{repo}/issues/{n}"))
                    else {
                        return false;
                    };
                    super::drain::priority_label(&issue).is_some() || self.add_label(repo, n, l)
                }
                Step::Label(l) => self.add_label(repo, n, l),
                Step::Assign(o) => self.assign(repo, n, o),
                Step::Post(body) => {
                    let engine = job.engine_label(&job.turns().unwrap_or_default());
                    self.post(repo, n, &format!("**{engine} job**\n\n{body}"))
                }
                Step::Close(reason) => self.close(repo, n, reason),
                Step::Split(tasks) => {
                    let Ok(input) = fs::read_to_string(d.join("input")) else {
                        return false;
                    };
                    self.prepare_plan(repo, n, &input, tasks) && self.split_children(repo, n, tasks)
                }
            };
            if !took || fs::write(d.join("did"), format!("{}\n", i + 1)).is_err() {
                return false;
            }
        }
        if self.inbox.planning
            && steps
                .iter()
                .any(|s| matches!(s, Step::Label(l) if l == READY))
        {
            let Ok(input) = fs::read_to_string(d.join("input")) else {
                return false;
            };
            if !self.prepare_plan(repo, n, &input, &[]) {
                return false;
            }
        }
        true
    }
}

/// One thing a verdict does on its issue.
#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(super) enum Step {
    Priority(String),
    Label(String),
    Assign(String),
    Post(String),
    Close(String),
    Split(Vec<crate::planning::Task>),
}
