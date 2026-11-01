//! The snapshot a reviewer reads of a PR's head: a fresh repo of two
//! commits, the PR's merge base and its head, each without the paths no
//! reviewer reads (the secrets paths). The commits are git's own trees with
//! those paths taken out, so nothing the repo's attributes or ignore files
//! say can change or hide what the PR changed. Taken once a head on a base,
//! for every reviewer, which queue.rs briefs and queues each one on;
//! what it keeps of the base (the rules the PR is held to) and what it hid
//! are kept beside it, for the brief and for a judge (reviewers.py
//! `unseen`).

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::{self, Config};
use crate::mirror::{self, git, git_in};
use crate::pass::{load, save};

/// Under the steward's state dir: what each snapshot keeps of its base and
/// what it hid, NAME.json (`Meta`).
pub const META: &str = "review/snap";

/// The head a snapshot is of: the PR's head, its base, and the snapshot's
/// name (reviewers.py `snap_name`, one a head on a base).
#[derive(Debug)]
pub struct Look {
    pub number: u64,
    pub head: String,
    pub base: String,
    pub name: String,
}

impl Look {
    /// Why the look can't be taken as given, if it can't: a head that
    /// isn't a commit's id, or a name that isn't a plain file name.
    pub fn wrong(&self) -> Option<String> {
        let hex = |s: &str| {
            s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        };
        if !matches!(self.head.len(), 40 | 64) || !hex(&self.head) {
            return Some(format!("{:?} isn't a commit's id", self.head));
        }
        let plain = |b: u8| b.is_ascii_alphanumeric() || b"-_.".contains(&b);
        if self.name.is_empty() || self.name.starts_with('.') || !self.name.bytes().all(plain) {
            return Some(format!("{:?} isn't a snapshot's name", self.name));
        }
        if self.base.is_empty() || self.base.starts_with('-') {
            return Some(format!("{:?} isn't a branch's name", self.base));
        }
        None
    }
}

/// What a snapshot keeps of its base, for the brief: the review rules and
/// house rules (`rules`), and the paths the PR changes that it hid, as
/// text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub rules: String,
    pub house: String,
    pub secret: Vec<String>,
}

/// The paths no reviewer reads in a repo: the machine's patterns, and the
/// repo's own on top, so nothing can take a secrets path back into view.
pub struct Hide(Vec<Regex>);

impl Hide {
    /// A pattern that isn't one stops the snapshot: it can't tell what to
    /// hide.
    pub fn new(cfg: &config::Snapshot, repo: &str) -> Result<Hide, String> {
        cfg.skip_paths
            .iter()
            .chain(cfg.repos.get(repo).into_iter().flatten())
            .map(|p| Regex::new(p).map_err(|e| format!("the skip path {p:?} isn't a pattern: {e}")))
            .collect::<Result<_, _>>()
            .map(Hide)
    }

    /// Whether a pattern finds the path. A pattern's `$` matches only at
    /// the very end, and git allows a name that ends in a newline, so such
    /// a path is tried without it too: `\.pem$` still hides `key.pem\n`.
    fn skipped(&self, path: &str) -> bool {
        let bare = path.strip_suffix('\n');
        self.0
            .iter()
            .any(|r| r.is_match(path) || bare.is_some_and(|b| r.is_match(b)))
    }

    /// A secrets path: matched itself, or in a directory that is (a
    /// directory matches with a trailing slash). Bytes that aren't UTF-8
    /// are matched as replacement characters.
    pub fn hidden(&self, path: &[u8]) -> bool {
        let text = shown(path);
        let parts: Vec<&str> = text.split('/').collect();
        self.skipped(&text)
            || (1..parts.len()).any(|i| self.skipped(&format!("{}/", parts[..i].join("/"))))
    }
}

/// Paths from git's -z output: bytes, not always UTF-8, which git gets
/// back as they are.
fn names(out: &[u8]) -> Vec<Vec<u8>> {
    out.split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

/// A path as text to show (the brief, a judge's task): bytes that aren't
/// UTF-8 as replacement characters.
pub fn shown(path: &[u8]) -> String {
    String::from_utf8_lossy(path).into_owned()
}

fn line(out: &[u8]) -> String {
    String::from_utf8_lossy(out).trim().to_string()
}

/// The snapshot `look.name` of the PR's head, taken now unless it's kept
/// already: its meta saved, and its checkout under the snapshots' dir. One
/// kept by a steward that read less of the base is taken again, and so is
/// one whose checkout the cache lost (a reboot, say).
pub fn take(cfg: &Config, state: &Path, repo: &str, look: &Look) -> Result<Meta, String> {
    let (name, head, base) = (&look.name, &look.head, &look.base);
    let kept = format!("{META}/{name}.json");
    let dst = cfg.snapshot.dir.join(name);
    if let Some(meta) = load::<Option<Meta>>(state, &kept)
        && dst.is_dir()
    {
        return Ok(meta);
    }
    let hide = Hide::new(&cfg.snapshot, repo)?;
    let n = look.number;
    let refs = [
        format!("+refs/pull/{n}/head:refs/pr/{n}"),
        format!("+refs/heads/{base}:refs/base/{base}"),
    ];
    let m = mirror::fetch(cfg, state, repo, &refs)?;
    let base_ref = format!("refs/base/{base}");
    let mb = line(&git_in(&m).args(["merge-base", &base_ref, head]).run()?);
    let changed = names(
        &git_in(&m)
            .args(["diff", "-z", "--name-only", "--no-renames", &mb, head])
            .run()?,
    );
    let moved = moved(&m, &mb, head, &hide)?;
    let unseen = |p: &[u8]| hide.hidden(p) || moved.contains(p);
    let tip = commits(&m, name, &mb, head, &unseen)?;
    checkout(&m, name, &tip, &dst)?;
    let shown_paths: Vec<&[u8]> = changed
        .iter()
        .map(Vec::as_slice)
        .filter(|p| !unseen(p))
        .collect();
    let (rules, house) = rules(&m, &base_ref, &shown_paths, &hide)?;
    let secret = changed
        .iter()
        .filter(|p| unseen(p))
        .map(|p| shown(p))
        .collect();
    let meta = Meta {
        rules,
        house,
        secret,
    };
    save(state, &kept, &meta)?;
    Ok(meta)
}

/// Where a secret was renamed or copied to: a secret still, so its new name
/// is hidden too. A plain file copied into a secrets path stays in view:
/// only the copy is hidden, by its own path.
fn moved(m: &Path, mb: &str, head: &str, hide: &Hide) -> Result<BTreeSet<Vec<u8>>, String> {
    let out = names(
        &git_in(m)
            .args([
                "diff",
                "-z",
                "--name-status",
                "-M",
                "-C",
                "--find-copies-harder",
                mb,
                head,
            ])
            .run()?,
    );
    let mut moved = BTreeSet::new();
    let mut i = 0;
    while i < out.len() {
        if matches!(out[i].first(), Some(b'R' | b'C')) {
            let (Some(src), Some(dst)) = (out.get(i + 1), out.get(i + 2)) else {
                return Err("git diff --name-status: a rename without its two names".into());
            };
            if hide.hidden(src) {
                moved.insert(dst.clone());
            }
            i += 3;
        } else {
            i += 2;
        }
    }
    Ok(moved)
}

/// The base's commit and the head's on it, each of its own commit's tree
/// less what `unseen` hides, built in the mirror (which only the steward
/// reads), so their contents never reach the snapshot. Returns the head's.
fn commits(
    m: &Path,
    name: &str,
    mb: &str,
    head: &str,
    unseen: &dyn Fn(&[u8]) -> bool,
) -> Result<String, String> {
    let index = m.join(format!("snap-{name}.index"));
    // The index only: git wants a work tree for it, and an empty one is
    // never read or written.
    let empty = tempfile::tempdir().map_err(|e| format!("a temp dir: {e}"))?;
    let made = (|| {
        let mut parent: Option<String> = None;
        for (label, rev) in [("base".to_string(), mb), (format!("head {head}"), head)] {
            let _ = fs::remove_file(&index);
            let on = |g: mirror::Git| {
                g.env("GIT_INDEX_FILE", &index)
                    .env("GIT_WORK_TREE", empty.path())
            };
            on(git_in(m).args(["read-tree", rev])).run()?;
            let gone: Vec<Vec<u8>> = names(&on(git_in(m).args(["ls-files", "-z"])).run()?)
                .into_iter()
                .filter(|p| unseen(p))
                .collect();
            if !gone.is_empty() {
                let mut list = gone.join(&0);
                list.push(0);
                on(git_in(m).args(["update-index", "-z", "--force-remove", "--stdin"]))
                    .input(list)
                    .run()?;
            }
            let tree = line(&on(git_in(m).arg("write-tree")).run()?);
            let mut c = git_in(m)
                .args([
                    "-c",
                    "user.name=autoland",
                    "-c",
                    "user.email=autoland@localhost",
                ])
                .args(["-c", "commit.gpgsign=false", "commit-tree", &tree]);
            if let Some(p) = &parent {
                c = c.args(["-p", p]);
            }
            parent = Some(line(&c.args(["-m", &label]).run()?));
        }
        parent.ok_or_else(|| "no commit".to_string())
    })();
    let _ = fs::remove_file(&index);
    made
}

/// The commit checked out at `dst`, a repo of its own: made aside and
/// moved into place whole, so a checkout under its own name is always a
/// finished one, which the meta kept from before may vouch for. Nothing
/// from the steward's own git config (an LFS filter, say) runs on what the
/// PR put there.
fn checkout(m: &Path, name: &str, tip: &str, dst: &Path) -> Result<(), String> {
    let snap = format!("refs/snap/{name}");
    git_in(m).args(["update-ref", &snap, tip]).run()?;
    let mut new = dst.as_os_str().to_owned();
    new.push(".new");
    let new = Path::new(&new);
    let _ = fs::remove_dir_all(new);
    let bare = |g: mirror::Git| {
        g.env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
    };
    bare(git().args(["init", "-q"]).arg(new)).run()?;
    bare(
        git_in(new)
            .args(["fetch", "-q", "--no-tags"])
            .arg(m)
            .arg(format!("{snap}:refs/heads/review")),
    )
    .run()?;
    git_in(m).args(["update-ref", "-d", &snap]).run()?;
    bare(git_in(new).args(["checkout", "-q", "review"])).run()?;
    let _ = fs::remove_dir_all(dst);
    fs::rename(new, dst).map_err(|e| format!("{}: {e}", dst.display()))
}

/// The `## Code Review Rules` of the AGENTS.md nearest each of `paths`, and
/// the `## House rules` the reasons a PR's description gives answer to,
/// from every AGENTS.md from the root down to each (a nearer one can add to
/// the root's), as `rev` (the base branch) has them: a PR can't rewrite its
/// own rules, and the base it lands on is the one whose rules hold. An
/// AGENTS.md in a secrets path is never read: its rules would reach the
/// reviewers in the brief. Returns the two, as text.
pub fn rules(
    m: &Path,
    rev: &str,
    paths: &[&[u8]],
    hide: &Hide,
) -> Result<(String, String), String> {
    let have: BTreeSet<Vec<u8>> = names(
        &git_in(m)
            .args(["ls-tree", "-r", "-z", "--name-only", rev])
            .run()?,
    )
    .into_iter()
    .filter(|f| !hide.hidden(f))
    .collect();
    let (mut nearest, mut above) = (BTreeSet::new(), BTreeSet::new());
    for p in paths {
        let mut mine = vec![];
        let mut d: &[u8] = p;
        while !d.is_empty() {
            d = &d[..d.iter().rposition(|b| *b == b'/').unwrap_or(0)];
            let f = if d.is_empty() {
                b"AGENTS.md".to_vec()
            } else {
                [d, b"/AGENTS.md"].concat()
            };
            if have.contains(&f) {
                mine.push(f);
            }
        }
        nearest.extend(mine.first().cloned());
        above.extend(mine);
    }
    let (mut review, mut house) = (vec![], vec![]);
    for f in &above {
        let spec = [rev.as_bytes(), b":", f].concat();
        let text = shown(&git_in(m).arg("show").arg(OsStr::from_bytes(&spec)).run()?);
        if nearest.contains(f)
            && let Some(s) = section(&text, "Code Review Rules")
        {
            review.push(format!("From {}:\n{s}", shown(f)));
        }
        if let Some(s) = section(&text, "House rules") {
            house.push(format!("From {}:\n{s}", shown(f)));
        }
    }
    Ok((review.join("\n\n"), house.join("\n\n")))
}

/// What's under the first `## HEADING` line (nothing but whitespace after
/// it) up to the next `## ` line or the end, trimmed.
fn section<'a>(text: &'a str, heading: &str) -> Option<&'a str> {
    let mut start = None;
    let mut at = 0;
    for l in text.split_inclusive('\n') {
        match start {
            None => {
                if l.strip_prefix("## ")
                    .and_then(|r| r.strip_prefix(heading))
                    .is_some_and(|r| r.chars().all(char::is_whitespace))
                {
                    start = Some(at + l.len());
                }
            }
            Some(s) if l.starts_with("## ") => return Some(text[s..at].trim()),
            Some(_) => {}
        }
        at += l.len();
    }
    start.map(|s| text[s..].trim())
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
