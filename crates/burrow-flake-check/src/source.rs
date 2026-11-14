//! gate-reuse's source: the gate evaluates and builds a private checkout of
//! the tree, whose HEAD is a commit of that tree alone, made in a git context
//! of its own (no parents, no history, fixed authors and dates, no inherited
//! config), so everything Nix reads of the source (`self.rev`, `shortRev`,
//! `revCount`, `lastModified`) comes from the tree: two checkouts of one tree
//! get one verdict, whichever commits they were, shallow or full, a
//! repository or a worktree. The caller's checkout is never touched, so
//! nothing needs putting back however the gate ends.
use super::*;

/// When every commit of a tree is dated: a constant, so it's the tree's.
const WHEN: &str = "@1 +0000";

/// The private checkout, removed when this is dropped.
pub struct TreeSource {
    _work: Work,
}

/// git, in a context of its own: no system or global config, and only the
/// identity and dates the tree's commit is given.
fn git(dir: &Path, args: &[&str]) -> Command {
    let mut c = cmd("git", &["-C", s(dir)]);
    c.args(args);
    for k in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
    ] {
        c.env_remove(k);
    }
    c.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for (k, v) in [
        ("GIT_AUTHOR_NAME", "flake-check"),
        ("GIT_AUTHOR_EMAIL", "flake-check@localhost"),
        ("GIT_AUTHOR_DATE", WHEN),
        ("GIT_COMMITTER_NAME", "flake-check"),
        ("GIT_COMMITTER_EMAIL", "flake-check@localhost"),
        ("GIT_COMMITTER_DATE", WHEN),
    ] {
        c.env(k, v);
    }
    c
}

impl TreeSource {
    /// Makes the private checkout of HEAD's tree and moves this process
    /// into it, at the same place in it as the checkout's.
    pub fn enter() -> Result<TreeSource> {
        let here = Path::new(".").canonicalize()?;
        let top = string(git(&here, &["rev-parse", "--show-toplevel"]))
            .map_err(|e| err(format!("gate-reuse checks a git checkout's tree: {e}")))?;
        let top = Path::new(&top);
        let prefix = string(git(&here, &["rev-parse", "--show-prefix"]))?;
        let tree = string(git(top, &["rev-parse", "--verify", "HEAD^{tree}"]))?;
        let head = string(git(top, &["rev-parse", "--verify", "HEAD^{commit}"]))?;
        let format = string(git(top, &["rev-parse", "--show-object-format"]))?;
        let objects = Path::new(&string(git(
            top,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ))?)
        .join("objects");
        let work = Work::new("tree-source")?;
        let source = work.0.join("source");
        checked_git(cmd(
            "git",
            &[
                "init",
                "-q",
                &format!("--object-format={format}"),
                s(&source),
            ],
        ))?;
        // The tree's objects, read from the checkout's store: a link keeps
        // its path's bytes out of the alternates file's line format.
        std::os::unix::fs::symlink(&objects, source.join(".git/borrowed-objects"))?;
        fs::write(
            source.join(".git/objects/info/alternates"),
            "../borrowed-objects\n",
        )?;
        fs::write(
            source.join(".git/config"),
            "[core]\n\trepositoryformatversion = 0\n\tbare = false\n",
        )?;
        let commit = string(git(
            &source,
            &[
                "commit-tree",
                "--no-gpg-sign",
                &tree,
                "-m",
                &format!("flake-check: tree {tree}, for gate-reuse"),
            ],
        ))?;
        checked(git(&source, &["update-ref", "refs/heads/main", &commit]))?;
        checked(git(&source, &["symbolic-ref", "HEAD", "refs/heads/main"]))?;
        checked(git(&source, &["read-tree", "HEAD"]))?;
        checked(git(&source, &["checkout-index", "-a", "-f"]))?;
        println!(
            "gate-reuse: checking tree {tree} as {commit}, a commit of it alone (checked out: {head})"
        );
        std::env::set_current_dir(source.join(&prefix))?;
        Ok(TreeSource { _work: work })
    }
}

fn checked_git(mut c: Command) -> Result<Vec<u8>> {
    c.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    checked(c)
}
