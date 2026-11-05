//! A PR's own change, as a fingerprint: what lets a reviewer's clean look
//! carry to a head that only merged its base in (verdict.rs). It's the PR's
//! diff against its merge base with the base, kept to what the PR itself
//! does: each file's header and its added and removed lines, in order.
//! What the merge base brought (the context around them, where its hunks
//! sit, the blobs they're in) is left out, so a merge of the base that
//! leaves the PR's own lines alone leaves its fingerprint alone too, and a
//! resolution that edits them, or a new commit, changes it.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::mirror::{self, git_in};

/// The fingerprint of a `git diff` of two trees, as `of` runs it: a hash of
/// each file's header lines but its `index` line (the blobs it's between,
/// which a merge of the base moves), and of its hunks' added and removed
/// lines (and git's note of a missing newline), in order. Context lines and
/// hunk headers (where a hunk sits, which the base's own changes move) are
/// left out. A binary patch is all header, so it's kept whole.
pub fn fingerprint(diff: &[u8]) -> String {
    let mut h = Sha256::new();
    let mut in_hunk = false;
    for line in diff.split(|b| *b == b'\n') {
        let keep = if line.starts_with(b"diff --git ") {
            in_hunk = false;
            true
        } else if line.starts_with(b"@@") {
            in_hunk = true;
            false
        } else if in_hunk {
            !line.starts_with(b" ")
        } else {
            !line.starts_with(b"index ")
        };
        if keep {
            h.update(line);
            h.update(b"\n");
        }
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The fingerprint of `head`'s own change on the base at `base_ref`, in the
/// repo at `m`: its diff against their merge base. The diff's options are
/// fixed here, whatever git's config says, so the same change always gives
/// the same fingerprint.
pub fn of(m: &Path, base_ref: &str, head: &str) -> Result<String, String> {
    if !mirror::id(head) {
        return Err(format!("{head:?} isn't a commit's id"));
    }
    let mb = git_in(m).args(["merge-base", base_ref, head]).run()?;
    let mb = String::from_utf8_lossy(&mb).trim().to_string();
    let diff = git_in(m)
        .args([
            "-c",
            "core.quotePath=true",
            "diff",
            "--no-color",
            "--no-ext-diff",
        ])
        .args([
            "--no-textconv",
            "--no-renames",
            "--binary",
            "--diff-algorithm=myers",
        ])
        .args(["--ignore-submodules=none", "-U0"])
        .args([mb.as_str(), head, "--"])
        .run()?;
    Ok(fingerprint(&diff))
}

/// The fingerprint of PR `n`'s own change at `head` on `base`, from the
/// steward's mirror of the repo, with the PR's head and the base's tip
/// fetched into it just now (as a snapshot fetches them, snapshot.rs).
pub fn change(
    cfg: &Config,
    state: &Path,
    repo: &str,
    n: u64,
    head: &str,
    base: &str,
) -> Result<String, String> {
    let more = [format!("+refs/heads/{base}:refs/base/{base}")];
    let m = mirror::fetch_pr(cfg, state, repo, n, head, &more)?;
    of(&m, &format!("refs/base/{base}"), head)
}

#[cfg(test)]
#[path = "change_tests.rs"]
mod tests;
