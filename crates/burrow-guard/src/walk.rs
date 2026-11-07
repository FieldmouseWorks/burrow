//! Every command a command line runs, as far as the guard reads it: each
//! command word, with the words after it, through quotes, substitutions,
//! `sh -c` and `eval` scripts, assignments' values and `${…}` texts
//! (shell.rs). push.rs finds a line's pushes in it, and codex.rs what would
//! hide a Codex job's commits from its hooks.

use std::cell::Cell;

use crate::shell;

/// Why a command wasn't read to its end.
#[derive(Debug)]
pub enum Unread {
    Deep,
    Long,
}

impl Unread {
    pub fn reason(&self) -> &'static str {
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

/// How many words a command's walk may read, each command word's from its
/// own place to the end of its command, so repeated ones cost the square of
/// their count. Past this the reading takes too long and holds too much, and
/// it asks: far more than a real command reads.
const WORDS: usize = 2_000_000;

/// The words left to read.
pub struct Budget(Cell<usize>);

impl Budget {
    /// Takes `n`, or as many as are left: then the reading has to stop.
    pub fn spend(&self, n: usize) -> Result<(), Unread> {
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

/// Each word of `cmd` that may run something, with the words after it
/// (`visit`'s [`Rest`]) and whether more of them come from its input,
/// unseen (`xargs`). A word that reads as more than itself (a script for
/// `bash -c`, `ssh` or `eval`) is read the same way, since it may run, and
/// so is the value of `NAME=value`, since it may be run later (`eval
/// "$NAME"`). The words after one reach its last command, as `eval` joins
/// them (`trailing`), unless a shell's `-c` takes them as its parameters.
/// Each reading is shorter than the word, so it ends. A word's walk is
/// linked, not copied, and every step of it comes off the budget: a command
/// that reads too long for the hook's timeout, which would let it run
/// unchecked, isn't read to its end.
pub fn walk(cmd: &str, visit: &mut Visit<'_>) -> Result<(), Unread> {
    let budget = Budget(Cell::new(WORDS));
    read(shell::commands(cmd)?, 0, None, &budget, visit)
}

/// What [`walk`] calls for each word: the word, the words after it, the
/// budget their reading comes off, and whether more come from its input.
pub type Visit<'v> = dyn FnMut(&String, Rest<'_>, &Budget, bool) -> Result<(), Unread> + 'v;

fn read(
    commands: Vec<Vec<String>>,
    depth: usize,
    trailing: Option<&Rest<'_>>,
    budget: &Budget,
    visit: &mut Visit<'_>,
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
                    read(inner, depth + 1, None, budget, visit)?;
                } else {
                    read(inner, depth + 1, Some(&rest), budget, visit)?;
                }
            }
            visit(word, rest, budget, input_as_words.is_some_and(|at| at < i))?;
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

/// Whether `word` read again is just itself. Reading a word again marks the
/// globs it holds as unquoted ones (shell.rs's `bare`), whatever quoting it
/// had the first time, so the marks don't count. (A brace list it holds
/// expands into more words, which aren't just itself.)
fn reads_as_itself(reading: &[Vec<String>], word: &str) -> bool {
    let bare = |w: &str| w.replace(shell::UNKNOWN, "");
    matches!(reading, [only] if only.len() == 1 && bare(&only[0]) == bare(word))
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
pub struct Rest<'a> {
    here: &'a [String],
    then: Option<&'a Rest<'a>>,
}

impl<'a> Rest<'a> {
    pub fn iter<'b>(self, budget: &'b Budget) -> Words<'a, 'b> {
        Words {
            here: self.here.iter(),
            then: self.then,
            budget,
        }
    }
}

/// A [`Rest`]'s words, in order, for as long as the budget lasts.
pub struct Words<'a, 'b> {
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
