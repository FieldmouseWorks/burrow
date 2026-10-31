//! R2: a force-push or delete of a default branch rewrites history that
//! every other clone depends on. What a push targets is read from its
//! refspecs, or, with none, from the branch checked out where it runs and
//! that branch's upstream (`git push --force` on main).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use regex::Regex;

use crate::pattern;

/// Branches other clones build on. HEAD and @ are whatever is checked out,
/// which may be one of them.
const SHARED: [&str; 5] = ["main", "master", "trunk", "HEAD", "@"];

/// `git [-C DIR | -c NAME=VALUE]... push ARGS`, ARGS up to the end of the
/// line or the next `;`, `&` or `|`.
static PUSH: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"git(?P<opts>([[:space:]]+-[Cc][[:space:]]+[^[:space:];&|]+)*)[[:space:]]+push(?P<args>[[:space:]][^;&|]*)?",
    )
});

/// The question for each push in `cmd` that can rewrite or delete a shared
/// branch; `cwd` is where the command runs.
pub fn asks(cmd: &str, cwd: &Path, git: &Path) -> Vec<String> {
    let mut asks = Vec::new();
    for line in cmd.split('\n') {
        for m in PUSH.captures_iter(line) {
            let at = dir(cwd, m.name("opts").map_or("", |o| o.as_str()));
            let push = Push::read(&words(m.name("args").map_or("", |a| a.as_str())));
            asks.extend(push.ask(&at, git));
        }
    }
    asks
}

/// A push's words, quotes and backslashes taken off: `"+main"` is `+main`.
fn words(args: &str) -> Vec<String> {
    args.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| !matches!(c, '\'' | '"' | '\\'))
                .collect()
        })
        .collect()
}

/// Where git runs: `cwd`, then each `-C DIR` in turn, as git takes them.
fn dir(cwd: &Path, opts: &str) -> PathBuf {
    let mut dir = cwd.to_path_buf();
    let mut words = words(opts).into_iter();
    while let (Some(opt), Some(value)) = (words.next(), words.next()) {
        if opt == "-C" {
            dir = dir.join(value);
        }
    }
    dir
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
