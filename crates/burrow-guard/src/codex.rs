//! Codex jobs (proj's codex engine). Their git hooks come in through the
//! GIT_CONFIG_* environment proj gives Codex: commit-msg marks each commit
//! `Engine: codex`, which sends its PR to a Claude judge rather than land
//! on Codex's own review, and pre-push refuses a default branch's
//! rewrite. Codex runs this guard as its managed hook, outside the job's
//! reach, so here it refuses what would make a commit those hooks never
//! see or push past them: turning them off (`--no-verify`, `commit -n`,
//! `core.hooksPath`), changing or clearing the environment that carries
//! them, git's commands that write commits without commit-msg (revert
//! does, and cherry-pick and rebase do by case), and writes to a repo's
//! commits, refs or contents through GitHub's API. A default
//! branch's force-push or delete is push.rs's, for both engines, whatever
//! the hooks.

use std::sync::LazyLock;

use regex::{Regex, RegexBuilder};

use crate::{any_line, pattern};

/// Where a command word starts: bare, after a path, or inside quotes, a
/// subshell or a substitution (undo.rs's start).
const START: &str = r#"(^|[;&|[:space:](/'"`])"#;

/// git, with any `-C DIR` or `-c KEY=VALUE` before its subcommand.
const GIT: &str = r"git([[:space:]]+-[Cc][[:space:]]+[^[:space:]]+)*[[:space:]]+";

/// Whatever turns the job's hooks off for one command.
static SKIPS_HOOKS: LazyLock<Regex> = LazyLock::new(|| {
    pattern(&format!(
        r"--no-verify|{START}{GIT}commit([[:space:]][^;&|]*)?[[:space:]]-[a-zA-Z]*n[a-zA-Z]*([[:space:]]|$)"
    ))
});

/// The setting the hooks are, in any spelling git takes.
static HOOKS_PATH: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"core\.hookspath")
        .case_insensitive(true)
        .unicode(false)
        .build()
        .expect("the guard's patterns are valid")
});

/// What changes or clears the environment the hooks come in: git's own
/// config variables, or a command run with a cleared environment.
static CLEARS_HOOKS: LazyLock<Regex> = LazyLock::new(|| {
    pattern(&format!(
        r"GIT_CONFIG|{START}env([[:space:]]+-[^[:space:]]*)*[[:space:]]+(-[a-zA-Z0-9]*[iu][a-zA-Z0-9_]*|--ignore-environment|--unset(=[^[:space:]]*)?|-)([[:space:]]|$)|{START}exec[[:space:]]+-[a-z]*c"
    ))
});

/// git's commands that write commits commit-msg may never see, and
/// send-pack, which pushes with no pre-push.
static UNMARKED: LazyLock<Regex> = LazyLock::new(|| {
    pattern(&format!(
        r#"{START}{GIT}((commit-tree|cherry-pick|revert|rebase|am|send-pack)([[:space:]'"`;&|)]|$)|pull([[:space:]][^;&|]*)?[[:space:]](--rebase|-r)([[:space:]='"`;&|)]|$))"#
    ))
});

/// `gh api` on a repo's git data, contents or merges: GitHub writes those
/// commits and refs itself, with no hook of the job's to see them.
static GH_API: LazyLock<Regex> =
    LazyLock::new(|| pattern(&format!(r"{START}gh[[:space:]]+api[[:space:]]")));
static GIT_DATA: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"/git/(commits|trees|blobs|refs|tags)|/contents(/|[[:space:]'\x22]|$)|/merges([[:space:]'\x22]|$)|/merge-upstream",
    )
});
/// A request that writes: a write method, or fields, which make it a POST.
static WRITES: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"(-X|--method)[[:space:]=]*(POST|PUT|PATCH|DELETE)|(^|[[:space:]])(-[fF]|--field|--raw-field|--input)([[:space:]=]|$)",
    )
});
static READS: LazyLock<Regex> = LazyLock::new(|| pattern(r"(-X|--method)[[:space:]=]*GET"));

fn writes_through_api(cmd: &str) -> bool {
    cmd.split('\n').any(|line| {
        GH_API.is_match(line)
            && GIT_DATA.is_match(line)
            && WRITES.is_match(line)
            && !READS.is_match(line)
    })
}

pub fn refusals(cmd: &str) -> Vec<String> {
    let mut refusals = Vec::new();
    if any_line(&SKIPS_HOOKS, cmd) || HOOKS_PATH.is_match(cmd) || any_line(&CLEARS_HOOKS, cmd) {
        refusals.push(
            "Turns off this Codex job's git hooks, which mark its commits for a Claude judge and keep its pushes off the default branch's history; run it without that.".to_owned(),
        );
    }
    if writes_through_api(cmd) {
        refusals.push(
            "Writes commits or refs through GitHub's API, where this Codex job's git hooks never see them; commit and push with git instead.".to_owned(),
        );
    }
    if any_line(&UNMARKED, cmd) {
        refusals.push(
            "Writes commits this Codex job's commit-msg hook may never mark (Engine: codex), or pushes past its pre-push; commit, merge or push with plain git commit, git merge and git push instead.".to_owned(),
        );
    }
    refusals
}
