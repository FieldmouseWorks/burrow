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
use crate::systemd;
use crate::turn::start;
use crate::{Fail, filled, io_fail, now};

/// A new job's settings: `proj run`'s flags.
#[derive(Clone, Debug, Default)]
pub struct New {
    /// A worker enrolled by the inbox, whose host consumes split declarations.
    pub inbox_issue: bool,
    /// Verified source kept beside a bounded planning prompt, before start.
    pub task_source: Option<String>,
    /// Structured inbox attribution, without its prompt or title.
    pub usage_kind: Option<String>,
    pub local_work: bool,
    pub admission: Option<crate::policy::Identity>,
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
    /// Inbox label choice, kept for every turn.
    pub engine: Option<crate::engine::Engine>,
    /// The kind of work it is, which picks its engine (Config::roles).
    pub role: Option<String>,
}

/// A --key: what a starter names a job by before it has its id, so it can
/// find the job (`proj find`) whatever became of the id. Letters, digits
/// and `._/:-`, at most 200 of them, starting with a letter or digit.
pub fn key_name(k: &str) -> bool {
    k.len() <= 200
        && k.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/:-".contains(&b))
}

/// Where a key is held: a link, in the jobs dir's `.keys`, to the job
/// holding it, made at once or not at all, so two starters can't both
/// take it. Its name is the key's, `/` as `+`, which no key has.
fn key_path(cfg: &Config, key: &str) -> std::path::PathBuf {
    cfg.jobs.join(".keys").join(key.replace('/', "+"))
}

/// A key's own lock, in `.keys/.locks` (no key's link starts with a dot),
/// held while its link is read and judged, let go or taken: so a link a
/// starter has just taken over is never let go on a judgment of the job it
/// pointed at before.
fn key_lock(cfg: &Config, key: &str) -> Result<File, Fail> {
    let dir = cfg.jobs.join(".keys").join(".locks");
    fs::create_dir_all(&dir).map_err(|e| io_fail(&dir, e))?;
    let path = dir.join(key.replace('/', "+"));
    let f = File::options()
        .append(true)
        .create(true)
        .open(&path)
        .map_err(|e| io_fail(&path, e))?;
    f.lock().map_err(|e| io_fail(&path, e))?;
    Ok(f)
}

/// The job holding the key, once it has started a turn. One that never
/// did (its first turn or its sandbox failed to start) lets the key go,
/// so it can be asked for again. That's judged under the key's lock and
/// the job's, which its starter holds until it has started, or failed to:
/// a start under way is waited for, never taken for one that failed. What
/// can't be read fails: absence is only ever what was read.
pub fn find(cfg: &Config, key: &str) -> Result<Option<String>, Fail> {
    let path = key_path(cfg, key);
    // No link, nothing to judge: a key no job has.
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_fail(&path, e)),
    }
    let _key = key_lock(cfg, key)?;
    find_held(cfg, key)
}

/// `find`, with the key's lock held.
fn find_held(cfg: &Config, key: &str) -> Result<Option<String>, Fail> {
    let path = key_path(cfg, key);
    let id = match fs::read_link(&path) {
        Ok(id) => id.to_string_lossy().into_owned(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_fail(&path, e)),
    };
    let job = Job::new(cfg, &id);
    let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
    if job.turns().is_some() {
        return Ok(Some(id));
    }
    // A turn whose start read as failed may have started all the same.
    match systemd::show(cfg, &job.unit(), "LoadState").as_str() {
        "not-found" => {}
        "loaded" => return Ok(Some(id)),
        other => {
            return Err(Fail::new(
                1,
                format!(
                    "proj find: can't tell whether {id} started (its unit's LoadState: {other:?})"
                ),
            ));
        }
    }
    match fs::remove_file(&path) {
        Ok(()) => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_fail(&path, e)),
    }
}

/// The key taken for the new job `id`, or the job that has it.
fn take_key(cfg: &Config, key: &str, id: &str) -> Result<(), Fail> {
    let path = key_path(cfg, key);
    let _key = key_lock(cfg, key)?;
    match std::os::unix::fs::symlink(id, &path) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(io_fail(&path, e)),
    }
    // One that never started lets it go, and, under the key's lock, it's
    // this job's before anything else can judge or take it.
    if let Some(had) = find_held(cfg, key)? {
        return Err(Fail::new(1, format!("proj run: {had} has the key {key}")));
    }
    std::os::unix::fs::symlink(id, &path).map_err(|e| io_fail(&path, e))
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
    run_with_cgroup(cfg, reply, args, || fs::read_to_string("/proc/self/cgroup"))
}

fn run_with_cgroup(
    cfg: &Config,
    reply: bool,
    args: &[String],
    cgroup: impl FnOnce() -> io::Result<String>,
) -> Result<(), Fail> {
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
            Some("--role") => {
                only_run("--role: the job's own is kept")?;
                let r = value
                    .filter(|r| {
                        !r.is_empty()
                            && r.len() <= 40
                            && r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    })
                    .ok_or_else(|| Fail::new(2, "proj run --role NAME (letters, digits and -)"))?;
                new.role = Some(r.to_owned());
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
            Some("--local-work") => {
                only_run("--local-work")?;
                new.local_work = true;
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
        if let Some(n) = cutoff.as_deref() {
            self::reply(cfg, id, &args[i + 1], new.first, Some(n))?;
        } else {
            reply_answer(cfg, id, &args[i + 1], new.first)?;
        }
        id.to_owned()
    } else {
        let usage = "usage: proj run [--vm] [--first] [--pipeline] [--unstick] [--timeout DUR] [--idle DUR] [--kick NAME]... [--owned] [--link URL] [--app NAME] [--since TIME] [--key KEY] [--role NAME] PROJECT \"task\"";
        let p = arg(args, i, usage)?;
        arg(args, i + 1, usage)?;
        super::starter::check(new.role.as_deref(), cgroup)?;
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
    if !new.key.as_deref().is_none_or(key_name) {
        return Err(Fail::new(2, KEY));
    }
    if new.app.is_some() && new.vm {
        return Err(Fail::new(
            2,
            "proj run --app with --vm: a sandbox has no GitHub login to act with",
        ));
    }
    let id = new_job(cfg, p)?;
    let job = Job::new(cfg, &id);
    // Held until its first turn has started or failed to, so what finds it
    // by its key never judges it halfway (find).
    let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
    if let Some(k) = &new.key
        && let Err(e) = take_key(cfg, k, &id)
    {
        let _ = fs::remove_dir_all(&job.dir);
        return Err(e);
    }
    let write = |name: &str, text: &str| {
        fs::write(job.path(name), text).map_err(|e| io_fail(&job.path(name), e))
    };
    write("project", &format!("{project}\n"))?;
    if new.inbox_issue {
        write("inbox-issue", "true\n")?;
    }
    let parent = std::env::var("PROJ_JOB").ok().filter(|s| !s.is_empty());
    // A launch from inside a job inherits that job's automation: only a
    // parent bound as manual can start manual work.
    let automated = new.role.is_some()
        || new.usage_kind.is_some()
        || parent
            .as_deref()
            .is_some_and(|p| crate::policy::automated_parent(cfg, p));
    let identity = match &new.admission {
        Some(identity) => identity.clone(),
        None => crate::policy::identify(cfg, &project, automated, new.local_work)
            .map_err(|e| Fail::new(1, e))?,
    };
    if let Some(kind) = &new.usage_kind {
        write("usage-kind", kind)?;
    }
    let starter = fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
    write(
        "starter",
        parent
            .as_deref()
            .or_else(|| {
                starter
                    .lines()
                    .find_map(|l| l.rsplit('/').next())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("unknown"),
    )?;
    if !new.kicks.is_empty() {
        write(
            "kick",
            &new.kicks
                .iter()
                .map(|k| format!("{k}\n"))
                .collect::<String>(),
        )?;
    }
    if let Some(engine) = new.engine {
        write("engine", engine.name())?;
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
    crate::ask::initialize(&job, new.link.as_deref()).map_err(|e| Fail::new(1, e))?;
    if let Some(l) = &new.link {
        write("link", &format!("{l}\n"))?;
    }
    if let Some(t) = &new.since {
        write("since", &format!("{t}\n"))?;
    }
    if let Some(a) = &new.app {
        write("app", &format!("{a}\n"))?;
    }
    if let Some(r) = &new.role {
        write("role", &format!("{r}\n"))?;
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
    let given = if let Some(source) = &new.task_source {
        write("task-source", source)?;
        task.replace(
            "@PROJ_TASK_SOURCE@",
            &job.path("task-source").to_string_lossy(),
        )
    } else {
        task.to_owned()
    };
    crate::policy::bind(
        &job,
        identity,
        crate::policy::kind(new.role.as_deref(), new.usage_kind.is_some() || automated),
    )
    .map_err(|e| Fail::new(1, e))?;
    start(cfg, &id, &given, new.first).map_err(|e| Fail::new(1, e))?;
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
    resume(cfg, id, msg, first, cutoff, false)
}

/// A person's explicit answer, rather than a machinery update or retry.
pub(crate) fn reply_answer(cfg: &Config, id: &str, msg: &str, first: bool) -> Result<(), Fail> {
    resume(cfg, id, msg, first, None, true)
}

fn resume(
    cfg: &Config,
    id: &str,
    msg: &str,
    first: bool,
    cutoff: Option<&str>,
    answered: bool,
) -> Result<(), Fail> {
    let sharing = resume_one(cfg, id, msg, first, cutoff, answered)?;
    // The jobs that were waiting on this answer's notice, each given it once,
    // after the first job's lock is let go: each is checked and started under
    // its own, and none passes the answer on again.
    for (other, question) in sharing.jobs {
        let note = format!(
            "The owner answered a question you share with job {id}. Their answer:\n\n{msg}"
        );
        if let Err(e) = deliver(cfg, &other, &sharing.key, &question, &note, first) {
            // Still waiting, it leads its own notice next.
            eprintln!(
                "proj: couldn't pass the answer on to {other}: {}",
                e.say.unwrap_or_default()
            );
        }
    }
    Ok(())
}

struct Sharing {
    key: String,
    jobs: Vec<(String, String)>,
}

fn deliver(
    cfg: &Config,
    id: &str,
    key: &str,
    question: &str,
    msg: &str,
    first: bool,
) -> Result<(), Fail> {
    let job = Job::new(cfg, id);
    let _held = job.lock().map_err(|e| io_fail(&job.path("lock"), e))?;
    crate::question::recover(&job).map_err(|e| Fail::new(1, e))?;
    if crate::question::acceptance_pending(&job).map_err(|e| Fail::new(1, e))? {
        return Ok(());
    }
    if !crate::ask::still_waiting(&job, key, question).map_err(|e| Fail::new(1, e))? {
        return Ok(());
    }
    let st = job.state();
    crate::planning::admit(&job).map_err(|e| Fail::new(1, e))?;
    if st == "queued" || st == "running" {
        return Err(Fail::new(1, format!("{id} is still {st}")));
    }
    crate::turn::start_answer(cfg, id, msg, first).map_err(|e| Fail::new(1, e))
}

fn resume_one(
    cfg: &Config,
    id: &str,
    msg: &str,
    first: bool,
    cutoff: Option<&str>,
    answered: bool,
) -> Result<Sharing, Fail> {
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
    crate::question::recover(&job).map_err(|e| Fail::new(no, e))?;
    let st = job.state();
    crate::planning::admit(&job).map_err(|e| Fail::new(no, e))?;
    if st == "queued" || st == "running" {
        return Err(Fail::new(no, format!("{id} is still {st}")));
    }
    if let Some(n) = cutoff
        && !job.cut_off(n)
    {
        return Err(Fail::new(3, format!("{id}'s turn {n} wasn't cut off")));
    }
    // A registry that can't name the cohort refuses the reply: resuming the
    // leader alone would leave the others waiting on an answer already given.
    let sharing = Sharing {
        key: crate::ask::key_of(&job)
            .map_err(|e| Fail::new(no, e))?
            .unwrap_or_default(),
        jobs: if answered {
            crate::ask::cohort(&job).map_err(|e| Fail::new(no, e))?
        } else {
            vec![]
        },
    };
    if answered {
        crate::turn::start_answer(cfg, id, msg, first)
    } else {
        start(cfg, id, msg, first)
    }
    .map_err(|e| Fail::new(1, e))?;
    Ok(sharing)
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
mod tests;
