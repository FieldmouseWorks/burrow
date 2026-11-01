//! `HOST-backup status`: where the backups stand.

use std::fs;
use std::io::{BufRead, BufReader};
use std::process::Stdio;

use crate::pass::Last;
use crate::{Run, read, stamp};

/// What the state says: the last backup, a failure, the day's upkeep and
/// what it found.
pub fn local(run: &Run) -> Result<String, String> {
    let state = &run.cfg.state;
    let mut out = String::new();
    match fs::read(state.join("last.json")) {
        Ok(b) => match serde_json::from_slice::<Last>(&b) {
            Ok(last) => out.push_str(&format!("last backup {}: {}\n", last.time, last.line())),
            Err(e) => out.push_str(&format!(
                "last backup: {}/last.json doesn't read ({e})\n",
                state.display()
            )),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => out.push_str("no backup yet\n"),
        Err(e) => return Err(format!("{}/last.json: {e}", state.display())),
    }
    if let Some(since) = read(&state.join("failed-since"))? {
        let shown = since.trim().parse().map_or(since.clone(), stamp);
        out.push_str(&format!("failing since {shown}\n"));
    }
    if let Ok(t) = fs::metadata(state.join("maintained")).and_then(|m| m.modified()) {
        let epoch = jiff::Timestamp::try_from(t).map_or(0, |t| t.as_second());
        out.push_str(&format!("pruned, checked and drilled {}\n", stamp(epoch)));
    }
    if let Some(found) = read(&state.join("problems"))?.filter(|p| !p.is_empty()) {
        out.push_str(&format!("and found:\n{found}\n"));
    }
    Ok(out)
}

/// What the repository says: the last three backups and its size.
pub fn repository(run: &Run) {
    let listed = run
        .restic()
        .args([
            "snapshots",
            "--compact",
            "--host",
            &run.cfg.host,
            "--latest",
            "3",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !listed {
        println!("repository unreachable");
    }
    let Ok(mut stats) = run
        .restic()
        .args(["stats", "--mode", "raw-data"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(out) = stats.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if ["Total Size", "Total Uncompressed Size", "Compression Ratio"]
                .iter()
                .any(|w| line.contains(w))
            {
                println!("{line}");
            }
        }
    }
    let _ = stats.wait();
}
