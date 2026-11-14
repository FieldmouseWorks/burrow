//! A proj job never puts the `first` label on: a job for a person's issue
//! acts on GitHub as them, where GitHub can't tell its label from theirs.

use std::path::PathBuf;
use std::time::SystemTime;

use burrow_guard::config::Config;
use burrow_guard::{Guard, Hook, Input, Namespace};

/// What the guard says about `cmd`, keeping `label`, in a job (`job`) or
/// not, Codex's (`codex`) or Claude's.
fn says(cmd: &str, label: Option<&str>, job: bool, codex: bool) -> Option<String> {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        git: PathBuf::from("git"),
        zfs: PathBuf::from("/no-such-zfs"),
        systemd_run: PathBuf::from("/no-such-systemd-run"),
        snapshot_datasets: Vec::new(),
        ask_after_seconds: 8,
        first_label: label.map(str::to_owned),
    };
    let guard = Guard {
        config: &config,
        codex,
        job,
        mark: dir.path().join("mark"),
        namespace: Namespace::Machine,
    };
    let hook = Hook {
        tool_name: Some("Bash".to_owned()),
        tool_input: Some(Input {
            command: Some(cmd.to_owned()),
        }),
        cwd: Some(dir.path().to_string_lossy().into_owned()),
    };
    guard.check(&hook, SystemTime::now())
}

fn refused(cmd: &str) -> bool {
    says(cmd, Some("first"), true, false)
        .is_some_and(|r| r.starts_with("Puts the `first` label on"))
}

const PUTS_FIRST_ON: &[&str] = &[
    "gh issue edit 5 --add-label first",
    "gh pr edit 5 --add-label bug,first",
    "gh pr edit 5 --add-label=First",
    "gh issue edit 5 --add-label ' first '",
    "gh -R o/r issue edit 5 --add-label first",
    "gh issue edit 5 -R o/r --add-label \"$label\"",
    "gh issue edit 5 --add-label fir*",
    "gh issue create --title x --label first",
    "gh pr create --fill -l first",
    "gh pr create --fill -lfirst",
    "gh pr create --fill --label=bug,first",
    "gh api repos/o/r/issues/5/labels -f 'labels[]=first'",
    "gh api -X PUT repos/o/r/issues/5/labels -f labels[]=first",
    "gh api --method PATCH /repos/o/r/issues/5 -f 'labels[]=first'",
    "gh api repos/o/r/issues -f title=x -f 'labels[]=first'",
    "gh api https://api.github.com/repos/o/r/issues/5/labels/ -F 'labels[]=FIRST'",
    "gh api 'repos/{owner}/{repo}/issues/5/labels' --input labels.json",
    "gh api repos/o/r/issues/5/labels -F labels[]=@labels.txt",
    "gh api repos/o/r/issues/5/labels -f \"labels[]=$l\"",
    "gh api repos/o/r/issues/5/labels -f \"$field\"",
    "gh api \"$endpoint\" -f 'labels[]=first'",
    "gh api \"repos/o/r/issues/$n\" -X PATCH -f 'labels[]=first'",
    "gh api graphql -f query='mutation { addLabelsToLabelable(input: {labelableId: \"I\", labelIds: [\"L\"]}) { clientMutationId } }'",
    "gh api graphql -f query='mutation($i: UpdateIssueInput!) { updateIssue(input: $i) { issue { id } } }' -f 'labelIds[]=L'",
    "gh api graphql -F query=@mutation.graphql",
    "gh api graphql --input body.json",
    "curl -X POST -H 'Authorization: token t' https://api.github.com/repos/o/r/issues/5/labels -d '{\"labels\":[\"first\"]}'",
    "curl https://api.github.com/repos/o/r/issues/5/labels -d '[\"first\"]'",
    "curl -X PATCH https://api.github.com/repos/o/r/issues/5 --json '{\"labels\":[{\"name\":\"First\"}]}'",
    "curl -X PATCH https://api.github.com/repos/o/r/issues/5 -d \"$body\"",
    "curl https://api.github.com/repos/o/r/issues/5/labels -d @labels.json",
    "curl -X POST \"$url\" -d '{\"labels\":[\"first\"]}'",
    "curl https://api.github.com/graphql -d @mutation.json",
    "sh -c 'gh issue edit 5 --add-label first'",
    "cd /work && gh issue edit 5 --add-label first",
    "gh issue view 5; /run/current-system/sw/bin/gh pr edit 5 --add-label first",
];

const LEAVES_IT_ALONE: &[&str] = &[
    "gh issue edit 5 --add-label bug",
    "gh issue edit 5 --remove-label first",
    "gh issue edit 5 --add-label firsts,first-time",
    "gh pr create --title 'do this first' --body first",
    "gh issue create --title first --label bug",
    "gh issue view 5 --json labels",
    "gh issue list --label first",
    "gh api repos/o/r/issues/5/labels",
    "gh api repos/o/r/issues/5/comments -f body=\"$msg\"",
    "gh api repos/o/r/pulls/5/comments/7/replies -f body=first",
    "gh api -X PATCH repos/o/r/issues/5 -f body=\"$text\"",
    "gh api repos/o/r/issues -f title='first things' -f body=first",
    "gh api repos/o/r/issues/5/labels -f 'labels[]=bug'",
    "gh api -X DELETE repos/o/r/issues/5/labels/first",
    "gh api graphql -f query='mutation { resolveReviewThread(input: {threadId: \"'$id'\"}) { thread { id } } }'",
    "gh api graphql -f query='query { repository(owner: \"o\", name: \"r\") { labels(first: 5) { nodes { name } } } }'",
    "curl https://api.github.com/repos/o/r/issues/5/labels",
    "curl -X POST https://api.github.com/repos/o/r/issues/5/comments -d '{\"body\":\"first\"}'",
    "curl -X POST https://api.github.com/repos/o/r/issues/5/labels -d '{\"labels\":[\"bug\"]}'",
    "curl -X POST \"$url\" -d '{\"text\":\"hi\"}'",
    "echo first",
];

#[test]
fn what_puts_first_on_is_refused_in_a_job() {
    for cmd in PUTS_FIRST_ON {
        assert!(refused(cmd), "{cmd}");
        // Codex's guard, every Codex it guards a job's, refuses it too.
        assert!(says(cmd, Some("first"), true, true).is_some(), "{cmd}");
    }
}

#[test]
fn what_leaves_first_alone_runs() {
    for cmd in LEAVES_IT_ALONE {
        assert_eq!(says(cmd, Some("first"), true, false), None, "{cmd}");
    }
}

/// The owner's own sessions put it on as theirs, and a machine whose
/// steward reads no such label keeps none.
#[test]
fn outside_a_job_or_with_no_label_nothing_is_asked() {
    for cmd in PUTS_FIRST_ON {
        assert_eq!(says(cmd, Some("first"), false, false), None, "{cmd}");
        assert_eq!(says(cmd, None, true, false), None, "{cmd}");
    }
}

/// The label is the machine's to name, spaces and all.
#[test]
fn the_label_is_the_one_the_machine_names() {
    let says = |cmd| says(cmd, Some("go now"), true, false);
    assert!(says("gh issue edit 5 --add-label 'Go Now'").is_some());
    assert!(says("gh api repos/o/r/issues/5/labels -f 'labels[]=go now'").is_some());
    assert!(says("gh issue edit 5 --add-label first").is_none());
}
