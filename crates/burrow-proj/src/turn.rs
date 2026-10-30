//! A turn of a job: [`start`]ed as the job's systemd unit, which runs it
//! ([`exec`]): its wait for a slot, then Claude, as many runs as it takes.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};
use std::{env, process};

use rustix::process::{Pid, Signal, kill_process};

use crate::config::Config;
use crate::job::{Job, answer, is_error, kick, last_result};
use crate::pick::{Class, demoted};
use crate::waits::{rewatch, rewatch_later, waiting};
use crate::{Fail, filled, mtime, mtime_nanos, nanos, now_nanos, read, slot, swap, systemd};

/// How a job waits on its own long work, which its answer would stop.
const LONG: &str = "A command that can outlast one Bash call (ten minutes), a long build or test run, goes to the background with its output and exit code written to a file, and you wait for that file in the foreground, in steps under ten minutes each; answer only once it's there.";

/// How a turn says what comes next: what resumes the job, if anything.
const NEXT: &str = "`proj await URL` for each PR or issue you're still waiting on (the job is resumed, with this conversation, once one of them changes: checks finish, a review, comment or push, merged or closed), `proj ask` when your answer is a question only a person can settle (their reply resumes it), or `proj done` when the work is finished";

/// Start the job's next turn with this prompt, ahead of the line or not.
/// Fails, saying why, when it can't: the job is then as it was.
pub fn start(cfg: &Config, id: &str, msg: &str, first: bool) -> Result<(), String> {
    let job = Job::new(cfg, id);
    let unit = job.unit();
    // The last turn reads as ended once it has written its outcome, but its
    // unit is still there while its _ended runs (telling the watchdog,
    // kicking proj-watch, which can start this turn), and no unit starts
    // under that name until it's gone. Waited out, for a minute at most,
    // before this turn is written, so _ended only ever reads the turn it ends
    // as the latest.
    for i in 1..=120 {
        systemd::reset_failed(cfg, &unit);
        if systemd::show(cfg, &unit, "LoadState") != "loaded" {
            break;
        }
        if i == 120 {
            return Err(format!(
                "proj: {id}'s last turn is still ending, so its next can't start yet"
            ));
        }
        sleep(Duration::from_millis(500));
    }
    let last: u64 = match job.turns() {
        None => 0,
        Some(t) => t
            .parse()
            .map_err(|_| format!("proj: {id}'s turns isn't a number: {t}"))?,
    };
    let n = last + 1;
    // The last turn's waits are the new turn's to clear (exec), so a turn
    // that can't start, or a start killed partway, leaves the job as it was.
    let prompt = job.path(&format!("prompt.{n}"));
    let couldnt = || format!("proj: turn {n} of {id} couldn't start");
    fs::write(&prompt, format!("{msg}\n"))
        .map_err(|e| format!("proj: {}: {e}", prompt.display()))?;
    let undo = || {
        if n == 1 {
            let _ = fs::remove_file(job.path("turns"));
        } else {
            let _ = job.set_turns(n - 1);
        }
        let _ = fs::remove_file(&prompt);
    };
    if job.set_turns(n).is_err() {
        undo();
        return Err(couldnt());
    }
    let me = env::current_exe().map_err(|e| format!("proj: {e}"))?;
    // Claude is what goes first when memory runs out, not whatever it's
    // next to. The job comes to its _ended in the unit's environment:
    // systemd splits and expands a command line, and a job's id is its
    // project's folder name, spaces, `%` and all.
    let started = systemd::systemd_run(cfg)
        .args(["--quiet", "--collect"])
        .arg(format!("--unit={unit}"))
        .arg(format!("--slice={}", cfg.slice))
        .arg(format!("--property=MemoryMax={}", cfg.job_memory_max))
        .arg("--property=OOMScoreAdjust=500")
        .arg(format!(
            "--property=WorkingDirectory={}",
            read(&job.path("project")).unwrap_or_default()
        ))
        .arg(format!("--setenv=PROJ_CONFIG={}", cfg.path.display()))
        .arg(format!("--setenv=PROJ_ENDED={id}"))
        .arg(format!("--property=ExecStopPost={} _ended", me.display()))
        .arg(&me)
        .args(["_exec", id, if first { "first" } else { "normal" }])
        .status()
        .is_ok_and(|s| s.success());
    if !started {
        undo();
        return Err(couldnt());
    }
    rewatch(cfg, &[id]);
    Ok(())
}

/// One turn of a job, inside its own systemd unit: `first` or `normal`, as
/// it was started.
pub fn exec(cfg: &Config, id: &str, given: &str) -> Result<(), Fail> {
    let job = Job::new(cfg, id);
    let n = job
        .turns()
        .ok_or_else(|| Fail::new(1, format!("proj: {id} has no turn to run")))?;
    let outcome = |text: &str| {
        let _ = fs::write(job.turn(&n, "outcome"), format!("{text}\n"));
    };
    // The last turn's waits, now that this one has started: this turn
    // writes its own to await.next, and await only once it ends; how it says
    // it ends goes to end.next.
    for f in ["await", "await.next", "end.next"] {
        let _ = fs::remove_file(job.path(f));
    }
    // As its App, or not at all. Its env, read now, is what every run of the
    // turn starts in, so none starts as the user if the App goes while the
    // turn waits for a slot: it acts as an App that's gone, and reaches no
    // GitHub at all.
    let mut app_env = String::new();
    if filled(&job.path("app")) {
        let app = read(&job.path("app")).unwrap_or_default();
        match cfg
            .app_env(&app)
            .and_then(|f| read(f))
            .filter(|e| !e.is_empty())
        {
            Some(e) => app_env = e,
            None => {
                outcome(&format!("no GitHub App {app} on this machine to act as"));
                return Err(Fail::quiet(1));
            }
        }
    }
    // In the turn's journal too, where its wait would be looked into.
    if let Some(why) = demoted(cfg, &job.dir) {
        eprintln!("proj: {why}");
    }
    let given = if given == "first" {
        Class::First
    } else {
        Class::Normal
    };
    let _slot = slot::take(cfg, id, &n, given).map_err(|e| Fail::new(1, format!("proj: {e}")))?;
    // It's at work now, which is news to what it was started to kick: a turn
    // can wait in line for hours, and the steward says which it is.
    kick(cfg, lines(&job.path("kick")));
    let limit = read(&job.path("timeout")).unwrap_or_else(|| cfg.timeout.clone());
    // One budget for the whole turn from the moment it took its slot,
    // however many runs it takes and the clearing up between them, to the
    // nanosecond; 0 is none. Its deadline is kept in N.deadline, which the
    // time it waits in line moves on (Watch).
    let Some(total) = nanos(&limit) else {
        eprintln!("proj: the timeout {limit} isn't a duration");
        outcome("failed");
        return Err(Fail::quiet(2));
    };
    for f in ["deadline", "wait", "late"] {
        let _ = fs::remove_file(job.turn(&n, f));
    }
    if total > 0 {
        let taken = mtime_nanos(&job.turn(&n, "slot")).unwrap_or_else(now_nanos);
        let _ = fs::write(job.turn(&n, "deadline"), format!("{}\n", taken + total));
    }
    // Where its commands say they're waiting in line (waits::waiting).
    let waitdir = job.turn(&n, "waits");
    let _ = fs::remove_dir_all(&waitdir);
    fs::create_dir(&waitdir)
        .map_err(|e| Fail::new(1, format!("proj: {}: {e}", waitdir.display())))?;
    rewatch(cfg, &[id]);
    // Whether its turns must say what comes next: an --owned job's starter
    // acts on each end itself, and a sandbox's Claude can't run proj.
    let agent = read(&job.path("agent")).filter(|a| !a.is_empty());
    let must = !job.path("owned").exists() && agent.is_none();
    let stream = job.turn(&n, "jsonl");
    File::create(&stream).map_err(|e| Fail::new(1, format!("proj: {}: {e}", stream.display())))?;
    let first_prompt = job.path(&format!("prompt.{n}"));
    let mut prompt = first_prompt.clone();
    let quiet = read(&job.path("idle"))
        .filter(|i| !i.is_empty())
        .and_then(|i| nanos(&i))
        .map_or(0, |q| (q + 999_999_999) / 1_000_000_000);
    let (last, mut rc) = loop {
        let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose"]
            .map(String::from)
            .to_vec();
        if let Some(s) = read(&job.path("session")).filter(|s| !s.is_empty()) {
            args.extend(["--resume".into(), s]);
        }
        // Where this run's stream starts: an earlier run's answer isn't its.
        let from = fs::metadata(&stream).map_or(0, |m| m.len());
        // Its deadline, looked at last thing before the run starts: past,
        // and no run starts, so the turn has timed out.
        if let Some(dl) = deadline(&job, &n)
            && dl <= now_nanos()
        {
            touch(&job.turn(&n, "late"));
            break (None, 124);
        }
        let child = match spawn(
            cfg,
            &job,
            &n,
            &prompt,
            &waitdir,
            agent.as_deref(),
            must,
            &app_env,
            args,
        ) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("proj: couldn't start claude: {e}");
                break (None, 1);
            }
        };
        let (status, cut) = Run {
            cfg,
            job: &job,
            n: &n,
            waitdir: &waitdir,
            quiet,
        }
        .watch(child, from);
        let rc = code(status);
        let last = last_result(&stream, from);
        // Still up after its answer, it was waiting on something it left in
        // the background (a build past one Bash call), which went with it.
        // Or it answered without saying what comes next, and nothing would
        // ever resume it. Either way, once a turn, it's told so in the same
        // conversation and slot, and answers again. Waiting on GitHub is
        // proj await's. Only once all of the last run is gone, so none of it
        // overlaps the next or writes to its stream (and the next starts only
        // if the turn has time left); a sandbox's work is in the VM, out of
        // reach, so a --vm job isn't told.
        let mut told = String::new();
        if cut {
            told.push_str("Your answer ended the turn while something you left running in the background was still going, and it was stopped with it: nothing more from it reaches you, and the turn isn't over. If you were waiting on it, run it again and wait for it now.");
        }
        if must && !filled(&job.path("end.next")) {
            if !told.is_empty() {
                told.push(' ');
            }
            told.push_str(&format!("Your answer didn't say what comes next, and nothing resumes a job that doesn't. Say it now: run {NEXT}. If you were waiting on anything else (a background run, a notification), nothing will tell you it's over: run it again and wait for it now, in the foreground. {LONG}"));
        } else if cut {
            told.push_str(&format!(
                " {LONG} If you were waiting on GitHub, run `proj await URL` for it."
            ));
        }
        if !told.is_empty()
            && prompt == first_prompt
            && agent.is_none()
            && last.as_ref().is_some_and(|l| !is_error(l))
            && reap()
        {
            let cut_prompt = job.turn(&n, "cut");
            let _ = fs::write(&cut_prompt, format!("{told} Then answer again.\n"));
            prompt = cut_prompt;
            // The next run's quiet counts from its start, and is its own.
            let _ = fs::remove_file(job.turn(&n, "quiet"));
            touch(&stream);
            // The last run's look telling the watchdog its wait was over may
            // not have got through before the rest of it went: told again.
            rewatch_later(cfg, id);
            continue;
        }
        break (last, rc);
    };
    // The turn ends as its last run did; its answer is every run's.
    let text = answer(&stream).unwrap_or_default();
    if !text.is_empty() {
        let _ = fs::write(job.turn(&n, "result"), format!("{text}\n"));
    }
    if let Some(last) = last {
        rc = 0;
        // How it said it ends (before its outcome, which the watchdog's look
        // reads it with), or that it wouldn't when it must.
        let mut end = read(&job.path("end.next")).unwrap_or_default();
        if end.is_empty() && must {
            end = "undeclared".into();
        }
        if !is_error(&last) && !end.is_empty() {
            let _ = fs::write(job.turn(&n, "end"), format!("{end}\n"));
        }
        let done = !is_error(&last);
        outcome(if done { "done" } else { "error" });
        // What it awaits, from the baseline `proj await` took; one line per
        // target, the first await of it in the turn.
        if done && end == "await" && filled(&job.path("await.next")) {
            let mut seen = Vec::new();
            let mut kept = String::new();
            for line in read(&job.path("await.next")).unwrap_or_default().lines() {
                let target = line.split('\t').next().unwrap_or_default();
                if !seen.contains(&target) {
                    seen.push(target);
                    kept.push_str(line);
                    kept.push('\n');
                }
            }
            // Swapped in whole, so _watch never reads half of it.
            let _ = swap(&job.path("await"), &kept);
        }
        for f in ["await.next", "end.next"] {
            let _ = fs::remove_file(job.path(f));
        }
    } else if job.turn(&n, "quiet").exists() {
        let idle = read(&job.path("idle")).unwrap_or_default();
        outcome(&format!("went quiet for {idle}"));
    } else if job.turn(&n, "late").exists() {
        outcome(&format!("timed out after {limit}"));
        rc = 124;
    } else {
        outcome("failed");
    }
    match rc {
        0 => Ok(()),
        rc => Err(Fail::quiet(u8::try_from(rc).unwrap_or(1))),
    }
}

/// A file's lines, none when it can't be read.
pub fn lines(path: &Path) -> Vec<String> {
    read(path)
        .map(|t| t.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// A turn's deadline as it stands, in nanoseconds; None for none.
fn deadline(job: &Job, n: &str) -> Option<i64> {
    read(&job.turn(n, "deadline")).and_then(|d| d.parse().ok())
}

/// The file's mtime moved on to now, made if it isn't there.
fn touch(path: &Path) {
    if let Ok(f) = File::options().append(true).create(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// An exit status as the shell has it: 128 and the signal for a run a
/// signal ended.
fn code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// One run of Claude, its output appended to the turn's stream. It goes
/// under timeout(1) with no limit of its own: stopping it through that
/// stops all of it (TERM, then KILL a minute on), as timeout(1) does at a
/// limit.
#[allow(clippy::too_many_arguments)]
fn spawn(
    cfg: &Config,
    job: &Job,
    n: &str,
    prompt: &Path,
    waitdir: &Path,
    agent: Option<&str>,
    must: bool,
    app_env: &str,
    mut args: Vec<String>,
) -> std::io::Result<Child> {
    let out = File::options().append(true).open(job.turn(n, "jsonl"))?;
    let mut c = Command::new(&cfg.tools.timeout);
    c.args(["-k", "1m", "0"]).stdout(out);
    if let Some(agent) = agent {
        c.args(["agent", "sh", agent, "claude"])
            .args(&args)
            .stdin(File::open(prompt)?);
        return c.spawn();
    }
    let id = job.id;
    args.push("--append-system-prompt".into());
    args.push(if must {
        format!("This is proj job {id}, running headless. Your turn ends with your answer, and anything you left running in the background is stopped then, so don't wait on anything in the background or with sleep. Before you answer, say what comes next, since nothing else ever resumes this job: run {NEXT}. Waiting on anything else (a background run, a notification) is waiting in this turn, in the foreground. {LONG}")
    } else {
        format!("This is proj job {id}, running headless. Your turn ends with your answer, and anything you left running in the background is stopped then, so don't wait on GitHub (CI, reviews, merges) in the background or with sleep. Run `proj await URL` for each PR or issue you're still waiting on, then end your turn saying what you wait for: the job is resumed, with this conversation, once one of them changes (checks finish, a review, comment or push, merged or closed). {LONG}")
    });
    // A login shell, so the job sees the same PATH and env as an SSH login,
    // and then its App's env, which nothing the login sets can undo; Claude
    // starts only once that has.
    c.arg(&cfg.tools.bash)
        .args([
            "-lc",
            r#"eval "$PROJ_APP_ENV" && unset PROJ_APP_ENV && exec claude "$@" <"$0""#,
        ])
        .arg(prompt)
        .args(&args)
        .env("PROJ_JOB", id)
        .env("PROJ_WAIT_DIR", waitdir)
        .env("PROJ_APP_ENV", app_env)
        .stdin(Stdio::null())
        .spawn()
}

/// A run being watched.
struct Run<'a> {
    cfg: &'a Config,
    job: &'a Job<'a>,
    n: &'a str,
    waitdir: &'a Path,
    /// Seconds its stream may say nothing before it's stuck; 0 for no limit.
    quiet: i64,
}

impl Run<'_> {
    /// Until the run is over: its exit status, and whether its answer cut
    /// it short. The answer ends the turn: a few seconds to exit by itself,
    /// then Claude and whatever it left in the background are stopped (the
    /// unit takes the rest when the turn ends), and the slot goes with them.
    /// Meanwhile its clock is read once a second, sooner when its deadline
    /// is closer ([`Clock`]).
    fn watch(&self, mut child: Child, from: u64) -> (ExitStatus, bool) {
        let pid = Pid::from_child(&child);
        let mut said = Said::new(self.job, self.n, from);
        let mut answered: Option<Instant> = None;
        let mut cut = false;
        let mut clock = Clock {
            last: now_nanos(),
            was: None,
            done: false,
            next: Instant::now(),
        };
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => (),
                Err(_) => match child.wait() {
                    Ok(status) => break status,
                    Err(_) => break ExitStatus::default(),
                },
            }
            if answered.is_none() || !said.named {
                said.look(false);
            }
            if answered.is_none() && said.answered {
                answered = Some(Instant::now());
            }
            if !cut && answered.is_some_and(|t| t.elapsed() >= Duration::from_secs(10)) {
                cut = true;
                stop(pid);
            }
            if !clock.done && Instant::now() >= clock.next {
                clock.tick(self, pid);
            }
            let poll = Instant::now() + Duration::from_millis(100);
            let until = if clock.done {
                poll
            } else {
                poll.min(clock.next)
            };
            sleep(until.saturating_duration_since(Instant::now()));
        };
        // What it said last, before it went, can be what names its session.
        if !said.named {
            said.look(true);
        }
        clock.finish(self);
        (status, cut)
    }
}

/// A run's clock: it stops the run once the turn's deadline in N.deadline
/// has passed (N.late), or once its stream has said nothing for the job's
/// --idle (N.quiet), and keeps the turn's wait in line (N.wait, from
/// [`waiting`]) for the watchdog, telling it as one starts or ends. While
/// one of its commands waits in line, the deadline moves on with the clock:
/// waiting its turn for something shared isn't being stuck, and the
/// watchdog holds a wait to stallAfterMinutes. Telling the watchdog goes on
/// in the background, never waited for: its lock or a slow kick holds up
/// neither the clock nor the end of the run.
struct Clock {
    /// When it was last read, in nanoseconds.
    last: i64,
    /// The wait in line it last saw.
    was: Option<String>,
    /// It stopped the run, or the run is over.
    done: bool,
    /// When it's read next.
    next: Instant,
}

impl Clock {
    fn tick(&mut self, run: &Run, pid: Pid) {
        let (job, n) = (run.job, run.n);
        let now = now_nanos();
        let w = waiting(run.waitdir).map(|(since, what)| format!("{since}\t{what}"));
        let mut dl = deadline(job, n);
        if w.is_some()
            && let Some(d) = dl
        {
            let moved = d + now - self.last;
            let _ = swap(&job.turn(n, "deadline"), &format!("{moved}\n"));
            dl = Some(moved);
        }
        self.last = now;
        if w != self.was {
            match &w {
                Some(w) => {
                    let _ = fs::write(job.turn(n, "wait"), format!("{w}\n"));
                }
                None => {
                    let _ = fs::remove_file(job.turn(n, "wait"));
                }
            }
            self.was = w.clone();
            rewatch_later(run.cfg, job.id);
        }
        if let Some(d) = dl
            && now >= d
        {
            touch(&job.turn(n, "late"));
            stop(pid);
            self.finish(run);
            return;
        }
        let stream = mtime(&job.turn(n, "jsonl")).unwrap_or(0);
        if run.quiet > 0 && now / 1_000_000_000 - stream >= run.quiet {
            touch(&job.turn(n, "quiet"));
            stop(pid);
            self.finish(run);
            return;
        }
        let mut nap = Duration::from_secs(1);
        if let Some(d) = dl
            && w.is_none()
            && d - now < 1_000_000_000
        {
            nap = Duration::from_nanos((d - now).max(0) as u64);
        }
        self.next = Instant::now() + nap;
    }

    /// Done with: a wait it was keeping is over.
    fn finish(&mut self, run: &Run) {
        if self.done {
            return;
        }
        self.done = true;
        if self.was.take().is_some() {
            let _ = fs::remove_file(run.job.turn(run.n, "wait"));
            rewatch_later(run.cfg, run.job.id);
        }
    }
}

/// What a run has said so far, read as it says it. Its session is kept as
/// soon as its stream first names it (the first line does), not from its
/// answer: a run killed before it answers (its unit past its memory cap,
/// say) can still be continued.
struct Said<'a> {
    job: &'a Job<'a>,
    file: Option<File>,
    partial: Vec<u8>,
    /// A line it has said is a result.
    answered: bool,
    /// A line it has said names a session.
    named: bool,
}

impl<'a> Said<'a> {
    fn new(job: &'a Job<'a>, n: &str, from: u64) -> Said<'a> {
        let file = File::open(job.turn(n, "jsonl")).ok().and_then(|mut f| {
            f.seek(SeekFrom::Start(from)).ok()?;
            Some(f)
        });
        Said {
            job,
            file,
            partial: Vec::new(),
            answered: false,
            named: false,
        }
    }

    /// Reads the lines it has said since the last look; `last` once it's
    /// gone, when a line it never ended counts too.
    fn look(&mut self, last: bool) {
        let Some(f) = &mut self.file else {
            return;
        };
        let mut more = Vec::new();
        if f.read_to_end(&mut more).is_err() {
            return;
        }
        self.partial.extend(more);
        let end = if last {
            self.partial.len()
        } else {
            match self.partial.iter().rposition(|&b| b == b'\n') {
                Some(end) => end + 1,
                None => return,
            }
        };
        let whole: Vec<u8> = self.partial.drain(..end).collect();
        for l in String::from_utf8_lossy(&whole).lines() {
            if !self.named && l.contains(r#""session_id""#) {
                self.named = true;
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(l)
                    && let Some(serde_json::Value::String(s)) = v.get("session_id")
                    && !s.is_empty()
                {
                    let _ = fs::write(self.job.path("session"), format!("{s}\n"));
                }
            }
            if l.contains(r#""type":"result""#) {
                self.answered = true;
            }
        }
    }
}

/// Stop a run: TERM to its timeout(1), which passes it on to all of it.
fn stop(pid: Pid) {
    let _ = kill_process(pid, Signal::TERM);
}

/// Everything in this process's unit but itself: what a run left behind, in
/// whatever process group, TERM first, then KILL. False if any of it is
/// still there after that.
fn reap() -> bool {
    let me = process::id();
    let cgroup = fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    let Some(path) = cgroup.lines().find_map(|l| l.strip_prefix("0::")) else {
        return true;
    };
    let procs = PathBuf::from("/sys/fs/cgroup")
        .join(path.trim_start_matches('/'))
        .join("cgroup.procs");
    let mut signals = vec![Some(Signal::TERM)];
    signals.extend([None; 9]);
    signals.push(Some(Signal::KILL));
    signals.extend([None; 9]);
    for sig in signals {
        let others: Vec<Pid> = fs::read_to_string(&procs)
            .unwrap_or_default()
            .lines()
            .filter_map(|p| p.trim().parse::<u32>().ok())
            .filter(|&p| p != me)
            .filter_map(|p| Pid::from_raw(p as i32))
            .collect();
        if others.is_empty() {
            return true;
        }
        if let Some(sig) = sig {
            for p in others {
                let _ = kill_process(p, sig);
            }
        }
        sleep(Duration::from_secs(1));
    }
    false
}
