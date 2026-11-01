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
/// branch; `cwd` is where the command runs. A command nested deeper than
/// the guard reads asks, since a push in it would go unseen.
pub fn asks(cmd: &str, cwd: &Path, git: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let read = shell::commands(cmd).and_then(|commands| pushes(commands, cwd, 0, None, &mut found));
    if read.is_err() {
        return vec![
            "The guard couldn't check this command (it nests too deep to read), so it asks."
                .to_owned(),
        ];
    }
    found
        .into_iter()
        .filter_map(|(at, push)| push.ask(&at, git))
        .collect()
}

/// How many times a word may hold a command line that holds another
/// before the reading gives up.
const NESTED: usize = 16;

/// Each `git … push …` in `commands`, with where it runs. A word that
/// reads as more than itself (a script for `bash -c`, `ssh` or `eval`) is
/// read the same way, since it may run, and so is the value of `NAME=value`,
/// since it may be run later (`eval "$NAME"`). The words after one reach its
/// last command, as `eval` joins them (`trailing`), unless a shell's `-c`
/// takes them as its parameters. Each reading is shorter than the word, so
/// it ends. Every word is visited once a reading, so a command's reading
/// takes time in step with its length: one cut off by the hook's timeout
/// would go unchecked.
fn pushes(
    commands: Vec<Vec<String>>,
    cwd: &Path,
    depth: usize,
    trailing: Option<&Rest<'_>>,
    found: &mut Vec<(PathBuf, Push)>,
) -> Result<(), shell::TooDeep> {
    if depth > NESTED {
        return Err(shell::TooDeep);
    }
    let last = commands.len().saturating_sub(1);
    for (n, words) in commands.iter().enumerate() {
        let trailing = if n == last { trailing } else { None };
        let shell = words.iter().position(|w| is_shell(w));
        let input_as_words = words.iter().position(|w| takes_input_as_words(w));
        // A shell takes the first word after its `-c` as the script, and
        // the rest as that script's parameters.
        let mut script_read = false;
        for (i, word) in words.iter().enumerate() {
            let rest = Rest {
                here: &words[i + 1..],
                then: trailing,
            };
            let inner = shell::commands(assigned(word).unwrap_or(word))?;
            if !matches!(inner.as_slice(), [only] if only.as_slice() == std::slice::from_ref(word))
            {
                if !script_read && shell.is_some_and(|at| at + 1 < i) && is_c(&words[i - 1]) {
                    script_read = true;
                    pushes(given_params(inner, word, rest), cwd, depth + 1, None, found)?;
                } else {
                    pushes(inner, cwd, depth + 1, Some(&rest), found)?;
                }
            }
            if runs_git(word, rest.iter().next())
                && let Some((at, mut push)) = after_git(cwd, rest.iter())
            {
                push.more = input_as_words.is_some_and(|at| at < i);
                found.push((at, push));
            }
        }
    }
    Ok(())
}

/// The words after one: the rest of its command, then, when that command
/// was read from a word, the words after that word. Linked rather than
/// joined, so no word is copied for each word before it.
#[derive(Clone, Copy)]
struct Rest<'a> {
    here: &'a [String],
    then: Option<&'a Rest<'a>>,
}

impl<'a> Rest<'a> {
    fn iter(self) -> Words<'a> {
        Words {
            here: self.here.iter(),
            then: self.then,
        }
    }
}

/// A [`Rest`]'s words, in order.
struct Words<'a> {
    here: std::slice::Iter<'a, String>,
    then: Option<&'a Rest<'a>>,
}

impl<'a> Iterator for Words<'a> {
    type Item = &'a String;

    fn next(&mut self) -> Option<&'a String> {
        loop {
            if let Some(word) = self.here.next() {
                return Some(word);
            }
            let next = self.then?;
            self.here = next.here.iter();
            self.then = next.then;
        }
    }
}

/// The value of a `NAME=value` word.
fn assigned(word: &str) -> Option<&str> {
    let (name, value) = word.split_once('=')?;
    let name = name.strip_suffix('+').unwrap_or(name);
    let is_name = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    is_name.then_some(value)
}

/// Whether `word` can run git: it ends so, whatever's joined on before it
/// (a path, `--command=git`, `!git` in a git alias, `arr[0]=git`, `${x}git`),
/// or an expansion that ends so (`${git}`) or is followed by `push`, as
/// whatever it holds may be git (`$GIT push`).
fn runs_git(word: &str, next: Option<&String>) -> bool {
    if word.ends_with("git") {
        return true;
    }
    (word.contains('$') || word.contains(shell::UNKNOWN))
        && (word.trim_end_matches('}').ends_with("git") || next.is_some_and(|n| n == "push"))
}

/// Whether `word` names a shell, which runs the script its `-c` gives it.
fn is_shell(word: &str) -> bool {
    let name = word.rsplit('/').next().unwrap_or(word);
    matches!(name, "sh" | "bash" | "dash" | "zsh" | "ksh")
}

/// Whether `flag` is a shell's `-c`, alone or with other short options.
fn is_c(flag: &str) -> bool {
    flag.starts_with('-') && !flag.starts_with("--") && flag.ends_with('c')
}

/// A `-c` script's commands, with the words after it (`args`) for `$0`,
/// `$1` and so on, and the ones after `$0` for `$@` and `$*`. A script
/// that names no parameter takes none, so its words are read as they are.
fn given_params(commands: Vec<Vec<String>>, script: &str, args: Rest<'_>) -> Vec<Vec<String>> {
    if !script.contains('$') {
        return commands;
    }
    let numbered: Vec<&String> = args.iter().take(10).collect();
    let given = |word: String| -> Vec<String> {
        if matches!(word.as_str(), "$@" | "$*" | "${@}" | "${*}") {
            return args.iter().skip(1).cloned().collect();
        }
        let mut word = word;
        for (n, arg) in numbered.iter().enumerate() {
            word = word
                .replace(&format!("${n}"), arg)
                .replace(&format!("${{{n}}}"), arg);
        }
        vec![word]
    };
    commands
        .into_iter()
        .map(|words| words.into_iter().flat_map(given).collect())
        .collect()
}

/// A program that turns its input into the words of what it runs.
fn takes_input_as_words(word: &str) -> bool {
    matches!(word.rsplit('/').next(), Some("xargs" | "parallel"))
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
        if self.more {
            return Some(
                "Force-push or delete of whatever its input names (R2: rewrites history other clones depend on)."
                    .to_owned(),
            );
        }
        let shared = if self.positional.len() > 1 {
            self.positional[1..]
                .iter()
                .find_map(|refspec| shared_target(refspec))?
        } else {
            let branch = run(git, at, &["symbolic-ref", "--short", "-q", "HEAD"]);
            let upstream = run(
                git,
                at,
                &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
            )
            .map(|u| u.split_once('/').map_or(u.clone(), |(_, b)| b.to_owned()));
            branch
                .into_iter()
                .chain(upstream)
                .find_map(|t| shared_target(&t))?
        };
        Some(format!(
            "Force-push or delete of '{shared}' (R2: rewrites history other clones depend on)."
        ))
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
