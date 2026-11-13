//! A turn of a job: [`start`]ed as the job's systemd unit, which runs it
//! ([`exec`]): its wait for a slot, then its engine, as many runs as it takes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::process::{Child, Command, ExitStatus};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};

use rustix::process::{Pid, Signal, kill_process};

use crate::config::Config;
use crate::engine::{self, Engine, LONG, NEXT, Said, answer, is_error};
use crate::job::{Job, USAGE_LIMIT, kick};
use crate::pick::{Class, demoted};
use crate::question::Answer;
use crate::waits::{rewatch, rewatch_later, waiting};
use crate::{
    Fail, filled, mtime_nanos, nanos, now, now_nanos, read, scratch, slot, systemd, usage,
};

/// What a run killed for memory before it answered is told in the next.
const KILLED: &str = "Your last run was killed for memory before you answered, and everything it had running went with it. Carry on from where you left off: see what had already finished before you run anything again.";

/// What a run refused for usage is told in the next, once it's back, with
/// what it was refused saying and the turn's prompt.
pub(crate) const SPENT: &str = "Your last run was refused before it finished: Claude was out of usage, and every Claude job waited until it was back. It's back now. Carry on from where you left off: see what had already finished before you run anything again. What it said, and what you were asked this turn:";

/// Start the job's next turn with this prompt, ahead of the line or not.
/// Fails, saying why, when it can't: the job is then as it was.
pub fn start(cfg: &Config, id: &str, msg: &str, first: bool) -> Result<(), String> {
    start_with(cfg, id, msg, first, Answer::Update, None)
}

/// News that what the job awaited closed: that settles a reversible open
/// question ([`Answer::Closed`]), whose receipt carries `what` as its cause.
pub(crate) fn start_closed(cfg: &Config, id: &str, msg: &str, what: String) -> Result<(), String> {
    start_with(cfg, id, msg, false, Answer::Closed(what), None)
}

/// An explicit operator or authorized human issue reply answers the question.
pub(crate) fn start_answer(cfg: &Config, id: &str, msg: &str, first: bool) -> Result<(), String> {
    start_with(cfg, id, msg, first, Answer::Person, None)
}

/// The deadline's reversible default answers the question: the launch's
/// receipt carries that as the question's cause.
pub(crate) fn start_default(
    cfg: &Config,
    id: &str,
    msg: &str,
    capacity: &crate::inbox::resumption::Resume,
) -> Result<(), String> {
    start_with(cfg, id, msg, false, Answer::Deadline, Some(capacity))
}

/// Restoration already owns the serialized inbox admission permit.
pub(crate) fn start_admitted(
    cfg: &Config,
    id: &str,
    msg: &str,
    first: bool,
    permit: &crate::inbox::resumption::Resume,
) -> Result<(), String> {
    start_with(cfg, id, msg, first, Answer::Update, Some(permit))
}

/// A failed write may leave a truncated file. Never leave that prompt in
/// the live history, even when the caller cannot publish the new turn.
pub(crate) fn write_prompt(path: &Path, message: &str) -> std::io::Result<()> {
    if let Err(e) = crate::state_reads::write(path, format!("{message}\n")) {
        let _ = crate::state_reads::remove(path);
        return Err(e);
    }
    Ok(())
}

fn start_with(
    cfg: &Config,
    id: &str,
    msg: &str,
    first: bool,
    answer: Answer,
    permit: Option<&crate::inbox::resumption::Resume>,
) -> Result<(), String> {
    let job = Job::new(cfg, id);
    crate::inbox::enrollment::admit(&job)?;
    crate::planning::admit(&job)?;
    let unit = job.unit();
    if job
        .turns()
        .is_some_and(|n| job.turn(&n, "retry-migration").exists())
    {
        return Err(format!(
            "proj: {id}'s retained retry is being transferred; try again"
        ));
    }
    // The last turn reads as ended once it has written its outcome, but its
    // unit is there until it's over, and no unit starts under that name
    // until it's gone; its _ended (telling the watchdog, kicking proj-watch,
    // which can start this turn) is queued as it goes, and runs after. Both
    // waited out, for a minute at most, before this turn is written, so
    // _ended only ever reads the turn it ends as the latest.
    for i in 1..=120 {
        systemd::reset_failed(cfg, &unit);
        if systemd::show(cfg, &unit, "LoadState") != "loaded" && !job.end_pending() {
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
    job.quarantine_unstarted(last)
        .map_err(|e| format!("proj: quarantine unstarted turns: {e}"))?;
    let question = crate::question::Resume::prepare_as(&job, answer)?;
    let msg = question.prompt(msg);
    let n = last + 1;
    // The last turn's waits are the new turn's to clear (exec), so a turn
    // that can't start, or a start killed partway, leaves the job as it was.
    let prompt = job.path(&format!("prompt.{n}"));
    let couldnt = || format!("proj: turn {n} of {id} couldn't start");
    let undo = || {
        if n == 1 {
            let _ = crate::state_reads::remove(job.path("turns"));
        } else {
            let _ = job.set_turns(n - 1);
        }
        let _ = crate::state_reads::remove(&prompt);
    };
    if let Err(e) = write_prompt(&prompt, &msg) {
        // write can leave a truncated or partial prompt even on failure.
        undo();
        return Err(format!("proj: {}: {e}", prompt.display()));
    }
    let acquired;
    let capacity = if let Some(permit) = permit {
        permit
    } else {
        match crate::inbox::resumption::admission(cfg, &job) {
            Ok(gate) => {
                acquired = gate;
                &acquired
            }
            Err(error) => {
                undo();
                return Err(error);
            }
        }
    };
    if job.set_turns(n).is_err() {
        undo();
        return Err(couldnt());
    }
    // Claude is what goes first when memory runs out, not whatever it's
    // next to. Past its own cap the kernel kills the job's biggest process
    // (a build or an evaluation it ran, as a rule) and the turn carries on,
    // told the command was killed: systemd's default OOMPolicy would stop
    // the whole turn. Its end is a unit of its own, however the turn ends:
    // what kills the turn's whole cgroup (systemd-oomd does) would take an
    // ExecStopPost with it. The name is doubled up on its backslashes, which
    // systemd-run reads as escapes.
    let _ = crate::state_reads::write(
        job.turn(&n.to_string(), "priority"),
        if first { "first" } else { "normal" },
    );
    let started = if crate::policy::denied(&job)
        || crate::policy::parked(&job)
        || matches!(capacity, crate::inbox::resumption::Resume::Waiting)
    {
        crate::policy::mark(&job, "queued", first)?;
        true
    } else {
        launch(cfg, &job, first)
    };
    if !started {
        undo();
        return Err(couldnt());
    }
    // Accepted resumption owns the previous intent, including an error
    // recovery. A failed launch above leaves it durable for another attempt.
    let _ = crate::state_reads::remove(job.path(crate::github_quota::PENDING));
    // systemd has accepted this turn. Reconciliation errors cannot turn it
    // into a failed start: callers must consume its input exactly once.
    let reconcile_pending = if let Err(e) = question.commit() {
        eprintln!("proj: accepted turn {n} of {id}; question reconciliation pending: {e}");
        true
    } else {
        false
    };
    if crate::ask::host_path(&job, "label").exists() {
        crate::job::kick(cfg, ["proj-watch".into(), "autoland".into()]);
    }
    if reconcile_pending || crate::question::needs_watch(&job) {
        kick(cfg, ["proj-watch".to_owned()]);
    }
    rewatch(cfg, &[id]);
    Ok(())
}

pub(crate) fn launch(cfg: &Config, job: &Job, first: bool) -> bool {
    let Ok(request) = crate::job::Request::load(job) else {
        return false;
    };
    let end = job.end_unit();
    let unit = job.unit();
    let id = job.id;
    let hook = end.replace('\\', "\\\\");
    systemd::systemd_run(cfg)
        .args(["--quiet", "--collect"])
        .arg(format!("--unit={unit}"))
        .arg(format!("--slice={}", cfg.slice))
        .arg(format!("--property=MemoryMax={}", cfg.job_memory_max))
        .arg("--property=OOMPolicy=continue")
        .arg("--property=OOMScoreAdjust=500")
        .arg(format!("--property=WorkingDirectory={}", request.project))
        .arg(format!("--property=OnSuccess={hook}"))
        .arg(format!("--property=OnFailure={hook}"))
        .arg(&cfg.me)
        .args(["_exec", id, if first { "first" } else { "normal" }])
        .status()
        .is_ok_and(|s| s.success())
}

pub(crate) fn initialize_deadline(
    job: &Job,
    n: &str,
    retry: Option<&crate::retry::Checkpoint>,
    total: i64,
) -> std::io::Result<()> {
    if retry.is_none_or(|s| s.initial()) {
        for f in ["deadline", "wait", "late"] {
            crate::state_reads::remove(job.turn(n, f)).or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            })?;
        }
        if total > 0 {
            let taken = mtime_nanos(&job.turn(n, "slot")).unwrap_or_else(now_nanos);
            crate::state_reads::write(job.turn(n, "deadline"), format!("{}\n", taken + total))?;
        }
    }
    Ok(())
}

/// One turn of a job, inside its own systemd unit: `first` or `normal`, as
/// it was started.
pub fn exec(cfg: &Config, id: &str, given: &str) -> Result<(), Fail> {
    let job = Job::new(cfg, id);
    let n = job
        .turns()
        .ok_or_else(|| Fail::new(1, format!("proj: {id} has no turn to run")))?;
    // Serialize receipt acceptance with the launcher and watcher. The launcher
    // holds this lock through systemd-run, but never waits for _exec to finish.
    {
        let _lock = job.lock().map_err(|e| Fail::new(1, e.to_string()))?;
        crate::question::accept(&job, &n).map_err(|e| Fail::new(1, e))?;
    }
    let outcome = |text: &str| {
        // Distinguish a failed checkpoint-aware startup from a completed old
        // turn whose leftover checkpoint must never replay its conversation.
        if job.turn(&n, "retry-migration").exists()
            && crate::state_reads::write(job.turn(&n, "retry-startup-failed"), "").is_err()
        {
            return;
        }
        let _ = crate::state_reads::write(job.turn(&n, "outcome"), format!("{text}\n"));
    };
    // The last turn's waits, now that this one has started: this turn
    // writes its own to await.next, and await only once it ends; how it says
    // it ends goes to end.next.
    if !job.turn(&n, "retry-migration").exists() {
        crate::ask::clear_declaration(&job).map_err(|e| Fail::new(1, e))?;
        for f in ["await", "await.next"] {
            let _ = crate::state_reads::remove(job.path(f));
        }
    }
    if read(&job.path(crate::github_quota::PENDING)).is_some_and(|text| {
        serde_json::from_str::<crate::github_quota::Pending>(&text).map_or(true, |p| p.turn != n)
    }) {
        let _ = crate::state_reads::remove(job.path(crate::github_quota::PENDING));
    }
    let retry =
        crate::retry::Checkpoint::load(&job, &n).map_err(|e| Fail::new(1, e.to_string()))?;
    // What it was started as, never what the job wrote of itself since: its
    // published copy goes back to match before anything reads it.
    let request = match crate::job::Request::restore(&job) {
        Ok(r) => r,
        Err(e) => {
            outcome(&e);
            return Err(Fail::quiet(1));
        }
    };
    let agent = request.agent.clone();
    // Its scratch, made before it takes a slot: one that can't be made fails
    // the turn rather than leave its work nowhere to go. A sandbox's job works
    // in the VM's own clone.
    if agent.is_none() {
        let dir = scratch::dir(cfg, id);
        if fs::create_dir_all(&dir).is_err() {
            outcome(&format!("couldn't make its scratch dir {}", dir.display()));
            return Err(Fail::quiet(1));
        }
    }
    let choice = match engine::choice(cfg, &job) {
        Ok(c) => c,
        Err(e) => {
            outcome(&e);
            return Err(Fail::quiet(1));
        }
    };
    if choice.engine == Engine::Codex && agent.is_some() {
        outcome("codex cannot run a --vm job; agent sandboxes are Claude-only");
        return Err(Fail::quiet(1));
    }
    crate::retry::Identity::capture_choice(&job, &n, &choice).map_err(|e| Fail::new(1, e))?;
    job.record_choice(&n, &choice)
        .map_err(|e| Fail::new(1, format!("couldn't record the turn's engine: {e}")))?;
    // As its App, or not at all ([`github_app`]). Its env, read now, is what
    // every run of the turn starts in, so none starts as the user if the App
    // goes while the turn waits for a slot: it acts as an App that's gone,
    // and reaches no GitHub at all. A switch while it waits brings it back
    // here (slot::take), to read it again: an App that switch took away
    // fails the turn.
    let mut app_env = String::new();
    match github_app(cfg, request.app.clone(), choice.engine) {
        Ok(None) => {}
        Ok(Some(app)) => match cfg.app_environment(&app).filter(|e| !e.is_empty()) {
            Some(e) => app_env = e,
            None => {
                outcome(&format!("no GitHub App {app} on this machine to act as"));
                return Err(Fail::quiet(1));
            }
        },
        Err(why) => {
            outcome(&why);
            return Err(Fail::quiet(1));
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
    // Both initial waits and retained retries follow the deployed scheduler.
    // A retry checkpoint makes that handover resume this turn in place.
    let take = |hand_over: bool| {
        slot::take_engine(cfg, id, &n, given, hand_over, choice.engine)
            .map_err(|e| Fail::new(1, format!("proj: {e}")))
    };
    let mut held = Some(take(true)?);
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
    initialize_deadline(&job, &n, retry.as_ref(), total)
        .map_err(|e| Fail::new(1, e.to_string()))?;
    // Where its commands say they're waiting in line (waits::waiting).
    let waitdir = job.turn(&n, "waits");
    if retry.is_none() {
        let _ = fs::remove_dir_all(&waitdir);
    }
    crate::state_reads::directory(&waitdir)
        .map_err(|e| Fail::new(1, format!("proj: {}: {e}", waitdir.display())))?;
    // Whether its turns must say what comes next: an --owned job's starter
    // acts on each end itself.
    let must = !request.owned;
    let stream = job.turn(&n, "jsonl");
    if retry.is_none() {
        crate::state_reads::write(&stream, "").map_err(|e| Fail::new(1, e.to_string()))?;
    } else {
        crate::state_reads::append(&stream).map_err(|e| Fail::new(1, e.to_string()))?;
    }
    // What it's asked, after what its engine's conversation missed of the
    // job, if it moved here from the other engine (engine/handoff.rs).
    let first = engine::first_prompt(&job, &n, choice.engine).map_err(|e| {
        Fail::new(
            1,
            format!("proj: {}: {e}", job.turn(&n, "handoff").display()),
        )
    })?;
    let mut prompt = first.clone();
    if let Some(retry) = retry {
        prompt = retry
            .resume(&job, &n, &first)
            .map_err(|e| Fail::new(1, e.to_string()))?;
        crate::snapshot::stream::notify(cfg);
    }
    rewatch(cfg, &[id]);
    let quiet = read(&job.path("idle"))
        .filter(|i| !i.is_empty())
        .and_then(|i| nanos(&i))
        .unwrap_or(0);
    let mut killed = 0;
    let (last, mut rc) = loop {
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
        let ooms = oom_kills();
        let started = now();
        let bridge = match crate::probe::Bridge::start(cfg, &job, &n, agent.as_deref()) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("proj probe: {e}");
                break (None, 1);
            }
        };
        let child = match engine::spawn(
            cfg,
            &job,
            &n,
            &prompt,
            &waitdir,
            agent.as_deref(),
            must,
            &app_env,
            &choice,
        ) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("proj: couldn't start {}: {e}", choice.engine.name());
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
        drop(bridge);
        let rc = code(status);
        let oomed = oom_kills() > ooms;
        let last = engine::ended(
            choice.engine,
            &stream,
            from,
            exit(
                rc,
                cut,
                job.turn(&n, "quiet").exists(),
                job.turn(&n, "late").exists(),
                oomed,
            ),
            &read(&job.turn(&n, "stderr")).unwrap_or_default(),
        );
        let declaration_error = agent
            .as_deref()
            .and_then(|a| crate::guest::collect(cfg, &job, &n, a).err());
        // The turn's next run, in the same conversation and slot, is told
        // `text`, kept in its N.KIND.
        let again = |text: &str, kind: &str| -> PathBuf {
            let told = job.turn(&n, kind);
            let _ = crate::state_reads::write(&told, format!("{text}\n"));
            // The next run's quiet counts from its start, and is its own.
            let _ = crate::state_reads::remove(job.turn(&n, "quiet"));
            touch(&stream);
            // The last run's look telling the watchdog its wait was over may
            // not have got through before the rest of it went: told again.
            rewatch_later(cfg, id);
            told
        };
        // A GitHub refusal takes precedence over an engine's ask/done or
        // the missing-declaration correction. Running peers are left alone.
        if crate::github_quota::finish(&job, &n).map_err(|e| Fail::new(1, e))? {
            // The refusal (recorded by the gh door) now holds its
            // installation's queue: the watchdog's deadlines follow it.
            crate::waits::rewatch_all(cfg);
            return Ok(());
        }
        // Refused because the plan's usage is spent: so is every other
        // turn's, until the limit resets (usage.rs). This one gives up its
        // slot and waits in line again, where none is handed out till then,
        // and runs once more in its own conversation, its clock stood still
        // meanwhile. Any other answer says usage is there.
        let refusal = last
            .as_ref()
            .and_then(|l| engine::refused_file(choice.engine, l, &stream, from, now()));
        if choice.engine == Engine::Codex {
            if let Some((Some(due), _)) = &refusal {
                if let Err(e) = crate::state_reads::write(
                    job.path("await"),
                    format!("{USAGE_LIMIT}\t{}\t{due}\n", now()),
                ) {
                    outcome(&format!("couldn't keep Codex's usage wait: {e}"));
                    return Err(Fail::quiet(1));
                }
                let _ = crate::state_reads::write(
                    job.turn(&n, "result"),
                    format!("{}\n", answer(&stream)),
                );
                outcome("usage limit; waiting for Codex's reset");
                return Ok(());
            }
            if last.as_ref().is_some_and(|l| {
                is_error(l) && burrow_codex::logged_out(l["result"].as_str().unwrap_or_default())
            }) {
                let _ = crate::state_reads::write(
                    job.turn(&n, "result"),
                    "codex is logged out; run autoland-codex-login job\n",
                );
                outcome("codex is logged out; run autoland-codex-login job on the machine");
                return Err(Fail::quiet(1));
            }
        }
        match refusal {
            Some((until, said)) => {
                // A hold that can't be written holds nothing: this turn
                // ends refused, as one did before the hold.
                let spent = match usage::record(cfg, id, until, &said, now()) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("{e}: so the turn ends here, refused");
                        break (last, rc);
                    }
                };
                eprintln!("proj: {}", usage::line(&spent));
                // All of the refused run gone first, as for any run after
                // another: what it left could hold the slot it gives up
                // (slot.rs held), or run beside the next. A sandbox's work
                // is in its VM, past the unit's reach: the VM is stopped,
                // which ends all of it, and the next run's `agent sh` starts
                // it again, its clone and home as they were.
                if reap() && agent.as_deref().is_none_or(|a| stopped("agent", a)) {
                    // Waiting, as `proj state` and the watchdog read it:
                    // under the job's lock, as its slot was said (slot.rs
                    // took_slot).
                    {
                        let _lock = job.lock();
                        let _ = crate::state_reads::remove(job.turn(&n, "slot"));
                    }
                    drop(held.take());
                    rewatch(cfg, &[id]);
                    let waited = now_nanos();
                    crate::retry::Checkpoint::retry(&job, &n, given == Class::First, waited, &said)
                        .map_err(|e| Fail::new(1, e.to_string()))?;
                    crate::snapshot::stream::notify(cfg);
                    let _ = held.insert(take(true)?);
                    kick(cfg, lines(&job.path("kick")));
                    let state = crate::retry::Checkpoint::load(&job, &n)
                        .map_err(|e| Fail::new(1, e.to_string()))?
                        .ok_or_else(|| Fail::new(1, "retry checkpoint disappeared"))?;
                    prompt = state
                        .resume(&job, &n, &first)
                        .map_err(|e| Fail::new(1, e.to_string()))?;
                    crate::snapshot::stream::notify(cfg);
                    rewatch_later(cfg, id);
                    continue;
                }
                eprintln!("proj: what the refused run left wouldn't stop, so the turn ends here");
            }
            None if last.is_some() && choice.engine == Engine::Claude => {
                usage::back(cfg, Some(started))
            }
            None => {}
        }
        // Killed for memory before it answered, Claude itself as a rule (a
        // command it ran that's killed leaves it to answer), and not stopped
        // for going quiet: carried on in its own conversation, which its
        // stream named, twice a turn at most, once all of it is gone. Not a
        // sandbox's, whose work in the VM may outlive its unit's.
        if last.is_none()
            && killed < 2
            && !job.turn(&n, "quiet").exists()
            && agent.is_none()
            && oomed
            && engine::stream_names_session(&stream, from)
            && reap()
        {
            killed += 1;
            prompt = again(KILLED, "killed");
            continue;
        }
        // Still up after its answer, it was waiting on something it left in
        // the background (a build past one Bash call), which went with it.
        // Or it answered without saying what comes next, and nothing would
        // ever resume it. Either way, once a turn, it's told so in the same
        // conversation and slot, and answers again. Waiting on GitHub is
        // proj await's. Only once all of the last run is gone, so none of it
        // overlaps the next or writes to its stream (and the next starts only
        // if the turn has time left). A sandbox is stopped before retrying.
        let mut told = String::new();
        if let Some(error) = declaration_error {
            crate::ask::clear_declaration(&job).map_err(|e| Fail::new(1, e))?;
            let _ = crate::state_reads::remove(job.path("await.next"));
            told.push_str(&format!("The host couldn't register your sandbox next step: {error}. Check what you await and declare the next step again. "));
        }
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
            && !job.turn(&n, "cut").exists()
            && last.as_ref().is_some_and(|l| !is_error(l))
            && reap()
            && agent.as_deref().is_none_or(|a| stopped("agent", a))
        {
            prompt = again(&format!("{told} Then answer again."), "cut");
            continue;
        }
        if let Some(a) = agent.as_deref()
            && !stopped("agent", a)
        {
            outcome("couldn't stop the sandbox at the end of its turn");
            return Err(Fail::quiet(1));
        }
        break (last, rc);
    };
    // The turn ends as its last run did; its answer is every run's.
    let text = answer(&stream);
    if !text.is_empty() {
        let _ = crate::state_reads::write(job.turn(&n, "result"), format!("{text}\n"));
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
            let _ = crate::state_reads::write(job.turn(&n, "end"), format!("{end}\n"));
        }
        let done = !is_error(&last);
        if done {
            crate::ask::finish(cfg, &job, &end).map_err(|e| Fail::new(1, e))?;
        }
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
            let _ = crate::state_reads::write(job.path("await"), &kept);
        }
        for f in ["await.next", "end.next"] {
            let _ = crate::state_reads::remove(job.path(f));
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

/// The GitHub App a turn on ENGINE acts as: the job's own (--app, its
/// `app`), else, on Codex, the machine's (`machinery`), else none, and the
/// turn acts as the user. A Codex turn's sandbox keeps the user's own logins
/// out (engine/sandbox.rs: gh's config, the keyring, a GH_TOKEN from their
/// profile), so as the user it would reach no GitHub at all, and find that
/// out only once its work was done: on a machine with no App of its own it
/// doesn't start.
pub fn github_app(
    cfg: &Config,
    own: Option<String>,
    engine: Engine,
) -> Result<Option<String>, String> {
    let named = |a: &Option<String>| a.clone().filter(|a| !a.is_empty());
    match (named(&own), engine) {
        (Some(app), _) => Ok(Some(app)),
        (None, Engine::Claude) => Ok(None),
        (None, Engine::Codex) => named(&cfg.machinery).map(Some).ok_or_else(|| {
            "a Codex job reaches GitHub only as a GitHub App (proj run --app, or burrow.github.machinery), since its sandbox keeps your own logins out, and this job and machine have none".to_owned()
        }),
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
    if let Ok(f) = crate::state_reads::append(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// An exit status as the shell has it: 128 and the signal for a run a
/// signal ended.
/// The exit a run's engine reads how it ended from, when the run ended
/// itself: not cut, gone quiet, timed out, or killed for memory, which the
/// turn ends or carries on from, so an engine that turns a failed exit
/// into an error (Codex's) doesn't make one of it. A memory kill can come
/// to the engine itself, or to what it ran, which it may exit failed over.
fn exit(rc: i32, cut: bool, quiet: bool, late: bool, oomed: bool) -> Option<i32> {
    (!cut && !quiet && !late && !oomed).then_some(rc)
}

fn code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// A run being watched.
struct Run<'a> {
    cfg: &'a Config,
    job: &'a Job<'a>,
    n: &'a str,
    waitdir: &'a Path,
    /// Nanoseconds its stream may say nothing before it's stuck; 0 for no limit.
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
            let _ = crate::state_reads::write(job.turn(n, "deadline"), format!("{moved}\n"));
            dl = Some(moved);
        }
        self.last = now;
        if w != self.was {
            match &w {
                Some(w) => {
                    let _ = crate::state_reads::write(job.turn(n, "wait"), format!("{w}\n"));
                }
                None => {
                    let _ = crate::state_reads::remove(job.turn(n, "wait"));
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
        let stream = mtime_nanos(&job.turn(n, "jsonl")).unwrap_or(0);
        if gone_quiet(now, stream, run.quiet) {
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
            let _ = crate::state_reads::remove(run.job.turn(run.n, "wait"));
            rewatch_later(run.cfg, run.job.id);
        }
    }
}

/// Whether a stream last written at `stream` has said nothing for all of
/// `quiet` by `now`, all in nanoseconds; a `quiet` of 0 is no --idle.
fn gone_quiet(now: i64, stream: i64, quiet: i64) -> bool {
    quiet > 0 && now - stream >= quiet
}

/// Stop a run: TERM to its timeout(1), which passes it on to all of it.
fn stop(pid: Pid) {
    let _ = kill_process(pid, Signal::TERM);
}

/// Everything in this process's unit but itself: what a run left behind, in
/// whatever process group, TERM first, then KILL. False if any of it is
/// still there after that.
/// Whether sandbox `id`'s VM, and so everything a run left in it, is
/// stopped: `agent stop` (as `agent`, the command) said so.
fn stopped(agent: &str, id: &str) -> bool {
    let ok = crate::guest::bounded(
        Command::new(agent).args(["stop", id]),
        Duration::from_secs(60),
    )
    .is_ok();
    if !ok {
        eprintln!("proj: agent stop {id} didn't say the sandbox stopped");
    }
    ok
}

fn reap() -> bool {
    let me = process::id();
    let Some(cgroup) = cgroup() else {
        return true;
    };
    let procs = cgroup.join("cgroup.procs");
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

/// This process's unit's cgroup.
fn cgroup() -> Option<PathBuf> {
    let text = fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    Some(PathBuf::from("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

/// How many of the unit's processes the kernel has killed for memory, past
/// whichever cap: the job's own, its slice's or the machine's.
fn oom_kills() -> u64 {
    cgroup()
        .and_then(|c| fs::read_to_string(c.join("memory.events")).ok())
        .and_then(|events| {
            events
                .lines()
                .find_map(|l| l.strip_prefix("oom_kill ")?.trim().parse().ok())
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod engine_tests;
