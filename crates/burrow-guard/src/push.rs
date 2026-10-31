//! R2: a force-push or delete of a default branch rewrites history that
//! every other clone depends on. What a push targets is read from its
//! refspecs, or, with none, from the branch checked out where it runs and
//! that branch's upstream (`git push --force` on main).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::shell;

/// Branches other clones build on. HEAD and @ are whatever is checked out,
/// which may be one of them.
const SHARED: [&str; 5] = ["main", "master", "trunk", "HEAD", "@"];

/// The question for each push in `cmd` that can rewrite or delete a shared
/// branch; `cwd` is where the command runs.
pub fn asks(cmd: &str, cwd: &Path, git: &Path) -> Vec<String> {
    pushes(cmd, cwd)
        .into_iter()
        .filter_map(|(at, push)| push.ask(&at, git))
        .collect()
}

/// Each `git … push …` in `text`, with where it runs, read word by word as
/// the shell splits them. A word with a command line in it (a script for
/// `bash -c`, `ssh` or `$(…)`) is read the same way, since it may run.
fn pushes(text: &str, cwd: &Path) -> Vec<(PathBuf, Push)> {
    let mut found = Vec::new();
    for words in shell::commands(text) {
        for (i, word) in words.iter().enumerate() {
            if word.contains(char::is_whitespace) {
                found.extend(pushes(word, cwd));
            }
            if (word == "git" || word.ends_with("/git"))
                && let Some(push) = after_git(cwd, &words[i + 1..])
            {
                found.push(push);
            }
        }
    }
    found
}

/// What follows `git`, if it's a push: git's own options (each `-C DIR`
/// taken in turn for where it runs), then `push` and its words.
fn after_git(cwd: &Path, words: &[String]) -> Option<(PathBuf, Push)> {
    let mut at = cwd.to_path_buf();
    let mut words = words.iter();
    while let Some(w) = words.next() {
        match w.as_str() {
            "push" => return Some((at, Push::read(words.as_slice()))),
            "-C" => at = at.join(words.next()?),
            // Options whose value is the next word.
            "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => {
                words.next();
            }
            w if w.starts_with('-') => {}
            _ => return None,
        }
    }
    None
}

#[derive(Debug, Default)]
struct Push {
    /// It can rewrite or delete what it targets.
    force: bool,
    /// It targets every branch.
    all: bool,
    /// The remote, then the refspecs.
    positional: Vec<String>,
}

impl Push {
    fn read(words: &[String]) -> Push {
        let mut p = Push::default();
        let mut words = words.iter();
        while let Some(w) = words.next() {
            match w.as_str() {
                "--force" | "--force-if-includes" | "--delete" => p.force = true,
                w if w.starts_with("--force-with-lease") => p.force = true,
                "--all" | "--branches" => p.all = true,
                // They force-update and delete remote refs by design.
                "--mirror" | "--prune" => (p.force, p.all) = (true, true),
                // Options whose value is the next word, not a refspec.
                "--push-option" | "--receive-pack" | "--exec" | "--repo" => {
                    words.next();
                }
                w if w.starts_with("--") => {}
                // Short options, apart or together: `-f`, `-d` and `-uf`
                // force; `-o` takes the rest of its word, or the next word.
                w if w.starts_with('-') => {
                    for (i, c) in w.char_indices().skip(1) {
                        match c {
                            'f' | 'd' => p.force = true,
                            'o' => {
                                if i + 1 == w.len() {
                                    words.next();
                                }
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                w if w.starts_with(['+', ':']) => {
                    p.force = true;
                    p.positional.push(w[1..].to_owned());
                }
                w => p.positional.push(w.to_owned()),
            }
        }
        p
    }

    fn ask(&self, at: &Path, git: &Path) -> Option<String> {
        if !self.force {
            return None;
        }
        if self.all {
            return Some(
                "Push that can rewrite or delete every remote branch (R2: rewrites history other clones depend on)."
                    .to_owned(),
            );
        }
        let targets: Vec<String> = if self.positional.len() > 1 {
            self.positional[1..]
                .iter()
                .map(|refspec| {
                    let dest = refspec.rsplit(':').next().unwrap_or(refspec);
                    dest.strip_prefix("refs/heads/").unwrap_or(dest).to_owned()
                })
                .collect()
        } else {
            let branch = run(git, at, &["symbolic-ref", "--short", "-q", "HEAD"]);
            let upstream = run(
                git,
                at,
                &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
            )
            .map(|u| u.split_once('/').map_or(u.clone(), |(_, b)| b.to_owned()));
            branch.into_iter().chain(upstream).collect()
        };
        let shared = targets.iter().find(|t| SHARED.contains(&t.as_str()))?;
        Some(format!(
            "Force-push or delete of '{shared}' (R2: rewrites history other clones depend on)."
        ))
    }
}

/// What `git -C AT ARGS` prints, trimmed, if it succeeds.
fn run(git: &Path, at: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new(git)
        .arg("-C")
        .arg(at)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
}
