//! Where a job's files are. The ones that say where it stands (its
//! markers: [`Marker`], [`TurnMarker`]) have their paths here, private to
//! the record: a reader asks the record ([`crate::record`]'s facts), a
//! writer marks them through [`crate::record::Mark`], which can't read.
//! Everything else in the folder is data the job keeps (its prompts, its
//! stream, its answers, what it runs as), named by [`JobFile`] and
//! [`TurnFile`], which can't name a marker.
//!
//! [`Job::path`] and [`Job::turn`], which take any name, are the inbox's
//! until #975 moves it onto the record, and its tests' and the other tests'
//! for building a job's folder by hand: deprecated, so nothing else can
//! use them while clippy's warnings are errors.

use std::path::PathBuf;

use crate::engine::Engine;
use crate::job::{Job, valid_turn};

/// A file of the job's own that holds data, not where it stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobFile {
    /// Its sandbox (with --vm).
    Agent,
    /// The answers a turn leaves for whoever collects them.
    AnswersJson,
    /// The GitHub App it acts as.
    App,
    /// The question the running turn asks, as it said it.
    AskNext,
    /// What the running turn awaits, until it ends.
    AwaitNext,
    /// How the running turn says it ends, until it ends.
    EndNext,
    /// The engine it was first run on, from before the request record.
    Engine,
    /// Its GitHub stand-in's folder.
    Github,
    /// The waits GitHub's quota kept it from registering
    /// ([`crate::github_quota`]).
    GithubPending,
    /// How long it may go quiet.
    Idle,
    /// The issue the inbox started it for.
    InboxIssue,
    /// What each turn's start and end kick, a line each.
    Kick,
    /// Its link.
    Link,
    /// The lock turns start under.
    Lock,
    /// The turn whose question is open.
    OpenQuestion,
    /// That its starter owns it.
    Owned,
    /// That it's finishing its task split.
    PlanningHold,
    /// Where it runs.
    Project,
    /// The turn whose question has been answered through.
    QuestionAnsweredThrough,
    /// Its role.
    Role,
    /// Claude's session, which the next turn resumes.
    Session,
    /// Each engine's own session.
    SessionOf(Engine),
    /// The engine its session is.
    SessionEngine,
    /// When it began waiting, for its place in line.
    Since,
    /// What its task came from.
    TaskSource,
    /// That it's the pipeline's own work.
    Pipeline,
    /// The unit or job that started it.
    Starter,
    /// What kind of usage it spends.
    UsageKind,
    /// How long a turn may run.
    Timeout,
    /// Where an unstarted turn's files are kept out of the way.
    Unstarted,
}

impl JobFile {
    fn name(self) -> String {
        match self {
            JobFile::Agent => "agent",
            JobFile::AnswersJson => "answers.json",
            JobFile::App => "app",
            JobFile::AskNext => "ask.next",
            JobFile::AwaitNext => "await.next",
            JobFile::EndNext => "end.next",
            JobFile::Engine => "engine",
            JobFile::Github => "github",
            JobFile::GithubPending => "github-await",
            JobFile::Idle => "idle",
            JobFile::InboxIssue => "inbox-issue",
            JobFile::Kick => "kick",
            JobFile::Link => "link",
            JobFile::Lock => "lock",
            JobFile::OpenQuestion => "open-question",
            JobFile::Owned => "owned",
            JobFile::PlanningHold => "planning-hold",
            JobFile::Project => "project",
            JobFile::QuestionAnsweredThrough => "question-answered-through",
            JobFile::Role => "role",
            JobFile::Session => "session",
            JobFile::SessionOf(engine) => return format!("session.{}", engine.name()),
            JobFile::SessionEngine => "session.engine",
            JobFile::Since => "since",
            JobFile::TaskSource => "task-source",
            JobFile::Pipeline => "pipeline",
            JobFile::Starter => "starter",
            JobFile::UsageKind => "usage-kind",
            JobFile::Timeout => "timeout",
            JobFile::Unstarted => ".unstarted",
        }
        .to_owned()
    }
}

/// A turn's file that holds data, not where the job stands: N.KIND.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnFile {
    Answers,
    Captured,
    Class,
    Cut,
    Effort,
    Engine,
    Execution,
    GithubAwaits,
    GithubLimit,
    GithubRefusals,
    Handoff,
    Holds,
    Jsonl,
    Killed,
    Late,
    Model,
    Priority,
    Question,
    QuestionClose,
    QuestionClosed,
    QuestionHidden,
    QuestionLapsed,
    QueueOrder,
    Quiet,
    Resets,
    Result,
    RetryAttempt,
    RetryCancelled,
    RetryFinalized,
    RetryFinalizing,
    RetryWake,
    Spent,
    Split,
    Started,
    Stderr,
    Timeout,
    Waits,
    Withdrawable,
}

impl TurnFile {
    fn kind(self) -> &'static str {
        match self {
            TurnFile::Answers => "answers",
            TurnFile::Captured => "captured",
            TurnFile::Class => "class",
            TurnFile::Cut => "cut",
            TurnFile::Effort => "effort",
            TurnFile::Engine => "engine",
            TurnFile::Execution => "execution",
            TurnFile::GithubAwaits => "github-awaits",
            TurnFile::GithubLimit => "github-limit",
            TurnFile::GithubRefusals => "github-refusals",
            TurnFile::Handoff => "handoff",
            TurnFile::Holds => "holds",
            TurnFile::Jsonl => "jsonl",
            TurnFile::Killed => "killed",
            TurnFile::Late => "late",
            TurnFile::Model => "model",
            TurnFile::Priority => "priority",
            TurnFile::Question => "question",
            TurnFile::QuestionClose => "question-close",
            TurnFile::QuestionClosed => "question-closed",
            TurnFile::QuestionHidden => "question-hidden",
            TurnFile::QuestionLapsed => "question-lapsed",
            TurnFile::QueueOrder => "queue-order",
            TurnFile::Quiet => "quiet",
            TurnFile::Resets => "resets",
            TurnFile::Result => "result",
            TurnFile::RetryAttempt => "retry-attempt",
            TurnFile::RetryCancelled => "retry-cancelled",
            TurnFile::RetryFinalized => "retry-finalized",
            TurnFile::RetryFinalizing => "retry-finalizing",
            TurnFile::RetryWake => "retry-wake",
            TurnFile::Spent => "spent",
            TurnFile::Split => "split",
            TurnFile::Started => "started",
            TurnFile::Stderr => "stderr",
            TurnFile::Timeout => "timeout",
            TurnFile::Waits => "waits",
            TurnFile::Withdrawable => "withdrawable",
        }
    }
}

/// A file of the job's own that says where it stands. Its path is the
/// record's alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Marker {
    /// The latest turn's number.
    Turns,
    /// Each PR or issue an ended turn awaits.
    Await,
    /// That it was made to run --unstick.
    Unstick,
}

/// A turn's file that says where the job stands. Its path is the record's
/// alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TurnMarker {
    /// How it ended: done, error, or what stopped it.
    Outcome,
    /// The slot it took.
    Slot,
    /// When a turn killed with nothing written ended, as its `_ended` saw.
    Ended,
    /// That proj stop stopped it.
    Stopped,
    /// How it said it ends.
    End,
    /// What one of its commands waits in line for.
    Wait,
    /// When its time is up, in nanoseconds.
    Deadline,
    /// A transfer's checkpoint, while one holds it.
    RetryMigration,
    /// That a transfer's resumed turn failed to start.
    RetryStartupFailed,
}

impl Marker {
    fn name(self) -> &'static str {
        match self {
            Marker::Turns => "turns",
            Marker::Await => "await",
            Marker::Unstick => "unstick",
        }
    }
}

impl TurnMarker {
    fn kind(self) -> &'static str {
        match self {
            TurnMarker::Outcome => "outcome",
            TurnMarker::Slot => "slot",
            TurnMarker::Ended => "ended",
            TurnMarker::Stopped => "stopped",
            TurnMarker::End => "end",
            TurnMarker::Wait => "wait",
            TurnMarker::Deadline => "deadline",
            TurnMarker::RetryMigration => "retry-migration",
            TurnMarker::RetryStartupFailed => "retry-startup-failed",
        }
    }
}

/// Turn N's file of this kind: in the job's folder, or on the host's side of
/// it for a transfer's own files and its execution record. An N that isn't
/// a turn's number names no turn's file.
fn numbered(job: &Job, n: &str, kind: &str) -> PathBuf {
    let n = if valid_turn(n) { n } else { "invalid-turn" };
    if (kind.starts_with("retry-") && kind != "retry-wake") || kind == "execution" {
        crate::ask::host_path(job, &format!("{n}.{kind}"))
    } else {
        job.dir.join(format!("{n}.{kind}"))
    }
}

impl Job<'_> {
    /// The job's data file `f`.
    pub fn file(&self, f: JobFile) -> PathBuf {
        self.dir.join(f.name())
    }

    /// Turn N's data file `f`: N.jsonl, N.result...
    pub fn turn_file(&self, n: &str, f: TurnFile) -> PathBuf {
        numbered(self, n, f.kind())
    }

    /// Turn N's prompt, prompt.N.
    pub fn prompt(&self, n: &str) -> PathBuf {
        self.dir.join(format!("prompt.{n}"))
    }

    /// Marker `m`'s path: the record's alone.
    pub(super) fn marker(&self, m: Marker) -> PathBuf {
        self.dir.join(m.name())
    }

    /// Turn N's marker `m`'s path: the record's alone.
    pub(super) fn turn_marker(&self, n: &str, m: TurnMarker) -> PathBuf {
        numbered(self, n, m.kind())
    }

    /// A file from before each turn kept its own: `slot`, `outcome` or
    /// `result`, unnumbered ([`Job::adopt`]).
    pub(super) fn unnumbered(&self, name: &'static str) -> PathBuf {
        self.dir.join(name)
    }

    /// Any file of the job's, by name. The inbox's until #975, and tests'.
    #[deprecated(
        note = "a job's markers are the record's (crate::record); its data is Job::file's"
    )]
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Turn N's file of any kind. The inbox's until #975, and tests'.
    #[deprecated(
        note = "a turn's markers are the record's (crate::record); its data is Job::turn_file's"
    )]
    pub fn turn(&self, n: &str, kind: &str) -> PathBuf {
        numbered(self, n, kind)
    }
}
