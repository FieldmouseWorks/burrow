//! The shells a command line's commands run in, and what joins each
//! command to the one before it (`;`, `&&`, `||`, `|`, `&`), as the lexer
//! reads them.

use super::{Command, Lexer, TooDeep};

/// Each command in `text`, as [`commands`](super::commands) reads them,
/// with the shell each runs in and how it's joined to the ones around it.
pub fn script(text: &str) -> Result<Script, TooDeep> {
    let mut lexer = Lexer::new(text);
    lexer.list(0, false)?;
    Ok(Script {
        runs: lexer.runs,
        parents: lexer.parents,
        joins: lexer.joins,
    })
}

/// A command line's commands, and the shells they run in.
pub struct Script {
    pub runs: Vec<Run>,
    /// The shell each shell (a [`Run::scope`]) starts from, a copy of it as
    /// it is then: the line's own (0) is its own. A `( … )` subshell or a
    /// substitution is one, and so is any text read apart from the line
    /// (a comment, a here-document or here-string, a backquoted
    /// substitution), which may be a script for another shell.
    pub parents: Vec<usize>,
    /// How each shell is joined to the command before it in its parent:
    /// as the command that holds a substitution is, or a subshell itself.
    pub joins: Vec<Joined>,
}

/// One command of a [`Script`].
#[derive(Debug)]
pub struct Run {
    pub words: Vec<String>,
    /// The shell it runs in.
    pub scope: usize,
    /// How it follows the command before it in its shell.
    pub joined: Joined,
    /// Neither piped nor put in the background, either of which runs it in
    /// a subshell of its own.
    pub alone: bool,
    /// It ends at a `(`, as a function's name does (`f() …`).
    pub defines: bool,
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
    /// here is joined to the one before it.
    pub(super) fn child(&mut self) -> usize {
        let flow = self.flow();
        let (parent, joined) = (flow.scope, joined(flow.pending));
        self.parents.push(parent);
        self.joins.push(joined);
        self.parents.len() - 1
    }

    /// `cmd` ends at `sep`, in the shell open here. A newline or a
    /// subshell's `(` after `&&`, `||` or `|` goes on with what they join.
    pub(super) fn end(&mut self, cmd: &mut Command, sep: Sep) -> Result<(), TooDeep> {
        let (words, scripts) = cmd.end()?;
        let flow = self.flow();
        let scope = flow.scope;
        if words.is_empty() {
            flow.pending = match (flow.pending, sep) {
                (Some(p @ (Sep::And | Sep::Or | Sep::Pipe)), Sep::Seq | Sep::Opens) => Some(p),
                _ => Some(sep),
            };
        } else {
            let before = flow.pending.replace(sep);
            self.runs.push(Run {
                words,
                scope,
                joined: joined(before),
                alone: before != Some(Sep::Pipe) && !matches!(sep, Sep::Pipe | Sep::Background),
                defines: sep == Sep::Opens,
            });
        }
        for script in scripts {
            let scope = self.child();
            self.runs.push(Run {
                words: vec![script],
                scope,
                joined: Joined::First,
                alone: true,
                defines: false,
            });
        }
        Ok(())
    }

    /// A substitution's commands, after its `(`, to the `)` that ends it,
    /// in a shell of their own.
    pub(super) fn substitution(&mut self, depth: usize) -> Result<(), TooDeep> {
        let scope = self.child();
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
