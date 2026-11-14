//! R2: a force-push or delete of a default branch rewrites history that
//! every other clone depends on. What a push targets is read from its
//! refspecs, or, with none, from the branch checked out where it runs and
//! that branch's upstream (`git push --force` on main).
//!
//! And any branch's forced update is refused unless it holds a lease on
//! the head its pusher saw ([`unleased`]): two jobs, or a job and a
//! session, can work on one PR, and a force-push rebuilt from a stale
//! branch drops whatever the other pushed since.

use std::cell::Cell;
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
    let mut places = Places {
        git,
        seen: HashMap::new(),
    };
    let mut asks = Vec::new();
    for (at, push) in pushes(cmd, cwd)? {
        asks.extend(push.ask(&at, &mut places)?);
    }
    Ok(asks)
}

/// Why a push in `cmd` is refused outright, whoever runs it and whatever
/// branch it names: it can force an update without a lease on the head
/// its pusher saw. Git's own `--force-with-lease`, bare, compares the
/// remote with the remote-tracking ref, which any fetch moves, so a branch
/// rebuilt from a stale local copy after a fetch still overwrites the
/// commits it never held; `--force-if-includes` adds that the
/// remote-tracking tip is in the local branch's reflog, that is, was
/// taken in. A lease naming the object it expects (`=BRANCH:SHA`) is the
/// head the pusher saw, said outright. `-f`, `--force`, a `+` refspec or
/// `--mirror` override any lease. A command the guard can't read is
/// [`asks`]'s.
pub fn unleased(cmd: &str, cwd: &Path) -> Option<String> {
    let pushes = pushes(cmd, cwd).ok()?;
    pushes
        .iter()
        .any(|(_, p)| !p.leased())
        .then(|| UNLEASED.to_owned())
}

/// What [`unleased`] says.
pub const UNLEASED: &str = "Force-push without a lease on the head you last saw: another job or session may have pushed to this branch since, and this would drop their commits. Fetch, take the remote branch in (rebase onto it, or reset to it and redo your change), then push with --force-with-lease --force-if-includes and no -f, --force or +refspec, or name the commit you mean to replace with --force-with-lease=BRANCH:SHA.";

/// Each push in `cmd`, with where it runs.
fn pushes(cmd: &str, cwd: &Path) -> Result<Vec<(PathBuf, Push)>, Unread> {
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
    Ok(found)
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
/// expansion among them may be an option, or nothing at all; a glob may be
/// any number of words, `push` and its words among them, so it's read as
/// a push the guard can't read.
fn after_git<'a>(cwd: &Path, words: impl Iterator<Item = &'a String>) -> Option<(PathBuf, Push)> {
    let mut at = cwd.to_path_buf();
    // Every word it reads, an option's value too.
    let globbed = Cell::new(false);
    let mut words = words.inspect(|w| globbed.set(globbed.get() || shell::globs(w)));
    let unread = || {
        let push = Push {
            globbed: true,
            ..Push::default()
        };
        Some((cwd.to_path_buf(), push))
    };
    while let Some(w) = words.next() {
        match w.as_str() {
            _ if globbed.get() => return unread(),
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
    if globbed.get() { unread() } else { None }
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
    /// A glob among its words, which the shell makes into the names of
    /// whatever files match it: any number of words, which may force
    /// (`-f`, `+…`) and name any branch.
    globbed: bool,
    /// Its forcing options as git reads them, for [`Push::leased`].
    lease: Lease,
}

/// What a push forces with, as git reads its options: the last of each
/// wins.
#[derive(Debug, Default)]
struct Lease {
    /// `-f` or `--force` (undone by a later `--no-force`), which overrides
    /// any lease.
    force: bool,
    /// `--mirror`, which force-updates every ref.
    mirror: bool,
    /// Each `--force-with-lease` since the last `--no-force-with-lease`:
    /// the object it expects, or None for the remote-tracking ref's.
    leases: Vec<Option<String>>,
    /// `--force-if-includes` (undone by a later `--no-force-if-includes`).
    if_includes: bool,
}

impl Push {
    fn read<'a>(words: impl Iterator<Item = &'a String>) -> Push {
        let mut p = Push::default();
        // Every word it reads, an option's value too.
        let globbed = Cell::new(false);
        let mut words = words.inspect(|w| globbed.set(globbed.get() || shell::globs(w)));
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
                    p.lease.read(name.unwrap_or(option), value);
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
                            'f' => (p.force, p.lease.force) = (true, true),
                            'd' => p.force = true,
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
        p.globbed = globbed.get();
        p
    }

    /// The remote or a refspec. A `+` forces, and an empty source deletes;
    /// `:` alone is an ordinary push of the branches both sides have.
    fn positional(&mut self, word: &str) {
        self.force |= word.starts_with('+') || (word.starts_with(':') && word != ":");
        self.positional.push(word.to_owned());
    }

    /// Whether it can force an update only where the remote still holds
    /// what its pusher saw ([`unleased`]): each lease checks an object id
    /// it names or, with `--force-if-includes`, a remote-tracking tip the
    /// local branch took in, and nothing overrides them. One that forces
    /// nothing is leased: git only fast-forwards it. One the guard can't
    /// read is [`Push::ask`]'s.
    fn leased(&self) -> bool {
        let lease = &self.lease;
        if self.globbed || self.more {
            return true;
        }
        if lease.force || lease.mirror || self.positional.iter().any(|w| w.starts_with('+')) {
            return false;
        }
        lease.leases.iter().all(|expect| match expect {
            None => lease.if_includes,
            Some(oid) => oid.is_empty() || is_object_id(oid),
        })
    }

    fn ask(&self, at: &Path, places: &mut Places<'_>) -> Result<Option<String>, Unread> {
        if self.globbed {
            return Ok(Some(
                "Push with a glob among its words, which may become any of them, forcing and naming any branch (R2: rewrites history other clones depend on)."
                    .to_owned(),
            ));
        }
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

impl Lease {
    /// Takes in `--NAME[=VALUE]` as git reads it: the option named so, or
    /// the only one NAME begins, undone by `--no-` before it.
    fn read(&mut self, name: &str, value: Option<&str>) {
        let (negated, name) = name
            .strip_prefix("no-")
            .map_or((false, name), |rest| (true, rest));
        match (resolve(name), negated) {
            (Some("force"), _) => self.force = !negated,
            (Some("mirror"), false) => self.mirror = true,
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

/// Whether `text` is a whole object id, as a lease's expected value must
/// be to name the commit its pusher saw: not a ref (`origin/x`, which a
/// fetch moves) nor an expansion the guard can't read.
fn is_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// What `refspec` updates, if that can be a shared branch: its destination,
/// or its source where it names no other, by name or by git's wildcard `*`
/// (a glob the shell expands asks before this: see [`Push::globbed`]). `:`
/// alone is every branch both sides have.
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
    let shared = if name.contains('*') {
        SHARED.iter().any(|branch| {
            [
                (*branch).to_owned(),
                format!("heads/{branch}"),
                format!("refs/heads/{branch}"),
            ]
            .iter()
            .any(|full| wildcard(name, full))
        })
    } else {
        SHARED.contains(&name)
    };
    shared.then(|| name.to_owned())
}

/// Whether `pattern`, whose `*`s may each become any text, can become
/// `text`.
fn wildcard(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let mut parts: Vec<&str> = parts.collect();
    let Some(last) = parts.pop() else {
        return rest.is_empty();
    };
    for part in parts {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
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
    resolve(name)
        .and_then(|name| LONG.iter().find(|(n, _)| *n == name))
        .map_or(Long::Other, |&(_, long)| long)
}

/// The full name of the option `--NAME`, if git takes it for one.
fn resolve(name: &str) -> Option<&'static str> {
    if let Some(&(n, _)) = LONG.iter().find(|(n, _)| *n == name) {
        return Some(n);
    }
    let mut begun = LONG.iter().filter(|(n, _)| n.starts_with(name));
    match (begun.next(), begun.next()) {
        (Some(&(n, _)), None) => Some(n),
        _ => None,
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{asks, unleased};

    /// What the guard asks of `cmd`; each names its refspecs, so git is
    /// never asked.
    fn said(cmd: &str) -> Vec<String> {
        asks(cmd, Path::new("/"), Path::new("git"))
    }

    #[test]
    fn a_wildcard_or_a_brace_list_in_a_destination_is_read() {
        for cmd in [
            // Git's own wildcard, quoted from the shell.
            "git push -f origin 'refs/heads/*:refs/heads/*'",
            "git push origin '+refs/heads/*:refs/heads/*'",
            "git push origin '+refs/*:refs/*'",
            "git push -f origin 'refs/heads/m*:refs/heads/m*'",
            "git push -f origin 'refs/heads/*'",
            "git push origin -- '+refs/heads/*:refs/heads/*'",
            "git push --force origin 'x:*'",
            "git push --force origin 'x:*a*n'",
            // A brace list is the words it makes, whichever of them it is.
            "git push -f origin {main,x}",
            "git push -f origin x:m{a,i}in",
            "git push -f origin x:refs/heads/{{},main}",
            "git push -f origin x:{a,{main,b}}",
            "git push -f origin {l..n}ain",
            "git push -f origin m{\"{\",ain}",
            "git push origin {+HEAD:refs/heads/main,HEAD:refs/heads/work}",
            "git push origin {--force,main}",
            "git push {-f,origin,main}",
            "git {push,-f} origin main",
            "{git,push,-f,origin,main}",
            "git {,p\"\"ush} -f origin main",
            "bash -c 'git push -f origin x:{main,y}'",
            "sh -c \"git push origin {+x:main,y}\"",
        ] {
            assert!(
                said(cmd)
                    .iter()
                    .any(|s| s.starts_with("Force-push or delete of '")),
                "{cmd}: {:?}",
                said(cmd)
            );
        }
        for cmd in [
            "git push -f origin 'refs/heads/f*:refs/heads/f*'",
            "git push -f origin '+refs/heads/*:refs/tags/*'",
            "git push -f origin 'x:*a'",
            "git push origin 'refs/heads/*:refs/heads/*'",
            "git push -f origin feature-{a,b}",
            "git push -f origin {x,y}:refs/heads/f{1..3}",
            "git push origin {main,x}",
            "git push -f origin 'x:{main,y}'",
            "git push -f origin feature $(echo main)",
        ] {
            assert_eq!(said(cmd), Vec::<String>::new(), "{cmd}");
        }
    }

    /// A glob the shell expands may become any number of file names, so a
    /// push that holds one, forced or not, may force and name any branch.
    #[test]
    fn a_push_with_a_glob_in_its_words_asks() {
        for cmd in [
            "git push -f origin refs/heads/*:refs/heads/*",
            "git push origin +refs/heads/*:refs/heads/*",
            "git push origin refs/heads/*:refs/heads/*",
            "git push -f origin ma?n",
            "git push -f origin [m]ain",
            "git push -f origin m[[:alpha:]]in",
            "git push -f origin x:m[[:alpha:]]in",
            "git push -f origin feat?re",
            "git push origin *",
            "git push origin -- x*",
            "git push -o ci.skip* origin x",
            "git pu?h -f origin main",
            "git -C * push origin x",
            "git push -f origin HEAD:refs/heads/ma{A..z..26}in]n",
            "bash -c 'git push origin x:ma?n'",
            "sh -c \"git push -f origin x:refs/heads/f[a-z]*\"",
        ] {
            assert!(
                said(cmd)
                    .iter()
                    .any(|s| s.starts_with("Push with a glob among its words")),
                "{cmd}: {:?}",
                said(cmd)
            );
        }
        for cmd in [
            "git push origin 'x:ma?n'",
            "git push -f origin \"feat?re\"",
            "git push -f origin feat\\?re",
            "bash -c \"git push origin 'x:ma?n'\"",
            "git add *.rs",
        ] {
            assert_eq!(said(cmd), Vec::<String>::new(), "{cmd}");
        }
    }

    #[test]
    fn a_push_after_a_here_document_with_a_glob_in_its_end_word_is_read() {
        for cmd in [
            "cat <<EOF?\nbody\nEOF?\ngit push \\\n-f origin main",
            "cat <<-E*\n\tbody\n\tE*\ngit push \\\n-f origin main",
            "cat <<'E'{a,b}\nbody\nE{a,b}\ngit push \\\n-f origin main",
            "echo \"$(cat <<E?\nbody\nE?\n)\"; git push \\\n-f origin main",
        ] {
            assert_eq!(
                said(cmd),
                ["Force-push or delete of 'main' (R2: rewrites history other clones depend on)."],
                "{cmd:?}"
            );
        }
    }

    /// git's forcing options as git reads them: the last of each wins, an
    /// abbreviation is the one option it begins, and `-f`, `--force`, `+`
    /// and `--mirror` override any lease.
    #[test]
    fn a_force_push_needs_a_lease_on_what_its_pusher_saw() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        let oid256 = "0123456789abcdef".repeat(4);
        for cmd in [
            "git push -f origin pr".to_owned(),
            "git push --force origin pr".to_owned(),
            "git push -uf origin pr".to_owned(),
            "git push origin +pr".to_owned(),
            "git push origin +HEAD:refs/heads/pr".to_owned(),
            "git push --force-with-lease --force-if-includes origin -- +pr".to_owned(),
            "git push --mirror".to_owned(),
            "git push --force-with-lease origin pr".to_owned(),
            "git push --force-with-l origin pr".to_owned(),
            "git push --force-with-lease=pr origin pr".to_owned(),
            "git push --force-with-lease=pr:origin/pr origin pr".to_owned(),
            "git push --force-with-lease=pr:$(git rev-parse origin/pr) origin pr".to_owned(),
            format!("git push --force-with-lease=pr:{} origin pr", &oid[..12]),
            "git push --force-with-lease --force-if-includes -f origin pr".to_owned(),
            "git push --force-with-lease --force-if-includes --no-force-if-includes origin pr"
                .to_owned(),
            "git push --no-force -f --force-with-lease --force-if-includes origin pr".to_owned(),
            format!("git push --force-with-lease=a:{oid} --force-with-lease=b origin a b"),
            "git push origin x && git push -f origin pr".to_owned(),
            "bash -c 'git push -f origin pr'".to_owned(),
            "git -C ../x push --force origin pr".to_owned(),
        ] {
            assert_eq!(
                unleased(&cmd, Path::new("/")).as_deref(),
                Some(super::UNLEASED),
                "{cmd}"
            );
        }
        for cmd in [
            "git push origin pr".to_owned(),
            "git push -u origin HEAD".to_owned(),
            "git push --force-with-lease --force-if-includes origin pr".to_owned(),
            "git push --force-if-includes --force-with-lease origin HEAD:pr".to_owned(),
            "git push --force-with-l --force-if-i origin pr".to_owned(),
            "git push --force-with-lease=pr --force-if-includes origin pr".to_owned(),
            format!("git push --force-with-lease=pr:{oid} origin pr"),
            format!("git push --force-with-lease=pr:{oid256} origin pr"),
            "git push --force-with-lease=pr: origin pr".to_owned(),
            "git push -f --no-force --force-with-lease --force-if-includes origin pr".to_owned(),
            "git push --force-with-lease --no-force-with-lease origin pr".to_owned(),
            "git push --force-if-includes origin pr".to_owned(),
            // A delete rewrites nothing onto a stale branch.
            "git push -d origin pr".to_owned(),
            "git push origin :pr".to_owned(),
            "git push -o ci.skip origin pr".to_owned(),
            "echo git push -o -f".to_owned(),
        ] {
            assert_eq!(unleased(&cmd, Path::new("/")), None, "{cmd}");
        }
    }
}
