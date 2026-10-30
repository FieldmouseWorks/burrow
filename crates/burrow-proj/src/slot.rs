//! A turn's wait in line for a slot.
//!
//! The line is jobs/.queue: one file per waiting turn, named so that sorting
//! puts pipeline ones ahead, then --first ones, then the oldest, holding the
//! waiting process's pid. A slot is a lock on jobs/.slots/N, and N.who says
//! whose (class and project), so a turn that's killed frees its slot by
//! itself; its place in line is cleared by the next look at the line, which
//! finds its pid no longer that turn's. [`pick`] says who has a slot. The turn
//! is first or normal, as it was started; its job's class ([`place`]) is
//! asked again every pass, and its place in line, its admission and its
//! slot's N.who all go by that one answer.

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::process;
use std::thread::sleep;
use std::time::Duration;

use crate::config::Config;
use crate::job::Job;
use crate::pick::{Class, Slot, Waiter, pick, place, project};
use crate::{now, now_nanos, read};

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
    loop {
        // Waited long enough: among the --first ones now, by when it came.
        if let Some(rest) = me.strip_prefix("2-")
            && now() - since >= cfg.max_wait as i64
        {
            let to = format!("1-{rest}");
            let _ = fs::rename(queue.join(&me), queue.join(&to));
            me = to;
        }
        // Where it stands now, and in the journal if that changed while it
        // waits.
        let (class, new) = place(cfg, &me, &job.dir);
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
        // One waiter looks at a time, and a slot is taken only under this
        // lock, so one it sees free stays free until it's done.
        let lk = File::create(slots.join(".lock"))?;
        lk.lock()?;
        let mut seen = Vec::new();
        let mut busy = false;
        for i in 1..=cfg.max_jobs {
            let f = File::create(slots.join(i.to_string()))?;
            if f.try_lock().is_ok() {
                seen.push(Slot::Free);
            } else {
                busy = true;
                let who = read(&slots.join(format!("{i}.who"))).unwrap_or_else(|| "-\t-".into());
                let (c, p) = who.split_once('\t').unwrap_or((&who, ""));
                seen.push(Slot::Held {
                    class: Class::parse(c),
                    project: p.to_owned(),
                });
            }
        }
        // The line, once the places of turns that are gone are cleared.
        let mut names: Vec<String> = fs::read_dir(&queue)?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort();
        let mut line = Vec::new();
        for name in names {
            let fid = name
                .split_once('-')
                .and_then(|(_, r)| r.split_once('-'))
                .map_or(name.as_str(), |(_, r)| r);
            let Some(pid) = read(&queue.join(&name)) else {
                continue;
            };
            if name == me {
                line.push(Waiter {
                    name,
                    class,
                    project: project(&job.dir),
                });
            } else if !pid.is_empty() && waits_as(&pid, fid) {
                let dir = cfg.jobs.join(fid);
                line.push(Waiter {
                    class: place(cfg, &name, &dir).0,
                    project: project(&dir),
                    name,
                });
            } else {
                let _ = fs::remove_file(queue.join(&name));
            }
        }
        // Short of memory matters only while another job runs: with nothing
        // running, waiting frees nothing.
        if let Some(&(_, n_slot)) = pick(cfg, &seen, &line).iter().find(|&&(i, _)| line[i].name == me)
            && (!busy || spare() >= cfg.min_free_memory)
        {
            let slot = File::create(slots.join(n_slot.to_string()))?;
            if slot.try_lock().is_ok() {
                fs::write(
                    slots.join(format!("{n_slot}.who")),
                    format!("{}\t{}\n", class.name(), project(&job.dir)),
                )?;
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

/// Whether the process PID is a turn of job ID waiting in line: its _exec.
fn waits_as(pid: &str, id: &str) -> bool {
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
