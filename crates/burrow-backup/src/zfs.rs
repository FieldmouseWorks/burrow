//! The datasets' snapshots and the backup's holds on them, through zfs.

use std::collections::BTreeSet;
use std::process::Stdio;

use crate::{Run, glob};

/// The ZFS hold on the snapshot the newest backup was read from.
pub const HOLD: &str = "burrow-backup";

/// The newest snapshot matching `pattern` that every dataset has, as its
/// name and creation time (seconds since the epoch).
pub fn newest(run: &Run, pattern: &str) -> Result<Option<(String, i64)>, String> {
    let (first, others) = run
        .cfg
        .datasets
        .split_first()
        .ok_or("no datasets to back up")?;
    let theirs: Vec<BTreeSet<String>> = others
        .iter()
        .map(|ds| Ok(snapshots(&list(run, ds, "name")?).map(|(n, _)| n).collect()))
        .collect::<Result<_, String>>()?;
    let listed = list(run, first, "name,creation")?;
    for (name, creation) in snapshots(&listed) {
        if glob::matches(pattern, &name) && theirs.iter().all(|t| t.contains(&name)) {
            let creation = creation
                .parse()
                .map_err(|_| format!("zfs gave {first}@{name} the creation time {creation:?}"))?;
            return Ok(Some((name, creation)));
        }
    }
    Ok(None)
}

/// `zfs list` of a dataset's snapshots, newest first: one line each.
fn list(run: &Run, ds: &str, fields: &str) -> Result<String, String> {
    run.zfs(&[
        "list", "-H", "-p", "-t", "snapshot", "-o", fields, "-S", "creation", "-d", "1", ds,
    ])
}

/// Each line's snapshot name (after its `@`) and the rest of the line.
fn snapshots(listed: &str) -> impl Iterator<Item = (String, String)> + '_ {
    listed.lines().filter_map(|l| {
        let (full, rest) = l.split_once('\t').unwrap_or((l, ""));
        let (_, name) = full.split_once('@')?;
        Some((name.to_owned(), rest.to_owned()))
    })
}

/// Whether `ds@name` is there.
pub fn exists(run: &Run, ds: &str, name: &str) -> bool {
    run.command("zfs")
        .args(["list", "-H", "-o", "name", &format!("{ds}@{name}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Holds every dataset's `name`, as far as it can: one already held stays so.
pub fn hold(run: &Run, name: &str) {
    for ds in &run.cfg.datasets {
        let _ = run
            .command("zfs")
            .args(["hold", HOLD, &format!("{ds}@{name}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Lets go of the backup's holds on every snapshot of the datasets but
/// `keep`'s (the newest one off the box, if any): ones from runs that died
/// stay held no longer than the next run.
pub fn release_except(run: &Run, keep: Option<&str>) -> Result<(), String> {
    for ds in &run.cfg.datasets {
        let listed = run.zfs(&[
            "list",
            "-H",
            "-t",
            "snapshot",
            "-o",
            "name,userrefs",
            "-d",
            "1",
            ds,
        ])?;
        for (name, refs) in snapshots(&listed) {
            if refs.trim() == "0" || keep == Some(name.as_str()) {
                continue;
            }
            let snap = format!("{ds}@{name}");
            let holds = run.zfs(&["holds", "-H", &snap])?;
            if holds.lines().any(|l| l.split('\t').nth(1) == Some(HOLD)) {
                run.zfs(&["release", HOLD, &snap])?;
            }
        }
    }
    Ok(())
}

impl Run<'_> {
    /// zfs's output, or why it failed.
    pub fn zfs(&self, args: &[&str]) -> Result<String, String> {
        let out = self
            .command("zfs")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("zfs: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "zfs {} failed ({}): {}",
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim_end()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::snapshots;

    #[test]
    fn a_listing_reads_into_names() {
        let got: Vec<_> =
            snapshots("tank/data@b\t200\ntank/data@a\t100\nnoat\t5\ntank/data@c d\t1\n").collect();
        assert_eq!(
            got,
            [
                ("b".to_owned(), "200".to_owned()),
                ("a".to_owned(), "100".to_owned()),
                ("c d".to_owned(), "1".to_owned())
            ]
        );
    }
}
