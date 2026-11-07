//! Codex jobs (proj's codex engine). Their git hooks come in through the
//! GIT_CONFIG_* environment proj gives Codex: commit-msg marks each commit
//! `Engine: codex`, which sends its PR to a Claude judge rather than land
//! on Codex's own review, and pre-push refuses a default branch's
//! rewrite. Codex runs this guard as its managed hook, outside the job's
//! reach, so here it refuses what would make a commit those hooks never
//! see or push past them: turning them off (`--no-verify`, `commit -n`,
//! `core.hooksPath`), changing or clearing the environment that carries
//! them, git's commands that write commits without commit-msg (revert
//! does, and cherry-pick and rebase do by case), a git alias, which could
//! be any of those, and writes to a repo's commits, refs or contents
//! through GitHub's API, which GitHub makes itself. A default branch's
//! force-push or delete is push.rs's, for both engines, whatever the
//! hooks.
//!
//! Each command is read on its own, as the shell splits the line
//! (walk.rs), so what one command is never speaks for another: a read
//! beside a write leaves the write refused. A command the guard couldn't
//! read to its end is refused too (push.rs's question).

use std::sync::LazyLock;

use regex::{Regex, RegexBuilder};

use crate::push::runs_git;
use crate::walk;
use crate::{any_line, pattern, shell};

/// Why a Codex job's command is refused.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Refusal {
    HooksOff,
    Api,
    Unmarked,
    Alias,
}

impl Refusal {
    fn reason(self) -> &'static str {
        match self {
            Refusal::HooksOff => {
                "Turns off this Codex job's git hooks, which mark its commits for a Claude judge and keep its pushes off the default branch's history; run it without that."
            }
            Refusal::Api => {
                "Writes commits or refs through GitHub (its API, or a merge or sync it makes), where this Codex job's git hooks never see them; commit and push with git instead, and leave merging to the steward."
            }
            Refusal::Unmarked => {
                "Writes commits this Codex job's commit-msg hook may never mark (Engine: codex), or pushes past its pre-push; commit, merge or push with plain git commit, git merge and git push instead."
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
        if w.starts_with('-') || w.contains(['$', shell::UNKNOWN]) {
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
static GIT_DATA: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"/git/(commits|trees|blobs|refs|tags)|/contents(/|$)|/merges$|/merge-upstream$|/pulls/[^/]+/(merge|update-branch)$|/branches/[^/]+/rename$",
    )
});

/// GitHub's GraphQL mutations that write commits or refs, or merge.
static MUTATION: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"createCommitOnBranch|createRef|updateRefs?([^a-zA-Z]|$)|deleteRef|mergeBranch|mergePullRequest|updatePullRequestBranch|revertPullRequest|enablePullRequestAutoMerge|enqueuePullRequest",
    )
});

/// An endpoint, as the API matches it: no query, no trailing slash.
fn endpoint(url: &str) -> &str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.trim_end_matches('/')
}

/// Whether a REST request at `url` by `method` writes commits or refs.
fn rest_writes(method: &str, url: &str) -> bool {
    !method.eq_ignore_ascii_case("GET")
        && !method.eq_ignore_ascii_case("HEAD")
        && GIT_DATA.is_match(endpoint(url))
}

/// What `gh` with these words does that GitHub writes commits or refs for:
/// `gh api`'s REST writes and GraphQL mutations, and `gh pr merge`,
/// `update-branch` and `revert`, and `gh repo sync`. Its own options may
/// come before its command, `-R` with a value.
fn gh<'a>(mut words: impl Iterator<Item = &'a String>) -> bool {
    let mut command = Vec::new();
    while command.len() < 2 {
        let Some(w) = words.next() else { break };
        match w.as_str() {
            "-R" | "--repo" => {
                words.next();
            }
            o if o.starts_with('-') => {}
            o => command.push(o),
        }
        if command.first().is_some_and(|c| *c == "api") {
            return gh_api(words);
        }
    }
    matches!(
        command.as_slice(),
        ["pr", "merge" | "update-branch" | "revert"] | ["repo", "sync"]
    )
}

/// Whether `gh api`'s words write commits or refs: a REST write (an
/// explicit method other than GET, or fields or input, which make it a
/// POST) at git data, or a GraphQL mutation that writes them, or a query
/// from a file or input, which the guard can't read.
fn gh_api<'a>(mut words: impl Iterator<Item = &'a String>) -> bool {
    let mut method = None;
    let mut body = false;
    let mut url = None;
    let mut values = Vec::new();
    let mut unread_query = false;
    while let Some(w) = words.next() {
        let w = w.as_str();
        let (name, joined) = match w.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => match w.get(..2) {
                Some(n @ ("-X" | "-f" | "-F" | "-H" | "-q" | "-t" | "-p")) if w.len() > 2 => {
                    (n, Some(&w[2..]))
                }
                _ => (w, None),
            },
        };
        let mut value = || joined.or_else(|| words.next().map(String::as_str));
        match name {
            "-X" | "--method" => method = value(),
            "-f" | "--raw-field" => {
                body = true;
                values.extend(value());
            }
            "-F" | "--field" => {
                body = true;
                if let Some(v) = value() {
                    unread_query |= v.starts_with("query=@");
                    values.push(v);
                }
            }
            "--input" => {
                body = true;
                unread_query = true;
                value();
            }
            "-H" | "--header" | "-q" | "--jq" | "-t" | "--template" | "--hostname" | "-p"
            | "--preview" | "--cache" => {
                value();
            }
            o if o.starts_with('-') => {}
            _ if url.is_none() => url = Some(w),
            _ => {}
        }
    }
    let Some(url) = url else { return false };
    if endpoint(url).ends_with("graphql") {
        return unread_query || values.iter().any(|v| MUTATION.is_match(v));
    }
    let method = method.unwrap_or(if body { "POST" } else { "GET" });
    rest_writes(method, url)
}

/// Whether `curl` with these words writes commits or refs through GitHub's
/// API, as `gh api` would.
fn curl<'a>(words: impl Iterator<Item = &'a String>) -> bool {
    let words: Vec<&str> = words.map(String::as_str).collect();
    let Some(url) = words
        .iter()
        .find(|w| w.contains("api.github.com") || w.contains("/api/v3/"))
    else {
        return false;
    };
    if endpoint(url).ends_with("graphql") {
        return words
            .iter()
            .any(|w| MUTATION.is_match(w) || (w.starts_with('@') && w.len() > 1));
    }
    let mut method = None;
    let mut body = false;
    let mut words = words.iter();
    while let Some(w) = words.next() {
        match *w {
            "-X" | "--request" => method = words.next().copied(),
            o if o.starts_with("--request=") => method = o.strip_prefix("--request="),
            o if o.starts_with("-X") => method = o.strip_prefix("-X"),
            "-T" | "--upload-file" => method = method.or(Some("PUT")),
            o if o.starts_with("--data") || o == "--json" || o.starts_with("--form") => {
                body = true;
            }
            o if o.starts_with("-d") || o.starts_with("-F") => body = true,
            _ => {}
        }
    }
    let method = method.unwrap_or(if body { "POST" } else { "GET" });
    rest_writes(method, url)
}

/// The program a word names, whatever path it's run by.
fn program(word: &str) -> &str {
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
                "curl" => curl(rest.iter(budget)).then_some(Refusal::Api),
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
