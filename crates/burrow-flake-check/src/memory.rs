//! Bound concurrent evaluations by both the machine and every visible
//! cgroup ancestor. An unreadable limit means one evaluation, never infinity.
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
pub fn side() -> usize {
    match room() {
        Ok(free) => {
            let cores = string(cmd("nproc", &[]))
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            cores.min((free / 2147483648).max(1) as usize).max(1)
        }
        Err(e) => {
            println!("this job's memory limit is unknown ({e}): one evaluation at a time");
            1
        }
    }
}
/// All workers finish before results are consumed; logs are printed in plan
/// order. Each worker owns its evaluator and roots, never a shell job table.
pub fn parallel<T: Sync, R: Send>(items: &[T], side: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let next = AtomicUsize::new(0);
    let results: Vec<_> = items.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..side.min(items.len()) {
            let f = &f;
            let next = &next;
            let results = &results;
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= items.len() {
                        break;
                    }
                    *results[i].lock().unwrap() = Some(f(&items[i]));
                }
            });
        }
    });
    results
        .into_iter()
        .map(|m| m.into_inner().unwrap().unwrap())
        .collect()
}
