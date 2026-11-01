//! `proj run` and `proj reply`: a job's first turn and its next ones, which
//! proj-inbox ([`crate::inbox`]) starts as these functions too.

use std::collections::hash_map::RandomState;
use std::fs::{self, DirBuilder, File};
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::process::Command;

use super::{arg, put};
use crate::config::Config;
use crate::job::Job;
use crate::pick::demoted;
use crate::turn::start;
use crate::{Fail, filled, io_fail, now};

/// A new job's settings: `proj run`'s flags.
#[derive(Clone, Debug, Default)]
pub struct New {
    pub vm: bool,
    pub first: bool,
    pub pipeline: bool,
    /// Every turn of it in the slot no one else's may take (slot::take).
    pub unstick: bool,
    /// Its starter acts on each turn's end itself.
    pub owned: bool,
    /// What gets a pass as each of its turns starts and ends.
    pub kicks: Vec<String>,
    /// Its turns' timeout and idle limit, as `proj run` takes them.
    pub limit: Option<String>,
    pub idle: Option<String>,
    /// What the job is about.
    pub link: Option<String>,
    /// The GitHub App it acts as.
    pub app: Option<String>,
    /// When every turn's place in line dates from (slot::entry).
    pub since: Option<String>,
    /// What its starter knows it by before it has its id (`proj find`).
    pub key: Option<String>,
}

/// A --key: what a starter names a job by before it has its id, so it can
/// find the job (`proj find`) whatever became of the id. Letters, digits
/// and `._/:-`, at most 200 of them.
pub fn key_name(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 200
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/:-".contains(&b))
}

/// The jobs under the key, oldest first: one, as a rule, since `proj run
/// --key` refuses a key a job has.
pub fn find(cfg: &Config, key: &str) -> Result<Vec<String>, Fail> {
    let mut ids: Vec<String> = match fs::read_dir(&cfg.jobs) {
        Ok(dir) => dir
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|id| !id.starts_with('.'))
            .filter(|id| {
                fs::read_to_string(Job::new(cfg, id).path("key")).is_ok_and(|k| k.trim() == key)
            })
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => vec![],
        Err(e) => return Err(io_fail(&cfg.jobs, e)),
    };
    ids.sort();
    Ok(ids)
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

/// A time proj run --since takes: Unix seconds, ten digits as every one has had
/// since 2001 and will until 2286, so its place's WHEN sorts with the rest, and
/// not after now.
fn since_time(t: &str, now: i64) -> bool {
    t.len() == 10
        && !t.starts_with('0')
        && t.bytes().all(|b| b.is_ascii_digit())
        && t.parse().is_ok_and(|t: i64| t <= now)
}

/// A --timeout or --idle: a number, then s, m, h or d.
fn duration(d: &str) -> bool {
    let digits = d.trim_end_matches(['s', 'm', 'h', 'd']);
    d.len() - digits.len() <= 1
        && !digits.is_empty()
        && !digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit())
}

/// A --link: https://..., with no space in it.
fn link(l: &str) -> bool {
    l.starts_with("https://") && l.len() > 8 && !l.contains(char::is_whitespace)
}

const KICK: &str = "proj run --kick HANDLER[/OWNER/REPO]";
const LINK: &str = "proj run --link URL (https://...)";
const APP: &str = "proj run --app NAME: one of this machine's GitHub Apps (burrow.github.apps)";
const SINCE: &str = "proj run --since TIME (Unix seconds, not after now)";
const KEY: &str = "proj run --key KEY (letters, digits and ._/:-, at most 200)";

/// `proj run` and `proj reply`.
pub(super) fn run(cfg: &Config, reply: bool, args: &[String]) -> Result<(), Fail> {
    let mut new = New::default();
    let mut cutoff = None;
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
                new.vm = true;
            }
            Some("--kick") => {
                only_run("--kick: the job's own are kept")?;
                let k = value
                    .filter(|k| kick_name(k))
                    .ok_or_else(|| Fail::new(2, KICK))?;
                new.kicks.push(k.to_owned());
                i += 1;
            }
            Some(flag @ ("--timeout" | "--idle")) => {
                only_run(&format!("{flag}: the job's own is kept"))?;
                let d = value.filter(|d| duration(d)).ok_or_else(|| {
                    Fail::new(
                        2,
                        format!("proj run {flag} DUR (a number, then s, m, h or d)"),
                    )
                })?;
                if flag == "--timeout" {
                    new.limit = Some(d.to_owned());
                } else {
                    new.idle = Some(d.to_owned());
                }
                i += 1;
            }
            Some("--owned") => {
                only_run("--owned: the job's own is kept")?;
                new.owned = true;
            }
            Some("--link") => {
                only_run("--link: the job's own is kept")?;
                // Empty is none.
                let l = value
                    .filter(|l| l.is_empty() || link(l))
                    .ok_or_else(|| Fail::new(2, LINK))?;
                new.link = Some(l.to_owned()).filter(|l| !l.is_empty());
                i += 1;
            }
            Some("--key") => {
                only_run("--key: the job's own is kept")?;
                let k = value
                    .filter(|k| key_name(k))
                    .ok_or_else(|| Fail::new(2, KEY))?;
                new.key = Some(k.to_owned());
                i += 1;
            }
            Some("--app") => {
                only_run("--app: the job's own is kept")?;
                let a = value
                    .filter(|a| cfg.app_env(a).is_some())
                    .ok_or_else(|| Fail::new(2, APP))?;
                new.app = Some(a.to_owned());
                i += 1;
            }
            Some("--since") => {
                only_run("--since: the job's own is kept")?;
                let t = value
                    .filter(|t| since_time(t, now()))
                    .ok_or_else(|| Fail::new(2, SINCE))?;
                new.since = Some(t.to_owned());
                i += 1;
            }
            Some("--first") => new.first = true,
            Some("--cut-off") => {
                let bad = || Fail::new(2, "proj reply --cut-off N ID \"message\"");
                let n = value
                    .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                    .ok_or_else(bad)?;
                if !reply {
                    return Err(bad());
                }
                cutoff = Some(n.to_owned());
                i += 1;
            }
            Some("--pipeline") => {
                only_run("--pipeline: the job's own class is kept")?;
                new.pipeline = true;
            }
            Some("--unstick") => {
                only_run("--unstick: the job's own slot is kept")?;
                new.unstick = true;
            }
            _ => break,
        }
        i += 1;
    }
    let id = if reply {
        let usage = "usage: proj reply [--first] [--cut-off N] ID \"message\"";
        let id = arg(args, i, usage)?;
        arg(args, i + 1, usage)?;
        self::reply(cfg, id, &args[i + 1], new.first, cutoff.as_deref())?;
        id.to_owned()
    } else {
        let usage = "usage: proj run [--vm] [--first] [--pipeline] [--unstick] [--timeout DUR] [--idle DUR] [--kick NAME]... [--owned] [--link URL] [--app NAME] [--since TIME] [--key KEY] PROJECT \"task\"";
        let p = arg(args, i, usage)?;
        arg(args, i + 1, usage)?;
        start_job(cfg, &new, p, &args[i + 1])?
    };
    put(&format!("{id}\n"));
    Ok(())
}

/// A new job in the project, its first turn started with the task: its id.
pub fn start_job(cfg: &Config, new: &New, p: &str, task: &str) -> Result<String, Fail> {
    let project = format!("{}/{p}", cfg.projects.display());
    if !fs::metadata(&project).is_ok_and(|m| m.is_dir()) {
        return Err(Fail::new(1, format!("no such project: {project}")));
    }
    // The flags' own checks, for a caller that didn't parse them.
    if !new.kicks.iter().all(|k| kick_name(k)) {
        return Err(Fail::new(2, KICK));
    }
    if !new.link.as_deref().is_none_or(link) {
        return Err(Fail::new(2, LINK));
    }
    if !new.app.as_deref().is_none_or(|a| cfg.app_env(a).is_some()) {
        return Err(Fail::new(2, APP));
    }
    if !new.since.as_deref().is_none_or(|t| since_time(t, now())) {
        return Err(Fail::new(2, SINCE));
    }
    for d in [&new.limit, &new.idle].into_iter().flatten() {
        if !duration(d) {
            return Err(Fail::new(
                2,
                format!("proj run: {d} isn't a duration (a number, then s, m, h or d)"),
            ));
        }
    }
    // Owned by what acts on each turn's end, which hears of it by its kick.
    if new.owned && new.kicks.is_empty() {
        return Err(Fail::new(
            2,
            "proj run --owned needs a --kick: what acts on each turn's end",
        ));
    }
    if let Some(k) = &new.key {
        if !key_name(k) {
            return Err(Fail::new(2, KEY));
        }
        // One job a key, so what it finds is that job.
        if let Some(id) = find(cfg, k)?.first() {
            return Err(Fail::new(1, format!("proj run: {id} has the key {k}")));
        }
    }
    if new.app.is_some() && new.vm {
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
    if !new.kicks.is_empty() {
        write(
            "kick",
            &new.kicks
                .iter()
                .map(|k| format!("{k}\n"))
                .collect::<String>(),
        )?;
    }
    if let Some(t) = &new.limit {
        write("timeout", &format!("{t}\n"))?;
    }
    if let Some(t) = &new.idle {
        write("idle", &format!("{t}\n"))?;
    }
    // Work on the pipeline holds the pipeline's slots only in its own
    // repos' projects (pick::class), and says so when it won't.
    if new.pipeline {
        write("pipeline", "")?;
        if let Some(why) = demoted(cfg, &job.dir) {
            eprintln!("proj: {id} {why}");
        }
    }
    if new.owned {
        write("owned", "")?;
    }
    if new.unstick {
        write("unstick", "")?;
    }
    if let Some(l) = &new.link {
        write("link", &format!("{l}\n"))?;
    }
    if let Some(t) = &new.since {
        write("since", &format!("{t}\n"))?;
    }
    if let Some(a) = &new.app {
        write("app", &format!("{a}\n"))?;
    }
    if let Some(k) = &new.key {
        write("key", &format!("{k}\n"))?;
    }
    if new.vm {
        let out = File::create(job.path("agent")).map_err(|e| io_fail(&job.path("agent"), e))?;
        let s = Command::new("agent")
            .args(["new", p])
            .stdout(out)
            .status()
            .map_err(|e| Fail::new(1, format!("proj: agent: {e}")))?;
        if !s.success() {
            return Err(Fail::quiet(s.code().unwrap_or(1) as u8));
        }
    }
    if let Err(e) = start(cfg, &id, task, new.first) {
        // A key names a job that started: its starter asks again.
        let _ = fs::remove_file(job.path("key"));
        return Err(Fail::new(1, e));
    }
    Ok(id)
}

/// The job's next turn, continuing its conversation with the message. With
/// `cutoff`, only if turn N was cut off: failing with 3 when there's nothing
/// to carry on, as against 1 for a turn that can't start.
pub fn reply(
    cfg: &Config,
    id: &str,
    msg: &str,
    first: bool,
    cutoff: Option<&str>,
) -> Result<(), Fail> {
    let job = Job::new(cfg, id);
    let no = if cutoff.is_some() { 3 } else { 1 };
    if !filled(&job.path("session")) {
        return Err(Fail::new(
            no,
            format!("{id} has no session to continue (yet)"),
        ));
    }
    // A job's turns start one at a time, and a look by proj-watch writes
    // back a wait only under the same lock.
    let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
    let st = job.state();
    if st == "queued" || st == "running" {
        return Err(Fail::new(no, format!("{id} is still {st}")));
    }
    if let Some(n) = cutoff
        && !job.cut_off(n)
    {
        return Err(Fail::new(3, format!("{id}'s turn {n} wasn't cut off")));
    }
    start(cfg, id, msg, first).map_err(|e| Fail::new(1, e))
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

#[cfg(test)]
mod tests {
    use super::{New, find, id_name, key_name, kick_name, since_time, start_job};

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
    fn since_times() {
        for ok in ["1792800000", "2000000000", "1000000000"] {
            assert!(since_time(ok, 2000000000), "{ok}");
        }
        for bad in [
            "",
            "0",
            "soon",
            "179280000",
            "17928000000",
            "0792800000",
            "179280000x",
            "-792800000",
            "2000000001",
        ] {
            assert!(!since_time(bad, 2000000000), "{bad}");
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

    #[test]
    fn keys() {
        for ok in ["upkeep/o/r/dashboard/1800000000.5", "a", "x:y-z_1"] {
            assert!(key_name(ok), "{ok}");
        }
        let long = "k".repeat(201);
        for bad in ["", "a b", "a\nb", "ü", long.as_str()] {
            assert!(!key_name(bad), "{bad}");
        }
    }

    // A key finds the jobs that have it, oldest first, and none in a jobs
    // dir that isn't there yet; its dotfiles are skipped.
    #[test]
    fn a_key_finds_its_job() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut v = crate::config::tests::example();
        v["jobs"] = tmp.path().join("jobs").to_str().unwrap().into();
        let cfg: crate::config::Config = serde_json::from_value(v).unwrap();
        assert_eq!(find(&cfg, "k").unwrap(), Vec::<String>::new());
        for (id, key) in [("b", "k\n"), ("a", "k\n"), ("c", "kk\n"), (".d", "k\n")] {
            std::fs::create_dir_all(tmp.path().join("jobs").join(id)).unwrap();
            std::fs::write(tmp.path().join("jobs").join(id).join("key"), key).unwrap();
        }
        std::fs::create_dir_all(tmp.path().join("jobs/e")).unwrap();
        assert_eq!(find(&cfg, "k").unwrap(), ["a", "b"]);
        assert_eq!(find(&cfg, "kk").unwrap(), ["c"]);
        assert!(find(&cfg, "x").unwrap().is_empty());
    }

    // A key a job has is refused, before any job is made.
    #[test]
    fn a_key_a_job_has_is_refused() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut v = crate::config::tests::example();
        v["jobs"] = tmp.path().join("jobs").to_str().unwrap().into();
        v["projects"] = tmp.path().join("projects").to_str().unwrap().into();
        let cfg: crate::config::Config = serde_json::from_value(v).unwrap();
        std::fs::create_dir_all(tmp.path().join("projects/p")).unwrap();
        std::fs::create_dir_all(tmp.path().join("jobs/p-1")).unwrap();
        std::fs::write(tmp.path().join("jobs/p-1/key"), "k\n").unwrap();
        let new = New {
            key: Some("k".into()),
            ..New::default()
        };
        let fail = start_job(&cfg, &new, "p", "task").unwrap_err();
        assert_eq!(
            (fail.code, fail.say.as_deref()),
            (1, Some("proj run: p-1 has the key k"))
        );
        let bad = New {
            key: Some("a b".into()),
            ..New::default()
        };
        assert_eq!(start_job(&cfg, &bad, "p", "task").unwrap_err().code, 2);
        assert_eq!(find(&cfg, "k").unwrap(), ["p-1"]);
    }
}
