//! A job: its folder in jobs, named by its id, and what proj reads of it.
//!
//! Set when it's run: `project` (where it runs), `kick` (what each turn's
//! start and end kick, a line each), `timeout` and `idle`, `pipeline` and
//! `owned` (there or not), `link`, `app`, and `agent` (its sandbox, with
//! --vm). What it runs as (`project`, `engine`, `role`, `app`, `agent`,
//! `owned`) is its protected request ([`Request`]); those files are its
//! published copy. Kept across turns: `turns` (the latest turn's number), `session`
//! (Claude's, which the next turn resumes) and `lock` (turns start under
//! it). Each turn N's own: `prompt.N`; `N.ended` (when a turn killed
//! with nothing written ended, as its `_ended` saw it), `N.withdrawable` (proj withdraw may
//! stop it while it waits), `N.slot` (the slot it took, once it has one,
//! and `N.class`, the class it took it as),
//! `N.started` (immutable first execution time, Unix seconds), `N.engine`,
//! `N.model` and `N.effort` (what it runs on: no model or effort file for
//! the engine's own), `N.jsonl`
//! (its stream, with host-written `burrow.run` boundaries), `N.result`
//! (its answer), `N.outcome`
//! (how it ended: done, error, or what stopped it), `N.end` (how it said it
//! ends), `N.deadline` (when its time is up, in nanoseconds), `N.wait` (what
//! one of its commands waits in line for), `N.waits` (where they say so),
//! `N.cut` (what it was told when its answer was cut short), `N.answers`
//! (the job's answers.json as the turn left it, if there was one), `N.holds` (what it held open then) and `N.captured` (both kept), and `N.late`,
//! `N.quiet` and `N.stopped` (what stopped it). And `await` (each PR or
//! issue an ended turn awaits: TARGET, since, its fingerprint then, a tab
//! apart), with the running turn's own in `await.next` and `end.next`.
//!
//! Where each of them is, [`crate::record`]'s alone says: the markers that
//! say where it stands (its turns, slot, outcome, end, ended, stopped,
//! deadline, wait, await, unstick and transfer) only through its facts and
//! its write-only `Mark`, the rest by [`crate::record::JobFile`] and
//! [`crate::record::TurnFile`].

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod request;
mod start;

pub(crate) use request::Request;
#[cfg(test)]
mod locking_tests;

use crate::config::Config;
use crate::held::Held;
use crate::record::{JobFile, TurnFile};
use crate::waits::Facts;
use crate::{read, systemd};

/// Numeric owner of a per-turn artifact, including temporary writes.
pub(crate) fn turn_number(name: &str) -> Option<u64> {
    name.strip_prefix("prompt.")
        .unwrap_or(name)
        .split('.')
        .next()?
        .parse()
        .ok()
}

pub struct Job<'a> {
    pub cfg: &'a Config,
    pub id: &'a str,
    pub dir: PathBuf,
}

impl<'a> Job<'a> {
    pub fn new(cfg: &'a Config, id: &'a str) -> Job<'a> {
        Job {
            cfg,
            id,
            dir: cfg.jobs.join(id),
        }
    }

    /// The engine recorded for this turn, including jobs that changed engines.
    pub fn engine_label(&self, n: &str) -> String {
        match read(&self.turn_file(n, TurnFile::Engine)).as_deref() {
            Some("claude") | None => "Claude".to_owned(),
            Some(burrow_settings::credit::ENGINE) => "Claude API".to_owned(),
            Some(other) => other.to_owned(),
        }
    }

    /// What turn N runs on, in its N.engine, N.model and N.effort: no model
    /// or effort file for the engine's own.
    pub fn record_choice(&self, n: &str, choice: &crate::engine::Choice) -> io::Result<()> {
        self.record_engine(n, &choice.engine)?;
        for (kind, said) in [
            (TurnFile::Model, &choice.model),
            (TurnFile::Effort, &choice.effort),
        ] {
            let path = self.turn_file(n, kind);
            match said {
                Some(v) => crate::state_reads::write(path, v)?,
                None => match crate::state_reads::remove(path) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                },
            }
        }
        Ok(())
    }

    /// Turn N's N.engine, every writer's: once a turn has been on the
    /// credit it names the credit for good, though its route moves it back
    /// to the subscription in the same conversation (engine/route.rs) and a
    /// retained retry or a handover records it again, since that is how the
    /// credit's ledger finds the turn's job (burrow-probe's credit.rs). The
    /// first move onto the credit keeps the engine the turn was on in
    /// N.engine-before first, for good: the runs it made before markers
    /// named their engine ran there, and the ledger charges them by it.
    pub fn record_engine(&self, n: &str, engine: &str) -> io::Result<()> {
        let credit_engine = burrow_settings::credit::ENGINE;
        let path = self.turn_file(n, TurnFile::Engine);
        let was = read(&path);
        if crate::engine::conversation_of(engine) == crate::engine::conversation_of(credit_engine)
            && was.as_deref() == Some(credit_engine)
        {
            return Ok(());
        }
        if engine == credit_engine
            && let Some(was) = was.filter(|was| was != engine)
        {
            let before = self.turn_file(n, TurnFile::EngineBefore);
            if read(&before).is_none() {
                crate::state_reads::write(before, &was)?;
            }
        }
        crate::state_reads::write(path, engine)
    }

    /// Before publishing another turn, remove every artifact beyond the last
    /// committed turn from the live namespace. Failed attempts can leave more
    /// than a prompt; none may become facts when its number is reused. Keep
    /// them for diagnosis, without overwriting an earlier failed attempt.
    /// The caller holds the job lock and has waited out the previous unit.
    pub(crate) fn quarantine_unstarted(&self, last: u64) -> io::Result<()> {
        let future = fs::read_dir(&self.dir)?
            .collect::<io::Result<Vec<_>>>()?
            .into_iter()
            .filter(|e| turn_number(&e.file_name().to_string_lossy()).is_some_and(|n| n > last))
            .collect::<Vec<_>>();
        if future.is_empty() {
            return Ok(());
        }
        let root = self.file(JobFile::Unstarted);
        fs::create_dir_all(&root)?;
        let attempt = tempfile::Builder::new()
            .prefix("attempt-")
            .tempdir_in(root)?
            .keep();
        for entry in future {
            fs::rename(entry.path(), attempt.join(entry.file_name()))?;
        }
        Ok(())
    }

    /// The job's systemd unit, which runs its turns one at a time.
    pub fn unit(&self) -> String {
        format!("proj-{}.service", escape(self.id))
    }

    /// The unit that ends each of its turns (`proj _ended`), outside the
    /// turn's own: proj-ended@ID.
    pub fn end_unit(&self) -> String {
        format!("proj-ended@{}.service", escape(self.id))
    }

    /// The lock turns start under: held, no turn starts until it's let go.
    pub fn lock(&self) -> io::Result<Held> {
        Held::lock(self.lock_file()?)
    }

    /// The same lock, shared: while it's held, no turn is halfway through
    /// starting, so its state and turn count agree.
    pub fn read_lock(&self) -> io::Result<Held> {
        Held::lock_shared(self.lock_file()?)
    }

    pub(crate) fn try_lock(&self) -> io::Result<Option<Held>> {
        Held::try_lock(self.lock_file()?)
    }

    /// [`Job::try_lock`] for a pass over every job's folder: the lock only
    /// when the job already has one, never made here, so a look at the jobs
    /// puts no file back into a folder that's being removed (whose `rm -rf`
    /// would then fail on it, and leave a job of nothing but a lock).
    /// `NotFound` when the job has none, or its folder is going: the caller
    /// says whether such a job has anything for it.
    pub(crate) fn try_lock_existing(&self) -> io::Result<Option<Held>> {
        Held::try_lock(crate::state_reads::Dir::open(&self.dir)?.append_file("lock")?)
    }

    /// [`Job::read_lock`], unless a turn is starting or being stopped right
    /// now: None then, without waiting.
    pub(crate) fn try_read_lock(&self) -> io::Result<Option<Held>> {
        Held::try_lock_shared(self.lock_file()?)
    }

    fn lock_file(&self) -> io::Result<File> {
        crate::state_reads::append(&self.file(JobFile::Lock))
    }

    /// What `proj state` says: its record's stage, as [`crate::record`]
    /// derives it.
    pub fn state(&self) -> String {
        crate::record::stage(self).to_string()
    }

    /// Whether the unit that ends its turns (`_ended`) is still to run, or
    /// running.
    pub fn end_pending(&self) -> bool {
        !systemd::show(self.cfg, &self.end_unit(), "Job").is_empty()
    }

    /// How it stands and what it holds open, read together under
    /// [`Job::read_lock`], so no turn starts between the two (a reply, a
    /// question's deadline, an await's change): `proj lifecycle`, the one
    /// word on whether a job ended, which the steward and burrow-upkeep ask
    /// for every kind of job. Its state is what `proj state` says; what it
    /// holds, [`Job::holds`]. An error when either can't be read: the caller
    /// keeps the job, as one nothing can say of.
    pub fn lifecycle(&self) -> Result<(String, Vec<String>), String> {
        let _held = self
            .read_lock()
            .map_err(|e| format!("{}: {e}", self.file(JobFile::Lock).display()))?;
        crate::record::derive(self).lifecycle()
    }

    /// Whether it's still at work ([`crate::record::Record::at_work`]),
    /// read as [`Job::lifecycle`] is: what a run under its key with
    /// `--join` joins.
    pub fn at_work(&self) -> Result<bool, String> {
        let _held = self
            .read_lock()
            .map_err(|e| format!("{}: {e}", self.file(JobFile::Lock).display()))?;
        crate::record::derive(self).at_work(crate::now())
    }

    /// Keeps the latest turn's answers as it left them, beside what it held
    /// open (`N.answers`, `N.holds`), and says so (`N.captured`, written
    /// last): the next turn rewrites answers.json, and whoever collects a
    /// job's answers (the steward's claims) may not have looked before then.
    /// Run as a turn ends and again before the next starts, so one whose end
    /// never ran, or failed, is kept all the same; and a turn doesn't start
    /// until it succeeds. A file that can't be read is an error, never an
    /// absent one.
    pub fn capture(&self) -> Result<(), String> {
        let Some(n) = self.turns() else { return Ok(()) };
        if self.turn_file(&n, TurnFile::Captured).exists() {
            return Ok(());
        }
        let failed = |p: &Path, e: &dyn std::fmt::Display| format!("{}: {e}", p.display());
        match crate::state_reads::bytes(&self.file(JobFile::AnswersJson)) {
            Ok(text) => {
                let (_, held) = crate::record::derive(self).lifecycle()?;
                let holds = self.turn_file(&n, TurnFile::Holds);
                crate::state_reads::write(&holds, held.join("\n"))
                    .map_err(|e| failed(&holds, &e))?;
                let answers = self.turn_file(&n, TurnFile::Answers);
                crate::state_reads::write(&answers, text).map_err(|e| failed(&answers, &e))?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(failed(&self.file(JobFile::AnswersJson), &e)),
        }
        let done = self.turn_file(&n, TurnFile::Captured);
        crate::state_reads::write(&done, "").map_err(|e| failed(&done, &e))
    }

    /// What it holds open, whatever its latest turn's outcome, a line each
    /// as `proj lifecycle` prints them: its record's waits that a turn
    /// holds open ([`crate::record::Wait::held`]). A job that holds anything
    /// is still at work. An error when a record that would say can't be
    /// read: only one that isn't there holds nothing.
    pub fn holds(&self) -> Result<Vec<String>, String> {
        Ok(crate::record::holds(self)?
            .iter()
            .filter_map(crate::record::Wait::line)
            .collect())
    }

    /// What the stall watchdog's look reads of the latest turn, as its
    /// record has it; None before the first.
    pub fn facts(&self) -> Option<Facts> {
        crate::record::derive(self).facts
    }
}

/// An earlier proj's usage refusal's await target, which proj-watch moves
/// into the machine's hold (usage.rs). Its fields are SINCE and reset's
/// epoch.
pub const USAGE_LIMIT: &str = "usage-limit";

/// One line of a job's await: what it awaits (a PR or issue, OWNER/REPO#N
/// as `proj await` was given it), since when, and what it was then (its
/// fingerprint, "" when GitHub couldn't be asked).
#[derive(Clone, Debug, PartialEq)]
pub struct Await {
    pub target: String,
    pub since: String,
    pub was: String,
}

impl Await {
    pub fn line(&self) -> String {
        format!("{}\t{}\t{}\n", self.target, self.since, self.was)
    }
}

/// An await file's lines, as written: each split at its first two tabs.
pub fn awaits(text: &str) -> Vec<Await> {
    text.lines()
        .map(|l| {
            let mut f = l.splitn(3, '\t');
            Await {
                target: f.next().unwrap_or_default().to_owned(),
                since: f.next().unwrap_or_default().to_owned(),
                was: f.next().unwrap_or_default().to_owned(),
            }
        })
        .collect()
}

/// Kick each handler named (burrow-events'), once each.
pub fn kick(cfg: &Config, names: impl IntoIterator<Item = String>) {
    if cfg.kick.is_empty() {
        return;
    }
    let mut names: Vec<String> = names.into_iter().filter(|k| !k.is_empty()).collect();
    names.sort();
    names.dedup();
    for k in names {
        let url = format!("{}/{k}", cfg.kick);
        if burrow_events::kick::kick(&url, Duration::from_secs(5)).is_err() {
            eprintln!("proj: couldn't kick {k}");
        }
    }
}

/// A unit name's part, as systemd-escape writes it: `/` becomes `-`, and
/// each byte that isn't a letter, digit, `:`, `_` or `.` (or is a leading
/// `.`) becomes `\xNN`.
pub fn escape(s: &str) -> String {
    let mut out = String::new();
    for (i, b) in s.bytes().enumerate() {
        match b {
            b'/' => out.push('-'),
            b'.' if i == 0 => out.push_str("\\x2e"),
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b':' | b'_' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

pub(crate) fn valid_turn(n: &str) -> bool {
    !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && n.parse::<u64>().is_ok_and(|v| v > 0)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{Job, awaits, escape};
    use crate::config::{Config, tests::example};
    use crate::engine::{answer, last_result, result_of};
    use crate::now;
    use crate::waits::job_wait;

    fn set_mtime(path: &std::path::Path, secs: u64) {
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        fs::File::options()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }

    /// A turn's first move onto the credit keeps the engine it was on, which
    /// the credit's ledger charges its unnamed older runs by; nothing after
    /// (a move back, a retained retry or a handover recording it again)
    /// changes it, and a turn that starts on the credit has none.
    #[test]
    fn a_turns_first_move_onto_the_credit_keeps_the_engine_it_was_on() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut v = example();
        v["jobs"] = tmp.path().to_str().unwrap().into();
        let cfg: Config = serde_json::from_value(v).unwrap();
        let job = Job::new(&cfg, "moved");
        fs::create_dir(&job.dir).unwrap();
        let before = |n: &str| fs::read_to_string(job.turn(n, "engine-before")).ok();
        let engine = |n: &str| fs::read_to_string(job.turn(n, "engine")).unwrap();
        job.record_engine("1", "claude").unwrap();
        job.record_engine("1", "claude").unwrap();
        assert_eq!(before("1"), None);
        job.record_engine("1", "claude-api").unwrap();
        assert_eq!(engine("1"), "claude-api");
        assert_eq!(before("1").as_deref(), Some("claude"));
        for again in ["claude", "claude-api", "claude"] {
            job.record_engine("1", again).unwrap();
            assert_eq!(engine("1"), "claude-api");
            assert_eq!(before("1").as_deref(), Some("claude"));
        }
        job.record_engine("2", "claude-api").unwrap();
        job.record_engine("2", "claude").unwrap();
        job.record_engine("2", "claude-api").unwrap();
        assert_eq!(before("2"), None);
    }

    #[test]
    fn queued_and_running_jobs_report_retained_early_release() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctl = tmp.path().join("systemctl");
        burrow_test::executable(&ctl, "#!/bin/sh\nexit 0\n");
        let mut v = example();
        v["jobs"] = tmp.path().to_str().unwrap().into();
        v["tools"]["systemctl"] = ctl.to_str().unwrap().into();
        let cfg: Config = serde_json::from_value(v).unwrap();
        let job = Job::new(&cfg, "released");
        fs::create_dir(&job.dir).unwrap();
        job.set_turns(1).unwrap();
        fs::write(job.turn("1", "engine"), "claude").unwrap();
        let release = now() - 10000;
        let spent = serde_json::json!({"until": now() + 86400, "said": "spent", "job": "other",
                                      "at": release - 100, "back": release, "told": 0});
        for running in [false, true] {
            if running {
                fs::write(job.turn("1", "slot"), "").unwrap();
            }
            fs::write(cfg.jobs.join(".usage.json"), spent.to_string()).unwrap();
            fs::write(cfg.jobs.join(".usage.released"), release.to_string()).unwrap();
            for cleaned in [false, true] {
                if cleaned {
                    fs::remove_file(cfg.jobs.join(".usage.json")).unwrap();
                }
                let (state, lines) = job.lifecycle().unwrap();
                assert_eq!(state, if running { "running" } else { "queued" });
                assert!(
                    lines.contains(&format!("released Claude {release}")),
                    "{lines:?}"
                );
            }
        }
    }

    // When a turn ended, as the stall watchdog goes by it. A turn killed with
    // nothing written ended when its _ended saw it had (mark_ended), not when it
    // last wrote anything. With no systemd here, every job reads as failed unless
    // it wrote an outcome.
    #[test]
    fn when_a_turn_ended() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut v = example();
        v["jobs"] = tmp.path().to_str().unwrap().into();
        let cfg: Config = serde_json::from_value(v).unwrap();
        let new = |id: &'static str| {
            let job = Job::new(&cfg, id);
            fs::create_dir(&job.dir).unwrap();
            job.set_turns(1).unwrap();
            set_mtime(&job.path("prompt.1"), 1000);
            job
        };
        let wait = |job: &Job| job_wait(&cfg, &job.facts().unwrap());
        let times = |job: &Job| {
            let w = wait(job).unwrap();
            (w.since as i64, w.due as i64)
        };
        let before = now();

        // Killed after its stream went quiet, with an old log: its grace is
        // whole. Then marked and looked at again: it stays where it was.
        let old = new("old");
        set_mtime(&old.turn("1", "slot"), 2000);
        set_mtime(&old.turn("1", "jsonl"), 2000);
        old.mark_ended();
        let (since, due) = times(&old);
        assert!(
            since >= before && due == since + crate::RESUME_GRACE,
            "{since} {due}"
        );
        assert!(wait(&old).unwrap().phase.contains("ended (failed)"));
        set_mtime(&old.turn("1", "ended"), 5000);
        old.mark_ended();
        assert_eq!(times(&old), (5000, 5000 + crate::RESUME_GRACE));
        assert_eq!(times(&old), (5000, 5000 + crate::RESUME_GRACE));

        // Killed before any stream, and before it even had a slot.
        let nostream = new("nostream");
        set_mtime(&nostream.turn("1", "slot"), 2000);
        nostream.mark_ended();
        assert!(times(&nostream).0 >= before);
        let noslot = new("noslot");
        noslot.mark_ended();
        assert!(times(&noslot).0 >= before);

        // A look at a turn being started, its number written and its unit not
        // yet, reads as failed: it leaves nothing behind, so when the turn it
        // was is killed hours on, the grace still counts from then.
        let starting = new("starting");
        assert_eq!(times(&starting).0, 1000);
        assert!(!starting.turn("1", "ended").exists());
        assert_eq!(
            crate::record::ended_at(&starting, "1", "running", || true),
            0
        );
        assert!(!starting.turn("1", "ended").exists());
        starting.mark_ended();
        assert!(times(&starting).0 >= before);

        // A look just before its _ended gets no expired grace, and marks nothing.
        let window = new("window");
        assert!(crate::record::ended_at(&window, "1", "failed", || true) >= before);
        assert!(!window.turn("1", "ended").exists());

        // Its _ended never ran (the machine went down): its last write, however
        // often it's looked at.
        let unseen = new("unseen");
        set_mtime(&unseen.turn("1", "jsonl"), 2000);
        assert_eq!(times(&unseen), (2000, 2000 + crate::RESUME_GRACE));
        assert_eq!(times(&unseen), (2000, 2000 + crate::RESUME_GRACE));

        // One that said how it ended ended then, whatever its log says: an error
        // or a timeout has its grace from there, and no stamp.
        for (id, how) in [
            ("error", "error"),
            ("timeout", "timed out after 4h"),
            ("quiet", "went quiet for 1h"),
        ] {
            let said = new(id);
            set_mtime(&said.turn("1", "jsonl"), 2000);
            fs::write(said.turn("1", "outcome"), format!("{how}\n")).unwrap();
            set_mtime(&said.turn("1", "outcome"), 3000);
            said.mark_ended();
            assert_eq!(times(&said), (3000, 3000 + crate::RESUME_GRACE), "{how}");
            assert!(!said.turn("1", "ended").exists(), "{how}");
        }

        // Undeclared stays due at once.
        let silent = new("silent");
        fs::write(silent.turn("1", "outcome"), "done\n").unwrap();
        fs::write(silent.turn("1", "end"), "undeclared\n").unwrap();
        set_mtime(&silent.turn("1", "outcome"), 3000);
        assert_eq!(times(&silent), (3000, 3000));

        // Stopped and --owned stay unwatched, killed or not.
        let stopped = new("stopped");
        fs::write(stopped.turn("1", "stopped"), "").unwrap();
        stopped.mark_ended();
        assert!(wait(&stopped).is_none());
        let owned = new("owned");
        fs::write(owned.path("owned"), "").unwrap();
        owned.mark_ended();
        assert!(wait(&owned).is_none());

        // Inbox-managed jobs (marked by inbox-issue, host-side inbox-issue, or
        // an active legacy inbox record) get INBOX_RESUME_GRACE; regular
        // non-inbox jobs keep RESUME_GRACE (120s).
        let mut inbox_v = example();
        inbox_v["jobs"] = tmp.path().to_str().unwrap().into();
        let inbox_state = tmp.path().join("inbox-state");
        fs::create_dir_all(&inbox_state).unwrap();
        inbox_v["inbox"]["state"] = inbox_state.to_str().unwrap().into();
        let inbox_cfg: Config = serde_json::from_value(inbox_v).unwrap();
        let new_in = |id: &'static str| {
            let job = Job::new(&inbox_cfg, id);
            fs::create_dir(&job.dir).unwrap();
            job.set_turns(1).unwrap();
            set_mtime(&job.path("prompt.1"), 1000);
            set_mtime(&job.turn("1", "ended"), 5000);
            job
        };
        let times_in = |job: &Job| {
            let w = job_wait(&inbox_cfg, &job.facts().unwrap()).unwrap();
            (w.since as i64, w.due as i64)
        };
        let inbox_marked = new_in("inbox-marked");
        fs::write(inbox_marked.path("inbox-issue"), "true\n").unwrap();
        assert_eq!(
            times_in(&inbox_marked),
            (5000, 5000 + crate::INBOX_RESUME_GRACE)
        );
        let inbox_host = new_in("inbox-host");
        crate::ask::ensure(&inbox_host).unwrap();
        fs::write(
            crate::ask::host_path(&inbox_host, "inbox-issue"),
            "sam/web#1\n",
        )
        .unwrap();
        assert_eq!(
            times_in(&inbox_host),
            (5000, 5000 + crate::INBOX_RESUME_GRACE)
        );
        let inbox_legacy = new_in("inbox-legacy");
        let rec = inbox_state.join("sam_web-7");
        fs::create_dir_all(&rec).unwrap();
        fs::write(rec.join("job"), "inbox-legacy\n").unwrap();
        assert_eq!(
            times_in(&inbox_legacy),
            (5000, 5000 + crate::INBOX_RESUME_GRACE)
        );
        let non_inbox = new_in("non-inbox");
        assert_eq!(times_in(&non_inbox), (5000, 5120));

        // Still going: no end to record.
        let going = new("going");
        set_mtime(&going.turn("1", "jsonl"), 2000);
        for how in ["queued", "running"] {
            assert_eq!(
                crate::record::ended_at(&going, "1", how, || true),
                0,
                "{how}"
            );
        }
    }

    // As systemd-escape writes them: a job's id is its project's folder
    // name, spaces, `%` and all.
    #[test]
    fn units() {
        assert_eq!(
            escape("web-20261030-120000-AbCd"),
            "web\\x2d20261030\\x2d120000\\x2dAbCd"
        );
        assert_eq!(escape("my web%n"), "my\\x20web\\x25n");
        assert_eq!(escape(".dot/a.b"), "\\x2edot-a.b");
        assert_eq!(escape("é"), "\\xc3\\xa9");
        let cfg: Config = serde_json::from_value(example()).unwrap();
        let job = Job::new(&cfg, "web-1");
        assert_eq!(job.unit(), "proj-web\\x2d1.service");
        assert_eq!(job.end_unit(), "proj-ended@web\\x2d1.service");
    }

    // Every result with something to say, in order; a run's last result is
    // its own.
    #[test]
    fn answers() {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("1.jsonl");
        fs::write(
            &s,
            concat!(
                "{\"type\":\"system\",\"session_id\":\"s\"}\n",
                "{\"type\":\"result\",\"session_id\":\"s\",\"result\":\"waiting on CI\",\"is_error\":false}\n",
                "{\"type\":\"result\",\"session_id\":\"s\",\"is_error\":true,\"errors\":[\"boom\"]}\n",
                "{\"type\":\"result\",\"session_id\":\"t\",\"result\":\"the build passed\",\"is_error\":false}\n",
            ),
        )
        .unwrap();
        assert_eq!(answer(&s), "waiting on CI\n\nthe build passed");
        assert_eq!(last_result(&s, 0).unwrap()["session_id"], "t");
        let len = fs::metadata(&s).unwrap().len();
        assert_eq!(last_result(&s, len), None);
        fs::write(&s, "{\"type\":\"result\", nope\n").unwrap();
        assert_eq!(answer(&s), "");
        assert_eq!(last_result(&s, 0), None);
        assert_eq!(answer(&d.path().join("none")), "");
        // What a resumed session puts out before it starts is no answer; a
        // turn taken with nothing to say, or an error, is a result still.
        let empty = "{\"type\":\"result\",\"num_turns\":0,\"result\":\"\",\"is_error\":false}";
        assert!(result_of(empty).is_none());
        assert!(result_of("{\"type\":\"result\",\"num_turns\":0,\"is_error\":false}").is_none());
        assert!(
            result_of("{\"type\":\"result\",\"num_turns\":3,\"result\":\"\",\"is_error\":false}")
                .is_some()
        );
        assert!(result_of("{\"type\":\"result\",\"result\":\"\"}").is_some());
        assert!(
            result_of("{\"type\":\"result\",\"num_turns\":0,\"result\":\"\",\"is_error\":true}")
                .is_some()
        );
        assert!(result_of("{\"type\":\"result\",\"num_turns\":0,\"result\":\"hi\"}").is_some());
        fs::write(&s, format!("{empty}\n")).unwrap();
        assert_eq!(last_result(&s, 0), None);
    }

    // Cut off: the latest turn, gone with no outcome, no answer and not stopped,
    // its session named, and no sandbox's.
    #[test]
    fn cut_off() {
        let d = tempfile::tempdir().unwrap();
        let mut v = crate::config::tests::example();
        v["jobs"] = d.path().to_str().unwrap().into();
        v["tools"]["systemctl"] = "/nowhere/systemctl".into();
        let cfg: Config = serde_json::from_value(v).unwrap();
        let job = Job::new(&cfg, "web-1");
        fs::create_dir_all(&job.dir).unwrap();
        fs::write(job.path("turns"), "2\n").unwrap();
        let stream = job.turn("2", "jsonl");
        let session = "{\"type\":\"system\",\"session_id\":\"s\"}\n";
        let empty = "{\"type\":\"result\",\"session_id\":\"s\",\"num_turns\":0,\"result\":\"\",\"is_error\":false}\n";
        fs::write(&stream, format!("{session}{empty}")).unwrap();
        assert!(job.cut_off("2"));
        assert!(!job.cut_off("1"), "not the latest");
        fs::write(&stream, "{\"type\":\"system\"}\n").unwrap();
        assert!(!job.cut_off("2"), "no session named");
        fs::write(
            &stream,
            format!("{session}{{\"type\":\"result\",\"is_error\":true}}\n"),
        )
        .unwrap();
        assert!(!job.cut_off("2"), "it ended in a result");
        fs::write(&stream, session).unwrap();
        fs::write(job.turn("2", "stopped"), "x\n").unwrap();
        assert!(!job.cut_off("2"), "stopped");
        fs::remove_file(job.turn("2", "stopped")).unwrap();
        // A sandbox's, as its starter made it: a file the job writes isn't.
        fs::write(job.path("agent"), "x\n").unwrap();
        assert!(job.cut_off("2"), "a written agent file");
        let mut request = crate::job::Request::load(&job).unwrap();
        request.agent = Some("x".into());
        request.save(&job).unwrap();
        assert!(!job.cut_off("2"), "a sandbox's");
        request.agent = None;
        request.save(&job).unwrap();
        fs::write(job.turn("2", "outcome"), "failed\n").unwrap();
        assert!(!job.cut_off("2"), "it has an outcome");
    }

    #[test]
    fn await_lines() {
        let got = awaits("o/r#5\t100\t{\"state\":\"open\"}\nx/y#1\t200\t\nbad\n");
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].was, "{\"state\":\"open\"}");
        assert_eq!(got[1].line(), "x/y#1\t200\t\n");
        assert_eq!((got[2].target.as_str(), got[2].since.as_str()), ("bad", ""));
    }
}

#[cfg(test)]
mod usage_start_tests {
    #[test]
    fn first_execution_is_recorded_once_across_later_waits_and_runs() {
        use super::super::{
            config::{Config, tests::example},
            job::Job,
        };
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(example()).unwrap();
        cfg.jobs = tmp.path().join("jobs");
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        fs::write(job.turn("1", "jsonl"), "").unwrap();
        assert!(!job.turn("1", "started").exists());
        job.start_run("1", 100, || Ok(())).unwrap();
        fs::write(job.turn("1", "slot"), "slot").unwrap();
        fs::remove_file(job.turn("1", "slot")).unwrap();
        job.start_run("1", 300, || Ok(())).unwrap();
        assert_eq!(
            fs::read_to_string(job.turn("1", "started")).unwrap(),
            "100\n"
        );
        let lines = fs::read_to_string(job.turn("1", "jsonl")).unwrap();
        assert_eq!(lines.lines().count(), 2);
    }
}
