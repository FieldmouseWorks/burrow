//! The shells a command line's commands run in, and what joins each
//! command to the one before it (`;`, `&&`, `||`, `|`, `&`), as the lexer
//! reads them.

use super::{Command, Lexer, TooDeep, assigned};

/// Each command in `text`, as [`commands`](super::commands) reads them,
/// with the shell each runs in and how it's joined to the ones around it.
pub fn script(text: &str) -> Result<Script, TooDeep> {
    let mut lexer = Lexer::new(text);
    lexer.list(0, false)?;
    Ok(Script {
        runs: lexer.runs,
        shells: lexer.shells,
    })
}

/// A command line's commands, and the shells they run in.
pub struct Script {
    pub runs: Vec<Run>,
    /// Each shell a [`Run::scope`] names: the line's own (0), and each
    /// `( … )` subshell, substitution and text read apart from the line.
    pub shells: Vec<Opened>,
}

/// A shell of a [`Script`], and where it starts.
#[derive(Debug, Clone, Copy)]
pub struct Opened {
    /// The shell it starts from, a copy of it as it is then: the line's
    /// own (0) is its own.
    pub parent: usize,
    /// How it's joined to the command before it in its parent: as the
    /// command that holds a substitution is, or a subshell itself.
    pub joined: Joined,
    /// It's a text of its own, read apart from the line (a comment, a
    /// here-document or here-string), which may be a script for another
    /// shell, rather than part of the line's script (a subshell, a
    /// substitution, a backquoted one).
    pub apart: bool,
    /// It's read line by line (a here-document), and a line left a
    /// subshell or a `case` open, which the next line's reading doesn't
    /// know it's in.
    pub torn: bool,
}

/// One command of a [`Script`].
#[derive(Debug)]
pub struct Run {
    pub words: Vec<String>,
    /// The shell it runs in.
    pub scope: usize,
    /// How it follows the command before it in its shell.
    pub joined: Joined,
    /// Neither piped nor put in the background (with the whole `&&`/`||`
    /// list it's in), either of which runs it in a subshell of its own.
    pub alone: bool,
    /// It's one word, not an assignment, ending at a `(`: a function's
    /// name (`f() …`).
    pub defines: bool,
    /// It has a redirect, which can fail and keep it from running.
    pub redirected: bool,
}

/// What joins a command to the one before it in its shell: nothing (`;`,
/// a newline, a pipe, or nothing before it), `&&` or `||`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Joined {
    First,
    And,
    Or,
}

/// What ends a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sep {
    /// `;`, a newline, a `)`, or the end of the text.
    Seq,
    And,
    Or,
    /// `|` or `|&`.
    Pipe,
    /// `&`.
    Background,
    /// `(`.
    Opens,
}

/// How a command is joined to the one before it, by what ended that one.
fn joined(before: Option<Sep>) -> Joined {
    match before {
        Some(Sep::And) => Joined::And,
        Some(Sep::Or) => Joined::Or,
        _ => Joined::First,
    }
}

/// A shell being read in: the one its commands run in, and what ended the
/// last of them, if nothing's begun since.
#[derive(Clone, Copy)]
pub(super) struct Flow {
    pub(super) scope: usize,
    pub(super) pending: Option<Sep>,
}

impl Lexer {
    pub(super) fn flow(&mut self) -> &mut Flow {
        self.flows
            .last_mut()
            .expect("the text's own shell stays open")
    }

    /// A new shell, started from the one open here, as the command read
    /// here is joined to the one before it; `apart`, [`Script::apart`].
    pub(super) fn child(&mut self, apart: bool) -> usize {
        let flow = self.flow();
        let (parent, joined) = (flow.scope, joined(flow.pending));
        self.shells.push(Opened {
            parent,
            joined,
            apart,
            torn: false,
        });
        self.shells.len() - 1
    }

    /// `cmd` ends at `sep`, in the shell open here. A newline or a
    /// subshell's `(` after `&&`, `||` or `|` goes on with what they join.
    pub(super) fn end(&mut self, cmd: &mut Command, sep: Sep) -> Result<(), TooDeep> {
        let (words, scripts, redirected) = cmd.end()?;
        let flow = self.flow();
        let scope = flow.scope;
        if words.is_empty() {
            flow.pending = match (flow.pending, sep) {
                (Some(p @ (Sep::And | Sep::Or | Sep::Pipe)), Sep::Seq | Sep::Opens) => Some(p),
                _ => Some(sep),
            };
        } else {
            let before = flow.pending.replace(sep);
            let defines =
                sep == Sep::Opens && matches!(words.as_slice(), [name] if assigned(name).is_none());
            self.runs.push(Run {
                words,
                scope,
                joined: joined(before),
                alone: before != Some(Sep::Pipe) && !matches!(sep, Sep::Pipe | Sep::Background),
                defines,
                redirected,
            });
        }
        if sep == Sep::Background {
            self.background(scope);
        }
        for script in scripts {
            let scope = self.child(true);
            self.runs.push(Run {
                words: vec![script],
                scope,
                joined: Joined::First,
                alone: true,
                defines: false,
                redirected: false,
            });
        }
        Ok(())
    }

    /// The `&&`/`||` list `&` ends, in `scope`'s shell, runs in the
    /// background: every command of it, not only its last.
    fn background(&mut self, scope: usize) {
        for run in self.runs.iter_mut().rev().filter(|r| r.scope == scope) {
            run.alone = false;
            if run.joined == Joined::First {
                break;
            }
        }
    }

    /// A substitution's commands, after its `(`, to the `)` that ends it,
    /// in a shell of their own.
    pub(super) fn substitution(&mut self, depth: usize) -> Result<(), TooDeep> {
        let scope = self.child(false);
        let open = self.flows.len();
        self.flows.push(Flow {
            scope,
            pending: None,
        });
        let read = self.list(depth + 1, true);
        self.flows.truncate(open);
        read
    }
}
