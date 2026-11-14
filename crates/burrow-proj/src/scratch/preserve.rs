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
//!
//! Nor does it follow the job's paths to anything it wasn't given: the
//! repository metadata (a gitfile, `commondir`, `HEAD` and the ref it names,
//! the object store and each alternate it lists, however deep) is read only
//! where it resolves inside the scratch or the job's own project, and what
//! doesn't keeps the scratch rather than reading further. Nothing read from it
//! is printed. A submodule whose commit the superproject's change moved is
//! kept with its objects even when its own files are clean, as is one a saved
//! head already names. Index flags (assume-unchanged, skip-worktree) don't hide
//! a present file's edits, and an unmerged index, which a patch can't hold, keeps
//! the scratch.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};
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

/// The paths the job's repositories may be read from: its scratch and its
/// project, resolved. What a job's links point at beyond them is not read.
struct Grants {
    roots: Vec<PathBuf>,
}

impl Grants {
    fn new(job: &Job, scratch: &Path) -> Result<Grants, String> {
        let mut roots = vec![
            scratch
                .canonicalize()
                .map_err(|e| format!("{}: {e}", scratch.display()))?,
        ];
        // The project granted at launch, from the job's request on the host:
        // the job writes its own state, so what it says of its project grants
        // nothing. It must also be one of the machine's projects.
        if let Some(request) = crate::job::Request::recorded(job)
            && let Ok(p) = Path::new(request.project.trim()).canonicalize()
            && let Ok(all) = job.cfg.projects.canonicalize()
            && p.starts_with(&all)
        {
            roots.push(p);
        }
        Ok(Grants { roots })
    }

    /// `p` with its links resolved, if that is a granted place.
    fn confine(&self, p: &Path) -> Result<PathBuf, String> {
        let c = p
            .canonicalize()
            .map_err(|e| format!("{}: {e}", p.display()))?;
        if self.roots.iter().any(|r| c.starts_with(r)) {
            Ok(c)
        } else {
            Err(format!(
                "{} is outside the job's granted paths",
                p.display()
            ))
        }
    }

    /// A granted file's text; none if it isn't there. The file is opened with
    /// no link followed anywhere on its path and checked to be a regular file
    /// before it is read (a FIFO or a device is refused, never opened for
    /// reading), and no more than [`META_LIMIT`] bytes of it are read.
    fn read(&self, p: &Path) -> Result<Option<String>, String> {
        match p.canonicalize() {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", p.display())),
        }
        let c = self.confine(p)?;
        match bounded(&c, META_LIMIT) {
            Ok(b) => String::from_utf8(b)
                .map(Some)
                .map_err(|_| format!("{} isn't text", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    }

    /// Where a tree's repository is: its own git dir and the common one (the
    /// same but for a linked worktree).
    fn locate(&self, tree: &Path) -> Result<(PathBuf, PathBuf), String> {
        let dotgit = tree.join(".git");
        let meta =
            fs::symlink_metadata(&dotgit).map_err(|e| format!("{}: {e}", dotgit.display()))?;
        let gitdir = if meta.is_dir() {
            dotgit
        } else if meta.is_file() {
            let text = bounded(&dotgit, META_LIMIT)
                .map_err(|e| format!("{}: {e}", dotgit.display()))
                .and_then(|b| {
                    String::from_utf8(b).map_err(|_| format!("{} isn't text", dotgit.display()))
                })?;
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
        let gitdir = self.confine(&gitdir)?;
        let common = match self.read(&gitdir.join("commondir"))? {
            Some(rel) => gitdir.join(rel.trim()),
            None => gitdir.clone(),
        };
        Ok((gitdir, self.confine(&common)?))
    }

    /// The branch HEAD names (`HEAD` when detached) and the commit it is at,
    /// none on a branch not yet born.
    fn head(&self, gitdir: &Path, common: &Path) -> Result<(String, Option<String>), String> {
        let text = self
            .read(&gitdir.join("HEAD"))?
            .ok_or_else(|| format!("{}/HEAD is missing", gitdir.display()))?;
        let text = text.trim();
        let Some(name) = text.strip_prefix("ref: ") else {
            return Ok(("HEAD".into(), Some(commit_id(gitdir, text)?)));
        };
        if !name.starts_with("refs/")
            || !Path::new(name)
                .components()
                .all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(format!("{}'s HEAD names no ref", gitdir.display()));
        }
        let mut sha = None;
        for dir in [gitdir, common] {
            if let Some(t) = self.read(&dir.join(name))? {
                sha = Some(commit_id(gitdir, t.trim())?);
                break;
            }
        }
        if sha.is_none()
            && let Some(packed) = self.read(&common.join("packed-refs"))?
        {
            sha = packed
                .lines()
                .filter_map(|l| l.split_once(' '))
                .find(|(_, r)| *r == name)
                .map(|(s, _)| commit_id(gitdir, s))
                .transpose()?;
        }
        Ok((
            name.strip_prefix("refs/heads/").unwrap_or(name).to_owned(),
            sha,
        ))
    }

    /// Every object directory the repository reads, its alternates and theirs
    /// included, each one granted.
    fn object_dirs(&self, common: &Path) -> Result<Vec<PathBuf>, String> {
        let mut seen: Vec<PathBuf> = Vec::new();
        let mut todo = vec![self.confine(&common.join("objects"))?];
        while let Some(o) = todo.pop() {
            if seen.contains(&o) {
                continue;
            }
            let alternates = self.read(&o.join("info/alternates"))?.unwrap_or_default();
            for line in alternates
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
            {
                todo.push(self.confine(&o.join(line))?);
            }
            seen.push(o);
        }
        for o in &seen {
            plain_objects(o)?;
        }
        Ok(seen)
    }
}

/// The most a repository metadata file (a HEAD, a ref, an alternates list, a
/// config) may hold; real ones are a few lines.
pub(super) const META_LIMIT: u64 = 1 << 20;
/// The most an index may hold.
const INDEX_LIMIT: u64 = 1 << 30;
/// The most entries one object directory may have, so a job can't make the
/// walk endless.
const OBJECT_ENTRIES: usize = 4_000_000;

/// A regular file's content, up to `limit` bytes, opened with no link followed
/// anywhere on its path: a link, a FIFO or a device is an error, and so is a
/// file past the limit, never a truncated read.
fn bounded(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let bytes = burrow_process::state_reads::prefix(path, limit + 1)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other(format!("larger than {limit} bytes")));
    }
    Ok(bytes)
}

/// An object directory holds only directories and regular files, none of them
/// links: git opens its loose objects and packs by path, and a link in there
/// would send the unconfined git into another repository. Each entry is
/// reached from a pinned directory, so a name swapped for a link later is
/// refused rather than followed.
fn plain_objects(dir: &Path) -> Result<(), String> {
    use burrow_process::state_reads::Dir;
    fn walk(d: &Dir, at: &Path, left: &mut usize, depth: usize) -> Result<(), String> {
        let names = d.names().map_err(|e| format!("{}: {e}", at.display()))?;
        for name in names {
            *left = left
                .checked_sub(1)
                .ok_or_else(|| format!("{} has too many entries", at.display()))?;
            let here = at.join(&name);
            if let Ok(sub) = d.dir(Path::new(&name)) {
                if depth == 0 {
                    return Err(format!("{} is nested too deep", here.display()));
                }
                walk(&sub, &here, left, depth - 1)?;
            } else {
                d.file(Path::new(&name)).map_err(|_| {
                    format!("{} is neither a plain file nor a directory", here.display())
                })?;
            }
        }
        Ok(())
    }
    let d = Dir::open(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    walk(&d, dir, &mut OBJECT_ENTRIES.clone(), 4)
}

/// A commit id as a git dir holds it; nothing else is repeated.
fn commit_id(gitdir: &Path, text: &str) -> Result<String, String> {
    if matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(text.to_owned())
    } else {
        Err(format!("{}'s head isn't a commit id", gitdir.display()))
    }
}

/// The ignore file git reads for a user when none is configured.
pub(super) fn default_global_ignore(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    match (xdg.filter(|x| !x.is_empty()), home) {
        (Some(x), _) => Some(PathBuf::from(x).join("git/ignore")),
        (None, Some(h)) => Some(PathBuf::from(h).join(".config/git/ignore")),
        (None, None) => None,
    }
}

/// This process's own user's global ignore file (not the job's), if it has one.
fn global_ignore(cfg: &Config) -> Option<PathBuf> {
    let set = Command::new(&cfg.tools.git)
        .current_dir("/")
        .args(["config", "--get", "--type=path", "core.excludesFile"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    set.or_else(|| {
        default_global_ignore(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    })
    .filter(|p| p.is_file())
}

/// Whether a repository config's `extensions.objectformat` is sha256: the
/// setting itself, not any mention of the word (a remote's URL, say).
pub(super) fn object_format_is_sha256(config: &str) -> bool {
    let mut section = String::new();
    let mut sha256 = false;
    for line in config.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('[') {
            section = rest
                .split(']')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
        } else if section == "extensions"
            && let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case("objectformat")
        {
            sha256 = value
                .split(['#', ';'])
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('"')
                .eq_ignore_ascii_case("sha256");
        }
    }
    sha256
}

/// A config value, quoted so that `#`, `;`, quotes and backslashes in it are
/// its own characters.
pub(super) fn quote_config(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' | '"' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The git dir this process runs git on in place of the job's. The job's own
/// `info/exclude` goes with it as data, the one place git reads ignore rules
/// that isn't in the working tree.
fn sanitized(
    cfg: &Config,
    g: &Grants,
    at: &Path,
    gitdir: &Path,
    common: &Path,
    sha: Option<&str>,
) -> Result<(), String> {
    let w = |rel: &str, data: &[u8]| {
        fs::write(at.join(rel), data).map_err(|e| format!("{}/{rel}: {e}", at.display()))
    };
    for d in ["refs/heads", "objects/info", "info"] {
        fs::create_dir_all(at.join(d)).map_err(|e| format!("{}/{d}: {e}", at.display()))?;
    }
    let sha256 = object_format_is_sha256(&g.read(&common.join("config"))?.unwrap_or_default());
    let mut config = String::from(if sha256 {
        "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tobjectformat = sha256\n"
    } else {
        "[core]\n\trepositoryformatversion = 0\n"
    });
    if let Some(global) = global_ignore(cfg) {
        config.push_str(&format!(
            "[core]\n\texcludesFile = {}\n",
            quote_config(&global.to_string_lossy())
        ));
    }
    w("config", config.as_bytes())?;
    match sha {
        Some(s) => w("HEAD", format!("{s}\n").as_bytes())?,
        None => w("HEAD", b"ref: refs/heads/unborn\n")?,
    }
    if let Some(exclude) = g.read(&common.join("info/exclude"))? {
        w("info/exclude", exclude.as_bytes())?;
    }
    let objects = g.object_dirs(common)?;
    w(
        "objects/info/alternates",
        format!("{}\n", objects[0].display()).as_bytes(),
    )?;
    let entries = fs::read_dir(gitdir).map_err(|e| format!("{}: {e}", gitdir.display()))?;
    for e in entries {
        let e = e.map_err(|e| format!("{}: {e}", gitdir.display()))?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name == "index" || name.starts_with("sharedindex.") {
            // A link or a special file there is no index: refused, not read.
            let bytes = bounded(&e.path(), INDEX_LIMIT)
                .map_err(|err| format!("{}: {err}", e.path().display()))?;
            fs::write(at.join(&name), bytes)
                .map_err(|err| format!("{}/{name}: {err}", at.display()))?;
        }
    }
    Ok(())
}

/// The submodules a diff's raw output (`-z`) shows a commit changed at: each
/// path with the commits on both sides, none of which may be lost.
fn gitlinks(raw: &[u8]) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let mut parts = raw.split(|b| *b == 0).filter(|p| !p.is_empty());
    while let Some(head) = parts.next() {
        let head = String::from_utf8_lossy(head);
        let mut fields = head.trim_start_matches(':').split(' ');
        let (old, new) = (fields.next(), fields.next());
        let shas: Vec<String> = [fields.next(), fields.next()]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty() && s.bytes().any(|b| b != b'0'))
            .map(str::to_owned)
            .collect();
        let Some(path) = parts.next() else { break };
        if old == Some("160000") || new == Some("160000") {
            out.push((String::from_utf8_lossy(path).into_owned(), shas));
        }
    }
    out
}

/// The submodules a `ls-tree -r -z` listing names: each path with its commit.
fn tree_gitlinks(listing: &[u8]) -> Links {
    listing
        .split(|b| *b == 0)
        .filter_map(|entry| {
            let (meta, path) = entry.split_at(entry.iter().position(|b| *b == b'\t')?);
            let meta = String::from_utf8_lossy(meta);
            let mut f = meta.split(' ');
            (f.next() == Some("160000") && f.next() == Some("commit")).then(|| {
                (
                    String::from_utf8_lossy(&path[1..]).into_owned(),
                    f.next().map(str::to_owned).into_iter().collect(),
                )
            })
        })
        .collect()
}

/// Keep what every working tree in job `id`'s scratch has not published. The
/// caller holds the job's clear lock: nobody else keeps or removes this
/// scratch meanwhile.
pub(super) fn unfinished(cfg: &Config, id: &str) -> Result<(), String> {
    let root = super::dir(cfg, id);
    let job = Job::new(cfg, id);
    let found = trees(&root)?;
    if found.is_empty() {
        return Ok(());
    }
    let grants = Grants::new(&job, &root)?;
    // Submodules whose commit a tree's change moved: kept, clean or not, since
    // that commit may exist nowhere but in them.
    // A submodule's worktree is kept with every commit the superproject's saved
    // states name there (its base, index and files), not only its checkout.
    let mut linked: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let mut found = found;
    // A superproject before the submodules inside it, whatever order the
    // directories were read in.
    found.sort();
    let trees: Vec<PathBuf> = found.clone();
    for tree in found {
        let rel = tree.strip_prefix(&root).unwrap_or(&tree);
        let name = rel.to_string_lossy().replace('/', "-");
        let base = job.path("preserved");
        if fs::symlink_metadata(&base).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("{} is a link", base.display()));
        }
        fs::create_dir_all(&base).map_err(|e| format!("{}: {e}", base.display()))?;
        let mut n = 1;
        while base.join(format!("{name}.{n}")).exists() {
            n += 1;
        }
        let part = base.join(format!(".{name}.{n}.partial"));
        let _ = fs::remove_dir_all(&part);
        fs::create_dir_all(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        let named = linked.get(&tree);
        let (kept, links) = keep(cfg, &grants, &tree, &part, named.map(Vec::as_slice))
            .inspect_err(|_| {
                let _ = fs::remove_dir_all(&part);
            })?;
        for (link, shas) in links {
            linked.entry(tree.join(link)).or_default().extend(shas);
        }
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
    // A submodule the saved states name, with no working tree to keep it from
    // (not checked out, or gone): its commits may be nowhere else, so the
    // scratch stays.
    if let Some(missing) = linked.keys().find(|p| !trees.contains(p)) {
        return Err(format!(
            "{} is a submodule of saved work with no working tree to keep it from",
            missing.display()
        ));
    }
    Ok(())
}

/// Submodule paths with the commits a change names there.
type Links = Vec<(String, Vec<String>)>;

/// The tree's unfinished work into `to`: whether there was any, and the
/// submodules its change moved. `named` is Some for a submodule the
/// superproject's change moved: kept though it has no changes, with the
/// commits the superproject names there.
fn keep(
    cfg: &Config,
    g: &Grants,
    tree: &Path,
    to: &Path,
    named: Option<&[String]>,
) -> Result<(bool, Links), String> {
    let force = named.is_some();
    // Every path below is under the tree as it really is, so nothing of it is
    // reached through a link.
    let canonical = g.confine(tree)?;
    let tree = canonical.as_path();
    let (gitdir, common) = g.locate(tree)?;
    let (branch, sha) = g.head(&gitdir, &common)?;
    let own = to.join(".gitdir");
    fs::create_dir(&own).map_err(|e| format!("{}: {e}", own.display()))?;
    sanitized(cfg, g, &own, &gitdir, &common, sha.as_deref())?;
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
    // The copied index may mark tracked files assume-unchanged or
    // skip-worktree, which makes a diff take the index's word for them. A flagged
    // file that is on disk is compared like any other; a skip-worktree file that
    // isn't there is a sparse checkout's, not a deletion, and stays flagged.
    let flagged = git.git(&["ls-files", "-v", "-z"])?;
    let mut present: Vec<String> = Vec::new();
    for entry in flagged.split(|b| *b == 0).filter(|e| e.len() > 2) {
        if matches!(entry[0], b'h' | b'S' | b's') {
            let path = std::str::from_utf8(&entry[2..]).map_err(|e| e.to_string())?;
            if fs::symlink_metadata(tree.join(path)).is_ok() {
                present.push(path.to_owned());
            }
        }
    }
    // One flag at a time: git applies only the last of several.
    for flag in ["--no-assume-unchanged", "--no-skip-worktree"] {
        for batch in present.chunks(256) {
            let mut args = vec!["update-index", flag, "--"];
            args.extend(batch.iter().map(String::as_str));
            git.git(&args)?;
        }
    }
    // A patch can't hold an index's conflict stages (a resolved working file
    // may be all it shows of them), and the index goes with the scratch: it
    // stays rather than lose the sides.
    if !git.git(&["ls-files", "-u", "-z"])?.is_empty() {
        return Err(format!(
            "{} has unmerged index entries, which can't be kept",
            tree.display()
        ));
    }
    // A submodule's own files are its own tree's to keep, and git is not to
    // look inside one: it would run what the job configured there.
    let diff = |extra: &[&str]| {
        let mut args = vec![
            "diff",
            "--ignore-submodules=dirty",
            "--no-ext-diff",
            "--no-textconv",
        ];
        args.extend(extra);
        args.push(&against);
        git.git(&args)
    };
    let patch = |extra: &[&str]| diff(&[&["--binary", "--full-index"], extra].concat());
    let tracked = patch(&[])?;
    let staged = patch(&["--cached"])?;
    let mut links = gitlinks(&diff(&["--raw", "-z", "--no-renames"])?);
    links.extend(gitlinks(&diff(&[
        "--raw",
        "-z",
        "--no-renames",
        "--cached",
    ])?));
    let listed = git.git(&["ls-files", "-z", "--others", "--exclude-standard"])?;
    if !force && tracked.is_empty() && staged.is_empty() && listed.is_empty() {
        return Ok((false, links));
    }
    // Every revision the saved state is built on (the head, and the commits a
    // superproject names here) holds gitlinks of its own, in a clone as in a
    // submodule: their commits are needed to restore it, clean modules and
    // commits not checked out included. Only a repository whose objects are in
    // the scratch loses them with it.
    let local = sha.is_some()
        && g.object_dirs(&common)?
            .iter()
            .any(|d| d.starts_with(&g.roots[0]));
    if force || local {
        let mut revs: Vec<&str> = named
            .unwrap_or_default()
            .iter()
            .map(String::as_str)
            .collect();
        revs.extend(sha.as_deref());
        for rev in revs {
            let tree_rev = format!("{rev}^{{tree}}");
            let listing = git.git(&["ls-tree", "-r", "-z", "--full-tree", &tree_rev])?;
            links.extend(tree_gitlinks(&listing));
        }
    }
    let put = |name: &str, data: &[u8]| {
        fs::write(to.join(name), data).map_err(|e| format!("{}: {e}", to.join(name).display()))
    };
    put("from", tree.as_os_str().as_encoded_bytes())?;
    put("branch", format!("{branch}\n").as_bytes())?;
    put("head", sha.as_deref().unwrap_or_default().as_bytes())?;
    put("tracked.patch", &tracked)?;
    put("staged.patch", &staged)?;
    if sha.is_some() && local {
        // Each commit the superproject names is a ref of this process's own
        // git dir, so the bundle carries it though it isn't in HEAD's history.
        let mut revs = vec![String::from("HEAD")];
        for (i, sha) in named.unwrap_or_default().iter().enumerate() {
            git.git(&["cat-file", "-e", &format!("{sha}^{{commit}}")])?;
            let name = format!("refs/preserved/{i}");
            git.git(&["update-ref", &name, sha])?;
            revs.push(name);
        }
        let out = to.join("base.bundle").to_string_lossy().into_owned();
        let mut args = vec!["bundle", "create", out.as_str()];
        args.extend(revs.iter().map(String::as_str));
        git.git(&args)?;
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
            // Opened with no link followed on its path, and checked to be a
            // regular file once open: a name swapped for a link or a FIFO
            // after the look above is refused, never read or waited on.
            let mut source = burrow_process::state_reads::open(&from)
                .map_err(|e| format!("{}: {e}", from.display()))?;
            let mut copy = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&into)
                .map_err(|e| format!("{}: {e}", into.display()))?;
            std::io::copy(&mut source, &mut copy)
                .map_err(|e| format!("{}: {e}", from.display()))?;
        } else {
            return Err(format!("{} is a special file", from.display()));
        }
    }
    fs::remove_dir_all(&own).map_err(|e| format!("{}: {e}", own.display()))?;
    Ok((true, links))
}
