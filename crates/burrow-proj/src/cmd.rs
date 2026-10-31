//! proj's commands, as the crate's docs list them.

use std::env;
use std::fs::{self, File};
use std::io::{self, Write};

use crate::config::Config;
use crate::github::{fingerprint, target};
use crate::job::{Job, answer, awaits, is_error, kick, result_of};
use crate::pick::demoted;
use crate::turn::{exec, lines};
use crate::waits::rewatch;
use crate::{Fail, io_fail, now, read, scratch, slot, systemd, watch};

mod run;

pub use run::{New, reply, start_job};

/// What a command prints: a reader that's gone (`proj status | head`) is no
/// failure of the command's.
fn put(text: &str) {
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
    match args.first().map(String::as_str).filter(|a| !a.is_empty()) {
        Some(cmd @ ("run" | "reply")) => run::run(cfg, cmd == "reply", &args[1..]),
        None | Some("status") if args.get(1).is_some_and(|a| a == "--line") => {
            put(&slot::in_line(cfg));
            Ok(())
        }
        None | Some("status") => status(cfg, args.get(1)),
        Some("log") => log(cfg, args),
        Some("state") => {
            let id = arg(args, 1, "usage: proj state ID")?;
            put(&format!("{}\n", Job::new(cfg, id).state()));
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
            put(&names.iter().map(|n| format!("{n}\n")).collect::<String>());
            Ok(())
        }
        Some("await") => wait_on(cfg, &args[1..]),
        Some(how @ ("ask" | "done")) => ends(cfg, how, args.len()),
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
            "usage: proj run|reply|status|log|state|stop|withdraw|unstick|list|await|ask|done",
        )),
    }
}

/// `proj status [N]`: the N jobs most recently changed, and how each is.
fn status(cfg: &Config, limit: Option<&String>) -> Result<(), Fail> {
    let limit: usize = match limit {
        None => 15,
        Some(n) => n
            .parse()
            .map_err(|_| Fail::new(1, format!("proj status: {n} isn't a number")))?,
    };
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
    for (_, id) in jobs.into_iter().take(limit) {
        let job = Job::new(cfg, &id);
        let st = job.state();
        let n = job.turns().unwrap_or_default();
        let each = |name: &str, f: &dyn Fn(&str) -> Option<String>| -> String {
            lines(&job.path(name))
                .iter()
                .filter_map(|l| f(l))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut notes = String::new();
        notes += &each("agent", &|l| Some(format!("[vm {l}] ")));
        notes += &each("app", &|l| Some(format!("[as {l}] ")));
        if let Some(why) = demoted(cfg, &job.dir) {
            notes += &format!("[{why}] ");
        }
        if st == "running" {
            notes += &each(&format!("{n}.wait"), &|l| {
                let what = l.split('\t').nth(1).unwrap_or(l);
                (!what.is_empty()).then(|| format!("[waiting in line: {what}] "))
            });
        }
        let targets: Vec<String> = awaits(&read(&job.path("await")).unwrap_or_default())
            .into_iter()
            .map(|a| a.target)
            .collect();
        if !targets.is_empty() {
            notes += &format!("[awaits {}] ", targets.join(" "));
        }
        notes += &each(&format!("{n}.end"), &|l| match l {
            "ask" => Some("[asks] ".into()),
            "undeclared" => Some("[no next step] ".into()),
            _ => None,
        });
        let result = fs::read(job.turn(&n, "result")).unwrap_or_default();
        let mut cut = result.len().min(100);
        while cut > 0 && std::str::from_utf8(&result[..cut]).is_err() {
            cut -= 1;
        }
        notes += &String::from_utf8_lossy(&result[..cut]).replace('\n', " ");
        put(&format!("{id:<40} {st:<8} {notes}\n"));
    }
    Ok(())
}

/// `proj log ID [--turn N] [--raw]`: a turn's answer, and how it ended.
fn log(cfg: &Config, args: &[String]) -> Result<(), Fail> {
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
        let text = fs::read(&stream).map_err(|e| io_fail(&stream, e))?;
        let _ = io::stdout().lock().write_all(&text);
        return Ok(());
    }
    match told(cfg, id, &n) {
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
    let job = Job::new(cfg, id);
    // A turn's answer is read from its own stream, so an earlier turn's is
    // still there once later ones have started.
    let stream = job.turn(n, "jsonl");
    let text = String::from_utf8_lossy(&fs::read(&stream).unwrap_or_default()).into_owned();
    let last = text.lines().rev().find_map(result_of);
    let said = Some(answer(&stream)).filter(|a| !a.is_empty());
    if let (Some(last), Some(said)) = (&last, said) {
        let mut out = format!("{said}\n");
        // An answer, then an error, or a run told to answer again that ended
        // without one, its unit included: say what the turn ended on.
        let outcome = job.ended(n);
        if is_error(last) {
            out.push_str(&format!("\n(turn {n} ended in an error)\n"));
            match last.get("errors") {
                None | Some(serde_json::Value::Null) | Some(serde_json::Value::Bool(false)) => (),
                Some(errors) => out.push_str(&format!("{errors}\n")),
            }
        } else if let Some(outcome) = outcome.filter(|o| !o.is_empty() && o != "done") {
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
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    out.push_str(&lines[lines.len().saturating_sub(3)..].concat());
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
    let unit = job.unit();
    let n = job.turns();
    if withdraw
        && let Some(said) = left_be(
            &job,
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
        let _ = fs::write(job.turn(n, "stopped"), "");
    }
    // No turn of it running, so no end of one to clear its scratch: this does.
    let idle = systemd::gone(cfg, &unit);
    let rc = if idle { 0 } else { systemd::stop(cfg, &unit) };
    // Its waits go whatever the stop said. A failed stop is an error unless
    // the turn is known to be over: one that ended on its own between the
    // look and the stop is stopped all the same, and one systemd can't be
    // asked about may still be running.
    for f in ["await", "await.next"] {
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
    rewatch(cfg, &[id]);
    if idle {
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
    fs::write(job.path("unstick"), "").map_err(|e| io_fail(&job.path("unstick"), e))?;
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
    let job = Job::new(cfg, &id);
    let text = baselines(cfg, &found, targets)?;
    let next = job.path("await.next");
    File::options()
        .append(true)
        .create(true)
        .open(&next)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| io_fail(&next, e))?;
    fs::write(job.path("end.next"), "await\n").map_err(|e| io_fail(&job.path("end.next"), e))?;
    put(&format!(
        "Noted. End your turn now: this job is resumed once {} changes.\n",
        targets.join(" ")
    ));
    Ok(())
}

/// What to await, a line each: every baseline, or none. One GitHub can't give
/// (its token refused, the PR not there) fails the whole await, with gh's own
/// words, rather than leaving the job waiting on something proj never read.
fn baselines(cfg: &Config, found: &[(String, String)], targets: &[String]) -> Result<String, Fail> {
    let mut text = String::new();
    for ((repo, n), t) in found.iter().zip(targets) {
        let fp = fingerprint(cfg, repo, n).ok_or_else(|| {
            Fail::new(1, format!("proj: couldn't read {t} from GitHub (gh's error is above), so nothing is awaited: fix that and run proj await again, or end the turn with proj ask"))
        })?;
        text += &format!("{repo}#{n}\t{}\t{fp}\n", now());
    }
    Ok(text)
}

/// `proj ask` and `proj done`, from inside a job: how its turn ends, said
/// before it does. The last of proj await, ask and done stands; neither of
/// these waits on GitHub any longer.
fn ends(cfg: &Config, how: &str, words: usize) -> Result<(), Fail> {
    let id = this_job(
        "PROJ_JOB",
        &format!("proj {how} is for a proj job, about how its turn ends"),
    )?;
    if words != 1 {
        return Err(Fail::new(
            2,
            format!("usage: proj {how} (nothing more: your answer says it)"),
        ));
    }
    let job = Job::new(cfg, &id);
    let _ = fs::remove_file(job.path("await.next"));
    fs::write(job.path("end.next"), format!("{how}\n"))
        .map_err(|e| io_fail(&job.path("end.next"), e))?;
    put(if how == "ask" {
        "Noted. End your turn now with your question: the job is resumed when someone answers it.\n"
    } else {
        "Noted. End your turn now with your answer.\n"
    });
    Ok(())
}

/// After each turn, however it ended (proj-ended@ID, its unit's OnSuccess
/// and OnFailure): its wait is the stall watchdog's to know, what the job was started to kick (proj run
/// --kick) gets a pass now, and so does proj-watch for each repo the turn
/// awaits, which then sees at once anything that changed since the turn
/// took its baseline.
fn ended(cfg: &Config) -> Result<(), Fail> {
    let id = this_job(
        "PROJ_ENDED",
        "proj _ended is for the end of a job turn, run by proj-ended@ID",
    )?;
    let job = Job::new(cfg, &id);
    job.mark_ended();
    rewatch(cfg, &[&id]);
    let mut names = lines(&job.path("kick"));
    for a in awaits(&read(&job.path("await")).unwrap_or_default()) {
        names.push(format!(
            "proj-watch/{}",
            a.target.split('#').next().unwrap_or_default()
        ));
    }
    kick(cfg, names);
    // A job this turn left over has its scratch cleared, after the kicks so
    // nothing waits on it; its next turn (a reply) starts only once this has
    // ended, and makes its own afresh.
    if scratch::over(&job) {
        scratch::clear(cfg, &id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{baselines, left_be};
    use crate::config::Config;
    use crate::job::Job;

    // proj withdraw stops only a turn that says it may be, and has no slot: one
    // that has started or ended it leaves be, saying so, and one from an older
    // proj, or one systemd can't say about, it fails on, to be asked about again.
    #[test]
    fn what_withdraw_leaves_be() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut v = crate::config::tests::example();
        v["jobs"] = tmp.path().to_str().unwrap().into();
        let cfg: Config = serde_json::from_value(v).unwrap();
        fs::create_dir(tmp.path().join("j")).unwrap();
        fs::write(tmp.path().join("j/turns"), "1\n").unwrap();
        let job = Job::new(&cfg, "j");
        let said = |active| left_be(&job, "1", active).map_err(|f| f.code);

        for active in ["inactive", "failed"] {
            assert_eq!(said(active), Ok(Some("over")), "{active}");
        }
        for active in ["", "deactivating", "maintenance"] {
            assert_eq!(said(active), Err(1), "{active:?}");
        }
        assert_eq!(
            said("active"),
            Err(1),
            "unsaid: it isn't known to take its slot locked"
        );
        fs::write(job.turn("1", "withdrawable"), "").unwrap();
        for active in ["active", "activating", "reloading"] {
            assert_eq!(said(active), Ok(None), "{active}");
        }
        fs::write(job.turn("1", "slot"), "2\n").unwrap();
        assert_eq!(said("active"), Ok(Some("running")));
    }

    // A baseline GitHub can't give fails the whole await, and writes nothing.
    #[test]
    fn an_await_needs_every_baseline() {
        let mut v = crate::config::tests::example();
        v["tools"]["gh"] = "/nowhere/gh".into();
        let cfg: Config = serde_json::from_value(v).unwrap();
        let found = [("o/r".to_owned(), "5".to_owned())];
        let fail = baselines(&cfg, &found, &["o/r#5".to_owned()]).unwrap_err();
        assert_eq!(fail.code, 1);
        assert!(
            fail.say
                .unwrap()
                .contains("couldn't read o/r#5 from GitHub")
        );
    }
}
