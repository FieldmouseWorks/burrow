//! A turn's wait in line for a slot.
//!
//! The line is jobs/.queue: one file per waiting turn, named ([`entry`]) so
//! that sorting puts pipeline ones ahead, then steward and --first ones, then the oldest,
//! holding the waiting process's pid. A slot is a lock on jobs/.slots/N, and N.who says
//! whose (class and project). The lock is the turn's and everything it starts
//! ([`held`]), so a turn that's killed frees its slot by itself, once the last
//! of what it left is gone too; its place in line is cleared by the next look
//! at the line, which finds its pid no longer that turn's. [`pick`] says who has a slot. The turn
//! is first or normal, as it was started; its job's class ([`Classes`]) is
//! asked again at every look, and its place in line, its admission and its
//! slot's N.who all go by that one answer.
//!
//! One waiter looks at a time, under jobs/.slots/.admission-lock, and only those pick
//! names can take a slot, so what a look costs comes out of every waiter's
//! turn at the lock: with the line long, of how soon a free slot gets filled.
//! So a look runs git once per project ([`Classes`]) and nothing else, and it
//! leaves all of pick's choice in jobs/.slots/.next. For [`NEXT_FRESH`] seconds
//! after that, any waiter it doesn't name has nothing to learn and skips its
//! look. It's only ever a reason not to look: a slot is taken only by a look,
//! under the lock.
//!
//! A job run --unstick waits in no line at all ([`unstick_slot`]), and one made
//! so while it waits (proj unstick) leaves the line within a pass.
//!
//! What a waiting turn decides with is the deployed proj's, not the one it
//! started on: at its next pass after a switch it execs that one, and picks up
//! its place in line there, so a change to who goes first reaches everyone
//! waiting and the line and [`pick`] are always one version. A running turn
//! stays on its proj, but what it tells the stall watchdog is the deployed
//! one's too ([`crate::waits::rewatch`]), so the format of a watch item has one
//! owner. All that crosses versions is these: `proj _exec ID CLASS`, in the
//! same pid, with the place left in jobs/.queue under that pid and the look's
//! lock held on fd 9 until that proj is up; and `proj _rewatch ID...`.
//! [`HANDOVER_FORMAT`] names them, and a proj that doesn't print the same one
//! isn't handed anything: the turn stays on the proj it has.

use std::fs::{self, File, FileTimes};
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, UNIX_EPOCH};

use crate::config::Config;
use crate::engine::Engine;
use crate::job::Job;
use crate::pick::{Class, Classes, Slot, Waiter, pick_budget, project};
use crate::snapshot::{self, Waiting};
use crate::{mtime, now, now_nanos, read, usage};

pub const HANDOVER_FORMAT: &str = "1";
pub const RETRY_HANDOVER_FORMAT: &str = "3";
const NEXT_FRESH: i64 = 2;
/// Whatever the note says, a waiter looks this often, which is as long as a
/// job whose class changed can go on holding its old place in line.
const LOOK_EVERY: i64 = 60;

/// The deployed proj's store path, so the switch after this one can't move it
/// from under what's run, if it isn't this one and can take a hand-over. Every
/// waiter asks at every pass, so while the deployed proj is this one the answer
/// runs nothing.
pub fn successor(cfg: &Config) -> Option<PathBuf> {
    let d = fs::canonicalize(&cfg.deployed).ok()?;
    if d == cfg.me {
        return None;
    }
    let said = Command::new(&d)
        .arg("_handover_format")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    (String::from_utf8_lossy(&said.stdout).trim_end() == HANDOVER_FORMAT).then_some(d)
}

/// Every turn requires protected-state-aware startup. Even initial waiters
/// already have an execution identity that older generations cannot maintain.
pub(crate) fn retry_compatible(to: &Path) -> bool {
    Command::new(to)
        .arg("_retry_format")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|said| {
            said.status.success()
                && String::from_utf8_lossy(&said.stdout).trim_end() == RETRY_HANDOVER_FORMAT
        })
}

/// On with the proj at `to`, in this pid, its look's lock on fd 9 until it's up:
/// mid-exec a waiter's cmdline reads empty, and a look clearing out dead places
/// would take it for gone. Only returns if the exec fails. This crate holds no
/// unsafe code, so a shell puts the lock on fd 9; the caller's lock, held till
/// the exec, and the shell's own cmdline, which holds the _exec, leave no gap
/// for a look.
fn hand_over(cfg: &Config, to: &Path, id: &str, given: Class) -> io::Error {
    Command::new(&cfg.tools.bash)
        .args(["-c", r#"exec 9>"$0"; "$1" 9; shift; exec "$@""#])
        .arg(cfg.jobs.join(".slots/.admission-lock"))
        .arg(&cfg.tools.flock)
        .arg(to)
        .args(["_exec", id, given.name()])
        .exec()
}

/// What `_exec` does first: lets go of the look's lock a hand-over came with,
/// so the looks can go on. Unlocking rather than closing, with no unsafe code
/// to close fd 9 with: what's left open locks nothing.
pub fn handed_over(cfg: &Config) {
    if Path::new("/proc/self/fd/9").exists() {
        let _ = Command::new(&cfg.tools.flock)
            .args(["-u", "9"])
            .stderr(Stdio::null())
            .status();
    }
}

/// The place a proj before this one left, if it handed over: this job's, under
/// this pid, and when it was made, which is when this turn came.
fn place_left(queue: &Path, id: &str, pid: u32) -> Option<(String, i64)> {
    let names = fs::read_dir(queue).ok()?;
    let want = pid.to_string();
    let name = names
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| {
            !n.starts_with('.')
                && job_of(n) == id
                && read(&queue.join(n)).as_deref() == Some(want.as_str())
        })?;
    let came = mtime(&queue.join(&name)).unwrap_or_else(now);
    Some((name, came))
}

/// A turn's place in line, the name jobs/.queue has for it (PREFIX-WHEN-ID): 1
/// for a --first turn, 2 for any other ([`Classes::place`] says what that makes
/// it now), and WHEN, in nanoseconds, from when its job's work began (proj run
/// --since), or else from now. Every WHEN has the same number of digits, so
/// sorting the names sorts them by when.
fn entry(job: &Job, id: &str, given: Class) -> String {
    entry_at(job, id, given, now_nanos())
}

pub(crate) fn entry_at(job: &Job, id: &str, given: Class, arrived: i64) -> String {
    if let Some(order) = read(&crate::policy::path(job, "order")) {
        return order;
    }
    let prefix = if given == Class::First { 1 } else { 2 };
    let since = read(&job.path("since"));
    let began = since.as_deref().and_then(|s| s.lines().next());
    match began.filter(|s| !s.is_empty()) {
        Some(s) => format!("{prefix}-{s}000000000-{id}"),
        None => format!("{prefix}-{arrived}-{id}"),
    }
}

/// A job run --unstick takes what the pipeline is stuck on, and that can be the
/// line itself ([`pick`] handing out no slots, say, or every slot held by turns
/// that wait on each other), so its turns never join it: they take a slot of
/// their own past the maxJobs the line shares, jobs/.slots/unstick, and wait
/// only for another --unstick turn to give it up. Nor do they wait for memory
/// to spare: the slice, which the machine caps, holds this turn with the rest,
/// and the job that finds why the machine is full can't wait for it to empty.
fn unstick_slot(cfg: &Config, id: &str, n: &str) -> io::Result<Option<File>> {
    let slots = cfg.jobs.join(".slots");
    let job = Job::new(cfg, id);
    fs::create_dir_all(&slots)?;
    let slot = File::create(slots.join("unstick"))?;
    slot.lock()?;
    // The one it waited for may have been refused for usage, and let go of
    // the slot to wait for the reset: so does this one (None).
    if usage::out(cfg).is_some()
        || automation_paused(cfg, &job)
        || crate::github_quota::held(cfg, &job, Engine::Claude).is_some()
    {
        return Ok(None);
    }
    let slot = held(slot)?;
    fs::write(
        slots.join("unstick.who"),
        format!("unstick\t{}\n", project(&job.dir)),
    )?;
    took_slot(&job, n, "unstick", "unstick")?;
    Ok(Some(slot))
}

/// Say that this job's turn N has slot S (N.slot, which proj state reads as
/// running), and the class it took it as (N.class), under the job's lock: proj
/// withdraw stops a turn only while it has none, and looks under that lock, so
/// none takes one between its look and its stop. One withdrawn while it waits
/// here is stopped with no slot.
fn took_slot(job: &Job, n: &str, s: &str, class: &str) -> io::Result<()> {
    let _held = job.lock()?;
    if automation_paused(job.cfg, job) {
        crate::policy::mark(job, "queued", false).map_err(io::Error::other)?;
        return Err(io::Error::from(io::ErrorKind::WouldBlock));
    }
    let _ = fs::remove_file(crate::policy::path(job, "order"));
    crate::state_reads::write(job.turn(n, "class"), format!("{class}\n"))?;
    crate::state_reads::write(job.turn(n, "slot"), format!("{s}\n"))?;
    crate::policy::accepted(job).map_err(io::Error::other)?;
    snapshot::stream::notify(job.cfg);
    Ok(())
}

/// Say this job's turn N may be withdrawn no more, under the job's lock, so none
/// is stopped on this proj's word once another has it: before a hand-over, to a
/// proj that may take its slot without that lock (one from before [`took_slot`],
/// after a switch back to it). One that can be withdrawn says so again itself,
/// at the top of its [`take`].
fn unmark(job: &Job, n: &str) -> io::Result<()> {
    let _held = job.lock()?;
    match crate::state_reads::remove(job.turn(n, "withdrawable")) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A slot once taken, open across exec to everything the turn starts, as a
/// shell's descriptor is: the lock is on the file, and it goes only with the
/// last process holding it. So what the turn leaves running (a child that
/// ignores TERM, a detached one) keeps the slot after `_exec` has ended or
/// died, until the unit's stop has taken the last of it, and a waiter can't
/// take a slot that work still runs in.
fn held(slot: File) -> io::Result<File> {
    rustix::io::fcntl_setfd(&slot, rustix::io::FdFlags::empty())?;
    Ok(slot)
}

/// Whether a waiter whose place is at this path, which last looked at this
/// time, should look now: it's never looked, or not lately, or there's no
/// fresh note, or the note names it. The note is the time and each name,
/// tab-separated, and may be caught half written (no newline yet): that costs
/// a look, or a pass until the writer's done.
fn look_due(note: Option<&str>, me: &str, looked: i64, now: i64) -> bool {
    if now - looked >= LOOK_EVERY {
        return true;
    }
    let Some((line, _)) = note.and_then(|n| n.split_once('\n')) else {
        return true;
    };
    let mut fields = line.split('\t');
    let at = fields
        .next()
        .filter(|a| !a.is_empty() && a.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|a| a.parse::<i64>().ok());
    match at {
        Some(at) if at <= now && now - at < NEXT_FRESH => fields.any(|f| f == me),
        _ => true,
    }
}

/// Whether the note at jobs/.slots/.next has this waiter look now.
fn due(slots: &Path, me: &Path, looked: i64) -> bool {
    let note = fs::read_to_string(slots.join(".next")).ok();
    look_due(note.as_deref(), &me.to_string_lossy(), looked, now())
}

/// What pick said, as the note [`look_due`] reads.
fn note_next(slots: &Path, names: &[String]) -> io::Result<()> {
    let mut note = now().to_string();
    for name in names {
        note.push('\t');
        note.push_str(name);
    }
    note.push('\n');
    fs::write(slots.join(".next"), note)
}

/// What a look sees: every slot, whether any is taken, and the line.
struct Look {
    slots: Vec<Slot>,
    busy: bool,
    line: Vec<Waiter>,
}

/// A look, for the waiter named `me`, of job `dir`, class `own`, under the
/// lock: the slots, then the line, once the places of turns that are gone are
/// cleared.
fn look(cfg: &Config, me: &str, dir: &Path, own: Class) -> io::Result<Look> {
    let queue = cfg.jobs.join(".queue");
    let dirs = cfg.jobs.join(".slots");
    let mut classes = Classes::default();
    let mut slots = Vec::new();
    let mut busy = false;
    for i in 1..=cfg.max_jobs {
        let f = File::create(dirs.join(i.to_string()))?;
        if f.try_lock().is_ok() {
            // Let go at once, whoever's forking: a child shares this file
            // until it execs, and closing it wouldn't free the slot till then.
            let _ = f.unlock();
            slots.push(Slot::Free);
        } else {
            busy = true;
            let who = read(&dirs.join(format!("{i}.who"))).unwrap_or_else(|| "-\t-".into());
            let (c, p) = who.split_once('\t').unwrap_or((&who, ""));
            slots.push(Slot::Held {
                class: Class::parse(c),
                project: p.to_owned(),
            });
        }
    }
    let pending = crate::retry::admission(cfg)?;
    let existing: Vec<_> = fs::read_dir(&queue)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    for (id, name, arrived) in &pending {
        let mut places = existing
            .iter()
            .filter(|n| !n.starts_with('.') && job_of(n) == id)
            .map(|n| queue.join(n))
            .collect::<Vec<_>>();
        if places.is_empty() {
            // A dead parent's place may have been cleared by another census.
            let path = queue.join(name);
            crate::state_reads::write(&path, "pending\n")?;
            places.push(path);
        }
        for path in places {
            let file = crate::state_reads::open(&path)?;
            let birth = UNIX_EPOCH + Duration::from_nanos((*arrived).max(0) as u64);
            if file.metadata()?.modified()? > birth {
                // Preserve the earlier refusal even when an old writer only
                // managed to publish its queue entry after reset.
                file.set_times(FileTimes::new().set_modified(birth))?;
            }
        }
    }
    let mut names: Vec<String> = fs::read_dir(&queue)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    let mut line = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // The live caller's place wins over any stale names of that same job.
    names.sort_by_key(|name| (name != me, name.clone()));
    for name in names {
        let fid = job_of(&name);
        let alive = name == me
            || waiter_alive(&queue.join(&name), fid)
            || pending.iter().any(|(id, _, _)| id == fid);
        if !alive || !seen.insert(fid.to_owned()) {
            remove_place(&queue.join(&name))?;
            continue;
        }
        let other = Job::new(cfg, fid);
        if automation_paused(cfg, &other)
            || crate::github_quota::held(cfg, &other, Engine::Claude).is_some()
        {
            continue;
        }
        if name == me {
            line.push(Waiter {
                name,
                class: own,
                project: project(dir),
            });
        } else {
            let dir = cfg.jobs.join(fid);
            line.push(Waiter {
                class: classes.place(cfg, &name, &dir).0,
                project: project(&dir),
                name,
            });
        }
    }
    line.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Look { slots, busy, line })
}

/// Codex never consumes Claude's slots, its unstick slot or its usage hold.
/// This small pool stays capped even for pipeline jobs, leaving the plan's
/// reviewer service first claim. Locks survive exec just as Claude's do.
fn codex_slot(
    cfg: &Config,
    id: &str,
    n: &str,
    given: Class,
    may_hand_over: bool,
) -> io::Result<File> {
    let slots = cfg.jobs.join(".codex-slots");
    fs::create_dir_all(&slots)?;
    fs::create_dir_all(cfg.jobs.join(".slots"))?;
    let job = Job::new(cfg, id);
    crate::state_reads::write(job.turn(n, "withdrawable"), "")?;
    // A turn that waited in Claude's line and was handed to this proj brings
    // its place with it, under this pid. It is Codex's now, and a place left in
    // Claude's line would count as a live waiter there (waiter_alive) while it
    // waits for, or runs in, Codex's pool.
    let queue = cfg.jobs.join(".queue");
    // Removal cannot hide a live rename: a census retaining this old name
    // treats its missing timestamp conservatively until its next look. Keep
    // this escape pool independent of a stalled Claude admission lock.
    while let Some((left, _)) = place_left(&queue, id, process::id()) {
        fs::remove_file(queue.join(left))?;
        snapshot::stream::notify(cfg);
    }
    loop {
        if may_hand_over
            && let Some(to) = successor(cfg)
            && retry_compatible(&to)
        {
            unmark(&job, n)?;
            let lk = File::create(cfg.jobs.join(".slots/.admission-lock"))?;
            lk.lock()?;
            return Err(hand_over(cfg, &to, id, given));
        }
        if automation_paused(cfg, &job)
            || crate::policy::earlier_pending(&job)
            || crate::github_quota::held(cfg, &job, Engine::Codex).is_some()
        {
            sleep(Duration::from_secs(2));
            continue;
        }
        for i in 1..=cfg.codex_slots {
            let slot = File::create(slots.join(i.to_string()))?;
            if slot.try_lock().is_ok() {
                let slot = held(slot)?;
                took_slot(&job, n, &format!("codex-{i}"), given.name())?;
                return Ok(slot);
            }
        }
        sleep(Duration::from_secs(2));
    }
}

/// Recheck bound repository admission before taking any slot. Explicit pauses
/// vacate every role; unlisted repos can still sort issues.
fn automation_paused(_cfg: &Config, job: &Job) -> bool {
    crate::policy::denied(job)
}

pub fn take_engine(
    cfg: &Config,
    id: &str,
    n: &str,
    given: Class,
    may_hand_over: bool,
    engine: Engine,
) -> io::Result<File> {
    crate::waits::rewatch_later(cfg, id);
    match engine {
        Engine::Claude => take(cfg, id, n, given, may_hand_over),
        Engine::Codex => codex_slot(cfg, id, n, given, may_hand_over),
    }
}

/// Wait in line for a slot, then hold it: the slot is this process's until
/// the file returned is closed, or the process ends. With `may_hand_over`, a
/// switch meanwhile hands the wait to the deployed proj ([`successor`]).
pub fn take(
    cfg: &Config,
    id: &str,
    n: &str,
    given: Class,
    may_hand_over: bool,
) -> io::Result<File> {
    let queue = cfg.jobs.join(".queue");
    let slots = cfg.jobs.join(".slots");
    fs::create_dir_all(&queue)?;
    fs::create_dir_all(&slots)?;
    let job = Job::new(cfg, id);
    // This turn says it has its slot only under its job's lock ([`took_slot`]),
    // so proj withdraw may stop it while it waits. On a proj from before that,
    // it can't be, never having said so: until it hands over to this one, or
    // from the moment this one hands it over ([`unmark`]).
    crate::state_reads::write(job.turn(n, "withdrawable"), "")?;
    // maxWait goes by when this turn came, not its name's WHEN, which for a job
    // run --since is when its work began.
    let retry = crate::retry::Checkpoint::load(&job, n)?;
    let original = retry.as_ref().map(|s| s.queue().1);
    let (mut me, came) = place_left(&queue, id, process::id())
        .or_else(|| {
            retry.as_ref().map(|s| {
                let (name, waited) = s.queue();
                (name.to_owned(), waited / 1_000_000_000)
            })
        })
        .unwrap_or_else(|| (entry(&job, id, given), now()));
    let mut looked = 0;
    // The class the snapshot last showed for this place: lead and pipeline
    // share a filename prefix, so only this sees one become the other.
    let mut shown: Option<Class> = None;
    loop {
        let paused = automation_paused(cfg, &job);
        // Out of usage, nothing takes a slot till it's back (usage.rs): every
        // run would be refused, an --unstick turn's too.
        usage::metered(cfg);
        let spent = usage::out(cfg).is_some();
        // Made --unstick while it waits (proj unstick): it goes now, look or not.
        if !paused && job.path("unstick").exists() {
            // Leaving the line needs no census lock: stale looks retain the
            // cap conservatively, and the emergency path must never wait on it.
            if fs::remove_file(queue.join(&me)).is_ok() {
                snapshot::stream::notify(cfg);
            }
            if spent || crate::github_quota::held(cfg, &job, Engine::Claude).is_some() {
                usage::remind(cfg);
                sleep(Duration::from_secs(2));
                continue;
            }
            // Out of usage by the time it had the slot: back in line.
            if let Some(slot) = unstick_slot(cfg, id, n)? {
                return Ok(slot);
            }
            continue;
        }
        // A switch since the last pass: on with the deployed proj, in this
        // place, whether or not a look is due (one the note skips can wait
        // LOOK_EVERY).
        if may_hand_over
            && let Some(to) = successor(cfg)
            && retry_compatible(&to)
        {
            unmark(&job, n)?;
            let lk = File::create(slots.join(".admission-lock"))?;
            lk.lock()?;
            let failed = hand_over(cfg, &to, id, given);
            drop(lk);
            return Err(failed);
        }
        if paused {
            sleep(Duration::from_secs(2));
            continue;
        }
        let lk = File::create(slots.join(".admission-lock"))?;
        // Keep checking for emergency reclassification while another census
        // is stuck: blocking here would strand a queue-to-unstick transition.
        match lk.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                sleep(Duration::from_secs(2));
                continue;
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
        // Adopt one logical place under the census lock. A checkpoint may
        // predate a promotion or a configuration-driven class rename.
        if retry.is_some() {
            reconcile(&queue, id, &me)?;
        }
        // Waited long enough: among the --first ones now, in its place by WHEN.
        if let Some(rest) = me.strip_prefix("2-")
            && now() - came >= cfg.max_wait as i64
        {
            let to = format!("1-{rest}");
            if fs::rename(queue.join(&me), queue.join(&to)).is_ok() {
                snapshot::stream::notify(cfg);
            }
            me = to;
        }
        if spent {
            if !job.path("unstick").exists() && !publish(cfg, &queue, &me, original)? {
                drop(lk);
                sleep(Duration::from_secs(2));
                continue;
            }
            drop(lk);
            usage::remind(cfg);
            sleep(Duration::from_secs(2));
            continue;
        }
        if !due(&slots, &queue.join(&me), looked) {
            drop(lk);
            sleep(Duration::from_secs(2));
            continue;
        }
        // Where it stands now, and in the journal if that changed while it
        // waits.
        let (class, new) = Classes::default().place(cfg, &me, &job.dir);
        if new != me {
            if fs::rename(queue.join(&me), queue.join(&new)).is_ok() {
                eprintln!("proj: it goes as {} now", class.name());
                snapshot::stream::notify(cfg);
            }
            me = new;
        }
        if class_is_news(&mut shown, class) {
            snapshot::stream::notify(cfg);
        }
        if !publish(cfg, &queue, &me, original)? {
            drop(lk);
            sleep(Duration::from_secs(2));
            continue;
        }
        if crate::policy::earlier_pending(&job) {
            drop(lk);
            sleep(Duration::from_secs(2));
            continue;
        }
        // A slot is taken only under this lock, so one a look sees free stays
        // free until it's done. Another waiter's look may have come while this
        // one waited for it.
        // Out of usage while it waited for the lock: no look, no slot.
        if !due(&slots, &queue.join(&me), looked) || usage::out(cfg).is_some() {
            drop(lk);
            sleep(Duration::from_secs(2));
            continue;
        }
        let seen = look(cfg, &me, &job.dir, class)?;
        looked = now();
        let cutoff = match usage::admission_cutoff(cfg) {
            Ok(cutoff) => cutoff,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                drop(lk);
                sleep(Duration::from_secs(2));
                continue;
            }
            Err(e) => return Err(e),
        };
        let draining = cutoff.is_some_and(|release| draining(&queue, &seen.line, release));
        let active = seen
            .slots
            .iter()
            .filter(|s| matches!(s, Slot::Held { .. }))
            .count();
        let budget = if draining {
            cfg.usage_trickle.saturating_sub(active)
        } else {
            usize::MAX
        };
        let picked = pick_budget(cfg, &seen.slots, &seen.line, budget);
        let names: Vec<String> = picked
            .iter()
            .map(|&(i, _)| {
                queue
                    .join(&seen.line[i].name)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        note_next(&slots, &names)?;
        // Short of memory matters only while another job runs: with nothing
        // running, waiting frees nothing.
        if let Some(&(_, n_slot)) = picked.iter().find(|&&(i, _)| seen.line[i].name == me)
            && (!seen.busy || spare() >= cfg.min_free_memory)
        {
            let slot = File::create(slots.join(n_slot.to_string()))?;
            if slot.try_lock().is_ok() {
                let slot = held(slot)?;
                fs::write(
                    slots.join(format!("{n_slot}.who")),
                    format!("{}\t{}\n", class.name(), project(&job.dir)),
                )?;
                // The note named this one: the next to go needn't wait it
                // out, and any others it named find theirs in the next look.
                fs::write(slots.join(".next"), "")?;
                let _ = fs::remove_file(queue.join(&me));
                drop(lk);
                fs::write(job.turn(n, "queue-order"), &me)?;
                took_slot(&job, n, &n_slot.to_string(), class.name())?;
                return Ok(slot);
            }
        }
        drop(lk);
        sleep(Duration::from_secs(2));
    }
}

/// Whether a place's class changed since the snapshot last showed it. Lead and
/// pipeline share a filename prefix, so a rename never tells of one becoming
/// the other. The first look is no news: the place's creation was.
pub(crate) fn class_is_news(shown: &mut Option<Class>, class: Class) -> bool {
    shown.replace(class).is_some_and(|before| before != class)
}

/// This waiter's place in line, `me`, published whole (a dot file, which the
/// line skips, then renamed), so a waiter clearing out dead places never sees
/// it without its pid. Put back if it went anyway. False means the usage
/// transaction is busy; retry from the emergency check without publishing.
fn publish(cfg: &Config, queue: &Path, me: &str, original: Option<i64>) -> io::Result<bool> {
    let created = read(&queue.join(me)).as_deref() != Some(process::id().to_string().as_str());
    if created {
        // Birth and early release are ordered by the same usage lock, so a
        // queue entry cannot appear between capturing the cutoff and committing
        // the release. The independent pools never publish here.
        let _usage = match usage::try_lock(cfg) {
            Ok(lock) => lock,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) => return Err(e),
        };
        let tmp = queue.join(format!(".{me}.new"));
        fs::write(&tmp, format!("{}\n", process::id()))?;
        let arrived = original.unwrap_or_else(now_nanos);
        File::open(&tmp)?.set_times(
            FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_nanos(arrived.max(0) as u64)),
        )?;
        fs::rename(&tmp, queue.join(me))?;
    }
    crate::policy::accepted(&Job::new(cfg, job_of(me))).map_err(io::Error::other)?;
    // Only a new place is news: a poll that finds it there changes nothing.
    if created {
        snapshot::stream::notify(cfg);
    }
    Ok(true)
}

/// Every turn in line, in the order pick takes them (lead ones, then the
/// rest), with its job, class, project and how long it has waited: the live
/// snapshot's line, which `proj status --line` prints. Only reads. The wait is
/// from when its place was made, not its name's WHEN, which for a job started
/// --since is when its work began.
pub fn line(cfg: &Config) -> Vec<Waiting> {
    let queue = cfg.jobs.join(".queue");
    let pending = crate::retry::admission(cfg).unwrap_or_default();
    let mut names: Vec<String> = fs::read_dir(&queue)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let mut classes = Classes::default();
    let at = now();
    let mut rows: Vec<Waiting> = Vec::new();
    for name in &names {
        let fid = job_of(name);
        if automation_paused(cfg, &Job::new(cfg, fid)) {
            continue;
        }
        if !waiter_alive(&queue.join(name), fid) && !pending.iter().any(|(id, _, _)| id == fid) {
            continue;
        }
        let Some(made) = mtime(&queue.join(name)) else {
            continue;
        };
        let dir = cfg.jobs.join(fid);
        let (class, _) = classes.place(cfg, name, &dir);
        rows.push(Waiting {
            job: fid.to_owned(),
            class: class.name().to_owned(),
            project: project(&dir),
            since: made,
            wait: at - made,
        });
    }
    rows.sort_by_key(|w| w.class != Class::Lead.name());
    rows
}

/// What a look at a slot's lock file finds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lock {
    /// Locked by nobody: tried, and let go at once.
    Free,
    Taken,
    /// No file: proj's own pools make theirs as they're first taken, so for
    /// them it's free; heavy's are made at boot, so one missing is unknown.
    Missing,
    /// It couldn't be opened or tried (permissions, too many files, I/O):
    /// never counted as free.
    Unknown,
}

fn lock(path: &Path) -> Lock {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Lock::Missing,
        Err(_) => return Lock::Unknown,
    };
    match f.try_lock() {
        Ok(()) => {
            let _ = f.unlock();
            Lock::Free
        }
        Err(std::fs::TryLockError::WouldBlock) => Lock::Taken,
        Err(std::fs::TryLockError::Error(_)) => Lock::Unknown,
    }
}

mod owner;

/// Every slot of the line's, Codex's pool and the machine's heavy work, for
/// the live snapshot. A taken one's holder is the job whose `_exec` the kernel
/// says took its lock, with matching lock evidence on that live process's
/// file descriptor, checked again after collecting the job metadata:
/// with what that job's latest turn says (its state, and the class it took the
/// slot as) once that turn's N.slot names the slot, and as starting until it
/// does. One taken by anything else (what an ended turn left running, heavy's
/// users) is busy, with no holder or time it can vouch for.
pub(crate) fn view(cfg: &Config, at: i64) -> Vec<snapshot::Slot> {
    use snapshot::{Holder, SlotKind, SlotState, held, job_slot};
    let mut slots = Vec::new();
    // Each slot by its name in its kind, the name a turn's N.slot gives it,
    // its lock, and what a missing lock file means.
    let mut one = |kind: SlotKind, name: String, key: String, path: PathBuf, missing: Lock| {
        let state = match lock(&path) {
            Lock::Missing => missing,
            l => l,
        };
        let observed_owner = (state == Lock::Taken).then(|| owner::read(&path)).flatten();
        let slot = match state {
            Lock::Free | Lock::Missing => held(kind, name, SlotState::Idle, None, at),
            Lock::Unknown => held(kind, name, SlotState::Unknown, None, at),
            Lock::Taken => match observed_owner.clone() {
                Some(owner) => {
                    let id = owner.job.clone();
                    let job = Job::new(cfg, &id);
                    let n = job.turns().unwrap_or_default();
                    if read(&job.turn(&n, "slot")).as_deref() == Some(key.as_str()) {
                        let mut s = job_slot(cfg, kind, name, &id, None, at);
                        if let Some(h) = s.holder.as_mut() {
                            h.class = read(&job.turn(&n, "class")).filter(|c| !c.is_empty());
                        }
                        s
                    } else {
                        snapshot::Slot {
                            holder: Some(Holder {
                                job: Some(id),
                                ..Holder::default()
                            }),
                            ..held(kind, name, SlotState::Starting, None, at)
                        }
                    }
                }
                None => held(kind, name, SlotState::Busy, None, at),
            },
        };
        let slot = if slot.holder.is_some()
            && state == Lock::Taken
            && owner::read(&path) != observed_owner
        {
            held(kind, slot.name.clone(), SlotState::Busy, None, at)
        } else {
            slot
        };
        slots.push(slot);
    };
    let dir = cfg.jobs.join(".slots");
    for i in 1..=cfg.max_jobs {
        one(
            SlotKind::Claude,
            i.to_string(),
            i.to_string(),
            dir.join(i.to_string()),
            Lock::Free,
        );
    }
    one(
        SlotKind::Unstick,
        "unstick".into(),
        "unstick".into(),
        dir.join("unstick"),
        Lock::Free,
    );
    let codex = cfg.jobs.join(".codex-slots");
    for i in 1..=cfg.codex_slots {
        one(
            SlotKind::Codex,
            i.to_string(),
            format!("codex-{i}"),
            codex.join(i.to_string()),
            Lock::Free,
        );
    }
    if let Some(heavy) = &cfg.heavy_dir {
        for i in 1..=cfg.heavy_slots {
            // Nothing of proj's takes these: whose they are is heavy's.
            one(
                SlotKind::Heavy,
                i.to_string(),
                String::new(),
                burrow_heavy::config::slot_file(heavy, i),
                Lock::Unknown,
            );
        }
    }
    slots
}

/// The job a place in line is for: PREFIX-WHEN-ID.
pub(crate) fn job_of(name: &str) -> &str {
    name.split_once('-')
        .and_then(|(_, r)| r.split_once('-'))
        .map_or(name, |(_, r)| r)
}

/// Whether the place at a path is a waiting turn's: its pid's command line is
/// that job's _exec.
fn waiter_alive(place: &Path, id: &str) -> bool {
    let Some(pid) = read(place).filter(|p| !p.is_empty()) else {
        return false;
    };
    let Ok(cmdline) = fs::read(Path::new("/proc").join(pid).join("cmdline")) else {
        return false;
    };
    let cmdline: Vec<u8> = cmdline
        .into_iter()
        .map(|b| if b == 0 { b' ' } else { b })
        .collect();
    String::from_utf8_lossy(&cmdline).contains(&format!(" _exec {id} "))
}

/// Whether a turn on this engine started now would get a slot, as far as a
/// look from outside the line can tell: for Claude, usage not spent, nobody
/// in line and a slot free beyond the ones kept for the pipeline; for Codex,
/// one of its pool free. Advisory: a turn already on its way may take the
/// slot first, and the new one then waits in line like any other. For what
/// work nobody is waiting on (proj-inbox's drain) starts only on idle slots.
pub fn room(cfg: &Config, engine: Engine) -> bool {
    let free = |dir: &str, n: usize| -> usize {
        let dir = cfg.jobs.join(dir);
        if fs::create_dir_all(&dir).is_err() {
            return 0;
        }
        // Only a slot seen free is room: one that can't be looked at isn't.
        (1..=n)
            .filter(|i| matches!(lock(&dir.join(i.to_string())), Lock::Free | Lock::Missing))
            .count()
    };
    match engine {
        Engine::Claude => {
            usage::metered(cfg);
            usage::out(cfg).is_none()
                && crate::retry::admission(cfg).is_ok_and(|p| p.is_empty())
                && line(cfg).is_empty()
                && free(".slots", cfg.max_jobs) > cfg.pipeline_slots
        }
        Engine::Codex => free(".codex-slots", cfg.codex_slots) > 0,
    }
}

/// Queue mutations and this census share the admission lock. An unreadable
/// place is uncertainty, never evidence that the old queue has drained.
fn draining(queue: &Path, line: &[Waiter], release: i64) -> bool {
    line.iter().any(|w| {
        fs::metadata(queue.join(&w.name))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_none_or(|came| came.as_nanos() < release as u128)
    })
}

/// Memory to spare, in MiB: what the kernel says is available, plus what the
/// ZFS ARC would give back (the kernel doesn't count it).
pub fn spare() -> u64 {
    let field = |text: &str, name: &str, at: usize| -> u64 {
        text.lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>())
            .find(|f| f.first().is_some_and(|k| k.trim_end_matches(':') == name))
            .and_then(|f| f.get(at).and_then(|v| v.parse().ok()))
            .unwrap_or(0)
    };
    let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let kb = field(&meminfo, "MemAvailable", 1);
    let arc = fs::read_to_string("/proc/spl/kstat/zfs/arcstats").map_or(0, |a| {
        let (size, min) = (field(&a, "size", 2), field(&a, "c_min", 2));
        size.saturating_sub(min) / 1024
    });
    (kb + arc) / 1024
}

#[cfg(test)]
mod tests;

/// Remove obsolete names only while holding the shared census lock.
fn reconcile(queue: &Path, id: &str, adopted: &str) -> io::Result<()> {
    for entry in fs::read_dir(queue)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') && name != adopted && job_of(&name) == id {
            remove_place(&entry.path())?;
        }
    }
    Ok(())
}

fn remove_place(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
