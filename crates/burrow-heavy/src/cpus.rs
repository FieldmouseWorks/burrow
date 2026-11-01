//! How many threads this process may run, counted as `nproc` counts them:
//! gnulib's `num_processors (NPROC_CURRENT_OVERRIDABLE)`, which coreutils'
//! nproc prints and the shell heavy used to read.
//!
//! OMP_NUM_THREADS, when it's a count, is the answer, but no more than
//! OMP_THREAD_LIMIT. Otherwise it's the least of OMP_THREAD_LIMIT, the
//! cgroup v2 CPU quota (unless the process is scheduled in real time) and
//! the CPUs its affinity allows.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// This process's count.
pub fn threads() -> io::Result<u64> {
    count(
        std::env::var("OMP_NUM_THREADS").ok().as_deref(),
        std::env::var("OMP_THREAD_LIMIT").ok().as_deref(),
        || {
            let stat = fs::read_to_string("/proc/self/stat").ok()?;
            if !quota_applies(&stat) {
                return None;
            }
            let cgroup = fs::read_to_string("/proc/self/cgroup").ok()?;
            quota(&cgroup, &cgroup2_mount()?)
        },
        || Ok(u64::from(rustix::thread::sched_getaffinity(None)?.count())),
    )
}

/// The count from the two OpenMP variables, the quota (none when there's
/// none) and the affinity. The last two are asked only when they could
/// lower it, as gnulib asks them.
pub fn count(
    omp_threads: Option<&str>,
    omp_limit: Option<&str>,
    quota: impl FnOnce() -> Option<u64>,
    affinity: impl FnOnce() -> io::Result<u64>,
) -> io::Result<u64> {
    let limit = omp(omp_limit).unwrap_or(u64::MAX);
    if let Some(threads) = omp(omp_threads) {
        return Ok(threads.min(limit));
    }
    let mut limit = limit;
    if limit > 1 {
        limit = limit.min(quota().unwrap_or(u64::MAX));
    }
    if limit > 1 {
        limit = limit.min(affinity()?);
    }
    Ok(limit)
}

/// An OpenMP thread count: a positive decimal, with white space around it
/// or a list's first value (gnulib's parse_omp_threads). Anything else,
/// zero included, sets nothing.
fn omp(value: Option<&str>) -> Option<u64> {
    let v = value?.trim_start_matches(c_space);
    let digits = v.len() - v.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let rest = v[digits..].trim_start_matches(c_space);
    if !(rest.is_empty() || rest.starts_with(',')) {
        return None;
    }
    // strtoul: a count past its range is its largest.
    let n = v[..digits].parse().unwrap_or(u64::MAX);
    (n > 0).then_some(n)
}

/// C's isspace in the C locale.
fn c_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// Whether the scheduler honours a CPU quota for the process whose
/// /proc/PID/stat this is: not under SCHED_FIFO, SCHED_RR or
/// SCHED_DEADLINE, nor when the policy can't be read.
fn quota_applies(stat: &str) -> bool {
    // The command's name, in parentheses, may hold anything; the policy is
    // the 41st field, the 39th after it.
    let policy = stat
        .rfind(')')
        .and_then(|end| stat[end + 1..].split_whitespace().nth(38))
        .and_then(|p| p.parse::<u32>().ok());
    // 1 SCHED_FIFO, 2 SCHED_RR, 6 SCHED_DEADLINE.
    matches!(policy, Some(p) if !matches!(p, 1 | 2 | 6))
}

/// Where the cgroup v2 hierarchy is mounted.
fn cgroup2_mount() -> Option<PathBuf> {
    let root = Path::new("/sys/fs/cgroup");
    if root.join("cgroup.controllers").exists() {
        return Some(root.to_path_buf());
    }
    mount_of(&fs::read_to_string("/proc/mounts").ok()?)
}

/// The first cgroup2 mount in a mount table, its octal escapes read.
fn mount_of(mounts: &str) -> Option<PathBuf> {
    mounts.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let dir = fields.nth(1)?;
        (fields.next()? == "cgroup2").then(|| PathBuf::from(unescape(dir)))
    })
}

/// A mount table's `\NNN` escapes (a space is `\040`), as getmntent reads
/// them.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes
            .get(i + 1..i + 4)
            .filter(|d| d.iter().all(|b| (b'0'..=b'7').contains(b)));
        match (bytes[i], octal) {
            (b'\\', Some(d)) => {
                out.push(d.iter().fold(0u8, |n, b| n.wrapping_mul(8) + (b - b'0')));
                i += 4;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The lowest CPU quota on the way from the process's cgroup (from
/// /proc/self/cgroup) up to the root, in CPUs rounded to the nearest and at
/// least one; none when no level has one.
fn quota(proc_cgroup: &str, mount: &Path) -> Option<u64> {
    let path = proc_cgroup
        .lines()
        .find_map(|l| l.strip_prefix("0::").filter(|p| p.starts_with('/')))?;
    let mut lowest: Option<u64> = None;
    let mut level = Some(path);
    while let Some(p) = level {
        let file = mount.join(p.trim_start_matches('/')).join("cpu.max");
        if let Some(cpus) = fs::read_to_string(file).ok().as_deref().and_then(cpu_max) {
            lowest = Some(lowest.map_or(cpus, |l| l.min(cpus)));
            if cpus == 1 {
                break;
            }
        }
        level = match p.rfind('/') {
            Some(0) if p.len() > 1 => Some("/"),
            Some(i) if i > 0 => Some(&p[..i]),
            _ => None,
        };
    }
    lowest
}

/// A cpu.max's quota in CPUs: `QUOTA PERIOD`, rounded half up and at least
/// one. `max` (no quota) and anything else that isn't two numbers set none.
fn cpu_max(text: &str) -> Option<u64> {
    let mut fields = text.split_whitespace();
    let quota: i64 = fields.next()?.parse().ok()?;
    let period: i64 = fields.next()?.parse().ok()?;
    if period == 0 {
        return None;
    }
    let cpus = (quota as f64 / period as f64 + 0.5) as i64;
    Some(cpus.max(1).unsigned_abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counted(
        threads: Option<&str>,
        limit: Option<&str>,
        quota: Option<u64>,
        affinity: u64,
    ) -> u64 {
        count(threads, limit, || quota, || Ok(affinity)).unwrap()
    }

    #[test]
    fn openmp_threads_win_up_to_the_limit() {
        assert_eq!(
            counted(Some("4"), None, Some(2), 12),
            4,
            "past the quota and affinity too"
        );
        assert_eq!(counted(Some("64"), None, None, 12), 64);
        assert_eq!(counted(Some("8"), Some("3"), None, 12), 3);
        assert_eq!(counted(Some(" 6 "), Some("\t9,4"), None, 12), 6);
        assert_eq!(counted(Some("5,2,1"), None, None, 12), 5);
    }

    #[test]
    fn otherwise_the_least_of_the_limit_the_quota_and_the_affinity() {
        assert_eq!(counted(None, None, None, 12), 12);
        assert_eq!(counted(None, Some("1"), None, 12), 1);
        assert_eq!(counted(None, Some("5"), None, 12), 5);
        assert_eq!(counted(None, Some("20"), None, 12), 12);
        assert_eq!(counted(None, None, Some(3), 12), 3);
        assert_eq!(counted(None, Some("2"), Some(3), 12), 2);
        assert_eq!(counted(None, None, Some(30), 12), 12);
    }

    #[test]
    fn what_cant_lower_the_count_isnt_asked() {
        let quota = || -> Option<u64> { panic!("quota asked") };
        let affinity = || -> io::Result<u64> { panic!("affinity asked") };
        assert_eq!(count(Some("3"), None, quota, affinity).unwrap(), 3);
        assert_eq!(count(None, Some("1"), quota, affinity).unwrap(), 1);
        assert_eq!(count(None, None, || Some(1), affinity).unwrap(), 1);
        let gone = || Err(io::Error::other("no affinity"));
        assert!(count(None, None, || None, gone).is_err());
    }

    #[test]
    fn an_openmp_value_that_isnt_a_count_sets_nothing() {
        for v in [
            "", " ", "0", "0,4", "-2", "+2", "2x", "x2", "2 3", "two", "2.5", ",2",
        ] {
            assert_eq!(omp(Some(v)), None, "{v:?}");
        }
        assert_eq!(counted(Some("0"), Some("0"), None, 12), 12);
        assert_eq!(omp(Some("\n7\r\n")), Some(7));
        assert_eq!(omp(Some("99999999999999999999999")), Some(u64::MAX));
        assert_eq!(omp(None), None);
    }

    /// A /proc/PID/stat line whose name has spaces and a parenthesis.
    fn stat(policy: u32) -> String {
        let mut fields: Vec<String> = (3..=52).map(|n| n.to_string()).collect();
        fields[41 - 3] = policy.to_string();
        format!("1234 (a (b) c) {}\n", fields.join(" "))
    }

    #[test]
    fn a_real_time_process_has_no_quota() {
        for (policy, applies) in [
            (0, true),
            (3, true),
            (5, true),
            (1, false),
            (2, false),
            (6, false),
        ] {
            assert_eq!(quota_applies(&stat(policy)), applies, "policy {policy}");
        }
        assert!(!quota_applies("1234 (short) R 1"), "a policy it can't read");
        assert!(quota_applies(
            &fs::read_to_string("/proc/self/stat").unwrap()
        ));
    }

    #[test]
    fn the_cgroup2_mount_is_the_first_in_the_table() {
        let table = "proc /proc proc rw 0 0\ncgroup /sys/fs/cgroup/cpu cgroup rw,cpu 0 0\n\
                     cgroup2 /run/my\\040cgroups cgroup2 rw 0 0\ncgroup2 /other cgroup2 rw 0 0\n";
        assert_eq!(mount_of(table), Some(PathBuf::from("/run/my cgroups")));
        assert_eq!(mount_of("proc /proc proc rw 0 0\n"), None);
        assert_eq!(unescape("a\\134b\\011c\\9d\\04"), "a\\b\tc\\9d\\04");
    }

    #[test]
    fn cpu_max_is_cpus_rounded_half_up() {
        assert_eq!(cpu_max("max 100000\n"), None);
        assert_eq!(cpu_max("200000 100000\n"), Some(2));
        assert_eq!(cpu_max("150000 100000\n"), Some(2));
        assert_eq!(cpu_max("149999 100000"), Some(1));
        assert_eq!(cpu_max("250000 100000"), Some(3));
        assert_eq!(cpu_max("10000 100000"), Some(1), "at least one");
        assert_eq!(cpu_max("50000 0"), None);
        assert_eq!(cpu_max("50000"), None);
        assert_eq!(cpu_max(""), None);
    }

    /// A cgroup tree with these cpu.max files (a path from the root, and
    /// its contents).
    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let d = dir.path().join(path.trim_start_matches('/'));
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("cpu.max"), text).unwrap();
        }
        dir
    }

    const SELF: &str = "1:cpu:/\n0::/runner.slice/job.scope\n";

    #[test]
    fn the_quota_is_the_lowest_from_the_cgroup_up() {
        let t = tree(&[
            ("/runner.slice/job.scope", "max 100000\n"),
            ("/runner.slice", "400000 100000\n"),
            ("/", "800000 100000\n"),
        ]);
        assert_eq!(quota(SELF, t.path()), Some(4));
        let t = tree(&[
            ("/runner.slice/job.scope", "300000 100000\n"),
            ("/runner.slice", "600000 100000\n"),
        ]);
        assert_eq!(quota(SELF, t.path()), Some(3));
        let t = tree(&[("/", "200000 100000\n")]);
        assert_eq!(quota(SELF, t.path()), Some(2), "the root counts");
        assert_eq!(quota("0::/\n", t.path()), Some(2));
        let t = tree(&[("/runner.slice/job.scope", "max 100000\n")]);
        assert_eq!(quota(SELF, t.path()), None);
        assert_eq!(quota("1:cpu:/\n", t.path()), None, "no cgroup v2");
        let t = tree(&[("/runner.slice", "200000 100000\n")]);
        assert_eq!(
            quota("0::/runner.slice/job.scope\n0::/other\n", t.path()),
            Some(2),
            "the first v2 line"
        );
    }
}
