//! proj's commands, as the crate's docs list them.

use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::time::Instant;

use crate::config::Config;
use crate::engine::{is_error, last_result};
use crate::github::{fingerprint_until, over, target};
use crate::host_jobs::View;
use crate::job::{Job, awaits, kick};
use crate::snapshot::{self, Snapshot};
use crate::turn::{exec, lines};
use crate::waits::rewatch;
use crate::{Fail, io_fail, now, read, scratch, slot, systemd, usage, watch};

mod run;
mod starter;

pub(crate) use run::key_name;
pub(crate) use run::reply_answer;
pub use run::{New, reply, start_job};

/// What a command prints: a reader that's gone (`proj status | head`) is no
/// failure of the command's.
pub(crate) fn put(text: &str) {
    let _ = io::stdout().lock().write_all(text.as_bytes());
}

/// The command line's word at `i`, which must be there and not empty.
fn arg<'a>(args: &'a [String], i: usize, usage: &str) -> Result<&'a str, Fail> {
    args.get(i)
        .map(String::as_str)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| Fail::new(1, usage))
}

/// The job a command run from inside one is about.
fn this_job(var: &str, what: &str) -> Result<String, Fail> {
    env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Fail::new(1, format!("{var}: {what}")))
}

pub fn dispatch(cfg: &Config, args: &[String]) -> Result<(), Fail> {
    dispatch_as(cfg, args, View::Owner)
}

/// A command, its output rendered for `view`. Only
/// [`crate::host_jobs::view`] asks for [`View::Bridge`], after validating
/// the bridge's grammar.
pub(crate) fn dispatch_as(cfg: &Config, args: &[String], view: View) -> Result<(), Fail> {
    match args.first().map(String::as_str).filter(|a| !a.is_empty()) {
        Some("_retry_format") => {
            put(crate::slot::RETRY_HANDOVER_FORMAT);
            Ok(())
        }
        Some("_retry-transfer") => {
            crate::retry::watch(cfg).map_err(|e| Fail::new(1, e.to_string()))
        }
        Some("check-config") => {
            if args.len() != 1 {
                return Err(Fail::new(2, "usage: proj check-config"));
            }
            cfg.check_config().map_err(|e| Fail::new(1, e))
        }
        Some("engines") => {
            let said = cfg
                .settings
                .as_ref()
                .ok_or_else(|| Fail::new(1, "settings are not configured"))?
                .command(args, "proj engines")
                .map_err(|e| Fail::new(1, e))?;
            put(&said);
            Ok(())
        }
        Some("settings") => {
            put(&crate::settings::command(cfg, &args[1..]).map_err(|e| Fail::new(1, e))?);
            Ok(())
        }
        Some("_claude") => crate::engine::exec_claude(cfg, &args[1..]),
        Some("_codex") => crate::engine::exec_codex(cfg, &args[1..]),
        Some(cmd @ ("run" | "reply")) => run::run(cfg, cmd == "reply", &args[1..]),
        Some("_host-view") if view == View::Owner => crate::host_jobs::view(cfg, &args[1..]),
        None | Some("status") if args.get(1).is_some_and(|a| a == "--line") => {
            put(&cfg.settings_line());
            put(&Snapshot::take_as(cfg, 0, view)?.in_line());
            Ok(())
        }
        None | Some("status") => status(cfg, args.get(1), view),
        Some("log") => log(cfg, args, view),
        Some("state") if args.get(1).is_some_and(|a| a == "--json") => {
            let jobs = match args.get(2) {
                None => snapshot::JOBS,
                Some(n) => n
                    .parse()
                    .map_err(|_| Fail::new(2, "usage: proj state --json [N]"))?,
            };
            put(&state_json(cfg, jobs, view)?);
            Ok(())
        }
        Some("state") if args.get(1).is_some_and(|a| a == "--schema") => {
            put(&Snapshot::schema());
            Ok(())
        }
        Some("state") => {
            let id = arg(args, 1, "usage: proj state ID")?;
            put(&format!("{}\n", Job::new(cfg, id).state()));
            Ok(())
        }
        Some("lifecycle") => {
            let id = arg(args, 1, "usage: proj lifecycle ID")?;
            let job = Job::new(cfg, id);
            if !job.dir.is_dir() {
                return Err(Fail::new(1, format!("no job {id}")));
            }
            let (state, held) = job.lifecycle().map_err(|e| Fail::new(1, e))?;
            put(&std::iter::once(state)
                .chain(held)
                .map(|l| format!("{}\n", view.plain(&l)))
                .collect::<String>());
            Ok(())
        }
        Some("find") => {
            let key = arg(args, 1, "usage: proj find KEY")?;
            if let Some(id) = run::find(cfg, key)? {
                put(&format!("{id}\n"));
            }
            Ok(())
        }
        Some("stop") => stop(cfg, arg(args, 1, "usage: proj stop ID")?, false),
        Some("withdraw") => stop(cfg, arg(args, 1, "usage: proj withdraw ID")?, true),
        Some("unstick") => unstick(cfg, arg(args, 1, "usage: proj unstick ID")?),
        Some("list") => {
            let mut names: Vec<String> = fs::read_dir(&cfg.projects)
                .map_err(|e| io_fail(&cfg.projects, e))?
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            put(&names
                .iter()
                .map(|n| format!("{}\n", view.plain(n)))
                .collect::<String>());
            Ok(())
        }
        Some("usage") => match args.get(1).map(String::as_str) {
            None => {
                // What can't be taken now is left where it is, for the next
                // pass: what's held already is still said.
                if let Err(e) = usage::adopt(cfg) {
                    eprintln!("{e}");
                }
                let mut value = usage::out(cfg)
                    .and_then(|s| serde_json::to_value(s).ok())
                    .unwrap_or_else(|| serde_json::json!({}));
                let meters = crate::github_quota::meters(cfg);
                if !meters.is_empty() {
                    value["github"] = serde_json::json!(meters);
                }
                if value.as_object().is_some_and(|v| !v.is_empty()) {
                    put(&format!("{value}\n"));
                }
                Ok(())
            }
            Some("back") => {
                if let Some(settings) = &cfg.settings {
                    settings
                        .command(&["back".into()], "proj usage back")
                        .map_err(|e| Fail::new(1, e))?;
                }
                usage::back(cfg, None);
                Ok(())
            }
            Some("hold") => {
                let help = "usage: proj usage hold UNTIL [--only pipeline | --reason TEXT]";
                if args.get(3).is_some_and(|s| s == "--only") {
                    if args.len() != 5 || args[4] != "pipeline" {
                        return Err(Fail::new(2, help));
                    }
                    let settings = cfg
                        .settings
                        .as_ref()
                        .ok_or_else(|| Fail::new(1, "settings are not configured"))?;
                    put(&settings
                        .command(&["pipeline".into(), args[2].clone()], "proj usage hold")
                        .map_err(|e| Fail::new(1, e))?);
                    return Ok(());
                }
                let until = arg(args, 2, help)?
                    .parse::<i64>()
                    .map_err(|_| Fail::new(2, help))?;
                let reason = match &args[3..] {
                    [] => "Claude work paused by the owner",
                    [flag, reason] if flag == "--reason" && !reason.trim().is_empty() => reason,
                    _ => return Err(Fail::new(2, help)),
                };
                usage::hold(cfg, until, reason).map_err(|e| Fail::new(1, e))
            }
            // Import the old watchdog's undated limit without reviving a released hold.
            Some("adopt") => {
                if args.len() != 3 {
                    return Err(Fail::new(2, "usage: proj usage adopt UNTIL"));
                }
                let until = arg(args, 2, "usage: proj usage adopt UNTIL")?
                    .parse::<i64>()
                    .map_err(|_| Fail::new(2, "usage: proj usage adopt UNTIL (seconds)"))?;
                usage::earlier(cfg, "the stall watchdog", until, None).map_err(|e| Fail::new(1, e))
            }
            Some(_) => Err(Fail::new(
                2,
                "usage: proj usage [back | hold UNTIL [--reason TEXT] | adopt UNTIL]",
            )),
        },
        Some("await") => wait_on(cfg, &args[1..]),
        Some("split") => {
            let id = this_job("PROJ_JOB", "proj split is for the inbox job being split")?;
            let file = arg(
                args,
                1,
                "usage: proj split FILE (JSON title/body/check tasks)",
            )?;
            let max = crate::planning::MAX_TASKS * (crate::planning::MAX_BODY + 1000);
            let mut bytes = Vec::new();
            File::open(file)
                .and_then(|f| f.take(max as u64 + 1).read_to_end(&mut bytes))
                .map_err(|e| io_fail(std::path::Path::new(file), e))?;
            if bytes.len() > max {
                return Err(Fail::new(2, "split exceeds the bounded plan size"));
            }
            let text = std::str::from_utf8(&bytes).map_err(|e| Fail::new(2, e.to_string()))?;
            let tasks = crate::planning::parse(text).map_err(|e| Fail::new(2, e))?;
            let job = Job::new(cfg, &id);
            if !job.path("inbox-issue").exists()
                || !cfg.inbox.as_ref().is_some_and(|i| i.planning)
                || read(&job.path("link"))
                    .as_deref()
                    .and_then(target)
                    .is_none()
            {
                return Err(Fail::new(
                    2,
                    "split needs an inbox issue with planning enabled",
                ));
            }
            let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
            let n = job.turns().ok_or_else(|| Fail::new(2, "no active turn"))?;
            let path = job.turn(&n, "split");
            crate::planning::declare_split(&job, &n, &tasks).map_err(|e| io_fail(&path, e))?;
            put(declare_end(&job, "split")?);
            Ok(())
        }
        Some(how @ ("ask" | "done")) => ends(cfg, how, &args[1..]),
        Some("merge-questions") => {
            let key = arg(args, 1, MERGE_USAGE)?;
            if args.len() < 3 {
                return Err(Fail::new(2, MERGE_USAGE));
            }
            let leader = crate::ask::merge(cfg, key, &args[2..]).map_err(|e| Fail::new(1, e))?;
            put(&format!("merged under {leader}'s notice\n"));
            Ok(())
        }
        Some("_policy") => crate::policy::reconcile(cfg).map_err(|e| Fail::new(1, e)),
        Some("_questions") => {
            put(&serde_json::to_string(&crate::ask::owner_lines(cfg)).unwrap());
            Ok(())
        }
        Some("_watch") => watch::look(cfg, args.get(1).map_or("", String::as_str)),
        Some("_ended") => ended(cfg),
        Some("_handover_format") => {
            put(&format!("{}\n", slot::HANDOVER_FORMAT));
            Ok(())
        }
        Some("_rewatch") => {
            let ids: Vec<&str> = args[1..].iter().map(String::as_str).collect();
            rewatch(cfg, &ids);
            Ok(())
        }
        Some("_exec") => {
            slot::handed_over(cfg);
            exec(
                cfg,
                arg(args, 1, "usage: proj _exec ID first|normal")?,
                args.get(2).map_or("", String::as_str),
            )
        }
        Some(_) => Err(Fail::new(
            2,
            "usage: proj run|reply|status|log|state|find|stop|withdraw|unstick|list|check-config|usage|await|ask|done|merge-questions",
        )),
    }
}

/// `proj status [N]`: the N jobs most recently changed, and how each is,
/// as the live snapshot has them.
fn status(cfg: &Config, limit: Option<&String>, view: View) -> Result<(), Fail> {
    let limit: usize = match limit {
        None => snapshot::JOBS,
        Some(n) => n
            .parse()
            .map_err(|_| Fail::new(1, format!("proj status: {n} isn't a number")))?,
    };
    put(&cfg.settings_line());
    put(&Snapshot::take_as(cfg, limit, view)?.status());
    Ok(())
}

/// `proj state --json [N]`: the live snapshot, with the N jobs most recently
/// changed.
pub(crate) fn state_json(cfg: &Config, jobs: usize, view: View) -> Result<String, Fail> {
    let snap = Snapshot::take_as(cfg, jobs, view)?;
    Ok(format!("{}\n", serde_json::to_string(&snap).unwrap()))
}

/// `proj log ID [--turn N] [--raw]`: a turn's answer, and how it ended.
fn log(cfg: &Config, args: &[String], view: View) -> Result<(), Fail> {
    let usage = "usage: proj log ID [--turn N] [--raw]";
    let id = arg(args, 1, usage)?;
    let (mut n, mut raw) = (None, false);
    let mut i = 2;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "--raw" => raw = true,
            "--turn" => {
                n = Some(arg(args, i + 1, usage)?.to_owned());
                i += 1;
            }
            _ => return Err(Fail::new(2, usage)),
        }
        i += 1;
    }
    let job = Job::new(cfg, id);
    // The latest turn's only when no turn was asked for.
    let n = match n {
        Some(n) => n,
        None => job
            .turns()
            .ok_or_else(|| Fail::new(1, format!("proj: {id} has no turns")))?,
    };
    if raw {
        let stream = job.turn(&n, "jsonl");
        crate::state_reads::copy(&stream, &mut io::stdout().lock())
            .map_err(|e| io_fail(&stream, e))?;
        return Ok(());
    }
    match told_as(cfg, id, &n, view) {
        Ok(text) => {
            put(&text);
            Ok(())
        }
        Err(text) => {
            put(&text);
            Err(Fail::quiet(1))
        }
    }
}

/// Turn N's answer, and how the turn ended, as `proj log` says them; Err
/// when it has no answer, with what's said instead.
pub fn told(cfg: &Config, id: &str, n: &str) -> Result<String, String> {
    told_as(cfg, id, n, View::Owner)
}
/// [`told`], as `view` shows it: the bridge's redacts each result, notice and
/// outcome on its own, so one's unclosed quote can't swallow the next.
fn told_as(cfg: &Config, id: &str, n: &str, view: View) -> Result<String, String> {
    let job = Job::new(cfg, id);
    // A turn's answer is read from its own stream, so an earlier turn's is
    // still there once later ones have started.
    let stream = job.turn(n, "jsonl");
    let last = last_result(&stream, 0);
    let mut said = crate::engine::results(&stream)
        .iter()
        .map(|r| view.plain(r))
        .collect::<Vec<_>>()
        .join("\n\n");
    if read(&job.turn(n, "end")).as_deref() == Some("ask") {
        match crate::ask::load(&job) {
            Ok(Some(q)) => said.push_str(&view.plain(&q.notice())),
            Ok(None) => (),
            Err(e) => said.push_str(&format!(
                "\n\nThis question's deadline and default can't be read now: {e}"
            )),
        }
    }
    let said = Some(said).filter(|a| !a.is_empty());
    if let (Some(last), Some(said)) = (&last, said) {
        let mut out = format!("{said}\n");
        // An answer, then an error, or a run told to answer again that ended
        // without one, its unit included: say what the turn ended on.
        let outcome = job.ended(n);
        if is_error(last) {
            out.push_str(&format!("\n(turn {n} ended in an error)\n"));
            match last.get("errors") {
                None | Some(serde_json::Value::Null) | Some(serde_json::Value::Bool(false)) => (),
                Some(errors) if view == View::Owner => out.push_str(&format!("{errors}\n")),
                Some(errors) => {
                    let clean = burrow_probe::scrub::independent_json(errors)
                        .unwrap_or_else(|_| serde_json::json!("[redacted]"));
                    out.push_str(&format!("{clean}\n"));
                }
            }
        } else if let Some(outcome) = outcome.filter(|o| !o.is_empty() && o != "done") {
            let outcome = view.plain(&outcome);
            out.push_str(&format!("\n(turn {n} then ended: {outcome})\n"));
        } else if read(&job.turn(n, "end")).as_deref() == Some("undeclared") {
            out.push_str(&format!(
                "\n(turn {n} ended without saying what comes next, even when asked again: nothing resumes the job but a reply)\n"
            ));
        }
        return Ok(out);
    }
    // No result yet, or none with anything to say: an error's can hold only
    // its errors.
    let mut out = if last.is_none() && job.turns().as_deref() == Some(n) {
        format!("({}; no answer yet)\n", job.state())
    } else {
        format!("(turn {n} ended without an answer)\n")
    };
    out.push_str(&crate::engine::stream_tail(&stream, view));
    Err(out)
}

/// What keeps `proj withdraw` from stopping a job, under its lock: that it's
/// running or over, which it prints and is settled, or that it can't be told.
/// Withdrawn only while it has no slot. A turn says it has one only under this
/// lock too ([`slot`]'s took_slot), so it can't take one between this look and
/// the stop; one that doesn't (N.withdrawable unsaid: its turn runs an older
/// proj, or hasn't reached the line) isn't stopped. Failing then, or when
/// systemd can't say (`active`, its unit's ActiveState), so the caller can ask
/// again.
fn left_be(job: &Job, n: &str, active: &str) -> Result<Option<&'static str>, Fail> {
    let id = job.id;
    // Checkpoint consumption takes the same job lock as withdrawal. Until
    // that commit, even an inactive transfer with stale slot files is queued.
    if job.retry_pending(n) {
        return Ok(None);
    }
    match active {
        "active" | "activating" | "reloading" => {}
        "inactive" | "failed" => return Ok(Some("over")),
        _ => {
            return Err(Fail::new(
                1,
                format!("proj: can't tell whether {id} still waits for a slot"),
            ));
        }
    }
    job.adopt();
    if job.turn(n, "slot").exists() {
        return Ok(Some("running"));
    }
    if !job.turn(n, "withdrawable").exists() {
        return Err(Fail::new(
            1,
            format!(
                "proj: {id}'s turn can't be withdrawn safely yet (it runs an older proj, or isn't in line yet)"
            ),
        ));
    }
    Ok(None)
}

/// `proj stop ID`: its running turn ended, and its waits. `proj withdraw ID`:
/// the same, only while its turn still waits for a slot ([`left_be`]), saying
/// what it did.
pub fn stop(cfg: &Config, id: &str, withdraw: bool) -> Result<(), Fail> {
    let job = Job::new(cfg, id);
    if !job.dir.is_dir() {
        return Err(Fail::new(1, format!("no job {id}")));
    }
    // Under the lock turns start under, so none starts between looking and
    // stopping. A stopped job is over: nothing it waited on resumes it.
    let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
    halt(cfg, &job, withdraw)
}

/// [`stop`]'s work, with the job's lock (the one turns start under) held by
/// the caller, who may have looked at the job under it first.
pub fn halt(cfg: &Config, job: &Job, withdraw: bool) -> Result<(), Fail> {
    let id = job.id;
    let unit = job.unit();
    let n = job.turns();
    if withdraw
        && let Some(said) = left_be(
            job,
            n.as_deref().unwrap_or_default(),
            &systemd::show(cfg, &unit, "ActiveState"),
        )?
    {
        put(&format!("{said}\n"));
        return Ok(());
    }
    // Marked first, so the turn's end already finds it stopped rather than
    // failed: nobody waits on it.
    if let Some(n) = &n {
        let _ = crate::state_reads::write(job.turn(n, "stopped"), "");
    }
    // No turn of it running, so no end of one to clear its scratch: this does.
    let idle = systemd::gone(cfg, &unit);
    let rc = if idle { 0 } else { systemd::stop(cfg, &unit) };
    // Its waits go whatever the stop said. A failed stop is an error unless
    // the turn is known to be over: one that ended on its own between the
    // look and the stop is stopped all the same, and one systemd can't be
    // asked about may still be running.
    for f in ["await", "await.next", "github-await"] {
        let _ = fs::remove_file(job.path(f));
    }
    if rc != 0 && !systemd::gone(cfg, &unit) {
        if let Some(n) = &n {
            let _ = fs::remove_file(job.turn(n, "stopped"));
        }
        rewatch(cfg, &[id]);
        return Err(Fail::new(
            u8::try_from(rc).unwrap_or(1),
            format!("proj: couldn't stop {id}"),
        ));
    }
    crate::policy::cancel(job).map_err(|e| Fail::new(1, e))?;
    if let Some(n) = &n {
        if job.turn(n, "retry-migration").exists() {
            crate::swap(&job.turn(n, "retry-cancelled"), "")
                .map_err(|e| Fail::new(1, e.to_string()))?;
        }
        fs::remove_file(job.turn(n, "retry-migration"))
            .or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            })
            .map_err(|e| Fail::new(1, e.to_string()))?;
    }
    crate::ask::clear(job);
    if crate::ask::host_path(job, "label").exists() {
        kick(cfg, ["proj-watch".into()]);
    }
    rewatch(cfg, &[id]);
    if crate::question::needs_watch(job) {
        kick(cfg, ["proj-watch".to_owned()]);
    }
    if let Err(e) = crate::question::notify(cfg, job) {
        eprintln!("proj: {e}");
    }
    // The end hook may have deferred while the checkpoint still existed.
    // Confirmed transfer cancellation owns finalization in either ordering.
    if n.as_ref()
        .is_some_and(|n| job.turn(n, "retry-cancelled").exists())
    {
        finish(cfg, id)?;
    } else if idle {
        scratch::clear(cfg, id);
    }
    if withdraw {
        put("withdrawn\n");
    }
    Ok(())
}

/// `proj unstick ID`: the job made one run --unstick from now on, its waits
/// told so.
pub fn unstick(cfg: &Config, id: &str) -> Result<(), Fail> {
    let job = Job::new(cfg, id);
    if !job.dir.is_dir() {
        return Err(Fail::new(1, format!("no job {id}")));
    }
    crate::state_reads::write(job.path("unstick"), "")
        .map_err(|e| io_fail(&job.path("unstick"), e))?;
    rewatch(cfg, &[id]);
    Ok(())
}

/// `proj await TARGET...`, from inside a job. The baseline is taken now, so
/// whatever changes from here on, even before the turn has ended, is news.
/// It's watched once the turn ends.
fn wait_on(cfg: &Config, targets: &[String]) -> Result<(), Fail> {
    let id = this_job(
        "PROJ_JOB",
        "proj await is for a proj job, about what it waits on",
    )?;
    if targets.is_empty() {
        return Err(Fail::new(2, "usage: proj await PR-OR-ISSUE-URL..."));
    }
    let mut found = Vec::new();
    for t in targets {
        found
            .push(target(t).ok_or_else(|| Fail::new(2, format!("not a GitHub PR or issue: {t}")))?);
    }
    record_wait(cfg, &Job::new(cfg, &id), &found, targets, None)
}

pub(crate) fn record_wait(
    cfg: &Config,
    job: &Job,
    found: &[(String, String)],
    targets: &[String],
    until: Option<Instant>,
) -> Result<(), Fail> {
    let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
    crate::github::take_limit();
    let pending = job.path(crate::github_quota::PENDING);
    let saved: Option<crate::github_quota::Pending> = crate::github_quota::state(&pending)?
        .map(|text| {
            serde_json::from_str::<crate::github_quota::Pending>(&text)
                .map_err(|e| Fail::new(1, format!("saved GitHub await: {e}")))
        })
        .transpose()?;
    let prior_due = saved.as_ref().map_or(0, |p| p.retry_at);
    let saved_baselines = saved
        .as_ref()
        .map(|p| p.baselines.clone())
        .unwrap_or_default();
    let captured =
        crate::github_quota::join_records(&crate::github_quota::captured(job)?, &saved_baselines);
    let known: Vec<_> = crate::job::awaits(&captured)
        .into_iter()
        .map(|a| a.target)
        .collect();
    let mut all = targets.to_vec();
    let mut found = found.to_vec();
    for saved_target in saved.map(|p| p.targets).unwrap_or_default() {
        if !all.contains(&saved_target) {
            found.push(
                target(&saved_target).ok_or_else(|| Fail::new(1, "invalid saved GitHub target"))?,
            );
            all.push(saved_target);
        }
    }
    let targets = &all;
    let mut limited_owner = String::new();
    let mut text = String::new();
    let result = found.iter().zip(targets).try_for_each(|(found, target)| {
        if known.contains(&format!("{}#{}", found.0, found.1)) {
            return Ok(());
        }
        limited_owner = found.0.split('/').next().unwrap_or_default().to_owned();
        text.push_str(&crate::github::for_job(job, || {
            baselines(
                cfg,
                std::slice::from_ref(found),
                std::slice::from_ref(target),
                until,
            )
        })?);
        Ok(())
    });
    match result {
        Ok(()) => {}
        Err(error) => {
            if let Some(due) = crate::github::take_limit() {
                let intent = crate::github_quota::Pending {
                    targets: targets.clone(),
                    retry_at: due.max(prior_due),
                    turn: job.turns().unwrap_or_default(),
                    baselines: crate::github_quota::join_records(&captured, &text),
                };
                crate::state_reads::write(
                    &pending,
                    serde_json::to_vec(&intent).expect("pending await"),
                )
                .map_err(|e| io_fail(&pending, e))?;
                crate::github_quota::remember(job, &text)?;
                crate::ask::declare(job, "await").map_err(|e| Fail::new(1, e))?;
                if let Err(e) = crate::github_quota::note(cfg, job, &limited_owner, due) {
                    eprintln!("proj: quota meter unavailable: {e}");
                }
                kick(cfg, ["proj-watch".into()]);
                put(
                    "Noted durably. GitHub quota is exhausted; proj will register the wait after its reset. End your turn now.\n",
                );
                return Ok(());
            }
            return Err(error);
        }
    };
    if until.is_some_and(|d| Instant::now() >= d) {
        return Err(Fail::new(
            1,
            "proj: turn deadline expired while registering waits; nothing is awaited",
        ));
    }
    crate::github_quota::remember(
        job,
        &crate::github_quota::join_records(&saved_baselines, &text),
    )?;
    let next = job.path("await.next");
    crate::state_reads::write(&next, crate::github_quota::captured(job)?)
        .map_err(|e| io_fail(&next, e))?;
    crate::ask::declare(job, "await").map_err(|e| Fail::new(1, e))?;
    let _ = fs::remove_file(job.path(crate::github_quota::PENDING));
    put(&format!(
        "Noted. End your turn now: this job is resumed once {} changes.\n",
        targets.join(" ")
    ));
    Ok(())
}

/// What to await, a line each: every baseline, or none. One GitHub can't give
/// (its token refused, the PR not there) fails the whole await, with gh's own
/// words, rather than leaving the job waiting on something proj never read.
/// So does one that's already merged or closed ([`over`]): the job checked it
/// before it merged, as a rule, and goes on to what comes after.
pub(crate) fn baselines(
    cfg: &Config,
    found: &[(String, String)],
    targets: &[String],
    until: Option<Instant>,
) -> Result<String, Fail> {
    let mut text = String::new();
    for ((repo, n), t) in found.iter().zip(targets) {
        let fp = fingerprint_until(cfg, repo, n, until).ok_or_else(|| {
            Fail::new(1, format!("proj: couldn't read {t} from GitHub (gh's error is above), so nothing is awaited: fix that and run proj await again, or end the turn with proj ask --default TEXT [--one-way]"))
        })?;
        if let Some(how) = over(&fp) {
            return Err(Fail::new(
                1,
                format!(
                    "proj: {t} is {how} already, so nothing is awaited: go on with what comes after it, then end the turn with proj await on what's still open, proj ask --default TEXT [--one-way] or proj done"
                ),
            ));
        }
        text += &format!("{repo}#{n}\t{}\t{fp}\n", now());
    }
    Ok(text)
}

const MERGE_USAGE: &str = "usage: proj merge-questions KEY JOB... (open questions about one subject: the newest keeps its notice)";

/// `proj ask` and `proj done`, from inside a job: how its turn ends, said
/// before it does. The last of proj await, ask and done stands; neither of
/// these waits on GitHub any longer.
fn ends(cfg: &Config, how: &str, args: &[String]) -> Result<(), Fail> {
    let id = this_job(
        "PROJ_JOB",
        &format!("proj {how} is for a proj job, about how its turn ends"),
    )?;
    let recommendation = if how == "ask" {
        Some(crate::ask::Recommendation::parse(args).map_err(|e| Fail::new(2, e))?)
    } else {
        None
    };
    if how == "done" && !args.is_empty() {
        return Err(Fail::new(
            2,
            format!("usage: proj {how} (nothing more: your answer says it)"),
        ));
    }
    let job = Job::new(cfg, &id);
    let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
    if let Some(recommendation) = recommendation {
        crate::ask::record(&job, &recommendation).map_err(|e| Fail::new(1, e))?;
    }
    put(declare_end(&job, how)?);
    Ok(())
}

/// Record the declaration and return the instruction the engine receives.
fn declare_end(job: &Job, how: &str) -> Result<&'static str, Fail> {
    let _ = fs::remove_file(job.path("await.next"));
    crate::ask::declare(job, how).map_err(|e| Fail::new(1, e))?;
    if how == "done" {
        let _ = fs::remove_file(job.path("open-question"));
    }
    Ok(if how == "ask" && job.path("open-question").exists() {
        "Noted. End your turn now without repeating the question already posted or pinging the owner again. The job is still waiting for their answer.\n"
    } else if how == "ask" {
        "Noted. End your turn now with your question: the job is resumed when someone answers it.\n"
    } else {
        "Noted. End your turn now with your answer.\n"
    })
}

/// After each turn, however it ended (proj-ended@ID, its unit's OnSuccess
/// and OnFailure): its wait is the stall watchdog's to know, what the job was started to kick (proj run
/// --kick) gets a pass now, and so does proj-watch for each repo the turn
/// awaits, which then sees at once anything that changed since the turn
/// took its baseline, and proj-inbox for a ready issue its drain holds
/// for a slot.
fn ended(cfg: &Config) -> Result<(), Fail> {
    let id = this_job(
        "PROJ_ENDED",
        "proj _ended is for the end of a job turn, run by proj-ended@ID",
    )?;
    // The unit has ended and its slot is free, however it ended (a stop or
    // an OOM kill included): news for the stream's subscribers.
    let done = finish(cfg, &id);
    crate::snapshot::stream::notify(cfg);
    done
}

pub(crate) fn finish(cfg: &Config, id: &str) -> Result<(), Fail> {
    let job = Job::new(cfg, id);
    if job
        .turns()
        .is_some_and(|n| job.turn(&n, "retry-migration").exists())
    {
        // The unit is gone, but its committed transfer is still pending.
        // Wake the filesystem consumer even when it failed after launch.
        if let Some(n) = job.turns() {
            crate::state_reads::write(job.turn(&n, "retry-wake"), "")
                .map_err(|e| Fail::new(1, e.to_string()))?;
        }
        return Ok(());
    }
    let cancelled = job
        .turns()
        .filter(|n| job.turn(n, "retry-cancelled").exists());
    let _finalizing = if let Some(n) = &cancelled {
        let lock = fs::File::create(job.turn(n, "retry-finalizing"))
            .map_err(|e| Fail::new(1, e.to_string()))?;
        lock.lock().map_err(|e| Fail::new(1, e.to_string()))?;
        if job.turn(n, "retry-finalized").exists() {
            return Ok(());
        }
        Some(lock)
    } else {
        None
    };
    if let Some(n) = job.turns()
        && !job.turn(&n, "stopped").exists()
        && !job.path("await").exists()
    {
        crate::github_quota::finish(&job, &n).map_err(|e| Fail::new(1, e))?;
    }
    job.mark_ended();
    crate::policy::wake(cfg);
    rewatch(cfg, &[id]);
    let mut names = lines(&job.path("kick"));
    if !matches!(crate::ask::load(&job), Ok(None)) || crate::ask::host_path(&job, "label").exists()
    {
        names.extend(["proj-watch".into(), "autoland".into()]);
    }
    if crate::question::needs_watch(&job) {
        names.push("proj-watch".into());
    }
    for a in awaits(&read(&job.path("await")).unwrap_or_default()) {
        names.push(watch::kick_for(&a.target));
    }
    // A ready issue waiting for a slot (proj-inbox's drain): whatever turn
    // this was, its slot is room now, and the issue's repo's pass looks.
    if let Some(inbox) = &cfg.inbox
        && let Some(repo) = read(&inbox.state.join(crate::inbox::WAITING))
    {
        names.push(format!("proj-inbox/{repo}"));
    }
    kick(cfg, names);
    // A job this turn left over has its scratch cleared, after the kicks so
    // nothing waits on it; its next turn (a reply) starts only once this has
    // ended, and makes its own afresh.
    if scratch::over(&job) {
        scratch::clear(cfg, id);
    }
    if let Some(n) = cancelled {
        crate::swap(&job.turn(&n, "retry-finalized"), "")
            .map_err(|e| Fail::new(1, e.to_string()))?;
    }
    // A resume may own the lock while waiting for this oneshot to finish.
    // The watcher kick above retains reconciliation when it cannot run here.
    let Some(_held) = job.try_lock().map_err(|e| io_fail(&job.path("lock"), e))? else {
        return Ok(());
    };
    crate::question::notify(cfg, &job).map_err(|e| Fail::new(1, e))?;
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod declaration_tests {
    use super::*;

    // Exercise the command dispatcher in a child so its job environment and
    // stdout are the real ones, without mutating the test runner's environment.
    pub(crate) fn response(cfg: &Config, id: &str, how: &str) -> String {
        let output = std::process::Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "cmd::declaration_tests::command_child",
                "--nocapture",
            ])
            .env("PROJ_TEST_END_JOBS", &cfg.jobs)
            .env("PROJ_TEST_END_HOW", how)
            .env("PROJ_JOB", id)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn command_child() {
        let Some(jobs) = env::var_os("PROJ_TEST_END_JOBS") else {
            return;
        };
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = jobs.into();
        let mut args = vec![env::var("PROJ_TEST_END_HOW").unwrap()];
        if args[0] == "ask" {
            args.extend(["--default".into(), "continue".into()]);
        }
        dispatch(&cfg, &args).unwrap();
    }
}
