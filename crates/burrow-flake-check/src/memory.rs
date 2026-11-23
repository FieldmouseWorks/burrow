//! Bound concurrent evaluations by both the machine and every visible
//! cgroup ancestor. An unreadable limit means one evaluation, never infinity.
//! What one evaluation holds isn't known before it runs, so the room left
//! is watched while they do: a new one starts only with [`RESERVE`] free,
//! and below [`FLOOR`] the biggest but the eldest is stopped, to run again alone.
use super::*;
fn read(p: &Path) -> Result<String> {
    Ok(fs::read_to_string(p)?)
}
fn number(p: &Path) -> Result<i128> {
    Ok(i128::from(read(p)?.trim().parse::<u64>()?))
}
fn field(text: &str, key: &str) -> Result<i128> {
    text.lines()
        .find_map(|l| {
            let mut w = l.split_whitespace();
            if w.next() == Some(key) {
                w.next()?.parse::<u64>().ok().map(i128::from)
            } else {
                None
            }
        })
        .ok_or_else(|| err(format!("missing {key}")))
}
fn unescape(s: &str) -> String {
    s.replace("\\040", " ").replace("\\011", "\t")
}
fn room() -> Result<i128> {
    let mut free = field(&read(Path::new("/proc/meminfo"))?, "MemAvailable:")? * 1024;
    let proc = env::var_os("CGROUP_PROC")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/proc/self".into());
    if !proc.join("cgroup").try_exists()? {
        return Ok(free);
    }
    let cg = read(&proc.join("cgroup")).map_err(|_| err("its cgroups couldn't be read"))?;
    let mounts = read(&proc.join("mountinfo")).map_err(|_| err("its cgroups couldn't be read"))?;
    let mut selected = None;
    for l in cg.lines() {
        let p: Vec<_> = l.splitn(3, ':').collect();
        if p.len() != 3 {
            return Err(err("bad cgroup"));
        }
        if p[1].split(',').any(|x| x == "memory") {
            selected = Some((false, p[2]));
            break;
        } else if p[1].is_empty() {
            selected = Some((true, p[2]));
        }
    }
    let Some((v2, job)) = selected else {
        return Ok(free);
    };
    let mut best: Option<(String, PathBuf)> = None;
    for l in mounts.lines() {
        let p: Vec<_> = l.split_whitespace().collect();
        let Some(dash) = p.iter().position(|x| *x == "-") else {
            continue;
        };
        if p.len() <= dash + 3 || p.len() < 6 {
            continue;
        }
        if v2 {
            if p[dash + 1] != "cgroup2" {
                continue;
            }
        } else if p[dash + 1] != "cgroup" || !p[dash + 3].split(',').any(|x| x == "memory") {
            continue;
        }
        let r = unescape(p[3]);
        let r = if r == "/" { String::new() } else { r };
        if job != r && !job.starts_with(&format!("{r}/")) {
            continue;
        }
        if best.as_ref().is_none_or(|(b, _)| r.len() < b.len()) {
            best = Some((r, PathBuf::from(unescape(p[4]))));
        }
    }
    let (r, top) = best.ok_or_else(|| err("memory cgroup isn't mounted here"))?;
    let mut at = top.join(job[r.len()..].trim_start_matches('/'));
    if !at.is_dir() {
        return Err(err(format!("memory cgroup isn't at {}", s(&at))));
    }
    let (max, held, cache) = if v2 {
        ("memory.max", "memory.current", "inactive_file")
    } else {
        (
            "memory.limit_in_bytes",
            "memory.usage_in_bytes",
            "total_inactive_file",
        )
    };
    loop {
        let file = at.join(max);
        if file.try_exists()? {
            let limit = read(&file).map_err(|_| err(format!("{} can't be read", s(&file))))?;
            if limit.trim() != "max" {
                let limit: i128 = limit
                    .trim()
                    .parse::<u64>()
                    .map(i128::from)
                    .map_err(|_| err(format!("{} can't be read", s(&file))))?;
                if limit < 1i128 << 62 {
                    let remaining = (|| -> Result<i128> {
                        Ok(limit - number(&at.join(held))?
                            + field(&read(&at.join("memory.stat"))?, cache)?)
                    })()
                    .map_err(|_| {
                        err(format!(
                            "{} has a limit but doesn't say what it holds",
                            s(&at)
                        ))
                    })?;
                    free = free.min(remaining);
                }
            }
        }
        if at == top {
            break;
        }
        if !at.pop() {
            return Err(err("cgroup ancestor escaped mount"));
        }
    }
    Ok(free)
}
/// What an evaluation needs free to start, and so what `side` gives each.
/// Half of burrow's 163 checks evaluate in under 0.7G and nine take 2-4.8G;
/// `agents` (its sandboxes' VMs and each engine's conformance VM) took 7.7G,
/// and 9.8G with #1476's, all measured on 2026-11-23 (#1484). A reserve sized to the largest would
/// evaluate one at a time on a hosted runner; the watch covers those.
const RESERVE: i128 = 2 << 30;
/// Below this much free the evaluations side by side hold more than the
/// job has: two `agents` together ran a 16G hosted runner out (#1484).
const FLOOR: i128 = 1 << 30;
/// How often the watch reads the room left. An evaluation grows by tens of
/// megabytes a second, well inside [`FLOOR`] between two reads.
const POLL: std::time::Duration = std::time::Duration::from_millis(200);
pub fn side() -> usize {
    match room() {
        Ok(free) => {
            let cores = string(cmd("nproc", &[]))
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            cores.min((free / RESERVE).max(1) as usize).max(1)
        }
        Err(e) => {
            println!("this job's memory limit is unknown ({e}): one evaluation at a time");
            1
        }
    }
}
/// What the watch reads: the room left, and what a process holds.
pub struct Gauge {
    room: Box<dyn Fn() -> Result<i128> + Sync>,
    held: Box<dyn Fn(u32) -> i128 + Sync>,
}
impl Gauge {
    fn machine() -> Self {
        Gauge {
            room: Box::new(room),
            held: Box::new(|pid| {
                read(Path::new(&format!("/proc/{pid}/status")))
                    .ok()
                    .and_then(|t| field(&t, "VmRSS:").ok())
                    .map_or(0, |kb| kb * 1024)
            }),
        }
    }
}
/// A process a task runs under the watch.
struct Running {
    task: usize,
    pid: u32,
    child: std::sync::Arc<std::sync::Mutex<std::process::Child>>,
    what: String,
    stopped: bool,
}
#[derive(Default)]
struct Watch {
    running: std::sync::Mutex<Vec<Running>>,
    /// Each task stopped, and what it was running.
    stopped: std::sync::Mutex<Vec<(usize, String)>>,
}
/// Which of these to stop when the room runs out, by what each holds, in
/// the order they started: the biggest but the eldest, which is the furthest
/// on. None while there's one alone: nothing else of the job's to blame.
fn victim(held: &[i128]) -> Option<usize> {
    (1..held.len()).max_by_key(|&i| (held[i], i))
}
impl Watch {
    /// One look at the room left: below [`FLOOR`], stop one. Not while one
    /// stopped still holds its memory, or the next look would stop another
    /// for what that one is already giving back.
    fn look(&self, gauge: &Gauge) {
        let Ok(room) = (gauge.room)() else {
            return;
        };
        if room >= FLOOR {
            return;
        }
        let mut running = self.running.lock().unwrap();
        if running.iter().any(|r| r.stopped) {
            return;
        }
        let held: Vec<_> = running.iter().map(|r| (gauge.held)(r.pid)).collect();
        let Some(i) = victim(&held) else {
            return;
        };
        let r = &mut running[i];
        if r.child.lock().unwrap().kill().is_ok() {
            r.stopped = true;
            self.stopped.lock().unwrap().push((r.task, r.what.clone()));
            println!(
                "{}G free, under {}G, with {} side by side: stopped {} ({}G), to run again alone once the rest have",
                room >> 30,
                FLOOR >> 30,
                held.len(),
                r.what,
                held[i] >> 30
            );
        }
    }
}
/// The task one worker runs, for what it runs under the watch.
pub struct Slot<'a> {
    task: usize,
    watch: Option<&'a Watch>,
}
impl Slot<'_> {
    /// Outside [`parallel`]: nothing to share the room with, so no watch.
    pub fn alone() -> Slot<'static> {
        Slot {
            task: 0,
            watch: None,
        }
    }
    /// The command's output, as [`Command::output`] gives it, `what` naming
    /// it should the watch stop it, which makes it an error.
    pub fn output(&self, mut c: Command, what: &str) -> Result<Output> {
        let Some(watch) = self.watch else {
            return output(c);
        };
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn()?;
        let pid = child.id();
        let (mut out, mut errs) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        let child = std::sync::Arc::new(std::sync::Mutex::new(child));
        watch.running.lock().unwrap().push(Running {
            task: self.task,
            pid,
            child: child.clone(),
            what: what.to_string(),
            stopped: false,
        });
        let read = |r: &mut dyn io::Read| {
            let mut v = Vec::new();
            r.read_to_end(&mut v).map(|_| v)
        };
        let (stdout, stderr) = std::thread::scope(|s| {
            let e = s.spawn(|| read(&mut errs));
            (read(&mut out), e.join().unwrap())
        });
        let status = loop {
            if let Some(status) = child.lock().unwrap().try_wait()? {
                break status;
            }
            std::thread::sleep(POLL);
        };
        let stopped = {
            let mut running = watch.running.lock().unwrap();
            let i = running.iter().position(|r| r.pid == pid).unwrap();
            running.remove(i).stopped
        };
        if stopped {
            return Err(err(format!(
                "{what} was stopped: the job ran short of memory"
            )));
        }
        Ok(Output {
            status,
            stdout: stdout?,
            stderr: stderr?,
        })
    }
}
/// All workers finish before results are consumed; logs are printed in plan
/// order. Each worker owns its evaluator and roots, never a shell job table.
/// A task starts only while [`RESERVE`] is free (or nothing else runs), and
/// one the watch stopped runs again, alone, once the rest have.
pub fn parallel<T: Sync, R: Send>(
    items: &[T],
    side: usize,
    f: impl Fn(&T, &Slot) -> R + Sync,
) -> Vec<R> {
    within(&Gauge::machine(), items, side, f)
}
fn within<T: Sync, R: Send>(
    gauge: &Gauge,
    items: &[T],
    side: usize,
    f: impl Fn(&T, &Slot) -> R + Sync,
) -> Vec<R> {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    let next = AtomicUsize::new(0);
    let busy = AtomicUsize::new(0);
    let done = AtomicBool::new(false);
    let admit = Mutex::new(());
    let watch = Watch::default();
    let results: Vec<_> = items.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..side.min(items.len()))
            .map(|_| {
                scope.spawn(|| {
                    loop {
                        // One worker at a time looks and takes its place, so
                        // only one takes the start an idle job allows.
                        let turn = admit.lock().unwrap();
                        loop {
                            let idle = busy.load(Ordering::SeqCst) == 0;
                            let short = (gauge.room)().is_ok_and(|r| r < RESERVE);
                            if idle || !short {
                                break;
                            }
                            std::thread::sleep(POLL);
                        }
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        if i >= items.len() {
                            break;
                        }
                        busy.fetch_add(1, Ordering::SeqCst);
                        drop(turn);
                        let slot = Slot {
                            task: i,
                            watch: Some(&watch),
                        };
                        let r = f(&items[i], &slot);
                        busy.fetch_sub(1, Ordering::SeqCst);
                        if !watch.stopped.lock().unwrap().iter().any(|(t, _)| *t == i) {
                            *results[i].lock().unwrap() = Some(r);
                        }
                    }
                })
            })
            .collect();
        scope.spawn(|| {
            while !done.load(Ordering::SeqCst) {
                std::thread::sleep(POLL);
                watch.look(gauge);
            }
        });
        for w in workers {
            w.join().unwrap();
        }
        done.store(true, Ordering::SeqCst);
    });
    for (i, what) in watch.stopped.into_inner().unwrap() {
        println!("running {what} again, alone");
        *results[i].lock().unwrap() = Some(f(&items[i], &Slot::alone()));
    }
    results
        .into_iter()
        .map(|m| m.into_inner().unwrap().unwrap())
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    const G: i128 = 1 << 30;
    #[test]
    fn the_biggest_but_the_eldest_is_stopped() {
        assert_eq!(victim(&[]), None);
        assert_eq!(victim(&[8 * G]), None);
        assert_eq!(victim(&[8 * G, G, 7 * G, G]), Some(2));
        assert_eq!(victim(&[G, 7 * G, 7 * G]), Some(2));
    }
    /// A job with 15G (a 16G hosted runner's) and four cores evaluating two
    /// checks of 8G and three of 1G, the two big ones first, as the negative
    /// put `agents`'s base and negative tree side by side (#1484). A task
    /// holds its weight while its process runs, a big one for longer; the
    /// gauge says what the job's tasks leave, and what a process holds by
    /// how long it sleeps.
    #[test]
    fn two_big_evaluations_never_run_out_the_room_together() {
        const BIG: &str = "1.5";
        let weight = |secs: &str| if secs == BIG { 8 * G } else { G };
        let tasks = [BIG, BIG, "0.3", "0.3", "0.3"];
        let used = Arc::new(Mutex::new(0i128));
        let u = used.clone();
        let gauge = Gauge {
            room: Box::new(move || Ok(15 * G - *u.lock().unwrap())),
            held: Box::new(move |pid| {
                let line = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                let line = String::from_utf8_lossy(&line);
                weight(line.split('\0').nth(1).unwrap_or_default())
            }),
        };
        // Each task that ran to its end: its weight, and what the job held.
        let ends = Mutex::new(Vec::new());
        let results = within(&gauge, &tasks, 4, |secs, slot| {
            let w = weight(secs);
            *used.lock().unwrap() += w;
            let o = slot.output(cmd("sleep", &[secs]), &format!("a {}G evaluation", w >> 30));
            let held = {
                let mut used = used.lock().unwrap();
                *used -= w;
                *used + w
            };
            if o.is_ok() {
                ends.lock().unwrap().push((w >> 30, held >> 30));
            }
            o.is_ok_and(|o| o.status.success())
        });
        assert_eq!(results, vec![true; 5]);
        let ends = ends.into_inner().unwrap();
        assert_eq!(ends.len(), 5, "{ends:?}");
        // A small one can end before the watch has looked; a big one can't.
        assert!(
            ends.iter().all(|&(w, held)| w < 8 || held <= 15),
            "{ends:?}"
        );
        // The one stopped ran again alone.
        assert!(ends.contains(&(8, 8)), "{ends:?}");
    }
    /// With less than [`RESERVE`] free (but more than [`FLOOR`], so the
    /// watch stops nothing), only one task may take the start an idle job
    /// allows, however many workers find it idle at once. The gauge holds
    /// each worker that reads the room until all four are reading it, or
    /// a while has passed: four that each saw the job idle would then all
    /// start.
    #[test]
    fn one_task_at_a_time_while_the_room_is_short() {
        use std::sync::Condvar;
        use std::sync::atomic::{AtomicUsize, Ordering};
        const SIDE: usize = 4;
        let reading = Arc::new((Mutex::new(0usize), Condvar::new()));
        let r = reading.clone();
        let gauge = Gauge {
            room: Box::new(move || {
                let (n, all) = &*r;
                let mut n = n.lock().unwrap();
                *n += 1;
                all.notify_all();
                let wait = std::time::Duration::from_millis(100);
                n = all.wait_timeout_while(n, wait, |n| *n < SIDE).unwrap().0;
                *n -= 1;
                Ok(RESERVE - G / 2)
            }),
            held: Box::new(|_| 0),
        };
        let (now, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
        within(&gauge, &[(); SIDE], SIDE, |_, _| {
            most.fetch_max(now.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(50));
            now.fetch_sub(1, Ordering::SeqCst);
        });
        assert_eq!(most.into_inner(), 1);
    }
    /// Without the watch, as one evaluation alone, a command runs as
    /// [`Command::output`] would run it.
    #[test]
    fn alone_is_plain_output() {
        let o = Slot::alone()
            .output(cmd("sh", &["-c", "echo out; echo err >&2; exit 3"]), "it")
            .unwrap();
        assert_eq!((o.stdout, o.stderr), (b"out\n".to_vec(), b"err\n".to_vec()));
        assert_eq!(o.status.code(), Some(3));
    }
}
