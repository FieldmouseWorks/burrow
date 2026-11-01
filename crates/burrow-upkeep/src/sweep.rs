//! What the reviewers leave of a repo's PRs, swept once nothing needs it:
//! the snapshots (snapshot.rs), the queue entries (queue.rs) and the
//! answers of heads its open PRs don't have any more, and the reviewers'
//! sessions of PRs that closed or were retargeted, marked gone for the
//! reviewers' service to delete (burrow-review's work.rs `forget`). A pass
//! sweeps each repo whose PRs it read, once it has seen to them; a repo
//! started over as new is swept of everything (repos.py `reset`).

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use burrow_review::queue::{GONE, remove, reviewer_of, session_name, taken};

use crate::queue::{DONE, QUEUE, SESSIONS};
use crate::snapshot::META;

/// What's under the state dir (and the snapshots' dir) a sweep looks at:
/// the queue first, since a look still under way keeps its snapshot.
#[derive(Clone, Copy)]
enum Part {
    Queue,
    Done,
    Meta,
    Tree,
}

/// The snapshots, queue entries and answers of the repo's heads none of its
/// open PRs has any more (`live`: the snapshot names of the heads they
/// have), and the sessions of its PRs that aren't open on the base they
/// were (`live` again). `key` is the repo as the steward's state files
/// spell it (`pr-KEY-N.json`). An open PR's earlier head keeps what came
/// back, for the PR's next pass to post on the commit it read or drop
/// (review_step.py's `overtaken`). A look the reviewers' service is on
/// stays, whoever's it is, with the snapshot it reads and the session it's
/// in, until it ends: so nothing of it is left to write after the sweep,
/// whatever code the service runs (it need only hold its entry's lock,
/// burrow-review's `taken`), and a later sweep takes the rest.
///
/// What it left of the heads it swept: their looks under way, by their
/// entries' names, and the sessions it couldn't mark gone. Fails, partway,
/// with why, when one of the dirs can't be read or something in them can't
/// be deleted.
pub fn sweep(
    state: &Path,
    trees: &Path,
    key: &str,
    live: &BTreeSet<String>,
) -> Result<Vec<String>, String> {
    let ours = |snap: &str| {
        let repo = snap.rsplitn(4, '-').last().unwrap_or(snap);
        repo.strip_prefix("pr-").unwrap_or(repo) == key
    };
    let pr_of = |snap: &str| snap.rsplitn(3, '-').last().unwrap_or(snap).to_owned();
    let open: BTreeSet<String> = live.iter().map(|s| pr_of(s)).collect();
    let (mut keep, mut busy, mut left) = (BTreeSet::new(), BTreeSet::new(), vec![]);
    for (part, dir) in [
        (Part::Queue, state.join(QUEUE)),
        (Part::Done, state.join(DONE)),
        (Part::Meta, state.join(META)),
        (Part::Tree, trees.to_path_buf()),
    ] {
        for (x, path) in names(&dir)? {
            let stem = x.strip_suffix(".json").unwrap_or(&x);
            let snap = match part {
                Part::Queue | Part::Done => stem.split_once("--").map_or("", |(_, s)| s),
                Part::Meta | Part::Tree => stem,
            };
            if !ours(snap) || live.contains(snap) {
                continue;
            }
            let gone = match part {
                Part::Queue => {
                    if !taken(&path) {
                        keep.insert(snap.to_owned());
                        busy.extend(session_name(reviewer_of(&x), snap));
                        left.push(x.clone());
                    }
                    false
                }
                Part::Done => !open.contains(&pr_of(snap)),
                Part::Meta | Part::Tree => !keep.contains(snap),
            };
            if gone {
                remove(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
    }
    // And the reviewers' sessions, for good, once their PR is closed (or
    // retargeted): nothing of one PR's carries over to another's.
    for (x, path) in names(&state.join(SESSIONS))? {
        let r = reviewer_of(&x);
        let pr = x.split_once("--").map_or("", |(_, p)| p);
        let repo = pr.rsplitn(3, '-').last().unwrap_or(pr);
        if x.starts_with(GONE)
            || repo.strip_prefix("pr-").unwrap_or(repo) != key
            || live
                .iter()
                .any(|s| session_name(r, s).as_deref() == Some(&x))
            || busy.contains(&x)
        {
            continue;
        }
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        if let Err(e) = fs::rename(&path, path.with_file_name(format!("{GONE}{at}-{x}"))) {
            eprintln!("couldn't mark the session {x} gone: {e}");
            left.push(x);
        }
    }
    Ok(left)
}

/// Why `live` can't be the repo's open PRs' snapshots, if it can't: a name
/// that isn't one of the repo's (reviewers.py `snap_name`). Taken as one, a
/// typo would keep nothing.
pub fn wrong(key: &str, live: &BTreeSet<String>) -> Option<String> {
    let hex = |s: &str, n: usize| {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    };
    live.iter()
        .find(|s| {
            let rest = s.strip_prefix(&format!("pr-{key}-")).unwrap_or("");
            match rest.splitn(3, '-').collect::<Vec<_>>()[..] {
                [n, head, base] => {
                    n.is_empty()
                        || !n.bytes().all(|b| b.is_ascii_digit())
                        || !hex(head, 12)
                        || !hex(base, 8)
                }
                _ => true,
            }
        })
        .map(|s| format!("{s:?} isn't the name of a snapshot of {key}'s"))
}

/// What's in dir, by name (as it's shown, where it isn't UTF-8), in order,
/// and the path of each.
fn names(dir: &Path) -> Result<Vec<(String, std::path::PathBuf)>, String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out = entries
        .map(|x| x.map(|x| (x.file_name().to_string_lossy().into_owned(), x.path())))
        .collect::<io::Result<Vec<_>>>()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    out.sort();
    Ok(out)
}

#[cfg(test)]
#[path = "sweep_tests.rs"]
mod tests;
