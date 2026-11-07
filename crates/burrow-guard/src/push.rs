//! R2: a force-push or delete of a default branch rewrites history that
//! every other clone depends on. What a push targets is read from its
//! refspecs, or, with none, from the branch checked out where it runs and
//! that branch's upstream (`git push --force` on main).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::shell;
use crate::walk::{self, Unread};

/// Branches other clones build on. HEAD and @ are whatever is checked out,
/// which may be one of them.
const SHARED: [&str; 5] = ["main", "master", "trunk", "HEAD", "@"];

/// The question for each push in `cmd` that can rewrite or delete a shared
/// branch; `cwd` is where the command runs. A command nested deeper than
/// the guard reads asks, since a push in it would go unseen, and so does one
/// too long to read in time (walk.rs) or within [`PLACES`].
pub fn asks(cmd: &str, cwd: &Path, git: &Path) -> Vec<String> {
    read(cmd, cwd, git).unwrap_or_else(|why| {
        vec![format!(
            "The guard couldn't check this command ({}), so it asks.",
            why.reason()
        )]
    })
}

fn read(cmd: &str, cwd: &Path, git: &Path) -> Result<Vec<String>, Unread> {
    let mut found = Vec::new();
    walk::walk(cmd, &mut |word, rest, budget, more| {
        if runs_git(word, rest.iter(budget).next())
            && let Some((at, mut push)) = after_git(cwd, rest.iter(budget))
        {
            push.more = more;
            found.push((at, push));
        }
        Ok(())
    })?;
    let mut places = Places {
        git,
        seen: HashMap::new(),
    };
    let mut asks = Vec::new();
    for (at, push) in found {
        asks.extend(push.ask(&at, &mut places)?);
    }
    Ok(asks)
}

/// How many places a command's pushes with no refspec may ask git about.
/// Each is two processes.
const PLACES: usize = 32;

/// Whether `word` can run git: it ends so, whatever's joined on before it
/// (a path, `--command=git`, `!git` in a git alias, `arr[0]=git`, `${x}git`),
/// or an expansion that ends so (`${git}`) or is followed by `push`, as
/// whatever it holds may be git (`$GIT push`).
pub fn runs_git(word: &str, next: Option<&String>) -> bool {
    if word.ends_with("git") {
        return true;
    }
    (word.contains('$') || word.contains(shell::UNKNOWN))
        && (word.trim_end_matches('}').ends_with("git") || next.is_some_and(|n| n == "push"))
}

/// What follows `git`, if it's a push: git's own options (each `-C DIR`
/// taken in turn for where it runs), then `push` and its words. An
/// expansion among them may be an option, or nothing at all.
fn after_git<'a>(
    cwd: &Path,
    mut words: impl Iterator<Item = &'a String>,
) -> Option<(PathBuf, Push)> {
    let mut at = cwd.to_path_buf();
    while let Some(w) = words.next() {
        match w.as_str() {
            "push" => return Some((at, Push::read(words))),
            "-C" => at = at.join(words.next()?),
            // Options whose value is the next word.
            "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env"
            | "--attr-source" | "--super-prefix" => {
                words.next();
            }
            w if w.starts_with('-') || w.contains(['$', shell::UNKNOWN]) => {}
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
    /// More of its words come from its input, unseen (`xargs`).
    more: bool,
}

impl Push {
    fn read<'a>(mut words: impl Iterator<Item = &'a String>) -> Push {
        let mut p = Push::default();
        while let Some(w) = words.next() {
            match w.as_str() {
                // The rest are the remote and refspecs, whatever they look
                // like.
                "--" => {
                    words.by_ref().for_each(|w| p.positional(w));
                    break;
                }
                // Its text was read in ahead of it, in its place. What `x`
                // holds if set isn't known, so it isn't taken for the remote.
                w if shell::is_only_a_parameter_text(w) => {}
                w if let Some(option) = w.strip_prefix("--") => {
                    let (name, value) = option.split_once('=').unzip();
                    match long(name.unwrap_or(option)) {
                        Long::Force => p.force = true,
                        Long::All => p.all = true,
                        Long::Everything => (p.force, p.all) = (true, true),
                        Long::Value if value.is_none() => {
                            words.next();
                        }
                        Long::Value | Long::Other => {}
                    }
                }
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
                w => p.positional(w),
            }
        }
        p
    }

    /// The remote or a refspec. A `+` forces, and an empty source deletes;
    /// `:` alone is an ordinary push of the branches both sides have.
    fn positional(&mut self, word: &str) {
        self.force |= word.starts_with('+') || (word.starts_with(':') && word != ":");
        self.positional.push(word.to_owned());
    }

    fn ask(&self, at: &Path, places: &mut Places<'_>) -> Result<Option<String>, Unread> {
        if !self.force {
            return Ok(None);
        }
        if self.all {
            return Ok(Some(
                "Push that can rewrite or delete every remote branch (R2: rewrites history other clones depend on)."
                    .to_owned(),
            ));
        }
        if self.more {
            return Ok(Some(
                "Force-push or delete of whatever its input names (R2: rewrites history other clones depend on)."
                    .to_owned(),
            ));
        }
        let shared = if self.positional.len() > 1 {
            self.positional[1..]
                .iter()
                .find_map(|refspec| shared_target(refspec))
        } else {
            places.branches(at)?.iter().find_map(|t| shared_target(t))
        };
        Ok(shared.map(|shared| {
            format!(
                "Force-push or delete of '{shared}' (R2: rewrites history other clones depend on)."
            )
        }))
    }
}

/// What git says each place's push with no refspec would push, asked once a
/// place: a command can repeat `git push -f` far faster than git can answer.
struct Places<'a> {
    git: &'a Path,
    seen: HashMap<PathBuf, Vec<String>>,
}

impl Places<'_> {
    /// The branch checked out at `at`, and its upstream.
    fn branches(&mut self, at: &Path) -> Result<&[String], Unread> {
        if !self.seen.contains_key(at) {
            if self.seen.len() == PLACES {
                return Err(Unread::Long);
            }
            let branch = run(self.git, at, &["symbolic-ref", "--short", "-q", "HEAD"]);
            let upstream = run(
                self.git,
                at,
                &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
            )
            .map(|u| u.split_once('/').map_or(u.clone(), |(_, b)| b.to_owned()));
            self.seen.insert(
                at.to_path_buf(),
                branch.into_iter().chain(upstream).collect(),
            );
        }
        Ok(&self.seen[at])
    }
}

/// What `refspec` updates, if that can be a shared branch: its destination,
/// or its source where it names no other, by name or by wildcard. `:` alone
/// is every branch both sides have.
fn shared_target(refspec: &str) -> Option<String> {
    let spec = refspec.strip_prefix('+').unwrap_or(refspec);
    let (source, dest) = spec.rsplit_once(':').unwrap_or((spec, spec));
    let dest = if dest.is_empty() { source } else { dest };
    if dest.is_empty() {
        return Some(":".to_owned());
    }
    let name = dest
        .strip_prefix("refs/heads/")
        .or_else(|| dest.strip_prefix("heads/"))
        .unwrap_or(dest);
    let shared = match name.split_once('*') {
        None => SHARED.contains(&name),
        Some((before, after)) => SHARED.iter().any(|branch| {
            [
                (*branch).to_owned(),
                format!("heads/{branch}"),
                format!("refs/heads/{branch}"),
            ]
            .iter()
            .any(|full| {
                full.len() >= before.len() + after.len()
                    && full.starts_with(before)
                    && full.ends_with(after)
            })
        }),
    };
    shared.then(|| name.to_owned())
}

/// What a long option of `git push` means here.
#[derive(Clone, Copy)]
enum Long {
    /// It can rewrite or delete what it targets.
    Force,
    /// It targets every branch.
    All,
    /// It force-updates and deletes every remote ref by design.
    Everything,
    /// Its value is the next word, unless given with `=`.
    Value,
    Other,
}

/// `git push`'s long options (`git push -h`).
const LONG: [(&str, Long); 28] = [
    ("verbose", Long::Other),
    ("quiet", Long::Other),
    ("repo", Long::Value),
    ("all", Long::All),
    ("branches", Long::All),
    ("mirror", Long::Everything),
    ("delete", Long::Force),
    ("tags", Long::Other),
    ("dry-run", Long::Other),
    ("porcelain", Long::Other),
    ("force", Long::Force),
    ("force-with-lease", Long::Force),
    ("force-if-includes", Long::Force),
    ("recurse-submodules", Long::Value),
    ("thin", Long::Other),
    ("receive-pack", Long::Value),
    ("exec", Long::Value),
    ("set-upstream", Long::Other),
    ("progress", Long::Other),
    ("prune", Long::Everything),
    ("no-verify", Long::Other),
    ("verify", Long::Other),
    ("follow-tags", Long::Other),
    ("signed", Long::Other),
    ("atomic", Long::Other),
    ("push-option", Long::Value),
    ("ipv4", Long::Other),
    ("ipv6", Long::Other),
];

/// The option `--NAME` is, as git reads it: the one named so, or the only
/// one NAME begins (`--del` is `--delete`). A `--no-…` takes no value and
/// forces nothing, and one that's unknown or could be several stops git
/// before it pushes.
fn long(name: &str) -> Long {
    if let Some(&(_, long)) = LONG.iter().find(|(n, _)| *n == name) {
        return long;
    }
    let mut begun = LONG.iter().filter(|(n, _)| n.starts_with(name));
    match (begun.next(), begun.next()) {
        (Some(&(_, long)), None) => long,
        _ => Long::Other,
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
