//! proj's commands, as the crate's docs list them.

use std::collections::hash_map::RandomState;
use std::env;
use std::fs::{self, DirBuilder, File};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::os::unix::fs::DirBuilderExt;
use std::process::Command;

use crate::config::Config;
use crate::github::{fingerprint, target};
use crate::job::{Job, answer, awaits, is_error, kick};
use crate::pick::demoted;
use crate::turn::{exec, lines, start};
use crate::waits::rewatch;
use crate::{Fail, filled, io_fail, now, read, systemd, watch};

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
        Some(cmd @ ("run" | "reply")) => run(cfg, cmd == "reply", &args[1..]),
        None | Some("status") => status(cfg, args.get(1)),
        Some("log") => log(cfg, args),
        Some("state") => {
            let id = arg(args, 1, "usage: proj state ID")?;
            put(&format!("{}\n", Job::new(cfg, id).state()));
            Ok(())
        }
        Some("stop") => stop(cfg, arg(args, 1, "usage: proj stop ID")?),
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
        Some("_exec") => exec(
            cfg,
            arg(args, 1, "usage: proj _exec ID first|normal")?,
            args.get(2).map_or("", String::as_str),
        ),
        Some(_) => Err(Fail::new(
            2,
            "usage: proj run|reply|status|log|state|stop|list|await|ask|done",
        )),
    }
}

/// A --kick: HANDLER or HANDLER/OWNER/REPO.
fn kick_name(k: &str) -> bool {
    let part = |s: &str, ok: fn(u8) -> bool| !s.is_empty() && s.bytes().all(ok);
    let handler = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-';
    let name = |b: u8| b.is_ascii_alphanumeric() || b"_.-".contains(&b);
    match k.split('/').collect::<Vec<_>>()[..] {
        [h] => part(h, handler),
        [h, owner, repo] => part(h, handler) && part(owner, name) && part(repo, name),
        _ => false,
    }
}

/// `proj run` and `proj reply`.
fn run(cfg: &Config, reply: bool, args: &[String]) -> Result<(), Fail> {
    let (mut vm, mut first, mut pipeline, mut owned) = (false, false, false, false);
    let (mut kicks, mut limit, mut idle, mut link, mut app) = (Vec::new(), None, None, None, None);
    let mut i = 0;
    let only_run = |what: &str| -> Result<(), Fail> {
        if reply {
            Err(Fail::new(2, format!("proj reply has no {what}")))
        } else {
            Ok(())
        }
    };
    loop {
        let value = args.get(i + 1).map(String::as_str);
        match args.get(i).map(String::as_str) {
            Some("--vm") => {
                only_run("--vm")?;
                vm = true;
            }
            Some("--kick") => {
                only_run("--kick: the job's own are kept")?;
                let k = value
                    .filter(|k| kick_name(k))
                    .ok_or_else(|| Fail::new(2, "proj run --kick HANDLER[/OWNER/REPO]"))?;
                kicks.push(k.to_owned());
                i += 1;
            }
            Some(flag @ ("--timeout" | "--idle")) => {
                only_run(&format!("{flag}: the job's own is kept"))?;
                let d = value
                    .filter(|d| {
                        let digits = d.trim_end_matches(['s', 'm', 'h', 'd']);
                        d.len() - digits.len() <= 1
                            && !digits.is_empty()
                            && !digits.starts_with('0')
                            && digits.bytes().all(|b| b.is_ascii_digit())
                    })
                    .ok_or_else(|| {
                        Fail::new(
                            2,
                            format!("proj run {flag} DUR (a number, then s, m, h or d)"),
                        )
                    })?;
                if flag == "--timeout" {
                    limit = Some(d.to_owned());
                } else {
                    idle = Some(d.to_owned());
                }
                i += 1;
            }
            Some("--owned") => {
                only_run("--owned: the job's own is kept")?;
                owned = true;
            }
            Some("--link") => {
                only_run("--link: the job's own is kept")?;
                // Empty is none.
                let l = value
                    .filter(|l| {
                        l.is_empty()
                            || l.starts_with("https://")
                                && l.len() > 8
                                && !l.contains(char::is_whitespace)
                    })
                    .ok_or_else(|| Fail::new(2, "proj run --link URL (https://...)"))?;
                link = Some(l.to_owned()).filter(|l| !l.is_empty());
                i += 1;
            }
            Some("--app") => {
                only_run("--app: the job's own is kept")?;
                let a = value
                    .filter(|a| cfg.app_env(a).is_some())
                    .ok_or_else(|| Fail::new(2, "proj run --app NAME: one of this machine's GitHub Apps (burrow.github.apps)"))?;
                app = Some(a.to_owned());
                i += 1;
            }
            Some("--first") => first = true,
            Some("--pipeline") => {
                only_run("--pipeline: the job's own class is kept")?;
                pipeline = true;
            }
            _ => break,
        }
        i += 1;
    }
    let id = if !reply {
        let usage = "usage: proj run [--vm] [--first] [--pipeline] [--timeout DUR] [--idle DUR] [--kick NAME]... [--owned] [--link URL] [--app NAME] PROJECT \"task\"";
        let p = arg(args, i, usage)?;
        arg(args, i + 1, usage)?;
        let project = format!("{}/{p}", cfg.projects.display());
        if !fs::metadata(&project).is_ok_and(|m| m.is_dir()) {
            return Err(Fail::new(1, format!("no such project: {project}")));
        }
        // Owned by what acts on each turn's end, which hears of it by its kick.
        if owned && kicks.is_empty() {
            return Err(Fail::new(
                2,
                "proj run --owned needs a --kick: what acts on each turn's end",
            ));
        }
        if app.is_some() && vm {
            return Err(Fail::new(
                2,
                "proj run --app with --vm: a sandbox has no GitHub login to act with",
            ));
        }
        let id = new_job(cfg, p)?;
        let job = Job::new(cfg, &id);
        let write = |name: &str, text: &str| {
            fs::write(job.path(name), text).map_err(|e| io_fail(&job.path(name), e))
        };
        write("project", &format!("{project}\n"))?;
        if !kicks.is_empty() {
            write(
                "kick",
                &kicks.iter().map(|k| format!("{k}\n")).collect::<String>(),
            )?;
        }
        if let Some(t) = &limit {
            write("timeout", &format!("{t}\n"))?;
        }
        if let Some(t) = &idle {
            write("idle", &format!("{t}\n"))?;
        }
        // Work on the pipeline holds the pipeline's slots only in its own
        // repos' projects (pick::class), and says so when it won't.
        if pipeline {
            write("pipeline", "")?;
            if let Some(why) = demoted(cfg, &job.dir) {
                eprintln!("proj: {id} {why}");
            }
        }
        if owned {
            write("owned", "")?;
        }
        if let Some(l) = &link {
            write("link", &format!("{l}\n"))?;
        }
        if let Some(a) = &app {
            write("app", &format!("{a}\n"))?;
        }
        if vm {
            let out =
                File::create(job.path("agent")).map_err(|e| io_fail(&job.path("agent"), e))?;
            let s = Command::new("agent")
                .args(["new", p])
                .stdout(out)
                .status()
                .map_err(|e| Fail::new(1, format!("proj: agent: {e}")))?;
            if !s.success() {
                return Err(Fail::quiet(s.code().unwrap_or(1) as u8));
            }
        }
        id
    } else {
        let usage = "usage: proj reply [--first] ID \"message\"";
        let id = arg(args, i, usage)?.to_owned();
        arg(args, i + 1, usage)?;
        id
    };
    let task = &args[i + 1];
    let job = Job::new(cfg, &id);
    // A job's turns start one at a time, and a look by proj-watch writes
    // back a wait only under the same lock, held until proj exits.
    let _held = if reply {
        if !filled(&job.path("session")) {
            return Err(Fail::new(
                1,
                format!("{id} has no session to continue (yet)"),
            ));
        }
        let held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
        let st = job.state();
        if st == "queued" || st == "running" {
            return Err(Fail::new(1, format!("{id} is still {st}")));
        }
        Some(held)
    } else {
        None
    };
    start(cfg, &id, task, first).map_err(|e| Fail::new(1, e))?;
    put(&format!("{id}\n"));
    Ok(())
}

/// A new job's dir, named for its project and when it started, unique even
/// when two start in the same second: its id.
fn new_job(cfg: &Config, project: &str) -> Result<String, Fail> {
    fs::create_dir_all(&cfg.jobs).map_err(|e| io_fail(&cfg.jobs, e))?;
    let name = id_name(project);
    let when = jiff::Zoned::now().strftime("%Y%m%d-%H%M%S").to_string();
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let random = RandomState::new();
    for attempt in 0u64.. {
        let mut h = random.build_hasher();
        h.write_u64(attempt);
        let mut bits = h.finish();
        let tag: String = (0..4)
            .map(|_| {
                let c = CHARS[(bits % CHARS.len() as u64) as usize] as char;
                bits /= CHARS.len() as u64;
                c
            })
            .collect();
        let id = format!("{name}-{when}-{tag}");
        match DirBuilder::new().mode(0o700).create(cfg.jobs.join(&id)) {
            Ok(()) => return Ok(id),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempt < 1000 => continue,
            Err(e) => return Err(io_fail(&cfg.jobs.join(&id), e)),
        }
    }
    unreachable!()
}

/// What a job's id starts with: its project's name, never hidden or
/// option-like whatever the project is called (.github, .-x). The jobs dir's
/// dotfiles are its own state, what goes through the jobs skips them, and
/// systemd-escape takes a leading hyphen for an option. The job's path stays
/// in its project file.
fn id_name(project: &str) -> String {
    if project.starts_with(['.', '-']) {
        format!("dot{project}")
    } else {
        project.to_owned()
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
    // A turn's answer is read from its own stream, so an earlier turn's is
    // still there once later ones have started.
    let stream = job.turn(&n, "jsonl");
    let text = fs::read(&stream);
    if raw {
        let text = text.map_err(|e| io_fail(&stream, e))?;
        let _ = io::stdout().lock().write_all(&text);
        return Ok(());
    }
    let text = String::from_utf8_lossy(&text.unwrap_or_default()).into_owned();
    let last = text.lines().rfind(|l| l.contains(r#""type":"result""#));
    let said = answer(&stream).filter(|a| !a.is_empty());
    if let (Some(last), Some(said)) = (last, said) {
        put(&format!("{said}\n"));
        // An answer, then an error, or a run told to answer again that ended
        // without one, its unit included: say what the turn ended on.
        let outcome = job.ended(&n);
        let last: Option<serde_json::Value> = serde_json::from_str(last).ok();
        if let Some(last) = last.filter(is_error) {
            put(&format!("\n(turn {n} ended in an error)\n"));
            match last.get("errors") {
                None | Some(serde_json::Value::Null) | Some(serde_json::Value::Bool(false)) => (),
                Some(errors) => put(&format!("{errors}\n")),
            }
        } else if let Some(outcome) = outcome.filter(|o| !o.is_empty() && o != "done") {
            put(&format!("\n(turn {n} then ended: {outcome})\n"));
        } else if read(&job.turn(&n, "end")).as_deref() == Some("undeclared") {
            put(&format!(
                "\n(turn {n} ended without saying what comes next, even when asked again: nothing resumes the job but a reply)\n"
            ));
        }
        return Ok(());
    }
    // No result yet, or none with anything to say: an error's can hold only
    // its errors.
    if last.is_none() && job.turns().as_deref() == Some(n.as_str()) {
        put(&format!("({}; no answer yet)\n", job.state()));
    } else {
        put(&format!("(turn {n} ended without an answer)\n"));
    }
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    put(&lines[lines.len().saturating_sub(3)..].concat());
    Err(Fail::quiet(1))
}

/// `proj stop ID`: its running turn ended, and its waits.
fn stop(cfg: &Config, id: &str) -> Result<(), Fail> {
    let job = Job::new(cfg, id);
    if !job.dir.is_dir() {
        return Err(Fail::new(1, format!("no job {id}")));
    }
    // Under the lock turns start under, so none starts between looking and
    // stopping. A stopped job is over: nothing it waited on resumes it.
    let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
    let unit = job.unit();
    // Marked first, so the turn's end already finds it stopped rather than
    // failed: nobody waits on it.
    let n = job.turns();
    if let Some(n) = &n {
        let _ = fs::write(job.turn(n, "stopped"), "");
    }
    let rc = if systemd::gone(cfg, &unit) {
        0
    } else {
        systemd::stop(cfg, &unit)
    };
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
    let mut text = String::new();
    for (repo, n) in found {
        // No fingerprint (GitHub can't be asked just now): _watch takes it
        // later.
        let fp = fingerprint(cfg, &repo, &n)
            .map(|f| f.to_string())
            .unwrap_or_default();
        text += &format!("{repo}#{n}\t{}\t{fp}\n", now());
    }
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
    rewatch(cfg, &[&id]);
    let job = Job::new(cfg, &id);
    let mut names = lines(&job.path("kick"));
    for a in awaits(&read(&job.path("await")).unwrap_or_default()) {
        names.push(format!(
            "proj-watch/{}",
            a.target.split('#').next().unwrap_or_default()
        ));
    }
    kick(cfg, names);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{id_name, kick_name};

    #[test]
    fn id_names() {
        for (project, name) in [
            ("web", "web"),
            ("my.repo", "my.repo"),
            (".github", "dot.github"),
            ("..x", "dot..x"),
            (".-x", "dot.-x"),
            ("-x", "dot-x"),
        ] {
            assert_eq!(id_name(project), name, "{project}");
        }
    }

    #[test]
    fn kicks() {
        for ok in [
            "autoland",
            "proj-watch",
            "autoland/o/r",
            "proj-inbox/Sam/my.repo_x-1",
        ] {
            assert!(kick_name(ok), "{ok}");
        }
        for bad in ["", "Autoland", "a/b", "a/b/c/d", "a//c", "a/b/", "a b"] {
            assert!(!kick_name(bad), "{bad}");
        }
    }
}
