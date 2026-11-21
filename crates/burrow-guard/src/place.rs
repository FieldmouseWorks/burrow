//! Where each command of a line runs: the hook's cwd, moved by each `cd`
//! or `pushd` the guard can follow, in the shell that runs it (a subshell's
//! or a substitution's ends with it). git's `-C` moves one command further.
//!
//! Whatever may move a shell in a way the guard can't follow makes its
//! place unread, and a push from an unread place is refused, as one from
//! `git -C "$DIR"` is (push/lease.rs). That's a `cd` or `pushd` to an
//! expansion, `-`, `~`, home or through `CDPATH`, with options, run behind
//! a keyword, an assignment, `eval`, `builtin` or `command`, inside `if`,
//! `case` or a group, piped or in the background, or after `&&` or `||`
//! once the commands it joins end; a `popd`; a command whose name is an
//! expansion that may be `cd`; and a reading `eval` runs that moves. A cd
//! to a place spelled out reads it again.
//!
//! A loop or a function runs its body at another time than it's read in,
//! so a line that holds one and any `cd` reads no place at all.
//!
//! What a sourced file, an alias or a function from outside the line does
//! isn't seen, any more than a push in it is.

use std::path::{Component, Path, PathBuf};

use crate::shell::{self, Joined, Run, Script};
use crate::walk::assigned;

/// Where a command runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub path: PathBuf,
    /// It may be anywhere: a `cd` the guard can't follow came before.
    pub unread: bool,
}

impl Place {
    pub fn at(path: &Path) -> Place {
        Place {
            path: path.to_path_buf(),
            unread: false,
        }
    }

    /// `git -C dir`, from here, as git joins it. One spelled out from the
    /// root is read whatever came before.
    pub fn join(&self, dir: &str) -> Place {
        Place {
            path: self.path.join(dir),
            unread: shell::unknown(dir) || (self.unread && Path::new(dir).is_relative()),
        }
    }

    /// `cd dir`, from here, as bash's logical `cd` goes: `..` takes off
    /// the name before it, whatever links there are.
    fn cd(&self, dir: &str) -> Place {
        let mut path = PathBuf::new();
        for part in self.path.join(dir).components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir
                    if matches!(path.components().next_back(), Some(Component::Normal(_))) =>
                {
                    path.pop();
                }
                Component::ParentDir if path.has_root() => {}
                part => path.push(part),
            }
        }
        if path.as_os_str().is_empty() {
            path.push(".");
        }
        Place {
            path,
            unread: self.unread && Path::new(dir).is_relative(),
        }
    }

    fn unread(&self) -> Place {
        Place {
            path: self.path.clone(),
            unread: true,
        }
    }
}

/// The shells of a [`Script`] as it's walked, each at its place.
pub struct Shells {
    shells: Vec<Option<Shell>>,
    parents: Vec<usize>,
    joins: Vec<Joined>,
    /// The line holds a loop or a function, and a `cd`.
    nowhere: bool,
    cdpath: bool,
}

struct Shell {
    place: Place,
    /// Keywords open (`if`, `case`, `{`, a loop): a `cd` inside runs only
    /// if they say so.
    open: usize,
    /// A `cd` behind `&&` moved it: once the commands that `&&` joins end
    /// (or `||` takes over), it may be either place.
    joined_cd: bool,
}

impl Shells {
    /// `script`'s shells, its own at `place`. `cdpath`: `CDPATH` may be
    /// set, which takes a relative `cd` anywhere.
    pub fn new(script: &Script, place: Place, cdpath: bool) -> Shells {
        let reads: Vec<Reading> = script
            .runs
            .iter()
            .map(|r| reading(&r.words, cdpath))
            .collect();
        let nowhere = reads.iter().any(|r| r.moves.is_some())
            && (reads.iter().any(|r| r.defers) || script.runs.iter().any(|r| r.defines));
        let mut shells: Vec<Option<Shell>> = script.parents.iter().map(|_| None).collect();
        shells[0] = Some(Shell {
            place,
            open: 0,
            joined_cd: false,
        });
        Shells {
            shells,
            parents: script.parents.clone(),
            joins: script.joins.clone(),
            nowhere,
            cdpath,
        }
    }

    /// Where `run` runs; its shell moves on past it.
    pub fn enter(&mut self, run: &Run) -> Place {
        let read = reading(&run.words, self.cdpath);
        let nowhere = self.nowhere;
        let shell = self.shell(run.scope);
        if run.joined != Joined::And && std::mem::take(&mut shell.joined_cd) {
            shell.place = shell.place.unread();
        }
        let here = if nowhere {
            shell.place.unread()
        } else {
            shell.place.clone()
        };
        shell.open = (shell.open + read.opens).saturating_sub(read.closes);
        match read.moves {
            None => {}
            Some(Move::To(dir)) if shell.open == 0 && run.alone && run.joined != Joined::Or => {
                shell.place = shell.place.cd(&dir);
                shell.joined_cd = run.joined == Joined::And;
            }
            Some(_) => shell.place = shell.place.unread(),
        }
        here
    }

    /// A reading `eval` runs in `scope`'s shell moved it, somewhere the
    /// guard didn't follow.
    pub fn moved(&mut self, scope: usize) {
        let shell = self.shell(scope);
        shell.place = shell.place.unread();
    }

    /// Where the script's own shell ends.
    pub fn end(mut self) -> Place {
        self.shell(0).place.clone()
    }

    /// `scope`'s shell, started from its parent's as it is now.
    fn shell(&mut self, scope: usize) -> &mut Shell {
        if self.shells[scope].is_none() {
            let joined = self.joins[scope];
            let parent = self.shell(self.parents[scope]);
            // A subshell, or the command holding a substitution, ends what
            // `&&` joined unless it's joined so itself.
            if joined != Joined::And && std::mem::take(&mut parent.joined_cd) {
                parent.place = parent.place.unread();
            }
            let place = parent.place.clone();
            self.shells[scope] = Some(Shell {
                place,
                open: 0,
                joined_cd: false,
            });
        }
        self.shells[scope].as_mut().expect("just started")
    }
}

/// What a command does to the place of the shell it runs in.
#[derive(Debug, Default)]
struct Reading {
    /// Keywords it opens and closes.
    opens: usize,
    closes: usize,
    /// It opens a loop, a function or a `{` group (a function's body),
    /// whose commands may run again, or later, than where they're read.
    defers: bool,
    /// It may move the shell, to where if the guard can follow it.
    moves: Option<Move>,
}

#[derive(Debug)]
enum Move {
    To(String),
    Unread,
}

/// What `words`, a command, does to its shell's place: the keywords at its
/// head, then the command they lead to.
fn reading(words: &[String], cdpath: bool) -> Reading {
    let mut read = Reading::default();
    let mut led = false;
    let mut words = words.iter().map(String::as_str);
    let name = loop {
        let Some(word) = words.next() else {
            return read;
        };
        match word {
            "if" | "case" => read.opens += 1,
            "while" | "until" | "for" | "select" | "function" | "{" => {
                read.opens += 1;
                read.defers = true;
            }
            "fi" | "done" | "esac" | "}" => read.closes += 1,
            "then" | "do" | "else" | "elif" | "!" | "time" | "coproc" | "-p" | "--" | "eval"
            | "builtin" | "command" | "exec" => {}
            w if assigned(w).is_some() => {}
            name => break name,
        }
        led = true;
    };
    let moves = match name {
        "cd" | "pushd" => !led,
        "popd" => false,
        name if may_be_cd(name) => false,
        _ => return read,
    };
    let mut args = words.peekable();
    args.next_if_eq(&"--");
    let args: Vec<&str> = args.collect();
    read.moves = Some(match args.as_slice() {
        [dir] if moves && followed(dir, cdpath) => Move::To((*dir).to_owned()),
        _ => Move::Unread,
    });
    read
}

/// Whether `cd dir` goes where `dir` says: spelled out, not an option,
/// `-`, `~` or empty, and not looked up in `CDPATH`.
fn followed(dir: &str, cdpath: bool) -> bool {
    let looked_up = cdpath
        && !dir.starts_with('/')
        && !matches!(
            Path::new(dir).components().next(),
            Some(Component::CurDir | Component::ParentDir)
        );
    !dir.is_empty() && !dir.starts_with(['-', '+', '~']) && !shell::unknown(dir) && !looked_up
}

/// Whether a command named `name` may be `cd`: an expansion is anything,
/// unless a `/` after the last of it makes it a file's path.
fn may_be_cd(name: &str) -> bool {
    if !shell::unknown(name) {
        return false;
    }
    let last = name.rfind(['$', '}', shell::UNKNOWN]);
    !matches!((name.rfind('/'), last), (Some(slash), Some(last)) if slash > last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cd_goes_as_bash_goes() {
        let at = Place::at(Path::new("/a/b"));
        assert_eq!(at.cd("c").path, Path::new("/a/b/c"));
        assert_eq!(at.cd("../c").path, Path::new("/a/c"));
        assert_eq!(at.cd("./c/..").path, Path::new("/a/b"));
        assert_eq!(at.cd("/x/../../y").path, Path::new("/y"));
        assert_eq!(Place::at(Path::new(".")).cd("..").path, Path::new(".."));
        let unread = at.unread();
        assert!(unread.cd("c").unread);
        assert!(!unread.cd("/c").unread);
        assert!(unread.join("c").unread);
        assert!(!unread.join("/c").unread);
        assert!(at.join("$D").unread);
    }

    #[test]
    fn a_name_may_be_cd_unless_its_a_path() {
        for name in ["$c", "${c}", "${X%/*}", "$X/$c", "c\u{FFFC}*"] {
            assert!(may_be_cd(name), "{name}");
        }
        for name in ["cd", "git", "$X/bin/tool", "${X}/x"] {
            assert!(!may_be_cd(name), "{name}");
        }
    }
}
