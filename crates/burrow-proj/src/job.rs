//! A job: its folder in jobs, named by its id, and what proj reads of it.
//!
//! Set when it's run: `project` (where it runs), `kick` (what each turn's
//! start and end kick, a line each), `timeout` and `idle`, `pipeline` and
//! `owned` (there or not), `link`, `app`, and `agent` (its sandbox, with
//! --vm). Kept across turns: `turns` (the latest turn's number), `session`
//! (Claude's, which the next turn resumes) and `lock` (turns start under
//! it). Each turn N's own: `prompt.N`; `N.ended` (when a turn killed
//! with nothing written ended, as its `_ended` saw it), `N.withdrawable` (proj withdraw may
//! stop it while it waits), `N.slot` (the slot it took, once it has one), `N.jsonl` (its stream), `N.result` (its answer), `N.outcome`
//! (how it ended: done, error, or what stopped it), `N.end` (how it said it
//! ends), `N.deadline` (when its time is up, in nanoseconds), `N.wait` (what
//! one of its commands waits in line for), `N.waits` (where they say so),
//! `N.cut` (what it was told when its answer was cut short), and `N.late`,
//! `N.quiet` and `N.stopped` (what stopped it). And `await` (each PR or
//! issue an ended turn awaits: TARGET, since, its fingerprint then, a tab
//! apart), with the running turn's own in `await.next` and `end.next`.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::config::Config;
use crate::waits::Facts;
use crate::{filled, mtime, now, read, swap, systemd};

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

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Turn N's file of this kind: N.slot, N.jsonl...
    pub fn turn(&self, n: &str, kind: &str) -> PathBuf {
        self.dir.join(format!("{n}.{kind}"))
    }

    /// The latest turn's number, as written; None before the first.
    pub fn turns(&self) -> Option<String> {
        read(&self.path("turns"))
    }

    /// Swapped in whole, so a reader never finds it missing or half written.
    pub fn set_turns(&self, n: u64) -> io::Result<()> {
        swap(&self.path("turns"), &format!("{n}\n"))
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
    pub fn lock(&self) -> io::Result<File> {
        let f = self.lock_file()?;
        f.lock()?;
        Ok(f)
    }

    /// The same lock, shared: while it's held, no turn is halfway through
    /// starting, so its state and turn count agree.
    pub fn read_lock(&self) -> io::Result<File> {
        let f = self.lock_file()?;
        f.lock_shared()?;
        Ok(f)
    }

    fn lock_file(&self) -> io::Result<File> {
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.path("lock"))
    }

    /// Jobs from before each turn kept its own files have the last turn's
    /// slot, outcome and answer unnumbered (a turn that ran across that
    /// update writes them there too, once it ends). They're turn N's if
    /// they're no older than its prompt; anything older is an earlier
    /// turn's, and goes.
    pub fn adopt(&self) {
        let Some(n) = self.turns() else { return };
        let prompt = mtime_of(&self.path(&format!("prompt.{n}")));
        for f in ["slot", "outcome", "result"] {
            let old = self.path(f);
            let Some(t) = mtime_of(&old) else { continue };
            let newer = prompt.is_some_and(|p| p > t);
            // A turn still running the old code reads its outcome back once
            // it has written it, so that one stays until it has ended.
            if f == "outcome" && !newer && !systemd::gone(self.cfg, &self.unit()) {
                continue;
            }
            let numbered = self.turn(&n, f);
            if !numbered.exists() && !newer {
                let _ = fs::rename(&old, &numbered);
            } else {
                let _ = fs::remove_file(&old);
            }
        }
    }

    /// What `proj state` says: queued or running while its unit is up (as
    /// its latest turn has a slot or not), then how that turn ended, as it
    /// wrote it, or failed when it wrote nothing.
    pub fn state(&self) -> String {
        self.adopt();
        let n = self.turns().unwrap_or_default();
        if systemd::active(self.cfg, &self.unit()) {
            if self.turn(&n, "slot").exists() {
                "running".into()
            } else {
                "queued".into()
            }
        } else if filled(&self.turn(&n, "outcome")) {
            read(&self.turn(&n, "outcome")).unwrap_or_default()
        } else {
            "failed".into()
        }
    }

    /// Whether turn N, the latest, was cut off: its whole unit killed (by oomd,
    /// as a rule), not stopped, with no answer or error but its session in its
    /// stream, and not a sandbox's, whose VM may outlive it. Under the job's
    /// lock.
    pub fn cut_off(&self, n: &str) -> bool {
        let text = fs::read(self.turn(n, "jsonl")).unwrap_or_default();
        let text = String::from_utf8_lossy(&text);
        self.turns().as_deref() == Some(n)
            && self.state() == "failed"
            && !filled(&self.turn(n, "outcome"))
            && !self.turn(n, "stopped").exists()
            && !filled(&self.path("agent"))
            && text.contains(r#""session_id""#)
            && !text.lines().any(|l| result_of(l).is_some())
    }

    /// How turn N ended, for its log: its outcome, or failed when it has
    /// none and isn't going (its unit stopped, or killed with it, before it
    /// wrote one), as `proj state` says. None while it's queued or running,
    /// a second run included, or for an earlier turn from before turns kept
    /// their outcomes (it has no slot either). Looked at under the lock
    /// turns start under, so no turn starts halfway through and has its
    /// state told as turn N's.
    pub fn ended(&self, n: &str) -> Option<String> {
        let lock = self.lock_file().ok()?;
        lock.lock_shared().ok()?;
        self.adopt();
        if filled(&self.turn(n, "outcome")) {
            read(&self.turn(n, "outcome"))
        } else if self.turns().as_deref() == Some(n) {
            (!systemd::active(self.cfg, &self.unit())).then(|| "failed".into())
        } else if self.turn(n, "slot").exists() {
            Some("failed".into())
        } else {
            None
        }
    }

    /// Whether the unit that ends its turns (`_ended`) is still to run, or
    /// running.
    pub fn end_pending(&self) -> bool {
        !systemd::show(self.cfg, &self.end_unit(), "Job").is_empty()
    }

    /// When a turn killed before it could write its outcome ended (what kills
    /// its whole cgroup takes everything in it along), from outside that
    /// cgroup: its `_ended`'s first act. Only `_ended` writes it, and a new turn
    /// waits out the last one's `_ended` before it's written, so this is always
    /// the turn that just ended, and no look at a turn being started can leave
    /// it on a live one. Kept once written, so nothing moves it.
    pub fn mark_ended(&self) {
        let Some(n) = self.turns() else { return };
        if !self.turn(&n, "outcome").exists() && !self.turn(&n, "ended").exists() {
            let _ = fs::write(self.turn(&n, "ended"), "");
        }
    }

    /// When turn N ended, in seconds; 0 while it's going. When it wrote its
    /// outcome, or when [`Job::mark_ended`] saw it had. A look that finds neither
    /// is one in the moment before its `_ended` (now, which that then fixes), or
    /// at a turn being started, or one whose `_ended` never ran (the machine went
    /// down): its last write is all that's known, and looking again doesn't move
    /// it.
    fn ended_at(&self, n: &str, state: &str, pending: impl FnOnce() -> bool) -> i64 {
        if matches!(state, "queued" | "running") {
            return 0;
        }
        let first = |files: &[PathBuf]| files.iter().find_map(|p| mtime(p));
        first(&[self.turn(n, "outcome"), self.turn(n, "ended")])
            .or_else(|| pending().then(now))
            .or_else(|| first(&[self.turn(n, "jsonl"), self.path(&format!("prompt.{n}"))]))
            .unwrap_or(0)
    }

    /// What the stall watchdog's look reads of the latest turn; None before
    /// the first.
    pub fn facts(&self) -> Option<Facts> {
        let n = self.turns()?;
        let state = self.state();
        let prompt = self.path(&format!("prompt.{n}"));
        let opt = |p: PathBuf| filled(&p).then(|| read(&p).unwrap_or_default());
        Some(Facts {
            id: self.id.to_owned(),
            owned: self.path("owned").exists(),
            unstick: self.path("unstick").exists(),
            stopped: self.turn(&n, "stopped").exists(),
            end: opt(self.turn(&n, "end")),
            link: opt(self.path("link")),
            queued: mtime(&prompt).unwrap_or(0),
            slot: mtime(&self.turn(&n, "slot")).unwrap_or(0),
            ended: self.ended_at(&n, &state, || self.end_pending()),
            // Its deadline as it stands, to the second (none without a
            // timeout): when it took its slot and its timeout, moved on by
            // any time it waited in line. And what it waits in line for now,
            // if anything.
            deadline: read(&self.turn(&n, "deadline"))
                .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|t| t.parse::<i64>().ok())
                .map_or(0, |t| t / 1_000_000_000),
            wait: opt(self.turn(&n, "wait")).map(|w| {
                let line = w.lines().next().unwrap_or_default();
                let (since, what) = line.split_once('\t').unwrap_or((line, ""));
                (since.to_owned(), what.to_owned())
            }),
            awaits: if filled(&self.path("await")) {
                awaits(&read(&self.path("await")).unwrap_or_default())
                    .into_iter()
                    .map(|a| (a.target, a.since))
                    .collect()
            } else {
                Vec::new()
            },
            turn: n,
            state,
        })
    }
}

/// When a file last changed, to the nanosecond, if it's there.
fn mtime_of(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

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

/// A line of a stream as a result, but for an empty one: no turn taken, nothing
/// said, no error. A resumed session whose last run was stopped with work in
/// the background puts one out before it starts on its prompt, then goes on
/// working, so it answers nothing.
pub fn result_of(line: &str) -> Option<Value> {
    if !line.contains(r#""type":"result""#) {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    let said = match v.get("result") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    };
    let idle = v.get("num_turns").and_then(Value::as_f64) == Some(0.0);
    (said || !idle || is_error(&v)).then_some(v)
}

/// A turn's answer, from its stream: every result with something to say, in
/// order. Claude answers again each time a background task it left running
/// reports back, so the last result can be a line about that task with the
/// real answer one before it.
pub fn answer(stream: &Path) -> String {
    let text = fs::read(stream).unwrap_or_default();
    let mut said = Vec::new();
    for v in String::from_utf8_lossy(&text).lines().filter_map(result_of) {
        match v.get("result") {
            Some(Value::String(s)) if !s.is_empty() => said.push(s.clone()),
            _ => (),
        }
    }
    said.join("\n\n")
}

/// The last result in a stream from byte `from` on, if any.
pub fn last_result(stream: &Path, from: u64) -> Option<Value> {
    let text = fs::read(stream).ok()?;
    let text = text.get(from as usize..).unwrap_or_default();
    String::from_utf8_lossy(text)
        .lines()
        .rev()
        .find_map(result_of)
}

/// Whether a result says it's an error.
pub fn is_error(result: &Value) -> bool {
    match result.get("is_error") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(_) => true,
    }
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{Job, answer, awaits, escape, last_result, result_of};
    use crate::config::{Config, tests::example};
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
        assert!(since >= before && due == since + 120, "{since} {due}");
        assert!(wait(&old).unwrap().phase.contains("ended (failed)"));
        set_mtime(&old.turn("1", "ended"), 5000);
        old.mark_ended();
        assert_eq!(times(&old), (5000, 5120));
        assert_eq!(times(&old), (5000, 5120));

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
        assert_eq!(starting.ended_at("1", "running", || true), 0);
        assert!(!starting.turn("1", "ended").exists());
        starting.mark_ended();
        assert!(times(&starting).0 >= before);

        // A look just before its _ended gets no expired grace, and marks nothing.
        let window = new("window");
        assert!(window.ended_at("1", "failed", || true) >= before);
        assert!(!window.turn("1", "ended").exists());

        // Its _ended never ran (the machine went down): its last write, however
        // often it's looked at.
        let unseen = new("unseen");
        set_mtime(&unseen.turn("1", "jsonl"), 2000);
        assert_eq!(times(&unseen), (2000, 2120));
        assert_eq!(times(&unseen), (2000, 2120));

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
            assert_eq!(times(&said), (3000, 3120), "{how}");
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

        // Still going: no end to record.
        let going = new("going");
        set_mtime(&going.turn("1", "jsonl"), 2000);
        for how in ["queued", "running"] {
            assert_eq!(going.ended_at("1", how, || true), 0, "{how}");
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
        for f in [job.turn("2", "stopped"), job.path("agent")] {
            fs::write(&f, "x\n").unwrap();
            assert!(!job.cut_off("2"), "{}", f.display());
            fs::remove_file(&f).unwrap();
        }
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
