//! Codex jobs (proj's codex engine). Their git hooks come in through the
//! GIT_CONFIG_* environment proj gives Codex: commit-msg marks each commit
//! `Engine: codex`, which sends its PR to a Claude judge rather than land
//! on Codex's own review (and refuses a message whose cleanup would lose
//! the mark), and pre-push refuses a default branch's rewrite and any
//! commit new to the remote without the mark, however it was made. Codex
//! runs this guard as its managed hook, outside the job's
//! reach, so here it refuses what would make a commit those hooks never
//! see or push past them: turning them off (`--no-verify`, `commit -n`,
//! `core.hooksPath`), changing or clearing the environment that carries
//! them, git's commands that write commits without commit-msg (revert
//! does, and cherry-pick and rebase do by case), a git alias, which could
//! be any of those, and writes to a repo's commits, refs or contents
//! through GitHub's API, which GitHub makes itself (curl's every request
//! read on its own: curl.rs). A default branch's
//! force-push or delete is push.rs's, for both engines, whatever the
//! hooks.
//!
//! Each command is read on its own, as the shell splits the line
//! (walk.rs), so what one command is never speaks for another: a read
//! beside a write leaves the write refused. A command the guard couldn't
//! read to its end is refused too (push.rs's question).

use std::sync::LazyLock;

use regex::{Regex, RegexBuilder};

use crate::curl::{self, Curl};
use crate::push::runs_git;
use crate::{any_line, gh, pattern, walk};

/// Why a Codex job's command is refused.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Refusal {
    HooksOff,
    Api,
    Unmarked,
    Alias,
    CurlConfig,
}

impl Refusal {
    fn reason(self) -> &'static str {
        match self {
            Refusal::HooksOff => {
                "Turns off this Codex job's git hooks, which mark its commits for a Claude judge and keep its pushes off the default branch's history; run it without that."
            }
            Refusal::Api => {
                "Writes commits or refs through GitHub (its API, or a merge or sync it makes), or calls its API where the guard cannot read the endpoint, method or query (an expansion, a file), where this Codex job's git hooks never see them; commit and push with git instead, spell the call out, and leave merging to the steward."
            }
            Refusal::Unmarked => {
                "Writes commits this Codex job's commit-msg hook may never mark (Engine: codex), or pushes past its pre-push; commit, merge or push with plain git commit, git merge and git push instead."
            }
            Refusal::CurlConfig => {
                "Runs curl with options from a file (--config, or a curlrc it would read), which may write commits or refs through GitHub's API where this Codex job's git hooks never see them; run it with -q first and its options on the line."
            }
            Refusal::Alias => {
                "Defines a git alias, which could run what this Codex job's hooks never see; run the git command itself instead."
            }
        }
    }
}

/// The setting the hooks are, in any spelling git takes.
static HOOKS_PATH: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"core\.hookspath")
        .case_insensitive(true)
        .unicode(false)
        .build()
        .expect("the guard's patterns are valid")
});

/// Where a command word starts: bare, after a path, or inside quotes, a
/// subshell or a substitution (undo.rs's start).
const START: &str = r#"(^|[;&|[:space:](/'"`])"#;

/// What turns the hooks off anywhere on a line, or changes or clears the
/// environment they come in: git's own config variables, or a command run
/// with a cleared environment.
static CLEARS_HOOKS: LazyLock<Regex> = LazyLock::new(|| {
    pattern(&format!(
        r"--no-verify|GIT_CONFIG|{START}env([[:space:]]+-[^[:space:]]*)*[[:space:]]+(-[a-zA-Z0-9]*[iu][a-zA-Z0-9_]*|--ignore-environment|--unset(=[^[:space:]]*)?|-)([[:space:]]|$)|{START}exec[[:space:]]+-[a-z]*c"
    ))
});

/// git's commands that write commits commit-msg may never see (revert,
/// cherry-pick, rebase, am, and the plumbing and rewriters that make
/// commits of their own), and send-pack, which pushes with no pre-push.
const UNMARKED: [&str; 11] = [
    "commit-tree",
    "cherry-pick",
    "revert",
    "rebase",
    "am",
    "send-pack",
    "fast-import",
    "filter-branch",
    "filter-repo",
    "replace",
    "subtree",
];

/// git's own options whose value is the next word.
const GIT_VALUE: [&str; 8] = [
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--config-env",
    "--attr-source",
    "--super-prefix",
];

/// What each git command in `words` is refused for, if anything: what
/// follows `git`, its own options first, as git reads them.
fn git<'a>(mut words: impl Iterator<Item = &'a String>) -> Option<Refusal> {
    let mut pull_rebases = false;
    let sub = loop {
        let w = words.next()?;
        let value = match w.as_str() {
            "-c" | "--config-env" => words.next().map(String::as_str),
            o if GIT_VALUE.contains(&o) => {
                words.next();
                continue;
            }
            o => o.strip_prefix("--config-env="),
        };
        if let Some(setting) = value {
            let key = setting.to_ascii_lowercase();
            if key.starts_with("alias.") {
                return Some(Refusal::Alias);
            }
            if key.starts_with("pull.rebase") || key.starts_with("branch.") {
                pull_rebases |= !key.ends_with("=false") && key.contains("rebase");
            }
            continue;
        }
        // An option, or an expansion, which may be one or nothing.
        if w.starts_with('-') || gh::expands(w) {
            continue;
        }
        break w.as_str();
    };
    let rest: Vec<&String> = words.collect();
    // `--no-verify` as git takes it: whole, or any prefix it alone begins.
    if rest
        .iter()
        .any(|w| w.len() > "--no-ver".len() && "--no-verify".starts_with(w.as_str()))
    {
        return Some(Refusal::HooksOff);
    }
    match sub {
        s if UNMARKED.contains(&s) => Some(Refusal::Unmarked),
        "commit" if commit_skips_hooks(&rest) => Some(Refusal::HooksOff),
        "pull" if pull_rebases || pull_rebase(&rest) => Some(Refusal::Unmarked),
        "config" if sets_alias(&rest) => Some(Refusal::Alias),
        _ => None,
    }
}

/// Whether `git commit`'s words turn its hooks off (`-n`, alone or among
/// other short options), read as git reads them: an option's value isn't
/// an option, and `--` ends them.
fn commit_skips_hooks(words: &[&String]) -> bool {
    /// Long options whose value is the next word, unless given with `=`.
    const VALUE: [&str; 11] = [
        "--message",
        "--file",
        "--reuse-message",
        "--reedit-message",
        "--template",
        "--author",
        "--date",
        "--cleanup",
        "--fixup",
        "--squash",
        "--trailer",
    ];
    let mut words = words.iter();
    while let Some(w) = words.next() {
        if *w == "--" {
            return false;
        }
        if w.starts_with("--") {
            if VALUE.contains(&w.as_str()) {
                words.next();
            }
            continue;
        }
        let Some(short) = w.strip_prefix('-') else {
            continue;
        };
        for (i, c) in short.char_indices() {
            match c {
                'n' => return true,
                // Its value is the rest of the word, or the next word.
                'm' | 'F' | 'c' | 'C' | 't' => {
                    if i + 1 == short.len() {
                        words.next();
                    }
                    break;
                }
                // An optional value, only joined on.
                'S' | 'u' => break,
                _ => {}
            }
        }
    }
    false
}

/// Whether `git pull`'s words make it rebase.
fn pull_rebase(words: &[&String]) -> bool {
    let mut rebases = false;
    let mut words = words.iter();
    while let Some(w) = words.next() {
        match w.as_str() {
            "--" => break,
            "--no-rebase" => rebases = false,
            "--rebase" => rebases = true,
            "--strategy" | "--strategy-option" => {
                words.next();
            }
            o if o.starts_with("--rebase=") => rebases = o != "--rebase=false",
            o if o.starts_with("--") => {}
            o if o.starts_with('-') => {
                for (i, c) in o.char_indices().skip(1) {
                    match c {
                        'r' => rebases = true,
                        's' | 'X' => {
                            if i + 1 == o.len() {
                                words.next();
                            }
                            break;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    rebases
}

/// Whether `git config`'s words set an alias rather than read one.
fn sets_alias(words: &[&String]) -> bool {
    const READS: [&str; 8] = [
        "get",
        "list",
        "--get",
        "--get-all",
        "--get-regexp",
        "--list",
        "-l",
        "--name-only",
    ];
    words
        .iter()
        .any(|w| w.to_ascii_lowercase().starts_with("alias."))
        && !words.iter().any(|w| READS.contains(&w.as_str()))
}

/// What GitHub's REST API writes commits or refs at: a repo's git data,
/// contents, merges, a pull request's merge or branch update, a branch's
/// rename.
pub(crate) static GIT_DATA: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"/git/(commits|trees|blobs|refs|tags)|/contents(/|$)|/merges$|/merge-upstream$|/pulls/[^/]+/(merge|update-branch)$|/branches/[^/]+/rename$",
    )
});

/// GitHub's GraphQL mutations that write commits or refs, or merge.
pub(crate) static MUTATION: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"createCommitOnBranch|createRef|updateRefs?([^a-zA-Z]|$)|deleteRef|mergeBranch|mergePullRequest|updatePullRequestBranch|revertPullRequest|enablePullRequestAutoMerge|enqueuePullRequest",
    )
});

/// An endpoint, as the API matches it: no query, no trailing slash.
pub(crate) fn endpoint(url: &str) -> &str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.trim_end_matches('/')
}

/// What `gh` with these words does that GitHub writes commits or refs for:
/// `gh api`'s REST writes and GraphQL mutations, and `gh pr merge`,
/// `update-branch` and `revert`, and `gh repo sync`.
fn gh<'a>(words: impl Iterator<Item = &'a String>) -> bool {
    let gh = gh::read(words);
    match gh.command.as_slice() {
        ["api"] => gh_api(&gh.rest),
        ["pr", "merge" | "update-branch" | "revert"] | ["repo", "sync"] => true,
        _ => false,
    }
}

/// Whether `gh api`'s words write commits or refs: a REST write at git
/// data, or a GraphQL mutation that writes them. Whatever the guard can't
/// read to its end is refused, since it could be a write: an expansion
/// anywhere in the call, an unquoted glob, a query from a file or input.
fn gh_api(words: &[&str]) -> bool {
    let api = gh::Api::read(words);
    if api.unreadable {
        return true;
    }
    api.urls.iter().any(|url| {
        if endpoint(url).ends_with("graphql") {
            api.unread_query() || api.fields.iter().any(|f| MUTATION.is_match(f.text))
        } else {
            api.writes() && GIT_DATA.is_match(endpoint(url))
        }
    })
}

/// Whether a write to GitHub's API (curl.rs's [`curl::Counts`]) writes
/// commits or refs: at git data, or a GraphQL mutation that does. Where
/// it goes unread, or a GraphQL body unread, it may.
fn writes_git(endpoint: Option<&str>, body: &curl::Body, unread_body: bool) -> bool {
    match endpoint {
        None => true,
        Some(path) if path.ends_with("graphql") => {
            unread_body || body.texts().any(|b| MUTATION.is_match(b))
        }
        Some(path) => GIT_DATA.is_match(path),
    }
}

/// The program a word names, whatever path it's run by.
pub(crate) fn program(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

pub fn refusals(cmd: &str) -> Vec<String> {
    let mut found = Vec::new();
    if HOOKS_PATH.is_match(cmd) || any_line(&CLEARS_HOOKS, cmd) {
        found.push(Refusal::HooksOff);
    }
    // A command too long or deep to read is push.rs's question, refused.
    let _ = walk::walk(cmd, &mut |word, rest, budget, _more| {
        let refusal = if runs_git(word, rest.iter(budget).next()) {
            git(rest.iter(budget))
        } else {
            match program(word) {
                "gh" => gh(rest.iter(budget)).then_some(Refusal::Api),
                "curl" => {
                    let words: Vec<&str> = rest.iter(budget).map(String::as_str).collect();
                    match curl::read(&words, &writes_git) {
                        Curl::Writes => Some(Refusal::Api),
                        Curl::Config => Some(Refusal::CurlConfig),
                        Curl::Fine => {
                            curl::reads_default_config(&words, cmd).then_some(Refusal::CurlConfig)
                        }
                    }
                }
                _ => None,
            }
        };
        found.extend(refusal);
        Ok(())
    });
    let mut refusals: Vec<String> = Vec::new();
    for refusal in found {
        let reason = refusal.reason().to_owned();
        if !refusals.contains(&reason) {
            refusals.push(reason);
        }
    }
    refusals
}
