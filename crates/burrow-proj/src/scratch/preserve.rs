//! Work a job hasn't published, kept before its scratch goes. A worktree with
//! changes to tracked files (staged or not) or files git doesn't ignore (a
//! build's output is ignored, and goes with the scratch as before) is copied to
//! JOB/preserved/NAME.N: `from`, `branch` and `head`, `tracked.patch` (the
//! working files against head, or against the empty tree before a first
//! commit), `staged.patch` (the index against the same), `untracked/`, and,
//! when the scratch holds the repository's objects too (a clone, a worktree of
//! one), `base.bundle`, which carries what the patches apply to. When it can't
//! be kept, or can't be looked at in full, the scratch is not removed: it stays
//! for a reply or a retry.
//!
//! The job wrote everything under its scratch, `.git/config` included, and
//! this runs outside the job's confinement. So git never reads the job's
//! repository config or hooks: it runs against a git dir of this process's
//! own, whose only links to the job's repository are its object store (as an
//! alternate), its head and a copy of its index, with no user or system config
//! and an empty environment. A `core.fsmonitor`, a filter or a diff driver the
//! job set names nothing there.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::Config;
use crate::job::Job;

/// Every working tree in a scratch: a directory with a `.git` in it, a file
/// (worktree) or a directory (clone), at any depth. A scratch that isn't there
/// has none; anything else that keeps a directory from being read is an error,
/// since what it hides is unaccounted for.
fn trees(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let entries = match fs::read_dir(&d) {
            Ok(e) => e,
            Err(e) if d == root && e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(e) => return Err(format!("{}: {e}", d.display())),
        };
        for e in entries {
            let e = e.map_err(|e| format!("{}: {e}", d.display()))?;
            let kind = e
                .file_type()
                .map_err(|err| format!("{}: {err}", e.path().display()))?;
            if e.file_name() == ".git" {
                found.push(d.clone());
            } else if kind.is_dir() {
                dirs.push(e.path());
            }
        }
    }
    found.sort();
    Ok(found)
}

/// A job's repository as git is run on it here: the git dir this process made,
/// and the working tree it looks at.
struct Inspect<'a> {
    cfg: &'a Config,
    gitdir: PathBuf,
    tree: &'a Path,
}

impl Inspect<'_> {
    fn git(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        let out = Command::new(&self.cfg.tools.git)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", "/nonexistent")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .arg("--git-dir")
            .arg(&self.gitdir)
            .arg("--work-tree")
            .arg(self.tree)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("git {args:?}: {e}"))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(format!(
                "git {args:?} in {}: {}",
                self.tree.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

fn read_opt(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Where a tree's repository is: its own git dir and the common one (the same
/// but for a linked worktree).
fn locate(tree: &Path) -> Result<(PathBuf, PathBuf), String> {
    let dotgit = tree.join(".git");
    let meta = fs::symlink_metadata(&dotgit).map_err(|e| format!("{}: {e}", dotgit.display()))?;
    let gitdir = if meta.is_dir() {
        dotgit
    } else if meta.is_file() {
        let text = fs::read_to_string(&dotgit).map_err(|e| format!("{}: {e}", dotgit.display()))?;
        let at = text
            .trim_end()
            .strip_prefix("gitdir: ")
            .ok_or_else(|| format!("{} doesn't name a git dir", dotgit.display()))?;
        tree.join(at)
    } else {
        return Err(format!(
            "{} is neither a file nor a directory",
            dotgit.display()
        ));
    };
    let gitdir = gitdir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", gitdir.display()))?;
    let common = match read_opt(&gitdir.join("commondir"))? {
        Some(rel) => gitdir.join(rel.trim()),
        None => gitdir.clone(),
    };
    let common = common
        .canonicalize()
        .map_err(|e| format!("{}: {e}", common.display()))?;
    Ok((gitdir, common))
}

/// The branch HEAD names (`HEAD` when detached) and the commit it is at, none
/// on a branch not yet born.
fn head(gitdir: &Path, common: &Path) -> Result<(String, Option<String>), String> {
    let text = read_opt(&gitdir.join("HEAD"))?
        .ok_or_else(|| format!("{}/HEAD is missing", gitdir.display()))?;
    let text = text.trim();
    let Some(name) = text.strip_prefix("ref: ") else {
        return Ok(("HEAD".into(), Some(text.to_owned())));
    };
    let mut sha = None;
    for dir in [gitdir, common] {
        if let Some(t) = read_opt(&dir.join(name))? {
            sha = Some(t.trim().to_owned());
            break;
        }
    }
    if sha.is_none()
        && let Some(packed) = read_opt(&common.join("packed-refs"))?
    {
        sha = packed
            .lines()
            .filter_map(|l| l.split_once(' '))
            .find(|(_, r)| *r == name)
            .map(|(s, _)| s.to_owned());
    }
    Ok((
        name.strip_prefix("refs/heads/").unwrap_or(name).to_owned(),
        sha,
    ))
}

/// The git dir this process runs git on in place of the job's.
fn sanitized(at: &Path, gitdir: &Path, common: &Path, sha: Option<&str>) -> Result<(), String> {
    let w = |rel: &str, data: &[u8]| {
        fs::write(at.join(rel), data).map_err(|e| format!("{}/{rel}: {e}", at.display()))
    };
    for d in ["refs/heads", "objects/info"] {
        fs::create_dir_all(at.join(d)).map_err(|e| format!("{}/{d}: {e}", at.display()))?;
    }
    let sha256 = read_opt(&common.join("config"))?
        .is_some_and(|c| c.to_ascii_lowercase().contains("sha256"));
    w(
        "config",
        if sha256 {
            b"[core]\n\trepositoryformatversion = 1\n[extensions]\n\tobjectformat = sha256\n"
        } else {
            b"[core]\n\trepositoryformatversion = 0\n"
        },
    )?;
    match sha {
        Some(s) if s.bytes().all(|b| b.is_ascii_hexdigit()) => {
            w("HEAD", format!("{s}\n").as_bytes())?;
        }
        Some(s) => {
            return Err(format!(
                "{}'s head isn't a commit id: {s}",
                gitdir.display()
            ));
        }
        None => w("HEAD", b"ref: refs/heads/unborn\n")?,
    }
    w(
        "objects/info/alternates",
        format!("{}\n", common.join("objects").display()).as_bytes(),
    )?;
    let entries = fs::read_dir(gitdir).map_err(|e| format!("{}: {e}", gitdir.display()))?;
    for e in entries {
        let e = e.map_err(|e| format!("{}: {e}", gitdir.display()))?;
        let name = e.file_name().to_string_lossy().into_owned();
        if (name == "index" || name.starts_with("sharedindex."))
            && e.file_type().is_ok_and(|t| t.is_file())
        {
            fs::copy(e.path(), at.join(&name))
                .map_err(|err| format!("{}: {err}", e.path().display()))?;
        }
    }
    Ok(())
}

/// Whether the scratch holds any of the repository's objects, so that what a
/// patch applies to has to go with it.
fn objects_inside(root: &Path, common: &Path) -> Result<bool, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("{}: {e}", root.display()))?;
    let objects = common.join("objects");
    if objects.starts_with(&root) {
        return Ok(true);
    }
    let alternates = read_opt(&objects.join("info/alternates"))?.unwrap_or_default();
    for line in alternates
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let p = objects.join(line);
        let p = p
            .canonicalize()
            .map_err(|e| format!("{}: {e}", p.display()))?;
        if p.starts_with(&root) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Keep what every working tree in job `id`'s scratch has not published. The
/// caller holds the job's clear lock: nobody else keeps or removes this
/// scratch meanwhile.
pub(super) fn unfinished(cfg: &Config, id: &str) -> Result<(), String> {
    let root = super::dir(cfg, id);
    let job = Job::new(cfg, id);
    for tree in trees(&root)? {
        let rel = tree.strip_prefix(&root).unwrap_or(&tree);
        let name = rel.to_string_lossy().replace('/', "-");
        let base = job.path("preserved");
        fs::create_dir_all(&base).map_err(|e| format!("{}: {e}", base.display()))?;
        let mut n = 1;
        while base.join(format!("{name}.{n}")).exists() {
            n += 1;
        }
        let part = base.join(format!(".{name}.{n}.partial"));
        let _ = fs::remove_dir_all(&part);
        fs::create_dir_all(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        let kept = keep(cfg, &root, &tree, &part).inspect_err(|_| {
            let _ = fs::remove_dir_all(&part);
        })?;
        if !kept {
            let _ = fs::remove_dir_all(&part);
            continue;
        }
        let dest = base.join(format!("{name}.{n}"));
        fs::rename(&part, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        eprintln!(
            "proj: kept {}'s unfinished work in {} before clearing its scratch",
            tree.display(),
            dest.display()
        );
    }
    Ok(())
}

/// The tree's unfinished work into `to`: whether there was any.
fn keep(cfg: &Config, root: &Path, tree: &Path, to: &Path) -> Result<bool, String> {
    let (gitdir, common) = locate(tree)?;
    let (branch, sha) = head(&gitdir, &common)?;
    let own = to.join(".gitdir");
    fs::create_dir(&own).map_err(|e| format!("{}: {e}", own.display()))?;
    sanitized(&own, &gitdir, &common, sha.as_deref())?;
    let git = Inspect {
        cfg,
        gitdir: own.clone(),
        tree,
    };
    // Before a first commit, the empty tree is what a patch is against.
    let against = match &sha {
        Some(s) => s.clone(),
        None => String::from_utf8_lossy(&git.git(&["hash-object", "-t", "tree", "/dev/null"])?)
            .trim()
            .to_owned(),
    };
    let diff = |extra: &[&str]| {
        let mut args = vec![
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
        ];
        args.extend(extra);
        args.push(&against);
        git.git(&args)
    };
    let tracked = diff(&[])?;
    let staged = diff(&["--cached"])?;
    let listed = git.git(&["ls-files", "-z", "--others", "--exclude-standard"])?;
    if tracked.is_empty() && staged.is_empty() && listed.is_empty() {
        return Ok(false);
    }
    let put = |name: &str, data: &[u8]| {
        fs::write(to.join(name), data).map_err(|e| format!("{}: {e}", to.join(name).display()))
    };
    put("from", tree.as_os_str().as_encoded_bytes())?;
    put("branch", format!("{branch}\n").as_bytes())?;
    put("head", sha.as_deref().unwrap_or_default().as_bytes())?;
    put("tracked.patch", &tracked)?;
    put("staged.patch", &staged)?;
    if sha.is_some() && objects_inside(root, &common)? {
        git.git(&[
            "bundle",
            "create",
            &to.join("base.bundle").to_string_lossy(),
            "HEAD",
        ])?;
    }
    let untracked = to.join("untracked");
    fs::create_dir_all(&untracked).map_err(|e| format!("{}: {e}", untracked.display()))?;
    for rel in listed.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let rel = Path::new(std::str::from_utf8(rel).map_err(|e| e.to_string())?);
        let from = tree.join(rel);
        let into = untracked.join(rel);
        if let Some(parent) = into.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let meta = fs::symlink_metadata(&from).map_err(|e| format!("{}: {e}", from.display()))?;
        if meta.file_type().is_symlink() {
            let target = fs::read_link(&from).map_err(|e| format!("{}: {e}", from.display()))?;
            symlink(target, &into).map_err(|e| format!("{}: {e}", into.display()))?;
        } else if meta.is_file() {
            fs::copy(&from, &into).map_err(|e| format!("{}: {e}", from.display()))?;
        }
    }
    fs::remove_dir_all(&own).map_err(|e| format!("{}: {e}", own.display()))?;
    Ok(true)
}
