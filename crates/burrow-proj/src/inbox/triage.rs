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
//! state, a folder an issue holding the job's id and the issue's
//! `updated_at` when it started. A job that ends with no answer at all (its
//! engine refused it, say) leaves `failed` there instead: the issue is
//! sorted again once it changes, or once another issue's verdict shows
//! sorting works again, and nothing is said on it meanwhile.

use std::fs;
use std::path::PathBuf;

use serde_json::Value;

use super::{Pass, dirs, key, raw, say, trimmed};
use crate::cmd::{New, start_job, stop, told};
use crate::config::Triage;
use crate::github::every;
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

Answer with the verdict alone on its first line, then a short paragraph on why:

- `ready`: a clear task an agent can finish now and prove with the repo's own checks, with nothing in it only the owner can decide. It will be handed to an agent as it stands, so the issue has to say enough.
- `owner`: it waits on a call only the owner can make (money, something that can't be undone, the product's direction, their own accounts). Say exactly what the call is: they're asked it.
- `later`: real, but too big, vague, or waiting on other work to hand an agent now. Say what it waits on.
- `tracker`: it tracks other work (a checklist, a milestone, a dashboard) rather than being work itself.
- `duplicate #M`: it asks for the same as open issue #M in this repo.
- `done`: the default branch already does what it asks, or what it reports is no longer true. Say where you saw that.";

impl Pass<'_> {
    fn sorting(&self) -> Option<&Triage> {
        self.inbox.triage.as_ref()
    }

    fn sorting_dir(&self) -> PathBuf {
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
    pub(super) fn sorted(&self, t: &Triage, issue: &Value) -> bool {
        if issue.get("pull_request").is_some()
            || !self.allowed(&raw(&issue["user"]["login"]))
            || self.outside(issue)
        {
            return true;
        }
        let labels: Vec<&str> = issue["labels"]
            .as_array()
            .map(|ls| ls.iter().filter_map(|l| l["name"].as_str()).collect())
            .unwrap_or_default();
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
        let Some(t) = self.sorting().cloned() else {
            return Ok(());
        };
        let root = self.sorting_dir();
        fs::create_dir_all(&root).map_err(|e| io_fail(&root, e))?;
        self.verdicts(repo, &t, all)?;
        let going = dirs(&root, "")
            .iter()
            .filter(|d| d.join("job").exists())
            .count();
        let mut room = t.at_once.saturating_sub(going);
        for issue in all {
            if room == 0 {
                break;
            }
            if self.sorted(&t, issue) {
                continue;
            }
            let n = raw(&issue["number"]);
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
            let id = match self.sort(repo, issue) {
                Ok(id) => id,
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
            room -= 1;
        }
        Ok(())
    }

    /// A sorting job for the issue: its id.
    fn sort(&self, repo: &str, issue: &Value) -> Result<String, Fail> {
        let n = raw(&issue["number"]);
        let comments =
            every(self.cfg, &format!("repos/{repo}/issues/{n}/comments")).ok_or_else(|| {
                Fail::new(
                    1,
                    format!("proj-inbox: couldn't read {repo}#{n}'s comments"),
                )
            })?;
        let labels: Vec<String> = issue["labels"]
            .as_array()
            .map(|ls| ls.iter().map(|l| raw(&l["name"])).collect())
            .unwrap_or_default();
        let labels = if labels.is_empty() {
            "none".to_owned()
        } else {
            labels.join(", ")
        };
        let task = format!(
            "{}\n\nIts labels: {labels}.\n\n{SORT}",
            self.issue_text(repo, issue, &comments)
        );
        let app = self
            .inbox
            .machinery
            .clone()
            .filter(|m| !m.is_empty() && !self.inbox.vm)
            .filter(|_| raw(&issue["user"]["login"]).ends_with("[bot]"));
        let new = New {
            vm: self.inbox.vm,
            owned: true,
            kicks: vec!["proj-inbox".into()],
            link: Some(raw(&issue["html_url"])).filter(|l| !l.is_empty() && l != "null"),
            app,
            role: Some("triage".into()),
            ..New::default()
        };
        start_job(self.cfg, &new, &self.project[repo], &task)
    }

    /// Each of the repo's sorting jobs that has ended, carried out: its
    /// verdict, then the job stopped and its record gone. One whose issue was
    /// closed or sorted meanwhile is only stopped.
    pub(super) fn verdicts(&mut self, repo: &str, t: &Triage, all: &[Value]) -> Result<(), Fail> {
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
            if matches!(job.state().as_str(), "queued" | "running") {
                continue;
            }
            let open = all.iter().find(|i| raw(&i["number"]) == n);
            let mut failed = false;
            if open.is_some_and(|i| !self.sorted(t, i)) {
                match told(self.cfg, &id, "1") {
                    Ok(answer) => {
                        if !self.carry_out(repo, n, t, &id, &answer) {
                            self.miss(repo);
                            continue;
                        }
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
        Ok(())
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

    /// A sorting job's answer, carried out on its issue: whether GitHub took
    /// all of it. An answer without a verdict is `later`, saying so, so the
    /// issue isn't sorted over and over.
    fn carry_out(&self, repo: &str, n: &str, t: &Triage, id: &str, answer: &str) -> bool {
        let answer = answer.to_owned();
        match verdict(&answer) {
            Some(Verdict::Ready) => self.add_label(repo, n, READY),
            Some(Verdict::Later) => self.add_label(repo, n, LATER),
            Some(Verdict::Tracker) => self.add_label(repo, n, TRACKER),
            Some(Verdict::Owner) => {
                self.add_label(repo, n, &t.owner_label)
                    && t.owner.as_deref().is_none_or(|o| self.assign(repo, n, o))
                    && self.post(
                        repo,
                        n,
                        &format!("Sorted: this waits on your call.\n\n{}", why(&answer)),
                    )
            }
            Some(Verdict::Duplicate(m)) => {
                self.post(
                    repo,
                    n,
                    &format!(
                        "Sorted: a duplicate of #{m}, so closed. Reopen it if it isn't.\n\n{}",
                        why(&answer)
                    ),
                ) && self.close(repo, n, "not_planned")
            }
            Some(Verdict::Done) => {
                self.post(
                    repo,
                    n,
                    &format!(
                        "Sorted: already done, so closed. Reopen it if it isn't.\n\n{}",
                        why(&answer)
                    ),
                ) && self.close(repo, n, "completed")
            }
            None => {
                self.add_label(repo, n, LATER)
                    && self.post(
                        repo,
                        n,
                        &format!(
                            "Couldn't sort this one, so it's `{LATER}`: its sorting job (`{id}` on {}) gave no verdict.",
                            self.inbox.host
                        ),
                    )
            }
        }
    }
}
