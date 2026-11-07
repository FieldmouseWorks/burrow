//! R1: what deletes or rewrites working files gets a ZFS snapshot of the
//! undo datasets first (burrow.undo.datasets). Snapshots are O(1) and
//! pruned after a few days (modules/undo.nix), so this turns `rm -rf`,
//! `git clean`, `git reset --hard` and the like into instant undo instead
//! of a question.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::{any_line, pattern};

/// What deletes or rewrites files, wherever it starts: bare, after a path
/// (`/bin/rm`), or inside quotes, a subshell or a substitution:
/// - rm and friends, and every git command that can drop uncommitted work
///   (checkout, switch, restore and reset all can, in their path and forced
///   forms);
/// - a redirect, which truncates its target, whichever descriptor it's
///   for (`>`, `2>`, `&>`), unless it's into /dev (`2>/dev/null` and
///   friends) or onto another descriptor (`2>&1`, `>&2`);
/// - rsync --delete and find -delete;
/// - mv, cp, tee and in-place sed and perl, which overwrite.
static DESTROYS: LazyLock<Regex> = LazyLock::new(|| {
    pattern(concat!(
        r#"(^|[;&|[:space:](/'"`])(rm|rmdir|shred|truncate|unlink|git([[:space:]]+-[Cc][[:space:]]+[^[:space:]]+)*[[:space:]]+(clean|reset|checkout|switch|restore|stash|branch[[:space:]]+-D|worktree[[:space:]]+remove|filter-branch|filter-repo))([[:space:]]|$)"#,
        r"|(^|[^>])>>?[[:space:]]*([^&>/[:space:]]|/[^d]|/d[^e])",
        r"|rsync[[:space:]].*--delete",
        r"|find[[:space:]].*-delete",
        r#"|(^|[;&|[:space:](/'"`])(mv|cp|tee|sed[[:space:]]+-i|perl[[:space:]]+-p?i)[[:space:]]"#,
    ))
});

/// Seconds that one round of snapshots covers.
const BURST: u64 = 5;

pub fn destroys(cmd: &str) -> bool {
    any_line(&DESTROYS, cmd)
}

/// Whether a patch in `text`, as Codex's apply_patch reads one (its own
/// tool's input, or a command that runs `apply_patch`), deletes or
/// replaces a file: a `*** Delete File:`, a `*** Move to:`, which takes the
/// place of whatever is there, or an `*** Add File:` onto a path that's
/// already there, which it overwrites. Paths are from `dir`, where the
/// patch is applied; with no `dir` (a command that may have moved before
/// it applies the patch: [`patch_dir`]), any Add File may land on a file
/// and counts. An `*** Update File:` alone edits in place, its hunks the
/// undo, as an edit is.
pub fn patch_destroys(text: &str, dir: Option<&Path>) -> bool {
    text.lines().map(str::trim_start).any(|line| {
        if line.starts_with("*** Delete File:") || line.starts_with("*** Move to:") {
            return true;
        }
        line.strip_prefix("*** Add File:").is_some_and(|path| {
            let path = path.trim();
            path.is_empty() || dir.is_none_or(|dir| dir.join(path).symlink_metadata().is_ok())
        })
    })
}

/// The one command line shape whose patch's directory the guard knows: the
/// whole command is `apply_patch` with its patch as a heredoc, optionally
/// after `cd DIR &&` with DIR a plain path (Codex's own reading of a
/// command it applies itself). Anything else (another `cd`, `pushd`, a
/// subshell, a nested shell, a variable, more after the patch) may apply
/// the patch anywhere, so its directory isn't known.
static PATCH_COMMAND: LazyLock<Regex> = LazyLock::new(|| {
    pattern(concat!(
        r#"^[[:space:]]*(cd[[:space:]]+([A-Za-z0-9_./@%+=:,][A-Za-z0-9_./@%+=:,-]*|'[^'\\$`]*'|"[^"\\$`]*")[[:space:]]*&&[[:space:]]*)?"#,
        r#"apply_patch[[:space:]]*<<[[:space:]]*('([A-Za-z0-9_]+)'|"([A-Za-z0-9_]+)"|([A-Za-z0-9_]+))[ \t]*\n"#,
    ))
});

/// Where a Bash `cmd` run at `cwd` applies its patch, when that's known
/// ([`PATCH_COMMAND`]); `None` when it may be anywhere.
pub fn patch_dir(cmd: &str, cwd: &Path) -> Option<PathBuf> {
    let found = PATCH_COMMAND.captures(cmd)?;
    let delim = found.get(4).or(found.get(5)).or(found.get(6))?.as_str();
    // The heredoc ends at its delimiter's line, and nothing may follow.
    let body = &cmd[found.get(0)?.end()..];
    let mut lines = body.split_inclusive('\n');
    lines.find(|l| l.trim_end_matches('\n') == delim)?;
    if !lines.all(|l| l.trim().is_empty()) {
        return None;
    }
    let to = found.get(2).map(|d| d.as_str().trim_matches(['\'', '"']));
    Some(to.map_or_else(|| cwd.to_owned(), |to| cwd.join(to)))
}

/// Takes the undo snapshot, one round per burst: `datasets@pre-STAMP`, all
/// at once. `None` once it's taken (or one was, this burst); the question
/// when it couldn't be, since then there'd be no undo.
pub fn snapshot(zfs: &Path, datasets: &[String], mark: &Path, now: SystemTime) -> Option<String> {
    if !due(mark, now) {
        return None;
    }
    let stamp = jiff::Zoned::try_from(now)
        .unwrap_or_else(|_| jiff::Zoned::now())
        .strftime("%Y%m%d-%H%M%S")
        .to_string();
    let names = datasets.iter().map(|d| format!("{d}@pre-{stamp}"));
    let failed = match Command::new(zfs)
        .arg("snapshot")
        .args(names)
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => None,
        Ok(out) => {
            let said = String::from_utf8_lossy(&[out.stdout, out.stderr].concat())
                .trim()
                .to_owned();
            Some(if said.is_empty() {
                "zfs snapshot failed".to_owned()
            } else {
                said
            })
        }
        Err(e) => Some(format!("{}: {e}", zfs.display())),
    };
    if let Some(why) = failed {
        return Some(format!(
            "Couldn't take the undo snapshot first ({why}), so this would have no undo."
        ));
    }
    // Only a taken snapshot holds off the next: a failed one asks again.
    let _ = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(mark)
        .and_then(|f| f.set_modified(now));
    None
}

/// Whether this burst has had no snapshot yet: none was ever marked, or the
/// mark is a burst old, in whole seconds. A mark from the future (the clock
/// went back) can't vouch for anything, so it's due then too.
fn due(mark: &Path, now: SystemTime) -> bool {
    let secs = |t: SystemTime| t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    match std::fs::metadata(mark).and_then(|m| m.modified()) {
        Err(_) => true,
        Ok(at) => secs(at) > secs(now) || secs(now) - secs(at) >= BURST,
    }
}
