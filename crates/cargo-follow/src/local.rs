//! What a pass does on the machine: the upstream's two trees compared, and
//! the bump made in a checkout of the follower, with cargo writing the lock.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::changes;
use crate::manifest::{self, Follow};

/// git in one repo.
pub struct Git {
    dir: PathBuf,
}

impl Git {
    pub fn new(dir: PathBuf) -> Self {
        Git { dir }
    }

    pub fn run(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .output()
            .map_err(|e| format!("git: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git {} in {}: {}",
                args.join(" "),
                self.dir.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(out.stdout)
    }

    /// A file at a commit, or None when it has none there.
    pub fn show(&self, rev: &str, path: &str) -> Result<Option<Vec<u8>>, String> {
        let spec = format!("{rev}:{path}");
        if self.run(&["cat-file", "-e", &spec]).is_err() {
            return Ok(None);
        }
        self.run(&["cat-file", "blob", &spec]).map(Some)
    }
}

/// What a pass asks of the machine, apart so its tests can answer.
pub trait Local {
    /// The upstream's changes from the follow's pin to `head` that count
    /// (changes.rs).
    fn changes(&mut self, follow: &Follow, head: &str) -> Result<Vec<String>, String>;
    /// The follower at `base`, with every dependency on the follow's URL
    /// moved to `head` and its lock file to match: each file that changed,
    /// and its new bytes.
    fn bump(
        &mut self,
        repo: &str,
        base: &str,
        manifest: &str,
        follow: &Follow,
        head: &str,
    ) -> Result<Vec<(String, Vec<u8>)>, String>;
}

/// The machine's: clones under `root`, kept between passes so each fetch
/// brings only what's new.
pub struct Work {
    pub root: PathBuf,
}

fn url(repo: &str) -> String {
    format!("https://github.com/{repo}")
}

impl Work {
    /// The repo's clone under `kind`, made when it has none.
    fn clone(&self, kind: &str, repo: &str, bare: bool) -> Result<Git, String> {
        let dir = self.root.join(kind).join(repo.to_ascii_lowercase());
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let git = Git::new(dir.clone());
            git.run(if bare {
                &["init", "-q", "--bare"]
            } else {
                &["init", "-q"]
            })?;
        }
        Ok(Git::new(dir))
    }
}

impl Local for Work {
    fn changes(&mut self, follow: &Follow, head: &str) -> Result<Vec<String>, String> {
        let git = self.clone("upstream", &follow.upstream, true)?;
        git.run(&[
            "fetch",
            "-q",
            "--no-tags",
            &url(&follow.upstream),
            &follow.pin,
            head,
        ])?;
        changes::changes(&git, &follow.rules, &follow.pin, head)
    }

    fn bump(
        &mut self,
        repo: &str,
        base: &str,
        manifest: &str,
        follow: &Follow,
        head: &str,
    ) -> Result<Vec<(String, Vec<u8>)>, String> {
        let git = self.clone("follower", repo, false)?;
        git.run(&["fetch", "-q", "--no-tags", &url(repo), base])?;
        git.run(&["checkout", "-q", "-f", "--detach", base])?;
        git.run(&["clean", "-q", "-f", "-d", "-x"])?;
        let path = git.dir.join(manifest);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let text = manifest::moved(&text, follow, head).map_err(|e| format!("{manifest}: {e}"))?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        cargo_update(&git.dir, &path, &self.root.join("cargo-home"))?;
        let status = git.run(&["status", "--porcelain", "-z", "--untracked-files=no"])?;
        let mut changed = Vec::new();
        for entry in status.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let entry = String::from_utf8_lossy(entry);
            let (state, file) = entry.split_at(3.min(entry.len()));
            if state.trim() != "M" {
                return Err(format!(
                    "{repo}: the bump left {file} {state}, not just changed"
                ));
            }
            let bytes = std::fs::read(git.dir.join(file)).map_err(|e| format!("{file}: {e}"))?;
            changed.push((file.to_string(), bytes));
        }
        Ok(changed)
    }
}

/// The lock file brought to the manifest as Renovate's cargo manager does
/// after it edits one: the workspace's own dependencies re-resolved, nothing
/// else updated.
fn cargo_update(dir: &Path, manifest: &Path, cargo_home: &Path) -> Result<(), String> {
    let out = Command::new("cargo")
        .args([
            "update",
            "--config",
            "net.git-fetch-with-cli=true",
            "--workspace",
            "--manifest-path",
        ])
        .arg(manifest)
        .current_dir(dir)
        .env("CARGO_HOME", cargo_home)
        .output()
        .map_err(|e| format!("cargo: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo update in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}
