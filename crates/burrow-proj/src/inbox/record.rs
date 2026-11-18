//! An issue's record: where it stands with the inbox, what it waits on and
//! what ends that, since when, when it's overdue, and what happens next
//! (docs/proj-design.md, "One record per item"). [`Look::record`] is the one
//! reading of what the inbox keeps of an issue (its record folder, its
//! sorting record, the sorting waits, the task lists, the ready lists, the
//! parked and retired marks) that says so, and every reader asks it: the
//! inbox's view and the snapshot's ready queue and sorting waits
//! ([`super::view`]), the pass's sorting slots and its sorting waits, the
//! drain's ranking, `proj-inbox status` and `proj why OWNER/REPO#N`. It
//! only reads: nothing in it settles, recovers or writes, so asking twice
//! gives one answer, whoever asks. An issue the inbox never enrolled has a
//! record that says so, and why.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::triage::{LATER, READY, TRACKER};
use super::{Pass, dirs, key, raw};
use crate::config::Config;
use crate::job::Job;
use crate::{mtime, read, usage};

/// Where an issue stands with the inbox.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stage {
    /// The inbox hasn't taken it up, and why.
    Unseen(Unseen),
    /// A sorting job reads it, waits to, or its verdict is being carried
    /// out.
    Sorting,
    /// It was split: its task list hands its children on one at a time.
    Planned,
    /// Sorted ready: the drain hands it on.
    Ready,
    /// Its job is at it, or its last answer is still to be posted.
    Working,
    /// Its job has answered; a reply from an author goes on with it.
    Answered,
    /// Closed.
    Closed,
}

/// Why the inbox hasn't taken an issue up.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Unseen {
    /// The inbox isn't on here.
    Off,
    /// Its repo isn't one of the projects here.
    NoProject,
    /// It wasn't opened by one of the inbox's authors: this one.
    NotAuthor(String),
    /// It carries the outside label: text from outside the authors.
    Outside(String),
    /// The inbox refused it, and said so on it: its opener isn't an author,
    /// or it carries outside text.
    Refused,
    /// It has no engine label.
    NoLabel,
    /// It carries a label that keeps it from being handed on: `later`,
    /// `tracker`, the owner's, or one other machinery owns.
    Label(String),
    /// Its repo is paused.
    Paused,
    /// Its job was stopped when its labels parked it: only an engine label
    /// put back on takes it up again.
    Parked,
    /// GitHub has refused to list its repo since then.
    Unlisted(i64),
    /// Nothing is kept of it here, and GitHub wasn't asked or didn't say.
    Unknown,
}

impl fmt::Display for Unseen {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Unseen::Off => f.write_str("the inbox isn't on here"),
            Unseen::NoProject => f.write_str("its repo isn't a project here"),
            Unseen::NotAuthor(who) => {
                write!(f, "it was opened by `{who}`, who isn't one of the inbox's authors")
            }
            Unseen::Outside(label) => write!(f, "it's labelled `{label}`, text from outside the authors"),
            Unseen::Refused => f.write_str(
                "the inbox refused it, as it said on the issue: its opener isn't one of the authors, or it carries outside text",
            ),
            Unseen::NoLabel => f.write_str("it has no engine label"),
            Unseen::Label(label) => write!(f, "it's labelled `{label}`"),
            Unseen::Paused => f.write_str("its repo is paused"),
            Unseen::Parked => {
                f.write_str("its job was stopped when its labels parked it")
            }
            Unseen::Unlisted(since) => write!(
                f,
                "GitHub has refused to list its repo since {}",
                usage::when(*since)
            ),
            Unseen::Unknown => f.write_str(
                "nothing is kept of it here, and GitHub wasn't asked about it or didn't say",
            ),
        }
    }
}

impl fmt::Display for Stage {
    /// The word or two `proj why` and `proj-inbox status` say.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Stage::Unseen(_) => "unseen",
            Stage::Sorting => "sorting",
            Stage::Planned => "planned",
            Stage::Ready => "ready",
            Stage::Working => "working",
            Stage::Answered => "answered",
            Stage::Closed => "closed",
        })
    }
}

/// What an issue waits on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Wait {
    /// One of the `atOnce` sorting slots.
    SortingSlot,
    /// Its sorting job's verdict carried out on GitHub.
    Verdict,
    /// Room among the drain's jobs.
    Drain,
    /// The plan step before it: the child, OWNER/REPO#N, that lands first.
    Plan(String),
    /// This job: its own, or the one whose brief claims it.
    Job(String),
    /// A person's answer to this job's question.
    Answer(String),
    /// Its repo's pause.
    Paused,
    /// GitHub's quota, resetting then.
    Quota(i64),
    /// The inbox's next pass that reaches GitHub.
    Pass,
}

impl Wait {
    /// What it is, in a few words.
    pub(crate) fn label(&self) -> String {
        match self {
            Wait::SortingSlot => "a sorting slot".into(),
            Wait::Verdict => "its verdict's steps on GitHub".into(),
            Wait::Drain => "room among the drain's jobs".into(),
            Wait::Plan(step) => format!("the plan step before it, {step}"),
            Wait::Job(id) => format!("job {id}"),
            Wait::Answer(id) => format!("an answer to job {id}'s question"),
            Wait::Paused => "its repo's pause".into(),
            Wait::Quota(_) => "GitHub's quota".into(),
            Wait::Pass => "the inbox's next pass".into(),
        }
    }

    /// What ends it, in words.
    pub(crate) fn ends(&self) -> String {
        match self {
            Wait::SortingSlot => "a sorting job ending, which frees its slot".into(),
            Wait::Verdict => "the next pass that reaches GitHub carrying them out".into(),
            Wait::Drain => "one of the drain's jobs ending".into(),
            Wait::Plan(step) => format!("{step} landing, or being dropped from the list"),
            Wait::Job(id) => format!("{id}'s turn ending (`proj why {id}` says what it waits on)"),
            Wait::Answer(_) => "a reply on the issue, or the question's deadline".into(),
            Wait::Paused => "the settings allowing its repo again".into(),
            Wait::Quota(reset) => format!("the quota resetting at {}", usage::when(*reset)),
            Wait::Pass => "an event on its repo, or the pass the inbox booked for it".into(),
        }
    }
}

/// An issue's record, as [`Look::record`] reads it.
#[derive(Clone, Debug)]
pub(crate) struct Record {
    /// OWNER/REPO#N.
    pub(crate) issue: String,
    pub(crate) stage: Stage,
    pub(crate) wait: Option<Wait>,
    /// When it entered this stage or wait, in seconds; 0 when nothing says.
    pub(crate) since: i64,
    /// Past this, it's a stall: its job's, when it waits on one.
    pub(crate) overdue_at: Option<i64>,
    /// What happens to it next, and what brings it about.
    pub(crate) next: String,
    /// Its job, or its sorting job: what `proj why` follows it into.
    pub(crate) job: Option<String>,
}

impl Record {
    fn new(issue: &str, stage: Stage, next: impl Into<String>) -> Record {
        Record {
            issue: issue.to_owned(),
            stage,
            wait: None,
            since: 0,
            overdue_at: None,
            next: next.into(),
            job: None,
        }
    }

    fn waiting(mut self, wait: Wait) -> Record {
        if let Wait::Quota(reset) = wait {
            self.overdue_at = Some(reset);
        }
        self.wait = Some(wait);
        self
    }

    fn since(mut self, at: Option<i64>) -> Record {
        self.since = at.unwrap_or(0);
        self
    }

    /// `proj why OWNER/REPO#N`: the record in words.
    pub(crate) fn why(&self) -> String {
        let at = |t: i64| {
            if t > 0 {
                usage::when(t)
            } else {
                "an unknown time".into()
            }
        };
        let mut out = match &self.stage {
            Stage::Unseen(why) => format!(
                "{} is unseen: the inbox hasn't taken it up, because {why}.\n",
                self.issue
            ),
            stage => format!("{} is {stage}, since {}.\n", self.issue, at(self.since)),
        };
        match &self.wait {
            Some(w) => out.push_str(&format!(
                "It waits on {}, which ends with {}.\n",
                w.label(),
                w.ends()
            )),
            None => out.push_str("It waits on nothing.\n"),
        }
        if let Some(t) = self.overdue_at {
            out.push_str(&format!("It's overdue at {}.\n", at(t)));
        }
        out.push_str(&format!("Next: {}.\n", self.next));
        out
    }

    /// `proj why OWNER/REPO#N --json`: the record whole.
    pub(crate) fn json(&self) -> Value {
        serde_json::json!({
            "issue": self.issue,
            "stage": self.stage.to_string(),
            "unseen": match &self.stage {
                Stage::Unseen(why) => Some(why.to_string()),
                _ => None,
            },
            "wait": self.wait.as_ref().map(|w| serde_json::json!({
                "wait": w.label(), "ends": w.ends(),
            })),
            "since": self.since,
            "overdue_at": self.overdue_at,
            "next": self.next,
            "job": self.job,
        })
    }
}

/// The inbox's state files, which only this module and the inbox's own
/// writers name: a reader asks the record.
pub(super) mod files {
    use std::path::{Path, PathBuf};

    /// The issue's record folder (KEY-N in the inbox's state).
    pub(in crate::inbox) fn worker(state: &Path, key: &str, n: &str) -> PathBuf {
        state.join(format!("{key}-{n}"))
    }
    /// Its sorting record.
    pub(in crate::inbox) fn sorting(state: &Path, key: &str, n: &str) -> PathBuf {
        sorting_root(state).join(format!("{key}-{n}"))
    }
    /// The sorting records.
    pub(in crate::inbox) fn sorting_root(state: &Path) -> PathBuf {
        state.join(".triage")
    }
    /// Why it waits to be sorted, while it does.
    pub(in crate::inbox) fn sorting_waits(state: &Path) -> PathBuf {
        state.join(".triage-waiting")
    }
    /// Each repo's ready issues, as its last pass listed them.
    pub(in crate::inbox) fn ready_lists(state: &Path) -> PathBuf {
        state.join(".ready")
    }
    /// Since when GitHub has refused each repo.
    pub(in crate::inbox) fn unreached(state: &Path) -> PathBuf {
        state.join(".unreached")
    }
    /// The issues whose labels parked their job.
    pub(in crate::inbox) fn parked(state: &Path, key: &str, n: &str) -> PathBuf {
        parked_root(state).join(key.to_lowercase()).join(n)
    }
    /// Every repo's parked issues.
    pub(in crate::inbox) fn parked_root(state: &Path) -> PathBuf {
        state.join(".parked")
    }
    /// The records of the issue's retired jobs.
    pub(in crate::inbox) fn retired(state: &Path, key: &str, n: &str) -> PathBuf {
        retired_root(state).join(key.to_lowercase()).join(n)
    }
    /// Every repo's retired records.
    pub(in crate::inbox) fn retired_root(state: &Path) -> PathBuf {
        state.join(".retired")
    }
    /// The drain's mark while a ready issue waits for a slot.
    pub(in crate::inbox) fn drain_waiting(state: &Path) -> PathBuf {
        state.join(".drain-waiting")
    }
}

/// One look at the inbox's state: [`Look::record`] for as many issues as
/// the caller asks about, with what's the same for all of them (the
/// drain's room, the claims of each repo's live jobs) read once.
pub(super) struct Look<'a, 'p> {
    pass: &'a Pass<'p>,
    claimed: std::cell::RefCell<BTreeMap<String, std::collections::BTreeSet<String>>>,
}

impl<'p> Pass<'p> {
    /// A look at the inbox's state, for its records.
    pub(super) fn look(&self) -> Look<'_, 'p> {
        Look {
            pass: self,
            claimed: Default::default(),
        }
    }

    /// Whether a sorting record holds one of the `at_once` slots: its job's,
    /// unless its job is settled ([`super::settle`]), its question has
    /// been left for the owner, or it has ended with no outstanding holds.
    /// The pass, proj's queue, resumption and the live snapshot all count
    /// with this one reading, and it only reads: a question an accepted
    /// answer retired reads as answered whether or not the pass's settling
    /// step has cleaned it up yet ([`crate::record::holds`]).
    pub(super) fn holds_sorting_slot(&self, d: &Path) -> bool {
        read(&d.join("job")).is_some_and(|id| {
            let job = Job::new(self.cfg, &id);
            // Serializes with new starts and question recovery. The running
            // turn itself publishes without this lock.
            let Ok(_held) = job.read_lock() else {
                return true;
            };
            if super::settle::settled(&job).is_some() {
                return false;
            }
            // Establish completion before sampling holds. Both the turn and
            // its _ended recovery can publish final holds without this lock;
            // neither may still be running or queued when we release capacity.
            let terminated = crate::systemd::terminated(self.cfg, &job.unit())
                && crate::systemd::terminated(self.cfg, &job.end_unit());
            let Ok(holds) = job.holds() else { return true };
            if holds.iter().any(|h| h == "owner") {
                return false;
            }
            if !terminated {
                return true;
            }
            // Policy-paused records keep their existing exception once their
            // units are known terminal. Retries and other holds retain capacity.
            self.record_uses_capacity(d) && !holds.is_empty()
        })
    }

    /// Each issue the inbox has a job for, and that job: its record
    /// folders' first, then its sorting records', `(sorting)` after the
    /// issue.
    pub(super) fn kept_jobs(&self) -> Vec<(String, String)> {
        let state = &self.inbox.state;
        let job = |d: &Path| read(&d.join("job")).filter(|j| !j.is_empty());
        let workers = dirs(state, "").into_iter().filter_map(|d| {
            let id = job(&d)?;
            Some((read(&d.join("issue")).unwrap_or_default(), id))
        });
        let sorting = dirs(&files::sorting_root(state), "")
            .into_iter()
            .filter_map(|d| {
                let id = job(&d)?;
                let name = d.file_name()?.to_string_lossy().into_owned();
                let issue = name
                    .rsplit_once('-')
                    .map(|(k, n)| format!("{}#{n} (sorting)", k.replacen('_', "/", 1)))
                    .unwrap_or(name);
                Some((issue, id))
            });
        workers.chain(sorting).collect()
    }

    /// The sorting records that hold a slot now, oldest job first.
    pub(super) fn sorting_holders(&self) -> Vec<PathBuf> {
        let mut held: Vec<_> = dirs(&files::sorting_root(&self.inbox.state), "")
            .into_iter()
            .filter(|d| self.holds_sorting_slot(d))
            .collect();
        held.sort_by_key(|d| (mtime(&d.join("job")).unwrap_or(0), d.clone()));
        held
    }
}

impl Look<'_, '_> {
    /// Issue N of `repo`'s record, from what the inbox keeps of it, and, when
    /// the caller has one, what GitHub says of it now (`issue`): only that
    /// can say why an issue the inbox keeps nothing of is unseen. Reads
    /// only.
    pub(super) fn record(&self, repo: &str, n: &str, issue: Option<&Value>) -> Record {
        let p = self.pass;
        let name = format!("{repo}#{n}");
        let state = &p.inbox.state;
        let k = key(repo);
        let paused = p.cfg.paused(repo);
        if !p.project.keys().any(|r| r.eq_ignore_ascii_case(repo)) {
            return Record::new(
                &name,
                Stage::Unseen(Unseen::NoProject),
                "nothing: no pass looks at its repo",
            );
        }
        // Its job's record folder: the issue is that job's.
        let worker = files::worker(state, &k, n);
        if let Some(id) = read(&worker.join("job")).filter(|j| !j.is_empty()) {
            return self.working(repo, &name, &worker, &id, issue, paused);
        }
        if worker.join("drained").exists() {
            let r = Record::new(
                &name,
                Stage::Ready,
                "the drain put its label on: its repo's next pass starts its job",
            )
            .since(mtime(&worker.join("drained")));
            return self.gated(r, repo, paused);
        }
        if worker.join("refused").exists() {
            let why = issue
                .and_then(|i| self.refused(repo, i))
                .unwrap_or(Unseen::Refused);
            return Record::new(
                &name,
                Stage::Unseen(why),
                "nothing, until one of the authors opens an issue of their own for it",
            )
            .since(mtime(&worker.join("refused")));
        }
        // Its sorting record: a sorting job is reading it, or its verdict is
        // being carried out. One whose sorting ended without an answer gives
        // way to its labels: the pass never sorts an issue an author has
        // closed or labelled `ready` again, so it keeps that record forever.
        if let Some(r) = self.sorting(repo, &name, &files::sorting(state, &k, n), paused) {
            if r.job.is_some() {
                return r;
            }
            return self
                .closed_or_ready(repo, &name, n, issue, paused)
                .unwrap_or(r);
        }
        let waiting = files::sorting_waits(state).join(format!("{}-{n}", k.to_lowercase()));
        if waiting.exists() {
            let r = Record::new(
                &name,
                Stage::Sorting,
                "a sorting job starts for it once a slot is free, at its repo's next full pass",
            )
            .since(mtime(&waiting))
            .waiting(Wait::SortingSlot);
            return self.gated(r, repo, paused);
        }
        if let Some(super::plans::Place::Parent(step)) = p.place(repo, n) {
            let r = match step {
                Some(step) => Record::new(
                    &name,
                    Stage::Planned,
                    format!("its list hands on {step}, then the child after it"),
                )
                .waiting(Wait::Plan(step)),
                None => Record::new(
                    &name,
                    Stage::Planned,
                    "its list closes it, every child having landed",
                )
                .waiting(Wait::Pass),
            };
            return self.gated(r, repo, paused);
        }
        if let Some(r) = self.closed_or_ready(repo, &name, n, issue, paused) {
            return r;
        }
        if !dirs(&files::retired(state, &k, n), "").is_empty()
            || files::parked(state, &k, n).exists()
        {
            return Record::new(
                &name,
                Stage::Unseen(Unseen::Parked),
                "nothing, until an engine label goes back on it",
            );
        }
        if let Some(i) = issue {
            return self.unseen(repo, &name, i, paused);
        }
        let unreached = files::unreached(state).join(&k);
        if let Some(since) = read(&unreached).and_then(|t| t.parse::<i64>().ok()) {
            return Record::new(
                &name,
                Stage::Unseen(Unseen::Unlisted(since)),
                "its repo's next pass that GitHub answers lists it",
            )
            .waiting(p.cfg.retry_at(repo).map_or(Wait::Pass, Wait::Quota));
        }
        if paused {
            return Record::new(
                &name,
                Stage::Unseen(Unseen::Paused),
                "nothing, until the settings allow its repo",
            )
            .waiting(Wait::Paused);
        }
        Record::new(
            &name,
            Stage::Unseen(Unseen::Unknown),
            "its repo's next pass looks at it, if it's open and labelled",
        )
    }

    /// The record of an issue whose job is `id`.
    fn working(
        &self,
        repo: &str,
        name: &str,
        d: &Path,
        id: &str,
        issue: Option<&Value>,
        paused: bool,
    ) -> Record {
        let job = Job::new(self.pass.cfg, id);
        let mut r = Record::new(name, Stage::Working, "");
        r.job = Some(id.to_owned());
        if !job.dir.is_dir() {
            r.next = "its job is gone: its repo's next pass drops the record, and takes the issue up afresh if it's still open".into();
            return r.waiting(Wait::Pass);
        }
        let jr = crate::record::derive(&job);
        let number = |p: &Path| read(p).and_then(|t| t.parse::<i64>().ok()).unwrap_or(0);
        let turns = job.turns().and_then(|t| t.parse::<i64>().ok()).unwrap_or(0);
        let answered = number(&d.join("answered"));
        let closed = issue.is_some_and(|i| i["state"] == "closed");
        use crate::record::{Stage as Turn, Wait as Held};
        let asked = jr.wait.iter().any(|w| matches!(w, Held::Ask | Held::Owner));
        let held = jr.wait.iter().any(Held::held);
        r.since = jr.since;
        match &jr.stage {
            Turn::Parked => {
                r.next = "its job goes on once the settings let it run".into();
                r = r.waiting(Wait::Paused);
            }
            Turn::Queued | Turn::Running => {
                r.next = if closed {
                    "its turn ends, its answer is posted, and the issue's closing ends the job"
                        .into()
                } else {
                    "its turn ends, and the inbox posts its answer on the issue".into()
                };
                r = r.waiting(Wait::Job(id.to_owned()));
                r.overdue_at = jr.overdue_at;
            }
            Turn::Ended(_) if answered < turns => {
                r.next = format!(
                    "its repo's next pass posts turn {}'s answer on the issue",
                    answered + 1
                );
                r = self.gated(r.waiting(Wait::Pass), repo, paused);
            }
            Turn::Ended(_) if asked => {
                r.stage = Stage::Answered;
                r.since = mtime(&d.join("answered")).unwrap_or(r.since);
                r.next = "its job goes on with the answer".into();
                r = r.waiting(Wait::Answer(id.to_owned()));
            }
            Turn::Ended(_) if held => {
                r.next = "its job goes on once what it awaits changes".into();
                r = r.waiting(Wait::Job(id.to_owned()));
                r.overdue_at = jr.overdue_at;
            }
            Turn::Ended(how) => {
                r.stage = Stage::Answered;
                r.since = mtime(&d.join("answered")).unwrap_or(r.since);
                r.next = if how == "done" {
                    "the inbox closes the issue, or an author's reply goes on with its job".into()
                } else {
                    format!("its job ended {how}: an author's reply goes on with it")
                };
            }
        }
        if r.wait.is_none() {
            r = self.gated(r, repo, paused);
        }
        r
    }

    /// The record of an issue with a sorting record at `d`, if it has one.
    fn sorting(&self, repo: &str, name: &str, d: &Path, paused: bool) -> Option<Record> {
        if !d.is_dir() {
            return None;
        }
        let Some(id) = read(&d.join("job")).filter(|j| !j.is_empty()) else {
            let r = Record::new(
                name,
                Stage::Sorting,
                "it's sorted again once it changes, or once another issue's verdict shows sorting works",
            )
            .since(mtime(&d.join("failed")));
            return Some(r);
        };
        let mut r = Record::new(name, Stage::Sorting, "").since(mtime(&d.join("job")));
        r.job = Some(id.clone());
        if self.pass.holds_sorting_slot(d) {
            let job = Job::new(self.pass.cfg, &id);
            let jr = crate::record::derive(&job);
            let asked = jr
                .wait
                .iter()
                .any(|w| matches!(w, crate::record::Wait::Ask | crate::record::Wait::Owner));
            if asked {
                r.next = "its sorting job goes on with the answer, then gives its verdict".into();
                r = r.waiting(Wait::Answer(id));
            } else {
                r.next =
                    "its sorting job gives its verdict, which the next pass carries out".into();
                r = r.waiting(Wait::Job(id));
            }
            r.overdue_at = jr.overdue_at;
            return Some(r);
        }
        if d.join("plan").is_file()
            || super::settle::settled(&Job::new(self.pass.cfg, &id)).is_some()
        {
            r.next = "the next pass that reaches GitHub carries out its verdict".into();
            return Some(r.waiting(Wait::Verdict));
        }
        r.next = "the next pass settles its sorting job and carries out its verdict".into();
        Some(self.gated(r.waiting(Wait::Pass), repo, paused))
    }

    /// A record that waits on nothing but the inbox's next pass, held
    /// instead by its repo's pause or GitHub's quota when one holds it.
    fn gated(&self, mut r: Record, repo: &str, paused: bool) -> Record {
        if !matches!(r.wait, None | Some(Wait::Pass)) {
            return r;
        }
        if paused {
            r.next = format!("once the settings allow its repo, {}", r.next);
            return r.waiting(Wait::Paused);
        }
        match self.pass.cfg.retry_at(repo) {
            Some(reset) => r.waiting(Wait::Quota(reset)),
            None => r,
        }
    }

    /// Issue N's record when it's closed, or ready, as GitHub (`issue`) has
    /// it, or else its repo's ready list: None when it's neither.
    fn closed_or_ready(
        &self,
        repo: &str,
        name: &str,
        n: &str,
        issue: Option<&Value>,
        paused: bool,
    ) -> Option<Record> {
        let listed;
        let i = match issue {
            Some(i) if i["state"] == "closed" => {
                return Some(Record::new(name, Stage::Closed, "nothing: it's closed"));
            }
            Some(i) => i,
            None => {
                listed = self.listed(repo, n)?;
                &listed
            }
        };
        self.ready(repo, name, n, i)
            .map(|r| self.gated(r, repo, paused))
    }

    /// Issue N as its repo's ready list has it, if it does.
    fn listed(&self, repo: &str, n: &str) -> Option<Value> {
        let f = files::ready_lists(&self.pass.inbox.state).join(key(repo));
        std::fs::read(&f)
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())?
            .into_iter()
            .find(|i| raw(&i["number"]) == n)
    }

    /// The record of a ready issue, as `i` (its ready list's entry, or what
    /// GitHub says of it) has it: None when it isn't one the drain hands
    /// on. Read the same way for the drain's ranking ([`Look::drains`]).
    fn ready(&self, repo: &str, name: &str, n: &str, i: &Value) -> Option<Record> {
        let p = self.pass;
        let t = p.inbox.triage.as_ref()?;
        if i.get("pull_request").is_some()
            || !p.eligible(repo, i)
            || super::enrollment::automatic_parked(p.inbox, repo, i)
        {
            return None;
        }
        let labels = labels(i);
        let theirs = labels.iter().any(|l| {
            p.inbox.labels.contains_key(*l) || *l == t.owner_label || t.skip.iter().any(|s| s == l)
        });
        if !labels.contains(&READY) || theirs {
            return None;
        }
        let since = mtime(&files::ready_lists(&p.inbox.state).join(key(repo)));
        let r = Record::new(name, Stage::Ready, "the drain hands it on, in its order").since(since);
        if let Some(super::plans::Place::Behind(step)) = p.place(repo, n) {
            return Some(
                Record {
                    next: format!("the drain hands it on once {step} has landed"),
                    ..r
                }
                .waiting(Wait::Plan(step)),
            );
        }
        let claimed = self
            .claimed
            .borrow_mut()
            .entry(repo.to_owned())
            .or_insert_with(|| p.claimed(repo))
            .contains(n);
        if claimed {
            let by = p.claimer(repo, n).unwrap_or_else(|| "a live job".into());
            return Some(
                Record {
                    next: format!("the drain hands it on once {by}, whose brief names it, is done"),
                    ..r
                }
                .waiting(Wait::Job(by)),
            );
        }
        Some(r)
    }

    /// Whether `i`, from a ready list or GitHub, is one the drain may hand
    /// on now: ready, and held by nothing but the drain's room.
    pub(super) fn drains(&self, repo: &str, i: &Value) -> bool {
        let n = raw(&i["number"]);
        let r = self.record(repo, &n, Some(i));
        r.stage == Stage::Ready
            && matches!(r.wait, None | Some(Wait::Quota(_)))
            && !files::worker(&self.pass.inbox.state, &key(repo), &n).exists()
    }

    /// Why an issue GitHub has, and the inbox keeps nothing of, is unseen.
    fn unseen(&self, repo: &str, name: &str, i: &Value, paused: bool) -> Record {
        let p = self.pass;
        if i["state"] == "closed" {
            return Record::new(name, Stage::Closed, "nothing: it's closed");
        }
        if let Some(why) = self.refused(repo, i) {
            return Record::new(
                name,
                Stage::Unseen(why),
                "nothing, until one of the authors opens an issue of their own for it",
            );
        }
        let labels = labels(i);
        let t = p.inbox.triage.as_ref();
        let keeps = [LATER, TRACKER]
            .into_iter()
            .chain(t.map(|t| t.owner_label.as_str()))
            .chain(
                t.into_iter()
                    .flat_map(|t| t.skip.iter().map(String::as_str)),
            );
        if let Some(label) = keeps.into_iter().find(|l| labels.contains(l)) {
            return Record::new(
                name,
                Stage::Unseen(Unseen::Label(label.to_owned())),
                format!("nothing, while it's labelled `{label}`"),
            );
        }
        if paused {
            return Record::new(
                name,
                Stage::Unseen(Unseen::Paused),
                "nothing, until the settings allow its repo",
            )
            .waiting(Wait::Paused);
        }
        if p.engine(i).is_some() {
            return Record::new(
                name,
                Stage::Unseen(Unseen::Unknown),
                "its repo's next pass starts its job",
            )
            .waiting(Wait::Pass);
        }
        let next = match t {
            Some(t) if t.sort_unlabelled => "its repo's next full pass starts a sorting job for it",
            _ => "nothing, until an engine label goes on it",
        };
        Record::new(name, Stage::Unseen(Unseen::NoLabel), next)
    }

    /// Why the inbox won't take `i` up whatever its labels: its opener, or
    /// outside text.
    fn refused(&self, repo: &str, i: &Value) -> Option<Unseen> {
        let p = self.pass;
        if p.outside(i) {
            return Some(Unseen::Outside(p.inbox.outside_label.clone()));
        }
        (!p.opened_by_ours(i) && !p.generated_child(repo, i))
            .then(|| Unseen::NotAuthor(raw(&i["user"]["login"])))
    }
}

/// An issue's label names.
fn labels(i: &Value) -> Vec<&str> {
    i["labels"]
        .as_array()
        .map(|ls| ls.iter().filter_map(|l| l["name"].as_str()).collect())
        .unwrap_or_default()
}

/// The pass the readers outside a pass look through: it only observes,
/// and asks GitHub nothing.
pub(super) fn observer<'a>(cfg: &'a Config, inbox: &'a crate::config::Inbox) -> Pass<'a> {
    Pass {
        cfg,
        inbox,
        project: super::projects(cfg),
        wake: BTreeMap::new(),
        unreached: Vec::new(),
        looked: Vec::new(),
        observe: true,
        scope: None,
        drain_at: None,
        skipped: Vec::new(),
    }
}

/// `proj why OWNER/REPO#N [--json]`: the issue's record, then its job's
/// when it has one. GitHub is asked about the issue only when nothing kept
/// here places it, since only GitHub can say why such an issue is unseen.
pub(crate) fn why(cfg: &Config, target: &str, json: bool) -> Result<String, crate::Fail> {
    let (repo, n) = target
        .rsplit_once('#')
        .filter(|(r, n)| r.contains('/') && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| crate::Fail::new(2, "usage: proj why OWNER/REPO#N [--json]"))?;
    let Some(inbox) = cfg.inbox.as_ref() else {
        let r = Record::new(
            target,
            Stage::Unseen(Unseen::Off),
            "nothing: the inbox isn't on here",
        );
        return Ok(says(cfg, &r, json));
    };
    Ok(observer(cfg, inbox).why(repo, n, json))
}

impl Pass<'_> {
    /// [`why`], through this pass.
    pub(super) fn why(&self, repo: &str, n: &str, json: bool) -> String {
        let repo = self
            .project
            .keys()
            .find(|r| r.eq_ignore_ascii_case(repo))
            .cloned()
            .unwrap_or_else(|| repo.to_owned());
        let look = self.look();
        let mut r = look.record(&repo, n, None);
        if matches!(
            r.stage,
            Stage::Unseen(Unseen::Unknown | Unseen::Refused | Unseen::Parked)
        ) && let Some(issue) = crate::github::api(self.cfg, &format!("repos/{repo}/issues/{n}"))
        {
            r = look.record(&repo, n, Some(&issue));
        }
        let room = || {
            self.inbox
                .triage
                .as_ref()
                .is_some_and(|t| self.drain_records().len() < t.drain_jobs)
        };
        if r.stage == Stage::Ready && r.wait.is_none() && !room() {
            r.next = "the drain hands it on once one of its jobs ends, in its order".into();
            r = r.waiting(Wait::Drain);
        }
        says(self.cfg, &r, json)
    }
}

/// The record as `proj why` prints it, followed into its job.
fn says(cfg: &Config, r: &Record, json: bool) -> String {
    let job = r
        .job
        .as_deref()
        .map(|id| (id, Job::new(cfg, id)))
        .filter(|(_, job)| job.dir.is_dir())
        .map(|(id, job)| (id, crate::record::derive(&job)));
    if json {
        let mut v = r.json();
        v["record"] = job.as_ref().map_or(Value::Null, |(id, jr)| jr.json(id));
        return format!("{v}\n");
    }
    let mut out = r.why();
    if let Some((id, jr)) = job {
        out.push_str(&format!("\nIts job, {id}:\n{}", jr.why(id)));
    }
    out
}

/// The issues waiting for sorting, each with why, as the pass recorded
/// them: what `proj status` lists.
pub(crate) fn sorting_waits(cfg: &Config) -> Vec<String> {
    let Some(i) = cfg.inbox.as_ref() else {
        return Vec::new();
    };
    let mut lines: Vec<String> = std::fs::read_dir(files::sorting_waits(&i.state))
        .into_iter()
        .flatten()
        .filter_map(|e| read(&e.ok()?.path()))
        .collect();
    lines.sort();
    lines
}

/// Whether a ready issue waits for a slot: any turn's end then kicks the
/// drain's own pass.
pub(crate) fn drain_waiting(cfg: &Config) -> bool {
    cfg.inbox
        .as_ref()
        .is_some_and(|i| files::drain_waiting(&i.state).exists())
}

/// Where the drain's waiting mark is, for tests outside the inbox that
/// set one up.
#[cfg(test)]
pub(crate) fn drain_waiting_mark(cfg: &Config) -> PathBuf {
    files::drain_waiting(&cfg.inbox.as_ref().expect("an inbox").state)
}
