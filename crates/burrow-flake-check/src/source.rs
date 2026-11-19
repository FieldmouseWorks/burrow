//! gate-reuse's source: the gate evaluates and builds a private checkout of
//! the tree, whose HEAD is a commit of that tree alone, made in a git context
//! of its own (no parents, no history, fixed authors and dates, no inherited
//! config or repository locations), so everything Nix reads of the source
//! (`self.rev`, `shortRev`, `revCount`, `lastModified`) comes from the tree:
//! two checkouts of one tree get one verdict, whichever commits they were,
//! shallow or full, a repository or a worktree. Relative submodule URLs
//! resolve against what the checkout's would. A checkout with uncommitted
//! changes is refused: its tree isn't what it holds. The caller's checkout
//! is never touched, so nothing needs putting back however the gate ends.
use super::*;

/// When every commit of a tree is dated: a constant, so it's the tree's.
const WHEN: &str = "@1 +0000";

/// The private checkout, removed when this is dropped.
pub struct TreeSource {
    _work: Work,
}

/// git on the private checkout, in a context of its own: no system or
/// global config, no repository locations from the environment, and only
/// the identity and dates the tree's commit is given.
fn git(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    c.args(["-C", s(dir)]).args(args);
    for k in GIT_LOCATIONS {
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

/// git on the caller's checkout, found as its environment says.
fn caller(args: &[&str]) -> Command {
    cmd("git", args)
}

impl TreeSource {
    /// Makes the private checkout of HEAD's tree and moves this process
    /// into it, at the same place in it as the checkout's; from then on no
    /// command inherits a repository location.
    pub fn enter() -> Result<TreeSource> {
        let top = string(caller(&["rev-parse", "--show-toplevel"]))
            .map_err(|e| err(format!("gate-reuse checks a git checkout's tree: {e}")))?;
        let top = Path::new(&top);
        let at = |args: &[&str]| {
            let mut c = caller(&["-C", s(top)]);
            c.args(args);
            c
        };
        let changed = string(at(&[
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--ignore-submodules=untracked",
        ]))?;
        if !changed.is_empty() {
            return Err(err(format!(
                "gate-reuse checks the committed tree, and this checkout has changes it doesn't hold; commit them, or check without gate-reuse:\n{changed}"
            )));
        }
        let prefix = string(caller(&["rev-parse", "--show-prefix"]))?;
        let tree = string(at(&["rev-parse", "--verify", "HEAD^{tree}"]))?;
        let head = string(at(&["rev-parse", "--verify", "HEAD^{commit}"]))?;
        let format = string(at(&["rev-parse", "--show-object-format"]))?;
        let objects = Path::new(&string(at(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ]))?)
        .join("objects");
        let (base, _) = snapshot::submodule_base(top)?;
        let work = Work::new("tree-source")?;
        let source = work.0.join("source");
        fs::create_dir(&source)?;
        checked(git(
            &source,
            &["init", "-q", &format!("--object-format={format}")],
        ))?;
        // The tree's objects, read from the checkout's store: a link keeps
        // its path's bytes out of the alternates file's line format.
        std::os::unix::fs::symlink(&objects, source.join(".git/borrowed-objects"))?;
        fs::write(
            source.join(".git/objects/info/alternates"),
            "../borrowed-objects\n",
        )?;
        let config = source.join(".git/config");
        fs::write(&config, "")?;
        for (k, v) in [
            ("core.repositoryformatversion", "0"),
            ("core.bare", "false"),
            ("remote.origin.url", base.as_str()),
        ] {
            checked(git(&source, &["config", "-f", s(&config), k, v]))?;
        }
        if format != "sha1" {
            checked(git(
                &source,
                &[
                    "config",
                    "-f",
                    s(&config),
                    "core.repositoryformatversion",
                    "1",
                ],
            ))?;
            checked(git(
                &source,
                &[
                    "config",
                    "-f",
                    s(&config),
                    "extensions.objectformat",
                    &format,
                ],
            ))?;
        }
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
        checked(git(&source, &["update-ref", "--no-deref", "HEAD", &commit]))?;
        checked(git(&source, &["read-tree", "HEAD"]))?;
        checked(git(&source, &["checkout-index", "-a", "-f"]))?;
        println!(
            "gate-reuse: checking tree {tree} as {commit}, a commit of it alone (checked out: {head})"
        );
        std::env::set_current_dir(source.join(&prefix))?;
        PRIVATE_SOURCE.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(TreeSource { _work: work })
    }
}
