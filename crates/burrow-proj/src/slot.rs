//! A turn's wait in line for a slot.
//!
//! The line is jobs/.queue: one file per waiting turn, named so that sorting
//! puts pipeline ones ahead, then --first ones, then the oldest, holding the
//! waiting process's pid. A slot is a lock on jobs/.slots/N, and N.who says
//! whose (class and project), so a turn that's killed frees its slot by
//! itself; its place in line is cleared by the next look at the line, which
//! finds its pid no longer that turn's. [`pick`] says who has a slot. The turn
//! is first or normal, as it was started; its job's class ([`Classes`]) is
//! asked again at every look, and its place in line, its admission and its
//! slot's N.who all go by that one answer.
//!
//! One waiter looks at a time, under jobs/.slots/.lock, and only those pick
//! names can take a slot, so what a look costs comes out of every waiter's
//! turn at the lock: with the line long, of how soon a free slot gets filled.
//! So a look runs git once per project ([`Classes`]) and nothing else, and it
//! leaves all of pick's choice in jobs/.slots/.next. For [`NEXT_FRESH`] seconds
//! after that, any waiter it doesn't name has nothing to learn and skips its
//! look. It's only ever a reason not to look: a slot is taken only by a look,
//! under the lock.

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::process;
use std::thread::sleep;
use std::time::Duration;

use crate::config::Config;
use crate::job::Job;
use crate::pick::{Class, Classes, Slot, Waiter, pick, project};
use crate::{now, now_nanos, read};

const NEXT_FRESH: i64 = 2;
/// Whatever the note says, a waiter looks this often, which is as long as a
/// job whose class changed can go on holding its old place in line.
const LOOK_EVERY: i64 = 60;

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
    let mut names: Vec<String> = fs::read_dir(&queue)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    let mut line = Vec::new();
    for name in names {
        let fid = job_of(&name);
        if name == me {
            line.push(Waiter {
                name,
                class: own,
                project: project(dir),
            });
        } else if waiter_alive(&queue.join(&name), fid) {
            let dir = cfg.jobs.join(fid);
            line.push(Waiter {
                class: classes.place(cfg, &name, &dir).0,
                project: project(&dir),
                name,
            });
        } else {
            let _ = fs::remove_file(queue.join(&name));
        }
    }
    Ok(Look { slots, busy, line })
}

/// Wait in line for a slot, then hold it: the slot is this process's until
/// the file returned is closed, or the process ends.
pub fn take(cfg: &Config, id: &str, n: &str, given: Class) -> io::Result<File> {
    let queue = cfg.jobs.join(".queue");
    let slots = cfg.jobs.join(".slots");
    fs::create_dir_all(&queue)?;
    fs::create_dir_all(&slots)?;
    let job = Job::new(cfg, id);
    let since = now();
    let prefix = if given == Class::First { 1 } else { 2 };
    let mut me = format!("{prefix}-{}-{id}", now_nanos());
    let mut looked = 0;
    loop {
        // Waited long enough: among the --first ones now, by when it came.
        if let Some(rest) = me.strip_prefix("2-")
            && now() - since >= cfg.max_wait as i64
        {
            let to = format!("1-{rest}");
            let _ = fs::rename(queue.join(&me), queue.join(&to));
            me = to;
        }
        if !due(&slots, &queue.join(&me), looked) {
            sleep(Duration::from_secs(2));
            continue;
        }
        // Where it stands now, and in the journal if that changed while it
        // waits.
        let (class, new) = Classes::default().place(cfg, &me, &job.dir);
        if new != me {
            if fs::rename(queue.join(&me), queue.join(&new)).is_ok() {
                eprintln!("proj: it goes as {} now", class.name());
            }
            me = new;
        }
        // Published whole (a dot file, which the line skips, then renamed),
        // so a waiter clearing out dead places never sees it without its
        // pid. Put back if it went anyway.
        if !queue.join(&me).exists() {
            let tmp = queue.join(format!(".{me}.new"));
            fs::write(&tmp, format!("{}\n", process::id()))?;
            fs::rename(&tmp, queue.join(&me))?;
        }
        // A slot is taken only under this lock, so one a look sees free stays
        // free until it's done. Another waiter's look may have come while this
        // one waited for it.
        let lk = File::create(slots.join(".lock"))?;
        lk.lock()?;
        if !due(&slots, &queue.join(&me), looked) {
            drop(lk);
            sleep(Duration::from_secs(2));
            continue;
        }
        let seen = look(cfg, &me, &job.dir, class)?;
        looked = now();
        let picked = pick(cfg, &seen.slots, &seen.line);
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
                fs::write(
                    slots.join(format!("{n_slot}.who")),
                    format!("{}\t{}\n", class.name(), project(&job.dir)),
                )?;
                // The note named this one: the next to go needn't wait it
                // out, and any others it named find theirs in the next look.
                fs::write(slots.join(".next"), "")?;
                drop(lk);
                let _ = fs::remove_file(queue.join(&me));
                fs::write(job.turn(n, "slot"), format!("{n_slot}\n"))?;
                return Ok(slot);
            }
        }
        drop(lk);
        sleep(Duration::from_secs(2));
    }
}

/// `proj status --line`: every turn in line, in the order pick takes them
/// (lead ones, then the rest), with its job, class, how long it has waited and
/// its project. Only reads.
pub fn in_line(cfg: &Config) -> String {
    let queue = cfg.jobs.join(".queue");
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
    let mut rows: Vec<(Class, String)> = Vec::new();
    for name in &names {
        let fid = job_of(name);
        if !waiter_alive(&queue.join(name), fid) {
            continue;
        }
        let dir = cfg.jobs.join(fid);
        let (class, _) = classes.place(cfg, name, &dir);
        let when: i64 = name
            .split_once('-')
            .and_then(|(_, r)| r.split_once('-'))
            .and_then(|(w, _)| w.parse().ok())
            .unwrap_or(0);
        let wait = now() - when / 1_000_000_000;
        let row = format!(
            "{fid:<40} {:<8} {:>4}m{:02}s  {}\n",
            class.name(),
            wait / 60,
            wait % 60,
            project(&dir)
        );
        rows.push((class, row));
    }
    rows.sort_by_key(|(c, _)| *c != Class::Lead);
    rows.into_iter().map(|(_, r)| r).collect()
}

/// The job a place in line is for: PREFIX-WHEN-ID.
fn job_of(name: &str) -> &str {
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
