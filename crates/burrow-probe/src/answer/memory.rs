//! `memory`: RAM and swap as /proc/meminfo has them, the ZFS ARC, the
//! kernel's memory pressure, and the cgroups holding the most.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::{Machine, size};

/// How deep the cgroup tree is read: deep enough for a user's units
/// (user.slice/user-UID.slice/user@UID.service/claude.slice/claude-rc.slice/UNIT).
const DEPTH: usize = 6;

/// How many cgroups the answer lists.
const TOP: usize = 20;

pub fn memory(m: &Machine) -> Result<String, String> {
    let meminfo =
        fs::read_to_string(m.path("/proc/meminfo")).map_err(|e| format!("/proc/meminfo: {e}"))?;
    let kb: BTreeMap<&str, u64> = meminfo
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.trim(), v.split_whitespace().next()?.parse().ok()?))
        })
        .collect();
    let get = |k: &str| kb.get(k).copied().unwrap_or(0) * 1024;
    let mut out = format!(
        "RAM {} total, {} available. Swap {} total, {} free.\n",
        size(get("MemTotal")),
        size(get("MemAvailable")),
        size(get("SwapTotal")),
        size(get("SwapFree")),
    );
    if let Ok(arc) = fs::read_to_string(m.path("/proc/spl/kstat/zfs/arcstats")) {
        let stat = |name: &str| {
            arc.lines()
                .find_map(|l| {
                    let mut w = l.split_whitespace();
                    (w.next() == Some(name)).then(|| w.nth(1)?.parse::<u64>().ok())?
                })
                .unwrap_or(0)
        };
        out += &format!(
            "ZFS ARC {} (target {}, most {}).\n",
            size(stat("size")),
            size(stat("c")),
            size(stat("c_max"))
        );
    }
    if let Ok(p) = fs::read_to_string(m.path("/proc/pressure/memory")) {
        out += "Pressure:\n";
        for l in p.lines() {
            out += &format!("  {l}\n");
        }
    }
    let root = m.path("/sys/fs/cgroup");
    let mut groups = Vec::new();
    walk(&root, &root, 0, &mut groups);
    groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out += &format!("The {TOP} cgroups holding the most (now, peak, swap):\n");
    if groups.is_empty() {
        out += "  none it can read\n";
    }
    for (name, now, peak, swap) in groups.iter().take(TOP) {
        out += &format!(
            "  {:>7} {:>7} {:>7}  {name}\n",
            size(*now),
            peak.map_or("-".into(), size),
            swap.map_or("-".into(), size),
        );
    }
    Ok(out)
}

/// Every cgroup under `dir` with a memory.current, to DEPTH: its path from
/// the root, now, peak and swap.
fn walk(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<(String, u64, Option<u64>, Option<u64>)>,
) {
    let read = |name: &str| -> Option<u64> {
        fs::read_to_string(dir.join(name)).ok()?.trim().parse().ok()
    };
    if let Some(now) = read("memory.current") {
        let name = dir.strip_prefix(root).unwrap_or(dir).display().to_string();
        out.push((name, now, read("memory.peak"), read("memory.swap.current")));
    }
    if depth == DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut dirs: Vec<_> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    for d in dirs {
        walk(root, &d, depth + 1, out);
    }
}
