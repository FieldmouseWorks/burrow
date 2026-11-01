//! R2: a force-push or delete of a default branch rewrites history that
//! every other clone depends on. What a push targets is read from its
//! refspecs, or, with none, from the branch checked out where it runs and
//! that branch's upstream (`git push --force` on main).

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::shell;

/// Branches other clones build on. HEAD and @ are whatever is checked out,
/// which may be one of them.
const SHARED: [&str; 5] = ["main", "master", "trunk", "HEAD", "@"];

/// The question for each push in `cmd` that can rewrite or delete a shared
/// branch; `cwd` is where the command runs. A command nested deeper than
/// the guard reads asks, since a push in it would go unseen, and so does one
/// too long to read within [`WORDS`] and [`PLACES`].
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
    let budget = Budget(Cell::new(WORDS));
    pushes(shell::commands(cmd)?, cwd, 0, None, &budget, &mut found)?;
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

/// Why a command wasn't read to its end.
#[derive(Debug)]
enum Unread {
    Deep,
    Long,
}

impl Unread {
    fn reason(&self) -> &'static str {
        match self {
            Unread::Deep => "it nests too deep to read",
            Unread::Long => "it's too long to read in time",
        }
    }
}

impl From<shell::TooDeep> for Unread {
    fn from(_: shell::TooDeep) -> Unread {
        Unread::Deep
    }
}

/// How many words a command's pushes may read, each from its own place to
/// the end of its command, so repeated pushes cost the square of their count.
/// Past this the reading takes too long and holds too much, and it asks: far
/// more than a real command's few pushes read.
const WORDS: usize = 2_000_000;

/// How many places a command's pushes with no refspec may ask git about.
/// Each is two processes.
const PLACES: usize = 32;

/// The words left to read.
struct Budget(Cell<usize>);

impl Budget {
    /// Takes `n`, or as many as are left: then the reading has to stop.
    fn spend(&self, n: usize) -> Result<(), Unread> {
        let left = self.0.get();
        self.0.set(left.saturating_sub(n));
        if n > left { Err(Unread::Long) } else { Ok(()) }
    }

    fn spent(&self) -> bool {
        self.0.get() == 0
    }
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
/// it ends. A word's walk is linked, not copied, and every step of it comes
/// off `budget`: a command that reads too long for the hook's timeout, which
/// would let it run unchecked, asks instead.
fn pushes(
    commands: Vec<Vec<String>>,
    cwd: &Path,
    depth: usize,
    trailing: Option<&Rest<'_>>,
    budget: &Budget,
    found: &mut Vec<(PathBuf, Push)>,
) -> Result<(), Unread> {
    if depth > NESTED {
        return Err(Unread::Deep);
    }
    let last = commands.len().saturating_sub(1);
    for (n, words) in commands.into_iter().enumerate() {
        let words = with_parameter_texts(words)?;
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
            let inner = reading(word)?;
            if !reads_as_itself(&inner, word) {
                if !script_read && shell.is_some_and(|at| at + 1 < i) && is_c(&words[i - 1]) {
                    script_read = true;
                    let inner = given_params(inner, word, rest, budget)?;
                    pushes(inner, cwd, depth + 1, None, budget, found)?;
                } else {
                    pushes(inner, cwd, depth + 1, Some(&rest), budget, found)?;
                }
            }
            if runs_git(word, rest.iter(budget).next())
                && let Some((at, mut push)) = after_git(cwd, rest.iter(budget))
            {
                push.more = input_as_words.is_some_and(|at| at < i);
                found.push((at, push));
            }
            if budget.spent() {
                return Err(Unread::Long);
            }
        }
    }
    Ok(())
}

/// `word` read as a command line: an assignment's value, or all of it.
fn reading(word: &str) -> Result<Vec<Vec<String>>, shell::TooDeep> {
    shell::commands(assigned(word).unwrap_or(word))
}

fn reads_as_itself(reading: &[Vec<String>], word: &String) -> bool {
    matches!(reading, [only] if only.as_slice() == std::slice::from_ref(word))
}

/// A command's words, each `${…}`'s text read in ahead of the word holding
/// it: an unset `x` makes `${x:-git push -f origin main}` those words, and
/// `eval` runs them even with a `;` among them, so its commands' words all
/// join. The word stays, as `x` may be set to anything. A word that reads as
/// more than itself has this done where it's read.
fn with_parameter_texts(words: Vec<String>) -> Result<Vec<String>, shell::TooDeep> {
    if !words.iter().any(|w| w.contains("${")) {
        return Ok(words);
    }
    let mut all = Vec::with_capacity(words.len());
    for word in words {
        if reads_as_itself(&reading(&word)?, &word) {
            for text in shell::parameter_texts(&word) {
                for command in shell::commands(text)? {
                    all.extend(with_parameter_texts(command)?);
                }
            }
        }
        all.push(word);
    }
    Ok(all)
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
    fn iter<'b>(self, budget: &'b Budget) -> Words<'a, 'b> {
        Words {
            here: self.here.iter(),
            then: self.then,
            budget,
        }
    }
}

/// A [`Rest`]'s words, in order, for as long as the budget lasts.
struct Words<'a, 'b> {
    here: std::slice::Iter<'a, String>,
    then: Option<&'a Rest<'a>>,
    budget: &'b Budget,
}

impl<'a> Iterator for Words<'a, '_> {
    type Item = &'a String;

    fn next(&mut self) -> Option<&'a String> {
        self.budget.spend(1).ok()?;
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
/// A word that still expands after that may hold any of them, by a spelling
/// not worked out here (`"${@:1}"`, `$a` after `a=("$@")`), so they follow it.
fn given_params(
    commands: Vec<Vec<String>>,
    script: &str,
    args: Rest<'_>,
    budget: &Budget,
) -> Result<Vec<Vec<String>>, Unread> {
    if !script.contains(['$', shell::UNKNOWN]) {
        return Ok(commands);
    }
    let numbered: Vec<&String> = args.iter(budget).take(10).collect();
    let after_zero: Vec<&String> = args.iter(budget).skip(1).collect();
    let mut given_commands = Vec::with_capacity(commands.len());
    for words in commands {
        let mut given = Vec::with_capacity(words.len());
        for word in words {
            let mut word = word;
            if matches!(word.as_str(), "$@" | "$*" | "${@}" | "${*}") {
                budget.spend(after_zero.len())?;
                given.extend(after_zero.iter().map(|w| (*w).clone()));
                continue;
            }
            for (n, arg) in numbered.iter().enumerate() {
                word = word
                    .replace(&format!("${n}"), arg)
                    .replace(&format!("${{{n}}}"), arg);
            }
            budget.spend(1 + word.len() / 8)?;
            let expands = word.contains(['$', shell::UNKNOWN]);
            given.push(word);
            if expands {
                budget.spend(after_zero.len())?;
                given.extend(after_zero.iter().map(|w| (*w).clone()));
            }
        }
        given_commands.push(given);
    }
    Ok(given_commands)
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
