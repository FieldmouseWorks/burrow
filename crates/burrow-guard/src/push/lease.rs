//! Why a push is refused outright, whoever runs it and whatever branch it
//! names: it can force an update without a lease on the head its pusher
//! saw. Git's own `--force-with-lease`, bare, compares the remote with the
//! remote-tracking ref, which any fetch moves, so a branch rebuilt from a
//! stale local copy after a fetch still overwrites the commits it never
//! held; `--force-if-includes` adds that the remote-tracking tip is in the
//! local branch's reflog, that is, was taken in. A lease naming the object
//! it expects (`=BRANCH:SHA`, SHA a whole object id in the repository's
//! format) is the head the pusher saw, said outright.
//!
//! What overrides any lease is read as git reads it: the force bit (`-f`,
//! `--force`, `--mirror`), a `+` refspec, and a remote configured to force
//! (`remote.NAME.push` with a `+`, `remote.NAME.mirror`), from the
//! repository's configuration or the command's own (`-c`). A delete
//! rebuilds nothing, so a push that only deletes is let through, forced or
//! not. And a push the guard can't read in full (an expansion where an
//! option or refspec begins, a glob, words from its input, configuration
//! from the environment, or git that can't answer) may force anything, so
//! it's refused too, unless what it can read already is.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{Lease, PLACES, Push, pushes, resolve, unknown};

/// Why `cmd`, run in `cwd`, is refused: one of its pushes forces without
/// a lease ([`UNLEASED`]) or by its remote's configuration
/// ([`CONFIGURED`]), or can't be read ([`UNREAD`]). A command the guard
/// can't walk is push.rs's `asks`'s. `git` answers what the repository says.
pub fn unleased(cmd: &str, cwd: &Path, git: &Path) -> Option<String> {
    let pushes = pushes(cmd, cwd).ok()?;
    if pushes.is_empty() {
        return None;
    }
    let mut repos = Repos {
        git,
        seen: HashMap::new(),
    };
    let environment = configured_by_environment(cmd);
    let mut unread = false;
    // What it reads that's refused says so first, ahead of what it can't.
    for (at, push) in &pushes {
        match push.verdict(at, environment, &mut repos) {
            Verdict::Leased => {}
            Verdict::Unread => unread = true,
            Verdict::Refused(why) => return Some(why.to_owned()),
        }
    }
    unread.then(|| UNREAD.to_owned())
}

/// What [`unleased`] says of a push that forces without a lease.
pub const UNLEASED: &str = "Force-push without a lease on the head you last saw: another job or session may have pushed to this branch since, and this would drop their commits. Fetch, take the remote branch in (rebase onto it, or reset to it and redo your change), then push with --force-with-lease --force-if-includes and no -f, --force, --mirror or +refspec, or name the commit you mean to replace with --force-with-lease=BRANCH:SHA (SHA the whole object id).";

/// What [`unleased`] says of a push its remote's configuration forces.
pub const CONFIGURED: &str = "Push its remote's git configuration forces (a remote.NAME.push refspec with +, or remote.NAME.mirror), which overrides any lease: another job's or session's commits on the branch would be dropped. Name the destination in the refspec (HEAD:refs/heads/BRANCH), with --force-with-lease --force-if-includes if it has to force, or take the forcing out of the configuration.";

/// What [`unleased`] says of a push it can't read.
pub const UNREAD: &str = "The guard can't read all of this push (an expansion where an option or refspec begins, a glob, words from its input, git configuration set in the environment or by --config-env, or a place or configuration git couldn't report), so it can't tell whether it forces without a lease on the head you last saw. Write the push out in full: its options, remote and refspecs as plain text, in the repository it runs in (git -C DIR with DIR spelled out).";

/// A push's verdict.
enum Verdict {
    Leased,
    Unread,
    Refused(&'static str),
}

impl Push {
    fn verdict(&self, at: &Path, environment: bool, repos: &mut Repos<'_>) -> Verdict {
        let lease = &self.lease;
        let refspecs = self.positional.get(1..).unwrap_or_default();
        // What it updates rather than deletes; with --delete, nothing.
        let updates: Vec<&String> = if lease.delete {
            Vec::new()
        } else {
            refspecs.iter().filter(|r| !deletes(r)).collect()
        };
        // With no refspec it pushes what git's configuration says.
        let updating = !lease.delete && (refspecs.is_empty() || !updates.is_empty());
        let unleased = lease.leases.iter().any(|expect| match expect {
            None => !lease.if_includes,
            Some(oid) => !oid.is_empty() && !is_object_id(oid),
        });
        if updating && (lease.force || updates.iter().any(|r| r.starts_with('+')) || unleased) {
            return Verdict::Refused(UNLEASED);
        }
        if self.globbed || self.more || self.unread || self.repo.unread || environment {
            return Verdict::Unread;
        }
        if !updating {
            return Verdict::Leased;
        }
        let Some(config) = repos.config(at, &self.repo.args) else {
            return Verdict::Unread;
        };
        let remote = self.positional.first().or(self.remote.as_ref());
        if config.forces(remote.map(String::as_str), refspecs, &updates) {
            return Verdict::Refused(CONFIGURED);
        }
        let named: Vec<&String> = lease
            .leases
            .iter()
            .flatten()
            .filter(|o| !o.is_empty())
            .collect();
        if named.is_empty() {
            return Verdict::Leased;
        }
        let Some(lengths) = repos.object_id_lengths(at, &self.repo.args) else {
            return Verdict::Unread;
        };
        if named.iter().all(|oid| lengths.contains(&oid.len())) {
            Verdict::Leased
        } else {
            Verdict::Refused(UNLEASED)
        }
    }
}

/// Whether `refspec` only deletes: its source is empty. `:` alone is an
/// ordinary push of the branches both sides have, and `+:` a forced one.
fn deletes(refspec: &str) -> bool {
    let spec = refspec.strip_prefix('+').unwrap_or(refspec);
    spec.starts_with(':') && spec != ":"
}

/// Whether `text` has the shape of a whole object id, SHA-1's or
/// SHA-256's: not a ref (`origin/x`, which a fetch moves), an abbreviation
/// nor an expansion the guard can't read. Whether its length is the
/// repository's is [`Repos::object_id_lengths`]'s, since git takes an
/// all-hex name of another length for a ref, which a fetch can move.
fn is_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether `cmd` sets an environment variable that changes which git
/// configuration a push reads (`GIT_CONFIG_*`, `GIT_DIR`, `HOME`, …),
/// which the guard can't follow.
fn configured_by_environment(cmd: &str) -> bool {
    crate::pattern(
        r"(^|[^A-Za-z0-9_])(GIT_CONFIG[A-Z0-9_]*|GIT_DIR|GIT_COMMON_DIR|HOME|XDG_CONFIG_HOME)\+?=",
    )
    .is_match(cmd)
}

impl Lease {
    /// Takes in `--NAME[=VALUE]` as git reads it: the option named so, or
    /// the only one NAME begins, undone by `--no-` before it.
    pub(super) fn read(&mut self, name: &str, value: Option<&str>) {
        let (negated, name) = name
            .strip_prefix("no-")
            .map_or((false, name), |rest| (true, rest));
        match (resolve(name), negated) {
            // --mirror and --no-mirror set and clear the force bit with
            // their own.
            (Some("force" | "mirror"), _) => self.force = !negated,
            (Some("delete"), _) => self.delete = !negated,
            (Some("force-with-lease"), false) => self.leases.push(
                value
                    .and_then(|v| v.split_once(':'))
                    .map(|(_, expect)| expect.to_owned()),
            ),
            (Some("force-with-lease"), true) => self.leases.clear(),
            (Some("force-if-includes"), _) => self.if_includes = !negated,
            _ => {}
        }
    }
}

/// A repository's push settings: each remote's `push` refspecs and
/// `mirror`, as `git config` reports them, the command's `-c` included.
struct Config(Vec<(String, String, Option<String>)>);

impl Config {
    /// Whether `remote`'s configuration (every remote's, where the push
    /// names none the guard reads) forces this push: a `+` push refspec
    /// where it names no destination of its own (git maps it, forcing,
    /// even onto a branch it names), or mirroring where it names none.
    fn forces(&self, remote: Option<&str>, refspecs: &[String], updates: &[&String]) -> bool {
        let applies = |name: &str| remote.is_none_or(|r| unknown(r) || r == name);
        let mapped = refspecs.is_empty() || updates.iter().any(|r| !r.contains(':'));
        let forced = self.0.iter().any(|(name, key, value)| {
            applies(name) && key == "push" && value.as_deref().is_some_and(|v| v.starts_with('+'))
        });
        let mut mirrors: HashMap<&str, bool> = HashMap::new();
        for (name, key, value) in &self.0 {
            if key == "mirror" && applies(name) {
                mirrors.insert(name, truthy(value.as_deref()));
            }
        }
        (forced && mapped) || (refspecs.is_empty() && mirrors.values().any(|&m| m))
    }
}

/// A boolean setting as git reads it: a key with no value is true.
fn truthy(value: Option<&str>) -> bool {
    value.is_none_or(|v| {
        !matches!(
            v.to_ascii_lowercase().as_str(),
            "" | "false" | "no" | "off" | "0"
        )
    })
}

/// What git says of each place a command pushes from, asked once a place:
/// a command can repeat a push far faster than git can answer.
struct Repos<'a> {
    git: &'a Path,
    seen: HashMap<(PathBuf, Vec<String>, &'static str), Option<String>>,
}

impl Repos<'_> {
    /// The push settings at `at`, with git's options `args`; None if git
    /// can't say.
    fn config(&mut self, at: &Path, args: &[String]) -> Option<Config> {
        let out = self.ask(
            at,
            args,
            &[
                "config",
                "--null",
                "--get-regexp",
                r"^remote\..*\.(push|mirror)$",
            ],
            "config",
        )?;
        let entries = out
            .split('\0')
            .filter(|e| !e.is_empty())
            .filter_map(|entry| {
                let (key, value) = entry
                    .split_once('\n')
                    .map_or((entry, None), |(k, v)| (k, Some(v.to_owned())));
                let (name, setting) = key.strip_prefix("remote.")?.rsplit_once('.')?;
                Some((name.to_owned(), setting.to_owned(), value))
            })
            .collect();
        Some(Config(entries))
    }

    /// The lengths of a whole object id in the repository at `at`: its
    /// storage format's, and its compatibility format's if it has one.
    fn object_id_lengths(&mut self, at: &Path, args: &[String]) -> Option<Vec<usize>> {
        let out = self.ask(
            at,
            args,
            &[
                "rev-parse",
                "--show-object-format=storage",
                "--show-object-format=compat",
            ],
            "format",
        )?;
        out.lines()
            .filter(|l| !l.is_empty())
            .map(|format| match format {
                "sha1" => Some(40),
                "sha256" => Some(64),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .filter(|lengths| !lengths.is_empty())
    }

    /// What `git -C AT ARGS QUERY` prints: empty where it finds nothing
    /// (exit 1), None where it fails otherwise or the command asks about
    /// more places than [`PLACES`].
    fn ask(
        &mut self,
        at: &Path,
        args: &[String],
        query: &[&str],
        what: &'static str,
    ) -> Option<String> {
        let key = (at.to_path_buf(), args.to_vec(), what);
        if let Some(seen) = self.seen.get(&key) {
            return seen.clone();
        }
        if self.seen.len() == PLACES {
            return None;
        }
        let answer = Command::new(self.git)
            .arg("-C")
            .arg(at)
            .args(args)
            .args(query)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .and_then(|out| match out.status.code() {
                Some(0) => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
                Some(1) => Some(String::new()),
                _ => None,
            });
        self.seen.insert(key, answer.clone());
        answer
    }
}

#[cfg(test)]
mod tests;
