//! The drill: check the newest backup against the snapshot it was read from
//! (held until the next one is off the box, so it's there), then restore
//! some paths from it to scratch and compare them with the snapshot.

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Stdio;

use serde_json::Value;

use crate::mounts::Mounts;
use crate::{Run, backup_flags, listing, short, snapshot, tail, write, zfs};

/// A backup in the repository: its ID and the snapshot it was read from.
pub struct Backup {
    pub id: String,
    pub name: String,
}

/// The newest backup of this machine.
pub fn latest(run: &Run) -> Option<Backup> {
    let out = run
        .restic()
        .args([
            "snapshots",
            "--json",
            "--latest",
            "1",
            "--host",
            &run.cfg.host,
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let all: Vec<Value> = serde_json::from_slice(&out.stdout).ok()?;
    // The last of the newest, as `sort_by(.time) | last` picks.
    let newest = all.iter().max_by(|a, b| time(a).cmp(time(b)))?;
    Some(Backup {
        id: newest.get("id")?.as_str()?.to_owned(),
        name: newest
            .get("tags")
            .and_then(|t| t.get(0))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    })
}

fn time(v: &Value) -> &str {
    v.get("time").and_then(Value::as_str).unwrap_or("")
}

/// Checks the newest backup holds everything its snapshot had, then
/// restores `paths` from it and compares them: whether it all held.
pub fn drill(run: &Run, paths: &[String]) -> bool {
    let Some(b) = latest(run) else {
        println!("drill: no backup to restore");
        return false;
    };
    let cfg = run.cfg;
    for ds in &cfg.datasets {
        if !zfs::exists(run, ds, &b.name) {
            println!(
                "drill: {ds}@{} is gone, nothing to check backup {} against",
                b.name,
                short(&b.id)
            );
            return false;
        }
    }
    let mounts = match Mounts::find(&cfg.datasets) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return false;
        }
    };
    let dir = match fs::create_dir_all(&cfg.scratch).and_then(|()| {
        tempfile::Builder::new()
            .prefix(&format!("{}-drill.", cfg.name))
            .tempdir_in(&cfg.scratch)
    }) {
        Ok(d) => d,
        Err(e) => {
            println!(
                "drill: no directory to restore to in {}: {e}",
                cfg.scratch.display()
            );
            return false;
        }
    };
    // A cache to restic, so no backup reads it should a snapshot catch it.
    if let Err(e) = fs::write(
        dir.path().join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    ) {
        println!("drill: {}: {e}", dir.path().display());
        return false;
    }
    if !holds_all(run, &b, &mounts.paths()) {
        return false;
    }
    paths.is_empty() || restores(run, dir.path(), &b, paths)
}

/// Forgets the drill's fresh backup: the one tagged with the random tag in
/// state/probe, which nothing but that drill could have given a snapshot.
pub fn forget_probe(run: &Run) -> Result<(), String> {
    let probe = run.cfg.state.join("probe");
    let Some(tag) = crate::read(&probe)? else {
        return Ok(());
    };
    if is_probe_tag(&tag) {
        let out = run
            .restic()
            .args([
                "snapshots",
                "--json",
                "--host",
                &format!("{}-drill", run.cfg.host),
                "--tag",
                &tag,
            ])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("restic: {e}"))?;
        if !out.status.success() {
            return Err(format!("restic snapshots failed ({})", out.status));
        }
        let found: Vec<Value> =
            serde_json::from_slice(&out.stdout).map_err(|e| format!("restic snapshots: {e}"))?;
        let ids: Vec<&str> = found.iter().filter_map(|s| s.get("id")?.as_str()).collect();
        if !ids.is_empty() {
            let ok = run
                .restic()
                .args(["forget", "-q"])
                .args(&ids)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                return Err("restic forget failed".to_owned());
            }
        }
    }
    crate::remove(&probe)
}

/// `drill-` and 32 hex digits.
fn is_probe_tag(tag: &str) -> bool {
    tag.strip_prefix("drill-").is_some_and(|h| {
        h.len() == 32
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Everything the snapshot has (at `paths`, the datasets), backup `b` has, as
/// the backup's own rules leave it: a fresh backup of the same snapshot
/// lists the same entries, of the same kind, mode, owner, size, times and
/// inode. Not the same device: each mount of a snapshot gets its own, and
/// restic keeps it for hardlinked files. The fresh backup is forgotten after.
fn holds_all(run: &Run, b: &Backup, paths: &[String]) -> bool {
    let cfg = run.cfg;
    // The last drill's, if it died before it could forget it.
    if forget_probe(run).is_err() {
        println!("drill: couldn't forget the fresh backup a drill left behind");
        return false;
    }
    let tag = match probe_tag() {
        Ok(t) => t,
        Err(e) => {
            println!("drill: no tag for a fresh backup: {e}");
            return false;
        }
    };
    if let Err(e) = write(&cfg.state.join("probe"), format!("{tag}\n").as_bytes()) {
        println!("drill: {e}");
        return false;
    }
    let mut cmd: Vec<OsString> = ["restic", "backup", "--json", "--quiet", "--host"]
        .map(OsString::from)
        .into();
    cmd.extend(
        [
            format!("{}-drill", cfg.host),
            "--tag".into(),
            tag,
            "--parent".into(),
            b.id.clone(),
        ]
        .map(OsString::from),
    );
    cmd.extend(backup_flags(cfg));
    cmd.extend(paths.iter().map(OsString::from));
    let out = match snapshot::command(run, &b.name, None, &cmd)
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            println!("drill: a fresh backup of {}: {e}", b.name);
            return false;
        }
    };
    if !out.status.success() {
        println!(
            "drill: a fresh backup of {} failed:\n{}",
            b.name,
            tail(&out.stderr, 5)
        );
        return false;
    }
    let Some(check) =
        summary(&out.stdout).and_then(|s| s.get("snapshot_id")?.as_str().map(str::to_owned))
    else {
        println!("drill: a fresh backup of {} made no snapshot", b.name);
        return false;
    };
    let listed = listing::fields(run, &b.id, paths)
        .and_then(|held| Ok((held, listing::fields(run, &check, paths)?)));
    // Kept in state/probe until it's gone, so the next drill tries again.
    let _ = forget_probe(run);
    let Ok((held, fresh)) = listed else {
        println!("drill: couldn't list backup {}", short(&b.id));
        return false;
    };
    if held != fresh {
        println!(
            "drill: backup {} doesn't hold all of {}; where a fresh backup of it differs (< the backup, > fresh):",
            short(&b.id),
            b.name
        );
        let (only_held, only_fresh) = listing::differ(&held, &fresh);
        let lines = only_held
            .into_iter()
            .map(|r| format!("< {}", listing::path_of(r)))
            .chain(
                only_fresh
                    .into_iter()
                    .map(|r| format!("> {}", listing::path_of(r))),
            );
        for line in lines.take(20) {
            println!("{line}");
        }
        return false;
    }
    println!("drill: backup {} holds all of {}", short(&b.id), b.name);
    true
}

/// A tag no other backup has: `drill-` and 16 random bytes in hex.
fn probe_tag() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    Ok(format!(
        "drill-{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

/// The last summary restic's `backup --json` printed.
pub fn summary(stdout: &[u8]) -> Option<Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .rfind(|v| v.get("message_type").and_then(Value::as_str) == Some("summary"))
}

/// Restores `paths` from backup `b` to `dir`/tree and compares them with the
/// snapshot they were read from.
fn restores(run: &Run, dir: &Path, b: &Backup, paths: &[String]) -> bool {
    let Ok(expected) = listing::names(run, &b.id, paths) else {
        println!("drill: couldn't list backup {}", short(&b.id));
        return false;
    };
    let mut nul = Vec::new();
    for e in &expected {
        nul.extend_from_slice(e.as_bytes());
        nul.push(0);
    }
    if let Err(e) = fs::write(dir.join("expected"), nul) {
        println!("drill: {}: {e}", dir.display());
        return false;
    }
    let restored = run
        .restic()
        .arg("restore")
        .arg(&b.id)
        .arg("--target")
        .arg(dir.join("tree"))
        .args(
            paths
                .iter()
                .flat_map(|p| ["--include".to_owned(), include(p)]),
        )
        .arg("-q")
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !restored {
        println!("drill: restic restore {} failed", short(&b.id));
        return false;
    }
    let mut cmd = run.itself();
    cmd.push("__compare".into());
    cmd.push(snapshot::ns(run).join("drill").into());
    cmd.extend(paths.iter().map(OsString::from));
    snapshot::command(run, &b.name, Some(dir), &cmd)
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A path as restic's `--include` takes it: its pattern characters as
/// themselves.
fn include(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    for c in p.chars() {
        if matches!(c, '\\' | '*' | '?' | '[') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_tag_is_the_drills_own() {
        let t = probe_tag().unwrap();
        assert!(is_probe_tag(&t), "{t}");
        assert_ne!(probe_tag().unwrap(), t);
        for not in [
            "drill",
            "drill-",
            &format!("drill-{}", "0".repeat(31)),
            &format!("drill-{}", "A".repeat(32)),
            &format!("x{t}"),
            &format!("{t}0"),
        ] {
            assert!(!is_probe_tag(not), "{not}");
        }
        assert!(is_probe_tag(&format!(
            "drill-{}",
            "0123456789abcdef".repeat(2)
        )));
    }

    #[test]
    fn an_include_takes_a_path_as_it_is() {
        assert_eq!(
            include("/srv/keep it/a*b?c[d]\\e"),
            "/srv/keep it/a\\*b\\?c\\[d]\\\\e"
        );
    }

    #[test]
    fn the_last_summary_counts() {
        let out = b"{\"message_type\":\"status\"}\nnoise\n{\"message_type\":\"summary\",\"snapshot_id\":\"a\"}\n{\"message_type\":\"summary\",\"snapshot_id\":\"b\"}\n";
        assert_eq!(summary(out).unwrap()["snapshot_id"], "b");
        assert!(summary(b"{\"message_type\":\"status\"}\n").is_none());
    }
}
