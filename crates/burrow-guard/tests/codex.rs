//! The guard as Codex's managed hook for proj's Codex jobs: it refuses
//! where Claude Code's asks, takes the same undo snapshot, and refuses what
//! would make a commit the job's hooks never mark or a push past them.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use burrow_guard::config::{Config, current_user};
use burrow_guard::{Guard, Hook, Input};
use common::zfs;

mod common;

fn bash(cmd: &str, cwd: &Path) -> Hook {
    Hook {
        tool_name: Some("Bash".to_owned()),
        tool_input: Some(Input {
            command: Some(cmd.to_owned()),
        }),
        cwd: Some(cwd.to_string_lossy().into_owned()),
    }
}

/// What the guard says about `cmd`, as Codex's (`codex`) or Claude's.
fn says(cmd: &str, codex: bool) -> Option<String> {
    common::stand_ins();
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        git: PathBuf::from("git"),
        zfs: PathBuf::from("/no-such-zfs"),
        snapshot_datasets: Vec::new(),
        ask_after_seconds: 8,
    };
    let guard = Guard {
        config: &config,
        codex,
        mark: dir.path().join("mark"),
    };
    guard.check(&bash(cmd, dir.path()), SystemTime::now())
}

#[test]
fn what_hides_a_commit_from_the_jobs_hooks_is_refused() {
    for cmd in [
        "git commit --no-verify -m change",
        "git commit -nm change",
        "git -C /work commit -a -n -m change",
        "git -c core.hooksPath=/dev/null commit -m change",
        "git -c core.HOOKSPATH=/tmp/none commit -m change",
        "git config core.hooksPath /dev/null",
        "GIT_CONFIG_COUNT=0 git commit -m change",
        "unset GIT_CONFIG_COUNT; git commit -m change",
        "env -u GIT_CONFIG_COUNT git commit -m change",
        "env --unset=GIT_CONFIG_COUNT git commit -m change",
        "env -i PATH=/bin git commit -m change",
        "env - git commit -m change",
        "exec -c git commit -m change",
        "git push --no-verify origin work",
        "git cherry-pick abc123",
        "git revert --no-edit HEAD",
        "git rebase origin/main",
        "git -C /work rebase -i HEAD~2",
        "git pull --rebase origin main",
        "git pull -r",
        "git commit-tree HEAD^{tree} -m change",
        "git am change.patch",
        "git send-pack origin work",
        "bash -c 'git revert HEAD'",
        "echo $(git cherry-pick abc)",
        "gh api -X PUT repos/o/r/contents/README.md -f message=change -f content=eA==",
        "gh api repos/o/r/git/commits -f message=change -f tree=abc",
        "gh api -X PATCH repos/o/r/git/refs/heads/work -F force=true -f sha=abc",
        "gh api --method DELETE repos/o/r/git/refs/heads/work",
        "gh api repos/o/r/merges -f base=work -f head=other",
    ] {
        assert!(says(cmd, true).is_some(), "let through: {cmd}");
        assert_eq!(
            says(cmd, false),
            None,
            "Claude's guard said something: {cmd}"
        );
    }
}

#[test]
fn ordinary_work_goes_through() {
    for cmd in [
        "git commit -m change",
        "git commit -am change",
        "git commit --amend --no-edit",
        "git commit -m subject -m 'with -x in it'",
        "git merge origin/main",
        "git pull origin main",
        "git push origin work",
        "git log --format=%n",
        "git status",
        "env FOO=1 cargo build",
        "cargo test --workspace",
        "rg -n 'fn read() -> String' crates",
        "git rebased-note",
        "env FOO=1 ls -lu",
        "gh api repos/o/r/contents/README.md",
        "gh api repos/o/r/git/refs/heads/main",
        "gh api -X GET repos/o/r/git/commits/abc -f per_page=1",
        "gh api repos/o/r/pulls/1/comments -f body=thanks",
        "gh pr view 9 --json headRefOid",
    ] {
        assert_eq!(says(cmd, true), None, "refused: {cmd}");
    }
}

/// Each command is read on its own: a read beside a write, in any way the
/// shell joins them, never speaks for the write.
#[test]
fn a_read_never_exempts_a_write() {
    let read = "gh api -X GET repos/o/r";
    let write = "gh api -X PATCH repos/o/r/git/refs/heads/main -F force=true -f sha=abc";
    for cmd in [
        format!("{read} && {write}"),
        format!("{read}; {write}"),
        format!("{read} | {write}"),
        format!("{read}\n{write}"),
        format!("({read}) || ({write})"),
        format!("echo $({read}) $({write})"),
        format!("{write} && {read}"),
        format!("bash -c '{read} && {write}'"),
        format!("git log --oneline && {write}"),
        format!("{read} && git revert HEAD"),
    ] {
        assert!(says(&cmd, true).is_some(), "let through: {cmd}");
    }
}

/// git's own options, before its command, never hide what the command is.
#[test]
fn gits_own_options_dont_hide_its_command() {
    for cmd in [
        "git --no-pager revert HEAD",
        "git -p cherry-pick abc",
        "git -P rebase origin/main",
        "git --paginate am change.patch",
        "git --git-dir=.git --work-tree=. rebase main",
        "git --git-dir .git --work-tree . revert HEAD",
        "git --namespace ns commit-tree HEAD^{tree} -m x",
        "git -c user.name=x revert HEAD",
        "git --no-optional-locks --literal-pathspecs commit -n -m x",
        "git -C /work --bare send-pack origin work",
        "/usr/bin/git --no-replace-objects cherry-pick abc",
        "git commit --no-verif -m x",
        "git -c pull.rebase=true pull",
        "git -c pull.rebase pull origin main",
        "git -c branch.work.rebase=true pull",
        "git pull --rebase=merges",
        "git pull -qr",
        "git pull --strategy ours --rebase",
    ] {
        assert!(says(cmd, true).is_some(), "let through: {cmd}");
    }
}

/// git's commands that make commits of their own, with no commit-msg, and
/// an alias, which could be any of them.
#[test]
fn plumbing_and_aliases_are_refused() {
    for cmd in [
        "git fast-import < stream",
        "git filter-branch --msg-filter cat HEAD",
        "git filter-repo --force",
        "git replace --graft HEAD HEAD~2",
        "git subtree split --prefix lib",
        "git -c alias.r=revert r HEAD",
        "git -c alias.r='!git revert' r HEAD",
        "git --config-env=alias.r=CMD r",
        "git config alias.r revert",
        "git config set alias.r revert",
        "git config --global alias.r '!sh -c x'",
    ] {
        assert!(says(cmd, true).is_some(), "let through: {cmd}");
    }
}

/// GitHub's writes of commits and refs, by gh's commands, its API (REST or
/// GraphQL, any spelling of the method), or curl.
#[test]
fn githubs_commit_and_ref_writes_are_refused() {
    for cmd in [
        "gh pr merge 5 --squash",
        "gh -R o/r pr merge 5",
        "gh pr --repo o/r merge 5",
        "gh pr update-branch 5",
        "gh pr revert 5",
        "gh repo sync o/r --force",
        "gh api --method=PATCH repos/o/r/git/refs/heads/main -f sha=x",
        "gh api -XPATCH repos/o/r/git/refs/heads/main -f sha=x",
        "gh api -X PUT repos/o/r/pulls/5/merge",
        "gh api repos/o/r/pulls/5/update-branch -X PUT",
        "gh api repos/o/r/merge-upstream -f branch=main",
        "gh api repos/o/r/branches/main/rename -f new_name=old",
        "gh api repos/o/r/git/refs/ -f ref=refs/heads/x -f sha=abc",
        "gh api https://api.github.com/repos/o/r/git/refs/heads/main -X DELETE",
        "gh api repos/o/r/contents/a.txt --input body.json",
        "gh api graphql -f query='mutation { createCommitOnBranch(input: {}) { commit { oid } } }'",
        "gh api graphql -F query='mutation($i: UpdateRefInput!) { updateRef(input: $i) { ref { name } } }'",
        "gh api graphql -f query='mutation { mergePullRequest(input: {}) { clientMutationId } }'",
        "gh api graphql -f query='mutation { enablePullRequestAutoMerge(input: {}) { clientMutationId } }'",
        "gh api graphql -F query=@mutation.graphql",
        "gh api graphql --input body.json",
        "ep=repos/o/r/git/refs/heads/main; gh api -X DELETE \"$ep\"",
        "gh api -X DELETE \"$ENDPOINT\"",
        "gh api -X DELETE \"$(printf repos/o/r/git/refs/heads/main)\"",
        "gh api \"$ep\" -f sha=abc",
        "gh api repos/o/r/git/refs/heads/main -X \"$m\"",
        "gh api $flags repos/o/r/git/refs/heads/main",
        "gh api graphql -f query=\"$(cat mutation.graphql)\"",
        "q=$(cat mutation.graphql); gh api graphql -f query=\"$q\"",
        "gh api graphql -F \"$field\"",
        "gh api graphql $args",
        "args='. repos/o/r/git/refs/heads/main -X DELETE --hostname'; gh api -q $args api.github.com",
        "gh api repos/o/r/git/ref[s]/heads/main -X DELETE",
        "gh api repos/o/r/git/ref?/heads/main -X DELETE",
        "gh api repos/o/r/{git/refs/heads/main,issues} -X DELETE",
        "gh api repos/o/r/issues/1/comments -f body=\"$msg\"",
        "gh api graphql -f query='query($o: String!) { repository(name: $o) { id } }' -f o=\"$repo\"",
        "gh api \"$ep\"",
        "gh api \"repos/$repo/issues\"",
        "gh api -X GET repos/o/r/git/refs/heads/main -q \"$jq\"",
        "curl -X PATCH -H 'Authorization: token x' https://api.github.com/repos/o/r/git/refs/heads/main -d '{\"sha\":\"x\"}'",
        "curl --request=DELETE https://api.github.com/repos/o/r/git/refs/heads/work",
        "curl -d '{}' https://api.github.com/repos/o/r/git/commits",
        "curl https://api.github.com/graphql -d '{\"query\":\"mutation { mergeBranch(input: {}) { clientMutationId } }\"}'",
        "curl https://api.github.com/graphql -d @body.json",
    ] {
        assert!(says(cmd, true).is_some(), "let through: {cmd}");
        assert_eq!(
            says(cmd, false),
            None,
            "Claude's guard said something: {cmd}"
        );
    }
}

/// What reads, or writes nothing git keeps, goes through, however it's
/// spelled.
#[test]
fn reads_and_other_writes_go_through() {
    for cmd in [
        "git --no-pager log --oneline",
        "git -c color.ui=never status",
        "git -C /work --git-dir=.git diff",
        "git pull --no-rebase",
        "git pull --rebase=false",
        "git -c pull.rebase=false pull",
        "git config --get alias.r",
        "git config user.name x",
        "git commit -m 'note -n' -m more",
        "git commit --author 'A -n <a@b>' -m x",
        "git commit -F msg -- -n",
        "git cherry -v origin/main",
        "git stash",
        "gh pr view 5",
        "gh pr create --fill",
        "gh -R o/r pr checks 5",
        "gh api graphql -f query='query { viewer { login } }'",
        "gh api -X GET repos/o/r/git/refs/heads/main && gh api repos/o/r/issues/1/comments -f body=x",
        "gh api repos/o/r/pulls/5/merge",
        "gh api repos/o/r/pulls/5/comments -f body=see",
        "curl https://api.github.com/repos/o/r/git/refs/heads/main",
        "curl -d x=1 https://example.org/git/refs",
    ] {
        assert_eq!(says(cmd, true), None, "refused: {cmd}");
    }
}

/// push.rs reads the push itself, so turning the hooks off doesn't get a
/// default branch's rewrite through: Claude's guard asks, Codex's refuses.
#[test]
fn a_default_branch_rewrite_is_caught_whatever_the_hooks() {
    let branch = "main";
    for cmd in [
        format!("git push origin --force {branch} --no-verify"),
        format!("git -c core.hooksPath=/dev/null push -f origin {branch}"),
        format!("GIT_CONFIG_COUNT=0 git push origin +{branch}"),
        format!("git push origin :{branch}"),
        format!("git push --delete origin {branch}"),
    ] {
        let claude = says(&cmd, false).unwrap_or_default();
        assert!(claude.contains(branch), "{cmd}: {claude}");
        let codex = says(&cmd, true).unwrap_or_default();
        assert!(codex.contains(&claude), "{cmd}: {codex}");
    }
}

/// `burrow-guard --codex-user USER CONFIG`, as Codex runs it, on `command`.
fn hook(user: &str, config: &str, command: &str) -> String {
    common::stand_ins();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("guard.json");
    fs::write(&path, config).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_burrow-guard"))
        .args(["--codex-user", user])
        .arg(&path)
        .env("XDG_RUNTIME_DIR", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "cwd": "/",
    });
    // One it doesn't guard, or can't read the config of, may answer
    // without reading its input.
    let _ = child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes());
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

fn decision(out: &str) -> (String, String) {
    let v: serde_json::Value = serde_json::from_str(out).unwrap();
    let o = &v["hookSpecificOutput"];
    (
        o["permissionDecision"].as_str().unwrap().to_owned(),
        o["permissionDecisionReason"].as_str().unwrap().to_owned(),
    )
}

#[test]
fn codexs_hook_snapshots_first_and_refuses_with_no_undo() {
    let me = current_user().expect("this user has a name");
    let dir = tempfile::tempdir().unwrap();
    let config = serde_json::json!({
        "git": "git",
        "zfs": zfs(dir.path()),
        "snapshotDatasets": ["pool/a"],
        "askAfterSeconds": 8,
    })
    .to_string();
    // Snapshotted, then let through.
    assert_eq!(hook(&me, &config, "rm -rf build"), "");
    let log = fs::read_to_string(dir.path().join("zfs.log")).unwrap();
    assert!(log.starts_with("snapshot pool/a@pre-"), "{log}");
    // No snapshot, no undo: refused, not asked (Codex runs what's asked).
    fs::write(dir.path().join("fail"), "").unwrap();
    for cmd in [
        "rm -rf build",
        "git clean -fdx",
        "git reset --hard",
        "echo x > out",
    ] {
        let (decision, reason) = decision(&hook(&me, &config, cmd));
        assert_eq!(decision, "deny", "{cmd}");
        assert!(reason.contains("undo snapshot"), "{cmd}: {reason}");
    }
    // A one-way door the guard reads, and a hidden commit.
    assert_eq!(decision(&hook(&me, &config, "efibootmgr -o 1")).0, "deny");
    assert_eq!(decision(&hook(&me, &config, "git revert HEAD")).0, "deny");
    // Another user's Codex (the reviewer's) isn't the jobs'.
    assert_eq!(hook("no-such-user", &config, "efibootmgr -o 1"), "");
}

#[test]
fn codexs_hook_that_cant_check_refuses() {
    let me = current_user().expect("this user has a name");
    let (decision, reason) = decision(&hook(&me, r#"{"git": "git"}"#, "ls"));
    assert_eq!(decision, "deny");
    assert!(
        reason.starts_with("The guard couldn't check this command ("),
        "{reason}"
    );
}

/// One curl may make many requests: each is read with its own operation's
/// options, so a harmless one never speaks for a write, and a header or
/// body naming GitHub is never taken for where a request goes.
#[test]
fn every_request_of_a_curl_is_read() {
    let refs = "https://api.github.com/repos/o/r/git/refs/heads/main";
    let read = "https://api.github.com/repos/o/r";
    let auth = "-H 'Authorization: token x'";
    let force = "-d '{\"sha\":\"abc\",\"force\":true}'";
    for cmd in [
        format!("curl {read} --next -X PATCH {auth} {refs} {force}"),
        format!("curl {read} -: -X PATCH {refs} {force}"),
        format!("curl -s {read} -:XPATCH {refs} {force}"),
        format!("curl -X PATCH {read} {refs} {force}"),
        format!("curl -X PATCH {force} {read} --url {refs}"),
        format!("curl -X PATCH {force} --url={refs}"),
        format!("curl -sSX PATCH {force} {refs}"),
        format!("curl -sSXPATCH {force} {refs}"),
        format!("curl {force} -- {refs}"),
        format!("curl -X PATCH {force} https://api.github.com/repos/o/r/issues/../git/refs/heads/main"),
        "curl -X PUT https://api.github.com/repos/o/r/pulls/5/merge/x/..".to_owned(),
        format!("curl -X PATCH {force} 'https://api.github.com/repos/o/r/git/refs/heads/{{main,x}}'"),
        format!("curl -X PATCH {force} \"$URL\""),
        format!("curl -X PATCH {force} --url @urls.txt"),
        format!("curl -X PATCH {force} --request-target /repos/o/r/git/refs/heads/main https://api.github.com/"),
        format!("curl -X PATCH {force} --connect-to example.org:443:api.github.com:443 -k https://example.org/repos/o/r/git/refs/heads/main"),
        format!("curl -X PATCH {force} -H 'Host: api.github.com' https://140.82.112.6/repos/o/r/git/refs/heads/main"),
        "curl https://api.github.com/graphql --next https://api.github.com/graphql -d '{\"query\":\"mutation { mergeBranch(input: {}) { clientMutationId } }\"}'".to_owned(),
        "curl https://api.github.com/graphql --data-binary @body.json".to_owned(),
        "curl https://api.github.com/graphql --json @-".to_owned(),
        "curl https://api.github.com/graphql -d \"$BODY\"".to_owned(),
        "curl --variable q@q.txt --expand-data '{{q}}' https://api.github.com/graphql".to_owned(),
        "curl https://ghe.example/api/graphql -d '{\"query\":\"mutation { createRef(input: {}) { clientMutationId } }\"}'".to_owned(),
        "curl -T body.json https://api.github.com/repos/o/r/contents/a.txt".to_owned(),
        // A switch turned off again: the body goes as a POST, the glob is one.
        "curl -q -G --no-get --json @commit.json https://api.github.com/repos/o/r/git/commits".to_owned(),
        "curl -G --no-get -d '{}' https://api.github.com/repos/o/r/git/commits".to_owned(),
        "curl -I --no-head -d x https://api.github.com/repos/o/r/git/refs/heads/main".to_owned(),
        format!("curl -g --no-globoff -X PATCH {force} 'https://{{api.github.com,example.org}}/repos/o/r/git/refs/heads/main'"),
        // curl's own variables, in a destination, target or header, globbing or not.
        format!("curl -g --variable u={refs} --expand-url '{{{{u}}}}' -X PATCH {force}"),
        format!("curl --variable u={refs} --expand-url '{{{{u}}}}' -X PATCH {force}"),
        format!("curl --variable u={refs} {read} --next -g --expand-url='{{{{u}}}}' -X PATCH {force}"),
        format!("curl -X PATCH {force} --variable t=/repos/o/r/git/refs/heads/main --expand-request-target '{{{{t}}}}' https://api.github.com/"),
        format!("curl -X PATCH {force} --variable 'h=Host: api.github.com' --expand-header '{{{{h}}}}' https://140.82.112.6/repos/o/r/git/refs/heads/main"),
        format!("curl --variable m=PATCH --expand-request '{{{{m}}}}' {force} {refs}"),
        format!("curl -K curl.cfg {read}"),
        format!("curl --config=curl.cfg {read}"),
        format!("curl -sK curl.cfg {read}"),
        format!("CURL_HOME=/tmp/c curl {read}"),
        format!("XDG_CONFIG_HOME=/tmp/c curl {read}"),
        format!("export HOME=/tmp/c; curl {read}"),
    ] {
        assert!(says(&cmd, true).is_some(), "let through: {cmd}");
        assert_eq!(says(&cmd, false), None, "Claude's guard said something: {cmd}");
    }
}

/// What curl reads, or writes anywhere but GitHub's git data, goes
/// through, whatever its headers and bodies name.
#[test]
fn curls_reads_and_other_writes_go_through() {
    for cmd in [
        "curl -H 'Referer: https://api.github.com/repos/o/r/git/refs/heads/main' -d x=1 https://example.org/hook",
        "curl -d 'see https://api.github.com/repos/o/r/git/refs/heads/main' https://example.org/hook",
        "curl -e https://api.github.com/repos/o/r/git/refs https://example.org/ -X POST",
        "curl https://api.github.com/repos/o/r/git/refs/heads/main --next https://api.github.com/repos/o/r",
        "curl -X PATCH https://example.org/repos/o/r/git/refs/heads/main --next https://api.github.com/repos/o/r/git/refs/heads/main",
        "curl -G -d per_page=1 https://api.github.com/repos/o/r/git/commits",
        "curl -G --no-get -G -d per_page=1 https://api.github.com/repos/o/r/git/commits",
        "curl -X PATCH -H \"Authorization: token $T\" -d x https://example.org/git/refs/heads/main",
        "curl -I https://api.github.com/repos/o/r/git/refs/heads/main -d x",
        "curl -o \"$HOME/out\" https://api.github.com/repos/o/r",
        "curl -q --url https://api.github.com/repos/o/r/issues/1/comments -d '{\"body\":\"x\"}'",
        "curl https://api.github.com/graphql -d '{\"query\":\"query { viewer { login } }\"}'",
    ] {
        assert_eq!(says(cmd, true), None, "refused: {cmd}");
    }
}
